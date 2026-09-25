//! `siftr sources`: what siftr can read here, whether each source is on, and whether it applies.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

fn siftr(dir: &Path, args: &[&str]) -> Output {
    let home = dir.join(".siftr-home");
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(dir)
        .env("SIFTR_HOME", home)
        // The sources these tests assert on are configurable, so the machine's own config and SPEC_OPTS
        // must not reach them: XDG_CONFIG_HOME points at a directory that doesn't exist.
        .env("XDG_CONFIG_HOME", dir.join(".siftr-config"))
        .env_remove("XDG_DATA_HOME")
        .env_remove("SPEC_OPTS")
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
    for name in ["stdout", "stderr", "rspec", "rails_log", "rusage"] {
        let source = row(&doc, name);
        assert_eq!(source["on"], true, "{name} is on by default");
        assert_eq!(source["applies"], true, "{name} applies: {source}");
    }
    // A source is named by its config key; `stream` is what joins it to an exemplar's `stream`.
    assert_eq!(row(&doc, "rspec")["stream"], "file:rspec-events");
    assert_eq!(row(&doc, "rails_log")["stream"], "file:log/test.log");
    assert_eq!(row(&doc, "stdout")["stream"], "stdout");
    // What the kernel charged the run is read from the wait, not from a stream, and says so.
    assert_eq!(row(&doc, "rusage")["stream"], Value::Null);
}

#[test]
fn a_source_that_does_not_apply_says_why() {
    let bare = tempfile::tempdir().unwrap();
    let doc = document(&siftr(
        bare.path(),
        &["sources", "-j", "--", "bundle", "exec", "rspec"],
    ));
    let log = row(&doc, "rails_log");
    assert_eq!(log["applies"], false, "no rails log here: {log}");
    assert!(
        log["why"].as_str().unwrap().contains("rails"),
        "says why: {log}"
    );
    assert_eq!(row(&doc, "rspec")["applies"], true);

    // In a Rails project, a command that isn't a test run gets neither side channel.
    let project = rails_project();
    let doc = document(&siftr(project.path(), &["sources", "-j", "--", "ls"]));
    for name in ["rspec", "rails_log"] {
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
        text.contains("rspec") && text.contains("rails_log"),
        "{text}"
    );
    assert!(
        text.ends_with("next: siftr sources -- CMD\n"),
        "it says how to get a precise answer: {text}"
    );
    let doc = document(&siftr(project.path(), &["sources", "-j"]));
    assert_eq!(doc["command"], Value::Null);
    let rspec = row(&doc, "rspec");
    assert_eq!(rspec["applies"], false, "no command to judge: {rspec}");
    assert!(rspec["why"].as_str().unwrap().contains("no command"));
}

#[test]
fn a_run_reports_the_streams_it_actually_captured() {
    // A command that writes nothing to stderr captured no stderr: the list is what arrived, not what was offered.
    let bare = tempfile::tempdir().unwrap();
    let doc = document(&siftr(bare.path(), &["run", "-j", "--", "echo", "hello"]));
    assert_eq!(doc["streams"], serde_json::json!(["stdout"]));

    // A replayed scenario: its stderr.txt is empty, so only the three streams with bytes are captured.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/n_plus_one");
    let doc = document(&siftr(bare.path(), &["-j", fixture.to_str().unwrap()]));
    assert_eq!(
        doc["streams"],
        serde_json::json!(["stdout", "file:rspec-events", "file:log/test.log"]),
        "the command's own output first, then each side channel as it was fed"
    );
}

/// One `history --sources` row.
fn run_row<'a>(doc: &'a Value, run: &str) -> &'a Value {
    doc.as_array()
        .expect("rows")
        .iter()
        .find(|row| row["run"] == run)
        .unwrap_or_else(|| panic!("no row for {run} in {doc}"))
}

