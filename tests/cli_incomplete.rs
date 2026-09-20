//! A run that didn't run the whole suite says so before anything else, in words and as `run.complete`, and shows
//! the error that stopped it in full.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

const CLEAN: [&str; 3] = ["baseline", "baseline_2", "baseline_documentation"];

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    /// Three clean runs of rails_demo.
    fn clean() -> Self {
        let sandbox = Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        };
        for scenario in CLEAN {
            sandbox.ingest(&fixture(scenario));
        }
        sandbox
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

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, dir: &Path) -> String {
        let dir = dir.to_str().unwrap();
        self.text(&["ingest", "--context", "rails_demo", "--dir", dir])
    }
}

fn fixture(scenario: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/rails_demo")
        .join(scenario)
}

/// The load_error capture with its syntax error's message padded past the 1024 bytes an exemplar keeps.
fn long_load_error() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in ["stdout.txt", "stderr.txt", "exit_code.txt", "test.log"] {
        std::fs::copy(fixture("load_error").join(name), dir.path().join(name)).unwrap();
    }
    let events = std::fs::read_to_string(fixture("load_error").join("rspec.ndjson")).unwrap();
    let end = r#"  30 | end\n","#;
    assert_eq!(events.matches(end).count(), 1);
    let padded = format!(r#"  30 | end\n{}\nEND-OF-MESSAGE","#, "x".repeat(1100));
    std::fs::write(
        dir.path().join("rspec.ndjson"),
        events.replace(end, &padded),
    )
    .unwrap();
    dir
}

#[test]
fn a_run_that_failed_to_load_a_file_says_so_first() {
    let sandbox = Sandbox::clean();
    let scenario = long_load_error();
    let recorded = sandbox.ingest(scenario.path());
    assert!(
        recorded.starts_with(
            "r4 (incomplete: 1 error outside examples) vs 3 baseline runs (r1 r2 r3): 1 change\n"
        ),
        "{recorded}"
    );
    let headline = recorded.lines().nth(1).unwrap();
    assert!(
        headline.starts_with("  s1   INCOMPLETE  ")
            && headline.ends_with("  failed outside examples: 1 now, 0 in baseline runs"),
        "{recorded}"
    );
    assert_eq!(sandbox.json(&["changes", "-j"])["run"]["complete"], false);
    assert_eq!(
        sandbox.json(&["changes", "r3", "-j"])["run"]["complete"],
        true
    );

    let explain = sandbox.text(&["explain", "s1"]);
    assert!(
        explain.contains(
            "\n          SyntaxError: ~/code/lib/rust/siftr/dogfood/rails_demo/spec/requests/users_spec.rb:29: syntax error found\n"
        ),
        "{explain}"
    );
    assert!(
        explain.contains("\n          END-OF-MESSAGE\n"),
        "the whole message, though the line kept as evidence is cut: {explain}"
    );
}

#[test]
fn a_run_stopped_early_says_how_far_it_got() {
    let sandbox = Sandbox::clean();
    let recorded = sandbox.ingest(&fixture("fail_fast"));
    assert!(
        recorded.starts_with(
            "r4 (incomplete: ran 6 of 10 examples) vs 3 baseline runs (r1 r2 r3): 2 changes, most important first\n"
        ),
        "{recorded}"
    );
    assert!(
        recorded.contains("  rspec  ran 6 examples, 10 in every baseline run\n"),
        "{recorded}"
    );
}
