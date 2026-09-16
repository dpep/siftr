//! `-j` prints one JSON document on stdout, whatever happens: results, nothing found, or an error.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};

fn siftr(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(dir)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout {:?}, stderr {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn nothing_found_is_still_a_document() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cases: [(&[&str], Value); 4] = [
        (
            &["-j", "changes"],
            json!({ "run": null, "behaviors": 0, "streams": null, "baseline_runs": [], "skipped_runs": [], "changes": 0, "groups": [], "signals": [], "open_signals": [] }),
        ),
        (
            &["-j", "summary"],
            json!({ "run": null, "behaviors_total": 0, "behaviors": [] }),
        ),
        (&["-j", "history"], json!([])),
        (&["-j", "history", "--signals"], json!([])),
    ];
    for (args, expected) in cases {
        let output = siftr(home.path(), project.path(), args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(document(&output), expected, "{args:?}");
        assert!(
            output.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // A behavior recorded in another project hasn't occurred in this one.
    let elsewhere = tempfile::tempdir().unwrap();
    let recorded = siftr(
        home.path(),
        elsewhere.path(),
        &["run", "-q", "--", "echo", "hello"],
    );
    assert!(recorded.status.success());
    let summary = document(&siftr(home.path(), elsewhere.path(), &["summary", "-j"]));
    let behavior = summary["behaviors"][0]["behavior"]["id"].as_str().unwrap();
    let output = siftr(home.path(), project.path(), &["evidence", behavior, "-j"]);
    assert_eq!(output.status.code(), Some(1));
    let evidence = document(&output);
    assert_eq!(
        (
            &evidence["behavior"]["id"],
            &evidence["run"],
            &evidence["exemplars"]
        ),
        (&json!(behavior), &Value::Null, &json!([]))
    );
}

#[test]
fn an_error_is_a_document_with_a_stable_code() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let cases: [(&[&str], &str); 6] = [
        (&["-j", "changes", "bogus"], "usage"),
        (&["summary", "--json", "--by", "size"], "usage"),
        (&["-j", "evidence", "s1"], "usage"),
        (&["-j", "changes", "r99"], "not_found"),
        (&["-j", "explain", "s99"], "not_found"),
        (&["-j", "ack", "s1"], "not_found"),
    ];
    for (args, code) in cases {
        let output = siftr(home.path(), project.path(), args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        let error = &document(&output)["error"];
        assert_eq!(error["code"], code, "{args:?}: {error}");
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            !message.is_empty() && !message.contains('\n') && !message.starts_with("error"),
            "{args:?}: {error}"
        );
    }

    let wrong_id = siftr(home.path(), project.path(), &["evidence", "s1"]);
    assert_eq!(wrong_id.status.code(), Some(2));
    assert!(wrong_id.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&wrong_id.stderr);
    assert!(stderr.contains("siftr explain s1"), "{stderr}");

    let help = siftr(home.path(), project.path(), &["-j", "changes", "--help"]);
    assert_eq!(help.status.code(), Some(0), "help is not an error");
    assert!(String::from_utf8_lossy(&help.stdout).contains("Usage:"));
}
