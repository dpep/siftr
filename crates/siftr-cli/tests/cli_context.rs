//! What makes two runs comparable: two apps in one repository that run the same command are different contexts.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

/// Prints the directory's own `spec.txt`: one command line, a different suite in each app.
const SUITE: [&str; 3] = ["sh", "-c", "cat spec.txt"];

fn siftr(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(dir)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

fn run_suite(home: &Path, dir: &Path) -> Value {
    let output = siftr(home, dir, &[&["run", "-j", "--"], &SUITE[..]].concat());
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// A repository holding apps `a` and `b`, each with a Gemfile and its own suite.
fn monorepo() -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    for (app, examples) in [("a", "Alpha one\nAlpha two\n"), ("b", "Beta three\n")] {
        let dir = repo.path().join(app);
        std::fs::create_dir_all(dir.join("spec")).unwrap();
        std::fs::write(dir.join("Gemfile"), "").unwrap();
        std::fs::write(dir.join("spec.txt"), examples).unwrap();
    }
    std::fs::copy(
        repo.path().join("a/spec.txt"),
        repo.path().join("a/spec/spec.txt"),
    )
    .unwrap();
    repo
}

#[test]
fn two_apps_in_one_repository_keep_separate_baselines() {
    let home = tempfile::tempdir().unwrap();
    let repo = monorepo();
    let (a, b) = (repo.path().join("a"), repo.path().join("b"));
    for _ in 0..3 {
        run_suite(home.path(), &a);
    }

    let first_in_b = run_suite(home.path(), &b);
    assert_eq!(
        (&first_in_b["baseline_runs"], &first_in_b["signals"]),
        (&serde_json::json!([]), &serde_json::json!([])),
        "b's first run has nothing of its own to compare with: {first_in_b:#}"
    );
    let b_root = std::fs::canonicalize(&b).unwrap();
    assert_eq!(first_in_b["run"]["project"], b_root.to_str().unwrap());

    let history = siftr(home.path(), &b, &["history"]);
    let history = String::from_utf8(history.stdout).unwrap();
    assert!(
        history.starts_with(&format!("runs in {}\n", b_root.display())),
        "{history}"
    );
    assert_eq!(history.matches("sh -c").count(), 1, "{history}");

    // A subdirectory of an app is still that app.
    let from_spec = run_suite(home.path(), &a.join("spec"));
    assert_eq!(from_spec["baseline_runs"].as_array().unwrap().len(), 3);
    assert_eq!(from_spec["signals"], serde_json::json!([]), "{from_spec:#}");
}
