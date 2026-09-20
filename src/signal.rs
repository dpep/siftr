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

use crate::aggregate::{BehaviorStats, Phase, RunStats, capped, overflow_behavior};
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
    /// Events of behaviors past the per-run cap: what a truncated run counted but couldn't tell apart.
    pub const PAST_CAP: &str = "events_past_cap";
}

/// NEW, DISAPPEARED, FREQUENCY and LATENCY need this many baseline runs; ERROR needs one.
pub const MIN_BASELINE_RUNS: u32 = rules::MIN_RUNS as u32;

/// Past this many signals, a run records none of them: that many says its behaviors don't recur, not that
/// this much changed. Signals, not the changes they group into — the two differ by design, and it is signals
/// that were measured: over 27 runs of the RSpec captures in `fixtures/` the most any one run produced was
/// 11, and one window of the macOS unified log produced 8,564, with nothing in between. A backstop, not a
/// tuned threshold: it clears a per-example change to a suite far larger than any here (the 934-example
/// suite of `docs/findings/signals.md`) by an order of magnitude.
pub const MAX_SIGNALS: usize = 1_000;

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

    /// Demoted because its count moved only as fast as the suite did: per-example bookkeeping, not a
    /// change in the code. Tier 5 is reached by exactly two paths, and the other one attributes to a
    /// phase outside every example, so the pair tells them apart.
    pub fn tracks_suite_size(&self) -> bool {
        self.tier == SUITE_SIZE_TIER && !self.outside_examples()
    }
}

/// The tier of changes outside every example.
pub const SETUP_TIER: u8 = 5;

/// The tier of a count that only tracks the suite's size. Shares [`SETUP_TIER`]'s rank: both are real
/// changes that aren't about the code under test.
pub const SUITE_SIZE_TIER: u8 = SETUP_TIER;

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
    comparison.truncated(&mut found);
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
        (Frequency, Request) | (New, Stderr) | (Latency, Example | Request) => 2,
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
    /// Examples of one spec file that appeared, or disappeared, together, with what was attributed to them.
    /// The kind is part of the key: renaming examples within a file moves some each way, and those are two
    /// changes, not one.
    File(SignalKind, String),
    /// Stderr has no per-example attribution: the same message from different call sites groups by prefix.
    Prefix(String),
    Behavior(BehaviorId),
}

/// How many characters of a stderr template decide its group.
const PREFIX_CHARS: usize = 60;

/// NEW or DISAPPEARED examples of one spec file group together from this many; a single one stays its own change.
const COLLAPSE_EXAMPLES: usize = 2;

/// The spec file of a `test.example` template, `<spec file> # <full description>`.
fn spec_file(template: &str) -> Option<&str> {
    template.split_once(" # ").map(|(file, _)| file)
}

/// The presence changes: the behavior arrived, or stopped occurring. Only these collapse by spec file, because
/// only these are what adding or deleting the file did — a count that moved is about the code, not the file.
fn presence(kind: SignalKind) -> bool {
    matches!(kind, SignalKind::New | SignalKind::Disappeared)
}

/// A group made of examples of one spec file that appeared, or disappeared, together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollapsedExamples<'a> {
    /// NEW or DISAPPEARED: which way the file's examples moved.
    pub kind: SignalKind,
    pub file: &'a str,
    /// Members that are those examples; any others were attributed to them.
    pub examples: usize,
}

/// Whether a group's members, headline first and each with its behavior, are examples of one spec file that
/// appeared or disappeared together, as [`detect`] groups them.
pub fn collapsed_examples<'a>(
    members: impl IntoIterator<Item = (&'a Signal, &'a Behavior)>,
) -> Option<CollapsedExamples<'a>> {
    let collapsible =
        |(s, b): &(&Signal, &Behavior)| presence(s.kind) && b.kind == Kind::TestExample;
    let mut members = members.into_iter();
    let (head, behavior) = members.next().filter(collapsible)?;
    let kind = head.kind;
    let file = spec_file(&behavior.template)?;
    let mut examples = 1;
    for (signal, behavior) in members.filter(collapsible) {
        if signal.kind != kind || spec_file(&behavior.template) != Some(file) {
            return None;
        }
        examples += 1;
    }
    (examples >= COLLAPSE_EXAMPLES).then_some(CollapsedExamples {
        kind,
        file,
        examples,
    })
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
    /// An example that appeared or disappeared: whatever is attributed to it moved with it, so it heads its group.
    moved: bool,
}

