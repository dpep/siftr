//! hunt2 #5 (s3): a spec file's examples collapse into one group when they appear or disappear together. The
//! human line reads as "N examples of <file> gone" (or "new"), not one example heading a `supporting:` list of
//! the others.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

struct Home {
    home: TempDir,
    project: TempDir,
}

impl Home {
    fn new() -> Self {
        Home {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn siftr(&self, args: &[&str]) -> Vec<u8> {
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
        output.stdout
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.siftr(args)).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args)).unwrap()
    }

    /// Ingests each state in order.
    fn runs(&self, states: &[&str]) {
        for state in states {
            let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/rspec_hunt")
                .join(state);
            self.text(&[dir.to_str().unwrap(), "--context", "hunt"]);
        }
    }
}

/// hunt2 #5 (s3): `./spec/b_spec.rb`'s 16 examples disappear together in the run that shrinks the suite.
#[test]
fn a_deleted_spec_files_examples_render_as_one_collapsed_group() {
    let home = Home::new();
    home.runs(&["a20_warn1", "a20_warn1", "a20_warn1", "a4_warn3"]);

    let human = home.text(&["changes"]);
    let headline = human
        .lines()
        .find(|line| line.contains("DISAPPEARED"))
        .unwrap_or_else(|| panic!("no DISAPPEARED line in:\n{human}"));
    assert!(
        headline.contains("16 examples of ./spec/b_spec.rb"),
        "{headline}"
    );
    assert!(
        headline.contains("gone, in all 3 baseline runs"),
        "{headline}"
    );
    // Not one example heading a `supporting:` list of the other fifteen.
    for line in human.lines() {
        if line.trim_start().starts_with("supporting:") {
            assert!(
                !line.contains("b_spec.rb # "),
                "a deleted example leaked onto a supporting line: {line}"
            );
        }
    }

    let changes = home.json(&["changes", "-j"]);
    let groups = changes["groups"].as_array().unwrap();
    let group = groups
        .iter()
        .find(|g| g["disappeared_examples"].is_object())
        .unwrap_or_else(|| panic!("no group carries disappeared_examples: {changes:#}"));
    assert_eq!(
        group["disappeared_examples"]["examples"].as_u64(),
        Some(16),
        "{group:#}"
    );
    assert_eq!(
        group["disappeared_examples"]["file"].as_str(),
        Some("./spec/b_spec.rb"),
        "{group:#}"
    );

    // A group not headed by a collapsed DISAPPEARED carries null, not the field's absence.
    let other = groups
        .iter()
        .find(|g| !g["disappeared_examples"].is_object())
        .unwrap_or_else(|| panic!("expected a second, non-collapsed group: {changes:#}"));
    assert!(other["disappeared_examples"].is_null(), "{other:#}");
}

/// `grouping.md` §6.3's pre-registered check: the suite grows 4 → 10 examples in the run that also regresses.
/// The six examples one spec file brought are one change, so the run reads as three — the failure, the
/// deprecation warning, and the added file — rather than eight.
#[test]
fn examples_added_in_one_file_render_as_one_collapsed_group() {
    let home = Home::new();
    home.runs(&["a4_clean", "a4_clean", "a4_clean", "a10_fail_warn"]);

    let changes = home.json(&["changes", "-j"]);
    assert_eq!(changes["groups_total"].as_u64(), Some(3), "{changes:#}");

    let human = home.text(&["changes", "-n", "10"]);
    let headline = human
        .lines()
        .find(|line| line.contains("NEW") && line.contains("examples of"))
        .unwrap_or_else(|| panic!("no collapsed NEW line in:\n{human}"));
    assert!(
        headline.contains("6 examples of ./spec/b_spec.rb"),
        "{headline}"
    );
    assert!(
        headline.contains("new, in none of 3 baseline runs"),
        "{headline}"
    );
    // Not one example heading a `supporting:` list of the other five.
    for line in human.lines() {
        if line.trim_start().starts_with("supporting:") {
            assert!(
                !line.contains("b_spec.rb # "),
                "an added example leaked onto a supporting line: {line}"
            );
        }
    }
    // Nothing disappeared here, so the DISAPPEARED-only field stays null throughout.
    for group in changes["groups"].as_array().unwrap() {
        assert!(group["disappeared_examples"].is_null(), "{group:#}");
    }
}
