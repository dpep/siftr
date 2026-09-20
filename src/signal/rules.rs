//! The decision rules of `docs/findings/signals.md` §2 as pure functions over per-run numbers.
//!
//! Thresholds were fitted to measured noise and backtested at zero false positives; the sweep shows
//! each sits one step from 4–7 false positives. Change one only with a re-run backtest.

use crate::baseline::evidence;
use crate::num::{mad, median, round_sig};

/// NEW, DISAPPEARED, FREQUENCY and LATENCY claim nothing from fewer baseline runs.
pub const MIN_RUNS: usize = 2;
/// LATENCY needs at least this absolute slowdown, since fast examples take ~70ms pauses…
pub const LATENCY_FLOOR_MS: f64 = 100.0;
/// …and this ratio to the baseline median, since relative noise stays at 2–4x at every speed.
pub const LATENCY_RATIO: f64 = 4.0;
/// An adjacent example slowed by this share of the slowdown means the machine stalled, not the code.
/// Examples only: see [`latency`].
pub const NEIGHBOUR_SHARE: f64 = 0.5;
/// The rest of the run slowing by more than this many robust spreads is also a stall.
pub const STALL_SPREADS: f64 = 3.0;
/// A varying measure must leave its range by more than this many range widths.
pub const VARYING_WIDTHS: f64 = 2.0;

/// Scales MAD to a standard deviation under normal noise.
const MAD_TO_SIGMA: f64 = 1.4826;

