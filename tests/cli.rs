//! End to end through the `siftr` binary. Every test gets its own `SIFTR_HOME` and project directory.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
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
            .env_remove("XDG_DATA_HOME");
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.siftr(args).output().unwrap()
    }

    /// `siftr - …`: the spelling that names stdin even with a flag in front of the input.
    fn read_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .siftr(&[&["-"], args].concat())
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

fn code(output: &Output) -> i32 {
    output.status.code().expect("exited normally")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(output)))
}

/// A synthetic app log. Run `k` of the baseline varies slightly; the regressed run triples a query,
/// stops hitting the cache, and starts warning about misses.
fn app_log(k: u64, regressed: bool) -> String {
    let mut log = String::new();
    let queries = if regressed { 200 } else { 50 + k % 3 };
    for i in 0..queries {
        writeln!(
            log,
            "User Load ({}.{}ms) SELECT * FROM users WHERE id = {i}",
            i % 5,
            i % 10
        )
        .unwrap();
    }
    for i in 0..30 + k {
        writeln!(log, "GET /users/{i} 200 in {}ms", 3 + i % 4).unwrap();
    }
    if regressed {
        for i in 0..20 {
            writeln!(log, "WARN cache miss for key {:x}", 0xbeef_0000_u64 + i).unwrap();
        }
    } else {
        for i in 0..10 {
            writeln!(log, "cache hit for key {:x}", 0xdead_0000_u64 + i).unwrap();
        }
    }
    log
}

fn signal_summary(changes: &Value) -> Vec<(String, String, f64, f64)> {
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_owned(),
                s["behavior"]["template"].as_str().unwrap().to_owned(),
                s["current"].as_f64().unwrap(),
                s["confidence"].as_f64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn a_regression_surfaces_as_new_disappeared_and_frequency_signals() {
    let sandbox = Sandbox::new();
    for k in 1..=3 {
        let baseline = sandbox.read_stdin(&["--context", "suite"], &app_log(k, false));
        assert_eq!(code(&baseline), 0, "{}", stderr(&baseline));
    }
    let regressed = sandbox.read_stdin(&["--context", "suite", "-j"], &app_log(4, true));
    assert_eq!(code(&regressed), 0, "{}", stderr(&regressed));
    let changes = json(&regressed);
    assert_eq!(changes["run"]["id"], "r4");
    assert_eq!(
        changes["baseline_runs"],
        serde_json::json!(["r3", "r2", "r1"])
    );
    // Ranked: a varying count far outside its range (tier 3) before new/gone lines (tier 4).
    let expected = [
        (
            "frequency",
            "User Load (<duration>) SELECT * FROM users WHERE id = <int>",
            200.0,
            0.79,
        ),
        ("new", "WARN cache miss for key <hex>", 20.0, 0.8),
        ("disappeared", "cache hit for key <hex>", 0.0, 0.8),
    ]
    .map(|(kind, template, count, confidence)| {
        (kind.to_owned(), template.to_owned(), count, confidence)
    });
    assert_eq!(signal_summary(&changes), expected);

    // Drill down the way the human output suggests.
    let human = sandbox.output(&["changes"]);
    assert_eq!(code(&human), 0);
    let first_signal = changes["signals"][0]["id"].as_str().unwrap();
    assert!(
        stdout(&human).ends_with(&format!("next: siftr explain {first_signal}\n")),
        "{}",
        stdout(&human)
    );

    let explain = sandbox.output(&["explain", first_signal]);
    assert_eq!(code(&explain), 0, "{}", stderr(&explain));
    assert!(
        stdout(&explain).contains("count     r4 200  |  baseline r3 50  r2 52  r1 51"),
        "{}",
        stdout(&explain)
    );

    let behavior = &changes["signals"][1]["behavior"]["id"].as_str().unwrap()[..10];
    let evidence = sandbox.output(&["explain", behavior, "-j"]);
    assert_eq!(code(&evidence), 0, "{}", stderr(&evidence));
    let evidence = json(&evidence);
    assert_eq!(evidence["run"], "r4");
    assert_eq!(
        evidence["exemplars"][0]["line"],
        "WARN cache miss for key beef0000"
    );
    assert_eq!(
        evidence["exemplars"][0]["seq"], 235,
        "line number in the run's stdout capture"
    );

    let summary = sandbox.output(&["summary", "r4", "-j"]);
    assert_eq!(json(&summary)["behaviors"][0]["stats"]["count"], 200);
    let history = sandbox.output(&["history", "-j"]);
    let changes_per_run: Vec<u64> = json(&history)
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["changes"].as_u64().unwrap())
        .collect();
    assert_eq!(changes_per_run, [3, 0, 0, 0]);
}

