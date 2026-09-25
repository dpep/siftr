use std::time::{Duration, SystemTime};

use siftr::aggregate::{Aggregator, Phase};
use siftr::analyze::{Analysis, Analyzer};
use siftr::baseline::Baseline;
use siftr::behavior::{BehaviorId, Kind};
use siftr::context::Context;
use siftr::interpret::Event;
use siftr::normalize::Normalizer;
use siftr::observation::{LineSplitter, Observation, Stream};
use siftr::signal::detect;
use siftr::store::{Finished, NewRun, Order, RunEnd, RunId, Store};

fn analyze(text: &str) -> Analysis {
    let mut analyzer = Analyzer::new();
    let stream = Stream::Stdout;
    let mut splitter = LineSplitter::new();
    splitter.feed(&stream, text.as_bytes(), |obs| analyzer.observe(obs));
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
        let stats = analysis.stats();
        let signals = detect(
            &stats,
            &Baseline::from_runs(&stats, baseline.iter().map(|(id, s)| (*id, s))),
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

/// One run's aggregates from `(kind, line, scope)` events, each with an optional `queries` measure.
fn scoped_analysis(events: &[(Kind, &str, Option<BehaviorId>, Option<f64>)]) -> Analysis {
    let mut aggregator = Aggregator::new();
    let mut normalizer = Normalizer::new();
    let stream = Stream::File("log/test.log".into());
    for (seq, &(kind, line, scope, queries)) in (1..).zip(events) {
        let measures = queries.map(|q| [("queries", q)]);
        aggregator.record(&Event {
            kind,
            template: normalizer.normalize(line.as_bytes()),
            input: line.as_bytes(),
            source: Observation {
                stream: &stream,
                seq,
                line: line.as_bytes(),
                raw_len: line.len() as u64 + 1,
            },
            duration: None,
            outcome: None,
            scope,
            measures: measures.as_ref().map_or(&[], |m| m.as_slice()),
        });
    }
    Analysis {
        observations: events.len() as u64,
        aggregates: aggregator.finish(),
        sources: Vec::new(),
    }
}

#[test]
fn scopes_measures_and_grouped_signals_read_back_as_detected() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    let context = Context::named("/project", "rspec");
    let example = BehaviorId::of(Kind::TestExample, b"shows a user");
    let run_of = |queries: usize| {
        let mut events = vec![
            (Kind::TestExample, "shows a user", None, None),
            (Kind::DbQuery, "SchemaMigration Load", None, None),
            (
                Kind::HttpRequest,
                "GET UsersController#show 2xx",
                Some(example),
                Some(queries as f64),
            ),
        ];
        events.extend(std::iter::repeat_n(
            (Kind::DbQuery, "Comment Load", Some(example), None),
            queries,
        ));
        scoped_analysis(&events)
    };
    let mut detected = Vec::new();
    let mut last = None;
    for queries in [3, 3, 10] {
        let run = store
            .begin_run(&NewRun {
                context: &context,
                command: "rspec",
                cwd: "/project",
                started_at: SystemTime::now(),
            })
            .unwrap();
        let analysis = run_of(queries);
        let baseline = store.baseline_runs(&context, run, 10).unwrap();
        let baseline_runs: Vec<RunId> = baseline.iter().map(|(id, _)| *id).collect();
        let stats = analysis.stats();
        detected = detect(
            &stats,
            &Baseline::from_runs(&stats, baseline.iter().map(|(id, s)| (*id, s))),
        );
        let end = RunEnd {
            wall: Duration::from_millis(5),
            exit_code: Some(0),
            lines: analysis.observations,
        };
        store
            .finish_run(
                run,
                &Finished {
                    end,
                    analysis: &analysis,
                    baseline_runs: &baseline_runs,
                    signals: &detected,
                },
            )
            .unwrap();
        assert_eq!(
            store.run_stats(run).unwrap(),
            analysis.stats(),
            "run stats round-trip"
        );
        last = Some(run);
    }
    let stored = store.signals(last.unwrap()).unwrap();
    assert!(!detected.is_empty());
    assert_eq!(
        stored.iter().map(|s| s.signal.clone()).collect::<Vec<_>>(),
        detected
    );
    let sql = stored
        .iter()
        .find(|s| s.behavior.template == "Comment Load")
        .expect("the SQL behind the request");
    assert_eq!(
        sql.scope.as_ref().map(|b| b.template.as_str()),
        Some("shows a user")
    );
    assert_eq!(sql.exemplars, 8);
}

/// A run with no test examples scopes its lines to the request that made them. That scope must read back
/// as a request: its endpoint names no behavior, so the stored id alone cannot say which kind it is.
#[test]
fn a_request_scope_reads_back_as_a_request_not_an_example() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    let context = Context::named("/project", "traffic");
    let endpoint = BehaviorId::of(Kind::HttpRequest, b"GET PostsController#index");
    let run_of = |queries: usize| {
        let mut events = vec![(
            Kind::HttpRequest,
            "GET PostsController#index 2xx",
            Some(endpoint),
            Some(queries as f64),
        )];
        events.extend(std::iter::repeat_n(
            (Kind::DbQuery, "Comment Load", Some(endpoint), None),
            queries,
        ));
        scoped_analysis(&events)
    };
    let mut detected = Vec::new();
    let mut last = None;
    for queries in [2, 2, 9] {
        let run = store
            .begin_run(&NewRun {
                context: &context,
                command: "curl",
                cwd: "/project",
                started_at: SystemTime::now(),
            })
            .unwrap();
        let analysis = run_of(queries);
        let baseline = store.baseline_runs(&context, run, 10).unwrap();
        let baseline_runs: Vec<RunId> = baseline.iter().map(|(id, _)| *id).collect();
        let stats = analysis.stats();
        detected = detect(
            &stats,
            &Baseline::from_runs(&stats, baseline.iter().map(|(id, s)| (*id, s))),
        );
        let end = RunEnd {
            wall: Duration::from_millis(5),
            exit_code: Some(0),
            lines: analysis.observations,
        };
        store
            .finish_run(
                run,
                &Finished {
                    end,
                    analysis: &analysis,
                    baseline_runs: &baseline_runs,
                    signals: &detected,
                },
            )
            .unwrap();
        last = Some(run);
    }
    let stored = store.signals(last.unwrap()).unwrap();
    assert!(!detected.is_empty(), "the query count moved");
    assert_eq!(
        stored.iter().map(|s| s.signal.clone()).collect::<Vec<_>>(),
        detected,
        "every signal reads back as detected, its request scope included"
    );
    let head = &stored[0];
    assert_eq!(
        head.signal.attribution.map(|a| a.scope),
        Some(Phase::Request(endpoint)),
        "not Phase::Example, which is what a bare id would read as"
    );
    assert!(
        head.scope.is_none(),
        "an endpoint is no behavior, so the store has none to name beside it"
    );
}

