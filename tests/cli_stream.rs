//! The one reading path: that `siftr FILE`, `cat log | siftr` and `tail -f log | siftr` really stream while
//! the input is still open, that Ctrl-C keeps what was read, and that a run with nothing to compare against
//! describes its input without claiming anything changed.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};
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

    /// The run's own report, without the behaviors streamed above it.
    fn report(&self, args: &[&str], input: &str) -> String {
        let out = self.piped(args, input);
        assert!(out.status.success(), "{out:?}");
        stdout(&out)
            .lines()
            .skip_while(|line| !line.starts_with('r'))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Reads one line from `child`'s stdout, on a thread, so a child that never writes fails the test rather
/// than hanging it.
fn first_line(child: &mut Child) -> String {
    let out = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    // The thread keeps reading to EOF after sending: dropping the read end early would give siftr EPIPE on
    // the report it prints once the input ends, and the test would be measuring its own plumbing.
    std::thread::spawn(move || {
        let mut reader = BufReader::new(out);
        let mut line = String::new();
        let _ = tx.send(reader.read_line(&mut line).map(|_| line));
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("a behavior before EOF, with stdin still open")
        .unwrap()
}

/// **The test that catches a fake stream.** One line is written and stdin is deliberately left open, so
/// nothing but genuine streaming can produce output: a run that collects and prints at EOF prints the same
/// bytes, and only the timing tells them apart.
#[test]
fn a_behavior_is_reported_while_the_writer_still_holds_stdin_open() {
    let sandbox = Sandbox::new();
    let mut child = sandbox
        .siftr(&["-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"GET /users/1 in 3ms\n").unwrap();
    stdin.flush().unwrap();

    let line = first_line(&mut child);
    assert!(line.contains("GET /users/<int> in <duration>"), "{line}");

    // Only now: holding `stdin` until here is what made the assertion above mean anything.
    drop(stdin);
    assert!(child.wait().unwrap().success());
}

/// The same property under `-J`, which is what an agent reads: rows as they arrive, not one document at the
/// end. `-j` cannot do this, which is the whole reason `-J` exists.
#[test]
fn ndjson_rows_arrive_while_stdin_is_still_open_and_end_with_the_report() {
    let sandbox = Sandbox::new();
    let mut child = sandbox
        .siftr(&["ingest", "-J"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"GET /users/1 in 3ms\n").unwrap();
    stdin.flush().unwrap();

    let row: Value = serde_json::from_str(&first_line(&mut child)).unwrap();
    assert_eq!(row["template"], "GET /users/<int> in <duration>");
    assert_eq!(row["stream"], "stdout");
    drop(stdin);
    assert!(child.wait().unwrap().success());

    // And the stream ends with the comparison, so a consumer that read the rows gets the report from the
    // same pipe rather than having to run a second command. A fresh sandbox: the run above already holds
    // this context's history, and a run that was compared has changes to report instead of a description.
    let out = Sandbox::new().piped(&["ingest", "-J"], CORPUS);
    let printed = stdout(&out);
    let lines: Vec<&str> = printed.lines().collect();
    let (last, rows) = lines.split_last().unwrap();
    let last: Value = serde_json::from_str(last).unwrap();
    assert!(
        last["run"]["id"].is_string() && last["described"]["events"].is_number(),
        "the last line is the whole report: {last}"
    );
    for line in rows {
        let row: Value = serde_json::from_str(line).unwrap();
        assert!(row["template"].is_string(), "{row}");
    }
}

/// Streaming and recording must never disagree about what a behavior is: they are one analyzer, and a
/// second one would be a second answer.
#[test]
fn streamed_behaviors_are_exactly_the_ones_recorded() {
    let sandbox = Sandbox::new();
    let streamed = sandbox.piped(&["ingest", "-J"], CORPUS);
    let mut from_stream: Vec<(String, String)> = stdout(&streamed)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["template"].is_string())
        .map(|row| {
            (
                row["kind"].as_str().unwrap().to_owned(),
                row["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(!from_stream.is_empty(), "{}", stdout(&streamed));

    let summary = sandbox
        .siftr(&["summary", "-j", "-n", "500"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let mut recorded: Vec<(String, String)> = serde_json::from_slice::<Value>(&summary.stdout)
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

    from_stream.sort();
    recorded.sort();
    assert_eq!(from_stream, recorded);
}

/// The point of first-seen reporting: most of a log's lines repeat a behavior already reported, so the
/// stream falls quiet instead of mirroring the input.
#[test]
fn a_behavior_already_reported_is_never_reported_again() {
    let sandbox = Sandbox::new();
    let repeated = "GET /users/1 in 3ms\n".repeat(100) + "ERROR -- boom id=7\n";
    let out = sandbox.piped(&["-"], &repeated);
    let printed = stdout(&out);
    let streamed: Vec<&str> = printed
        .lines()
        .take_while(|line| !line.starts_with('r'))
        .collect();
    assert_eq!(streamed.len(), 2, "{streamed:#?}");
    assert!(
        streamed[0].contains("GET /users/<int> in <duration>"),
        "{streamed:#?}"
    );
    assert!(streamed[1].contains("boom id=<int>"), "{streamed:#?}");
}

/// A stream's last line may carry no newline, and the run records it, so the stream must report it too.
#[test]
fn a_final_line_without_a_newline_is_still_streamed() {
    let sandbox = Sandbox::new();
    let out = sandbox.piped(&["-"], "one 1\ntwo 2");
    let printed = stdout(&out);
    let streamed = printed.lines().take_while(|line| !line.starts_with('r'));
    assert_eq!(streamed.count(), 2, "{printed}");
}

/// What a run with no baseline may say: what the input holds, and nothing that reads as a finding. 67% of a
/// real log's templates are one-offs (`docs/findings/log-contexts.md` §2) and treating novelty as signal
/// produced 10,362 of them, so a first run describes and stops.
#[test]
fn a_first_run_describes_its_input_and_claims_no_change() {
    let sandbox = Sandbox::new();
    let report = sandbox.report(&["-"], CORPUS);
    assert!(
        report.contains("no earlier runs of this context to compare with"),
        "{report}"
    );
    assert!(
        report.contains("in this input, not a comparison:"),
        "{report}"
    );
    assert!(report.contains("carry"), "{report}");
    assert!(report.contains("error lines: 1 on 1 behavior"), "{report}");
    assert!(report.contains("seen once:"), "{report}");
    for claim in ["change", "NEW", "anomal", "unusual", "spike", "regress"] {
        assert!(
            !report.contains(claim),
            "a first run has nothing to be different from, so it must not say {claim:?}: {report}"
        );
    }
}

/// The same block is `described` under `-j`, with its shares already rounded where they were built.
#[test]
fn the_description_is_a_document_too() {
    let sandbox = Sandbox::new();
    let out = sandbox.piped(&["-j", "ingest"], CORPUS);
    let document: Value = serde_json::from_slice(&out.stdout).unwrap();
    let described = &document["described"];
    assert_eq!(described["events"], 8, "{document}");
    assert_eq!(described["errors"]["lines"], 1, "{document}");
    assert!(
        described["head"].as_array().unwrap().len() <= 3,
        "{document}"
    );
    let share = described["seen_once"]["share_of_events"].as_f64().unwrap();
    assert_eq!(
        share,
        siftr::num::round_sig(share, 2),
        "shares carry the two significant figures a ratio of counts has: {document}"
    );
    // Nothing streamed: one pretty document has one beginning and one end.
    assert_eq!(stdout(&out).matches("\n{").count(), 0, "{}", stdout(&out));
}

/// A run that *was* compared has its changes to say, and a description below them would compete with the
/// answer.
#[test]
fn a_compared_run_reports_changes_instead_of_a_description() {
    let sandbox = Sandbox::new();
    for _ in 0..2 {
        assert!(
            sandbox
                .piped(&["ingest", "--context", "app"], CORPUS)
                .status
                .success()
        );
    }
    let report = sandbox.report(&["ingest", "--context", "app"], CORPUS);
    assert!(report.contains("vs 2 baseline runs"), "{report}");
    assert!(
        !report.contains("in this input, not a comparison"),
        "{report}"
    );
}

/// **Ctrl-C must not throw the run away.** The first thing a user does to a `tail -f` is interrupt it, and a
/// siftr that lost what it had read would not be tried twice. The run is kept, reported and described — and,
/// being partial, never allowed into a later baseline, where its missing tail reads as mass DISAPPEARED.
#[test]
fn an_interrupt_keeps_the_run_reports_it_and_never_baselines_it() {
    let sandbox = Sandbox::new();
    // Two clean runs of the same context, to be the baseline a later run should find — and not find the
    // interrupted one in.
    for _ in 0..2 {
        assert!(
            sandbox
                .piped(&["ingest", "--context", "app"], CORPUS)
                .status
                .success()
        );
    }

    let mut child = sandbox
        .siftr(&["ingest", "--context", "app"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // Everything but the last line, so the run is genuinely partial when it is interrupted.
    let (kept, _) = CORPUS.rsplit_once("job ").unwrap();
    stdin.write_all(kept.as_bytes()).unwrap();
    stdin.flush().unwrap();
    // Wait for a behavior to be reported: proof the bytes reached the analyzer before the signal does.
    let streamed = first_line(&mut child);
    assert!(
        streamed.contains("GET /users/<int> in <duration>"),
        "{streamed}"
    );

    kill_process(Pid::from_child(&child), Signal::INT).unwrap();
    // `stdin` is deliberately still open and still held here: the signal alone has to end the read, exactly
    // as it must on a `tail -f` nobody is going to close. An interrupt that only worked because the writer
    // had gone away would prove nothing.
    let status = child.wait().unwrap();
    drop(stdin);
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(2),
        "siftr dies by the signal it was sent, so a shell loop stops: {stderr}"
    );

    let history: Value = serde_json::from_slice(
        &sandbox
            .siftr(&["history", "-j"])
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(history[0]["interrupted"], 2, "{history}");
    assert!(
        history[0]["lines"].as_u64().unwrap() > 0,
        "what was read before the interrupt is kept: {history}"
    );

    // The summary the user sees on Ctrl-C, read back from the same stored run.
    let changes: Value = serde_json::from_slice(
        &sandbox
            .siftr(&["changes", "-j"])
            .stdin(Stdio::null())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(changes["run"]["interrupted"], 2, "{changes}");

    // A fourth, clean run: its baseline is the two clean runs, never the interrupted one.
    let out = sandbox.piped(&["ingest", "--context", "app"], CORPUS);
    assert!(out.status.success(), "{out:?}");
    let report = stdout(&out);
    assert!(report.contains("vs 2 baseline runs (r1 r2)"), "{report}");
    assert!(
        !report.contains("disappeared"),
        "a partial run must not read as behaviors disappearing: {report}"
    );
}

/// `--no-report` and `--quiet-unless-changed` ask for silence, and a stream is not silent. Neither can be
/// honored halfway: what changed isn't known until the input ends.
#[test]
fn asking_for_quiet_silences_the_stream_too() {
    let sandbox = Sandbox::new();
    for flag in ["--no-report", "--quiet-unless-changed"] {
        let out = sandbox.piped(&["ingest", flag], CORPUS);
        assert!(out.status.success(), "{flag}: {out:?}");
        assert!(stdout(&out).is_empty(), "{flag}: {}", stdout(&out));
    }
}
