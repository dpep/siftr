//! Behavioral changes between a run and its baseline, grouped under the change that explains them.
//!
//! Rules, thresholds, tiers and grouping follow `docs/findings/signals.md`; the rules themselves are
//! pure functions in [`rules`]. This module decides which rule applies to which behavior, and how the
//! resulting signals group and rank.

pub mod rules;

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::str::FromStr;

use crate::aggregate::{BehaviorStats, Phase, RunStats, overflow_behavior};
use crate::baseline::{Baseline, Ineligible};
use crate::behavior::{Behavior, BehaviorId, Kind};
use crate::interpret::rspec::{EVENTS_STREAM, summary};
use crate::num::{median, round_sig};
use crate::observation::Stream;
use rules::Presence;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SignalKind {
    /// The run didn't run what its baseline runs did (a spec file failed to load, it stopped early): what it
    /// lacks is not signalled, and this says why.
    Incomplete,
    /// An example failed that passed in the baseline.
    Error,
    /// Occurs now; absent from every baseline run.
    New,
    /// Absent now; present in every baseline run.
    Disappeared,
    /// A count or measure left what every baseline run agreed on.
    Frequency,
    /// An example got much slower than every baseline run.
    Latency,
}

impl SignalKind {
    pub const ALL: [SignalKind; 6] = [
        SignalKind::Incomplete,
        SignalKind::Error,
        SignalKind::New,
        SignalKind::Disappeared,
        SignalKind::Frequency,
        SignalKind::Latency,
    ];

    /// Persisted and printed in JSON: stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            SignalKind::Incomplete => "incomplete",
            SignalKind::Error => "error",
            SignalKind::New => "new",
            SignalKind::Disappeared => "disappeared",
            SignalKind::Frequency => "frequency",
            SignalKind::Latency => "latency",
        }
    }
}

impl fmt::Display for SignalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSignalKind(pub String);

impl fmt::Display for UnknownSignalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown signal kind {:?}", self.0)
    }
}

impl std::error::Error for UnknownSignalKind {}

impl FromStr for SignalKind {
    type Err = UnknownSignalKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SignalKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| UnknownSignalKind(s.to_owned()))
    }
}

/// What a signal measured. Persisted and printed in JSON: stable.
pub mod measure {
    /// Occurrences in the run.
    pub const COUNT: &str = "count";
    /// Queries: a request's summed `queries` measure, or the SQL attributed to an example.
    pub const QUERIES: &str = "queries";
    /// An example's duration, in milliseconds.
    pub const DURATION_MS: &str = "duration_ms";
    /// Failed occurrences of an example.
    pub const FAILED: &str = "failed";
}

/// NEW, DISAPPEARED, FREQUENCY and LATENCY need this many baseline runs; ERROR needs one.
pub const MIN_BASELINE_RUNS: u32 = rules::MIN_RUNS as u32;

/// The baseline numbers a rule used, rounded to the precision they have.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BaselineNumbers {
    /// Baseline runs the rule compared against.
    pub runs: u32,
    /// Of those, runs in which the behavior occurred.
    pub present_in: u32,
    /// The measure across baseline runs that had it; `None` when no run had it.
    pub median: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// ERROR: baseline runs in which the example failed.
    pub failures: Option<u32>,
}

/// Where a signal's change happened: inside one example, or in one phase outside them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attribution {
    pub scope: Phase,
    /// The measure within that scope, now.
    pub current: f64,
    /// The measure within that scope, median across baseline runs.
    pub baseline: f64,
}

/// Every number here is the evidence for the claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub kind: SignalKind,
    pub behavior: BehaviorId,
    /// One of [`measure`]'s names.
    pub measure: String,
    /// The measure in this run.
    pub current: f64,
    pub baseline: BaselineNumbers,
    /// ERROR: the exception class(es) the example failed with now, when known.
    pub exception: Option<String>,
    pub attribution: Option<Attribution>,
    /// In [0, 1): the probability this isn't noise, two significant figures.
    pub confidence: f64,
    /// How directly the signal names a developer-visible change, 1 (an error) to 5 (setup); lower ranks first.
    pub tier: u8,
    /// 1-based rank of the group this signal belongs to.
    pub group: u32,
    /// The member that speaks for the group; the others are supporting evidence.
    pub headline: bool,
}

