//! What a context's first run says about itself: what siftr read here, what that means it can't report, and
//! whether a blank history is the surprise. Said once per context, and never past the quiet flags.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn siftr(home: &Path, dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(dir)
        .env("SIFTR_HOME", home)
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

/// A project root of its own, so two tests never share one.
fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".git")).unwrap();
    dir
}

/// `siftr run -q -- sh -c <script>`, returning siftr's own output.
fn run(home: &Path, dir: &Path, script: &str) -> String {
    let output = siftr(home, dir, &["run", "-q", "--", "sh", "-c", script]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn the_first_run_of_a_context_says_what_siftr_could_and_could_not_read() {
    let (home, dir) = (tempfile::tempdir().unwrap(), project());
    let first = run(home.path(), dir.path(), "echo one");

    assert!(
        first.contains("read: stdout, stderr, rusage\n"),
        "the sources that fed this run, in listing order:\n{first}"
    );
    // The limitation the trial never discovered: no telemetry means no query-shaped change can ever fire.
    assert!(
        first.contains("not read: rspec (the command isn't an rspec run)")
            && first.contains("no per-example timing"),
        "{first}"
    );
    assert!(
        first.contains("not read: rails_log (the command isn't a Ruby test run)")
            && first.contains("no query-count or query-latency change can be found"),
        "{first}"
    );
    assert!(
        first.contains("2 more runs of this command before anything but ERROR can fire"),
        "and how far off a comparison is:\n{first}"
    );
    assert!(
        first.contains("siftr sources -- sh -c 'echo one'"),
        "with the command that says more:\n{first}"
    );

    // Once per context, not per run: the same command again is the report as it always was.
    let second = run(home.path(), dir.path(), "echo one");
    assert!(
        !second.contains("read:") && !second.contains("not read:"),
        "the note must not repeat:\n{second}"
    );
}

/// Being off is not the same as not applying, and the reason is the one the user can act on.
#[test]
fn a_source_switched_off_says_so_rather_than_why_it_would_not_have_applied() {
    let (home, dir) = (tempfile::tempdir().unwrap(), project());
    std::fs::write(
        dir.path().join(".siftr.toml"),
        "[sources.rusage]\nenabled = false\n",
    )
    .unwrap();
    let first = run(home.path(), dir.path(), "echo one");
    assert!(
        first.contains("not read: rusage (switched off in .siftr.toml)")
            && first.contains("no CPU, memory or context-switch accounting"),
        "{first}"
    );
}

#[test]
fn a_first_run_with_nothing_else_in_the_project_is_not_a_surprise() {
    let (home, dir) = (tempfile::tempdir().unwrap(), project());
    let first = run(home.path(), dir.path(), "echo one");
    assert!(
        !first.contains("new baseline"),
        "a new project starting fresh is what a first run is:\n{first}"
    );
}

#[test]
fn a_new_context_beside_one_with_history_says_why_it_starts_from_scratch() {
    let (home, dir) = (tempfile::tempdir().unwrap(), project());
    for _ in 0..3 {
        run(home.path(), dir.path(), "echo one");
    }
    let fresh = run(home.path(), dir.path(), "echo two");
    // The rule itself, not a pointer to it: a README read hours ago isn't available when a command changes.
    assert!(
        fresh.contains(
            "new baseline: a baseline is keyed on the project and the command as you typed it, \
             so `sh -c 'echo two'` starts from nothing rather than joining `sh -c 'echo one'` (3 runs here)"
        ),
        "the rule, both commands and the consequence, in the line that surprises someone:\n{fresh}"
    );
}

/// The neighbour worth naming is the near miss, not the busiest context: a command sharing leading words is
/// the one whose history the user thought they were adding to.
#[test]
fn the_nearest_context_is_named_over_the_one_with_the_most_runs() {
    let (home, dir) = (tempfile::tempdir().unwrap(), project());
    for _ in 0..3 {
        let output = siftr(home.path(), dir.path(), &["run", "-q", "--", "true"]);
        assert_eq!(output.status.code(), Some(0));
    }
    run(home.path(), dir.path(), "echo one");

    let fresh = run(home.path(), dir.path(), "echo two");
    assert!(
        fresh.contains("rather than joining `sh -c 'echo one'` (1 run here)"),
        "one shared-prefix run beats three unrelated ones:\n{fresh}"
    );
}

#[test]
fn the_quiet_flags_still_mean_silence_on_a_first_run() {
    for flag in ["--no-report", "--quiet-unless-changed"] {
        let (home, dir) = (tempfile::tempdir().unwrap(), project());
        let output = siftr(
            home.path(),
            dir.path(),
            &["run", "-q", flag, "--", "sh", "-c", "echo one"],
        );
        assert_eq!(
            (
                output.status.code(),
                output.stdout.len(),
                output.stderr.len()
            ),
            (Some(0), 0, 0),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// `-j` prints one document on stdout, shared with `changes`, which could never reconstruct the note. So the
/// note goes to stderr, where everything else siftr says about a run already goes.
#[test]
fn under_j_the_note_is_on_stderr_and_the_document_is_unchanged() {
    let (home, dir) = (tempfile::tempdir().unwrap(), project());
    let output = siftr(
        home.path(),
        dir.path(),
        &["run", "-j", "--", "sh", "-c", "echo one"],
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["baseline_runs"], serde_json::json!([]));
    assert!(
        document.get("first_run").is_none(),
        "the run document keeps the shape `changes -j` also answers with"
    );

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("siftr: r1: first run of this context\n"),
        "headed, so its lines aren't orphaned beside a document:\n{stderr}"
    );
    assert!(stderr.contains("not read: rspec"), "{stderr}");
}
