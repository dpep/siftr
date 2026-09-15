//! Per-behavior statistics for one run, in bounded memory.

mod histogram;

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use histogram::LogHistogram;

use crate::behavior::{Behavior, BehaviorId, Kind};
use crate::interpret::{Event, Outcome};
use crate::observation::{Observation, Stream};

/// Exemplars always kept from the start of a behavior's occurrences.
pub const FIRST_EXEMPLARS: usize = 3;
/// Exemplars reservoir-sampled from the rest, so late occurrences are represented too.
pub const SAMPLED_EXEMPLARS: usize = 5;
/// Exemplar text is cut here; the run's capture keeps the whole line at the exemplar's `seq`.
pub const MAX_EXEMPLAR_BYTES: usize = 1024;
/// Distinct behaviors kept per run. Events of later behaviors count into the overflow behavior.
pub const MAX_BEHAVIORS: usize = 20_000;
/// (behavior, scope) attributions kept per run. Past it, a scoped event still counts, as unattributed.
pub const MAX_SCOPE_CELLS: usize = 200_000;
/// Distinct measure names kept per behavior. A test summary has six.
pub const MAX_MEASURES: usize = 6;
/// The template of the one behavior that absorbs events past [`MAX_BEHAVIORS`].
pub const OVERFLOW_TEMPLATE: &str = "siftr: events of behaviors beyond the per-run cap";

/// The behavior counting events whose own behavior didn't fit under [`MAX_BEHAVIORS`].
pub fn overflow_behavior() -> Behavior {
    Behavior::new(Kind::Log, OVERFLOW_TEMPLATE.as_bytes())
}

/// A raw line kept as evidence, addressable in the run's capture by `stream` and `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exemplar {
    pub stream: Stream,
    pub seq: u64,
    pub line: String,
}

