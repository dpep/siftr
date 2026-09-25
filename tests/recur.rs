//! A regression that is fixed and breaks again, repeatedly: every run it returns in must say so, and the
//! outcome history must carry every cycle rather than freezing after the first one.
//!
//! The rolling baseline absorbs a value it has already seen — by r8 the N+1's query count reads
//! `[1,1,1,9,1,9,1]` against a current 9, which is inside the baseline range, so no FREQUENCY rule can fire
//! on it. Only the signal's own frozen baseline (its original clean runs) still says 9 is a change, which is
//! what the still-open judgement re-runs.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

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

    fn ingest(&self, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        self.json(&["-j", dir.to_str().unwrap(), "--context", "rails_demo"])
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }

    /// The outcome row of `signal`, as `history --signals -j` derives it.
    fn outcome(&self, signal: &str) -> Value {
        let rows = self.json(&["history", "--signals", "-j"]);
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["signal"]["id"] == signal)
            .unwrap_or_else(|| panic!("no {signal} in {rows}"))
            .clone()
    }

    /// Three clean runs, then the N+1 in r4: the change headed by s1, queries 3 → 10.
    fn with_regression() -> Self {
        let sandbox = Sandbox::new();
        for scenario in CLEAN {
            sandbox.ingest(scenario);
        }
        assert_eq!(sandbox.ingest("n_plus_one")["changes"], 1, "caught in r4");
        sandbox
    }
}

/// `(id, run)` of each open signal.
fn open_ids(changes: &Value) -> Vec<(String, String)> {
    changes["open_signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["run"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// The regression is fixed in r5, back in r6, fixed in r7 and back in r8. The rolling baseline absorbs it
/// after r4, so `changes` is 0 from r5 on; every run it is actually present in must still report it as open.
/// This is what a CI gate reading `changes` and `open_signals` depends on.
#[test]
fn a_repeated_regression_is_reported_on_every_run_it_returns_in() {
    let sandbox = Sandbox::with_regression();

    let fixed = sandbox.ingest("baseline");
    assert_eq!(open_ids(&fixed), [], "r5 fixed it: nothing open");

    let broken_again = sandbox.ingest("n_plus_one");
    assert_eq!(
        broken_again["changes"], 0,
        "r4 is in r6's baseline, so no new change fires"
    );
    assert_eq!(
        open_ids(&broken_again)
            .first()
            .map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "the N+1 is back in r6 and siftr must say so: {broken_again:#}"
    );

    let fixed_again = sandbox.ingest("baseline");
    assert_eq!(open_ids(&fixed_again), [], "r7 fixed it again");

    let broken_twice = sandbox.ingest("n_plus_one");
    assert_eq!(
        open_ids(&broken_twice)
            .first()
            .map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "back a second time in r8: {broken_twice:#}"
    );

    // The human report a coding agent reads must not say "0 changes" with nothing beside it.
    let human = sandbox.text(&["changes", "r8"]);
    assert!(
        human.contains("still open"),
        "r8 must not report silence while the N+1 is present: {human}"
    );
    assert_eq!(
        sandbox.siftr(&["changes", "r8"]).status.code(),
        Some(0),
        "a regression that is present is something to look at"
    );
}

/// One resolve and one recurrence is all two `Option` positions can express, so a second cycle used to be
/// unrepresentable: `history --signals` froze at r6 and never mentioned r7's fix or r8's re-break.
#[test]
fn the_outcome_history_carries_every_cycle() {
    let sandbox = Sandbox::with_regression();
    sandbox.ingest("baseline");
    sandbox.ingest("n_plus_one");

    // Fixed again in r7: the signal's latest verdict is that it is resolved, not that it recurred.
    sandbox.ingest("baseline");
    let after_fix = sandbox.outcome("s1");
    assert_eq!(
        after_fix["outcome"], "resolved",
        "r7 fixed it, so the latest state is resolved: {after_fix:#}"
    );

    // Broken a second time in r8.
    sandbox.ingest("n_plus_one");
    let after_break = sandbox.outcome("s1");
    assert_eq!(
        (&after_break["outcome"], &after_break["recurrences"]),
        (&Value::from("recurred"), &Value::from(2)),
        "two separate recurrences, r6 and r8: {after_break:#}"
    );
    assert_eq!(
        (&after_break["resolved_in"], &after_break["recurred_in"]),
        (&Value::from("r5"), &Value::from("r6")),
        "the first resolve and the first recurrence keep their meaning"
    );

    let human = sandbox.text(&["history", "--signals"]);
    let s1 = human
        .lines()
        .find(|line| line.trim_start().starts_with("s1 "))
        .unwrap_or_else(|| panic!("{human}"));
    assert!(
        s1.contains("r8"),
        "the latest cycle must be named, not frozen at the first: {s1}"
    );
}

/// A change called wrong stays called wrong across cycles: siftr must not re-report a regression the
/// developer said was intended, however many times it comes and goes.
#[test]
fn a_dismissed_regression_is_not_reported_when_it_returns() {
    let sandbox = Sandbox::with_regression();
    sandbox.siftr(&["ack", "s1", "--wrong", "-m", "intended"]);
    sandbox.ingest("baseline");

    let broken_again = sandbox.ingest("n_plus_one");
    assert_eq!(
        open_ids(&broken_again),
        [],
        "dismissed in r4, so its return is not reported: {broken_again:#}"
    );
    assert!(
        !sandbox.text(&["changes", "r6"]).contains("still open"),
        "a dismissed change is never reminded"
    );
}
