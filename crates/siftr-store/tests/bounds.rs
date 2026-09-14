//! What a store never does to other siftr processes or to live answers: wait on another siftr without a bound,
//! prune a run that is still recording, or prune evidence a still-open change points to.

use std::fs::File;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use siftr_core::analyze::{Analysis, Analyzer};
use siftr_core::behavior::{BehaviorId, Kind};
use siftr_core::context::Context;
use siftr_core::observation::{LineSplitter, Stream};
use siftr_store::{Finished, NewRun, Retention, RunEnd, RunId, Store};

fn analyze(text: &str) -> Analysis {
    let mut analyzer = Analyzer::new();
    let stream = Stream::Stdout;
    let mut splitter = LineSplitter::new();
    splitter.feed(&stream, text.as_bytes(), |obs| analyzer.observe(obs));
    analyzer.finish()
}

fn begin(store: &Store, context: &Context, started_at: SystemTime) -> RunId {
    store
        .begin_run(&NewRun {
            context,
            command: context.name(),
            cwd: "/project",
            started_at,
        })
        .unwrap()
}

fn end(analysis: &Analysis) -> RunEnd {
    RunEnd {
        wall: Duration::from_millis(1),
        exit_code: Some(0),
        lines: analysis.observations,
    }
}

/// A finished run of `text`, judged against the context's recent runs.
fn record(store: &mut Store, context: &Context, text: &str) -> RunId {
    let run = begin(store, context, SystemTime::now());
    let analysis = analyze(text);
    let baseline: Vec<RunId> = store
        .baseline_runs(context, run, 10)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let finished = Finished {
        end: end(&analysis),
        analysis: &analysis,
        baseline_runs: &baseline,
        signals: &[],
    };
    store.finish_run(run, &finished).unwrap();
    run
}

fn interrupted(store: &mut Store, context: &Context, text: &str) -> RunId {
    let run = begin(store, context, SystemTime::now());
    let analysis = analyze(text);
    store
        .finish_interrupted_run(run, end(&analysis), &analysis, 2)
        .unwrap();
    run
}

/// A siftr suspended mid-migration, or a stale lock, must cost a run its recording, never its start.
#[test]
fn opening_gives_up_on_a_migration_lock_another_siftr_holds() {
    let home = tempfile::tempdir().unwrap();
    let held = File::create(home.path().join("siftr.lock")).unwrap();
    held.lock().unwrap();
    let path = home.path().to_owned();
    let (done, opened) = mpsc::channel();
    let started = Instant::now();
    thread::spawn(move || {
        let _ = done.send(Store::open(&path).map(drop));
    });
    let result = opened
        .recv_timeout(Duration::from_secs(4))
        .expect("open returns while another siftr still holds the lock");
    let error = result.expect_err("a store another siftr is migrating can't be used yet");
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(
        error.downcast_ref::<siftr_store::StoreBusy>().is_some(),
        "{error:#}"
    );
    drop(held);
}

#[test]
fn beginning_a_run_gives_up_on_a_write_lock_another_siftr_holds() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let other = rusqlite::Connection::open(home.path().join("siftr.db")).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let started = Instant::now();
    let result = store.begin_run(&NewRun {
        context: &Context::named("/project", "rspec"),
        command: "rspec",
        cwd: "/project",
        started_at: SystemTime::now(),
    });
    let waited = started.elapsed();
    other.execute_batch("ROLLBACK").unwrap();
    let error = result.expect_err("the write lock was held throughout");
    assert!(waited < Duration::from_secs(3), "waited {waited:?}");
    assert!(
        error.downcast_ref::<siftr_store::StoreBusy>().is_some(),
        "{error:#}"
    );
}

#[test]
fn a_run_still_recording_is_never_pruned_however_idle_but_an_abandoned_one_is() {
    let home = tempfile::tempdir().unwrap();
    let recorder = Store::open(home.path()).unwrap();
    let server = Context::named("/project", "rails server");
    let two_days_ago = SystemTime::now() - Duration::from_secs(2 * 24 * 60 * 60);
    let live = begin(&recorder, &server, two_days_ago);
    let mut capture = recorder.capture(live).unwrap();
    capture.write(&Stream::Stdout, b"booted\n").unwrap();
    capture.finish().unwrap();

    let mut other = Store::open(home.path()).unwrap();
    other.set_retention(Retention::from_vars(|name| {
        (name == "SIFTR_KEEP_DAYS").then(|| "1".to_owned())
    }));
    record(&mut other, &Context::named("/project", "echo"), "hi\n");
    let captured = other.capture_file(live, &Stream::Stdout);
    assert!(captured.exists(), "its capture is still being written");
    other
        .run_stats(live)
        .expect("a run still recording keeps its stats");
    assert!(other.prune_plan().unwrap().iter().all(|s| s.run != live));

    // siftr died mid-run: nothing holds the run any more, so the idle rule takes it.
    drop(recorder);
    other.prune(None).unwrap();
    let error = other.run_stats(live).unwrap_err();
    assert!(
        error.downcast_ref::<siftr_store::Pruned>().is_some(),
        "{error:#}"
    );
    assert!(!captured.exists());
}

/// Ctrl-C'd runs count toward the limits by position, but must not age out evidence that the latest run's
/// reminder of a still-open DISAPPEARED points `explain` to.
#[test]
fn evidence_a_still_open_change_points_to_outlives_interrupted_runs() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    store.set_retention(Retention::from_vars(|_| None));
    let suite = Context::named("/project", "rspec");
    let before: Vec<RunId> = (0..12)
        .map(|_| record(&mut store, &suite, "a\nb\n"))
        .collect();
    let disappeared = record(&mut store, &suite, "a\n");
    let mut latest = disappeared;
    for _ in 0..10 {
        interrupted(&mut store, &suite, "a\n");
        latest = record(&mut store, &suite, "a\n");
    }

    assert!(
        store.baseline_of(latest).unwrap().contains(&disappeared),
        "the change is still in the latest run's baseline, so it can be reminded"
    );
    store
        .exemplars(before[11], BehaviorId::of(Kind::Log, b"b"), 1)
        .expect("the evidence of what disappeared is kept");
}
