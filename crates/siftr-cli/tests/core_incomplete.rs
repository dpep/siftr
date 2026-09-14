//! A current run that didn't run the suite, replayed with `siftr ingest --dir` after clean captures: its one
//! change is the incompleteness, not what it never ran.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn ingest(home: &Path, project: &Path, scenario: &str) -> Value {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rails_demo")
        .join(scenario);
    let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(["ingest", "--context", "rails_demo", "-j", "--dir"])
        .arg(dir)
        .current_dir(project)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{scenario}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// `(group, headline, kind, measure, current, template)` per signal, in rank order.
fn signals_after_clean_runs(scenario: &str) -> Vec<(u64, bool, String, String, f64, String)> {
    let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for clean in ["baseline", "baseline_2", "baseline_documentation"] {
        ingest(home.path(), project.path(), clean);
    }
    let changes = ingest(home.path(), project.path(), scenario);
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["group"].as_u64().unwrap(),
                s["headline"].as_bool().unwrap(),
                s["kind"].as_str().unwrap().to_owned(),
                s["measure"].as_str().unwrap().to_owned(),
                s["current"].as_f64().unwrap(),
                s["behavior"]["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn signal(
    group: u64,
    kind: &str,
    measure: &str,
    current: f64,
    template: &str,
) -> (u64, bool, String, String, f64, String) {
    (
        group,
        true,
        kind.to_owned(),
        measure.to_owned(),
        current,
        template.to_owned(),
    )
}

/// A syntax error in `users_spec.rb`: its two examples and their requests never ran.
#[test]
fn a_spec_file_that_failed_to_load_is_the_one_change() {
    assert_eq!(
        signals_after_clean_runs("load_error"),
        [signal(
            1,
            "incomplete",
            "count",
            1.0,
            "SyntaxError: While loading ./spec/requests/users_spec.rb a `raise SyntaxError` occurred, RSpec will now quit."
        )]
    );
}

#[test]
fn a_fail_fast_stop_is_a_change_beside_the_failure_that_stopped_it() {
    assert_eq!(
        signals_after_clean_runs("fail_fast"),
        [
            signal(1, "incomplete", "examples", 6.0, "rspec"),
            signal(
                2,
                "error",
                "failed",
                1.0,
                "./spec/models/user_spec.rb # User requires an email"
            ),
        ]
    );
}
