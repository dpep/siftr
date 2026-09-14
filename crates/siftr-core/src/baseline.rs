//! A context's recent runs that are comparable with the current one: what the signal rules compare it against.
//!
//! With 2–10 runs there is no distribution to fit, so the baseline keeps each run and the rules read
//! per-run values (median, range, presence) directly. See `docs/findings/signals.md`.
//!
//! NEW, DISAPPEARED and FREQUENCY need a behavior in every baseline run, so a run that skipped examples
//! would silence them for as long as it stays recent. Such a run is skipped, not compared: fewer baseline
//! runs lower confidence through [`evidence`], which is the honest price.
//!
//! Example counts can't tell a skipped example from one not written yet (a growing suite) or deleted (a
//! shrinking one), and a suite can be partial on every run (a red `--fail-fast` suite). So a run only counts
//! as skipping examples when both its own counts say it may have (errors outside examples, fewer run than
//! loaded, fewer loaded than defined) and it lacks an example that the run it's compared with ran.

use std::collections::HashSet;
use std::fmt;

use crate::aggregate::RunStats;
use crate::behavior::{BehaviorId, Kind};
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
    partial: bool,
}

impl<'a, K> Baseline<'a, K> {
    /// Judges at most [`MAX_RUNS`] candidates, taken from the front (pass the most recent first), against `current`.
    pub fn from_runs(
        current: &RunStats,
        candidates: impl IntoIterator<Item = (K, &'a RunStats)>,
    ) -> Self {
        let now = Tests::of(current);
        let ran_now = examples(current);
        let candidates: Vec<(K, &'a RunStats)> = candidates.into_iter().take(MAX_RUNS).collect();
        // Examples this run ran that a recent run ran too: they existed then, so a run without one skipped it.
        let existed: HashSet<BehaviorId> = candidates
            .iter()
            .flat_map(|(_, run)| examples(run))
            .filter(|example| ran_now.contains(example))
            .collect();
        let mut baseline = Baseline {
            runs: Vec::new(),
            skipped: Vec::new(),
            incomplete: None,
            partial: false,
        };
        for (key, run) in candidates {
            let verdict = match now {
                None => Ok(()),
                Some(now) => now.comparable(Tests::of(run), &examples(run), &existed),
            };
            match verdict {
                Ok(()) => baseline.runs.push((key, run)),
                Err(why) => baseline.skipped.push((key, why)),
            }
        }
        let runs: Vec<&RunStats> = baseline.iter().collect();
        baseline.incomplete = Tests::incomplete(now, &ran_now, &runs);
        baseline.partial = !runs.is_empty()
            && runs
                .iter()
                .all(|run| Tests::of(run).is_some_and(Tests::skipped_existing));
        baseline
    }

    /// Why the current run itself skipped examples its baseline runs ran; `None` when it didn't, or there are none.
    pub fn incomplete(&self) -> Option<Ineligible> {
        self.incomplete
    }

    /// Every baseline run skipped examples that existed (e.g. a red `--fail-fast` suite), so an example none of
    /// them ran isn't necessarily new.
    pub fn partial(&self) -> bool {
        self.partial
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

/// Why a test run skipped examples the run it's compared with ran: a recent run left out of the baseline, or
/// the current run against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ineligible {
    /// Stopped before the test reporter summarized (e.g. killed), or recorded before siftr read test results.
    NoTestSummary,
    /// More errors outside examples than the other side: a spec file that failed to load never ran its examples.
    ErrorsOutsideExamples { errors: u64, compared: u64 },
    /// Ran fewer examples than it loaded, e.g. stopped by `--fail-fast`.
    Stopped { ran: u64, loaded: u64 },
    /// Loaded fewer examples than its files define (e.g. a focus filter), or recorded before siftr counted them;
    /// `compared` is the other side's examples run.
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

/// The examples a run ran.
fn examples(run: &RunStats) -> HashSet<BehaviorId> {
    run.iter()
        .filter(|b| b.behavior.kind == Kind::TestExample && b.stats.count > 0)
        .map(|b| b.behavior.id)
        .collect()
}

/// What a run's test summaries say it ran.
#[derive(Debug, Clone, Copy)]
struct Tests {
    ran: u64,
    /// Examples to run after filters. `None` for runs recorded before siftr kept it.
    loaded: Option<u64>,
    /// Examples in the files that loaded, before filters. `None` for runs recorded before siftr kept it.
    defined: Option<u64>,
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
            defined: total(summary::DEFINED),
            errors_outside: total(summary::ERRORS_OUTSIDE_OF_EXAMPLES).unwrap_or(0),
        })
    }

    fn stopped(self) -> Option<u64> {
        self.loaded.filter(|&loaded| self.ran < loaded)
    }

    fn filtered(self) -> bool {
        matches!((self.loaded, self.defined), (Some(loaded), Some(defined)) if loaded < defined)
    }

    /// Its own counts say some existing examples didn't run. A deleted spec file leaves no such mark.
    fn skipped_existing(self) -> bool {
        self.errors_outside > 0 || self.stopped().is_some() || self.filtered()
    }

    /// Whether `then`, a baseline candidate that ran `then_ran`, ran what this run did of the examples in `existed`.
    fn comparable(
        self,
        then: Option<Tests>,
        then_ran: &HashSet<BehaviorId>,
        existed: &HashSet<BehaviorId>,
    ) -> Result<(), Ineligible> {
        let then = then.ok_or(Ineligible::NoTestSummary)?;
        // Counts that show every existing example ran: a smaller run predates the examples it lacks.
        let ran_all = !then.skipped_existing() && then.defined.is_some();
        if ran_all || existed.is_subset(then_ran) {
            return Ok(());
        }
        Err(if then.errors_outside > self.errors_outside {
            Ineligible::ErrorsOutsideExamples {
                errors: then.errors_outside,
                compared: self.errors_outside,
            }
        } else if let Some(loaded) = then.stopped() {
            Ineligible::Stopped {
                ran: then.ran,
                loaded,
            }
        } else {
            Ineligible::Subset {
                ran: then.ran,
                compared: self.ran,
            }
        })
    }

    /// The same judgement turned around: whether `now`, which ran `ran_now`, skipped examples its baseline `runs` ran.
    fn incomplete(
        now: Option<Tests>,
        ran_now: &HashSet<BehaviorId>,
        runs: &[&RunStats],
    ) -> Option<Ineligible> {
        if runs.is_empty() {
            return None;
        }
        let then: Vec<Option<Tests>> = runs.iter().map(|run| Tests::of(run)).collect();
        let Some(now) = now else {
            return then
                .iter()
                .all(Option::is_some)
                .then_some(Ineligible::NoTestSummary);
        };
        let missed = runs.iter().any(|run| !examples(run).is_subset(ran_now));
        if !missed {
            return None;
        }
        let then: Vec<Tests> = then.into_iter().flatten().collect();
        let errors = then.iter().map(|t| t.errors_outside).max().unwrap_or(0);
        if now.errors_outside > errors {
            return Some(Ineligible::ErrorsOutsideExamples {
                errors: now.errors_outside,
                compared: errors,
            });
        }
        if let Some(loaded) = now.stopped() {
            return Some(Ineligible::Stopped {
                ran: now.ran,
                loaded,
            });
        }
        if now.filtered() {
            let ran: Vec<f64> = then.iter().map(|t| t.ran as f64).collect();
            return Some(Ineligible::Subset {
                ran: now.ran,
                compared: median(&ran).unwrap_or(0.0) as u64,
            });
        }
        // Nothing it defines went unrun: the examples it lacks were deleted.
        None
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

    /// Counts a test summary reports; `None` omits the measure, as older listeners did.
    #[derive(Clone, Copy)]
    struct Counts {
        ran: u64,
        loaded: Option<u64>,
        defined: Option<u64>,
        errors: u64,
    }

    /// Ran every one of `n` defined examples.
    fn all(n: u64) -> Counts {
        Counts {
            ran: n,
            loaded: Some(n),
            defined: Some(n),
            errors: 0,
        }
    }

    fn stat(behavior: Behavior, measures: Vec<Measure>) -> BehaviorStats {
        BehaviorStats {
            behavior,
            first: None,
            stats: Stats {
                count: 1,
                ..Stats::default()
            },
            measures,
            scopes: Vec::new(),
            unattributed: 0,
        }
    }

    /// A test run with `counts` in its summary that ran the examples named in `ran`.
    fn tests(counts: Counts, ran: &str) -> RunStats {
        let measures = [
            (summary::EXAMPLES, Some(counts.ran)),
            (summary::EXPECTED, counts.loaded),
            (summary::DEFINED, counts.defined),
            (summary::ERRORS_OUTSIDE_OF_EXAMPLES, Some(counts.errors)),
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
        let examples = ran.split_whitespace().map(|name| {
            stat(
                Behavior::new(Kind::TestExample, name.as_bytes()),
                Vec::new(),
            )
        });
        std::iter::once(stat(Behavior::new(Kind::TestSummary, b"rspec"), measures))
            .chain(examples)
            .collect()
    }

    const TEN: &str = "a1 a2 a3 a4 b1 b2 b3 b4 b5 b6";

    /// Each candidate alongside clean ten-example runs, judged against `now`: its verdict.
    fn verdict(now: &RunStats, candidate: RunStats) -> Option<Ineligible> {
        let clean = tests(all(10), TEN);
        let runs = [candidate, clean.clone(), clean];
        let baseline = Baseline::from_runs(now, runs.iter().enumerate());
        baseline
            .skipped()
            .iter()
            .find(|(i, _)| *i == 0)
            .map(|(_, why)| *why)
    }

    #[test]
    fn a_recent_run_is_skipped_only_for_examples_it_skipped_that_this_run_ran() {
        let now = tests(all(10), TEN);
        let stopped = Counts {
            ran: 6,
            loaded: Some(10),
            ..all(10)
        };
        let focused = Counts {
            loaded: Some(1),
            ran: 1,
            ..all(10)
        };
        let load_error = Counts {
            errors: 1,
            ..all(6)
        };
        let old = Counts {
            defined: None,
            ..all(1)
        };
        let cases: [(&str, RunStats, Option<Ineligible>); 7] = [
            ("clean", tests(all(10), TEN), None),
            ("before the suite grew", tests(all(4), "a1 a2 a3 a4"), None),
            (
                "killed",
                RunStats::default(),
                Some(Ineligible::NoTestSummary),
            ),
            (
                "--fail-fast",
                tests(stopped, "a1 a2 a3 a4 b1 b2"),
                Some(Ineligible::Stopped { ran: 6, loaded: 10 }),
            ),
            (
                "a focus filter",
                tests(focused, "a3"),
                Some(Ineligible::Subset {
                    ran: 1,
                    compared: 10,
                }),
            ),
            (
                "a spec file failed to load",
                tests(load_error, "b1 b2 b3 b4 b5 b6"),
                Some(Ineligible::ErrorsOutsideExamples {
                    errors: 1,
                    compared: 0,
                }),
            ),
            (
                "recorded before siftr counted defined examples",
                tests(old, "a3"),
                Some(Ineligible::Subset {
                    ran: 1,
                    compared: 10,
                }),
            ),
        ];
        for (name, candidate, expected) in cases {
            assert_eq!(verdict(&now, candidate), expected, "{name}");
        }
    }

    #[test]
    fn runs_that_skipped_the_same_examples_compare() {
        // A red `--fail-fast` suite stops at a4 every run.
        let stopped = Counts {
            ran: 4,
            loaded: Some(10),
            ..all(10)
        };
        let now = tests(stopped, "a1 a2 a3 a4");
        let runs = vec![tests(stopped, "a1 a2 a3 a4"); 3];
        let baseline = Baseline::from_runs(&now, runs.iter().enumerate());
        assert_eq!((baseline.runs(), baseline.incomplete()), (3, None));
        assert!(baseline.partial());

        // A spec file that never loaded, fixed now: its example is new, not skipped.
        let broken = Counts {
            errors: 1,
            ..all(10)
        };
        let fixed = tests(all(11), &format!("{TEN} c1"));
        let runs = vec![tests(broken, TEN); 3];
        let baseline = Baseline::from_runs(&fixed, runs.iter().enumerate());
        assert_eq!((baseline.runs(), baseline.incomplete()), (3, None));
    }

    #[test]
    fn the_current_run_is_incomplete_only_for_examples_it_skipped() {
        let clean = [tests(all(10), TEN), tests(all(10), TEN)];
        let cases: [(&str, RunStats, Option<Ineligible>); 6] = [
            ("clean", tests(all(10), TEN), None),
            ("a spec file deleted", tests(all(4), "a1 a2 a3 a4"), None),
            (
                "killed",
                RunStats::default(),
                Some(Ineligible::NoTestSummary),
            ),
            (
                "a spec file failed to load",
                tests(
                    Counts {
                        errors: 1,
                        ..all(6)
                    },
                    "b1 b2 b3 b4 b5 b6",
                ),
                Some(Ineligible::ErrorsOutsideExamples {
                    errors: 1,
                    compared: 0,
                }),
            ),
            (
                "--fail-fast",
                tests(
                    Counts {
                        ran: 6,
                        loaded: Some(10),
                        ..all(10)
                    },
                    "a1 a2 a3 a4 b1 b2",
                ),
                Some(Ineligible::Stopped { ran: 6, loaded: 10 }),
            ),
            (
                "a focus filter",
                tests(
                    Counts {
                        ran: 1,
                        loaded: Some(1),
                        ..all(10)
                    },
                    "a3",
                ),
                Some(Ineligible::Subset {
                    ran: 1,
                    compared: 10,
                }),
            ),
        ];
        for (name, now, expected) in cases {
            let baseline = Baseline::from_runs(&now, clean.iter().enumerate());
            assert_eq!(baseline.incomplete(), expected, "{name}");
            assert!(!baseline.partial(), "{name}");
        }
        let none: [RunStats; 0] = [];
        let alone = Baseline::from_runs(&RunStats::default(), none.iter().enumerate());
        assert_eq!(alone.incomplete(), None, "nothing to compare with");
    }

    #[test]
    fn ignores_runs_beyond_the_cap() {
        let runs = vec![RunStats::default(); MAX_RUNS + 5];
        let baseline = Baseline::from_runs(&RunStats::default(), runs.iter().enumerate());
        assert_eq!(baseline.runs(), MAX_RUNS as u32);
    }

    #[test]
    fn errors_outside_examples_that_persist_do_not_skip_every_run() {
        let raising = Counts {
            errors: 1,
            ..all(10)
        };
        let now = tests(raising, TEN);
        let runs = [tests(raising, TEN)];
        let baseline = Baseline::from_runs(&now, runs.iter().enumerate());
        assert_eq!((baseline.runs(), baseline.incomplete()), (1, None));
    }

    #[test]
    fn a_run_without_a_test_summary_compares_with_every_recent_run() {
        let runs = [tests(all(1), "a1"), RunStats::default()];
        let baseline = Baseline::from_runs(&RunStats::default(), runs.iter().enumerate());
        assert_eq!(baseline.runs(), 2);
    }

    #[test]
    fn evidence_grows_with_quiet_runs() {
        let rounded: Vec<f64> = [2, 3, 5, 10]
            .map(|n| crate::num::round_sig(evidence(n), 2))
            .to_vec();
        assert_eq!(rounded, [0.75, 0.8, 0.86, 0.92]);
    }
}
