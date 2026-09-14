use std::time::Duration;

use super::*;
use crate::aggregate::{DurationSummary, Measure, MeasureStats, ScopeStats, Stats};
use crate::behavior::Behavior;
use crate::interpret::rspec::summary;

/// A behavior as one run saw it.
#[derive(Clone)]
struct B(BehaviorStats);

fn b(kind: Kind, template: &str) -> B {
    B(BehaviorStats {
        behavior: Behavior::new(kind, template.as_bytes()),
        first: Some((Stream::File("log/test.log".into()), 1)),
        stats: Stats {
            count: 1,
            ..Stats::default()
        },
        measures: Vec::new(),
        scopes: Vec::new(),
        unattributed: 0,
    })
}

impl B {
    fn id(&self) -> BehaviorId {
        self.0.behavior.id
    }
    fn count(mut self, count: u64) -> Self {
        self.0.stats.count = count;
        self
    }
    fn seq(mut self, seq: u64) -> Self {
        self.0.first = Some((Stream::File("rspec-events".into()), seq));
        self
    }
    fn stderr(mut self) -> Self {
        self.0.first = Some((Stream::Stderr, 1));
        self
    }
    fn stdout(mut self) -> Self {
        self.0.first = Some((Stream::Stdout, 1));
        self
    }
    fn failed(mut self) -> Self {
        self.0.stats.errors = 1;
        self
    }
    fn ms(mut self, ms: f64) -> Self {
        let total = Duration::from_secs_f64(ms / 1e3);
        self.0.stats.duration = Some(DurationSummary {
            count: 1,
            total,
            p50: total,
            p95: total,
            max: total,
        });
        self
    }
    /// `count` occurrences inside `scope`, plus `queries` summed there when given.
    fn within(self, scope: &B, count: u64, queries: Option<f64>) -> Self {
        self.in_phase(Phase::Example(scope.id()), count, queries)
    }
    /// `count` occurrences in a phase other than setup, plus `queries` summed there when given.
    fn in_phase(mut self, phase: Phase, count: u64, queries: Option<f64>) -> Self {
        self.0.scopes.push(ScopeStats {
            scope: phase.scope_id().expect("setup is what no scope holds"),
            count,
            sums: queries
                .map(|q| ("queries".to_owned(), q))
                .into_iter()
                .collect(),
        });
        self.0.scopes.sort_by_key(|s| s.scope);
        self
    }
    fn queries(mut self, sum: f64) -> Self {
        self.0.measures = vec![Measure {
            name: "queries".to_owned(),
            stats: MeasureStats {
                count: self.0.stats.count,
                sum,
                min: sum,
                max: sum,
            },
        }];
        self
    }
}

fn run(behaviors: &[B]) -> RunStats {
    behaviors.iter().map(|b| b.0.clone()).collect()
}

/// A test summary: `examples` ran of `expected` loaded, with `errors` outside examples.
fn summary(examples: f64, expected: f64, errors: f64) -> B {
    let mut s = b(Kind::TestSummary, "rspec");
    s.0.measures = [
        (summary::ERRORS_OUTSIDE_OF_EXAMPLES, errors),
        (summary::EXAMPLES, examples),
        (summary::EXPECTED, expected),
    ]
    .map(|(name, sum)| Measure {
        name: name.to_owned(),
        stats: MeasureStats {
            count: 1,
            sum,
            min: sum,
            max: sum,
        },
    })
    .to_vec();
    s
}

fn baseline<'a>(current: &RunStats, runs: &'a [RunStats]) -> Baseline<'a, usize> {
    Baseline::from_runs(current, runs.iter().enumerate())
}

/// `(group, headline, kind, measure, current, confidence)` per signal.
type Row = (u32, bool, SignalKind, String, f64, f64);

fn rows(current: &RunStats, baseline: &[RunStats]) -> Vec<Row> {
    detect(current, &self::baseline(current, baseline))
        .into_iter()
        .map(|s| {
            (
                s.group,
                s.headline,
                s.kind,
                s.measure,
                s.current,
                s.confidence,
            )
        })
        .collect()
}

fn row(
    group: u32,
    headline: bool,
    kind: SignalKind,
    measure: &str,
    current: f64,
    conf: f64,
) -> Row {
    (group, headline, kind, measure.to_owned(), current, conf)
}

