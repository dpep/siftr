//! Human output reads right: counts agree with their nouns, a change names the baseline backing it rather
//! than a confidence score, a single timing is exact, and a failure shows its message.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    /// Whatever the command did, including its exit code.
    fn siftr_raw(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap()
    }

    fn siftr(&self, args: &[&str]) -> Output {
        let output = self.siftr_raw(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, scenario: &str) -> String {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        self.text(&[dir.to_str().unwrap(), "--context", "rails_demo"])
    }
}

#[test]
fn a_failure_after_one_clean_run_reads_right() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    sandbox.ingest("baseline");
    let fail = sandbox.ingest("fail");
    assert!(
        fail.starts_with(
            "r2 vs 1 baseline run (r1): 1 change; only ERROR can fire until there are 2 baseline runs\n  s1   ERROR       1 baseline run "
        ),
        "{fail}"
    );

    let explain = sandbox.text(&["explain", "s1"]);
    let lines: Vec<&str> = explain.lines().collect();
    let raw = lines
        .iter()
        .position(|line| line.trim_start().starts_with("file:rspec-events:"))
        .unwrap_or_else(|| panic!("{explain}"));
    assert_eq!(
        lines[raw - 1].trim(),
        r#"RSpec::Expectations::ExpectationNotMetError: expected #<User id: nil, created_at: nil, email: nil, name: "Ada", updated_at: nil> not to be valid"#,
        "{explain}"
    );

    let signal: Value =
        serde_json::from_slice(&sandbox.siftr(&["explain", "s1", "-j"]).stdout).unwrap();
    let behavior = &signal["signal"]["behavior"]["id"].as_str().unwrap()[..10];
    let evidence = sandbox.text(&["explain", behavior, "--run", "r2"]);
    assert!(
        evidence.contains("\nr2: 1 occurrence, 1 error; 1 line kept\n  RSpec::Expectations::ExpectationNotMetError: "),
        "{evidence}"
    );

    let history = sandbox.text(&["history"]);
    assert!(history.contains(" 1 change "), "{history}");
    assert!(history.contains(" 0 changes "), "{history}");

    // The slowest behavior occurs once, so its percentiles are its total, not a histogram estimate.
    let summary = sandbox.text(&["summary", "r1", "--by", "time", "-n", "1"]);
    let row: Vec<&str> = summary.lines().nth(2).unwrap().split_whitespace().collect();
    assert_eq!(row[0], "1", "{summary}");
    assert_eq!((row[2], row[3]), (row[4], row[4]), "{summary}");
    let json: Value = serde_json::from_slice(
        &sandbox
            .siftr(&["summary", "r1", "--by", "time", "-n", "1", "-j"])
            .stdout,
    )
    .unwrap();
    let d = &json["behaviors"][0]["stats"]["duration"];
    assert_eq!(
        (&d["p50_us"], &d["p95_us"]),
        (&d["total_us"], &d["total_us"])
    );
}

