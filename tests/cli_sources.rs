//! `siftr sources`: what siftr can read here, whether each source is on, and whether it applies.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn siftr(dir: &Path, args: &[&str]) -> Output {
    let home = dir.join(".siftr-home");
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(dir)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(output)))
}

/// A Rails project: `log/test.log` is what the rails-log source reads.
fn rails_project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("log")).unwrap();
    std::fs::write(dir.path().join("log/test.log"), "").unwrap();
    std::fs::write(dir.path().join("Gemfile"), "gem 'rails'\n").unwrap();
    dir
}

/// A source's row: on, applies, and why.
fn row<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .find(|source| source["name"] == name)
        .unwrap_or_else(|| panic!("no source {name} in {doc}"))
}

#[test]
fn a_rails_project_running_rspec_has_every_source_apply() {
    let project = rails_project();
    let output = siftr(
        project.path(),
        &["sources", "-j", "--", "bundle", "exec", "rspec"],
    );
    assert_eq!(output.status.code(), Some(0), "{}", stdout(&output));
    let doc = document(&output);
    assert_eq!(doc["command"], "bundle exec rspec");
    for name in ["stdout", "stderr", "rspec-events", "log/test.log"] {
        let source = row(&doc, name);
        assert_eq!(source["on"], true, "{name} is on by default");
        assert_eq!(source["applies"], true, "{name} applies: {source}");
    }
}

#[test]
fn a_source_that_does_not_apply_says_why() {
    let bare = tempfile::tempdir().unwrap();
    let doc = document(&siftr(
        bare.path(),
        &["sources", "-j", "--", "bundle", "exec", "rspec"],
    ));
    let log = row(&doc, "log/test.log");
    assert_eq!(log["applies"], false, "no rails log here: {log}");
    assert!(
        log["why"].as_str().unwrap().contains("rails"),
        "says why: {log}"
    );
    assert_eq!(row(&doc, "rspec-events")["applies"], true);

    // In a Rails project, a command that isn't a test run gets neither side channel.
    let project = rails_project();
    let doc = document(&siftr(project.path(), &["sources", "-j", "--", "ls"]));
    for name in ["rspec-events", "log/test.log"] {
        let source = row(&doc, name);
        assert_eq!(source["applies"], false, "{name}: {source}");
    }
    assert_eq!(
        row(&doc, "stdout")["applies"],
        true,
        "the command's own output is always read"
    );
}

#[test]
fn without_a_command_it_says_what_it_cannot_judge() {
    let project = rails_project();
    let output = siftr(project.path(), &["sources"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(
        text.contains("rspec-events") && text.contains("log/test.log"),
        "{text}"
    );
    assert!(
        text.ends_with("next: siftr sources -- CMD\n"),
        "it says how to get a precise answer: {text}"
    );
    let doc = document(&siftr(project.path(), &["sources", "-j"]));
    assert_eq!(doc["command"], Value::Null);
    let rspec = row(&doc, "rspec-events");
    assert_eq!(rspec["applies"], false, "no command to judge: {rspec}");
    assert!(rspec["why"].as_str().unwrap().contains("no command"));
}

#[test]
fn a_run_reports_the_sources_it_actually_captured() {
    // A command that writes nothing to stderr captured no stderr: the list is what arrived, not what was offered.
    let bare = tempfile::tempdir().unwrap();
    let doc = document(&siftr(bare.path(), &["run", "-j", "--", "echo", "hello"]));
    assert_eq!(doc["sources"], serde_json::json!(["stdout"]));

    // A replayed scenario: its stderr.txt is empty, so only the three streams with bytes are captured.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/n_plus_one");
    let doc = document(&siftr(
        bare.path(),
        &["ingest", "-j", "--dir", fixture.to_str().unwrap()],
    ));
    assert_eq!(
        doc["sources"],
        serde_json::json!(["stdout", "rspec-events", "log/test.log"]),
        "the command's own output first, then each side channel as it was fed"
    );
}

#[test]
fn the_human_listing_reads_as_a_table() {
    let project = rails_project();
    let text = stdout(&siftr(
        project.path(),
        &["sources", "--", "bundle", "exec", "rspec"],
    ));
    let mut lines = text.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("sources for bundle exec rspec in "),
        "{text}"
    );
    let rows: Vec<&str> = lines.filter(|line| line.starts_with("  ")).collect();
    assert_eq!(rows.len(), 4, "{text}");
    assert!(
        rows[2].contains("rspec-events") && rows[2].contains("applies"),
        "{text}"
    );
    assert!(
        text.ends_with("next: siftr run -- bundle exec rspec\n"),
        "{text}"
    );
}