impl Signal {
    /// Changed outside every example (setup, between examples, teardown): the environment or suite hooks,
    /// e.g. a cold DB's schema load, not the code under test.
    pub fn outside_examples(&self) -> bool {
        self.attribution.is_some_and(|a| a.scope.outside_examples())
    }
}

/// The tier of changes outside every example.
pub const SETUP_TIER: u8 = 5;

/// Signals ranked by group, each group's headline first.
pub fn detect<K>(current: &RunStats, baseline: &Baseline<'_, K>) -> Vec<Signal> {
    let runs: Vec<&RunStats> = baseline.iter().collect();
    if runs.is_empty() {
        return Vec::new();
    }
    let comparison = Comparison::new(current, &runs);
    let ids: BTreeSet<BehaviorId> = std::iter::once(current)
        .chain(runs.iter().copied())
        .flat_map(|run| run.iter().map(|b| b.behavior.id))
        .collect();
    let mut found = Vec::new();
    for id in ids {
        comparison.judge(id, &mut found);
    }
    match baseline.incomplete() {
        None => comparison.latency(&mut found),
        Some(why) => {
            found.retain(|f| !comparison.partial_run_explains(&f.signal));
            comparison.incomplete(why, &mut found);
        }
    }
    if baseline.partial() {
        // Every baseline run skipped existing examples: one that none of them ran was skipped, not written since.
        let unrun = |example: BehaviorId| runs.iter().all(|run| run.count(example) == 0);
        found.retain(|f| {
            f.signal.kind != SignalKind::New
                || !comparison.within_examples(f.signal.behavior, unrun)
        });
    }
    comparison.collapse(&mut found);
    rank(found)
}

/// The measure `name` of `behavior` in `run`, as the rules read it; `None` when the run lacks it.
pub fn value(run: &RunStats, behavior: BehaviorId, name: &str) -> Option<f64> {
    let b = run.get(behavior)?;
    match name {
        measure::COUNT => Some(b.stats.count as f64),
        measure::FAILED => Some(b.stats.errors as f64),
        measure::DURATION_MS => duration_ms(b),
        measure::QUERIES if b.behavior.kind == Kind::TestExample => {
            Some(example_queries(run).get(&behavior).copied().unwrap_or(0.0))
        }
        _ => b.measure(name).map(|m| m.sum),
    }
}

/// The measure `name` of `behavior` within `scope` of `run`; zero when absent.
pub fn scoped_value(run: &RunStats, behavior: BehaviorId, scope: Phase, name: &str) -> f64 {
    let Some(b) = run.get(behavior) else {
        return 0.0;
    };
    let sum = |s: &crate::aggregate::ScopeStats| {
        if name == measure::COUNT {
            s.count as f64
        } else {
            s.sums
                .iter()
                .find(|(n, _)| n == name)
                .map_or(0.0, |(_, sum)| *sum)
        }
    };
    match scope.scope_id() {
        Some(id) => b.scopes.iter().find(|s| s.scope == id).map_or(0.0, sum),
        None if name == measure::COUNT => b.unscoped_count() as f64,
        None => {
            let total = b.measure(name).map_or(0.0, |m| m.sum);
            total - b.scopes.iter().map(sum).sum::<f64>()
        }
    }
}

fn duration_ms(b: &BehaviorStats) -> Option<f64> {
    let d = b.stats.duration?;
    (d.count > 0).then(|| d.total.as_secs_f64() * 1e3 / d.count as f64)
}

