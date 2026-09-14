//! The signal rules end to end over captured rails_demo scenarios, replayed with `siftr ingest --dir`.
//!
//! Only three clean captures exist, so every comparison here has n ≤ 3 and E(3) = 0.8 caps the
//! confidence of count rules; signals.md's backtest covers n = 5 and 10.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

fn fixture(scenario: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rails_demo")
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
        let dir = fixture(scenario);
        let output = self.siftr(&[
            "ingest",
            "--context",
            "rails_demo",
            "-j",
            "--dir",
            dir.to_str().unwrap(),
        ]);
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// The changes `current` shows against `baselines`, recorded in that order.
    fn compare(baselines: &[&str], current: &str) -> (Sandbox, Value) {
        let sandbox = Sandbox::new();
        for baseline in baselines {
            sandbox.ingest(baseline);
        }
        let changes = sandbox.ingest(current);
        (sandbox, changes)
    }
}

/// `(kind, measure, current, confidence, template)` of each signal in `group`, headline first.
fn group(changes: &Value, rank: u64) -> Vec<(String, String, f64, f64, String)> {
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["group"] == rank)
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_owned(),
                s["measure"].as_str().unwrap().to_owned(),
                s["current"].as_f64().unwrap(),
                s["confidence"].as_f64().unwrap(),
                s["behavior"]["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn signal(
    kind: &str,
    measure: &str,
    current: f64,
    confidence: f64,
    template: &str,
) -> (String, String, f64, f64, String) {
    (
        kind.to_owned(),
        measure.to_owned(),
        current,
        confidence,
        template.to_owned(),
    )
}

#[test]
fn clean_captures_show_no_signals_against_each_other() {
    for (i, current) in CLEAN.iter().enumerate() {
        let others: Vec<&str> = CLEAN
            .iter()
            .enumerate()
            .filter(|&(j, _)| j != i)
            .map(|(_, s)| *s)
            .collect();
        let (_, changes) = Sandbox::compare(&others, current);
        assert_eq!(changes["baseline_runs"].as_array().unwrap().len(), 2);
        assert_eq!(
            changes["signals"],
            serde_json::json!([]),
            "{current} against {others:?}"
        );
    }
}

#[test]
fn n_plus_one_is_one_change_headed_by_the_request_query_count() {
    let (sandbox, changes) = Sandbox::compare(&CLEAN, "n_plus_one");
    assert_eq!(changes["changes"], 1);
    let comment_by_post =
        r#"Comment Load SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?"#;
    let members = group(&changes, 1);
    assert_eq!(
        members[0],
        signal(
            "frequency",
            "queries",
            10.0,
            0.8,
            "GET UsersController#show 2xx"
        )
    );
    assert_eq!(members[0].2, 10.0);
    let example = "./spec/requests/users_spec.rb # Users shows a user with posts and comments";
    let supporting: Vec<_> = members[1..]
        .iter()
        .map(|m| (m.0.as_str(), m.1.as_str(), m.2))
        .collect();
    assert_eq!(
        supporting,
        [
            ("frequency", "count", 9.0),
            ("frequency", "queries", 35.0),
            ("disappeared", "count", 0.0)
        ]
    );
    assert!(
        members[1].4.starts_with(comment_by_post),
        "{}",
        members[1].4
    );
    assert_eq!(members[2].4, example);

    let sql = &changes["signals"][1];
    assert_eq!(sql["attribution"]["scope"]["template"], example);
    assert_eq!(
        (
            sql["attribution"]["baseline"].as_f64(),
            sql["attribution"]["current"].as_f64()
        ),
        (Some(0.0), Some(8.0))
    );

    let human = String::from_utf8(
        sandbox
            .siftr(&["changes", "--context", "rails_demo"])
            .stdout,
    )
    .unwrap();
    assert!(
        human.contains("GET UsersController#show 2xx  queries 3 → 10"),
        "{human}"
    );
    assert!(human.ends_with("next: siftr explain s1\n"), "{human}");
}

#[test]
fn a_failure_is_one_error_naming_its_exception() {
    let (_, changes) = Sandbox::compare(&CLEAN, "fail");
    assert_eq!(
        group(&changes, 1),
        [signal(
            "error",
            "failed",
            1.0,
            0.8,
            "./spec/models/user_spec.rb # User requires an email"
        )]
    );
    assert_eq!(
        changes["signals"][0]["exception"],
        "RSpec::Expectations::ExpectationNotMetError"
    );
    assert_eq!(
        changes["signals"].as_array().unwrap().len(),
        1,
        "the reporter's stdout adds nothing"
    );
}

#[test]
fn a_slow_example_is_one_latency_signal() {
    let (_, changes) = Sandbox::compare(&CLEAN, "slow");
    let [(kind, measure, current, confidence, template)] = group(&changes, 1).try_into().unwrap();
    assert_eq!(
        (kind.as_str(), measure.as_str(), template.as_str()),
        (
            "latency",
            "duration_ms",
            "./spec/models/post_spec.rb # Post summarizes the body"
        )
    );
    assert!(current > 300.0, "{current}");
    // signals.md backtest at n = 3: 0.60–0.61.
    assert!((0.6..=0.61).contains(&confidence), "{confidence}");
    assert_eq!(changes["signals"].as_array().unwrap().len(), 1);
}

#[test]
fn a_deprecation_warning_is_one_change_from_two_call_sites() {
    let (_, changes) = Sandbox::compare(&CLEAN, "warn");
    assert_eq!(changes["changes"], 1);
    let members = group(&changes, 1);
    assert_eq!(members.len(), 2);
    for (kind, measure, current, confidence, template) in &members {
        assert_eq!(
            (kind.as_str(), measure.as_str(), *current, *confidence),
            ("new", "count", 1.0, 0.8)
        );
        assert!(
            template.starts_with("DEPRECATION WARNING: User#display_name is deprecated"),
            "{template}"
        );
    }
}