#[test]
fn an_interrupted_run_keeps_its_evidence_but_never_joins_a_baseline() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    let suite = Context::named("/project", "rspec");
    let (r1, _) = record(&mut store, &suite, "a\nb\n", true);
    let (r2, _) = record(&mut store, &suite, "a\nb\n", true);
    let interrupted = store
        .begin_run(&NewRun {
            context: &suite,
            command: "rspec",
            cwd: "/project",
            started_at: SystemTime::now(),
        })
        .unwrap();
    let partial = analyze("a\n");
    let end = RunEnd {
        wall: Duration::from_millis(40),
        exit_code: Some(130),
        lines: partial.observations,
    };
    store
        .finish_interrupted_run(interrupted, end, &partial, 2)
        .unwrap();
    let (r4, _) = record(&mut store, &suite, "a\nb\n", true);

    let stored = store.run(interrupted).unwrap().expect("recorded");
    assert_eq!(
        (stored.interrupted, stored.end.map(|e| e.exit_code)),
        (Some(2), Some(Some(130)))
    );
    assert!(
        store.signals(interrupted).unwrap().is_empty(),
        "a partial run claims no changes"
    );
    assert_eq!(
        store
            .behaviors(interrupted, Order::Count, 10)
            .unwrap()
            .len(),
        1,
        "its evidence is kept"
    );
    assert_eq!(store.baseline_of(r4).unwrap(), [r2, r1]);
    assert_eq!(store.run(r4).unwrap().unwrap().interrupted, None);
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

