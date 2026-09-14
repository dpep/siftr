//! Interpretation of real captures (`fixtures/rails_demo`): identity across clean runs, scope
//! attribution, and the listener's offsets. Also the event-capturing harness the unit tests share.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Duration;

use super::rspec::{EVENTS_STREAM, LOG_STREAM, Rspec};
use super::{Claim, Event, Outcome};
use crate::behavior::{BehaviorId, Kind};
use crate::normalize::Normalizer;
use crate::observation::{LineSplitter, Observation, Stream};

/// An owned copy of what an interpreter emitted.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Seen {
    pub kind: Kind,
    pub template: String,
    pub scope: Option<BehaviorId>,
    pub outcome: Option<Outcome>,
    pub duration: Option<Duration>,
    pub measures: Vec<(&'static str, f64)>,
    pub stream: String,
}

impl Seen {
    pub fn id(&self) -> BehaviorId {
        BehaviorId::of(self.kind, self.template.as_bytes())
    }
}

impl From<&Event<'_>> for Seen {
    fn from(event: &Event<'_>) -> Self {
        Seen {
            kind: event.kind,
            template: String::from_utf8_lossy(event.template.template).into_owned(),
            scope: event.scope,
            outcome: event.outcome,
            duration: event.duration,
            measures: event.measures.to_vec(),
            stream: event.source.stream.to_string(),
        }
    }
}

/// Feeds whole streams, in order, through one [`Rspec`] and returns every event it emitted.
pub(super) fn interpret(streams: &[(&str, &[u8])]) -> Vec<Seen> {
    let mut rspec = Rspec::default();
    let mut normalizer = Normalizer::new();
    let mut seen = Vec::new();
    let mut emit = |event: &Event<'_>| seen.push(Seen::from(event));
    for (name, bytes) in streams {
        let stream = Stream::File((*name).into());
        let mut splitter = LineSplitter::new();
        let mut observe = |seq, line: &[u8]| {
            let obs = Observation {
                stream: &stream,
                seq,
                line,
            };
            let claim = rspec.interpret(obs, &mut normalizer, &mut emit);
            assert_eq!(claim, Claim::Claimed, "{name} line {seq}");
        };
        splitter.feed(bytes, &mut observe);
        splitter.finish(&mut observe);
    }
    seen
}

const SCENARIOS: [&str; 7] = [
    "baseline",
    "baseline_2",
    "baseline_documentation",
    "fail",
    "n_plus_one",
    "slow",
    "warn",
];

fn read(scenario: &str, file: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../fixtures/rails_demo/{scenario}/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&path).unwrap_or_else(|error| panic!("reading {path}: {error}"))
}

fn capture(scenario: &str) -> Vec<Seen> {
    let events = read(scenario, "rspec.ndjson");
    let log = read(scenario, "test.log");
    interpret(&[(EVENTS_STREAM, &events), (LOG_STREAM, &log)])
}

/// Occurrences per (behavior, scope, measures), named by template so a failing diff says what moved.
/// A behavior id is a fixed hash of (kind, template), so equal templates are equal ids.
fn census(seen: &[Seen]) -> BTreeMap<String, usize> {
    let examples: HashMap<BehaviorId, &str> = seen
        .iter()
        .filter(|s| s.kind == Kind::TestExample)
        .map(|s| (s.id(), s.template.as_str()))
        .collect();
    let mut census = BTreeMap::new();
    for s in seen {
        let scope = s
            .scope
            .map_or("-", |id| examples.get(&id).copied().unwrap_or("?"));
        let key = format!("{} {} @ {scope} {:?}", s.kind, s.template, s.measures);
        *census.entry(key).or_default() += 1;
    }
    census
}

fn diff(
    before: &BTreeMap<String, usize>,
    after: &BTreeMap<String, usize>,
) -> Vec<(String, usize, usize)> {
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    keys.into_iter()
        .map(|key| {
            let count = |census: &BTreeMap<String, usize>| census.get(key).copied().unwrap_or(0);
            (key.clone(), count(before), count(after))
        })
        .filter(|(_, before, after)| before != after)
        .collect()
}

const SHOW: &str = "./spec/requests/users_spec.rb # Users shows a user with posts and comments";

