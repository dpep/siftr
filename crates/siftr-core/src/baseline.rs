//! What a context's recent runs say is normal, per behavior.

use std::collections::{HashMap, HashSet};

use crate::aggregate::RunStats;
use crate::behavior::BehaviorId;

/// Older runs than this say more about the past than about normal.
pub const MAX_RUNS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BehaviorBaseline {
    /// Baseline runs in which the behavior occurred at least once.
    pub present_in: u32,
    /// Mean occurrences per run, counting runs where it was absent as zero.
    pub mean_count: f64,
    /// Sample standard deviation of occurrences per run; zero with fewer than two runs.
    pub count_spread: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Baseline {
    runs: u32,
    behaviors: HashMap<BehaviorId, BehaviorBaseline>,
}

impl Baseline {
    /// Uses at most [`MAX_RUNS`] runs, taken from the front: pass the most recent first.
    pub fn from_runs<'a>(runs: impl IntoIterator<Item = &'a RunStats>) -> Self {
        let runs: Vec<&RunStats> = runs.into_iter().take(MAX_RUNS).collect();
        let n = runs.len() as f64;
        let ids: HashSet<BehaviorId> = runs.iter().flat_map(|run| run.keys().copied()).collect();
        let behaviors = ids
            .into_iter()
            .map(|id| {
                let counts = runs
                    .iter()
                    .map(|run| run.get(&id).map_or(0, |stats| stats.count));
                let present_in = counts.clone().filter(|&count| count > 0).count() as u32;
                let mean_count = counts.clone().sum::<u64>() as f64 / n;
                let count_spread = if runs.len() < 2 {
                    0.0
                } else {
                    let squares: f64 = counts
                        .map(|count| (count as f64 - mean_count).powi(2))
                        .sum();
                    (squares / (n - 1.0)).sqrt()
                };
                let baseline = BehaviorBaseline {
                    present_in,
                    mean_count,
                    count_spread,
                };
                (id, baseline)
            })
            .collect();
        Baseline {
            runs: runs.len() as u32,
            behaviors,
        }
    }

    pub fn runs(&self) -> u32 {
        self.runs
    }

    /// A behavior no baseline run saw has a zero baseline.
    pub fn behavior(&self, id: BehaviorId) -> BehaviorBaseline {
        self.behaviors.get(&id).copied().unwrap_or_default()
    }

    pub fn behaviors(&self) -> impl Iterator<Item = (BehaviorId, BehaviorBaseline)> + '_ {
        self.behaviors.iter().map(|(&id, &baseline)| (id, baseline))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::Stats;
    use crate::behavior::Kind;

    #[test]
    fn counts_absent_runs_as_zero() {
        let id = BehaviorId::of(Kind::Log, b"x");
        let run = |count| {
            RunStats::from([(
                id,
                Stats {
                    count,
                    ..Stats::default()
                },
            )])
        };
        let runs = [run(4), RunStats::new(), run(2)];
        let baseline = Baseline::from_runs(&runs);
        assert_eq!(baseline.runs(), 3);
        let b = baseline.behavior(id);
        assert_eq!((b.present_in, b.mean_count, b.count_spread), (2, 2.0, 2.0));
        assert_eq!(
            baseline.behavior(BehaviorId::of(Kind::Log, b"never")),
            BehaviorBaseline::default()
        );
    }

    #[test]
    fn ignores_runs_beyond_the_cap() {
        let runs = vec![RunStats::new(); MAX_RUNS + 5];
        assert_eq!(Baseline::from_runs(&runs).runs(), MAX_RUNS as u32);
    }
}
