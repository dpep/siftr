//! A context's recent runs: what the signal rules compare a run against.
//!
//! With 2–10 runs there is no distribution to fit, so the baseline keeps each run and the rules read
//! per-run values (median, range, presence) directly. See `docs/findings/signals.md`.

use crate::aggregate::RunStats;

/// Older runs than this say more about the past than about normal.
pub const MAX_RUNS: usize = 10;

#[derive(Debug, Clone, Default)]
pub struct Baseline<'a> {
    runs: Vec<&'a RunStats>,
}

impl<'a> Baseline<'a> {
    /// Uses at most [`MAX_RUNS`] runs, taken from the front: pass the most recent first.
    pub fn from_runs(runs: impl IntoIterator<Item = &'a RunStats>) -> Self {
        Baseline {
            runs: runs.into_iter().take(MAX_RUNS).collect(),
        }
    }

    pub fn runs(&self) -> u32 {
        self.runs.len() as u32
    }

    /// Most recent first.
    pub fn iter(&self) -> impl Iterator<Item = &'a RunStats> + '_ {
        self.runs.iter().copied()
    }
}

/// Evidence from `n` quiet runs, by the rule of succession: after `n` runs without an event, P(event) ≈ 1/(n+2).
pub fn evidence(n: usize) -> f64 {
    (n as f64 + 1.0) / (n as f64 + 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_runs_beyond_the_cap() {
        let runs = vec![RunStats::default(); MAX_RUNS + 5];
        assert_eq!(Baseline::from_runs(&runs).runs(), MAX_RUNS as u32);
    }

    #[test]
    fn evidence_grows_with_quiet_runs() {
        let rounded: Vec<f64> = [2, 3, 5, 10]
            .map(|n| crate::num::round_sig(evidence(n), 2))
            .to_vec();
        assert_eq!(rounded, [0.75, 0.8, 0.86, 0.92]);
    }
}
