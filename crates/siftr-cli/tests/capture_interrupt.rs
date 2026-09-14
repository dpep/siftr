//! An interrupted `siftr run` is recorded as evidence, flagged `interrupted`, and excluded from any
//! baseline a later run of the same command would build — so it never reads as mass DISAPPEARED.

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;

fn siftr(home: &TempDir, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
    command
        .args(args)
        .current_dir(home.path())
        .env("SIFTR_HOME", home.path())
        // Never the terminal cargo runs in: siftr would take the foreground path, leaving Ctrl-C to the
        // terminal instead of forwarding it itself (the non-TTY path this test exercises).
        .stdin(Stdio::null());
    command
}

/// Same command for every run in this test, so they share one context and baseline. Long enough that an
/// interrupt sent right after "ready" reliably lands mid-sleep.
const SCRIPT: &str = "echo ready; sleep 0.3; echo done";

#[test]
fn an_interrupted_run_is_recorded_but_never_joins_a_later_baseline() {
    let home = tempfile::tempdir().unwrap();

    // Two clean baseline runs.
    for _ in 0..2 {
        let output = siftr(&home, &["run", "--", "sh", "-c", SCRIPT])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    // A third run of the same command, interrupted before it can finish.
    let mut child = siftr(&home, &["run", "--", "sh", "-c", SCRIPT])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    kill_process(Pid::from_child(&child), Signal::INT).unwrap();

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.signal(),
        Some(2),
        "the child doesn't trap SIGINT, so it dies from the forwarded signal and siftr re-raises it: {stderr}"
    );
    assert!(
        stderr.contains("r3: interrupted by signal 2"),
        "a one-line note, not a behavioral-change summary: {stderr}"
    );

    let history = siftr(&home, &["history", "-j"]).output().unwrap();
    let runs: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(
        runs[0]["interrupted"], 2,
        "the interrupted run, listed with its signal: {runs}"
    );

    // A fourth, clean run of the same command: its baseline must be the two clean runs, not the interrupted one.
    let output = siftr(&home, &["run", "--", "sh", "-c", SCRIPT])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    let changes = siftr(&home, &["changes", "-j"]).output().unwrap();
    let changes: serde_json::Value = serde_json::from_slice(&changes.stdout).unwrap();
    let mut baseline_runs: Vec<&str> = changes["baseline_runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    baseline_runs.sort_unstable();
    assert_eq!(
        baseline_runs,
        ["r1", "r2"],
        "the interrupted run r3 is excluded: {changes}"
    );
    let disappeared: Vec<&serde_json::Value> = changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == "disappeared")
        .collect();
    assert!(
        disappeared.is_empty(),
        "the interrupted run's missing tail must not read as behaviors disappearing: {changes}"
    );
}
