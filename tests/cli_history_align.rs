//! `siftr history` keeps its columns aligned once run ids reach two digits.

use std::path::Path;
use std::process::Command;

#[test]
fn history_columns_align_across_one_and_two_digit_run_ids() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/baseline");
    let siftr = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(project.path())
            .env("SIFTR_HOME", home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    };
    for _ in 0..11 {
        let ingest = siftr(&[fixture.to_str().unwrap(), "--context", "demo"]);
        assert!(
            ingest.status.success(),
            "{}",
            String::from_utf8_lossy(&ingest.stderr)
        );
    }

    let output = siftr(&["history", "--context", "demo"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let age_column = |id: &str| {
        let row = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no row for {id}:\n{stdout}"));
        row.find(" ago").unwrap()
    };
    assert_eq!(age_column("r9"), age_column("r11"), "\n{stdout}");
}

#[test]
fn history_agrees_line_counts_with_their_noun_and_keeps_the_columns() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let siftr = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(project.path())
            .env("SIFTR_HOME", home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    };
    for (name, text) in [("one.log", "only line\n"), ("two.log", "first\nsecond\n")] {
        let path = project.path().join(name);
        std::fs::write(&path, text).unwrap();
        let ingest = siftr(&[path.to_str().unwrap(), "--context", "logs"]);
        assert!(
            ingest.status.success(),
            "{}",
            String::from_utf8_lossy(&ingest.stderr)
        );
    }

    let output = siftr(&["history", "--context", "logs"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let row = |id: &str| {
        stdout
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no row for {id}:\n{stdout}"))
            .to_owned()
    };
    assert!(row("r1").contains(" 1 line "), "\n{stdout}");
    assert!(row("r2").contains(" 2 lines "), "\n{stdout}");
    assert_eq!(
        row("r1").find(" change"),
        row("r2").find(" change"),
        "\n{stdout}"
    );
}

/// A Ctrl-C'd read is routine now that `tail -f log | siftr` is a front door, so its row has to line up with
/// the rest rather than pushing every column after it to the right.
#[test]
fn history_keeps_its_columns_when_a_read_was_interrupted() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;

    use rustix::process::{Pid, Signal, kill_process};

    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let siftr = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
        command
            .current_dir(project.path())
            .env("SIFTR_HOME", home.path())
            .env_remove("XDG_DATA_HOME");
        command
    };

    let mut child = siftr()
        .args(["-", "--context", "app"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"GET /users/1 in 3ms\n").unwrap();
    stdin.flush().unwrap();
    // Sync on the streamed behavior, so the interrupt lands on a run that has read something.
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    kill_process(Pid::from_child(&child), Signal::INT).unwrap();
    child.wait().unwrap();
    drop(stdin);

    let mut clean = siftr()
        .args(["-", "--context", "app"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    clean
        .stdin
        .take()
        .unwrap()
        .write_all(b"GET /users/2 in 4ms\n")
        .unwrap();
    assert!(clean.wait().unwrap().success());

    let output = siftr()
        .args(["history", "--context", "app"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Up to the verdict the rows are the same columns; a marker after it pushes the command right, which is
    // how `incomplete` has always read.
    let lines_column = |id: &str| {
        let row = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no row for {id}:\n{stdout}"));
        row.find(" line")
            .unwrap_or_else(|| panic!("no line count in {row:?}:\n{stdout}"))
    };
    assert_eq!(
        lines_column("r1"),
        lines_column("r2"),
        "the interrupted row keeps the columns:\n{stdout}"
    );
    assert!(
        stdout.contains("interrupted (signal 2)"),
        "and still says it was interrupted:\n{stdout}"
    );
    assert!(
        !stdout.contains("incomplete"),
        "the interruption is why it is incomplete, so it says that once:\n{stdout}"
    );
}
