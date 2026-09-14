//! Per-behavior statistics for one run, in bounded memory.

mod histogram;

use std::collections::HashMap;
use std::time::Duration;

use histogram::LogHistogram;

use crate::behavior::{Behavior, BehaviorId};
use crate::interpret::{Event, Outcome};
use crate::observation::{Observation, Stream};

/// Exemplars always kept from the start of a behavior's occurrences.
pub const FIRST_EXEMPLARS: usize = 3;
/// Exemplars reservoir-sampled from the rest, so late occurrences are represented too.
pub const SAMPLED_EXEMPLARS: usize = 5;

/// A raw line kept as evidence, addressable in the run's capture by `stream` and `seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exemplar {
    pub stream: Stream,
    pub seq: u64,
    pub line: String,
}

impl From<&Observation<'_>> for Exemplar {
    fn from(obs: &Observation<'_>) -> Self {
        Exemplar {
            stream: obs.stream.clone(),
            seq: obs.seq,
            line: String::from_utf8_lossy(obs.line).into_owned(),
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

/// One run's stats, by behavior: what baselines are built from.
pub type RunStats = HashMap<BehaviorId, Stats>;

#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    pub behavior: Behavior,
    pub stats: Stats,
    /// In stream order.
    pub exemplars: Vec<Exemplar>,
}

#[derive(Debug)]
pub struct Aggregator {
    behaviors: HashMap<BehaviorId, Accumulator>,
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
            rng: XorShift(0x9e37_79b9_7f4a_7c15),
        }
    }

    pub fn record(&mut self, event: &Event<'_>) {
        let id = crate::behavior::BehaviorId::of(event.kind, event.template.template);
        let acc = self.behaviors.entry(id).or_insert_with(|| Accumulator {
            behavior: Behavior {
                id,
                kind: event.kind,
                template: String::from_utf8_lossy(event.template.template).into_owned(),
            },
            count: 0,
            errors: 0,
            durations: None,
            first: Vec::new(),
            sampled: Vec::new(),
        });
        acc.count += 1;
        if event.outcome == Some(Outcome::Failure) {
            acc.errors += 1;
        }
        if let Some(duration) = event.duration {
            acc.durations.get_or_insert_default().record(duration);
        }
        // Per-slot value stats (siftr-normalize's SlotStats) are observed here: `event.template.slots` index `event.input`.
        acc.keep_exemplar(&event.source, &mut self.rng);
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
    first: Vec<Exemplar>,
    sampled: Vec<Exemplar>,
}

impl Accumulator {
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
        Aggregate {
            behavior: self.behavior,
            stats: Stats {
                count: self.count,
                errors: self.errors,
                duration: self.durations.map(|h| h.summary()),
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::behavior::Kind;
    use crate::normalize::Normalizer;

    fn record(
        aggregator: &mut Aggregator,
        normalizer: &mut Normalizer,
        seq: u64,
        line: &str,
        duration: Option<Duration>,
    ) {
        let stream = Stream::Stdout;
        let source = Observation {
            stream: &stream,
            seq,
            line: line.as_bytes(),
        };
        aggregator.record(&Event {
            kind: Kind::Log,
            template: normalizer.normalize(line.as_bytes()),
            input: line.as_bytes(),
            source,
            duration,
            outcome: None,
            scope: None,
            measures: &[],
        });
    }

    #[test]
    fn exemplars_are_bounded_and_start_with_the_first_occurrences() {
        let mut aggregator = Aggregator::new();
        let mut normalizer = Normalizer::new();
        for seq in 1..=1_000 {
            record(
                &mut aggregator,
                &mut normalizer,
                seq,
                &format!("tick {seq}"),
                None,
            );
        }
        let [aggregate] = aggregator.finish().try_into().expect("one behavior");
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
        let mut aggregator = Aggregator::new();
        let mut normalizer = Normalizer::new();
        for ms in 1..=100 {
            record(
                &mut aggregator,
                &mut normalizer,
                ms,
                "query",
                Some(Duration::from_millis(ms)),
            );
        }
        let summary = aggregator.finish()[0]
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
}
