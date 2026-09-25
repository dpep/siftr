//! siftr never presents something as a command unless it is one, and a command it prints has to work when it
//! is retyped.
//!
//! Two ways that broke. A read is stored under dispatch's own spelling — `siftr ingest --context C X` — and
//! `ingest` was retired in 0.3.0, so printing that verbatim hands the reader a line that errors; the store
//! keeps it (`docs/json.md`, and two migrations match on it), so only the printing is inverted. And a read's
//! *context* is its `--context` name rather than a command line at all, so the places that printed one as a
//! command were offering `ingest` or `app` to be typed. These tests retype what was printed and check the run
//! it records lands in the same context, rather than comparing strings to strings.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const LOG: &str = "GET /users/1 in 3ms\nGET /users/2 in 4ms\n";

struct Sandbox {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Sandbox {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        };
        std::fs::create_dir(sandbox.project.path().join("logs")).unwrap();
        std::fs::write(sandbox.project.path().join("logs/app.log"), LOG).unwrap();
        sandbox
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
        command
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME");
        command
    }

    /// Runs siftr with stdin a pipe carrying `LOG`, as every spelling of a read may want it.
    fn siftr(&self, args: &[&str]) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(LOG.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "siftr {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn stdout(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }

    /// The line as a shell would read it, which is the whole point: `line` is what a human copied.
    fn retype(&self, line: &str) -> Output {
        let rest = line.strip_prefix("siftr ").unwrap_or_else(|| {
            panic!("a printed siftr command should start with `siftr `: {line:?}")
        });
        let binary = env!("CARGO_BIN_EXE_siftr").replace('\'', r"'\''");
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!("exec '{binary}' {rest}"))
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(LOG.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn runs(&self) -> Vec<Value> {
        let json = self.stdout(&["-j", "history"]);
        serde_json::from_str(&json).unwrap()
    }

    /// The newest run, which after a retype is the run the retyped line recorded.
    fn newest(&self) -> Value {
        self.runs().first().cloned().unwrap()
    }

    /// The command as `siftr summary` prints it: the header is `ID: N lines, N behaviors, COMMAND`.
    fn printed(&self, run: &str) -> String {
        let stdout = self.stdout(&["summary", run]);
        let header = stdout.lines().next().unwrap();
        header
            .splitn(3, ", ")
            .nth(2)
            .unwrap_or_else(|| panic!("no command in {header:?}"))
            .to_owned()
    }
}

/// Every spelling of a read, printed and then retyped. Each has to record a run of the same context: the
/// printed line is only useful if running it puts you back where the run it describes was.
#[test]
fn every_printed_read_command_records_the_same_context_when_it_is_retyped() {
    let sandbox = Sandbox::new();
    let scenario = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/rails_demo/baseline")
        .to_str()
        .unwrap()
        .to_owned();
    let reads: [&[&str]; 6] = [
        // A named file names its own context.
        &["logs/app.log"],
        // A named file, with a context of its own.
        &["logs/app.log", "--context", "app"],
        // A context name the shell would split, so the stored line is quoted.
        &["logs/app.log", "--context", "my logs"],
        // An unnamed pipe, which every other unnamed pipe here shares.
        &["-"],
        // A named pipe.
        &["-", "--context", "piped"],
        // A captured scenario replayed.
        &[&scenario, "--context", "demo"],
    ];

    for read in reads {
        sandbox.siftr(read);
        let run = sandbox.newest();
        let (id, context) = (
            run["id"].as_str().unwrap(),
            run["context"].as_str().unwrap(),
        );
        let (id, context) = (id.to_owned(), context.to_owned());

        let printed = sandbox.printed(&id);
        assert!(
            !printed.contains("ingest"),
            "`siftr {}` printed a retired spelling: {printed:?}",
            read.join(" ")
        );
        // The store keeps dispatch's spelling: only the printing is inverted.
        assert!(
            run["command"]
                .as_str()
                .unwrap()
                .starts_with("siftr ingest "),
            "the stored command must not change: {run}"
        );

        let retyped = sandbox.retype(&printed);
        assert!(
            retyped.status.success(),
            "`{printed}` did not run: {}",
            String::from_utf8_lossy(&retyped.stderr)
        );
        let recorded = sandbox.newest();
        assert_ne!(recorded["id"].as_str().unwrap(), id, "it recorded a run");
        assert_eq!(
            recorded["context"].as_str().unwrap(),
            context,
            "`{printed}` recorded a run of another context"
        );
    }
}

/// `history` is where the junior agent copied from, so its columns carry the same line `summary` does.
#[test]
fn history_prints_the_same_retypable_command_as_summary() {
    let sandbox = Sandbox::new();
    sandbox.siftr(&["logs/app.log", "--context", "app"]);
    let id = sandbox.newest()["id"].as_str().unwrap().to_owned();
    let printed = sandbox.printed(&id);
    assert_eq!(printed, "siftr logs/app.log --context app");

    for listing in [vec!["history"], vec!["history", "--sources"]] {
        let stdout = sandbox.stdout(&listing);
        let row = stdout
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no row for {id} in {listing:?}:\n{stdout}"));
        assert!(row.contains(&printed), "{listing:?}:\n{stdout}");
        assert!(!row.contains("ingest"), "{listing:?}:\n{stdout}");
    }
}

/// A wrapped command is stored as the command itself, not as a read, and must reach the page untouched.
#[test]
fn a_wrapped_command_is_printed_exactly_as_it_was_stored() {
    let sandbox = Sandbox::new();
    sandbox.siftr(&["run", "--", "/bin/echo", "ingest --context x"]);
    let run = sandbox.newest();
    let stored = run["command"].as_str().unwrap().to_owned();
    assert_eq!(stored, "/bin/echo 'ingest --context x'");
    assert_eq!(sandbox.printed(run["id"].as_str().unwrap()), stored);
}

/// The first-run note exists to be acted on: it names the baseline this command isn't joining so the reader
/// can join it instead. A read's context is its `--context` name, which nothing can be typed from, so the
/// note carries the neighbouring run's own command.
#[test]
fn the_first_run_note_names_its_neighbour_by_a_command_that_joins_it() {
    let sandbox = Sandbox::new();
    sandbox.siftr(&["logs/app.log", "--context", "app"]);
    let fresh = sandbox.siftr(&["run", "--", "/bin/echo", "hi"]);
    let note = String::from_utf8_lossy(&fresh.stderr).into_owned()
        + &String::from_utf8_lossy(&fresh.stdout);

    assert!(note.contains("new baseline:"), "{note}");
    assert!(
        note.contains("rather than joining `siftr logs/app.log --context app`"),
        "the neighbour has to be a line that joins it:\n{note}"
    );
    // The reader is invited to type it, so it had better work — and land where the note said.
    let retyped = sandbox.retype("siftr logs/app.log --context app");
    assert!(
        retyped.status.success(),
        "{}",
        String::from_utf8_lossy(&retyped.stderr)
    );
    assert_eq!(sandbox.newest()["context"].as_str().unwrap(), "app");
}

/// `siftr status` lists contexts, and its column said COMMAND over them — so a read appeared as the command
/// `ingest`, which is the retired word. The values are what `--context` takes, so that is what heads them.
#[test]
fn status_heads_the_contexts_it_lists_with_the_word_that_selects_one() {
    let sandbox = Sandbox::new();
    sandbox.siftr(&["-"]);
    let stdout = sandbox.stdout(&["status"]);
    let header = stdout
        .lines()
        .find(|line| line.contains("NEWEST"))
        .unwrap_or_else(|| panic!("no listing header:\n{stdout}"));
    assert!(header.ends_with("CONTEXT"), "{stdout}");
    assert!(!header.contains("COMMAND"), "{stdout}");
    // The row under it is a context name and never was a command: that is the whole reason the header moved.
    assert!(
        stdout.lines().any(|line| line.ends_with("  ingest")),
        "{stdout}"
    );
    assert!(stdout.contains("1 run of 1 context;"), "{stdout}");
}
