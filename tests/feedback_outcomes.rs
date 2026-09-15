//! What became of a signal, derived from later runs and feedback rather than stored: the rails_demo N+1,
//! fixed after an investigation, fixed without one, left alone, and back again.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const N_PLUS_ONE: [&str; 4] = [
    "baseline",
    "baseline_2",
    "baseline_documentation",
    "n_plus_one",
];

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    /// Three clean runs, then the N+1: r4, whose change is headed by s1, queries 3 → 10.
    fn n_plus_one() -> Self {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        };
        for scenario in N_PLUS_ONE {
            sandbox.ingest(scenario);
        }
        sandbox
    }

    fn ingest(&self, scenario: &str) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
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
            stderr(&output)
        );
        output
    }

    /// `(outcome, resolved_in, recurred_in, investigated)` of `signal`, as `history --signals -j` derives it.
    fn outcome(&self, signal: &str) -> (Value, Value, Value, Value) {
        let history = self.siftr(&["history", "--signals", "-j"]);
        let rows: Value = serde_json::from_slice(&history.stdout).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["signal"]["id"] == signal)
            .unwrap_or_else(|| panic!("no {signal} in {rows}"));
        (
            row["outcome"].clone(),
            row["resolved_in"].clone(),
            row["recurred_in"].clone(),
            row["investigated"].clone(),
        )
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn outcome(
    status: &str,
    resolved_in: Option<&str>,
    recurred_in: Option<&str>,
    investigated: bool,
) -> (Value, Value, Value, Value) {
    (
        status.into(),
        resolved_in.into(),
        recurred_in.into(),
        investigated.into(),
    )
}

#[test]
fn an_n_plus_one_explained_then_fixed_is_resolved_after_investigation() {
    let sandbox = Sandbox::n_plus_one();
    sandbox.siftr(&["explain", "s1"]);
    sandbox.ingest("baseline");

    assert_eq!(
        sandbox.outcome("s1"),
        outcome("resolved", Some("r5"), None, true)
    );
    let human = String::from_utf8(sandbox.siftr(&["history", "--signals"]).stdout).unwrap();
    let s1 = human
        .lines()
        .find(|line| line.trim_start().starts_with("s1 "))
        .unwrap();
    assert!(
        s1.ends_with("resolved in r5 after investigation"),
        "{human}"
    );
}

#[test]
fn a_regression_left_alone_stays_open_though_the_live_baseline_absorbs_it() {
    let sandbox = Sandbox::n_plus_one();
    sandbox.ingest("n_plus_one");
    let r5: Value =
        serde_json::from_slice(&sandbox.siftr(&["changes", "r5", "-j"]).stdout).unwrap();
    assert_eq!(
        r5["changes"], 0,
        "r4 joined r5's baseline, so the unfixed N+1 no longer fires"
    );
    assert_eq!(sandbox.outcome("s1"), outcome("open", None, None, false));

    sandbox.ingest("baseline");
    // Looking after the fix landed didn't bring it about.
    sandbox.siftr(&["explain", "s1"]);
    assert_eq!(
        sandbox.outcome("s1"),
        outcome("resolved", Some("r6"), None, false)
    );

    sandbox.ingest("n_plus_one");
    assert_eq!(
        sandbox.outcome("s1"),
        outcome("recurred", Some("r6"), Some("r7"), false)
    );

    // A signal today's rules wouldn't raise can't be judged against later runs.
    rusqlite::Connection::open(sandbox.home.path().join("siftr.db"))
        .unwrap()
        .execute(
            "UPDATE signals SET measure = 'duration_ms' WHERE id = 1",
            [],
        )
        .unwrap();
    assert_eq!(sandbox.outcome("s1").0, "unknown");
}
