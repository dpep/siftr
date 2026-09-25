//! What a still-open reminder carries: a regression with the signals that support it, including a DISAPPEARED
//! one, as the rails_demo N+1's vanished eager-load query.

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
            output.status.code().is_some_and(|code| code < 2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn ingest(&self, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        let output = self.siftr(&["-j", dir.to_str().unwrap(), "--context", "rails_demo"]);
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn a_reminder_keeps_the_disappeared_query_that_supports_its_headline() {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    for scenario in [
        "baseline",
        "baseline_2",
        "baseline_documentation",
        "n_plus_one",
    ] {
        sandbox.ingest(scenario);
    }
    let again = sandbox.ingest("n_plus_one");
    let open: Vec<&str> = again["open_signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap())
        .collect();
    assert_eq!(open.first(), Some(&"frequency"), "{open:?}");
    assert!(open.contains(&"disappeared"), "{open:?}");

    let human = String::from_utf8(sandbox.siftr(&["changes"]).stdout).unwrap();
    assert!(
        human.lines().any(|line| line.starts_with(
            "  still open: s1 (r4) FREQUENCY GET UsersController#show 2xx  queries 3 → 10"
        ) && line.ends_with(" (+3 supporting) · siftr explain s1")),
        "{human}"
    );
}