impl<'a> Comparison<'a> {
    fn new(current: &'a RunStats, runs: &'a [&'a RunStats]) -> Self {
        let queries = std::iter::once(current)
            .chain(runs.iter().copied())
            .map(|run| {
                // A truncated run's total is short by whatever query behaviors the cap cut, and nothing says
                // which, so it can't be compared either.
                let complete = run.events_past_cap() == 0
                    && run
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

    /// The phase a scope id names in these runs. Only the behavior it names tells an example from a
    /// request, and a request's endpoint names none — see [`Phase::of_scope`].
    fn phase(&self, scope: BehaviorId) -> Phase {
        let kind = std::iter::once(self.current)
            .chain(self.runs.iter().copied())
            .find_map(|run| run.get(scope))
            .map(|b| b.behavior.kind);
        Phase::of_scope(Some(scope), kind)
    }

    /// Examples of one spec file that appeared or disappeared together, and what is attributed to them, become
    /// one group: adding, deleting or renaming a file is one change, not one per example. Grouped on that shared
    /// identity alone and never on how the counts moved — `docs/findings/grouping.md` measured the count-vector
    /// rule at a 15% false-merge rate, and 15 of its 21 false merges are pairs this rule makes correctly.
    fn collapse(&self, found: &mut [Found]) {
        let mut files: BTreeMap<(SignalKind, &str), Vec<BehaviorId>> = BTreeMap::new();
        for f in found.iter().filter(|f| f.moved) {
            if let Some(file) = spec_file(self.template(f.signal.behavior)) {
                files
                    .entry((f.signal.kind, file))
                    .or_default()
                    .push(f.signal.behavior);
            }
        }
        let file_of: HashMap<BehaviorId, (SignalKind, &str)> = files
            .into_iter()
            .filter(|(_, examples)| examples.len() >= COLLAPSE_EXAMPLES)
            .flat_map(|(key, examples)| examples.into_iter().map(move |e| (e, key)))
            .collect();
        for f in found {
            if let Key::Scope(Phase::Example(example)) = f.key
                && let Some(&(kind, file)) = file_of.get(&example)
            {
                f.key = Key::File(kind, file.to_owned());
            }
        }
    }

    /// Whether the compared runs disagreeing about a source accounts for `id` being present now and absent
    /// then, or the reverse. A behavior is attributed to the stream it was first seen on, the only source
    /// evidence an aggregate carries; a run that didn't record what it read answers `None` and explains
    /// nothing. Deliberately narrow: it fires only when the difference accounts for the whole absence.
    fn configuration_explains(&self, id: BehaviorId, kind: SignalKind) -> bool {
        let streams = || {
            std::iter::once(self.current)
                .chain(self.runs.iter().copied())
                .filter_map(|run| run.get(id))
                .filter_map(|b| b.first.as_ref().map(|(stream, _)| stream))
        };
        match kind {
            // Absent now because this run never read the stream it comes from.
            SignalKind::Disappeared => streams().any(|s| self.current.read(s) == Some(false)),
            // Absent from the baseline because not one of its runs read that stream.
            SignalKind::New => {
                streams().any(|s| self.runs.iter().all(|run| run.read(s) == Some(false)))
            }
            _ => false,
        }
    }

    /// Whether the per-run behavior cap accounts for `id` being absent from one side. Admission goes by the
    /// arrival order of a behavior's first occurrence, so a truncated run's absences are not evidence: two
    /// runs of the very same lines in a different order disagree about which behaviors exist, and that
    /// disagreement reads as NEW and DISAPPEARED. Deliberately narrow, like [`Self::configuration_explains`]:
    /// an admitted behavior's counts are exact, so only presence is in doubt, and only for a [`capped`] kind.
    fn truncation_explains(&self, id: BehaviorId, kind: SignalKind) -> bool {
        let Some(b) = self
            .current
            .get(id)
            .or_else(|| self.runs.iter().find_map(|run| run.get(id)))
        else {
            return false;
        };
        if !capped(b.behavior.kind) {
            return false;
        }
        match kind {
            // Absent now, from a run that couldn't keep every behavior it saw.
            SignalKind::Disappeared => self.current.events_past_cap() > 0,
            // Absent from the baseline, where not one run could keep every behavior it saw.
            SignalKind::New => self.runs.iter().all(|run| run.events_past_cap() > 0),
            _ => false,
        }
    }

    /// Whether `id`'s count moved only because the suite changed size. What rspec's transactional
    /// fixtures run once per example tracks the example count, so its count restates what the NEW and
    /// DISAPPEARED examples already say: on a suite that grew 1 → 7, `ROLLBACK TRANSACTION` went 1 → 7
    /// and headlined both reporting runs (`docs/findings/dogfood-junior-loop.md`).
    ///
    /// No threshold of its own. It asks [`rules::frequency`] — the rule that raised the signal — whether
    /// the count *per example* moved, and that rule is invariant under scaling every run's value by the
    /// same factor. So a suite holding its size is judged exactly as before, which is why this is gated
    /// on the size having moved rather than left to the arithmetic.
    fn suite_size_explains(&self, id: BehaviorId, name: &str) -> bool {
        if name != measure::COUNT {
            return false;
        }
        let examples = |run: &RunStats| {
            run.iter()
                .filter(|b| b.behavior.kind == Kind::TestExample && b.stats.count > 0)
                .count() as f64
        };
        let now = examples(self.current);
        let then: Vec<f64> = self.runs.iter().map(|run| examples(run)).collect();
        // A run with no examples has no rate, and a suite that held its size explains nothing.
        if now == 0.0 || then.contains(&0.0) || then.iter().all(|&n| n == now) {
            return false;
        }
        // Concentrated in one example is a change someone can go and look at; this is about diffuse ones.
        let in_examples = self.current.get(id).map_or(0, |b| {
            b.scopes
                .iter()
                .filter(|s| s.count > 0 && !self.phase(s.scope).outside_examples())
                .count()
        });
        if in_examples < 2 {
            return false;
        }
        let rate = |run: &RunStats, examples: f64| value(run, id, name).unwrap_or(0.0) / examples;
        let baseline: Vec<f64> = self
            .runs
            .iter()
            .zip(&then)
            .map(|(run, &n)| rate(run, n))
            .collect();
        rules::frequency(&baseline, rate(self.current, now)).is_none()
    }

    /// INCOMPLETE when the cap cut behaviors out of this run: it didn't record what its baseline runs did, so
    /// its absences went unjudged. Said once, on the overflow behavior that counts what was cut — otherwise
    /// the run reports "0 changes", which here would mean "I didn't look".
    fn truncated(&self, found: &mut Vec<Found>) {
        let events = self.current.events_past_cap();
        if events == 0 {
            return;
        }
        let Some(confidence) = rules::incomplete(self.n()) else {
            return;
        };
        let then: Vec<f64> = self
            .runs
            .iter()
            .map(|run| run.events_past_cap() as f64)
            .collect();
        found.push(Found {
            signal: Signal {
                kind: SignalKind::Incomplete,
                behavior: overflow_behavior().id,
                measure: measure::PAST_CAP.to_owned(),
                current: events as f64,
                baseline: BaselineNumbers {
                    runs: then.len() as u32,
                    present_in: then.iter().filter(|&&events| events > 0.0).count() as u32,
                    median: median(&then),
                    min: then.iter().copied().reduce(f64::min),
                    max: then.iter().copied().reduce(f64::max),
                    failures: None,
                },
                exception: None,
                attribution: None,
                confidence,
                tier: tier(SignalKind::Incomplete, Class::Output),
                group: 0,
                headline: false,
            },
            key: Key::Incomplete,
            moved: false,
        });
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
            // A source switched on or off since the baseline accounts for this by itself: it is a change to
            // what siftr read, not to what the command did, and no verdict beats a confident wrong one.
            if !self.configuration_explains(id, kind) && !self.truncation_explains(id, kind) {
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

    /// LATENCY on every behavior that carries a duration: what the run timed is what can have slowed.
    /// An example's duration is one sample a run, so a GC pause lands on it whole and the rest of the
    /// suite vetoes it — the examples either side, or enough of the others; a behavior that recurs
    /// through a run is a mean over its occurrences, which divides such a pause by their number and
    /// leaves the window as the guard that fits it.
    fn latency(&self, found: &mut Vec<Found>) {
        let history = |id: BehaviorId| -> Vec<f64> {
            self.runs
                .iter()
                .filter_map(|run| run.get(id).and_then(duration_ms))
                .collect()
        };
        let total_ms = |b: &BehaviorStats| Some(b.stats.duration?.total.as_secs_f64() * 1e3);
        // The window a slowdown would have to hide in: the run's whole time in this kind of work.
        // Never across kinds — a query's time is inside its request, so a request that slowed with it
        // is that query's consequence, not the rest of the run stalling. Examples use the reporter's
        // suite duration when it reported one: the same total, measured from outside.
        let window_ms = |run: &RunStats, kind: Kind| -> f64 {
            let summed = |kind| {
                run.iter()
                    .filter(|b| b.behavior.kind == kind)
                    .filter_map(|b| b.stats.duration)
                    .map(|d| d.total.as_secs_f64() * 1e3)
                    .sum::<f64>()
            };
            match kind {
                Kind::TestExample => Some(summed(Kind::TestSummary))
                    .filter(|&ms| ms > 0.0)
                    .unwrap_or_else(|| summed(Kind::TestExample)),
                _ => summed(kind),
            }
        };

        let mut examples: Vec<&BehaviorStats> = self
            .current
            .iter()
            .filter(|b| b.behavior.kind == Kind::TestExample)
            .collect();
        examples.sort_by_key(|b| (b.first.as_ref().map(|(_, seq)| *seq), b.behavior.id));
        let excess: Vec<Option<f64>> = examples
            .iter()
            .map(|b| Some(duration_ms(b)? - median(&history(b.behavior.id))?))
            .collect();
        // Examples run one at a time, so the ones either side are the same machine moments earlier and
        // a slowdown they share is the machine. Nothing else has an adjacent anything — behaviors that
        // recur are interleaved, and their causally related neighbours are the ones a real regression
        // moves too — so this veto stays an example's alone.
        let neighbours: HashMap<BehaviorId, f64> = examples
            .iter()
            .enumerate()
            .filter_map(|(i, b)| {
                let worst = [i.checked_sub(1), Some(i + 1)]
                    .into_iter()
                    .flatten()
                    .filter_map(|j| excess.get(j).copied().flatten())
                    .reduce(f64::max)?;
                Some((b.behavior.id, worst))
            })
            .collect();
        // The rest of the suite, for the same reason at a distance: a distant example is the same machine
        // too, just not moments earlier, so it takes several of them to say the machine moved. One slice,
        // sorted once, read by binary search per candidate.
        let mut slowdowns: Vec<f64> = excess.iter().copied().flatten().collect();
        slowdowns.sort_by(|a, b| b.total_cmp(a));

        let mut candidates: Vec<(&BehaviorStats, Class)> = self
            .current
            .iter()
            .filter(|b| b.stats.duration.is_some())
            .filter_map(|b| Some((b, self.class(b.behavior.id)?)))
            .collect();
        candidates.sort_by_key(|(b, _)| (b.first.as_ref().map(|(_, seq)| *seq), b.behavior.id));
        let mut windows: Vec<(Kind, Vec<f64>, f64)> = Vec::new();
        for kind in candidates.iter().map(|(b, _)| b.behavior.kind) {
            if !windows.iter().any(|(seen, ..)| *seen == kind) {
                let baseline: Vec<f64> = self
                    .runs
                    .iter()
                    .map(|run| window_ms(run, kind))
                    .filter(|&ms| ms > 0.0)
                    .collect();
                windows.push((kind, baseline, window_ms(self.current, kind)));
            }
        }

        for (b, class) in candidates {
            let id = b.behavior.id;
            let (Some(current), Some(total)) = (duration_ms(b), total_ms(b)) else {
                continue;
            };
            let baseline = history(id);
            let window = windows
                .iter()
                .find(|(kind, ..)| *kind == b.behavior.kind)
                .and_then(|(_, window_baseline, window_current)| {
                    let totals: Vec<f64> = self
                        .runs
                        .iter()
                        .filter_map(|run| run.get(id).and_then(total_ms))
                        .collect();
                    Some(rules::Window {
                        baseline: window_baseline,
                        current: *window_current,
                        excess: total - median(&totals)?,
                    })
                });
            let cohort = (b.behavior.kind == Kind::TestExample).then(|| rules::Cohort {
                neighbour: neighbours.get(&id).copied(),
                slowdowns: &slowdowns,
            });
            let Some(l) = rules::latency(&baseline, current, cohort, window) else {
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
                id,
                class,
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
                        && b.scopes
                            .iter()
                            .all(|s| matches!(self.phase(s.scope), Phase::Example(e) if ran(e)))
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
                moved: false,
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
        if kind == SignalKind::Frequency && self.suite_size_explains(id, name) {
            tier = SUITE_SIZE_TIER;
        }
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
                    // Grouping asks which unit of work a change is about; attribution asks how much of
                    // the measure moved inside it. A behavior that occurs only inside one scope answers
                    // the first on its own, so a duration — which no scope records, leaving `owners`
                    // comparing zeroes — still groups with the rest of its request. No attribution: the
                    // scope holds no number for such a measure, and 0 → 0 would read as one.
                    _ => match self.sole_scope(id) {
                        Some(scope) => {
                            if scope.outside_examples() {
                                tier = SETUP_TIER;
                            }
                            (Key::Scope(scope), None)
                        }
                        None => (Key::Behavior(id), None),
                    },
                }
            }
        };
        found.push(Found {
            moved: class == Class::Example && presence(kind),
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
            .flat_map(|b| b.scopes.iter().map(|s| self.phase(s.scope)))
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

    /// The one scope every occurrence of `id` falls inside, in each run that has it; `None` when they
    /// spread across scopes, or any falls outside them all.
    ///
    /// Structural, where [`Self::owners`] is measure-based, so this still answers for a measure no scope
    /// records. It cannot disagree with `owners` on one they do record: a sole scope's value *is* the
    /// behavior's own, and every rule that raises a signal needs the behavior's own value to have moved.
    fn sole_scope(&self, id: BehaviorId) -> Option<Phase> {
        let mut sole = None;
        for b in std::iter::once(self.current)
            .chain(self.runs.iter().copied())
            .filter_map(|run| run.get(id))
            .filter(|b| b.stats.count > 0)
        {
            if b.unattributed > 0 || b.unscoped_count() > 0 {
                return None;
            }
            let mut occupied = b.scopes.iter().filter(|s| s.count > 0);
            let (Some(only), None) = (occupied.next(), occupied.next()) else {
                return None;
            };
            let phase = self.phase(only.scope);
            if sole.is_some_and(|seen| seen != phase) {
                return None;
            }
            sole = Some(phase);
        }
        sole
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
    for Found { signal, key, moved } in found {
        groups.entry(key).or_default().push((moved, signal));
    }
    let mut groups: Vec<Vec<(bool, Signal)>> = groups.into_values().collect();
    for members in &mut groups {
        members.sort_by(|(a_moved, a), (b_moved, b)| {
            b_moved.cmp(a_moved).then_with(|| precedence(a, b))
        });
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