#[test]
fn a_steady_run_has_no_changes() {
    let sandbox = Sandbox::new();
    for _ in 0..4 {
        assert_eq!(code(&sandbox.read_stdin(&[], &app_log(1, false))), 0);
    }
    let changes = sandbox.output(&["changes"]);
    assert_eq!(
        code(&changes),
        1,
        "nothing found exits 1: {}",
        stdout(&changes)
    );
    assert!(
        stdout(&changes).starts_with("r4 vs 3 baseline runs (r1 r2 r3): 0 changes"),
        "{}",
        stdout(&changes)
    );
}

#[test]
fn queries_exit_1_when_empty_and_2_on_error() {
    let sandbox = Sandbox::new();
    let cases: [(&[&str], i32); 5] = [
        (&["changes"], 1),
        (&["history"], 1),
        (&["changes", "r99"], 2),
        (&["explain", "s1"], 2),
        (&["explain", "not-hex"], 2),
    ];
    for (args, expected) in cases {
        let output = sandbox.output(args);
        assert_eq!(code(&output), expected, "{args:?}: {}", stderr(&output));
    }
}

#[test]
fn run_passes_output_through_and_exits_with_the_childs_code() {
    let sandbox = Sandbox::new();
    let output = sandbox.output(&["run", "--", "sh", "-c", "echo hi; echo oops >&2; exit 3"]);
    assert_eq!(code(&output), 3);
    assert_eq!(stdout(&output), "hi\n");
    let stderr = stderr(&output);
    assert!(stderr.starts_with("oops\n"), "{stderr}");
    assert!(
        // Three, not two: every run also carries what the kernel charged it, which costs no line.
        stderr.contains("r1: 2 lines, 3 behaviors; no earlier runs"),
        "{stderr}"
    );

    let json_run = sandbox.output(&["run", "-j", "--", "sh", "-c", "echo hi; exit 3"]);
    assert_eq!(code(&json_run), 3);
    let summary = json(&json_run);
    assert_eq!(summary["run"]["exit_code"], 3);
    assert_eq!(summary["run"]["command"], "sh -c 'echo hi; exit 3'");
}

#[test]
fn run_exit_codes_when_the_command_cannot_start() {
    let sandbox = Sandbox::new();
    let missing = sandbox.output(&["run", "--", "siftr-no-such-command"]);
    assert_eq!(code(&missing), 127, "{}", stderr(&missing));
}

/// 125 is reserved for failures that genuinely prevent spawning; a store that can't open must not be one
/// of them, or every command run under a broken SIFTR_HOME would stop working (CLAUDE.md principle 6).
#[test]
fn run_still_runs_the_command_when_the_store_cannot_open() {
    let sandbox = Sandbox::new();
    let not_a_dir = sandbox.project.path().join("file");
    std::fs::write(&not_a_dir, "").unwrap();
    let output = sandbox.output(&["--home", not_a_dir.to_str().unwrap(), "run", "--", "true"]);
    assert_eq!(
        code(&output),
        0,
        "the child still runs: {}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("not recording this run"),
        "{}",
        stderr(&output)
    );
}

/// Replays a captured rails_demo scenario through `siftr run`, keeping the command (and so the context) constant.
fn replay(sandbox: &Sandbox, scenario: &str) -> Output {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/rails_demo")
        .join(scenario);
    let script = r#"cat "$FIXTURE/stdout.txt"; cat "$FIXTURE/stderr.txt" >&2; exit "$(cat "$FIXTURE/exit_code.txt")""#;
    sandbox
        .siftr(&["run", "-j", "--", "sh", "-c", script])
        .env("FIXTURE", fixture)
        .output()
        .unwrap()
}

#[test]
fn rails_fixture_deprecation_warnings_are_new_behaviors_on_stderr() {
    let sandbox = Sandbox::new();
    for scenario in ["baseline", "baseline_2", "baseline"] {
        let output = replay(&sandbox, scenario);
        assert_eq!(code(&output), 0, "{scenario}: {}", stderr(&output));
    }
    let warn = replay(&sandbox, "warn");
    assert_eq!(code(&warn), 0, "{}", stderr(&warn));
    let signals = signal_summary(&json(&warn));
    assert_eq!(signals.len(), 2, "{signals:?}");
    for (kind, template, count, confidence) in &signals {
        assert_eq!(
            (kind.as_str(), *count, *confidence),
            ("new", 1.0, 0.8),
            "{template}"
        );
        assert!(
            template.starts_with("DEPRECATION WARNING: User#display_name is deprecated"),
            "{template}"
        );
    }

    let fail = replay(&sandbox, "fail");
    assert_eq!(code(&fail), 1, "the suite's failure is siftr's exit code");
}
