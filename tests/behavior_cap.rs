//! What a run whose behaviors hit `MAX_BEHAVIORS` may claim.
//!
//! Past the cap a behavior is admitted on the arrival order of its first occurrence, so which behaviors a
//! truncated run reports depends on where in the stream they appeared. Two runs of the very same lines in a
//! different order then disagree about what exists — and that disagreement reads as NEW and DISAPPEARED.
//! An admitted behavior's own counts are exact (admission is decided at its first occurrence and never
//! revisited), so FREQUENCY, ERROR and LATENCY still stand; it is only *absence* that means nothing.
//!
//! The corpus here is the cheapest thing that gets past the cap: one line per distinct template. Letters,
//! not digits — the normalizer masks digit runs, which would fold them into one behavior.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use siftr::aggregate::{MAX_BEHAVIORS, RunStats};
use siftr::analyze::{Analysis, Analyzer};
use siftr::baseline::Baseline;
use siftr::observation::{LineSplitter, Stream};
use siftr::signal::{MAX_SIGNALS, SignalKind, detect};
use tempfile::TempDir;

/// Distinct templates emitted before the core, enough to fill the cap on their own.
const FILLER: usize = MAX_BEHAVIORS;
/// Distinct templates that lose to the filler when they arrive after it.
const CORE: usize = 60;

/// `n` as four lowercase letters: a distinct template the normalizer cannot fold.
fn token(mut n: usize) -> String {
    (0..4)
        .map(|_| {
            let letter = (b'a' + (n % 26) as u8) as char;
            n /= 26;
            letter
        })
        .collect()
}

fn filler() -> Vec<String> {
    (0..FILLER)
        .map(|i| format!("widget {} ready", token(i)))
        .collect()
}

fn core() -> Vec<String> {
    (0..CORE)
        .map(|i| format!("gadget {} done", token(i)))
        .collect()
}

/// The same lines every run: the filler first, so the cap fills with it and the core falls past it.
fn filler_first() -> String {
    let mut lines = filler();
    lines.extend(core());
    lines.join("\n") + "\n"
}

/// The identical multiset of lines, core first: now the core is admitted and the filler's tail is cut.
fn core_first() -> String {
    let mut lines = core();
    lines.extend(filler());
    lines.join("\n") + "\n"
}

fn analyze(text: &str) -> Analysis {
    let mut analyzer = Analyzer::new();
    let stream = Stream::Stdout;
    let mut splitter = LineSplitter::new();
    splitter.feed(&stream, text.as_bytes(), |obs| analyzer.observe(obs));
    splitter.finish(&stream, |obs| analyzer.observe(obs));
    analyzer.finish()
}

fn kinds(current: &RunStats, runs: &[RunStats]) -> Vec<SignalKind> {
    let baseline = Baseline::from_runs(current, runs.iter().enumerate());
    let mut kinds: Vec<SignalKind> = detect(current, &baseline)
        .into_iter()
        .map(|s| s.kind)
        .collect();
    kinds.sort();
    kinds.dedup();
    kinds
}

/// Reordering a run's lines changes nothing about what the run did, so it must change no verdict. Under the
/// cap it changed every verdict: 60 NEW and 60 DISAPPEARED at confidence 0.80, from identical content.
#[test]
fn reordering_the_same_lines_is_not_a_behavioral_change() {
    let truncated = analyze(&filler_first());
    assert_eq!(
        truncated.aggregates.len(),
        MAX_BEHAVIORS + 1,
        "the corpus must actually hit the cap: {} behaviors + the overflow one",
        MAX_BEHAVIORS
    );
    let baseline_runs = vec![truncated.stats(), truncated.stats(), truncated.stats()];
    let reordered = analyze(&core_first()).stats();

    assert_eq!(
        kinds(&reordered, &baseline_runs),
        [SignalKind::Incomplete],
        "the cap cut behaviors, so absence proves nothing: the only honest claim is that it was truncated"
    );
}

/// The invariant: a truncated run never presents "nothing changed", because it did not see everything.
#[test]
fn a_truncated_run_never_claims_nothing_changed() {
    let quiet = analyze(&filler_first()).stats();
    let runs = vec![quiet.clone(); 3];
    let signals = detect(
        &quiet,
        &Baseline::from_runs(&quiet, runs.iter().enumerate()),
    );

    assert_eq!(
        signals.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [SignalKind::Incomplete],
        "an identical run still cannot claim a clean comparison while its behaviors are cut"
    );
}

struct Sandbox {
    home: TempDir,
    project: TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            home: tempfile::tempdir().unwrap(),
            project: tempfile::tempdir().unwrap(),
        }
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

    fn json(&self, args: &[&str]) -> Value {
        serde_json::from_slice(&self.siftr(args).stdout).unwrap()
    }

    fn ingest(&self, name: &str, log: &str) -> Value {
        let path = self.project.path().join(format!("{name}.log"));
        std::fs::write(&path, log).unwrap();
        self.json(&["-j", path.to_str().unwrap(), "--context", "cap"])
    }

    fn ingest_fixture(&self, family: &str, scenario: &str) -> Value {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(family)
            .join(scenario);
        self.json(&["-j", dir.to_str().unwrap(), "--context", family])
    }
}

