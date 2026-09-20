//! `siftr history --scorecard`: the roll-up of what became of this project's signals. Its job is to agree, row
//! for row, with the `history --signals` lines it counts — a rate that drifts from its own drill-down is worse
//! than no rate — and to keep only the digits its counts back.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Three clean runs, then the N+1 — r4 raises four signals over three behaviors.
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
        self.siftr(&[
            "ingest",
            "--context",
            "rails_demo",
            "--dir",
            dir.to_str().unwrap(),
        ]);
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

    fn scorecard(&self) -> Value {
        self.json(&["history", "--scorecard", "-j"])
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }
}

/// The scorecard's own drill-down, counted independently: how many `history --signals` lines say each thing.
fn from_signals(sandbox: &Sandbox) -> (usize, usize, usize) {
    let rows = sandbox.json(&["history", "--signals", "-j"]);
    let rows = rows.as_array().unwrap();
    let count = |f: &dyn Fn(&Value) -> bool| rows.iter().filter(|row| f(row)).count();
    (
        rows.len(),
        count(&|row| row["investigated"] == Value::Bool(true)),
        count(&|row| row["outcome"] == "resolved" && row["investigated"] == Value::Bool(false)),
    )
}

#[test]
fn the_totals_are_the_history_signals_rows_counted() {
    let sandbox = Sandbox::n_plus_one();
    sandbox.siftr(&["explain", "s1"]);
    sandbox.ingest("baseline");

    let (raised, investigated, resolved_unexamined) = from_signals(&sandbox);
    assert_eq!(
        (raised, investigated, resolved_unexamined),
        (4, 1, 3),
        "the fixture's own shape changed; the scorecard is still judged against it, not against these numbers"
    );

    let total = sandbox.scorecard()["total"].clone();
    assert_eq!(total["raised"], raised);
    assert_eq!(total["examined"], investigated);
    assert_eq!(total["resolved_unexamined"], resolved_unexamined);
    assert_eq!(total["judged"], raised, "every signal has a later verdict");
    assert_eq!(total["unjudged"], 0);
    assert_eq!(total["open"], 0);
}

#[test]
fn rates_keep_only_the_figures_their_counts_back() {
    let sandbox = Sandbox::n_plus_one();
    sandbox.siftr(&["explain", "s1"]);
    sandbox.ingest("baseline");

    let card = sandbox.scorecard();
    // 1 of 4 examined and 3 of 4 resolved unexamined: four signals back one significant figure, so 0.3 and 0.8,
    // never 0.25 and 0.75. Rounding each where it is built is why they don't sum to 1 — the counts beside them
    // are the answer, and a rate that hid its own coarseness would be the worse trade. The JSON must hold the
    // same rounded value the human output shows.
    assert_eq!(card["total"]["examined_rate"], 0.3);
    assert_eq!(card["total"]["unexamined_rate"], 0.8);
    let human = sandbox.text(&["history", "--scorecard"]);
    let all = human.lines().find(|l| l.contains(" all ")).unwrap();
    assert!(all.contains("1  0.3"), "{human}");
    assert!(all.contains("3  0.8"), "{human}");
    assert!(
        !human.contains("0.25") && !human.contains("0.75"),
        "{human}"
    );
}

#[test]
fn kinds_a_project_never_raised_are_left_out_rather_than_padded_with_zeros() {
    let sandbox = Sandbox::n_plus_one();
    let kinds: Vec<String> = sandbox.scorecard()["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, ["disappeared", "frequency"]);
}

#[test]
fn an_unfixed_change_is_open_and_counts_towards_no_resolution_rate() {
    let sandbox = Sandbox::n_plus_one();
    sandbox.ingest("n_plus_one");

    let total = sandbox.scorecard()["total"].clone();
    assert_eq!(total["open"], 4);
    assert_eq!(total["resolved"], 0);
    assert_eq!(
        total["unexamined_rate"],
        Value::Null,
        "nothing resolved, so the rate has nothing behind it and says so"
    );
}

#[test]
fn a_signal_todays_rules_would_not_raise_is_unjudged_rather_than_counted_either_way() {
    let sandbox = Sandbox::n_plus_one();
    sandbox.ingest("baseline");
    rusqlite::Connection::open(sandbox.home.path().join("siftr.db"))
        .unwrap()
        .execute(
            "UPDATE signals SET measure = 'duration_ms' WHERE id = 1",
            [],
        )
        .unwrap();

    let total = sandbox.scorecard()["total"].clone();
    assert_eq!(total["raised"], 4);
    assert_eq!(total["judged"], 3);
    assert_eq!(total["unjudged"], 1);
    assert_eq!(
        total["resolved"], 3,
        "the unjudged signal is out of the numerator and the denominator alike"
    );
}

#[test]
fn an_empty_project_reports_nothing_rather_than_a_rate_of_zero() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    let output = sandbox.siftr(&["history", "--scorecard", "-j"]);
    assert_eq!(output.status.code(), Some(1), "no results");
    let card: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(card["total"]["raised"], 0);
    assert_eq!(card["total"]["examined_rate"], Value::Null);
    assert_eq!(card["kinds"], Value::Array(Vec::new()));
}