/// The names of the sources `run` recorded reading, as the store ordered them.
fn read_by(doc: &Value, run: &str) -> Vec<String> {
    run_row(doc, run)["sources"]
        .as_array()
        .unwrap_or_else(|| panic!("{run} recorded no sources: {doc}"))
        .iter()
        .map(|source| source["name"].as_str().unwrap().to_owned())
        .collect()
}

/// `siftr sources` says what siftr *can* read here; what a run actually read is only knowable from the
/// recording. A run whose configuration turned a source off must still say so afterwards, and a run that
/// recorded nothing must say it couldn't tell rather than claim it read nothing.
#[test]
fn history_says_what_each_run_read() {
    let project = rails_project();
    std::fs::create_dir(project.path().join("bin")).unwrap();
    let rspec = project.path().join("bin/rspec");
    std::fs::write(
        &rspec,
        "#!/bin/sh\nprintf 'an example\\n'\nprintf '  Load (0.3ms)  SELECT \"users\".* FROM \"users\"\\n' >> log/test.log\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&rspec, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let rails_log = |enabled: bool| {
        std::fs::write(
            project.path().join(".siftr.toml"),
            format!("[sources.rails_log]\nenabled = {enabled}\n"),
        )
        .unwrap();
    };
    let run = || {
        let output = siftr(project.path(), &["run", "-q", "--", "bin/rspec"]);
        assert!(output.status.success(), "{}", stdout(&output));
    };

    // r1 read the Rails log; r2 had it switched off. Same command, same directory: only the recording can say.
    rails_log(true);
    run();
    rails_log(false);
    run();

    let doc = document(&siftr(project.path(), &["history", "--sources", "-j"]));
    assert_eq!(
        read_by(&doc, "r1"),
        ["rails_log", "rusage", "stderr", "stdout"]
    );
    assert_eq!(
        read_by(&doc, "r2"),
        ["rusage", "stderr", "stdout"],
        "rails_log was off for r2, and the run it fed nothing to says so"
    );
    // The stream spelling is what joins a source to the exemplars it produced.
    let log = &run_row(&doc, "r1")["sources"][0];
    assert_eq!(
        (&log["name"], &log["stream"]),
        (&json!("rails_log"), &json!("file:log/test.log"))
    );
    // What the kernel charged the run opens no stream, here as in `siftr sources`.
    let rusage = &run_row(&doc, "r2")["sources"][0];
    assert_eq!(
        (&rusage["name"], &rusage["stream"]),
        (&json!("rusage"), &Value::Null)
    );

    let text = stdout(&siftr(project.path(), &["history", "--sources"]));
    let line = |run: &str| -> String {
        text.lines()
            .find(|line| line.trim_start().starts_with(run))
            .unwrap_or_else(|| panic!("no {run} line in {text}"))
            .to_owned()
    };
    assert!(line("r1").contains("rails_log"), "{text}");
    assert!(!line("r2").contains("rails_log"), "{text}");
    assert!(line("r2").contains("stdout"), "{text}");

    // An ingested run replays a capture rather than choosing sources, so it recorded none. That is unknown,
    // not empty: claiming it read nothing would be a provenance answer siftr doesn't have.
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rails_demo/baseline");
    let ingested = siftr(project.path(), &[fixture.to_str().unwrap()]);
    assert!(ingested.status.success(), "{}", stdout(&ingested));
    let doc = document(&siftr(project.path(), &["history", "--sources", "-j"]));
    assert_eq!(
        run_row(&doc, "r3")["sources"],
        Value::Null,
        "a run that recorded no sources can't say what it read"
    );
    let text = stdout(&siftr(project.path(), &["history", "--sources"]));
    assert!(
        text.lines()
            .any(|line| line.trim_start().starts_with("r3") && line.contains("not recorded")),
        "{text}"
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
    assert_eq!(rows.len(), 5, "{text}");
    assert!(
        rows[2].contains("rspec") && rows[2].contains("applies"),
        "{text}"
    );
    assert!(
        text.ends_with("next: siftr run -- bundle exec rspec\n"),
        "{text}"
    );
}