/// Two `siftr run`s started together on a fresh or older home must both open it: a failed open is a
/// command that never ran.
#[test]
fn concurrent_opens_of_an_unmigrated_home_all_succeed() {
    const OPENS: usize = 16;
    for _ in 0..100 {
        let home = tempfile::tempdir().unwrap();
        let start = std::sync::Barrier::new(OPENS);
        std::thread::scope(|scope| {
            let opens: Vec<_> = (0..OPENS)
                .map(|_| {
                    scope.spawn(|| {
                        start.wait();
                        Store::open(home.path()).map(drop)
                    })
                })
                .collect();
            for open in opens {
                if let Err(error) = open.join().unwrap() {
                    panic!("{error:#}");
                }
            }
        });
    }
}

/// Stats for 21 runs of each context, evidence for 2, contexts idle for 30 days forgotten.
fn smallest_retention() -> siftr::store::Retention {
    siftr::store::Retention::from_vars(|name| match name {
        "SIFTR_KEEP_RUNS" => Some("21".to_owned()),
        "SIFTR_KEEP_EVIDENCE" => Some("2".to_owned()),
        _ => None,
    })
}

fn pruned(error: anyhow::Error) -> siftr::store::Pruned {
    error
        .downcast::<siftr::store::Pruned>()
        .expect("a Pruned error")
}

#[test]
fn retention_keeps_what_the_latest_run_reads_and_runs_still_recording() {
    let home = tempfile::tempdir().unwrap();
    let mut store = Store::open(home.path()).unwrap();
    store.set_retention(smallest_retention());
    let suite = Context::named("/project", "rspec");
    let usable: Vec<RunId> = (0..21)
        .map(|_| record(&mut store, &suite, "a\n", true).0)
        .collect();
    // Interrupted runs count toward the limits, but never cost the latest run its baselines.
    for _ in 0..5 {
        let run = store
            .begin_run(&NewRun {
                context: &suite,
                command: "rspec",
                cwd: "/project",
                started_at: SystemTime::now(),
            })
            .unwrap();
        let end = RunEnd {
            wall: Duration::from_millis(1),
            exit_code: Some(130),
            lines: 1,
        };
        store
            .finish_interrupted_run(run, end, &analyze("a\n"), 2)
            .unwrap();
    }
    let (recording, _) = record(&mut store, &suite, "a\n", false);
    // A command last run 40 days ago, whose runs finish now.
    let idle = Context::named("/project", "rake");
    let long_ago = store
        .begin_run(&NewRun {
            context: &idle,
            command: "rake",
            cwd: "/project",
            started_at: SystemTime::now() - Duration::from_secs(40 * 24 * 60 * 60),
        })
        .unwrap();
    store.capture(long_ago).unwrap().finish().unwrap();
    assert!(
        store
            .capture_file(long_ago, &Stream::Stdout)
            .parent()
            .unwrap()
            .exists()
    );
    let analysis = analyze("b\n");
    let end = RunEnd {
        wall: Duration::from_millis(1),
        exit_code: Some(0),
        lines: 1,
    };
    let finished = Finished {
        end,
        analysis: &analysis,
        baseline_runs: &[],
        signals: &[],
    };
    store.finish_run(long_ago, &finished).unwrap();

    assert!(
        store.prune_plan().unwrap().is_empty(),
        "each finish pruned what was due"
    );
    for &run in &usable {
        store
            .run_stats(run)
            .expect("all 21 usable runs keep their stats");
    }
    store
        .run_stats(recording)
        .expect("a run still recording is untouched");
    let (newest, older) = usable.split_last().unwrap();
    store
        .exemplars(*newest, BehaviorId::of(Kind::Log, b"a"), 1)
        .unwrap();
    store
        .exemplars(older[older.len() - 1], BehaviorId::of(Kind::Log, b"a"), 1)
        .unwrap();
    let oldest = pruned(
        store
            .exemplars(usable[0], BehaviorId::of(Kind::Log, b"a"), 1)
            .unwrap_err(),
    );
    assert_eq!(
        (oldest.tier, oldest.by.as_str()),
        (siftr::store::Tier::Evidence, "SIFTR_KEEP_EVIDENCE=2")
    );
    let forgotten = pruned(store.run_stats(long_ago).unwrap_err());
    assert_eq!(
        (forgotten.tier, forgotten.by.as_str()),
        (siftr::store::Tier::Stats, "SIFTR_KEEP_DAYS=30")
    );
    assert!(
        !store
            .capture_file(long_ago, &Stream::Stdout)
            .parent()
            .unwrap()
            .exists()
    );
}
