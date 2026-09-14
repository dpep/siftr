//! A context's recent runs that are comparable with the current one: what the signal rules compare it against.
//!
//! With 2–10 runs there is no distribution to fit, so the baseline keeps each run and the rules read
//! per-run values (median, range, presence) directly. See `docs/findings/signals.md`.
//!
//! NEW, DISAPPEARED and FREQUENCY need a behavior in every baseline run, so one run that didn't run the
//! suite would silence them for as long as it stays recent. Such a run is skipped, not compared: fewer
//! baseline runs lower confidence through [`evidence`], which is the honest price.

use std::fmt;

use crate::aggregate::RunStats;
use crate::behavior::Kind;
use crate::interpret::rspec::summary;
use crate::num::median;

/// Older runs than this say more about the past than about normal.
pub const MAX_RUNS: usize = 10;

/// The comparable runs, each with the key its caller names it by (e.g. a store's run id).
#[derive(Debug, Clone)]
pub struct Baseline<'a, K> {
    runs: Vec<(K, &'a RunStats)>,
    skipped: Vec<(K, Ineligible)>,
    incomplete: Option<Ineligible>,
}

impl<'a, K> Baseline<'a, K> {
    /// Judges at most [`MAX_RUNS`] candidates, taken from the front (pass the most recent first), against `current`.
    pub fn from_runs(
        current: &RunStats,
        candidates: impl IntoIterator<Item = (K, &'a RunStats)>,
    ) -> Self {
        let now = Tests::of(current);
        let mut baseline = Baseline {
            runs: Vec::new(),
            skipped: Vec::new(),
            incomplete: None,
        };
        for (key, run) in candidates.into_iter().take(MAX_RUNS) {
            match now.map_or(Ok(()), |now| now.comparable(Tests::of(run))) {
                Ok(()) => baseline.runs.push((key, run)),
                Err(why) => baseline.skipped.push((key, why)),
            }
        }
        let then: Vec<Option<Tests>> = baseline.iter().map(Tests::of).collect();
        baseline.incomplete = Tests::incomplete(now, &then);
        baseline
    }

    /// Why the current run itself didn't run what every baseline run did; `None` when it did, or there are none.
    pub fn incomplete(&self) -> Option<Ineligible> {
        self.incomplete
    }

    pub fn runs(&self) -> u32 {
        self.runs.len() as u32
    }

    /// Most recent first.
    pub fn iter(&self) -> impl Iterator<Item = &'a RunStats> + '_ {
        self.runs.iter().map(|(_, run)| *run)
    }

    /// The comparable runs' keys, most recent first.
    pub fn keys(&self) -> impl Iterator<Item = &K> + '_ {
        self.runs.iter().map(|(key, _)| key)
    }

    /// Recent runs left out of the baseline and why, most recent first.
    pub fn skipped(&self) -> &[(K, Ineligible)] {
        &self.skipped
    }
}

/// Why a test run didn't run what the run it's compared with did: a recent run skipped from the baseline,
/// or the current run against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    /// Stopped before the test reporter summarized (e.g. killed), or recorded before siftr read test results.
    NoTestSummary,
    /// More errors outside examples than the other side. RSpec still runs the other files when one fails to
    /// load, so this is the only mark of the examples that never ran.
    ErrorsOutsideExamples { errors: u64, compared: u64 },
    /// Ran fewer examples than it loaded, e.g. stopped by `--fail-fast`.
    Stopped { ran: u64, loaded: u64 },
    /// Ran under half the examples of the other side, e.g. a focus filter.
    Subset { ran: u64, compared: u64 },
}

impl Ineligible {
    /// Printed in JSON: stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Ineligible::NoTestSummary => "no_test_summary",
            Ineligible::ErrorsOutsideExamples { .. } => "errors_outside_examples",
            Ineligible::Stopped { .. } => "stopped",
            Ineligible::Subset { .. } => "subset",
        }
    }
}

impl fmt::Display for Ineligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Ineligible::NoTestSummary => f.write_str("no test summary"),
            Ineligible::ErrorsOutsideExamples { errors, compared } => {
                write!(f, "{errors} errors outside examples vs {compared}")
            }
            Ineligible::Stopped { ran, loaded } => {
                write!(f, "stopped after {ran} of {loaded} examples")
            }
            Ineligible::Subset { ran, compared } => {
                write!(f, "ran {ran} examples vs {compared}")
            }
        }
    }
}

