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
            "./spec/requests/users_spec.rb failed to load: SyntaxError: unexpected 'end'; expected a `)` to close the arguments"
        )]
    );
}

/// Errors outside examples that skip no example: `raise SyntaxError` after `users_spec.rb`'s describe block, and an
/// `after(:suite)` hook that raises. Every example ran, so the run is complete and the new error is its change;
/// the next clean run has nothing new and nothing still open.
#[test]
fn a_new_error_outside_examples_that_skipped_nothing_is_the_change() {
    for (scenario, template) in [
        (
            "load_error_after_examples",
            "./spec/requests/users_spec.rb failed to load: SyntaxError: compile error",
        ),
        (
            "after_suite_error",
            "An error occurred in an `after(:suite)` hook: RuntimeError: after suite boom",
        ),
    ] {
        let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for clean in ["baseline", "baseline_2", "baseline_documentation"] {
            ingest(home.path(), project.path(), clean);
        }
        let raised = ingest(home.path(), project.path(), scenario);
        let headlines: Vec<(&str, &str, bool)> = raised["signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["kind"].as_str().unwrap(),
                    s["behavior"]["template"].as_str().unwrap(),
                    s["headline"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(headlines, [("new", template, true)], "{scenario}");
        assert_eq!(raised["run"]["complete"], true, "{scenario}");

        let restored = ingest(home.path(), project.path(), "baseline");
        assert_eq!(restored["signals"], serde_json::json!([]), "{scenario}");
        assert_eq!(
            restored["open_signals"],
            serde_json::json!([]),
            "{scenario}"
        );
    }
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
