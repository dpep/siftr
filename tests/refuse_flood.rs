//! A comparison that produces more changes than a reader can use is not a finding: the run records
//! none of them and says so, keeping its behaviors as evidence. A normal RSpec context is untouched.
//!
//! The threshold is `signal::MAX_CHANGES`. Measured separation behind it: over 27 runs of the real
//! RSpec captures in `fixtures/`, the most any one run produced was 11 signals; the macOS unified log
//! produced 8,564 in one run. Nothing observed lies between.

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

    fn siftr(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(
            output.status.code().is_some_and(|code| code < 2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args).stdout).unwrap()
    }

    /// One run of a corpus whose templates are unique to `run`, so every one of them is NEW.
    fn ingest_flood(&self, run: usize, templates: usize) -> Value {
        // Letters, not digits: the normalizer masks digit runs, which would fold these into one
        // template. A per-run prefix letter makes each run's templates disjoint from the others'.
        let prefix = (b'q' + run as u8) as char;
        let log: String = (0..templates)
            .map(|i| {
                let letter = |n: usize| (b'a' + (n % 26) as u8) as char;
                format!(
                    "widget {prefix}{}{}{} ready\n",
                    letter(i / 676),
                    letter(i / 26),
                    letter(i)
                )
            })
            .collect();
        let path = self.project.path().join(format!("flood{run}.log"));
        std::fs::write(&path, log).unwrap();
        self.json(&["ingest", "--context", "flood", "-j", path.to_str().unwrap()])
    }

    fn ingest_fixture(&self, family: &str, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(family)
            .join(scenario);
        self.json(&[
            "ingest",
            "--context",
            family,
            "-j",
            "--dir",
            dir.to_str().unwrap(),
        ])
    }
}

fn signals(changes: &Value) -> usize {
    changes["signals"].as_array().unwrap().len()
}

/// A source where nothing recurs reports one refusal, not one change per template.
#[test]
fn a_comparison_that_produces_more_changes_than_a_reader_can_use_records_none() {
    let sandbox = Sandbox::new();
    const TEMPLATES: usize = 1500;
    for run in 1..=2 {
        let early = sandbox.ingest_flood(run, TEMPLATES);
        assert_eq!(
            signals(&early),
            0,
            "run {run} has too little baseline to fire"
        );
    }
    let flooded = sandbox.ingest_flood(3, TEMPLATES);

    assert_eq!(
        flooded["run"]["uncompared"].as_u64(),
        Some(TEMPLATES as u64),
        "the refused count is recorded, so `changes` can say so later: {flooded}"
    );
    assert_eq!(
        (signals(&flooded), flooded["changes"].as_u64()),
        (0, Some(0)),
        "a failed comparison claims no changes"
    );

    // The run is still evidence: its behaviors are recorded and reachable.
    let summary = sandbox.json(&["summary", "r3", "-j"]);
    assert_eq!(
        summary["behaviors_total"].as_u64(),
        Some(TEMPLATES as u64),
        "the behaviors are kept"
    );

    // One line, naming what siftr saw and what is still there; never as an error.
    let human = String::from_utf8(sandbox.siftr(&["changes", "r3"]).stdout).unwrap();
    let headline = human.lines().next().unwrap_or_default();
    assert!(
        headline.contains("1500 changes") && headline.contains("evidence"),
        "{human}"
    );
    assert!(
        !human.to_lowercase().contains("error") && !human.to_lowercase().contains("fail"),
        "refusing is the right answer, not a failure: {human}"
    );
    assert!(human.ends_with("next: siftr summary r3\n"), "{human}");
    assert!(
        sandbox.siftr(&["changes", "r3"]).status.code() == Some(1),
        "no changes to look at"
    );
}

/// The corpora siftr is for: a suite that grows by 10 examples, and an N+1. Both stay reported.
#[test]
fn a_normal_rspec_context_is_unaffected() {
    let hunt = Sandbox::new();
    for _ in 0..3 {
        hunt.ingest_fixture("rspec_hunt", "a10_clean");
    }
    let grown = hunt.ingest_fixture("rspec_hunt", "a20_warn1");
    assert_eq!(
        grown["run"]["uncompared"],
        Value::Null,
        "a real suite is never refused"
    );
    assert_eq!(
        signals(&grown),
        11,
        "10 new examples and the warning, each reported"
    );

    let rails = Sandbox::new();
    for scenario in ["baseline", "baseline_2", "baseline_documentation"] {
        rails.ingest_fixture("rails_demo", scenario);
    }
    let n_plus_one = rails.ingest_fixture("rails_demo", "n_plus_one");
    assert_eq!(n_plus_one["run"]["uncompared"], Value::Null);
    assert_eq!(
        n_plus_one["changes"].as_u64(),
        Some(1),
        "the N+1 still lands"
    );
    assert_eq!(signals(&n_plus_one), 4);
}
