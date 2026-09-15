//! What makes two runs comparable: two suites in one repository that run the same command are different contexts.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

/// Prints the directory's own `spec.txt`: one command line, a different suite in each directory.
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

/// A repository holding suites `a` and `b`, with a Gemfile each or none; `a/spec` holds a copy of a's suite.
fn monorepo(manifests: bool) -> TempDir {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    for (dir, examples) in [
        ("a", "Alpha one\nAlpha two\n"),
        ("a/spec", "Alpha one\nAlpha two\n"),
        ("b", "Beta three\n"),
    ] {
        let dir = repo.path().join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.txt"), examples).unwrap();
    }
    if manifests {
        for app in ["a", "b"] {
            std::fs::write(repo.path().join(app).join("Gemfile"), "").unwrap();
        }
    }
    repo
}

/// Three runs in `a`, then b's first: it must have nothing of its own to compare with, and its history is its own.
fn b_starts_fresh(repo: &Path) -> TempDir {
    let home = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        run_suite(home.path(), &repo.join("a"));
    }
    let b = repo.join("b");
    let first_in_b = run_suite(home.path(), &b);
    assert_eq!(
        (&first_in_b["baseline_runs"], &first_in_b["signals"]),
        (&json!([]), &json!([])),
        "{first_in_b:#}"
    );
    let b_root = std::fs::canonicalize(&b).unwrap();
    assert_eq!(first_in_b["run"]["project"], b_root.to_str().unwrap());

    let history = String::from_utf8(siftr(home.path(), &b, &["history"]).stdout).unwrap();
    assert!(
        history.starts_with(&format!("runs in {}\n", b_root.display())),
        "{history}"
    );
    assert_eq!(history.matches("sh -c").count(), 1, "{history}");
    home
}

#[test]
fn two_apps_in_one_repository_keep_separate_baselines() {
    let repo = monorepo(true);
    let home = b_starts_fresh(repo.path());

    let from_spec = run_suite(home.path(), &repo.path().join("a/spec"));
    assert_eq!(
        from_spec["baseline_runs"].as_array().unwrap().len(),
        3,
        "a subdirectory of an app is that app"
    );
    assert_eq!(from_spec["signals"], json!([]), "{from_spec:#}");
}

#[test]
fn without_manifests_each_directory_keeps_its_own_baseline() {
    let repo = monorepo(false);
    let home = b_starts_fresh(repo.path());

    let from_spec = run_suite(home.path(), &repo.path().join("a/spec"));
    assert_eq!(
        (&from_spec["baseline_runs"], &from_spec["signals"]),
        (&json!([]), &json!([])),
        "nothing says a/spec belongs to a, so it starts fresh rather than risk a false signal"
    );
}
