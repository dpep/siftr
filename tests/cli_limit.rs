//! `siftr changes -n` bounds how many changes a report shows. The document's totals count every change the
//! run raised, whatever the report shows, so a consumer can always tell a slice from the whole.

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

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args).stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, scenario: &str) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/rspec_hunt")
            .join(scenario);
        self.text(&[
            "ingest",
            "--context",
            "hunt",
            "--dir",
            dir.to_str().unwrap(),
        ]);
    }
}

/// A run with more changes than the report shows: a failure, a deprecation warning, the six examples one added
/// spec file brought (one change between them), and a lone example of another file. This is the legitimate
/// many-changes case — a flood is refused before it is recorded. Its 4 changes carry 9 signals on purpose: a
/// fixture whose two totals matched could not tell them apart.
fn four_changes() -> Sandbox {
    let sandbox = Sandbox {
        home: tempfile::tempdir().unwrap(),
        project: tempfile::tempdir().unwrap(),
    };
    for _ in 0..3 {
        sandbox.ingest("a4_clean");
    }
    sandbox.ingest("c_fixed_fail_warn");
    sandbox
}

/// How many changes a human report printed: one headline line, opening with the signal's id, per change shown.
fn shown(report: &str) -> usize {
    report
        .lines()
        .filter(|line| {
            line.strip_prefix("  s")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .count()
}

fn len(document: &Value, field: &str) -> usize {
    document[field].as_array().expect(field).len()
}

#[test]
fn the_document_counts_every_change_whatever_it_lists() {
    let sandbox = four_changes();
    let whole = sandbox.json(&["changes", "r4", "-j"]);
    assert_eq!(
        (
            whole["groups_total"].as_u64(),
            whole["signals_total"].as_u64()
        ),
        (Some(4), Some(9)),
        "a change and a signal are different things to count: {whole}"
    );
    assert_eq!((len(&whole, "groups"), len(&whole, "signals")), (4, 9));

    // A limit bounds what is listed. The totals and the headline count are the run's, so they do not move:
    // that difference is how a consumer tells a slice from the whole.
    let two = sandbox.json(&["changes", "r4", "-j", "-n", "2"]);
    assert_eq!((len(&two, "groups"), len(&two, "signals")), (2, 2));
    assert_eq!(
        (two["groups_total"].as_u64(), two["signals_total"].as_u64()),
        (Some(4), Some(9)),
        "the totals count the run's changes, not the page's: {two}"
    );
    assert_eq!(
        two["changes"], whole["changes"],
        "the headline count must not shrink with -n"
    );

    // The signals listed are exactly the ones the listed groups name, so the two can never disagree.
    let named: Vec<&str> = two["groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["signals"].as_array().unwrap())
        .map(|id| id.as_str().unwrap())
        .collect();
    let listed: Vec<&str> = two["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|signal| signal["id"].as_str().unwrap())
        .collect();
    assert_eq!(named, listed, "{two}");

    // Asking for more than there are is not a truncation.
    let all = sandbox.json(&["changes", "r4", "-j", "-n", "50"]);
    assert_eq!((len(&all, "groups"), len(&all, "signals")), (4, 9));
}

#[test]
fn the_same_limit_applies_to_the_human_report() {
    let sandbox = four_changes();
    // Unbounded, the report shows its usual few and points at -j for the rest.
    let default = sandbox.text(&["changes", "r4"]);
    assert_eq!(shown(&default), 3, "{default}");
    assert!(default.contains("… 1 more change"), "{default}");

    let two = sandbox.text(&["changes", "r4", "-n", "2"]);
    assert_eq!(shown(&two), 2, "{two}");
    assert!(
        two.contains("… 2 more changes"),
        "what a limit left out is still counted: {two}"
    );

    // Past the end nothing is left over, so nothing claims to be.
    let all = sandbox.text(&["changes", "r4", "-n", "20"]);
    assert_eq!(shown(&all), 4, "{all}");
    assert!(!all.contains("more change"), "{all}");

    // The exit code answers "were there changes", not "were any shown": a limit must not make a run look clean.
    for args in [&["changes", "r4"][..], &["changes", "r4", "-n", "0"][..]] {
        assert_eq!(
            sandbox.siftr(args).status.code(),
            Some(0),
            "{args:?} still found changes"
        );
    }
}
