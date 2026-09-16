//! Wrapping stays invisible when siftr's reader goes away or its store can't keep up: the command starts, streams
//! and ends as it would unwrapped, and recording is what gives.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

fn siftr(home: &TempDir, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
    command
        .args(args)
        .current_dir(home.path())
        .env("SIFTR_HOME", home.path())
        .stdin(Stdio::null());
    command
}

fn runs(home: &TempDir) -> Value {
    let output = siftr(home, &["history", "-j"]).output().unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Like `CMD | head -1`: reads one line and goes away. `None` if the command still ran 5s later.
fn first_line_then_close(mut command: Command) -> (String, Option<ExitStatus>, Duration) {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    while started.elapsed() < Duration::from_secs(5) {
        if let Some(status) = child.try_wait().unwrap() {
            return (line, Some(status), started.elapsed());
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    (line, None, started.elapsed())
}

#[test]
fn a_reader_that_goes_away_stops_the_command_as_it_would_unwrapped() {
    let mut bare = Command::new("sh");
    bare.args(["-c", "yes"]).stdin(Stdio::null());
    let (line, bare, _) = first_line_then_close(bare);
    assert_eq!(line, "y\n");
    let bare = bare.expect("bare `yes` stops once its reader is gone");

    let home = tempfile::tempdir().unwrap();
    let (line, wrapped, _) = first_line_then_close(siftr(&home, &["run", "--", "sh", "-c", "yes"]));
    assert_eq!(line, "y\n");
    // Not timed: this run records, and recording runs after the command is already dead. What the command
    // actually waits for is timed store-free below.
    let wrapped = wrapped.expect("siftr kept the command running after its reader went away");
    assert_eq!(
        (wrapped.code(), wrapped.signal()),
        (bare.code(), bare.signal()),
        "same status as unwrapped"
    );
    let runs = runs(&home);
    assert_eq!(
        runs[0]["interrupted"], 13,
        "its capture is cut short, so never a baseline: {runs}"
    );
}

/// How fast siftr gets out of the command's way, with nothing else in the measurement: `SIFTR_HOME` is a file, so
/// the store never opens. The run above records instead, and recording happens after the command is already dead —
/// it costs ~0.9s on a loaded machine against ~0.1s of propagation, so a clock spanning both times the store, not
/// siftr. Propagation measured ~11ms idle and ~137ms at 6x oversubscription; the bound is `run`'s `ORPHAN_GRACE`,
/// the shortest wait a regression here could park on.
///
/// The status is compared with an unwrapped run rather than named: whether a shell whose reader went away dies by
/// SIGPIPE or reports 128+SIGPIPE is the platform's `/bin/sh` deciding whether it exec'd the command or waited on
/// it, which siftr has no part in. Parity is also the stronger claim — it is what fails if a store that can't open
/// ever does change the command's result.
#[test]
fn getting_out_of_the_way_waits_for_nothing() {
    let mut bare = Command::new("sh");
    bare.args(["-c", "yes"]).stdin(Stdio::null());
    let (line, bare, _) = first_line_then_close(bare);
    assert_eq!(line, "y\n");
    let bare = bare.expect("bare `yes` stops once its reader is gone");

    let home = tempfile::tempdir().unwrap();
    let not_a_dir = home.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let mut command = siftr(&home, &["run", "--", "sh", "-c", "yes"]);
    command.env("SIFTR_HOME", &not_a_dir);

    let (line, wrapped, elapsed) = first_line_then_close(command);
    assert_eq!(line, "y\n");
    let wrapped = wrapped.expect("siftr kept the command running after its reader went away");
    assert_eq!(
        (wrapped.code(), wrapped.signal()),
        (bare.code(), bare.signal()),
        "same status as unwrapped, though the store never opened"
    );
    assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
}

/// `siftr run -- sh -c 'echo first; exit 3'`: when its first line arrived, and its output. `release` frees the store
/// once that line is in, or after 3s, so a siftr that waits for the store fails the test rather than hanging it.
fn first_line_while_busy(
    home: &TempDir,
    release: impl FnOnce() + Send + 'static,
) -> (String, Duration, Output) {
    let (line_in, wait) = mpsc::channel::<()>();
    let releaser = thread::spawn(move || {
        let _ = wait.recv_timeout(Duration::from_secs(3));
        release();
    });
    let started = Instant::now();
    let mut child = siftr(home, &["run", "--", "sh", "-c", "echo first; exit 3"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let first = started.elapsed();
    let _ = line_in.send(());
    releaser.join().unwrap();
    (line, first, child.wait_with_output().unwrap())
}

fn assert_recorded_after_the_wait(home: &TempDir, run: &str, output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    let runs = runs(home);
    assert_eq!(
        (&runs[0]["id"], &runs[0]["exit_code"], &runs[0]["lines"]),
        (&Value::from(run), &Value::from(3), &Value::from(1)),
        "output from before the store was ready is recorded too: {stderr}"
    );
    let captured = std::fs::read(home.path().join(format!("runs/{run}/stdout.log"))).unwrap();
    assert_eq!(captured, b"first\n");
}

#[test]
fn a_store_another_siftr_is_migrating_does_not_delay_the_command() {
    let home = tempfile::tempdir().unwrap();
    let lock = File::create(home.path().join("siftr.lock")).unwrap();
    lock.lock().unwrap();
    let (line, first, output) = first_line_while_busy(&home, move || lock.unlock().unwrap());
    assert_eq!(line, "first\n");
    assert!(first < Duration::from_secs(1), "{first:?}");
    assert_recorded_after_the_wait(&home, "r1", &output);
}

#[test]
fn a_store_busy_writing_does_not_hold_back_the_output() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        siftr(&home, &["run", "-q", "--", "true"])
            .status()
            .unwrap()
            .success()
    );
    let db = rusqlite::Connection::open(home.path().join("siftr.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; UPDATE runs SET lines = lines;")
        .unwrap();
    let (line, first, output) =
        first_line_while_busy(&home, move || db.execute_batch("COMMIT").unwrap());
    assert_eq!(line, "first\n");
    assert!(first < Duration::from_secs(1), "{first:?}");
    assert_recorded_after_the_wait(&home, "r2", &output);
}

#[test]
fn output_that_outgrows_the_wait_for_a_busy_store_passes_through_unrecorded() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        siftr(&home, &["run", "-q", "--", "true"])
            .status()
            .unwrap()
            .success()
    );
    let db = rusqlite::Connection::open(home.path().join("siftr.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; UPDATE runs SET lines = lines;")
        .unwrap();
    let started = Instant::now();
    let output = siftr(
        &home,
        &[
            "run",
            "--",
            "sh",
            "-c",
            "head -c 20000000 /dev/zero; exit 3",
        ],
    )
    .output()
    .unwrap();
    let elapsed = started.elapsed();
    db.execute_batch("COMMIT").unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.stdout.len(), 20_000_000, "{stderr}");
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        elapsed < siftr::store::BUSY_WAIT,
        "gave up at the buffer's bound, not after the store's own wait: {elapsed:?}"
    );
    assert_eq!(
        stderr
            .matches("siftr: warning: not recording this run:")
            .count(),
        1,
        "{stderr}"
    );
    assert_eq!(
        runs(&home).as_array().map(Vec::len),
        Some(1),
        "only the warm-up run"
    );
}

#[test]
fn a_store_that_stays_busy_costs_the_run_its_recording_and_says_so() {
    let home = tempfile::tempdir().unwrap();
    assert!(
        siftr(&home, &["run", "-q", "--", "true"])
            .status()
            .unwrap()
            .success()
    );
    let db = rusqlite::Connection::open(home.path().join("siftr.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE; UPDATE runs SET lines = lines;")
        .unwrap();
    let started = Instant::now();
    let output = siftr(&home, &["-j", "run", "--", "sh", "-c", "exit 3"])
        .output()
        .unwrap();
    let elapsed = started.elapsed();
    db.execute_batch("COMMIT").unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(
        elapsed < siftr::store::BUSY_WAIT * 2,
        "exits once the store's own wait ends: {elapsed:?}"
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["not_recorded"]["code"], "busy", "{document}");
    assert!(
        stderr.contains("not recording this run:") && stderr.contains("is busy"),
        "{stderr}"
    );
}

#[test]
fn json_is_still_a_document_when_the_run_is_not_recorded() {
    let home = tempfile::tempdir().unwrap();
    let not_a_dir = home.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let output = siftr(&home, &["-j", "run", "--", "sh", "-c", "exit 3"])
        .env("SIFTR_HOME", &not_a_dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    let document: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {:?} {stderr}", output.stdout));
    assert_eq!(document["run"], Value::Null, "{document}");
    assert_eq!(document["signals"], serde_json::json!([]), "{document}");
    assert_eq!(document["not_recorded"]["code"], "failed", "{document}");
    assert!(
        document["not_recorded"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&*not_a_dir.to_string_lossy())),
        "{document}"
    );
}