impl From<&Observation<'_>> for Exemplar {
    fn from(obs: &Observation<'_>) -> Self {
        let mut line = String::from_utf8_lossy(obs.line).into_owned();
        if line.len() > MAX_EXEMPLAR_BYTES {
            let cut = (0..=MAX_EXEMPLAR_BYTES)
                .rev()
                .find(|&i| line.is_char_boundary(i))
                .unwrap_or(0);
            line.truncate(cut);
        }
        Exemplar {
            stream: obs.stream.clone(),
            seq: obs.seq,
            line,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DurationSummary {
    pub count: u64,
    pub total: Duration,
    /// Estimated from a log histogram and rounded to two significant figures.
    pub p50: Duration,
    /// Estimated from a log histogram and rounded to two significant figures.
    pub p95: Duration,
    pub max: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub count: u64,
    /// Events whose outcome was a failure.
    pub errors: u64,
    /// Present when any event carried a duration.
    pub duration: Option<DurationSummary>,
}

/// A named measure's values across a behavior's events in one run.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeasureStats {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

impl MeasureStats {
    fn record(&mut self, value: f64) {
        if self.count == 0 {
            self.min = value;
            self.max = value;
        } else {
            self.min = self.min.min(value);
            self.max = self.max.max(value);
        }
        self.count += 1;
        self.sum += value;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Measure {
    pub name: String,
    pub stats: MeasureStats,
}

/// A behavior's occurrences within one scope (e.g. one test example), with its measures summed there.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeStats {
    pub scope: BehaviorId,
    pub count: u64,
    /// `(measure name, sum)`, by name.
    pub sums: Vec<(String, f64)>,
}

/// Where a test run's log line fell relative to its examples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Phase {
    /// Before the first example started: boot, schema checks, `before(:suite)`.
    Setup,
    /// Inside this example.
    Example(BehaviorId),
    /// After one example finished and before the next started: `before(:context)`, `after(:context)`.
    Between,
    /// After the last example finished: `after(:suite)`.
    Teardown,
}

static BETWEEN: LazyLock<BehaviorId> =
    LazyLock::new(|| BehaviorId::of(Kind::TestSummary, b"siftr: between examples"));
static TEARDOWN: LazyLock<BehaviorId> =
    LazyLock::new(|| BehaviorId::of(Kind::TestSummary, b"siftr: after the last example"));

impl Phase {
    /// The phase as an event's scope, the id aggregates and the store keep it by. Setup is no scope, as
    /// runs recorded before phases were told apart kept it; the other phases outside examples take
    /// reserved ids no behavior has.
    pub fn scope_id(self) -> Option<BehaviorId> {
        match self {
            Phase::Setup => None,
            Phase::Example(id) => Some(id),
            Phase::Between => Some(*BETWEEN),
            Phase::Teardown => Some(*TEARDOWN),
        }
    }

    pub fn from_scope_id(id: Option<BehaviorId>) -> Self {
        match id {
            None => Phase::Setup,
            Some(id) if id == *BETWEEN => Phase::Between,
            Some(id) if id == *TEARDOWN => Phase::Teardown,
            Some(id) => Phase::Example(id),
        }
    }

    /// Not inside any one example: the environment or suite-level hooks, not an example's own code.
    pub fn outside_examples(self) -> bool {
        !matches!(self, Phase::Example(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub behavior: Behavior,
    pub stats: Stats,
    /// By name.
    pub measures: Vec<Measure>,
    /// Scoped occurrences, by scope id. Occurrences outside any scope are `count - Σ scopes - unattributed`.
    pub scopes: Vec<ScopeStats>,
    /// Scoped occurrences that didn't fit under [`MAX_SCOPE_CELLS`]: counted, but not attributed.
    pub unattributed: u64,
    /// In stream order.
    pub exemplars: Vec<Exemplar>,
}

#[derive(Debug)]
pub struct Aggregator {
    behaviors: HashMap<BehaviorId, Accumulator>,
    cells: usize,
    overflow: BehaviorId,
    rng: XorShift,
}

impl Default for Aggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl Aggregator {
    pub fn new() -> Self {
        Aggregator {
            behaviors: HashMap::new(),
            cells: 0,
            overflow: overflow_behavior().id,
            rng: XorShift(0x9e37_79b9_7f4a_7c15),
        }
    }

    pub fn record(&mut self, event: &Event<'_>) {
        let id = BehaviorId::of(event.kind, event.template.template);
        // Examples and summaries are exempt: the suite bounds them, and they are what everything else is scoped to.
        let admitted = self.behaviors.contains_key(&id)
            || self.behaviors.len() < MAX_BEHAVIORS
            || matches!(event.kind, Kind::TestExample | Kind::TestSummary);
        let overflow = self.overflow;
        let acc = if admitted {
            self.behaviors.entry(id).or_insert_with(|| {
                Accumulator::new(Behavior {
                    id,
                    kind: event.kind,
                    template: String::from_utf8_lossy(event.template.template).into_owned(),
                    roles: event.template.roles,
                })
            })
        } else {
            self.behaviors
                .entry(overflow)
                .or_insert_with(|| Accumulator::new(overflow_behavior()))
        };
        acc.count += 1;
        if event.outcome == Some(Outcome::Failure) {
            acc.errors += 1;
        }
        acc.keep_exemplar(&event.source, &mut self.rng);
        if !admitted {
            return;
        }
        if let Some(duration) = event.duration {
            acc.durations.get_or_insert_default().record(duration);
        }
        let measures = event.measures;
        for &(name, value) in measures.iter().filter(|(_, v)| v.is_finite()) {
            match acc.measures.iter().position(|(known, _)| *known == name) {
                Some(i) => acc.measures[i].1.record(value),
                None if acc.measures.len() < MAX_MEASURES => {
                    let mut stats = MeasureStats::default();
                    stats.record(value);
                    acc.measures.push((name, stats));
                }
                None => {}
            }
        }
        if let Some(scope) = event.scope {
            match acc.scopes.get_mut(&scope) {
                Some(cell) => cell.record(measures),
                None if self.cells < MAX_SCOPE_CELLS => {
                    self.cells += 1;
                    acc.scopes.entry(scope).or_default().record(measures);
                }
                None => acc.unattributed += 1,
            }
        }
        // Per-slot value stats (`normalize::SlotStats`) are observed here: `event.template.slots` index `event.input`.
    }

    /// Most frequent first.
    pub fn finish(self) -> Vec<Aggregate> {
        let mut aggregates: Vec<Aggregate> = self
            .behaviors
            .into_values()
            .map(Accumulator::finish)
            .collect();
        aggregates.sort_by(|a, b| {
            b.stats
                .count
                .cmp(&a.stats.count)
                .then_with(|| a.behavior.id.cmp(&b.behavior.id))
        });
        aggregates
    }
}

#[derive(Debug)]
struct Accumulator {
    behavior: Behavior,
    count: u64,
    errors: u64,
    durations: Option<Box<LogHistogram>>,
    measures: Vec<(&'static str, MeasureStats)>,
    scopes: HashMap<BehaviorId, Cell>,
    unattributed: u64,
    first: Vec<Exemplar>,
    sampled: Vec<Exemplar>,
}

#[derive(Debug, Default)]
struct Cell {
    count: u64,
    sums: Vec<(&'static str, f64)>,
}

impl Cell {
    fn record(&mut self, measures: &[(&'static str, f64)]) {
        self.count += 1;
        for &(name, value) in measures.iter().filter(|(_, v)| v.is_finite()) {
            match self.sums.iter().position(|(known, _)| *known == name) {
                Some(i) => self.sums[i].1 += value,
                None if self.sums.len() < MAX_MEASURES => self.sums.push((name, value)),
                None => {}
            }
        }
    }
}

impl Accumulator {
    fn new(behavior: Behavior) -> Self {
        Accumulator {
            behavior,
            count: 0,
            errors: 0,
            durations: None,
            measures: Vec::new(),
            scopes: HashMap::new(),
            unattributed: 0,
            first: Vec::new(),
            sampled: Vec::new(),
        }
    }

    /// Algorithm R over occurrences after the first few. Call after counting this occurrence.
    fn keep_exemplar(&mut self, source: &Observation<'_>, rng: &mut XorShift) {
        if self.first.len() < FIRST_EXEMPLARS {
            self.first.push(source.into());
        } else if self.sampled.len() < SAMPLED_EXEMPLARS {
            self.sampled.push(source.into());
        } else {
            let seen = self.count - FIRST_EXEMPLARS as u64;
            let slot = rng.below(seen) as usize;
            if let Some(kept) = self.sampled.get_mut(slot) {
                *kept = source.into();
            }
        }
    }

    fn finish(self) -> Aggregate {
        let mut exemplars = self.first;
        let mut sampled = self.sampled;
        sampled.sort_by_key(|e| e.seq);
        exemplars.append(&mut sampled);
        let mut measures: Vec<Measure> = self
            .measures
            .into_iter()
            .map(|(name, stats)| Measure {
                name: name.to_owned(),
                stats,
            })
            .collect();
        measures.sort_by(|a, b| a.name.cmp(&b.name));
        let mut scopes: Vec<ScopeStats> = self
            .scopes
            .into_iter()
            .map(|(scope, cell)| {
                let mut sums: Vec<(String, f64)> = cell
                    .sums
                    .into_iter()
                    .map(|(name, sum)| (name.to_owned(), sum))
                    .collect();
                sums.sort_by(|a, b| a.0.cmp(&b.0));
                ScopeStats {
                    scope,
                    count: cell.count,
                    sums,
                }
            })
            .collect();
        scopes.sort_by_key(|s| s.scope);
        Aggregate {
            behavior: self.behavior,
            stats: Stats {
                count: self.count,
                errors: self.errors,
                duration: self.durations.map(|h| h.summary()),
            },
            measures,
            scopes,
            unattributed: self.unattributed,
            exemplars,
        }
    }
}

/// Fixed seed: the same input keeps the same exemplars.
#[derive(Debug)]
struct XorShift(u64);

impl XorShift {
    fn below(&mut self, bound: u64) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x % bound
    }
}

/// One run as the signal rules read it: every behavior's stats, measures and scope attribution.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunStats {
    behaviors: HashMap<BehaviorId, BehaviorStats>,
}

/// An [`Aggregate`] without its exemplars, but with where it first occurred.
#[derive(Debug, Clone, PartialEq)]
pub struct BehaviorStats {
    pub behavior: Behavior,
    /// The first occurrence: its stream classifies the behavior, its line orders examples.
    pub first: Option<(Stream, u64)>,
    pub stats: Stats,
    pub measures: Vec<Measure>,
    pub scopes: Vec<ScopeStats>,
    pub unattributed: u64,
}

impl BehaviorStats {
    pub fn from_aggregate(aggregate: &Aggregate) -> Self {
        BehaviorStats {
            behavior: aggregate.behavior.clone(),
            first: aggregate
                .exemplars
                .first()
                .map(|e| (e.stream.clone(), e.seq)),
            stats: aggregate.stats,
            measures: aggregate.measures.clone(),
            scopes: aggregate.scopes.clone(),
            unattributed: aggregate.unattributed,
        }
    }

    pub fn measure(&self, name: &str) -> Option<&MeasureStats> {
        self.measures
            .iter()
            .find(|m| m.name == name)
            .map(|m| &m.stats)
    }

    /// Occurrences outside every scope: a test run's [`Phase::Setup`], or a stream with no examples.
    pub fn unscoped_count(&self) -> u64 {
        let scoped: u64 = self.scopes.iter().map(|s| s.count).sum();
        self.stats
            .count
            .saturating_sub(scoped)
            .saturating_sub(self.unattributed)
    }
}

impl RunStats {
    pub fn from_aggregates(aggregates: &[Aggregate]) -> Self {
        aggregates
            .iter()
            .map(BehaviorStats::from_aggregate)
            .collect()
    }

    pub fn get(&self, id: BehaviorId) -> Option<&BehaviorStats> {
        self.behaviors.get(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &BehaviorStats> {
        self.behaviors.values()
    }

    pub fn len(&self) -> usize {
        self.behaviors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.behaviors.is_empty()
    }

    /// Occurrences of `id` in this run; zero when absent.
    pub fn count(&self, id: BehaviorId) -> u64 {
        self.get(id).map_or(0, |b| b.stats.count)
    }
}

impl FromIterator<BehaviorStats> for RunStats {
    fn from_iter<I: IntoIterator<Item = BehaviorStats>>(iter: I) -> Self {
        RunStats {
            behaviors: iter.into_iter().map(|b| (b.behavior.id, b)).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalize::Normalizer;

    struct Recorder {
        aggregator: Aggregator,
        normalizer: Normalizer,
        stream: Stream,
        seq: u64,
    }

    impl Recorder {
        fn new() -> Self {
            Recorder {
                aggregator: Aggregator::new(),
                normalizer: Normalizer::new(),
                stream: Stream::Stdout,
                seq: 0,
            }
        }

        fn event(
            &mut self,
            kind: Kind,
            line: &str,
            duration: Option<Duration>,
            scope: Option<BehaviorId>,
            measures: &[(&'static str, f64)],
        ) {
            self.seq += 1;
            let source = Observation {
                stream: &self.stream,
                seq: self.seq,
                line: line.as_bytes(),
                raw_len: line.len() as u64 + 1,
            };
            self.aggregator.record(&Event {
                kind,
                template: self.normalizer.normalize(line.as_bytes()),
                input: line.as_bytes(),
                source,
                duration,
                outcome: None,
                scope,
                measures,
            });
        }

        fn log(&mut self, line: &str, duration: Option<Duration>) {
            self.event(Kind::Log, line, duration, None, &[]);
        }
    }

    #[test]
    fn exemplars_are_bounded_and_start_with_the_first_occurrences() {
        let mut r = Recorder::new();
        for seq in 1..=1_000 {
            r.log(&format!("tick {seq}"), None);
        }
        let [aggregate] = r.aggregator.finish().try_into().expect("one behavior");
        assert_eq!(aggregate.stats.count, 1_000);
        let seqs: Vec<u64> = aggregate.exemplars.iter().map(|e| e.seq).collect();
        assert_eq!(seqs.len(), FIRST_EXEMPLARS + SAMPLED_EXEMPLARS);
        assert_eq!(seqs[..3], [1, 2, 3]);
        assert!(
            seqs[3..].iter().any(|&seq| seq > 100),
            "sampling reaches past the start: {seqs:?}"
        );
        assert!(seqs.is_sorted());
    }

    #[test]
    fn summarizes_durations() {
        let mut r = Recorder::new();
        for ms in 1..=100 {
            r.log("query", Some(Duration::from_millis(ms)));
        }
        let summary = r.aggregator.finish()[0]
            .stats
            .duration
            .expect("durations recorded");
        assert_eq!(summary.count, 100);
        assert_eq!(summary.total, Duration::from_millis(5_050));
        assert_eq!(summary.max, Duration::from_millis(100));
        let near = |actual: Duration, expected_ms: f64| {
            (actual.as_secs_f64() * 1e3 / expected_ms - 1.0).abs() < 0.15
        };
        assert!(near(summary.p50, 50.0), "p50 {:?}", summary.p50);
        assert!(near(summary.p95, 95.0), "p95 {:?}", summary.p95);
    }

    #[test]
    fn attributes_occurrences_and_measures_to_scopes() {
        let mut r = Recorder::new();
        let show = BehaviorId::of(Kind::TestExample, b"shows a user");
        let index = BehaviorId::of(Kind::TestExample, b"lists users");
        r.event(Kind::DbQuery, "SELECT users", None, None, &[]);
        for _ in 0..3 {
            r.event(Kind::DbQuery, "SELECT users", None, Some(show), &[]);
        }
        r.event(Kind::DbQuery, "SELECT users", None, Some(index), &[]);
        r.event(
            Kind::HttpRequest,
            "UsersController#show",
            None,
            Some(show),
            &[("queries", 3.0)],
        );
        r.event(
            Kind::HttpRequest,
            "UsersController#show",
            None,
            Some(show),
            &[("queries", 7.0)],
        );
        let run = RunStats::from_aggregates(&r.aggregator.finish());
        let sql = run
            .iter()
            .find(|b| b.behavior.kind == Kind::DbQuery)
            .unwrap();
        assert_eq!(sql.unscoped_count(), 1, "setup, before any example");
        let counts: Vec<_> = sql.scopes.iter().map(|s| (s.scope, s.count)).collect();
        let mut expected = vec![(show, 3), (index, 1)];
        expected.sort();
        assert_eq!(counts, expected);

        let request = run
            .iter()
            .find(|b| b.behavior.kind == Kind::HttpRequest)
            .unwrap();
        let queries = request.measure("queries").unwrap();
        assert_eq!(
            (queries.count, queries.sum, queries.min, queries.max),
            (2, 10.0, 3.0, 7.0)
        );
        assert_eq!(request.scopes[0].sums, [("queries".to_owned(), 10.0)]);
    }

    #[test]
    fn distinct_behaviors_are_capped_into_one_overflow_behavior() {
        let mut r = Recorder::new();
        // Letters only, so the normalizer can't fold them into one template.
        let word = |mut i: usize| {
            let mut w = String::new();
            loop {
                w.push((b'a' + (i % 26) as u8) as char);
                i /= 26;
                if i == 0 {
                    break w;
                }
            }
        };
        let extra = 50;
        for i in 0..MAX_BEHAVIORS + extra {
            r.log(&format!("event {}", word(i)), None);
        }
        r.event(
            Kind::TestExample,
            "an example past the cap",
            None,
            None,
            &[],
        );
        let aggregates = r.aggregator.finish();
        assert_eq!(
            aggregates.len(),
            MAX_BEHAVIORS + 2,
            "cap + overflow + exempt example"
        );
        let overflow = aggregates
            .iter()
            .find(|a| a.behavior == overflow_behavior())
            .expect("overflow behavior");
        assert_eq!(overflow.stats.count, extra as u64);
        assert!(overflow.exemplars.len() <= FIRST_EXEMPLARS + SAMPLED_EXEMPLARS);
    }

    #[test]
    fn phases_round_trip_through_scope_ids() {
        let example = Phase::Example(BehaviorId::of(Kind::TestExample, b"passes"));
        for phase in [Phase::Setup, example, Phase::Between, Phase::Teardown] {
            assert_eq!(Phase::from_scope_id(phase.scope_id()), phase);
        }
    }

    #[test]
    fn exemplar_text_is_capped_on_a_char_boundary() {
        let mut r = Recorder::new();
        r.log(&"é".repeat(MAX_EXEMPLAR_BYTES), None);
        let line = &r.aggregator.finish()[0].exemplars[0].line;
        assert!(line.len() <= MAX_EXEMPLAR_BYTES && line.len() >= MAX_EXEMPLAR_BYTES - 1);
    }
}