/// SQL occurrences attributed to each example in `run`, counted as Rails counts a request's queries.
fn example_queries(run: &RunStats) -> HashMap<BehaviorId, f64> {
    let mut totals = HashMap::new();
    let counted = |b: &&BehaviorStats| {
        b.behavior.kind == Kind::DbQuery
            && !["TRANSACTION ", "SCHEMA "]
                .iter()
                .any(|p| b.behavior.template.starts_with(p))
    };
    for b in run.iter().filter(counted) {
        for s in &b.scopes {
            *totals.entry(s.scope).or_insert(0.0) += s.count as f64;
        }
    }
    totals
}

/// Which rules a behavior gets, and at what tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Example,
    Request,
    Sql,
    Stderr,
    /// Other lines: stdout, or a side channel no interpreter recognized.
    Output,
    /// An error the test reporter saw outside every example: a spec file that failed to load, a hook that raised.
    OutsideExamples,
}

fn tier(kind: SignalKind, class: Class) -> u8 {
    use Class::*;
    use SignalKind::*;
    match (kind, class) {
        (Error | Incomplete, _) | (New, OutsideExamples) => 1,
        (Frequency, Request) | (New, Stderr) | (Latency, Example) => 2,
        (Frequency, _) => 3,
        _ => 4,
    }
}

/// Signals sharing a key form one group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    /// The run's own incompleteness.
    Incomplete,
    /// An example, or one phase outside them.
    Scope(Phase),
    /// Examples of one spec file that disappeared together, with what was attributed to them.
    File(String),
    /// Stderr has no per-example attribution: the same message from different call sites groups by prefix.
    Prefix(String),
    Behavior(BehaviorId),
}

/// How many characters of a stderr template decide its group.
const PREFIX_CHARS: usize = 60;

/// DISAPPEARED examples of one spec file group together from this many; a single one stays its own change.
const COLLAPSE_EXAMPLES: usize = 2;

/// The spec file of a `test.example` template, `<spec file> # <full description>`.
fn spec_file(template: &str) -> Option<&str> {
    template.split_once(" # ").map(|(file, _)| file)
}

/// A group made of examples of one spec file that disappeared together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisappearedExamples<'a> {
    pub file: &'a str,
    /// Members that are those examples; any others were attributed to them.
    pub examples: usize,
}

/// Whether a group's members, headline first and each with its behavior, are examples of one spec file that
/// disappeared together, as [`detect`] groups them.
pub fn disappeared_examples<'a>(
    members: impl IntoIterator<Item = (&'a Signal, &'a Behavior)>,
) -> Option<DisappearedExamples<'a>> {
    let gone = |(s, b): &(&Signal, &Behavior)| {
        s.kind == SignalKind::Disappeared && b.kind == Kind::TestExample
    };
    let mut members = members.into_iter();
    let (_, head) = members.next().filter(gone)?;
    let file = spec_file(&head.template)?;
    let mut examples = 1;
    for (_, behavior) in members.filter(gone) {
        if spec_file(&behavior.template) != Some(file) {
            return None;
        }
        examples += 1;
    }
    (examples >= COLLAPSE_EXAMPLES).then_some(DisappearedExamples { file, examples })
}

struct Comparison<'a> {
    current: &'a RunStats,
    runs: &'a [&'a RunStats],
    /// A test reporter ran: stdout is its rendering of results already known from its events.
    reporter: bool,
    /// Per run, current first: SQL attributed to each example, when every query was attributed.
    queries: Vec<Option<HashMap<BehaviorId, f64>>>,
}

struct Found {
    signal: Signal,
    key: Key,
    /// A DISAPPEARED example: what else is attributed to it moved because it went, so it heads its group.
    gone: bool,
}

