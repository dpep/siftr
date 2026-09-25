//! A failing test must be reported as a failure, whether or not its author gave it a name.
//!
//! `it { … }` declares no description, so RSpec writes one from the matcher that ran once the
//! example finishes. Keying a behavior on those words moves its identity with its outcome, and a
//! regression then reads as NEW + DISAPPEARED — which is what a rename looks like, inviting the
//! developer to dismiss a red suite as a refactor. Replays the two-example repro from
//! `docs/findings/oss-corpus.md`.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

/// The listener's events for run `run` of the repro: a named example and an `it { … }`, both passing
/// or both failing on the same expectation, plus an `it { … }` that always passes and whose matcher
/// renders an object address — a fresh one every run.
fn events(broken: bool, run: usize) -> String {
    let (status, failures) = if broken { ("failed", 1) } else { ("passed", 0) };
    // RSpec renders the matcher's argument, so the generated words follow the code under test.
    let generated = if broken {
        "is expected to eq 2"
    } else {
        "is expected to eq 1"
    };
    let addressed = format!("is expected to eq #<Object:0x000000010{run:07x}>");
    [
        r#"{"event":"start","expected":3,"defined":3,"load_time":0.1}"#.to_owned(),
        example("1:1", "named example", "outcome stability named example", None, status),
        example(
            "1:2",
            generated,
            &format!("outcome stability {generated}"),
            Some("outcome stability"),
            status,
        ),
        example(
            "1:3",
            &addressed,
            &format!("outcome stability {addressed}"),
            Some("outcome stability"),
            "passed",
        ),
        format!(
            r#"{{"event":"summary","duration":0.02,"load_time":0.1,"examples":3,"failures":{},"pending":0,"errors_outside_of_examples":0}}"#,
            failures * 2
        ),
    ]
    .join("\n")
}

fn example(
    id: &str,
    description: &str,
    full: &str,
    declared: Option<&str>,
    status: &str,
) -> String {
    let declared = declared.map_or(String::new(), |d| {
        format!(r#","declared_full_description":"{d}""#)
    });
    let exception = if status == "failed" {
        r#","exception":{"class":"RSpec::Expectations::ExpectationNotMetError","message":"expected"}"#
    } else {
        ""
    };
    format!(
        r#"{{"event":"example","id":"./spec/pair_spec.rb[{id}]","description":"{description}","full_description":"{full}"{declared},"file_path":"./spec/pair_spec.rb","line_number":3,"status":"{status}","run_time":0.001{exception}}}"#
    )
}

/// Ingests `runs` in order into one context and returns the last one's changes.
fn replay(runs: &[bool]) -> Value {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let mut changes = Value::Null;
    for (run, &broken) in runs.iter().enumerate() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("rspec.ndjson"), events(broken, run)).unwrap();
        std::fs::write(
            dir.path().join("exit_code.txt"),
            if broken { "1" } else { "0" },
        )
        .unwrap();
        changes = ingest(home.path(), project.path(), dir.path());
    }
    changes
}

fn ingest(home: &Path, project: &Path, dir: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
        .arg("-j")
        .arg(dir)
        .args(["--context", "pair"])
        .current_dir(project)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.code().is_some_and(|code| code < 2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// `(kind, template)` of every signal about a `test.example`.
fn examples(changes: &Value) -> Vec<(&str, &str)> {
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["behavior"]["kind"] == "test.example")
        .map(|s| {
            (
                s["kind"].as_str().unwrap(),
                s["behavior"]["template"].as_str().unwrap(),
            )
        })
        .collect()
}

#[test]
fn both_examples_are_reported_as_failing_not_as_renamed() {
    let changes = replay(&[false, false, false, false, true]);
    let mut signals = examples(&changes);
    signals.sort_unstable();
    assert_eq!(
        signals,
        [
            (
                "error",
                "./spec/pair_spec.rb # outcome stability <unnamed example>"
            ),
            (
                "error",
                "./spec/pair_spec.rb # outcome stability named example"
            ),
        ]
    );
}

/// The address half of the same bug: on a suite nobody touched, an address in the generated
/// description made a fresh behavior every run — 61% of the signals in the faraday replay.
#[test]
fn a_green_rerun_says_nothing_about_any_example() {
    let changes = replay(&[false, false, false, false, false]);
    assert_eq!(examples(&changes), [] as [(&str, &str); 0]);
}