/// A query an `after(:suite)` hook ran is named for where it happened: after the last example, not setup.
#[test]
fn a_change_after_the_last_example_reads_as_teardown() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    for scenario in ["baseline", "baseline_2", "baseline_documentation"] {
        sandbox.ingest(scenario);
    }

    // The clean run again, with one more log line between the last example's end and the summary.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/baseline");
    let after = tempfile::tempdir().unwrap();
    let mut log = std::fs::read(fixture.join("test.log")).unwrap();
    let ended = log.len();
    log.extend_from_slice(b"  Cleanup Load (0.1ms)  SELECT \"jobs\".* FROM \"jobs\"\n");
    std::fs::write(after.path().join("test.log"), &log).unwrap();
    let events: String = std::fs::read_to_string(fixture.join("rspec.ndjson"))
        .unwrap()
        .lines()
        .map(|line| match line.starts_with(r#"{"event":"summary""#) {
            true => line.replace(
                &format!(r#""log_offset":{ended}}}"#),
                &format!(r#""log_offset":{}}}"#, log.len()),
            ),
            false => line.to_owned(),
        } + "\n")
        .collect();
    assert!(events.contains(&format!(r#""log_offset":{}}}"#, log.len())));
    std::fs::write(after.path().join("rspec.ndjson"), events).unwrap();
    for name in ["stdout.txt", "stderr.txt", "exit_code.txt"] {
        std::fs::copy(fixture.join(name), after.path().join(name)).unwrap();
    }
    let dir = after.path().to_str().unwrap();
    sandbox.text(&[dir, "--context", "rails_demo"]);

    let changes: Value = serde_json::from_slice(&sandbox.siftr(&["changes", "-j"]).stdout).unwrap();
    let signal = &changes["signals"][0];
    assert_eq!(
        (
            signal["kind"].as_str(),
            signal["attribution"]["phase"].as_str(),
            signal["attribution"]["setup"].as_bool()
        ),
        (Some("new"), Some("teardown"), Some(false)),
        "{changes:#}"
    );
    let explain = sandbox.text(&["explain", "s1"]);
    assert!(
        explain.starts_with("s1  NEW  in r4, group 1 headline\n"),
        "{explain}"
    );
    assert!(explain.contains(" (after the last example)\n"), "{explain}");
    assert!(
        explain.contains("\nscope     after the last example (teardown)\n"),
        "{explain}"
    );
}

/// Confidence ranks below chance (`docs/findings/confidence.md`) and reads as severity wherever it is printed.
/// It is `-j` only: this sweeps every human command so the next place it could reappear is caught here.
#[test]
fn no_human_output_prints_a_confidence_score() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    let mut reports = vec![sandbox.ingest("baseline")];
    for scenario in ["baseline_2", "baseline_documentation", "n_plus_one", "fail"] {
        reports.push(sandbox.ingest(scenario));
    }
    let behavior: Value =
        serde_json::from_slice(&sandbox.siftr(&["explain", "s1", "-j"]).stdout).unwrap();
    let behavior = &behavior["signal"]["behavior"]["id"].as_str().unwrap()[..10];
    for args in [
        vec!["changes"],
        vec!["changes", "r4"],
        vec!["explain", "s1"],
        vec!["explain", behavior],
        vec!["summary", "r4"],
        vec!["history"],
        vec!["history", "--signals"],
        vec!["status"],
    ] {
        // Raw: `status` exits 1 when the data dir needs attention, and its output still counts.
        reports.push(String::from_utf8(sandbox.siftr_raw(&args).stdout).unwrap());
    }
    for report in reports {
        assert!(
            !report.contains("conf ") && !report.contains("confidence"),
            "a score a reader will rank on: {report}"
        );
    }
}

/// One command for either kind of id, and `--run` on both: the two halves used to be two commands, and a
/// reader with an id in hand had to know which.
#[test]
fn explain_takes_either_kind_of_id_and_a_run() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    for scenario in ["baseline", "baseline_2", "baseline_documentation", "fail"] {
        sandbox.ingest(scenario);
    }
    let signal = sandbox.text(&["explain", "s1"]);
    assert!(
        signal.starts_with("s1  ERROR  in r4, group 1 headline\n"),
        "{signal}"
    );

    let doc: Value =
        serde_json::from_slice(&sandbox.siftr(&["explain", "s1", "-j"]).stdout).unwrap();
    let behavior = &doc["signal"]["behavior"]["id"].as_str().unwrap()[..10];
    assert_eq!(doc["evidence"]["run"], "r4");
    // `explain s1 --run r3` was an argument error while only `evidence` took `--run`.
    let earlier: Value = serde_json::from_slice(
        &sandbox
            .siftr(&["explain", "s1", "--run", "r3", "-j"])
            .stdout,
    )
    .unwrap();
    assert_eq!(earlier["evidence"]["run"], "r3");
    assert_eq!(
        earlier["signal"]["id"], "s1",
        "still the signal's own report"
    );

    // The same id typed as a behavior reads that behavior's lines, under the same command and flags.
    let lines = sandbox.text(&["explain", behavior, "--run", "r3", "-n", "1"]);
    assert!(
        lines.starts_with(&format!("{behavior}  test.example  ")),
        "{lines}"
    );
    assert!(
        lines.contains("\nr3: 1 occurrence, 0 errors; 1 line kept\n"),
        "{lines}"
    );

    // A run outside the comparison is refused rather than answered with unrelated lines.
    let stray = sandbox.siftr_raw(&["explain", "s1", "--run", "r1"]);
    assert_eq!(stray.status.code(), Some(0), "r1 is a baseline run");
    let unknown = sandbox.siftr_raw(&["explain", "s1", "--run", "r99"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("no r99 in s1's comparison"),
        "{}",
        String::from_utf8_lossy(&unknown.stderr)
    );
}

/// The order is the ranking, said once, and only where there is an order to speak of.
#[test]
fn a_report_of_several_changes_says_the_order_is_the_ranking() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    for scenario in ["baseline", "baseline_2", "baseline_documentation"] {
        sandbox.ingest(scenario);
    }
    let one = sandbox.ingest("n_plus_one");
    assert!(
        one.starts_with("r4 vs 3 baseline runs (r1 r2 r3): 1 change\n"),
        "{one}"
    );
    assert!(
        !one.contains("most important first"),
        "one change has no order: {one}"
    );

    let several = sandbox.ingest("fail_fast");
    assert_eq!(
        several.matches("most important first").count(),
        1,
        "said once, in the header: {several}"
    );
    assert!(
        several
            .lines()
            .next()
            .unwrap()
            .ends_with("2 changes, most important first"),
        "{several}"
    );
}
