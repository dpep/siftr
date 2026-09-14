//! Retention as commands see it: a pruned run's stats and evidence fail loudly naming the setting, and a
//! signal whose baseline was pruned has an unknown outcome, never a verdict from partial data.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn ingest(&self, scenario: &str) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/rails_demo")
            .join(scenario);
        let output = self.siftr(&[
            "ingest",
            "--context",
            "rails_demo",
            "--dir",
            dir.to_str().unwrap(),
        ]);
        assert!(output.status.success(), "{}", stderr(&output));
    }

    /// With the smallest retention siftr allows: stats for 21 runs, evidence for 2.
    fn siftr(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env("SIFTR_KEEP_RUNS", "21")
            .env("SIFTR_KEEP_EVIDENCE", "2")
            .env_remove("SIFTR_KEEP_DAYS")
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let output = self.siftr(args);
        assert!(
            output.status.code().is_some_and(|code| code < 2),
            "{args:?}: {}",
            stderr(&output)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn what_retention_pruned_is_named_never_read_as_empty() {
    // r1–r3 clean, r4 the N+1 (s1, judged against r1–r3), then 20 clean runs: r24.
    let sandbox = Sandbox::new();
    for scenario in [
        "baseline",
        "baseline_2",
        "baseline_documentation",
        "n_plus_one",
    ] {
        sandbox.ingest(scenario);
    }
    for _ in 0..20 {
        sandbox.ingest("baseline");
    }

    let runs = sandbox.json(&["history", "-n", "50", "-j"]);
    assert_eq!(
        runs.as_array().unwrap().len(),
        24,
        "pruned runs stay listed"
    );
    let captures: Vec<bool> = (1..=24)
        .map(|n| sandbox.home.path().join(format!("runs/r{n}")).exists())
        .collect();
    assert_eq!(
        captures.iter().filter(|&&kept| kept).count(),
        2,
        "{captures:?}"
    );
    assert!(
        captures[22] && captures[23],
        "the latest two keep their capture"
    );

    // r4 keeps its stats (it's the 21st newest) but its baseline, r1–r3, doesn't.
    let explain = sandbox.siftr(&["explain", "s1"]);
    assert_eq!(explain.status.code(), Some(2));
    assert!(
        stderr(&explain).contains("r3's stats and evidence were pruned: siftr keeps the last 21 runs of each command (SIFTR_KEEP_RUNS)"),
        "{}",
        stderr(&explain)
    );

    let r4 = sandbox.json(&["changes", "r4", "-j"]);
    let behavior = r4["signals"][0]["behavior"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let evidence = sandbox.siftr(&["evidence", &behavior, "--run", "r4"]);
    assert_eq!(evidence.status.code(), Some(2));
    assert!(
        stderr(&evidence).contains("r4's evidence was pruned: siftr keeps evidence for the last 2 runs of each command (SIFTR_KEEP_EVIDENCE)"),
        "{}",
        stderr(&evidence)
    );

    let outcomes = sandbox.json(&["history", "--signals", "-n", "50", "-j"]);
    let s1 = outcomes
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["signal"]["id"] == "s1")
        .unwrap_or_else(|| panic!("no s1 in {outcomes}"));
    assert_eq!(s1["outcome"], "unknown");
    assert_eq!(
        s1["unknown_reason"],
        "baseline runs pruned (SIFTR_KEEP_RUNS)"
    );

    sandbox.ingest("baseline");
    let latest = sandbox.json(&["changes", "-j"]);
    assert_eq!(
        latest["run"]["id"], "r25",
        "a pruned run's id is never reused"
    );
    assert_eq!(latest["baseline_runs"].as_array().unwrap().len(), 10);
}
