//! `--quiet-unless-changed`: a job wrapped for cron, CI or a git hook adds nothing to its output unless something is
//! worth reading, and never changes the command's own output or exit code.

use std::process::{Command, Output};

use tempfile::TempDir;

/// Output on both streams and a failing exit code, all of it the command's own.
const JOB: [&str; 3] = ["sh", "-c", "cat app.log; echo err >&2; exit 3"];

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

    fn siftr(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    }

    /// Writes `app.log` with `n` queries, and returns its text.
    fn queries(&self, n: usize) -> String {
        let log: String = (0..n)
            .map(|i| format!("User Load SELECT * FROM users WHERE id = {i}\n"))
            .collect();
        std::fs::write(self.project.path().join("app.log"), &log).unwrap();
        log
    }
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

fn quiet(command: &[&str]) -> Vec<String> {
    ["--quiet-unless-changed", "--"]
        .iter()
        .chain(command)
        .map(|arg| (*arg).to_owned())
        .collect()
}

fn args(owned: &[String]) -> Vec<&str> {
    owned.iter().map(String::as_str).collect()
}

#[test]
fn a_clean_run_adds_nothing_to_the_commands_own_output() {
    let sandbox = Sandbox::new();
    let log = sandbox.queries(5);
    let (silent, job) = (quiet(&["sh", "-c", "exit 0"]), quiet(&JOB));
    // The first run has no baseline, the second too little for most signals, the rest a full one.
    for run in 1..=4 {
        let out = sandbox.siftr(&args(&silent));
        assert_eq!(
            (out.status.code(), text(&out.stdout), text(&out.stderr)),
            (Some(0), "", ""),
            "run {run}"
        );
        let out = sandbox.siftr(&args(&job));
        assert_eq!(
            (out.status.code(), text(&out.stdout), text(&out.stderr)),
            (Some(3), log.as_str(), "err\n"),
            "run {run}"
        );
    }
    let history = sandbox.siftr(&["history", "-j"]);
    let runs: serde_json::Value = serde_json::from_slice(&history.stdout).unwrap();
    assert_eq!(
        runs.as_array().map(Vec::len),
        Some(8),
        "quiet still records"
    );
}

#[test]
fn a_change_is_reported_and_reminded_until_dismissed() {
    let sandbox = Sandbox::new();
    let job = quiet(&JOB);
    sandbox.queries(5);
    for _ in 0..3 {
        assert_eq!(text(&sandbox.siftr(&args(&job)).stderr), "err\n");
    }

    let log = sandbox.queries(20);
    let headlines = [
        "r4 vs 3 baseline runs (r1 r2 r3): 1 change\n",
        "r5 vs 4 baseline runs (r1…r4): no new changes · 1 still open\n",
    ];
    for headline in headlines {
        let out = sandbox.siftr(&args(&job));
        assert_eq!(
            (out.status.code(), text(&out.stdout)),
            (Some(3), log.as_str())
        );
        let stderr = text(&out.stderr);
        let report = stderr
            .strip_prefix("err\n")
            .unwrap_or_else(|| panic!("the command's stderr comes first: {stderr}"));
        assert!(report.starts_with(headline), "{report}");
        assert!(report.ends_with("next: siftr explain s1\n"), "{report}");
    }

    assert_eq!(sandbox.siftr(&["dismiss", "s1"]).status.code(), Some(0));
    assert_eq!(
        text(&sandbox.siftr(&args(&job)).stderr),
        "err\n",
        "dismiss is the way to stop a reminder"
    );
}

#[test]
fn a_run_that_isnt_recorded_still_warns() {
    let sandbox = Sandbox::new();
    let not_a_dir = sandbox.project.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let home = ["--home", not_a_dir.to_str().unwrap()];
    let out = sandbox.siftr(&[&home[..], &args(&quiet(&["sh", "-c", "echo out"]))].concat());
    assert_eq!((out.status.code(), text(&out.stdout)), (Some(0), "out\n"));
    assert!(
        text(&out.stderr).starts_with("siftr: warning: not recording this run:"),
        "{}",
        text(&out.stderr)
    );
}

/// `-j` always prints one document, so the flag would do nothing there: refused before the command runs.
#[test]
fn json_refuses_the_flag_before_running_the_command() {
    let sandbox = Sandbox::new();
    let spellings: [&[&str]; 3] = [
        &["-j", "--quiet-unless-changed", "--", "sh", "-c", "echo ran"],
        &[
            "run",
            "--quiet-unless-changed",
            "-j",
            "--",
            "sh",
            "-c",
            "echo ran",
        ],
        &["ingest", "-j", "--quiet-unless-changed", "app.log"],
    ];
    sandbox.queries(1);
    for spelling in spellings {
        let out = sandbox.siftr(spelling);
        assert_eq!(out.status.code(), Some(2), "{spelling:?}");
        let document: serde_json::Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|_| panic!("{spelling:?}: {}", text(&out.stdout)));
        assert_eq!(document["error"]["code"], "usage", "{document}");
    }
    let history = sandbox.siftr(&["history", "-j"]);
    assert_eq!(
        text(&history.stdout).trim(),
        "[]",
        "nothing ran or was recorded"
    );
}

#[test]
fn ingest_is_quiet_the_same_way() {
    let sandbox = Sandbox::new();
    let ingest = ["ingest", "--quiet-unless-changed", "app.log"];
    sandbox.queries(5);
    for run in 1..=3 {
        let out = sandbox.siftr(&ingest);
        assert_eq!(
            (out.status.code(), text(&out.stdout), text(&out.stderr)),
            (Some(0), "", ""),
            "run {run}"
        );
    }
    sandbox.queries(20);
    let out = sandbox.siftr(&ingest);
    assert!(
        text(&out.stdout).starts_with("r4 vs 3 baseline runs (r1 r2 r3): 1 change\n"),
        "{}",
        text(&out.stdout)
    );
}
