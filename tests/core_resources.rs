//! The kernel's accounting is evidence the signal rules never judge.
//!
//! CPU and peak RSS vary run to run — `docs/findings/signals.md` §1 measured a suite's wall time spanning 10x —
//! so a measure rule reading them would raise a change on nearly every run and cost siftr its precision. This
//! is the test that says it doesn't: runs whose CPU and memory differ by three orders of magnitude, and not one
//! signal between them.

use std::time::Duration;

use siftr::aggregate::RunStats;
use siftr::analyze::Analyzer;
use siftr::baseline::Baseline;
use siftr::behavior::Kind;
use siftr::interpret::resources::{self, Resources};
use siftr::observation::{LineSplitter, Stream};
use siftr::signal::detect;

/// One run: the same output every time, and whatever the kernel happened to charge it.
fn run(output: &str, cpu_ms: u64, rss_mb: u64) -> RunStats {
    let mut analyzer = Analyzer::new();
    let stream = Stream::Stdout;
    let mut splitter = LineSplitter::new();
    splitter.feed(&stream, output.as_bytes(), |obs| analyzer.observe(obs));
    splitter.finish(&stream, |obs| analyzer.observe(obs));
    analyzer.record_resources(&Resources {
        cpu_user: Duration::from_millis(cpu_ms),
        cpu_system: Duration::from_millis(cpu_ms / 4),
        max_rss_bytes: rss_mb * 1_000_000,
        voluntary_switches: cpu_ms * 3,
        involuntary_switches: cpu_ms / 2,
    });
    analyzer.finish().stats()
}

const OUTPUT: &str = "starting the widget service\nready\n";

/// Wildly different machines, loads and workloads, run to run.
fn noisy_baseline() -> Vec<RunStats> {
    [(30, 12), (3_400, 412), (95, 40), (21_000, 3_800)]
        .map(|(cpu, rss)| run(OUTPUT, cpu, rss))
        .to_vec()
}

fn signals(current: &RunStats, baseline: &[RunStats]) -> Vec<siftr::signal::Signal> {
    let baseline = Baseline::from_runs(current, baseline.iter().enumerate());
    detect(current, &baseline)
}

#[test]
fn resource_usage_never_raises_a_signal_however_far_it_moves() {
    let baseline = noisy_baseline();
    // Two orders of magnitude past anything in the baseline, in both CPU and memory.
    let current = run(OUTPUT, 2_400_000, 96_000);

    let found = signals(&current, &baseline);
    assert!(
        found.is_empty(),
        "resource usage must never be judged, and raised: {:?}",
        found
            .iter()
            .map(|s| (s.kind, s.measure.as_str(), s.current))
            .collect::<Vec<_>>()
    );

    // And not because the behavior went missing: it is in every run, where `explain` can read it.
    assert_eq!(current.count(resources::behavior()), 1);
    for (n, run) in baseline.iter().enumerate() {
        assert_eq!(run.count(resources::behavior()), 1, "baseline run {n}");
    }
}

/// The guard against the assertion above passing for the wrong reason: the same runs, with one ordinary
/// behavior moved, do raise a signal. So `detect` was reached, and silence is about the kind, not the setup.
#[test]
fn an_ordinary_behavior_in_the_same_runs_still_fires() {
    let baseline = noisy_baseline();
    let mut output = OUTPUT.to_owned();
    output.push_str(&"ready\n".repeat(9));
    let current = run(&output, 2_400_000, 96_000);

    let found = signals(&current, &baseline);
    let kinds: Vec<(siftr::signal::SignalKind, &str)> =
        found.iter().map(|s| (s.kind, s.measure.as_str())).collect();
    assert_eq!(
        kinds,
        [(siftr::signal::SignalKind::Frequency, "count")],
        "the run's own output is still judged as it always was"
    );
    assert_ne!(
        found[0].behavior,
        resources::behavior(),
        "and what fired is the output, not the accounting"
    );
}

/// A first run after upgrading has the behavior that no earlier run has. NEW comes from the presence rule,
/// which `judge` never reaches for this kind — so upgrading is silent rather than one-off noisy.
#[test]
fn a_first_run_that_has_it_beside_baseline_runs_that_dont_is_silent() {
    let without: Vec<RunStats> = (0..3)
        .map(|_| {
            let mut analyzer = Analyzer::new();
            let stream = Stream::Stdout;
            let mut splitter = LineSplitter::new();
            splitter.feed(&stream, OUTPUT.as_bytes(), |obs| analyzer.observe(obs));
            splitter.finish(&stream, |obs| analyzer.observe(obs));
            analyzer.finish().stats()
        })
        .collect();
    assert!(
        without.iter().all(|r| r.count(resources::behavior()) == 0),
        "runs recorded before siftr measured this"
    );

    let upgraded = run(OUTPUT, 3_400, 412);
    assert!(
        signals(&upgraded, &without).is_empty(),
        "the first run after upgrading must not report the accounting as NEW"
    );

    // And the reverse, for a run recorded with the source switched off after runs that had it.
    let with = noisy_baseline();
    assert!(
        signals(&without[0], &with).is_empty(),
        "nor DISAPPEARED when it stops being collected"
    );
}

#[test]
fn the_accounting_is_the_one_kind_no_rule_judges() {
    let current = run(OUTPUT, 3_400, 412);
    let b = current
        .get(resources::behavior())
        .expect("the resources behavior");
    assert_eq!(b.behavior.kind, Kind::Resources);
    assert_eq!(b.stats.count, 1, "one event per run, not one per line");
    assert_eq!(
        b.behavior.template, "siftr: what the kernel charged this run",
        "the template carries no values, so the behavior id is stable across runs"
    );
}
