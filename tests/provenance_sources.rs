//! Turning a source off in `.siftr.toml` is a configuration change, not a behavioral one: the behaviors it
//! fed must not be reported DISAPPEARED, and turning it back on must not report them NEW. What the run could
//! read is evidence the store keeps, so a comparison across the change can say it has no verdict.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

/// A Rails project whose `rspec` appends SQL to `log/test.log` and prints to stdout.
struct Project {
    dir: TempDir,
    home: TempDir,
    config: TempDir,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("log")).unwrap();
        std::fs::write(dir.path().join("log/test.log"), "").unwrap();
        std::fs::write(dir.path().join("Gemfile"), "gem 'rails'\n").unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let project = Project {
            dir,
            home: tempfile::tempdir().unwrap(),
            config: tempfile::tempdir().unwrap(),
        };
        project.suite(&["stdout line", "another stdout line"]);
        project
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Writes `bin/rspec`: it prints `stdout_lines` and appends the same two queries to the Rails log.
    fn suite(&self, stdout_lines: &[&str]) {
        let mut script = String::from("#!/bin/sh\n");
        for line in stdout_lines {
            script.push_str(&format!("printf '%s\\n' '{line}'\n"));
        }
        for table in ["users", "posts"] {
            script.push_str(&format!(
                "printf '%s\\n' '  Load (0.3ms)  SELECT \"{table}\".* FROM \"{table}\"' >> log/test.log\n"
            ));
        }
        let path = self.path().join("bin/rspec");
        std::fs::write(&path, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// `.siftr.toml` turning `rails_log` on or off.
    fn rails_log(&self, enabled: bool) {
        std::fs::write(
            self.path().join(".siftr.toml"),
            format!("[sources.rails_log]\nenabled = {enabled}\n"),
        )
        .unwrap();
    }

    /// One `siftr run -- bin/rspec`, as its `-j` document.
    fn run(&self) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
            .args(["run", "-j", "--", "bin/rspec"])
            .current_dir(self.path())
            .env("SIFTR_HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env_remove("XDG_DATA_HOME")
            .env_remove("SPEC_OPTS")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)))
    }
}

/// `(kind, template)` of every signal in the document.
fn signals(doc: &Value) -> Vec<(String, String)> {
    doc["signals"]
        .as_array()
        .expect("signals")
        .iter()
        .map(|s| {
            (
                s["kind"].as_str().unwrap().to_owned(),
                s["behavior"]["template"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn of_kind<'a>(signals: &'a [(String, String)], kind: &str) -> Vec<&'a str> {
    signals
        .iter()
        .filter(|(k, _)| k == kind)
        .map(|(_, template)| template.as_str())
        .collect()
}

/// A binary built from this crate, so the test can say which one it drove.
fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_siftr"))
}

#[test]
fn a_source_switched_off_does_not_make_the_behaviors_it_fed_disappear() {
    assert!(binary().exists());
    let project = Project::new();
    project.rails_log(true);
    for _ in 0..3 {
        project.run();
    }

    // The log source goes off, and the suite stops printing one of its stdout lines: one is a configuration
    // change, the other a real one, and the same run carries both.
    project.rails_log(false);
    project.suite(&["stdout line"]);
    let signals = signals(&project.run());

    let gone = of_kind(&signals, "disappeared");
    assert!(
        gone.iter().any(|t| t.contains("another stdout line")),
        "a line the run still read and stopped printing is still news: {signals:?}"
    );
    assert!(
        !gone.iter().any(|t| t.contains("SELECT")),
        "the rails log was switched off, so its queries didn't disappear: {signals:?}"
    );
}

#[test]
fn a_source_switched_on_does_not_make_the_behaviors_it_feeds_new() {
    let project = Project::new();
    project.rails_log(false);
    for _ in 0..3 {
        project.run();
    }

    // On for the first time: every query it feeds is absent from the whole baseline for want of the source.
    project.rails_log(true);
    project.suite(&["stdout line", "another stdout line", "a third stdout line"]);
    let signals = signals(&project.run());

    let fresh = of_kind(&signals, "new");
    assert!(
        fresh.iter().any(|t| t.contains("a third stdout line")),
        "a line the run started printing is still news: {signals:?}"
    );
    assert!(
        !fresh.iter().any(|t| t.contains("SELECT")),
        "the rails log was switched on, so its queries aren't new behavior: {signals:?}"
    );
}