/// The rails_demo N+1: a request's query count moves inside one example, with the SQL that explains it.
/// signals.md vector 30 lists two supporting signals, but its 28 → 35 needs the `IN (…)` query that
/// the N+1 replaced to disappear, as it did in the backtest (whose supporting list includes it).
#[test]
fn an_n_plus_one_is_one_group_headed_by_the_request() {
    use SignalKind::*;
    let destroy = b(Kind::TestExample, "User destroys posts with the user")
        .seq(1)
        .ms(4.0);
    let show = b(
        Kind::TestExample,
        "Users shows a user with posts and comments",
    )
    .seq(2)
    .ms(15.0);
    let other_sql = b(Kind::DbQuery, "User Load")
        .count(27)
        .within(&show, 27, None);
    let setup = b(Kind::DbQuery, "SchemaMigration Load");
    let baseline_run = run(&[
        destroy.clone(),
        show.clone(),
        other_sql.clone(),
        setup.clone(),
        b(Kind::HttpRequest, "UsersController#show")
            .within(&show, 1, Some(3.0))
            .queries(3.0),
        b(Kind::DbQuery, "Comment Load post_id = ?").within(&destroy, 1, None),
        b(Kind::DbQuery, "Comment Load post_id IN (?)").within(&show, 1, None),
    ]);
    let current = run(&[
        destroy.clone(),
        show.clone(),
        other_sql,
        setup,
        b(Kind::HttpRequest, "UsersController#show")
            .within(&show, 1, Some(10.0))
            .queries(10.0),
        b(Kind::DbQuery, "Comment Load post_id = ?")
            .count(9)
            .within(&destroy, 1, None)
            .within(&show, 8, None),
    ]);
    let baseline = vec![baseline_run; 5];
    assert_eq!(
        rows(&current, &baseline),
        [
            row(1, true, Frequency, "queries", 10.0, 0.86),
            row(1, false, Frequency, "count", 9.0, 0.86),
            row(1, false, Frequency, "queries", 35.0, 0.86),
            row(1, false, Disappeared, "count", 0.0, 0.86),
        ]
    );
    let signals = detect(&current, &self::baseline(&current, &baseline));
    let sql = &signals[1];
    assert_eq!(
        sql.attribution,
        Some(Attribution {
            scope: Phase::Example(show.id()),
            current: 8.0,
            baseline: 0.0
        }),
        "the SQL moved inside the example the request did"
    );
    assert_eq!(
        (sql.baseline.median, sql.baseline.min, sql.baseline.max),
        (Some(1.0), Some(1.0), Some(1.0))
    );
}

#[test]
fn a_clean_run_has_no_signals() {
    let show = b(Kind::TestExample, "shows").seq(1).ms(15.0);
    let runs: Vec<RunStats> = [12.0, 15.0, 40.0, 9.0]
        .iter()
        .map(|&ms| {
            run(&[
                show.clone().ms(ms),
                b(Kind::DbQuery, "User Load")
                    .count(3)
                    .within(&show, 3, None),
                b(Kind::Log, "Finished in <duration> seconds").stdout(),
            ])
        })
        .collect();
    assert_eq!(rows(&runs[0], &runs[1..]), []);
}

#[test]
fn changes_before_the_first_example_form_one_setup_group_ranked_last() {
    use SignalKind::*;
    let example = b(Kind::TestExample, "requires a name").seq(1);
    let metadata = b(Kind::DbQuery, "ar_internal_metadata Load");
    let baseline = vec![run(&[example.clone(), metadata.clone()]); 3];
    let current = run(&[
        example.clone().failed(),
        metadata.count(2),
        b(Kind::DbQuery, "CREATE TABLE users"),
    ]);
    let signals = detect(&current, &self::baseline(&current, &baseline));
    let summary: Vec<_> = signals
        .iter()
        .map(|s| (s.group, s.kind, s.tier, s.outside_examples()))
        .collect();
    assert_eq!(
        summary,
        [
            (1, Error, 1, false),
            (2, New, SETUP_TIER, true),
            (2, Frequency, SETUP_TIER, true),
        ]
    );
}