/// What a run's test summaries say it ran.
#[derive(Debug, Clone, Copy)]
struct Tests {
    ran: u64,
    /// `None` for runs recorded before siftr kept it.
    loaded: Option<u64>,
    errors_outside: u64,
}

impl Tests {
    fn of(run: &RunStats) -> Option<Self> {
        let summaries = || run.iter().filter(|b| b.behavior.kind == Kind::TestSummary);
        summaries().next()?;
        let total = |name| {
            summaries()
                .map(|b| b.measure(name).map(|m| m.sum as u64))
                .sum::<Option<u64>>()
        };
        Some(Tests {
            ran: total(summary::EXAMPLES).unwrap_or(0),
            loaded: total(summary::EXPECTED),
            errors_outside: total(summary::ERRORS_OUTSIDE_OF_EXAMPLES).unwrap_or(0),
        })
    }

    /// Whether `then`, a baseline candidate, ran what this run did.
    fn comparable(self, then: Option<Tests>) -> Result<(), Ineligible> {
        let then = then.ok_or(Ineligible::NoTestSummary)?;
        if then.errors_outside > self.errors_outside {
            return Err(Ineligible::ErrorsOutsideExamples {
                errors: then.errors_outside,
                compared: self.errors_outside,
            });
        }
        if let Some(loaded) = then.loaded
            && then.ran < loaded
        {
            return Err(Ineligible::Stopped {
                ran: then.ran,
                loaded,
            });
        }
        // Example counts are identical run to run and an edit moves them by a few; a filter moves them by most.
        let now = self.loaded.unwrap_or(self.ran);
        if then.ran * 2 < now {
            return Err(Ineligible::Subset {
                ran: then.ran,
                compared: now,
            });
        }
        Ok(())
    }

    /// The same judgement turned around: whether `now` ran what its baseline runs `then` did.
    fn incomplete(now: Option<Tests>, then: &[Option<Tests>]) -> Option<Ineligible> {
        let Some(now) = now else {
            let all = !then.is_empty() && then.iter().all(Option::is_some);
            return all.then_some(Ineligible::NoTestSummary);
        };
        // `from_runs` already skipped baseline runs without a summary.
        let then: Vec<Tests> = then.iter().flatten().copied().collect();
        let errors = then.iter().map(|t| t.errors_outside).max()?;
        if now.errors_outside > errors {
            return Some(Ineligible::ErrorsOutsideExamples {
                errors: now.errors_outside,
                compared: errors,
            });
        }
        if let Some(loaded) = now.loaded
            && now.ran < loaded
        {
            return Some(Ineligible::Stopped {
                ran: now.ran,
                loaded,
            });
        }
        let ran: Vec<f64> = then.iter().map(|t| t.ran as f64).collect();
        let compared = median(&ran)? as u64;
        (now.ran * 2 < compared).then_some(Ineligible::Subset {
            ran: now.ran,
            compared,
        })
    }
}

