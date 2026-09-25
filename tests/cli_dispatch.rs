//! `siftr` without a subcommand, through the binary: every dispatch row, and that nothing is ever a guess.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use tempfile::TempDir;

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
            .env_remove("XDG_DATA_HOME")
            .stdin(Stdio::null());
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.siftr(args).output().unwrap()
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
            // A refused invocation exits without reading stdin, so the pipe may already be closed.
            .ok();
        child.wait_with_output().unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        let path = self.project.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// Commands of the runs recorded so far, newest first.
    fn recorded(&self) -> Vec<String> {
        let history = json(&self.output(&["-j", "history"]));
        history
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["command"].as_str().unwrap().to_owned())
            .collect()
    }
}

fn code(output: &Output) -> i32 {
    output.status.code().expect("exited normally")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            stderr(output)
        )
    })
}

#[test]
fn what_follows_the_double_dash_is_run_with_its_exit_code() {
    let sandbox = Sandbox::new();
    let out = sandbox.output(&["-q", "--", "sh", "-c", "echo hi; exit 3"]);
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    assert_eq!(sandbox.recorded(), ["sh -c 'echo hi; exit 3'"]);
}

#[test]
fn a_subcommand_or_preset_is_never_read_as_a_file_of_that_name() {
    let sandbox = Sandbox::new();
    sandbox.write("status", "not a log\n");
    sandbox.write("cron", "not a log\n");
    assert_eq!(code(&sandbox.output(&["status"])), 0);
    let cron = sandbox.output(&["-j", "cron"]);
    assert!(json(&cron)["jobs"].is_array(), "the cron preset ran");
    assert!(sandbox.recorded().is_empty(), "neither was ingested");

    let file = json(&sandbox.output(&["-j", "./status"]));
    assert_eq!(file["run"]["context"], "status", "./ names the file");
}

#[test]
fn an_existing_file_is_read_and_compared_only_with_that_file() {
    let sandbox = Sandbox::new();
    sandbox.write("log/a.log", "alpha\n");
    sandbox.write("log/b.log", "beta\n");
    let a = json(&sandbox.output(&["-j", "log/a.log"]));
    let b = json(&sandbox.output(&["-j", "log/../log/b.log"]));
    assert_eq!(
        (&a["run"]["context"], &b["run"]["context"]),
        (&"log/a.log".into(), &"log/b.log".into())
    );

    let missing = sandbox.output(&["-j", "log/c.log"]);
    assert_eq!(
        (code(&missing), &json(&missing)["error"]["code"]),
        (2, &"not_found".into())
    );
    let dir = sandbox.output(&["log"]);
    assert_eq!(code(&dir), 2);
    assert!(
        stderr(&dir).contains("log is a directory, and not a captured scenario"),
        "{}",
        stderr(&dir)
    );
}

/// A directory of captured streams replays; the same directory without them is refused. Nothing between the
/// two is a judgement call, which is the point: `siftr DIR` recognises a capture, it never tries one out.
#[test]
fn a_captured_scenario_directory_replays_and_a_plain_one_is_refused() {
    let sandbox = Sandbox::new();
    sandbox.write("capture/stdout.txt", "alpha ready\nbeta ready\n");
    sandbox.write("capture/exit_code.txt", "0\n");
    sandbox.write("notes/readme.txt", "not a capture\n");

    let replayed = json(&sandbox.output(&["-j", "capture"]));
    assert_eq!(replayed["run"]["lines"], 2, "{replayed}");
    assert_eq!(
        sandbox.recorded(),
        ["siftr ingest --context ingest --dir capture"]
    );

    let refused = sandbox.output(&["notes"]);
    assert_eq!(code(&refused), 2);
    assert!(
        stderr(&refused).contains("rspec.ndjson"),
        "the refusal names what would make it one: {}",
        stderr(&refused)
    );
    assert_eq!(sandbox.recorded().len(), 1, "nothing else was recorded");
}