fn signal_kinds(changes: &Value) -> Vec<String> {
    changes["signals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// End to end, and persisted: `siftr changes` must still say so when asked again later, the way a refused
/// flood does. A run that reported "0 changes" here would have been actively misleading.
#[test]
fn a_truncated_run_reports_its_truncation_to_the_reader() {
    let sandbox = Sandbox::new();
    for run in 1..=3 {
        sandbox.ingest(&format!("run{run}"), &filler_first());
    }
    let reordered = sandbox.ingest("run4", &core_first());

    assert_eq!(
        reordered["run"]["overflow_events"].as_u64(),
        Some(CORE as u64),
        "the run really was truncated: {reordered}"
    );
    assert_eq!(
        signal_kinds(&reordered),
        ["incomplete"],
        "no NEW or DISAPPEARED invented from reordered lines: {reordered}"
    );
    assert_eq!(
        reordered["run"]["complete"], false,
        "a run that could not tell its behaviors apart did not see everything"
    );

    // Read back from the store, not from the run that produced it.
    let human = String::from_utf8(sandbox.siftr(&["changes", "r4"]).stdout).unwrap();
    let headline = human.lines().next().unwrap_or_default();
    assert!(
        !headline.contains("0 changes"),
        "\"0 changes\" from a run that did not look at everything: {human}"
    );
    assert!(
        headline.contains("incomplete"),
        "the verdict line says the comparison was partial: {human}"
    );
}

/// Truncation is a property of the run, not of its signals. A first run of a context has no baseline, so
/// `detect` returns before the INCOMPLETE is ever produced — and reading completeness from the signals then
/// calls the run complete, though it could not tell its behaviors apart.
#[test]
fn a_truncated_first_run_is_not_complete() {
    let sandbox = Sandbox::new();
    let first = sandbox.ingest("run1", &filler_first());

    assert_eq!(
        first["run"]["overflow_events"].as_u64(),
        Some(CORE as u64),
        "the run really was truncated: {first}"
    );
    assert!(
        signal_kinds(&first).is_empty(),
        "no baseline, so no signal can carry the truncation: {first}"
    );
    assert_eq!(
        first["run"]["complete"], false,
        "a run that couldn't record what it saw is not complete: {first}"
    );

    let human = String::from_utf8(sandbox.siftr(&["history"]).stdout).unwrap();
    assert!(
        human.contains("incomplete"),
        "a reader scanning runs must see that this one couldn't record what it saw: {human}"
    );
}

/// Truncated *and* past the change cap: the refusal stores no signals, so it discards the truncation
/// INCOMPLETE along with the flood. This run lost events and refused every change, yet read as the cleanest
/// run of its context — the compared ones are marked incomplete and it wasn't.
#[test]
fn a_truncated_run_whose_changes_were_refused_is_not_complete() {
    let sandbox = Sandbox::new();
    // Two short runs of lines the flood also has: enough baseline for NEW, and nothing to disappear.
    let seed = filler()[..2].join("\n") + "\n";
    for run in 1..=2 {
        sandbox.ingest(&format!("run{run}"), &seed);
    }
    let flooded = sandbox.ingest("run3", &filler_first());

    assert!(
        flooded["run"]["uncompared"]
            .as_u64()
            .is_some_and(|signals| signals as usize > MAX_SIGNALS),
        "the comparison was refused for producing too many signals: {flooded}"
    );
    assert_eq!(
        flooded["run"]["overflow_events"].as_u64(),
        Some(CORE as u64),
        "and the run was truncated as well: {flooded}"
    );
    assert!(
        signal_kinds(&flooded).is_empty(),
        "the refusal stored no signals, the truncation INCOMPLETE among them: {flooded}"
    );
    assert_eq!(
        flooded["run"]["complete"], false,
        "the most broken run of the context must not read as the cleanest: {flooded}"
    );

    let human = String::from_utf8(sandbox.siftr(&["history"]).stdout).unwrap();
    let row = human
        .lines()
        .find(|line| line.trim_start().starts_with("r3 "))
        .unwrap_or_default();
    assert!(
        row.contains("incomplete"),
        "the refused run's row says it couldn't record what it saw: {human}"
    );
}

/// The cap must never fire on a real suite: `dogfood/rails_demo` runs produce ~46 behaviors against a cap of
/// 20,000, so there are three orders of magnitude of headroom. The N+1 it exists to find still lands.
#[test]
fn a_real_suite_is_nowhere_near_the_cap() {
    let rails = Sandbox::new();
    for scenario in ["baseline", "baseline_2", "baseline_documentation"] {
        let run = rails.ingest_fixture("rails_demo", scenario);
        assert_eq!(run["run"]["overflow_events"].as_u64(), Some(0), "{run}");
    }
    let n_plus_one = rails.ingest_fixture("rails_demo", "n_plus_one");

    let behaviors = rails.json(&["summary", "-j"])["behaviors_total"]
        .as_u64()
        .unwrap();
    assert!(
        behaviors * 100 < MAX_BEHAVIORS as u64,
        "{behaviors} behaviors leaves little headroom under the {MAX_BEHAVIORS}-behavior cap"
    );
    assert_eq!(n_plus_one["run"]["overflow_events"].as_u64(), Some(0));
    assert_eq!(n_plus_one["run"]["complete"], true);
    assert_eq!(
        n_plus_one["changes"].as_u64(),
        Some(1),
        "the N+1 still lands"
    );
    assert!(
        !signal_kinds(&n_plus_one).contains(&"incomplete".to_owned()),
        "a suite far from the cap is never called incomplete: {n_plus_one}"
    );
}
