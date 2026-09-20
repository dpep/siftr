//! `siftr follow`: that it really streams, and that it never disagrees with `ingest` about a shape.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

/// Lines whose shapes exercise the normalizer, with repeats: three `GET`s are one shape, not three.
const CORPUS: &str = "\
GET /users/1 in 3ms
GET /users/22 in 5ms
GET /users/333 in 4ms
ERROR -- boom id=7
SELECT \"posts\".* FROM \"posts\" WHERE \"posts\".\"id\" IN (1, 2, 3)
SELECT \"posts\".* FROM \"posts\" WHERE \"posts\".\"id\" IN (4)
job 9f1c2e4a-1b3d-4f5e-8a7b-6c9d0e1f2a3b finished
Finished in 1 minute 3.5 seconds
";

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn siftr(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
        command
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME");
        command
    }

    fn piped(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .siftr(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// **The test that catches a fake stream.** One line is written and stdin is deliberately left open, so
/// nothing but genuine streaming can produce output: a run that collects and prints at EOF prints the same
/// bytes, and only the timing tells them apart.
#[test]
fn a_shape_is_reported_while_the_writer_still_holds_stdin_open() {
    let sandbox = Sandbox::new();
    let mut child = sandbox
        .siftr(&["follow"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"GET /users/1 in 3ms\n").unwrap();
    stdin.flush().unwrap();

    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(out).read_line(&mut line);
        let _ = tx.send(read.map(|_| line));
    });
    let line = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("a shape before EOF, with stdin still open")
        .unwrap();
    assert!(line.contains("GET /users/<int> in <duration>"), "{line}");

    // Only now: holding `stdin` until here is what made the assertion above mean anything.
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

/// A follow and an `ingest` of the same bytes must name the same shapes. Two code paths that disagreed
/// about a template would be worse than no follow at all.
#[test]
fn follow_reports_exactly_the_shapes_ingest_records() {
    let sandbox = Sandbox::new();
    let followed = sandbox.piped(&["follow", "-J"], CORPUS);
    let mut from_follow: Vec<(String, String)> = stdout(&followed)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .map(|row| {
            (
                row["kind"].as_str().unwrap().to_owned(),
                row["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(!from_follow.is_empty(), "{}", stdout(&followed));

    let path = sandbox.project.path().join("app.log");
    std::fs::write(&path, CORPUS).unwrap();
    assert!(
        sandbox
            .siftr(&["ingest", "--context", "c", path.to_str().unwrap()])
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .status
            .success()
    );
    let summary = sandbox
        .siftr(&["summary", "-j", "-n", "500"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let mut from_ingest: Vec<(String, String)> = serde_json::from_slice::<Value>(&summary.stdout)
        .unwrap()["behaviors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["behavior"]["kind"].as_str().unwrap().to_owned(),
                row["behavior"]["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect();

    from_follow.sort();
    from_ingest.sort();
    assert_eq!(from_follow, from_ingest);
}

/// The point of first-seen reporting: most of a log's lines repeat a shape already reported.
#[test]
fn a_shape_already_reported_is_never_reported_again() {
    let sandbox = Sandbox::new();
    let repeated = "GET /users/1 in 3ms\n".repeat(100) + "ERROR -- boom id=7\n";
    let out = sandbox.piped(&["follow"], &repeated);
    let printed = stdout(&out);
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[0].contains("GET /users/<int> in <duration>"),
        "{lines:#?}"
    );
    assert!(lines[1].contains("boom id=<int>"), "{lines:#?}");
}

/// A stream's last line may carry no newline, and `ingest` records it, so a follow must report it too.
#[test]
fn a_final_line_without_a_newline_is_still_a_shape() {
    let sandbox = Sandbox::new();
    let out = sandbox.piped(&["follow"], "one 1\ntwo 2");
    assert_eq!(stdout(&out).lines().count(), 2, "{}", stdout(&out));
}

#[test]
fn pretty_json_is_a_usage_error_because_a_follow_has_no_end() {
    let sandbox = Sandbox::new();
    let out = sandbox
        .siftr(&["-j", "follow"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let document: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(document["error"]["code"], "usage");
    assert!(
        document["error"]["message"]
            .as_str()
            .unwrap()
            .contains("-J"),
        "{document}"
    );
}
