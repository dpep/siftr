//! `siftr history` exits as the other query commands do: 0 results, 1 empty, 2 error.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn history(home: &std::path::Path, project: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .arg("history")
        .args(args)
        .current_dir(project)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

#[test]
fn an_unknown_context_is_not_found_as_in_changes() {
    let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for args in [
        &["--context", "nope"][..],
        &["--signals", "--context", "nope"],
    ] {
        let json = history(home.path(), project.path(), &[args, &["-j"]].concat());
        assert_eq!(json.status.code(), Some(2), "{args:?}");
        let error: Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(error["error"]["code"], "not_found", "{args:?}");

        let human = history(home.path(), project.path(), args);
        assert_eq!(human.status.code(), Some(2), "{args:?}");
        let stderr = String::from_utf8_lossy(&human.stderr);
        assert!(stderr.contains("\"nope\""), "{args:?}: {stderr}");
    }
}

/// A run that didn't run its suite reads differently from one that failed: `history` says so, as JSON does.
#[test]
fn an_incomplete_run_is_marked_in_the_run_table() {
    let (home, project) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for scenario in [
        "baseline",
        "baseline_2",
        "baseline_documentation",
        "load_error",
    ] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/rails_demo")
            .join(scenario);
        let ingest = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(["ingest", "--context", "rails_demo", "--dir"])
            .arg(dir)
            .current_dir(project.path())
            .env("SIFTR_HOME", home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(ingest.status.success(), "{scenario}");
    }
    let table = history(home.path(), project.path(), &[]);
    let table = String::from_utf8_lossy(&table.stdout);
    // The command follows the status column, so a marked row carries the word between them.
    let marked: Vec<&str> = table
        .lines()
        .filter(|line| line.contains("  incomplete  "))
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert_eq!(marked, ["r4"], "{table}");

    let json: Value =
        serde_json::from_slice(&history(home.path(), project.path(), &["-j"]).stdout).unwrap();
    let flags: Vec<(String, Value, Value)> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|run| {
            (
                run["id"].as_str().unwrap().to_owned(),
                run["complete"].clone(),
                run["interrupted"].clone(),
            )
        })
        .collect();
    assert_eq!(
        flags,
        [
            ("r4".to_owned(), Value::from(false), Value::Null),
            ("r3".to_owned(), Value::from(true), Value::Null),
            ("r2".to_owned(), Value::from(true), Value::Null),
            ("r1".to_owned(), Value::from(true), Value::Null),
        ]
    );
}