/// The words siftr used to have, and a program it never had: each is answered with the line to type, because
/// the edit distance below can reach none of them.
#[test]
fn a_retired_word_and_a_program_on_path_are_each_answered_not_listed() {
    let sandbox = Sandbox::new();
    let gone = sandbox.output(&["ingest", "--dir", "somewhere"]);
    assert_eq!(code(&gone), 2);
    assert!(
        stderr(&gone).contains("'ingest' was removed; use `siftr FILE (or siftr -, siftr DIR)`"),
        "{}",
        stderr(&gone)
    );
    assert!(
        stderr(&gone).contains("replays by naming its directory"),
        "--dir is the half a reader would think they had lost: {}",
        stderr(&gone)
    );

    // `sh` is on PATH everywhere this test runs; siftr still refuses to run it, and says how.
    let program = sandbox.output(&["sh", "-c", "echo hi"]);
    assert_eq!(code(&program), 2);
    assert!(
        stderr(&program).contains("to run it: siftr -- sh -c 'echo hi'"),
        "{}",
        stderr(&program)
    );
    assert!(sandbox.recorded().is_empty(), "nothing was ever run");
}

#[test]
fn a_bare_siftr_ingests_piped_stdin_and_otherwise_prints_help() {
    let sandbox = Sandbox::new();
    let piped = sandbox.piped(&["-j"], "one\ntwo\n");
    assert_eq!(json(&piped)["run"]["lines"], 2);
    let dash = sandbox.piped(&["-"], "one\n");
    assert_eq!(code(&dash), 0, "{}", stderr(&dash));

    let bare = sandbox.output(&[]);
    assert_eq!(code(&bare), 2);
    assert!(stderr(&bare).contains("Usage: siftr"), "{}", stderr(&bare));
    assert_eq!(
        sandbox.recorded().len(),
        2,
        "stdin from /dev/null isn't input"
    );
}

#[test]
fn an_unknown_flag_before_the_double_dash_is_a_usage_error_never_the_command() {
    let sandbox = Sandbox::new();
    // Explicit `run`, the bare spelling that implies it, and `sources`, which wraps a command the same way.
    for args in [
        &["run", "--qiet", "--", "echo", "hi"][..],
        &["--qiet", "--", "echo", "hi"][..],
        &["sources", "--qiet", "--", "echo", "hi"][..],
    ] {
        let out = sandbox.output(args);
        let message = stderr(&out);
        assert_eq!(code(&out), 2, "{args:?}: {message}");
        assert!(message.contains("'--qiet'"), "{args:?} names it: {message}");
    }
    let near = stderr(&sandbox.output(&["run", "--qiet", "--", "echo", "hi"]));
    assert!(
        near.contains("'--quiet'"),
        "suggests the near match: {near}"
    );

    let as_json = sandbox.output(&["-j", "run", "--qiet", "--", "echo", "hi"]);
    assert_eq!(
        (code(&as_json), &json(&as_json)["error"]["code"]),
        (2, &"usage".into())
    );
    assert!(sandbox.recorded().is_empty(), "nothing was ever run");
}

#[test]
fn a_flag_after_the_double_dash_is_the_wrapped_command_s_own() {
    let sandbox = Sandbox::new();
    let out = sandbox.output(&["--", "sh", "-c", r#"printf %s "$1""#, "sh", "--qiet"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "--qiet",
        "passed through untouched"
    );
    assert_eq!(sandbox.recorded().len(), 1);
}

#[test]
fn an_unknown_word_is_an_error_with_suggestions_even_with_stdin_piped() {
    let sandbox = Sandbox::new();
    let typo = sandbox.piped(&["statu"], "a line\n");
    assert_eq!(code(&typo), 2);
    let message = stderr(&typo);
    assert!(message.contains("did you mean 'status'?"), "{message}");
    assert!(message.contains("presets: cron"), "{message}");
    let as_json = sandbox.output(&["-j", "chnages"]);
    assert_eq!(
        (code(&as_json), &json(&as_json)["error"]["code"]),
        (2, &"usage".into())
    );
    assert!(sandbox.recorded().is_empty());
}
