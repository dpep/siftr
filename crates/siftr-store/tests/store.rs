use std::time::{Duration, SystemTime};

use siftr_core::analyze::{Analysis, Analyzer};
use siftr_core::baseline::Baseline;
use siftr_core::context::Context;
use siftr_core::observation::{LineSplitter, Observation, Stream};
use siftr_core::signal::detect;
use siftr_store::{Finished, NewRun, Order, RunEnd, RunId, Store};

fn analyze(text: &str) -> Analysis {
    let mut analyzer = Analyzer::new();
    let stream = Stream::Stdout;
    let mut splitter = LineSplitter::new();
    splitter.feed(text.as_bytes(), |seq, line| {
        analyzer.observe(Observation {
            stream: &stream,
            seq,
            line,
        })
    });
    analyzer.finish()
}

fn record(store: &mut Store, context: &Context, text: &str, finish: bool) -> (RunId, Analysis) {
    let run = store
        .begin_run(&NewRun {
            context,
            command: context.name(),
            cwd: "/project",
            started_at: SystemTime::now(),
        })
        .unwrap();
    let analysis = analyze(text);
    if finish {
        let baseline = store.baseline_runs(context, run, 10).unwrap();
        let baseline_runs: Vec<RunId> = baseline.iter().map(|(id, _)| *id).collect();
        let signals = detect(
            &analysis.stats(),
            &Baseline::from_runs(baseline.iter().map(|(_, s)| s)),
        );
        let end = RunEnd {
            wall: Duration::from_millis(1500),
            exit_code: Some(3),
            lines: analysis.observations,
        };
        let finished = Finished {
            end,
            analysis: &analysis,
            baseline_runs: &baseline_runs,
            signals: &signals,
        };
        store.finish_run(run, &finished).unwrap();
    }
    (run, analysis)
}

#[test]
fn baselines_are_finished_runs_of_the_same_context_newest_first() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    let suite = Context::named("/project", "rspec");
    let other = Context::named("/project", "server");
    let (r1, _) = record(&mut store, &suite, "a\n", true);
    let (r2, _) = record(&mut store, &suite, "a\n", true);
    let (_, _) = record(&mut store, &other, "a\n", true);
    let (_unfinished, _) = record(&mut store, &suite, "a\n", false);
    let (r5, _) = record(&mut store, &suite, "a\nb\n", true);

    let baseline: Vec<RunId> = store.baseline_of(r5).unwrap();
    assert_eq!(baseline, [r2, r1]);
    let signals = store.signals(r5).unwrap();
    assert_eq!(signals.len(), 1, "b is new against two runs of a");
    assert_eq!(signals[0].behavior.template, "b");
    assert_eq!(
        store.latest_run("/project").unwrap().map(|r| r.id),
        Some(r5)
    );
    assert_eq!(
        store.runs("/project", 10).unwrap().len(),
        5,
        "history includes the unfinished run"
    );
}

#[test]
fn a_finished_run_reads_back_as_it_was_analyzed() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    let context = Context::named("/project", "rspec");
    let text = (1..=20)
        .map(|i| format!("GET /users/{i} in {i}.5ms\n"))
        .collect::<String>()
        + "ERROR boom\n";
    let (run, analysis) = record(&mut store, &context, &text, true);

    let record = store.run(run).unwrap().expect("run exists");
    assert_eq!(record.context, context);
    assert_eq!(
        record.end.map(|e| (e.exit_code, e.lines, e.wall)),
        Some((Some(3), 21, Duration::from_millis(1500)))
    );

    let stored = store.behaviors(run, Order::Count, 10).unwrap();
    let analyzed: Vec<_> = analysis
        .aggregates
        .iter()
        .map(|a| (a.behavior.clone(), a.stats))
        .collect();
    assert_eq!(stored, analyzed);

    let top = &analysis.aggregates[0];
    assert_eq!(
        store.exemplars(run, top.behavior.id, 100).unwrap(),
        top.exemplars
    );
    let prefix = &top.behavior.id.short()[..6];
    assert_eq!(
        store.resolve_behavior(prefix).unwrap(),
        Some(top.behavior.clone())
    );
    assert!(store.resolve_behavior("xyz!").is_err());
    assert_eq!(
        store.latest_run_with("/project", top.behavior.id).unwrap(),
        Some(run)
    );
}

#[test]
fn capture_keeps_raw_bytes_per_stream() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let context = Context::named("/project", "x");
    let run = store
        .begin_run(&NewRun {
            context: &context,
            command: "x",
            cwd: "/project",
            started_at: SystemTime::now(),
        })
        .unwrap();
    let mut capture = store.capture(run).unwrap();
    capture.write(&Stream::Stdout, b"one\n\x1b[1mtw").unwrap();
    capture.write(&Stream::Stderr, b"err\n").unwrap();
    capture.write(&Stream::Stdout, b"o\n").unwrap();
    capture.finish().unwrap();
    let stdout = std::fs::read(store.capture_file(run, &Stream::Stdout)).unwrap();
    assert_eq!(stdout, b"one\n\x1b[1mtwo\n");
    let stderr = std::fs::read(store.capture_file(run, &Stream::Stderr)).unwrap();
    assert_eq!(stderr, b"err\n");
}