#[test]
fn a_known_flaky_failure_is_not_an_error_but_a_new_exception_is() {
    let example = b(Kind::TestExample, "requires an email").seq(1);
    let timeout = b(Kind::Exception, "Net::ReadTimeout").within(&example, 1, None);
    let mismatch = b(Kind::Exception, "ExpectationNotMetError").within(&example, 1, None);
    let flaky_baseline = vec![
        run(&[example.clone().failed(), timeout.clone()]),
        run(std::slice::from_ref(&example)),
        run(std::slice::from_ref(&example)),
    ];
    let same = run(&[example.clone().failed(), timeout]);
    assert!(
        detect(&same, &baseline(&same, &flaky_baseline))
            .iter()
            .all(|s| s.kind != SignalKind::Error)
    );
    let different = run(&[example.clone().failed(), mismatch]);
    let error = detect(&different, &baseline(&different, &flaky_baseline))
        .into_iter()
        .find(|s| s.kind == SignalKind::Error)
        .expect("a different exception is news");
    assert_eq!(
        (
            error.confidence,
            error.baseline.failures,
            error.exception.as_deref()
        ),
        (0.6, Some(1), Some("ExpectationNotMetError"))
    );
}

#[test]
fn a_slow_example_is_latency_unless_its_neighbour_slowed_too() {
    let examples = |a: f64, b_ms: f64, c: f64| {
        run(&[
            b(Kind::TestExample, "a").seq(1).ms(a),
            b(Kind::TestExample, "b").seq(2).ms(b_ms),
            b(Kind::TestExample, "c").seq(3).ms(c),
        ])
    };
    let baseline = vec![
        examples(1.0, 1.1, 1.0),
        examples(1.1, 0.9, 1.2),
        examples(0.9, 1.0, 1.0),
    ];
    let slow = rows(&examples(1.0, 304.0, 1.1), &baseline);
    assert_eq!(
        slow,
        [row(1, true, SignalKind::Latency, "duration_ms", 304.0, 0.6)]
    );
    let stall = rows(&examples(1.0, 304.0, 201.0), &baseline);
    assert!(
        stall
            .iter()
            .all(|r| r.3 != "duration_ms" || r.2 != SignalKind::Latency || r.4 != 304.0),
        "b is vetoed by c's slowdown: {stall:?}"
    );
}

#[test]
fn stderr_messages_group_by_prefix_and_stdout_only_counts_without_a_reporter() {
    use SignalKind::*;
    let warning = "DEPRECATION WARNING: User#display_name is deprecated; use #name (called from";
    let baseline = vec![run(&[b(Kind::Log, "cache hit").stdout()]); 3];
    let current = run(&[
        b(Kind::Log, &format!("{warning} spec at <path>")).stderr(),
        b(Kind::Log, &format!("{warning} view at <path>")).stderr(),
        b(Kind::Log, "cache miss").stdout(),
    ]);
    let groups: Vec<_> = rows(&current, &baseline)
        .into_iter()
        .map(|r| (r.0, r.1, r.2))
        .collect();
    assert_eq!(
        groups,
        [
            (1, true, New),
            (1, false, New),
            (2, true, New),
            (3, true, Disappeared),
        ],
        "one stderr group, then stdout's own groups"
    );

    let example = b(Kind::TestExample, "passes").seq(1);
    let with_reporter = |line: &str| run(&[example.clone(), b(Kind::Log, line).stdout()]);
    let baseline = vec![with_reporter("10 examples, 0 failures"); 3];
    assert_eq!(
        rows(&with_reporter("10 examples, 1 failure"), &baseline),
        [],
        "a test reporter's stdout restates its events"
    );
}

#[test]
fn unattributed_occurrences_leave_a_signal_ungrouped() {
    let show = b(Kind::TestExample, "shows").seq(1);
    let other = b(Kind::TestExample, "lists").seq(2);
    let mut partial = b(Kind::DbQuery, "Comment Load")
        .count(9)
        .within(&show, 5, None);
    partial.0.unattributed = 4;
    let baseline = vec![
        run(&[
            show.clone(),
            other.clone(),
            b(Kind::DbQuery, "Comment Load").within(&show, 1, None)
        ]);
        3
    ];
    let current = run(&[show, other, partial]);
    let signals = detect(&current, &self::baseline(&current, &baseline));
    let sql = signals
        .iter()
        .find(|s| s.measure == "count")
        .expect("frequency still fires");
    assert_eq!(sql.attribution, None, "attribution is unknown, not guessed");
    assert!(
        signals.iter().all(|s| s.measure != "queries"),
        "an example's query total isn't claimed from partial attribution"
    );
}

