//! `siftr DIR`: replaying a captured scenario records every stream byte for byte.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn fixture(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/rails_demo")
        .join(scenario)
}

fn siftr(home: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(home.path())
        .env("SIFTR_HOME", home.path())
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn a_scenario_is_replayed_into_the_runs_capture_byte_for_byte() {
    let home = tempfile::tempdir().unwrap();
    let dir = fixture("n_plus_one");
    let run = json(&siftr(&home, &["-j", dir.to_str().unwrap()]));

    let run_dir = home
        .path()
        .join("runs")
        .join(run["run"]["id"].as_str().unwrap());
    let streams = [
        ("stdout.txt", "stdout.log"),
        ("stderr.txt", "stderr.log"),
        ("rspec.ndjson", "file-rspec-events.log"),
        ("test.log", "file-log_test.log"),
    ];
    let mut lines = 0;
    for (source, captured) in streams {
        let expected = std::fs::read(dir.join(source)).unwrap();
        lines += expected.iter().filter(|&&b| b == b'\n').count() as u64;
        // The store creates a stream's capture on its first byte, so an empty stream has none.
        let actual = std::fs::read(run_dir.join(captured)).unwrap_or_default();
        assert!(actual == expected, "{captured} differs from {source}");
    }
    assert_eq!(run["run"]["lines"], lines, "every stream was analyzed");
    assert_eq!(run["run"]["exit_code"], 0);
}

#[test]
fn the_scenarios_exit_code_is_recorded_but_the_replay_still_succeeds() {
    let home = tempfile::tempdir().unwrap();
    let dir = fixture("fail");
    let run = json(&siftr(&home, &["-j", dir.to_str().unwrap()]));
    assert_eq!(run["run"]["exit_code"], 1);
}

/// A directory has to say it is a capture before siftr replays it, and the refusal says what would make it
/// one — this is the only thing `siftr DIR` can be, so there is nothing else to fall back to.
#[test]
fn a_directory_without_captured_streams_is_an_error_and_records_nothing() {
    let home = tempfile::tempdir().unwrap();
    let empty = tempfile::tempdir().unwrap();
    let output = siftr(&home, &[empty.path().to_str().unwrap()]);
    let message = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{message}");
    assert!(message.contains("not a captured scenario"), "{message}");
    assert!(message.contains("stdout.txt"), "{message}");
    assert_eq!(siftr(&home, &["history"]).status.code(), Some(1));
}

/// One file is enough, and it need not be stdout: a cargo scenario is stderr and an exit code.
#[test]
fn a_directory_holding_any_one_captured_file_is_a_scenario() {
    for name in ["stdout.txt", "stderr.txt", "rspec.ndjson", "test.log"] {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(name), "one line\n").unwrap();
        let run = json(&siftr(&home, &["-j", dir.path().to_str().unwrap()]));
        assert_eq!(run["run"]["lines"], 1, "{name}");
    }
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("exit_code.txt"), "3\n").unwrap();
    let run = json(&siftr(&home, &["-j", dir.path().to_str().unwrap()]));
    assert_eq!(run["run"]["exit_code"], 3);
}
