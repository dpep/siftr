//! Recent runs left out of a baseline are named, with why, so "3 baseline runs" never quietly means "4 recent".

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

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, context: &str, scenario: &str) -> String {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rails_demo")
            .join(scenario);
        self.text(&[
            "ingest",
            "--context",
            context,
            "--dir",
            dir.to_str().unwrap(),
        ])
    }
}

#[test]
fn a_recent_run_that_did_not_run_the_suite_is_named_as_skipped() {
    let sandbox = Sandbox::new();
    for scenario in CLEAN {
        sandbox.ingest("rails_demo", scenario);
    }
    sandbox.ingest("rails_demo", "load_error");
    let recorded = sandbox.ingest("rails_demo", "baseline");
    assert!(
        recorded.starts_with(
            "r5 vs 3 baseline runs (r1 r2 r3; skipped r4: 1 error outside examples, 0 now): 0 changes\n"
        ),
        "{recorded}"
    );
    assert_eq!(
        sandbox.text(&["changes", "r5"]),
        recorded,
        "a later look judges the candidates as recording did"
    );
    let changes: Value =
        serde_json::from_slice(&sandbox.siftr(&["changes", "r5", "-j"]).stdout).unwrap();
    assert_eq!(
        changes["skipped_runs"],
        json!([{ "run": "r4", "reason": "errors_outside_examples" }])
    );

    // Where no recent run ran the file that failed to load, nothing says its examples existed yet: compared.
    sandbox.ingest("alone", "load_error");
    let alone = sandbox.ingest("alone", "baseline");
    assert!(
        alone.starts_with("r7 vs 1 baseline run (r6): 0 changes"),
        "{alone}"
    );
}
