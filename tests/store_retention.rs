//! Retention as commands see it: a pruned run's stats fail loudly naming the setting, pruned evidence is said
//! so next to the numbers, and a signal whose baseline was pruned has an unknown outcome, never a verdict from
//! partial data.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// The smallest retention siftr allows.
const SMALLEST: &[(&str, &str)] = &[("SIFTR_KEEP_RUNS", "21"), ("SIFTR_KEEP_EVIDENCE", "2")];

struct Sandbox {
    home: TempDir,
    project: TempDir,
    retention: &'static [(&'static str, &'static str)],
}

impl Sandbox {
    /// Three clean runs, the N+1 (r4, whose s1 was judged against r1–r3), then 20 clean runs: r24.
    fn n_plus_one_then_twenty(retention: &'static [(&'static str, &'static str)]) -> Self {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
            retention,
        };
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
        sandbox
    }

    fn ingest(&self, scenario: &str) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        let output = self.siftr(&[dir.to_str().unwrap(), "--context", "rails_demo"]);
        assert!(output.status.success(), "{}", stderr(&output));
    }

    /// With this sandbox's retention and nothing else from the environment.
    fn siftr(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
        command
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME");
        for name in ["SIFTR_KEEP_RUNS", "SIFTR_KEEP_EVIDENCE", "SIFTR_KEEP_DAYS"] {
            command.env_remove(name);
        }
        command
            .envs(self.retention.iter().copied())
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
    let sandbox = Sandbox::n_plus_one_then_twenty(SMALLEST);

    let runs = sandbox.json(&["history", "-n", "50", "-j"]);
    assert_eq!(
        runs.as_array().unwrap().len(),
        24,
        "pruned runs stay listed"
    );
    let captured: Vec<usize> = (1..=24)
        .filter(|n| sandbox.home.path().join(format!("runs/r{n}")).exists())
        .collect();
    assert_eq!(
        captured,
        (13..=24).collect::<Vec<_>>(),
        "evidence back to r13: r24's baseline, and the run each of those was judged against"
    );

    // r4 keeps its stats (r14 was judged against it) but its own baseline, r1–r3, doesn't.
    let explain = sandbox.siftr(&["explain", "s1"]);
    assert_eq!(explain.status.code(), Some(2));
    assert!(
        stderr(&explain).contains("r3's stats and evidence were pruned: siftr keeps the last 21 runs of each context (SIFTR_KEEP_RUNS)"),
        "{}",
        stderr(&explain)
    );

    let r4 = sandbox.json(&["changes", "r4", "-j"]);
    let behavior = r4["signals"][0]["behavior"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let evidence = sandbox.siftr(&["explain", &behavior, "--run", "r4"]);
    assert_eq!(evidence.status.code(), Some(2));
    assert!(
        stderr(&evidence).contains("r4's evidence was pruned: siftr keeps evidence for the last 2 runs of each context (SIFTR_KEEP_EVIDENCE)"),
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

#[test]
fn explain_shows_the_numbers_when_only_the_evidence_was_pruned() {
    let sandbox = Sandbox::n_plus_one_then_twenty(&[("SIFTR_KEEP_EVIDENCE", "2")]);

    let explain = sandbox.siftr(&["explain", "s1"]);
    let stdout = String::from_utf8_lossy(&explain.stdout);
    assert_eq!(explain.status.code(), Some(0), "{}", stderr(&explain));
    assert!(
        stdout.contains("r4 10  |  baseline r3 3  r2 3  r1 3"),
        "{stdout}"
    );
    assert!(
        stdout.contains("evidence  r4 pruned (SIFTR_KEEP_EVIDENCE)"),
        "{stdout}"
    );

    let json = sandbox.json(&["explain", "s1", "-j"]);
    assert_eq!(json["evidence"]["pruned"], "SIFTR_KEEP_EVIDENCE");
    assert_eq!(json["evidence"]["exemplars"], serde_json::json!([]));
}