/// Evidence from `n` quiet runs, by the rule of succession: after `n` runs without an event, P(event) ≈ 1/(n+2).
pub fn evidence(n: usize) -> f64 {
    (n as f64 + 1.0) / (n as f64 + 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::{BehaviorStats, Measure, MeasureStats, Stats};
    use crate::behavior::Behavior;

    /// A run whose test summary reports `(examples, expected, errors outside)`; `expected: None` omits it.
    fn tests(examples: u64, expected: Option<u64>, errors: u64) -> RunStats {
        let measures = [
            (summary::EXAMPLES, Some(examples)),
            (summary::EXPECTED, expected),
            (summary::ERRORS_OUTSIDE_OF_EXAMPLES, Some(errors)),
        ]
        .into_iter()
        .filter_map(|(name, value)| {
            let sum = value? as f64;
            Some(Measure {
                name: name.to_owned(),
                stats: MeasureStats {
                    count: 1,
                    sum,
                    min: sum,
                    max: sum,
                },
            })
        })
        .collect();
        std::iter::once(BehaviorStats {
            behavior: Behavior::new(Kind::TestSummary, b"rspec"),
            first: None,
            stats: Stats {
                count: 1,
                ..Stats::default()
            },
            measures,
            scopes: Vec::new(),
            unattributed: 0,
        })
        .collect()
    }

    #[test]
    fn ignores_runs_beyond_the_cap() {
        let runs = vec![RunStats::default(); MAX_RUNS + 5];
        let baseline = Baseline::from_runs(&RunStats::default(), runs.iter().enumerate());
        assert_eq!(baseline.runs(), MAX_RUNS as u32);
    }

    #[test]
    fn a_test_run_skips_recent_runs_that_did_not_run_its_suite() {
        let now = tests(10, Some(10), 0);
        let not_a_test_run = RunStats::default();
        let cases: [(&str, RunStats, Option<Ineligible>); 8] = [
            ("clean", tests(10, Some(10), 0), None),
            ("an example added since", tests(9, Some(9), 0), None),
            (
                "kept before siftr read `expected`",
                tests(10, None, 0),
                None,
            ),
            (
                "killed, or recorded before siftr read test results",
                not_a_test_run,
                Some(Ineligible::NoTestSummary),
            ),
            (
                "a spec file failed to load",
                tests(9, Some(9), 1),
                Some(Ineligible::ErrorsOutsideExamples {
                    errors: 1,
                    compared: 0,
                }),
            ),
            (
                "--fail-fast",
                tests(6, Some(10), 0),
                Some(Ineligible::Stopped { ran: 6, loaded: 10 }),
            ),
            (
                "a focus filter",
                tests(1, Some(1), 0),
                Some(Ineligible::Subset {
                    ran: 1,
                    compared: 10,
                }),
            ),
            ("half the suite", tests(5, Some(5), 0), None),
        ];
        let baseline = Baseline::from_runs(&now, cases.iter().map(|(name, run, _)| (*name, run)));
        let skipped: Vec<_> = baseline.skipped().to_vec();
        let expected: Vec<_> = cases
            .iter()
            .filter_map(|(name, _, why)| Some((*name, (*why)?)))
            .collect();
        assert_eq!(skipped, expected);
        assert_eq!(baseline.runs(), 4);
    }

    #[test]
    fn errors_outside_examples_that_persist_do_not_skip_every_run() {
        let now = tests(10, Some(10), 1);
        let run = tests(10, Some(10), 1);
        let baseline = Baseline::from_runs(&now, [((), &run)]);
        assert_eq!((baseline.runs(), baseline.skipped()), (1, &[][..]));
    }

    #[test]
    fn a_run_without_a_test_summary_compares_with_every_recent_run() {
        let runs = [tests(1, Some(10), 3), RunStats::default()];
        let baseline = Baseline::from_runs(&RunStats::default(), runs.iter().enumerate());
        assert_eq!(baseline.runs(), 2);
    }

    #[test]
    fn a_current_run_that_did_not_run_what_its_baseline_did_is_incomplete() {
        let clean = [tests(10, Some(10), 0), tests(10, Some(10), 0)];
        let cases: [(&str, RunStats, Option<Ineligible>); 6] = [
            ("clean", tests(10, Some(10), 0), None),
            ("an example deleted", tests(9, Some(9), 0), None),
            (
                "killed",
                RunStats::default(),
                Some(Ineligible::NoTestSummary),
            ),
            (
                "a spec file failed to load",
                tests(8, Some(8), 1),
                Some(Ineligible::ErrorsOutsideExamples {
                    errors: 1,
                    compared: 0,
                }),
            ),
            (
                "--fail-fast",
                tests(6, Some(10), 0),
                Some(Ineligible::Stopped { ran: 6, loaded: 10 }),
            ),
            (
                "a focus filter",
                tests(1, Some(1), 0),
                Some(Ineligible::Subset {
                    ran: 1,
                    compared: 10,
                }),
            ),
        ];
        for (name, now, expected) in cases {
            let baseline = Baseline::from_runs(&now, clean.iter().enumerate());
            assert_eq!(baseline.incomplete(), expected, "{name}");
        }
        let none: [RunStats; 0] = [];
        let alone = Baseline::from_runs(&RunStats::default(), none.iter().enumerate());
        assert_eq!(alone.incomplete(), None, "nothing to compare with");
    }

    #[test]
    fn evidence_grows_with_quiet_runs() {
        let rounded: Vec<f64> = [2, 3, 5, 10]
            .map(|n| crate::num::round_sig(evidence(n), 2))
            .to_vec();
        assert_eq!(rounded, [0.75, 0.8, 0.86, 0.92]);
    }
}
