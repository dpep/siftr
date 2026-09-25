//! A red `cargo test` run, replayed from `fixtures/cargo_test`: one broken test is one change.
//!
//! The captures are trimmed from real runs of this repo's own gate, which reported 18 changes in 18
//! groups for a single failing test — the libtest lines were plain `log` behaviors, so nothing carried
//! an outcome for the ERROR rule and nothing grouped.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn fixture(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/cargo_test")
        .join(scenario)
}

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

    fn ingest(&self, scenario: &str) -> Value {
        let dir = fixture(scenario);
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .arg("-j")
            .arg(&dir)
            .args(["--context", "cargo"])
            .current_dir(self.project.path())
            .env("SIFTR_HOME", self.home.path())
            .env_remove("XDG_DATA_HOME")
            .output()
            .unwrap();
        assert!(
            output.status.code().is_some_and(|code| code < 2),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

/// `(kind, group, headline, behavior kind, template)` of every signal, in rank order.
fn signals(changes: &Value) -> Vec<(String, u64, bool, String, String)> {
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_owned(),
                s["group"].as_u64().unwrap(),
                s["headline"].as_bool().unwrap(),
                s["behavior"]["kind"].as_str().unwrap().to_owned(),
                s["behavior"]["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

const FAILING: &str = "siftr_gate_off_beats_a_siftr_that_works";

#[test]
fn one_broken_test_is_one_change() {
    let sandbox = Sandbox::new();
    for _ in 0..3 {
        sandbox.ingest("pass");
    }
    let signals = signals(&sandbox.ingest("fail"));
    let groups: std::collections::BTreeSet<u64> = signals.iter().map(|s| s.1).collect();
    assert_eq!(
        signals
            .first()
            .map(|s| (s.0.as_str(), s.3.as_str(), s.4.as_str())),
        Some(("error", "test.example", FAILING)),
        "the failure leads: {signals:#?}"
    );
    assert_eq!(groups.len(), 1, "one change, not many: {signals:#?}");
}

/// Every green run of the same suite agrees, so nothing is news: a test's identity must not move with
/// its outcome, and the harness lines around it must not become behaviors of their own.
#[test]
fn a_green_rerun_reports_nothing() {
    let sandbox = Sandbox::new();
    for _ in 0..3 {
        sandbox.ingest("pass");
    }
    assert_eq!(signals(&sandbox.ingest("pass")), Vec::new());
}

/// The failure's own output stays reachable as evidence under the example, not as peer changes.
#[test]
fn the_panic_lines_are_not_changes_of_their_own() {
    let sandbox = Sandbox::new();
    for _ in 0..3 {
        sandbox.ingest("pass");
    }
    let templates: Vec<String> = signals(&sandbox.ingest("fail"))
        .into_iter()
        .map(|s| s.4)
        .collect();
    for line in [
        "the kill switch left something wrapped:",
        "self-test: absent passes the command's code and output through",
        "---- siftr_gate_off_beats_a_siftr_that_works stdout ----",
        "failures:",
    ] {
        assert!(
            !templates.iter().any(|t| t == line),
            "{line:?} is a change: {templates:#?}"
        );
    }
}
