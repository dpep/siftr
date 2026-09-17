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
    fn siftr(&self, args: &[&str]) -> Output {
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
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
        self.text(&[
            "ingest",
            "--context",
            "rails_demo",
            "--dir",
            dir.to_str().unwrap(),
        ])
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
    let evidence = sandbox.text(&["evidence", behavior, "--run", "r2"]);
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
    sandbox.text(&["ingest", "--context", "rails_demo", "--dir", dir]);

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
        explain.starts_with("s1  NEW  conf 0.80  in r4, group 1 headline\n"),
        "{explain}"
    );
    assert!(explain.contains(" (after the last example)\n"), "{explain}");
    assert!(
        explain.contains("\nscope     after the last example (teardown)\n"),
        "{explain}"
    );
}
