//! Behavioral changes between a run and its baseline, each with evidence-derived confidence.
//!
//! Precision over recall: nothing is claimed without [`MIN_BASELINE_RUNS`], and confidence is a
//! product of evidence fractions in [0, 1), so thin evidence always reads as low confidence.

use std::fmt;
use std::str::FromStr;

use crate::aggregate::RunStats;
use crate::baseline::{Baseline, BehaviorBaseline};
use crate::behavior::BehaviorId;
use crate::num::round_sig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SignalKind {
    /// Occurs now; absent from every baseline run.
    New,
    /// Absent now; present in nearly every baseline run.
    Disappeared,
    /// Occurs far more or less often than run-to-run spread explains.
    Frequency,
}

impl SignalKind {
    pub const ALL: [SignalKind; 3] = [
        SignalKind::New,
        SignalKind::Disappeared,
        SignalKind::Frequency,
    ];

    /// Persisted and printed in JSON: stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            SignalKind::New => "new",
            SignalKind::Disappeared => "disappeared",
            SignalKind::Frequency => "frequency",
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

/// Every number here is the evidence for the claim, rounded to the precision it has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Signal {
    pub kind: SignalKind,
    pub behavior: BehaviorId,
    /// Occurrences in the run under test.
    pub count: u64,
    pub baseline_runs: u32,
    /// Mean rounded to three significant figures, spread to two.
    pub baseline: BehaviorBaseline,
    /// In [0, 1), two significant figures.
    pub confidence: f64,
}

pub const MIN_BASELINE_RUNS: u32 = 2;
/// DISAPPEARED needs the behavior in at least this share of baseline runs.
pub const DISAPPEARED_MIN_PRESENCE: f64 = 0.8;
/// FREQUENCY needs the change to exceed this many spreads…
pub const FREQUENCY_MIN_SPREADS: f64 = 3.0;
/// …and this fraction of the baseline mean.
pub const FREQUENCY_MIN_RELATIVE: f64 = 0.25;

/// Most confident first.
pub fn detect(current: &RunStats, baseline: &Baseline) -> Vec<Signal> {
    let runs = baseline.runs();
    if runs < MIN_BASELINE_RUNS {
        return Vec::new();
    }
    let present = current
        .iter()
        .filter(|(_, stats)| stats.count > 0)
        .filter_map(|(&id, stats)| judge_present(id, stats.count, baseline.behavior(id), runs));
    let absent = baseline
        .behaviors()
        .filter(|(id, _)| current.get(id).is_none_or(|stats| stats.count == 0))
        .filter_map(|(id, b)| judge_absent(id, b, runs));
    let mut signals: Vec<Signal> = present.chain(absent).collect();
    signals.sort_by(|a, b| {
        b.confidence
            .total_cmp(&a.confidence)
            .then(a.kind.cmp(&b.kind))
            .then(a.behavior.cmp(&b.behavior))
    });
    signals
}

fn judge_present(id: BehaviorId, count: u64, b: BehaviorBaseline, runs: u32) -> Option<Signal> {
    let n = f64::from(runs);
    if b.present_in == 0 {
        // Laplace's rule of succession: absent from n runs, it stays absent next run with p = (n+1)/(n+2).
        let confidence = (n + 1.0) / (n + 2.0) * volume(count as f64);
        return Some(signal(SignalKind::New, id, count, runs, b, confidence));
    }
    // Counts vary at least like a Poisson process, even when a few baseline runs happen to agree exactly.
    let spread = b.count_spread.max(b.mean_count.sqrt()).max(1.0);
    let change = (count as f64 - b.mean_count).abs();
    let spreads = change / spread;
    if spreads < FREQUENCY_MIN_SPREADS || change < FREQUENCY_MIN_RELATIVE * b.mean_count {
        return None;
    }
    let confidence = (1.0 - 1.0 / spreads) * (n / (n + 1.0));
    Some(signal(
        SignalKind::Frequency,
        id,
        count,
        runs,
        b,
        confidence,
    ))
}

