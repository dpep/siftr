//! Human output reads right: counts agree with their nouns, confidence has two decimals, a single timing is
//! exact, and a failure shows its message.

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
            .join("../../fixtures/rails_demo")
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
            "r2 vs 1 baseline run (r1): 1 change; only ERROR can fire until there are 2 baseline runs\n  s1   ERROR       conf 0.67  "
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
