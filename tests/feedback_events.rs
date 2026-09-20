//! Feedback recorded while signals are read and acted on, checked in the `feedback` table a later analysis
//! will read, over the rails_demo N+1 scenario.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    /// Three clean runs, then the N+1: r4, whose one change is headed by s1.
    fn n_plus_one() -> Self {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        };
        for scenario in CLEAN.iter().chain(&["n_plus_one"]) {
            sandbox.ingest(scenario);
        }
        sandbox
    }

    fn ingest(&self, scenario: &str) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        let output = self.siftr(&[
            "ingest",
            "--context",
            "rails_demo",
            "--dir",
            dir.to_str().unwrap(),
        ]);
        assert!(output.status.success(), "{}", stderr(&output));
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

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.home.path().join("siftr.db")).unwrap()
    }

    /// `kind command interface run signal note` per row, oldest first.
    fn feedback(&self) -> Vec<String> {
        let db = self.db();
        let mut stmt = db
            .prepare(
                "SELECT kind, command, interface, run_id, signal_id, note FROM feedback ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok(format!(
                "{} {} {} r{} {} {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?
                    .map_or("-".to_owned(), |s| format!("s{s}")),
                row.get::<_, Option<String>>(5)?
                    .unwrap_or_else(|| "-".to_owned()),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }
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

#[test]
fn reading_and_acting_on_signals_is_recorded_where_it_happened() {
    let sandbox = Sandbox::n_plus_one();
    let before = sandbox.feedback().len();

    assert!(
        sandbox
            .siftr(&["changes", "--context", "rails_demo"])
            .status
            .success()
    );
    let changes = sandbox.siftr(&["changes", "--context", "rails_demo", "-j"]);
    let signals = json(&changes)["signals"].as_array().unwrap().clone();
    assert_eq!(signals.len(), 4, "{signals:?}");

    let explain = sandbox.siftr(&["explain", "s1", "-j"]);
    assert!(explain.status.success(), "{}", stderr(&explain));

    // A disappearance's evidence comes from the latest run that still had the behavior.
    let gone = signals
        .iter()
        .find(|s| s["kind"] == "disappeared")
        .expect("the renamed example");
    let gone_behavior = gone["behavior"]["id"].as_str().unwrap();
    let evidence = sandbox.siftr(&["explain", &gone_behavior[..10]]);
    assert!(evidence.status.success(), "{}", stderr(&evidence));

    let ack = sandbox.siftr(&["ack", "s1", "-m", "fixing the N+1"]);
    assert!(ack.status.success(), "{}", stderr(&ack));
    assert!(
        stdout(&ack)
            .ends_with("no longer reminded of this change\nnext: siftr history --signals\n"),
        "{}",
        stdout(&ack)
    );
    let dismiss = json(&sandbox.siftr(&["ack", "s2", "--wrong", "-j"]));
    assert_eq!(
        (
            &dismiss["kind"],
            &dismiss["signal"],
            &dismiss["run"],
            &dismiss["interface"],
            &dismiss["note"]
        ),
        (
            &Value::from("dismissed"),
            &Value::from("s2"),
            &Value::from("r4"),
            &Value::from("json"),
            &Value::Null
        )
    );

    let mut expected: Vec<String> = Vec::new();
    for interface in ["human", "json"] {
        expected.extend((1..=4).map(|s| format!("surfaced changes {interface} r4 s{s} -")));
    }
    expected.extend([
        "investigated explain json r4 s1 -".to_owned(),
        "evidence_requested explain human r3 - -".to_owned(),
        "acked ack human r4 s1 fixing the N+1".to_owned(),
        "dismissed ack json r4 s2 -".to_owned(),
    ]);
    assert_eq!(sandbox.feedback()[before..], expected);

    let db = sandbox.db();
    let evidence_behavior: String = db
        .query_row(
            "SELECT behavior_id FROM feedback WHERE kind = 'evidence_requested'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(evidence_behavior, gone_behavior);
    let disagreeing: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM feedback f JOIN signals s ON s.id = f.signal_id
             WHERE f.behavior_id != s.behavior_id OR f.run_id != s.run_id",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        disagreeing, 0,
        "a signal's feedback names its run and behavior"
    );
}

#[test]
fn a_run_that_shows_a_signal_records_it_as_surfaced() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    // Same command every time (env alone decides the extra line), so all three share one context.
    let script = r#"echo hi; if [ -n "$SIFTR_TEST_EXTRA" ]; then echo surprise; fi"#;
    for _ in 0..2 {
        let output = sandbox.siftr(&["run", "--", "sh", "-c", script]);
        assert!(output.status.success(), "{}", stderr(&output));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(["run", "-j", "--", "sh", "-c", script])
        .current_dir(sandbox.project.path())
        .env("SIFTR_HOME", sandbox.home.path())
        .env_remove("XDG_DATA_HOME")
        .env("SIFTR_TEST_EXTRA", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let signals = json(&output)["signals"].as_array().unwrap().clone();
    assert_eq!(signals.len(), 1, "{signals:?}");

    assert_eq!(sandbox.feedback(), ["surfaced run json r3 s1 -".to_owned()]);
}

#[test]
fn feedback_that_cannot_be_recorded_warns_and_never_fails_a_reading_command() {
    let sandbox = Sandbox::n_plus_one();
    sandbox
        .db()
        .execute_batch(
            "CREATE TRIGGER refuse BEFORE INSERT ON feedback BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();

    let explain = sandbox.siftr(&["explain", "s1"]);
    assert_eq!(explain.status.code(), Some(0), "{}", stderr(&explain));
    assert!(stdout(&explain).contains("next: siftr summary"));
    assert!(
        stderr(&explain).contains("siftr: warning: feedback not recorded"),
        "{}",
        stderr(&explain)
    );

    let changes = sandbox.siftr(&["changes", "--context", "rails_demo", "-j"]);
    assert_eq!(changes.status.code(), Some(0), "{}", stderr(&changes));
    assert_eq!(json(&changes)["run"]["id"], "r4");

    // Recording is ack's whole job, so there it is an error.
    let dismiss = sandbox.siftr(&["ack", "s1"]);
    assert_eq!(dismiss.status.code(), Some(2));
    assert!(stderr(&dismiss).contains("refused"), "{}", stderr(&dismiss));
}
