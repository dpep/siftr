//! Baseline eligibility and incompleteness across suites that grow, shrink, stay red under `--fail-fast`, or
//! fix a spec file that never loaded; and signal outcomes across focus and load-error runs. Each run is a real
//! RSpec capture of a small project (`fixtures/rspec_hunt/<state>`), replayed with `siftr DIR`.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

struct Home {
    home: TempDir,
    project: TempDir,
}

impl Home {
    fn new() -> Self {
        Home {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn siftr(&self, args: &[&str]) -> Value {
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
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Ingests each state in order; the changes of the last.
    fn runs(&self, states: &[&str]) -> Value {
        let mut last = Value::Null;
        for state in states {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/rspec_hunt")
                .join(state);
            last = self.siftr(&["-j", dir.to_str().unwrap(), "--context", "hunt"]);
        }
        last
    }
}

/// `(kind, template)` of each signal, in rank order.
fn signals(changes: &Value) -> Vec<(String, String)> {
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
            (text(&s["kind"]), text(&s["behavior"]["template"]))
        })
        .collect()
}

fn has(changes: &Value, kind: &str, template: &str) -> bool {
    signals(changes)
        .iter()
        .any(|(k, t)| k == kind && t == template)
}

fn baseline_runs(changes: &Value) -> usize {
    changes["baseline_runs"].as_array().unwrap().len()
}

const FAILURE: &str = "./spec/a_spec.rb # a a1";
const WARNING: &str = "DEPRECATION: old api";

/// The regression a failing a1 and a new warning make, with how many baseline runs saw it.
fn regression(changes: &Value) -> (usize, bool, bool, bool) {
    (
        baseline_runs(changes),
        has(changes, "error", FAILURE),
        has(changes, "new", WARNING),
        signals(changes).iter().any(|(k, _)| k == "incomplete"),
    )
}

/// hunt2 #4 (s2): the suite grows from 4 to 10 examples in the run that regresses.
#[test]
fn a_suite_that_grew_keeps_its_history_as_baseline() {
    let control = Home::new().runs(&["a10_clean", "a10_clean", "a10_clean", "a10_fail_warn"]);
    assert_eq!(regression(&control), (3, true, true, false), "control");
    let grown = Home::new().runs(&["a4_clean", "a4_clean", "a4_clean", "a10_fail_warn"]);
    assert_eq!(regression(&grown), (3, true, true, false));
}

/// hunt2 #4 (s6): a spec file that failed to load in every earlier run is fixed in the run that regresses.
#[test]
fn fixing_a_spec_file_that_never_loaded_keeps_its_history() {
    let fixed = Home::new().runs(&[
        "c_load_error_clean",
        "c_load_error_clean",
        "c_load_error_clean",
        "c_fixed_fail_warn",
    ]);
    assert_eq!(regression(&fixed), (3, true, true, false));
}

/// hunt2 #3 (s5): `--fail-fast` and a4 failing every run; a new warning in a2, which runs before a4.
#[test]
fn a_red_fail_fast_suite_compares_runs_that_stopped_alike() {
    let changes = Home::new().runs(&["ff10_fail", "ff10_fail", "ff10_fail", "ff10_fail_warn"]);
    assert_eq!(baseline_runs(&changes), 3);
    assert_eq!(signals(&changes), [("new".to_owned(), WARNING.to_owned())]);
}

/// hunt2 #5 (s3): a spec file of 16 examples is deleted in the run a warning goes from once to three times.
#[test]
fn a_suite_that_shrank_is_complete_and_its_regression_shows() {
    let frequency = |changes: &Value| {
        changes["signals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["kind"] == "frequency" && s["behavior"]["template"] == WARNING)
            .map(|s| (s["current"].as_f64(), s["baseline"]["median"].as_f64()))
    };
    let control = Home::new().runs(&["a20_warn1", "a20_warn1", "a20_warn1", "a20_warn3"]);
    assert_eq!(frequency(&control), Some((Some(3.0), Some(1.0))), "control");

    let home = Home::new();
    let shrunk = home.runs(&["a20_warn1", "a20_warn1", "a20_warn1", "a4_warn3"]);
    assert_eq!(frequency(&shrunk), Some((Some(3.0), Some(1.0))));
    assert!(signals(&shrunk).iter().all(|(k, _)| k != "incomplete"));
    let later = home.runs(&["a4_warn3"]);
    let open: Vec<&str> = later["open_signals"]
        .as_array()
        .map(|open| open.iter().filter_map(|s| s["kind"].as_str()).collect())
        .unwrap_or_default();
    assert!(!open.contains(&"incomplete"), "{open:?}");
}

/// hunt2 #5 (s3), what a reader sees: the deleted file's 16 examples are one change ranked below the warning,
/// and later runs remind only of the warning.
#[test]
fn a_deleted_spec_file_is_one_change_and_never_a_reminder() {
    let home = Home::new();
    let shrunk = home.runs(&["a20_warn1", "a20_warn1", "a20_warn1", "a4_warn3"]);
    let signals = shrunk["signals"].as_array().unwrap();
    let groups: Vec<(&str, &str, usize)> = shrunk["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| {
            let head = signals.iter().find(|s| s["id"] == g["headline"]).unwrap();
            (
                head["kind"].as_str().unwrap(),
                head["behavior"]["template"].as_str().unwrap(),
                g["signals"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(groups.len(), 2, "{groups:?}");
    assert_eq!(groups[0], ("frequency", WARNING, 1));
    assert_eq!(
        (groups[1].0, groups[1].2),
        ("disappeared", 16),
        "{groups:?}"
    );
    assert!(groups[1].1.starts_with("./spec/b_spec.rb # "), "{groups:?}");

    for later in ["r5", "r6"] {
        let changes = home.runs(&["a4_warn3"]);
        let open: Vec<&str> = changes["open_signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect();
        assert_eq!(open, ["frequency"], "{later}");
    }
}

/// `(outcome, resolved_in, recurred_in)` of the ERROR on a1 raised in r4.
fn failure_outcome(home: &Home) -> (String, Value, Value) {
    let history = home.siftr(&["history", "--signals", "-j"]);
    let row = history
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["signal"]["kind"] == "error" && row["signal"]["run"] == "r4")
        .expect("the ERROR raised in r4");
    (
        row["outcome"].as_str().unwrap().to_owned(),
        row["resolved_in"].clone(),
        row["recurred_in"].clone(),
    )
}

/// hunt2 #6 (s1): a focus run of the same command, between two failing runs, neither resolves the failure.
#[test]
fn a_focus_run_is_incomplete_and_resolves_nothing() {
    let home = Home::new();
    let focus = home.runs(&[
        "a10_clean",
        "a10_clean",
        "a10_clean",
        "a10_fail",
        "a10_fail_focus",
    ]);
    assert_eq!(
        signals(&focus)
            .iter()
            .map(|(k, _)| k.as_str())
            .collect::<Vec<_>>(),
        ["incomplete"]
    );
    home.runs(&["a10_fail"]);
    assert_eq!(
        failure_outcome(&home),
        ("open".to_owned(), Value::Null, Value::Null)
    );
}

/// hunt2 #6, as the README demo met it: the run after the failure can't load the failing example's file.
#[test]
fn a_load_error_run_does_not_resolve_a_failure_the_next_clean_run_does() {
    let home = Home::new();
    home.runs(&[
        "a10_clean",
        "a10_clean",
        "a10_clean",
        "a10_fail",
        "a10_a_spec_load_error",
        "a10_clean",
    ]);
    assert_eq!(
        failure_outcome(&home),
        ("resolved".to_owned(), Value::from("r6"), Value::Null)
    );
}