fn confidence(value: f64) -> f64 {
    round_sig(value, 2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    New,
    Disappeared,
}

/// Present now and in none of `n` runs, or absent now and present in all. Intermittent is never news.
pub fn presence(now: bool, present_in: usize, n: usize) -> Option<(Presence, f64)> {
    if n < MIN_RUNS {
        return None;
    }
    let kind = match (now, present_in) {
        (true, 0) => Presence::New,
        (false, k) if k == n => Presence::Disappeared,
        _ => return None,
    };
    Some((kind, confidence(evidence(n))))
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frequency {
    /// The measure was identical in every baseline run.
    pub exact: bool,
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub confidence: f64,
}

/// `baseline` holds the measure from every baseline run; a run without it disqualifies the comparison.
pub fn frequency(baseline: &[f64], current: f64) -> Option<Frequency> {
    let n = baseline.len();
    if n < MIN_RUNS {
        return None;
    }
    let min = baseline.iter().copied().fold(f64::INFINITY, f64::min);
    let max = baseline.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let median = median(baseline)?;
    if (min..=max).contains(&current) {
        return None;
    }
    let exact = min == max;
    let distance = (current - median).abs();
    let width = max - min;
    if !exact && distance <= VARYING_WIDTHS * width {
        return None;
    }
    let value = evidence(n) * if exact { 1.0 } else { 1.0 - width / distance };
    Some(Frequency {
        exact,
        median,
        min,
        max,
        confidence: confidence(value),
    })
}

/// The current run didn't run what its `n ≥ 1` baseline runs all did. Like a failure, one run is enough to say so.
pub fn incomplete(n: usize) -> Option<f64> {
    (n > 0).then(|| confidence(evidence(n)))
}

/// A failure now, judged against the `n ≥ 1` baseline runs that ran the example, `failures` of which failed.
/// `known_flaky`: a baseline failure had the same exception, so this one is not news.
pub fn error(failures: usize, n: usize, known_flaky: bool) -> Option<f64> {
    if n == 0 || (failures > 0 && known_flaky) {
        return None;
    }
    Some(confidence(1.0 - (failures as f64 + 1.0) / (n as f64 + 2.0)))
}

/// The window a slowdown would have to hide in (ms of total time), for telling a stall from a real
/// change: the suite for an example, the run's total time in that kind of work for anything else.
#[derive(Debug, Clone, Copy)]
pub struct Window<'a> {
    pub baseline: &'a [f64],
    pub current: f64,
    /// How much of `current − median(baseline)` this behavior accounts for, in the same unit. Totals,
    /// not per-occurrence: a behavior occurring 8 times a run moves the window by 8 times its delta,
    /// and charging it only its delta would read the other 7 as the rest of the run stalling.
    pub excess: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Latency {
    pub median: f64,
    pub confidence: f64,
}

/// One behavior's mean duration per occurrence (ms) against the runs that timed it. `neighbour_excess`:
/// the largest slowdown (current − baseline median) of the examples run just before and after it —
/// examples only, since nothing else has an adjacent anything. `window`: what it would have to have
/// slowed alongside to be a stall rather than a change.
pub fn latency(
    baseline: &[f64],
    current: f64,
    neighbour_excess: Option<f64>,
    window: Option<Window<'_>>,
) -> Option<Latency> {
    let n = baseline.len();
    if n < MIN_RUNS {
        return None;
    }
    let typical = median(baseline)?;
    let max = baseline.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let need = LATENCY_FLOOR_MS.max((LATENCY_RATIO - 1.0) * typical);
    let delta = current - typical;
    if delta <= need || current <= max {
        return None;
    }
    if neighbour_excess.unwrap_or(0.0) >= NEIGHBOUR_SHARE * delta {
        return None;
    }
    if let Some(window) = window
        && let (Some(window_median), Some(window_mad)) =
            (median(window.baseline), mad(window.baseline))
    {
        let rest = (window.current - window_median) - window.excess;
        if rest > window.excess && rest > STALL_SPREADS * MAD_TO_SIGMA * window_mad {
            return None;
        }
    }
    let e = delta / need;
    Some(Latency {
        median: typical,
        confidence: confidence(evidence(n) * e / (1.0 + e)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(vector, baseline, current, neighbour excess, window, expected confidence)`.
    type LatencyCase<'a> = (
        u8,
        &'a [f64],
        f64,
        Option<f64>,
        Option<Window<'a>>,
        Option<f64>,
    );
    /// `(vector, baseline, current, expected (exact, confidence))`.
    type FrequencyCase<'a> = (u8, &'a [f64], f64, Option<(bool, f64)>);

    /// signals.md §6, vectors 1–13 and 31.
    #[test]
    fn latency_vectors() {
        let tens = [1.0; 10];
        let quiet = [100.0, 105.0, 110.0];
        let cases: [LatencyCase<'_>; 14] = [
            (1, &[1.0, 1.1, 0.9], 304.0, None, None, Some(0.60)),
            (2, &[1.0, 1.1], 304.0, None, None, Some(0.56)),
            (3, &[1.0], 304.0, None, None, None),
            (4, &tens, 304.0, None, None, Some(0.69)),
            (5, &[1.0, 1.1, 0.9], 150.0, None, None, Some(0.48)),
            (6, &[20.0, 22.0, 250.0], 240.0, None, None, None),
            (7, &[50.0, 55.0, 60.0], 180.0, None, None, None),
            (8, &[50.0, 55.0, 60.0], 260.0, None, None, Some(0.44)),
            (9, &[1000.0, 1100.0, 900.0], 2500.0, None, None, None),
            (10, &[1000.0, 1100.0, 900.0], 4200.0, None, None, Some(0.41)),
            (11, &[1.0, 1.1, 0.9], 304.0, Some(201.0 - 1.0), None, None),
            (
                12,
                &[10.0, 11.0, 12.0],
                120.0,
                None,
                Some(Window {
                    baseline: &quiet,
                    current: 707.0,
                    excess: 109.0,
                }),
                None,
            ),
            (
                13,
                &[10.0, 11.0, 12.0],
                120.0,
                None,
                Some(Window {
                    baseline: &quiet,
                    current: 215.0,
                    excess: 109.0,
                }),
                Some(0.42),
            ),
            // Vector 12's window, for a behavior occurring 8 times a run: its own 109ms per occurrence
            // is 872ms of the window's move, which leaves no rest to blame. Charging it 109ms, as an
            // example is charged, would veto it — the trap the `excess` unit exists to close.
            (
                31,
                &[10.0, 11.0, 12.0],
                120.0,
                None,
                Some(Window {
                    baseline: &quiet,
                    current: 977.0,
                    excess: 872.0,
                }),
                Some(0.42),
            ),
        ];
        for (vector, baseline, current, neighbour, window, expected) in cases {
            let got = latency(baseline, current, neighbour, window).map(|l| l.confidence);
            assert_eq!(got, expected, "vector {vector}");
        }
    }

    /// signals.md §6, vectors 14–19.
    #[test]
    fn frequency_vectors() {
        let cases: [FrequencyCase<'_>; 6] = [
            (14, &[3.0, 3.0, 3.0], 10.0, Some((true, 0.80))),
            (15, &[3.0; 5], 2.0, Some((true, 0.86))),
            (16, &[3.0, 3.0], 3.0, None),
            (17, &[3.0], 10.0, None),
            (18, &[10.0, 12.0, 11.0], 13.0, None),
            (19, &[10.0, 12.0, 11.0], 20.0, Some((false, 0.62))),
        ];
        for (vector, baseline, current, expected) in cases {
            let got = frequency(baseline, current).map(|f| (f.exact, f.confidence));
            assert_eq!(got, expected, "vector {vector}");
        }
    }

    /// signals.md §6, vectors 20–24.
    #[test]
    fn presence_vectors() {
        use Presence::*;
        let cases = [
            (20, true, 0, 5, Some((New, 0.86))),
            (21, true, 2, 5, None),
            (22, true, 0, 1, None),
            (23, false, 5, 5, Some((Disappeared, 0.86))),
            (24, false, 4, 5, None),
        ];
        for (vector, now, present_in, n, expected) in cases {
            assert_eq!(presence(now, present_in, n), expected, "vector {vector}");
        }
    }

    /// signals.md §6, vectors 25–29. The caller decides flakiness by exception class.
    #[test]
    fn error_vectors() {
        let cases = [
            (25, 0, 5, false, Some(0.86)),
            (26, 0, 1, false, Some(0.67)),
            (27, 1, 5, true, None),
            (28, 1, 5, false, Some(0.71)),
            (29, 0, 0, false, None),
        ];
        for (vector, failures, n, flaky, expected) in cases {
            assert_eq!(error(failures, n, flaky), expected, "vector {vector}");
        }
    }
}
