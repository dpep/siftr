//! The config file: `.siftr.toml` in a project, `~/.config/siftr/config.toml` for the user. Both say the same
//! one thing — which sources run — and a typo in either must never stop the wrapped command (principle 6).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

struct Sandbox {
    home: TempDir,
    config: TempDir,
    /// Holds the project, so a file "above the project root" stays inside this sandbox.
    outer: TempDir,
}

impl Sandbox {
    /// A project root `root_of` will find: a git dir and a manifest, with a subdirectory to run from.
    fn new() -> Self {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            config: tempfile::tempdir().unwrap(),
            outer: tempfile::tempdir().unwrap(),
        };
        let root = sandbox.root();
        std::fs::create_dir_all(root.join("spec")).unwrap();
        std::fs::create_dir(root.join(".git")).unwrap();
        std::fs::write(root.join("Gemfile"), "").unwrap();
        sandbox
    }

    fn root(&self) -> PathBuf {
        self.outer.path().join("project")
    }

    /// As siftr will report it: `getcwd` resolves the symlink macOS puts in front of a temp dir.
    fn real_root(&self) -> PathBuf {
        self.root().canonicalize().unwrap()
    }

    fn project_file(&self, text: &str) {
        std::fs::write(self.root().join(".siftr.toml"), text).unwrap();
    }

    fn user_file(&self, text: &str) {
        let dir = self.config.path().join("siftr");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), text).unwrap();
    }

    /// Run from `spec/`, so every project file is found by walking up.
    fn siftr(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_siftr"));
        command
            .args(args)
            .current_dir(self.root().join("spec"))
            .env("SIFTR_HOME", self.home.path())
            .env("XDG_CONFIG_HOME", self.config.path())
            .env_remove("XDG_DATA_HOME");
        for name in ["SIFTR_KEEP_RUNS", "SIFTR_KEEP_EVIDENCE", "SIFTR_KEEP_DAYS"] {
            command.env_remove(name);
        }
        command.output().unwrap()
    }

    fn status(&self) -> Value {
        let output = self.siftr(&["status", "-j"]);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {}", out(&output)))
    }
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A source's value and where `status` says it came from: `default`, or the file that set it.
fn source(status: &Value, name: &str) -> (bool, String) {
    let setting = &status["config"]["sources"][name];
    let origin = match setting["source"].as_str().expect("a source") {
        "file" => setting["path"].as_str().expect("a path").to_owned(),
        other => other.to_owned(),
    };
    (setting["value"].as_bool().expect("a bool"), origin)
}

fn shown(path: &Path) -> String {
    path.display().to_string()
}

#[test]
fn every_source_runs_until_a_file_says_otherwise() {
    let sandbox = Sandbox::new();
    let status = sandbox.status();
    assert_eq!(source(&status, "rspec"), (true, "default".to_owned()));
    assert_eq!(source(&status, "rails_log"), (true, "default".to_owned()));

    let human = sandbox.siftr(&["status"]);
    assert!(
        out(&human).contains("rspec on (default)"),
        "{}",
        out(&human)
    );
}

#[test]
fn the_project_file_wins_over_the_user_file_and_both_over_the_default() {
    let sandbox = Sandbox::new();
    sandbox.user_file("[sources.rspec]\nenabled = false\n[sources.rails_log]\nenabled = false\n");
    sandbox.project_file("[sources.rails_log]\nenabled = true\n");

    let status = sandbox.status();
    let user = shown(&sandbox.config.path().join("siftr/config.toml"));
    let project = shown(&sandbox.real_root().join(".siftr.toml"));
    assert_eq!(
        source(&status, "rspec"),
        (false, user),
        "only the user file sets it"
    );
    assert_eq!(
        source(&status, "rails_log"),
        (true, project),
        "the project file wins"
    );

    let human = out(&sandbox.siftr(&["status"]));
    assert!(
        human.contains("rails_log on (") && human.contains(".siftr.toml)"),
        "{human}"
    );
}

#[test]
fn a_file_above_the_project_root_is_not_read() {
    let sandbox = Sandbox::new();
    std::fs::write(
        sandbox.outer.path().join(".siftr.toml"),
        "[sources.rspec]\nenabled = false\n",
    )
    .unwrap();

    let status = sandbox.status();
    assert_eq!(source(&status, "rspec"), (true, "default".to_owned()));
}

#[test]
fn an_unknown_key_or_source_warns_once_and_keeps_the_defaults() {
    let sandbox = Sandbox::new();
    sandbox.project_file(
        "[retention]\nruns = 5\n\n[sources.rspce]\nenabled = false\n\n\
         [sources.rspec]\nenable = false\n",
    );

    let output = sandbox.siftr(&["status", "-j"]);
    let stderr = err(&output);
    assert_eq!(stderr.matches("rspce").count(), 1, "warns once: {stderr}");
    assert!(stderr.contains("is not a source"), "{stderr}");
    assert!(stderr.contains("retention"), "{stderr}");
    assert!(stderr.contains("enable"), "{stderr}");

    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(source(&status, "rspec"), (true, "default".to_owned()));
}

#[test]
fn a_value_of_the_wrong_type_warns_and_keeps_the_default() {
    let sandbox = Sandbox::new();
    sandbox.project_file("[sources.rspec]\nenabled = 1\n");

    let output = sandbox.siftr(&["status", "-j"]);
    assert!(
        err(&output).contains("is not true or false"),
        "{}",
        err(&output)
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(source(&status, "rspec"), (true, "default".to_owned()));
}

/// Principle 6: siftr's own config can't be what stops your command.
#[test]
fn a_malformed_file_warns_and_never_breaks_the_wrapped_command() {
    let sandbox = Sandbox::new();
    sandbox.project_file("[sources.rspec\nenabled = ");

    let run = sandbox.siftr(&["run", "--", "sh", "-c", "echo hi; exit 3"]);
    assert_eq!(run.status.code(), Some(3), "{}", err(&run));
    assert_eq!(out(&run), "hi\n");

    let output = sandbox.siftr(&["status", "-j"]);
    let stderr = err(&output);
    assert_eq!(
        stderr.matches("not valid TOML").count(),
        1,
        "warns once: {stderr}"
    );
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(source(&status, "rspec"), (true, "default".to_owned()));
    let files = status["config"]["files"].as_array().unwrap().clone();
    assert!(
        files.iter().any(|file| file["read"] == false),
        "status says the file wasn't used: {files:?}"
    );
}

#[test]
fn the_user_file_falls_back_to_home_when_xdg_config_home_is_unset() {
    let sandbox = Sandbox::new();
    let dir = sandbox.home.path().join(".config/siftr");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.toml"),
        "[sources.rspec]\nenabled = false\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(["status", "-j"])
        .current_dir(sandbox.root())
        .env("SIFTR_HOME", sandbox.home.path())
        .env("HOME", sandbox.home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        source(&status, "rspec"),
        (false, "~/.config/siftr/config.toml".to_owned()),
        "shown as the user would type it"
    );
}