fn judge_absent(id: BehaviorId, b: BehaviorBaseline, runs: u32) -> Option<Signal> {
    let n = f64::from(runs);
    if f64::from(b.present_in) < DISAPPEARED_MIN_PRESENCE * n {
        return None;
    }
    let confidence = (f64::from(b.present_in) + 1.0) / (n + 2.0) * volume(b.mean_count);
    Some(signal(SignalKind::Disappeared, id, 0, runs, b, confidence))
}

/// One occurrence is half as convincing as many.
fn volume(count: f64) -> f64 {
    count / (count + 1.0)
}

fn signal(
    kind: SignalKind,
    behavior: BehaviorId,
    count: u64,
    runs: u32,
    b: BehaviorBaseline,
    confidence: f64,
) -> Signal {
    Signal {
        kind,
        behavior,
        count,
        baseline_runs: runs,
        baseline: BehaviorBaseline {
            present_in: b.present_in,
            mean_count: round_sig(b.mean_count, 3),
            count_spread: round_sig(b.count_spread, 2),
        },
        confidence: round_sig(confidence, 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::Stats;
    use crate::behavior::Kind;

    fn id(name: &str) -> BehaviorId {
        BehaviorId::of(Kind::Log, name.as_bytes())
    }

    fn run(counts: &[(&str, u64)]) -> RunStats {
        counts
            .iter()
            .map(|&(name, count)| {
                (
                    id(name),
                    Stats {
                        count,
                        ..Stats::default()
                    },
                )
            })
            .collect()
    }

    /// `(kind, count, confidence)` per signal.
    type Found = Vec<(SignalKind, u64, f64)>;

    /// Signals for behavior `x` against a baseline where an unrelated behavior holds steady.
    fn detect_x(baseline_counts: &[u64], current: u64) -> Found {
        let runs: Vec<RunStats> = baseline_counts
            .iter()
            .map(|&c| run(&[("x", c), ("steady", 5)]))
            .collect();
        let current = run(&[("x", current), ("steady", 5)]);
        detect(&current, &Baseline::from_runs(&runs))
            .into_iter()
            .map(|s| (s.kind, s.count, s.confidence))
            .collect()
    }

    #[test]
    fn judges_changes_against_the_baseline() {
        use SignalKind::*;
        let cases: [(&str, &[u64], u64, Found); 9] = [
            ("one baseline run is not enough", &[0], 9, vec![]),
            (
                "new: (3+1)/(3+2) * 4/5",
                &[0, 0, 0],
                4,
                vec![(New, 4, 0.64)],
            ),
            (
                "new with thin baseline reads lower",
                &[0, 0],
                4,
                vec![(New, 4, 0.6)],
            ),
            (
                "disappeared: (3+1)/(3+2) * 10/11",
                &[10, 10, 10],
                0,
                vec![(Disappeared, 0, 0.73)],
            ),
            (
                "flaky behavior absent now is not a signal",
                &[3, 0, 0],
                0,
                vec![],
            ),
            ("within spread", &[10, 13, 8], 12, vec![]),
            (
                "frequency: 10 -> 40",
                &[10, 11, 9],
                40,
                vec![(Frequency, 40, 0.67)],
            ),
            (
                "identical runs still get a Poisson floor",
                &[100, 100, 100],
                120,
                vec![],
            ),
            (
                "frequency drop",
                &[100, 100, 100],
                40,
                vec![(Frequency, 40, 0.63)],
            ),
        ];
        for (name, baseline, current, expected) in cases {
            assert_eq!(detect_x(baseline, current), expected, "{name}");
        }
    }

    #[test]
    fn more_baseline_runs_mean_more_confidence() {
        let thin = detect_x(&[0, 0], 5)[0].2;
        let deep = detect_x(&[0; 10], 5)[0].2;
        assert!(thin < deep, "{thin} < {deep}");
    }

    #[test]
    fn evidence_is_rounded_where_built() {
        let runs: Vec<RunStats> = [1, 2, 2].iter().map(|&c| run(&[("x", c)])).collect();
        let [signal] = detect(&run(&[("x", 20)]), &Baseline::from_runs(&runs))
            .try_into()
            .expect("one signal");
        assert_eq!(signal.baseline.mean_count, 1.67);
        assert_eq!(signal.baseline.count_spread, 0.58);
    }
}
