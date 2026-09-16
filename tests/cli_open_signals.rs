//! A regression left in place stays in view after the rolling baseline absorbs it: the rails_demo N+1, twice.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

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
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(args)
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(
            output.status.code().is_some_and(|code| code < 2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        let args = ["ingest", "-j", "--context", "rails_demo", "--dir"];
        self.json(&[&args[..], &[dir.to_str().unwrap()]].concat())
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }
}

fn open_ids(changes: &Value) -> Vec<(String, String)> {
    changes["open_signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["run"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[test]
fn an_unfixed_regression_is_reminded_until_it_is_fixed_or_dismissed() {
    let sandbox = Sandbox::new();
    for scenario in CLEAN {
        sandbox.ingest(scenario);
    }
    let first = sandbox.ingest("n_plus_one");
    assert_eq!(first["changes"], 1);
    assert_eq!(
        first["open_signals"],
        json!([]),
        "nothing earlier to be open"
    );

    let again = sandbox.ingest("n_plus_one");
    assert_eq!(again["changes"], 0, "r4 is in r5's baseline");
    let open = open_ids(&again);
    assert_eq!(
        open.first().map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "{open:?}"
    );
    assert!(open.iter().all(|(_, run)| run == "r4"), "{open:?}");
    // Everything but `sources`: only the run that did the capturing knows what it read, and the store doesn't
    // hold it, so a run read back reports null rather than guessing.
    let mut stored = sandbox.json(&["changes", "-j"]);
    assert_eq!(stored["sources"], Value::Null);
    assert!(again["sources"].is_array());
    stored["sources"] = again["sources"].clone();
    assert_eq!(stored, again, "changes says what ingest did");

    let changes = sandbox.siftr(&["changes"]);
    assert_eq!(
        changes.status.code(),
        Some(0),
        "an open change is something to look at"
    );
    let human = String::from_utf8(changes.stdout).unwrap();
    assert!(
        human.starts_with("r5 vs 4 baseline runs (r1…r4): no new changes · 1 still open\n"),
        "{human}"
    );
    let reminder = human
        .lines()
        .find(|line| line.starts_with("  still open: "))
        .unwrap_or_else(|| panic!("{human}"));
    assert!(
        reminder.starts_with(
            "  still open: s1 (r4) FREQUENCY GET UsersController#show 2xx  queries 3 → 10"
        ) && reminder.ends_with(" · siftr explain s1"),
        "{human}"
    );
    assert!(human.ends_with("next: siftr explain s1\n"), "{human}");

    let fixed = sandbox.ingest("baseline");
    assert_eq!(fixed["open_signals"], json!([]), "fixed in r6");
    assert!(!sandbox.text(&["changes"]).contains("still open"));
    assert_eq!(
        open_ids(&sandbox.json(&["changes", "r5", "-j"])),
        open,
        "r5 is judged by the runs up to r5"
    );

    sandbox.siftr(&["dismiss", "s1"]);
    assert_eq!(
        sandbox.json(&["changes", "r5", "-j"])["open_signals"],
        json!([]),
        "a dismissed change isn't reminded"
    );
}

/// `siftr run -q` is what a coding agent reads, so its summary carries the reminder too.
#[test]
fn the_run_summary_reminds_of_an_unfixed_regression() {
    let sandbox = Sandbox::new();
    let queries = |n: usize| {
        let log: String = (0..n)
            .map(|i| format!("User Load SELECT * FROM users WHERE id = {i}\n"))
            .collect();
        std::fs::write(sandbox.project.path().join("app.log"), log).unwrap();
    };
    let suite = ["run", "-q", "--", "cat", "app.log"];
    let suite_json = ["run", "-j", "--", "cat", "app.log"];

    queries(5);
    for _ in 0..3 {
        sandbox.siftr(&suite);
    }
    queries(20);
    assert_eq!(sandbox.json(&suite_json)["changes"], 1);

    let again = sandbox.siftr(&suite);
    let report = String::from_utf8(again.stderr).unwrap();
    assert!(
        report.starts_with("r5 vs 4 baseline runs (r1…r4): no new changes · 1 still open\n"),
        "{report}"
    );
    assert!(
        report.contains("\n  still open: s1 (r4) FREQUENCY User Load"),
        "{report}"
    );
    assert!(report.ends_with("next: siftr explain s1\n"), "{report}");

    let third = sandbox.json(&suite_json);
    assert_eq!(
        open_ids(&third)
            .first()
            .map(|(id, run)| (id.as_str(), run.as_str())),
        Some(("s1", "r4")),
        "{third:#}"
    );
}