#[test]
fn clean_runs_produce_identical_behaviors_scopes_and_counts() {
    let baseline = capture("baseline");
    let census_of = census(&baseline);
    for other in ["baseline_2", "baseline_documentation"] {
        let changed = diff(&census_of, &census(&capture(other)));
        assert!(changed.is_empty(), "{other}: {changed:#?}");
    }
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for s in &baseline {
        *kinds.entry(s.kind.to_string()).or_default() += 1;
    }
    let kinds: Vec<_> = kinds.iter().map(|(k, n)| (k.as_str(), *n)).collect();
    assert_eq!(
        kinds,
        [
            ("db.query", 196),
            ("http.request", 2),
            ("log", 9),
            ("test.example", 10),
            ("test.summary", 1)
        ],
        "a guard against passing on an empty interpretation"
    );
}

#[test]
fn n_plus_one_changes_only_the_show_example() {
    let changed = diff(
        &census(&capture("baseline")),
        &census(&capture("n_plus_one")),
    );
    let comments = "db.query Comment Load SELECT \"comments\".* FROM \"comments\" WHERE \"comments\".\"post_id\"";
    let show = "http.request GET UsersController#show 2xx";
    let mut expected = vec![
        (format!("{comments} = ? @ {SHOW} []"), 0, 8),
        (format!("{comments} IN (?) @ {SHOW} []"), 1, 0),
        (format!("{show} @ {SHOW} [(\"queries\", 3.0)]"), 1, 0),
        (format!("{show} @ {SHOW} [(\"queries\", 10.0)]"), 0, 1),
    ];
    expected.sort();
    assert_eq!(changed, expected);

    let example_queries = |seen: &[Seen]| {
        let show = BehaviorId::of(Kind::TestExample, SHOW.as_bytes());
        seen.iter()
            .filter(|s| s.kind == Kind::DbQuery && s.scope == Some(show))
            .filter(|s| !s.template.starts_with("TRANSACTION "))
            .count()
    };
    assert_eq!(
        (
            example_queries(&capture("baseline")),
            example_queries(&capture("n_plus_one"))
        ),
        (28, 35)
    );
}

#[test]
fn only_log_lines_before_the_first_example_are_unscoped() {
    let unscoped: Vec<_> = capture("baseline")
        .into_iter()
        .filter(|s| s.stream == format!("file:{LOG_STREAM}") && s.scope.is_none())
        .map(|s| s.template)
        .collect();
    assert_eq!(
        unscoped,
        [
            "ActiveRecord::InternalMetadata Load SELECT * FROM \"ar_internal_metadata\" WHERE \"ar_internal_metadata\".\"key\" = ? ORDER BY \"ar_internal_metadata\".\"key\" ASC LIMIT <int>",
            "ActiveRecord::SchemaMigration Load SELECT \"schema_migrations\".\"version\" FROM \"schema_migrations\" ORDER BY \"schema_migrations\".\"version\" ASC",
        ]
    );
}

#[test]
fn a_failure_carries_its_exception_class_scoped_to_the_example() {
    let example = "./spec/models/user_spec.rb # User requires an email";
    let example_id = BehaviorId::of(Kind::TestExample, example.as_bytes());
    let failures: Vec<_> = capture("fail")
        .into_iter()
        .filter(|s| s.outcome == Some(Outcome::Failure))
        .map(|s| (s.kind, s.template, s.scope))
        .collect();
    assert_eq!(
        failures,
        [
            (Kind::TestExample, example.to_owned(), None),
            (
                Kind::Exception,
                "RSpec::Expectations::ExpectationNotMetError".to_owned(),
                Some(example_id)
            ),
            (Kind::TestSummary, "rspec".to_owned(), None),
        ]
    );
}

#[test]
fn listener_offsets_land_on_log_line_starts() {
    for scenario in SCENARIOS {
        let log = read(scenario, "test.log");
        let starts: BTreeSet<usize> = std::iter::once(0)
            .chain(
                log.iter()
                    .enumerate()
                    .filter(|(_, b)| **b == b'\n')
                    .map(|(i, _)| i + 1),
            )
            .collect();
        let events = read(scenario, "rspec.ndjson");
        for line in events.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
            let event: serde_json::Value = serde_json::from_slice(line).expect("ndjson");
            let offset = event["log_offset"].as_u64().expect("log_offset") as usize;
            assert!(starts.contains(&offset), "{scenario}: {offset} is mid-line");
        }
    }
}