#[test]
fn changes_after_the_last_example_are_teardown_ranked_with_setup() {
    use SignalKind::*;
    let example = b(Kind::TestExample, "passes").seq(1);
    let metadata = b(Kind::DbQuery, "ar_internal_metadata Load");
    let runs = vec![run(&[example.clone(), metadata.clone()]); 3];
    let current = run(&[
        example.clone().failed(),
        metadata.count(2),
        b(Kind::DbQuery, "Job Load").in_phase(Phase::Teardown, 1, None),
    ]);
    let phases: Vec<_> = detect(&current, &baseline(&current, &runs))
        .iter()
        .map(|s| (s.group, s.kind, s.tier, s.attribution.map(|a| a.scope)))
        .collect();
    assert_eq!(
        phases,
        [
            (1, Error, 1, None),
            (2, New, SETUP_TIER, Some(Phase::Teardown)),
            (3, Frequency, SETUP_TIER, Some(Phase::Setup)),
        ]
    );
}

/// hunt-blind: one run that didn't run the whole suite used to silence every count rule for ten runs.
#[test]
fn one_incomplete_run_in_the_baseline_does_not_blind_the_rest() {
    let [a, c, d, show] = ["a", "c", "d", "shows"].map(|t| b(Kind::TestExample, t));
    let request = |q| {
        b(Kind::HttpRequest, "UsersController#show")
            .within(&show, 1, Some(q))
            .queries(q)
    };
    let suite = |ran: f64, queries| {
        run(&[
            summary(ran, ran, 0.0),
            a.clone(),
            c.clone(),
            d.clone(),
            show.clone(),
            request(queries),
        ])
    };
    let (clean, current) = (suite(4.0, 3.0), suite(4.0, 10.0));
    let incomplete = [
        ("killed", run(std::slice::from_ref(&a))),
        (
            "load error",
            run(&[summary(3.0, 3.0, 1.0), a.clone(), c.clone(), d.clone()]),
        ),
        (
            "fail-fast",
            run(&[summary(2.0, 4.0, 0.0), a.clone(), c.clone()]),
        ),
    ];
    for (name, incomplete) in incomplete {
        let runs = [clean.clone(), incomplete, clean.clone(), clean.clone()];
        let comparable = baseline(&current, &runs);
        assert_eq!(
            comparable.keys().copied().collect::<Vec<_>>(),
            [0, 2, 3],
            "{name}"
        );
        assert_eq!(
            rows(&current, &runs),
            [row(1, true, SignalKind::Frequency, "queries", 10.0, 0.8)],
            "{name}"
        );
    }
}

/// hunt #4: after an upgrade, runs recorded without test results made every example NEW.
#[test]
fn runs_recorded_before_test_results_were_read_are_no_baseline_for_a_test_run() {
    let output = b(Kind::Log, "1 example, 0 failures").stdout();
    let old = vec![run(std::slice::from_ref(&output)); 3];
    let current = run(&[
        summary(1.0, 1.0, 0.0),
        b(Kind::TestExample, "passes").seq(1),
        output,
    ]);
    assert_eq!(baseline(&current, &old).skipped().len(), 3);
    assert_eq!(rows(&current, &old), []);
}

/// A run that stopped early: what it didn't run stays quiet, what did run is still judged.
#[test]
fn an_incomplete_run_keeps_what_ran_and_names_what_did_not() {
    use SignalKind::*;
    let [a, c, d] = ["a", "c", "d"].map(|t| b(Kind::TestExample, t));
    let posts = |q| {
        b(Kind::HttpRequest, "PostsController#index")
            .within(&c, 1, Some(q))
            .queries(q)
    };
    let clean = run(&[
        summary(3.0, 3.0, 0.0),
        a.clone(),
        c.clone(),
        d.clone(),
        posts(3.0),
        b(Kind::Log, "warming cache").stderr(),
    ]);
    let stopped = run(&[
        summary(2.0, 3.0, 0.0),
        a.clone().failed(),
        c.clone(),
        posts(5.0),
    ]);
    assert_eq!(
        rows(&stopped, &vec![clean; 3]),
        [
            row(1, true, Incomplete, "examples", 2.0, 0.8),
            row(2, true, Error, "failed", 1.0, 0.8),
            row(3, true, Frequency, "queries", 5.0, 0.8),
        ],
        "no DISAPPEARED for d, which didn't run, nor for stderr, which a partial run can lack"
    );
}
