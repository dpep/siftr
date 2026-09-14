//! `siftr run` stays invisible: a terminal child still sees a terminal, interrupts arrive once, a signal death
//! propagates, and a background process holding the output doesn't hold siftr.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::os::fd::OwnedFd;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{OptionalActions, OutputModes, Winsize, tcgetattr, tcsetattr, tcsetwinsize};
use tempfile::TempDir;

fn siftr(home: &TempDir, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
    command
        .args(args)
        .current_dir(home.path())
        .env("SIFTR_HOME", home.path())
        // Never the terminal cargo runs in: siftr would take the foreground path and leave Ctrl-C to it.
        .stdin(Stdio::null());
    command
}

/// A terminal for siftr's stdout, 33 rows by 101 columns, that doesn't rewrite what's written to it.
fn terminal() -> (File, OwnedFd) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY,
        Mode::empty(),
    )
    .unwrap();
    let mut termios = tcgetattr(&slave).unwrap();
    termios.output_modes.remove(OutputModes::OPOST);
    tcsetattr(&slave, OptionalActions::Now, &termios).unwrap();
    let size = Winsize {
        ws_row: 33,
        ws_col: 101,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    tcsetwinsize(&slave, size).unwrap();
    (master.into(), slave)
}

#[test]
fn a_terminal_child_sees_a_sized_terminal_and_its_bytes_arrive_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let (mut master, slave) = terminal();
    let script = r#"[ -t 1 ] && echo tty || echo pipe; [ -t 2 ] && echo tty || echo pipe; stty size <&1; printf 'a\033[31mb'"#;
    let mut child = siftr(&home, &["run", "--", "sh", "-c", script])
        .stdout(slave)
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut seen = Vec::new();
    // Once siftr exits the master reads EOF, or EIO on Linux.
    let _ = master.read_to_end(&mut seen);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status:?}");

    let expected = "tty\npipe\n33 101\na\x1b[31mb";
    assert_eq!(
        String::from_utf8_lossy(&seen),
        expected,
        "stdout a terminal, stderr still a pipe"
    );
    let captured = std::fs::read(home.path().join("runs/r1/stdout.log")).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&captured),
        expected,
        "the capture holds the child's bytes"
    );
}

#[test]
fn an_interrupt_reaches_the_child_once_and_the_partial_run_stays_out_of_baselines() {
    let home = tempfile::tempdir().unwrap();
    let script = r#"n=0; trap 'n=$((n+1))' INT; echo ready; while [ $n -eq 0 ]; do sleep 0.05; done; sleep 0.3; echo "interrupts=$n"; exit 7"#;
    let mut child = siftr(&home, &["run", "--", "sh", "-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert_eq!(line, "ready\n");
    kill_process(Pid::from_child(&child), Signal::INT).unwrap();

    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(rest, "interrupts=1\n", "{stderr}");
    assert_eq!(
        output.status.code(),
        Some(7),
        "the child's own exit, as without siftr"
    );
    assert!(stderr.contains("r1: interrupted by signal 2"), "{stderr}");
    assert!(
        home.path().join("runs/r1/stdout.log").exists(),
        "the capture is kept"
    );
    let history = siftr(&home, &["history", "-j"]).output().unwrap();
    let runs: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(
        runs[0]["interrupted"], 2,
        "flagged interrupted, so never in a baseline: {runs}"
    );
    assert_eq!(
        runs[0]["exit_code"], 7,
        "the child's own exit (it trapped SIGINT rather than dying by it): {runs}"
    );
}

#[test]
fn a_child_killed_by_a_signal_takes_siftr_down_the_same_way() {
    let home = tempfile::tempdir().unwrap();
    let output = siftr(&home, &["run", "--", "sh", "-c", "echo bye; kill -TERM $$"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.signal(),
        Some(15),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"bye\n");
    let history = siftr(&home, &["history", "-j"]).output().unwrap();
    let runs: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(runs[0]["exit_code"], 143, "recorded before siftr went down");
}

#[test]
fn a_background_process_holding_the_output_does_not_hold_siftr() {
    let home = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let output = siftr(&home, &["run", "--", "sh", "-c", "sleep 5 & echo started"])
        .output()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"started\n");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("still holds its output"), "{stderr}");
}
