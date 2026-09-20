//! `rusage`: the kernel's accounting reaches a real run, however the command ended, and can be switched off.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn siftr(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_siftr"))
        .args(args)
        .current_dir(dir)
        .env("SIFTR_HOME", dir.join(".siftr-home"))
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap()
}

fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout {:?}, stderr {:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn project() -> TempDir {
    tempfile::tempdir().unwrap()
}

/// The run's `run.resources` behavior, if it recorded one.
fn accounting(dir: &Path, run: &str) -> Option<Value> {
    let doc = document(&siftr(dir, &["summary", run, "-j"]));
    doc["behaviors"]
        .as_array()
        .expect("behaviors")
        .iter()
        .find(|b| b["behavior"]["kind"] == "run.resources")
        .cloned()
}

/// The evidence line kept for a behavior: what `siftr explain <behavior>` shows a person.
fn evidence_line(dir: &Path, run: &str, behavior: &str) -> String {
    let doc = document(&siftr(dir, &["explain", behavior, "--run", run, "-j"]));
    doc["exemplars"][0]["line"]
        .as_str()
        .unwrap_or_else(|| panic!("an exemplar line: {doc}"))
        .to_owned()
}

/// `cpu 3.42s (3.1s user, 0.32s sys), max rss 412 MB, 1230 voluntary and 56 involuntary switches`
fn parse(line: &str) -> (f64, f64) {
    let after = |field: &str| -> f64 {
        let rest = line
            .split_once(field)
            .unwrap_or_else(|| panic!("no {field:?} in {line:?}"))
            .1;
        rest.trim()
            .split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .find(|word| !word.is_empty())
            .and_then(|word| word.parse().ok())
            .unwrap_or_else(|| panic!("no number after {field:?} in {line:?}"))
    };
    (after("cpu "), after("max rss "))
}

#[test]
fn a_run_records_what_the_kernel_charged_it() {
    let dir = project();
    let run = document(&siftr(
        dir.path(),
        &["run", "-j", "--", "sh", "-c", "sleep 0.2"],
    ));
    let id = run["run"]["id"].as_str().expect("a run id").to_owned();

    let behavior = accounting(dir.path(), &id).expect("the kernel's accounting");
    assert_eq!(behavior["stats"]["count"], 1, "one event per run");
    assert_eq!(
        behavior["behavior"]["template"], "siftr: what the kernel charged this run",
        "a template with no values in it"
    );

    // The numbers are evidence, readable without a unit lookup, and plausible for what ran.
    let id_short = behavior["behavior"]["id"].as_str().unwrap();
    let line = evidence_line(dir.path(), &id, id_short);
    let (cpu, rss_mb) = parse(&line);
    assert!(cpu < 1.0, "sleeping burns little CPU: {line}");
    assert!(
        (0.1..10_000.0).contains(&rss_mb),
        "peak rss in megabytes, normalized from the platform's own unit: {line}"
    );
    assert!(
        line.contains("involuntary switches"),
        "context switches are reported too: {line}"
    );
}

#[test]
fn a_busy_command_is_charged_more_cpu_than_an_idle_one() {
    let dir = project();
    let cpu_of = |script: &str| {
        let run = document(&siftr(dir.path(), &["run", "-j", "--", "sh", "-c", script]));
        let id = run["run"]["id"].as_str().expect("a run id").to_owned();
        let behavior = accounting(dir.path(), &id).expect("the kernel's accounting");
        let short = behavior["behavior"]["id"].as_str().unwrap().to_owned();
        parse(&evidence_line(dir.path(), &id, &short)).0
    };
    let idle = cpu_of("sleep 0.2");
    let busy = cpu_of("i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done");
    assert!(
        busy > idle,
        "a busy loop is charged more than a sleep: {busy} vs {idle}"
    );
}

/// An interrupted run is kept as evidence and never compared, but what the kernel gave is still recorded.
#[test]
fn a_command_killed_by_a_signal_still_records_its_accounting() {
    let dir = project();
    let output = siftr(dir.path(), &["run", "-j", "--", "sh", "-c", "kill -9 $$"]);
    let run = document(&output);
    assert_eq!(output.status.code(), Some(137), "128 + SIGKILL");
    assert_eq!(run["run"]["interrupted"], 9);

    let id = run["run"]["id"].as_str().expect("a run id");
    let behavior = accounting(dir.path(), id).expect("recorded even for a run that died");
    assert_eq!(behavior["stats"]["count"], 1);
}

#[test]
fn a_command_that_failed_is_measured_and_still_exits_as_it_did() {
    let dir = project();
    let output = siftr(dir.path(), &["run", "-j", "--", "sh", "-c", "exit 3"]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "principle 6: the child's code"
    );
    let run = document(&output);
    let id = run["run"]["id"].as_str().expect("a run id");
    assert!(accounting(dir.path(), id).is_some());
}

#[test]
fn turning_the_source_off_collects_nothing() {
    let dir = project();
    std::fs::write(
        dir.path().join(".siftr.toml"),
        "[sources.rusage]\nenabled = false\n",
    )
    .unwrap();
    let run = document(&siftr(dir.path(), &["run", "-j", "--", "echo", "hello"]));
    let id = run["run"]["id"].as_str().expect("a run id");
    assert_eq!(
        accounting(dir.path(), id),
        None,
        "a source that is off is never collected"
    );
}

#[test]
fn siftr_sources_lists_it_as_reading_no_stream() {
    let dir = project();
    let doc = document(&siftr(dir.path(), &["sources", "-j", "--", "echo", "hi"]));
    let row = doc["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .find(|s| s["name"] == "rusage")
        .unwrap_or_else(|| panic!("no rusage source in {doc}"));
    assert_eq!(row["on"], true, "on by default");
    assert_eq!(row["applies"], true, "it needs nothing of the command");
    assert_eq!(
        row["stream"],
        Value::Null,
        "it reads no bytes, so it feeds no stream"
    );

    // And the human listing shows an em dash where the others name a file.
    let text = String::from_utf8_lossy(&siftr(dir.path(), &["sources", "--", "echo", "hi"]).stdout)
        .into_owned();
    let row = text
        .lines()
        .find(|line| line.trim_start().starts_with("rusage"))
        .unwrap_or_else(|| panic!("no rusage row in {text}"));
    assert!(row.contains("— the CPU"), "{row}");
    assert!(!row.contains("file:"), "it names no file: {row}");
}

/// A run that never opened a stream for it: the accounting is not a side channel and never claims to be one.
#[test]
fn it_never_joins_the_streams_a_run_captured() {
    let dir = project();
    let run = document(&siftr(dir.path(), &["run", "-j", "--", "echo", "hello"]));
    assert_eq!(
        run["streams"],
        serde_json::json!(["stdout"]),
        "streams is what the command wrote, and it wrote no accounting"
    );
}