impl<'a> Comparison<'a> {
    fn new(current: &'a RunStats, runs: &'a [&'a RunStats]) -> Self {
        let queries = std::iter::once(current)
            .chain(runs.iter().copied())
            .map(|run| {
                let complete = run
                    .iter()
                    .all(|b| b.behavior.kind != Kind::DbQuery || b.unattributed == 0);
                complete.then(|| example_queries(run))
            })
            .collect();
        Comparison {
            current,
            runs,
            reporter: current.iter().any(|b| b.behavior.kind == Kind::TestExample),
            queries,
        }
    }

    fn n(&self) -> usize {
        self.runs.len()
    }

    fn template(&self, id: BehaviorId) -> &str {
        self.current
            .get(id)
            .or_else(|| self.runs.iter().find_map(|run| run.get(id)))
            .map_or("", |b| b.behavior.template.as_str())
    }

    /// DISAPPEARED examples of one spec file, and what is attributed to them, become one group: deleting or renaming
    /// a file is one change, not one per example.
    fn collapse(&self, found: &mut [Found]) {
        let mut files: BTreeMap<&str, Vec<BehaviorId>> = BTreeMap::new();
        for f in found.iter().filter(|f| f.gone) {
            if let Some(file) = spec_file(self.template(f.signal.behavior)) {
                files.entry(file).or_default().push(f.signal.behavior);
            }
        }
        let file_of: HashMap<BehaviorId, &str> = files
            .into_iter()
            .filter(|(_, examples)| examples.len() >= COLLAPSE_EXAMPLES)
            .flat_map(|(file, examples)| examples.into_iter().map(move |e| (e, file)))
            .collect();
        for f in found {
            if let Key::Scope(Phase::Example(example)) = f.key
                && let Some(file) = file_of.get(&example)
            {
                f.key = Key::File((*file).to_owned());
            }
        }
    }

    fn class(&self, id: BehaviorId) -> Option<Class> {
        let b = self
            .current
            .get(id)
            .or_else(|| self.runs.iter().find_map(|run| run.get(id)))?;
        match b.behavior.kind {
            Kind::TestExample => Some(Class::Example),
            Kind::HttpRequest => Some(Class::Request),
            Kind::DbQuery => Some(Class::Sql),
            Kind::TestSummary => None,
            // Evidence, never a signal. CPU and peak RSS vary run to run (§1: a suite's wall time spans 10x),
            // so judging them would raise a FREQUENCY on nearly every run. `judge` returns on None, so this
            // arm is the whole gate: no presence, frequency, error or latency rule ever sees these measures.
            Kind::Resources => None,
            Kind::Log | Kind::Exception if id == overflow_behavior().id => None,
            // Seen on the reporter's own event stream, so the rule below would drop it; RSpec exits 1 on it.
            Kind::Exception if error_outside_examples(b) => Some(Class::OutsideExamples),
            Kind::Log | Kind::Exception => match &b.first {
                Some((Stream::Stderr, _)) => Some(Class::Stderr),
                _ if self.reporter => None,
                _ => Some(Class::Output),
            },
        }
    }

    fn judge(&self, id: BehaviorId, found: &mut Vec<Found>) {
        let Some(class) = self.class(id) else {
            return;
        };
        let now = self.current.get(id).filter(|b| b.stats.count > 0);
        let present: Vec<&BehaviorStats> = self
            .runs
            .iter()
            .filter_map(|run| run.get(id))
            .filter(|b| b.stats.count > 0)
            .collect();
        let n = self.n();
        if let Some((kind, confidence)) = rules::presence(now.is_some(), present.len(), n) {
            let counts: Vec<f64> = present.iter().map(|b| b.stats.count as f64).collect();
            let (kind, current) = match kind {
                Presence::New => (SignalKind::New, now.map_or(0.0, |b| b.stats.count as f64)),
                Presence::Disappeared => (SignalKind::Disappeared, 0.0),
            };
            let baseline = BaselineNumbers {
                runs: n as u32,
                present_in: present.len() as u32,
                median: median(&counts),
                min: counts.iter().copied().reduce(f64::min),
                max: counts.iter().copied().reduce(f64::max),
                failures: None,
            };
            self.push(
                found,
                id,
                class,
                kind,
                measure::COUNT,
                current,
                baseline,
                confidence,
            );
        }
        let Some(now) = now else {
            return;
        };
        if present.len() < n {
            // FREQUENCY and ERROR below need the behavior in every baseline run, except ERROR's n ≥ 1 below.
            if class == Class::Example {
                self.error(id, now, found);
            }
            return;
        }
        match class {
            Class::Example => {
                self.error(id, now, found);
                self.example_queries(id, found);
            }
            Class::Request => {
                self.frequency(id, class, measure::QUERIES, found);
                self.frequency(id, class, measure::COUNT, found);
            }
            Class::Sql | Class::Stderr | Class::Output | Class::OutsideExamples => {
                self.frequency(id, class, measure::COUNT, found);
            }
        }
    }

    /// FREQUENCY of a measure every baseline run has.
    fn frequency(&self, id: BehaviorId, class: Class, name: &str, found: &mut Vec<Found>) {
        let Some(current) = value(self.current, id, name) else {
            return;
        };
        let Some(baseline) = self
            .runs
            .iter()
            .map(|run| value(run, id, name))
            .collect::<Option<Vec<f64>>>()
        else {
            return;
        };
        self.push_frequency(id, class, name, current, &baseline, found);
    }

    fn example_queries(&self, id: BehaviorId, found: &mut Vec<Found>) {
        let Some(per_run) = self
            .queries
            .iter()
            .map(Option::as_ref)
            .collect::<Option<Vec<&HashMap<BehaviorId, f64>>>>()
        else {
            return;
        };
        let at = |run: &HashMap<BehaviorId, f64>| run.get(&id).copied().unwrap_or(0.0);
        let baseline: Vec<f64> = per_run[1..].iter().map(|run| at(run)).collect();
        self.push_frequency(
            id,
            Class::Example,
            measure::QUERIES,
            at(per_run[0]),
            &baseline,
            found,
        );
    }

    fn push_frequency(
        &self,
        id: BehaviorId,
        class: Class,
        name: &str,
        current: f64,
        baseline: &[f64],
        found: &mut Vec<Found>,
    ) {
        let Some(f) = rules::frequency(baseline, current) else {
            return;
        };
        let numbers = BaselineNumbers {
            runs: baseline.len() as u32,
            present_in: baseline.len() as u32,
            median: Some(f.median),
            min: Some(f.min),
            max: Some(f.max),
            failures: None,
        };
        self.push(
            found,
            id,
            class,
            SignalKind::Frequency,
            name,
            current,
            numbers,
            f.confidence,
        );
    }

    /// ERROR against the baseline runs that ran the example, unless one failed the same way.
    fn error(&self, id: BehaviorId, now: &BehaviorStats, found: &mut Vec<Found>) {
        if now.stats.errors == 0 {
            return;
        }
        let ran: Vec<(&RunStats, &BehaviorStats)> = self
            .runs
            .iter()
            .filter_map(|run| run.get(id).map(|b| (*run, b)))
            .collect();
        let exceptions_now = exceptions(self.current, id);
        let failed: Vec<&RunStats> = ran
            .iter()
            .filter(|(_, b)| b.stats.errors > 0)
            .map(|(run, _)| *run)
            .collect();
        let known_flaky = failed.iter().any(|run| {
            let then = exceptions(run, id);
            then.keys().any(|e| exceptions_now.contains_key(e))
                || (then.is_empty() && exceptions_now.is_empty())
        });
        let Some(confidence) = rules::error(failed.len(), ran.len(), known_flaky) else {
            return;
        };
        let baseline = BaselineNumbers {
            runs: ran.len() as u32,
            present_in: ran.len() as u32,
            failures: Some(failed.len() as u32),
            ..BaselineNumbers::default()
        };
        let before = found.len();
        self.push(
            found,
            id,
            Class::Example,
            SignalKind::Error,
            measure::FAILED,
            now.stats.errors as f64,
            baseline,
            confidence,
        );
        if let Some(pushed) = found.get_mut(before)
            && !exceptions_now.is_empty()
        {
            pushed.signal.exception =
                Some(exceptions_now.into_values().collect::<Vec<_>>().join(", "));
        }
    }

    /// LATENCY per example, in execution order so a neighbour's slowdown can veto a machine stall.
    fn latency(&self, found: &mut Vec<Found>) {
        let mut examples: Vec<&BehaviorStats> = self
            .current
            .iter()
            .filter(|b| b.behavior.kind == Kind::TestExample)
            .collect();
        examples.sort_by_key(|b| (b.first.as_ref().map(|(_, seq)| *seq), b.behavior.id));
        let history = |id: BehaviorId| -> Vec<f64> {
            self.runs
                .iter()
                .filter_map(|run| run.get(id).and_then(duration_ms))
                .collect()
        };
        let excess: Vec<Option<f64>> = examples
            .iter()
            .map(|b| Some(duration_ms(b)? - median(&history(b.behavior.id))?))
            .collect();
        // The reporter's suite duration when it has one, else the examples' own time.
        let suite_ms = |run: &RunStats| -> f64 {
            let summed = |kind| {
                run.iter()
                    .filter(|b| b.behavior.kind == kind)
                    .filter_map(|b| b.stats.duration)
                    .map(|d| d.total.as_secs_f64() * 1e3)
                    .sum::<f64>()
            };
            Some(summed(Kind::TestSummary))
                .filter(|&ms| ms > 0.0)
                .unwrap_or_else(|| summed(Kind::TestExample))
        };
        let suite_baseline: Vec<f64> = self
            .runs
            .iter()
            .map(|run| suite_ms(run))
            .filter(|&ms| ms > 0.0)
            .collect();
        let suite = rules::Suite {
            baseline: &suite_baseline,
            current: suite_ms(self.current),
        };
        for (i, b) in examples.iter().enumerate() {
            let Some(current) = duration_ms(b) else {
                continue;
            };
            let baseline = history(b.behavior.id);
            let neighbours = [i.checked_sub(1), Some(i + 1)]
                .into_iter()
                .flatten()
                .filter_map(|j| excess.get(j).copied().flatten())
                .reduce(f64::max);
            let Some(l) = rules::latency(&baseline, current, neighbours, Some(suite)) else {
                continue;
            };
            let ms = |v: f64| round_sig(v, 3);
            let numbers = BaselineNumbers {
                runs: baseline.len() as u32,
                present_in: baseline.len() as u32,
                median: Some(ms(l.median)),
                min: baseline.iter().copied().reduce(f64::min).map(ms),
                max: baseline.iter().copied().reduce(f64::max).map(ms),
                failures: None,
            };
            self.push(
                found,
                b.behavior.id,
                Class::Example,
                SignalKind::Latency,
                measure::DURATION_MS,
                ms(current),
                numbers,
                l.confidence,
            );
        }
    }

    /// Whether a partial run could account for `signal` by what it didn't run. NEW and ERROR stand: a partial
    /// run adds nothing, and an example that ran failed. LATENCY never stands, since a partial suite's
    /// duration can't veto a machine stall.
    fn partial_run_explains(&self, signal: &Signal) -> bool {
        match signal.kind {
            SignalKind::Incomplete | SignalKind::Error | SignalKind::New => false,
            SignalKind::Latency => true,
            SignalKind::Disappeared | SignalKind::Frequency => {
                !self.within_examples(signal.behavior, |example| self.current.count(example) > 0)
            }
        }
    }

    /// Every occurrence of `id`, now and in the baseline, is an example for which `ran` holds, or lies inside one.
    fn within_examples(&self, id: BehaviorId, ran: impl Fn(BehaviorId) -> bool) -> bool {
        std::iter::once(self.current)
            .chain(self.runs.iter().copied())
            .filter_map(|run| run.get(id))
            .all(|b| match b.behavior.kind {
                Kind::TestExample => ran(id),
                _ => {
                    b.unattributed == 0
                        && b.unscoped_count() == 0
                        && b.scopes.iter().all(|s| {
                            matches!(Phase::from_scope_id(Some(s.scope)), Phase::Example(e) if ran(e))
                        })
                }
            })
    }

    /// INCOMPLETE, on the behaviors that show it: each new error outside examples, else the test summary.
    fn incomplete(&self, why: Ineligible, found: &mut Vec<Found>) {
        let Some(confidence) = rules::incomplete(self.n()) else {
            return;
        };
        let numbers = |values: &[Option<f64>]| {
            let present: Vec<f64> = values.iter().flatten().copied().collect();
            BaselineNumbers {
                runs: values.len() as u32,
                present_in: present.len() as u32,
                median: median(&present),
                min: present.iter().copied().reduce(f64::min),
                max: present.iter().copied().reduce(f64::max),
                failures: None,
            }
        };
        let summary_of = |run: &RunStats| {
            run.iter()
                .find(|b| b.behavior.kind == Kind::TestSummary)
                .map(|b| b.behavior.id)
        };
        let mut evidence: Vec<(BehaviorId, &'static str, f64, BaselineNumbers)> = Vec::new();
        let on_summary = |name: &'static str| {
            let Some(id) = summary_of(self.current)
                .or_else(|| self.runs.iter().find_map(|run| summary_of(run)))
            else {
                return Vec::new();
            };
            let then: Vec<Option<f64>> = self.runs.iter().map(|run| value(run, id, name)).collect();
            let now = value(self.current, id, name).unwrap_or(0.0);
            vec![(id, name, now, numbers(&then))]
        };
        match why {
            Ineligible::NoTestSummary => evidence = on_summary(measure::COUNT),
            Ineligible::Stopped { .. } | Ineligible::Subset { .. } => {
                evidence = on_summary(summary::EXAMPLES);
            }
            Ineligible::ErrorsOutsideExamples { .. } => {
                for b in self.current.iter().filter(|b| error_outside_examples(b)) {
                    let then: Vec<Option<f64>> = self
                        .runs
                        .iter()
                        .map(|run| Some(run.count(b.behavior.id) as f64))
                        .collect();
                    let most = then.iter().flatten().copied().fold(0.0, f64::max);
                    let now = b.stats.count as f64;
                    if now > most {
                        evidence.push((b.behavior.id, measure::COUNT, now, numbers(&then)));
                    }
                }
                // A listener that predates error events: the summary's count is the evidence.
                if evidence.is_empty() {
                    evidence = on_summary(summary::ERRORS_OUTSIDE_OF_EXAMPLES);
                }
            }
        }
        // The error that left the run incomplete is named once, as that.
        found.retain(|f| !evidence.iter().any(|&(id, ..)| id == f.signal.behavior));
        for (behavior, name, current, baseline) in evidence {
            found.push(Found {
                signal: Signal {
                    kind: SignalKind::Incomplete,
                    behavior,
                    measure: name.to_owned(),
                    current,
                    baseline,
                    exception: None,
                    attribution: None,
                    confidence,
                    tier: tier(SignalKind::Incomplete, Class::Output),
                    group: 0,
                    headline: false,
                },
                key: Key::Incomplete,
                gone: false,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        found: &mut Vec<Found>,
        id: BehaviorId,
        class: Class,
        kind: SignalKind,
        name: &str,
        current: f64,
        baseline: BaselineNumbers,
        confidence: f64,
    ) {
        let mut tier = tier(kind, class);
        let (key, attribution) = match class {
            Class::Example => (Key::Scope(Phase::Example(id)), None),
            _ => {
                let owners = self.owners(id, class, name);
                match owners.as_slice() {
                    [owner] => {
                        let baseline_values: Vec<f64> = self
                            .runs
                            .iter()
                            .map(|run| scoped_value(run, id, *owner, name))
                            .collect();
                        let attribution = Attribution {
                            scope: *owner,
                            current: scoped_value(self.current, id, *owner, name),
                            baseline: median(&baseline_values).unwrap_or(0.0),
                        };
                        if owner.outside_examples() {
                            tier = SETUP_TIER;
                        }
                        (Key::Scope(*owner), Some(attribution))
                    }
                    _ if class == Class::Stderr => (
                        Key::Prefix(self.template(id).chars().take(PREFIX_CHARS).collect()),
                        None,
                    ),
                    _ => (Key::Behavior(id), None),
                }
            }
        };
        found.push(Found {
            gone: class == Class::Example && kind == SignalKind::Disappeared,
            signal: Signal {
                kind,
                behavior: id,
                measure: name.to_owned(),
                current,
                baseline,
                exception: None,
                attribution,
                confidence,
                tier,
                group: 0,
                headline: false,
            },
            key,
        });
    }

    /// The scopes whose own measure moved against the baseline median.
    /// Unknown (empty) when any run couldn't attribute every occurrence.
    fn owners(&self, id: BehaviorId, class: Class, name: &str) -> Vec<Phase> {
        let all = || std::iter::once(self.current).chain(self.runs.iter().copied());
        if all().any(|run| run.get(id).is_some_and(|b| b.unattributed > 0)) {
            return Vec::new();
        }
        let mut scopes: BTreeSet<Phase> = all()
            .filter_map(|run| run.get(id))
            .flat_map(|b| b.scopes.iter().map(|s| Phase::from_scope_id(Some(s.scope))))
            .collect();
        // Only a test run has a setup phase, and only side-channel SQL and requests are attributed to examples.
        if self.reporter && matches!(class, Class::Sql | Class::Request) {
            scopes.insert(Phase::Setup);
        }
        scopes
            .into_iter()
            .filter(|&scope| {
                let now = scoped_value(self.current, id, scope, name);
                let then: Vec<f64> = self
                    .runs
                    .iter()
                    .map(|run| scoped_value(run, id, scope, name))
                    .collect();
                median(&then).is_some_and(|m| m != now)
            })
            .collect()
    }
}

/// An error the RSpec listener reported outside every example, such as a spec file that failed to load.
fn error_outside_examples(b: &BehaviorStats) -> bool {
    b.behavior.kind == Kind::Exception
        && b.scopes.is_empty()
        && matches!(&b.first, Some((Stream::File(name), _)) if &**name == EVENTS_STREAM)
}

/// Exception behaviors attributed to `example` in `run`, by id, with their templates.
fn exceptions(run: &RunStats, example: BehaviorId) -> BTreeMap<BehaviorId, String> {
    run.iter()
        .filter(|b| b.behavior.kind == Kind::Exception)
        .filter(|b| b.scopes.iter().any(|s| s.scope == example && s.count > 0))
        .map(|b| (b.behavior.id, b.behavior.template.clone()))
        .collect()
}

/// Lower tier first, then higher confidence; the rest only makes the order deterministic.
fn precedence(a: &Signal, b: &Signal) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then(b.confidence.total_cmp(&a.confidence))
        .then(a.kind.cmp(&b.kind))
        .then(a.behavior.cmp(&b.behavior))
        .then(a.measure.cmp(&b.measure))
}

fn rank(found: Vec<Found>) -> Vec<Signal> {
    let mut groups: BTreeMap<Key, Vec<(bool, Signal)>> = BTreeMap::new();
    for Found { signal, key, gone } in found {
        groups.entry(key).or_default().push((gone, signal));
    }
    let mut groups: Vec<Vec<(bool, Signal)>> = groups.into_values().collect();
    for members in &mut groups {
        members
            .sort_by(|(a_gone, a), (b_gone, b)| b_gone.cmp(a_gone).then_with(|| precedence(a, b)));
    }
    groups.sort_by(|a, b| precedence(&a[0].1, &b[0].1));
    groups
        .into_iter()
        .zip(1..)
        .flat_map(|(members, rank)| {
            members
                .into_iter()
                .enumerate()
                .map(move |(i, (_, mut signal))| {
                    signal.group = rank;
                    signal.headline = i == 0;
                    signal
                })
        })
        .collect()
}

#[cfg(test)]
mod tests;
