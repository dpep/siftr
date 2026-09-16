//! What the kernel charged the wrapped command: CPU time, peak memory, context switches.
//!
//! One event per run, count 1, on a single behavior: no per-line cost and no unbounded behaviors.
//!
//! **Evidence, never a signal.** No threshold over these numbers works: against a two-run baseline a
//! measure rule fires on ~37% of runs, but by five it has gone blind instead — the noise floor is
//! 20–43% of median CPU, so a regression adding a fifth of a suite's CPU passes unremarked. Peak RSS
//! fails the opposite way: baseline windows are often exactly equal, which lands it in the rule's
//! exact branch, where any change at all fires (`docs/findings/resources.md` §3).
//! [`Kind::Resources`] is the one kind the rules never judge:
//! `signal::Comparison::class` maps it to `None`, and `judge` returns there before any rule runs.
//! That one arm is what keeps these numbers out of every rule; `explain` and `-j` show them instead.

use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;

use super::{Event, literal};
use crate::aggregate::{Aggregator, RunStats};
use crate::behavior::{BehaviorId, Kind};
use crate::num::{median, round_sig};
use crate::observation::{Observation, Stream};

/// Measures of the `run.resources` behavior. Each name carries its unit, so none needs a lookup.
pub mod measure {
    /// User-mode CPU the command and its descendants burned.
    pub const CPU_USER_MS: &str = "cpu_user_ms";
    /// Kernel-mode CPU: a jump here is I/O or syscall volume rather than slower code.
    pub const CPU_SYSTEM_MS: &str = "cpu_system_ms";
    /// Peak resident set. Normalized to bytes at the syscall: `ru_maxrss` is bytes on macOS and
    /// kibibytes on Linux, so the raw field lies by 1024x on one of them.
    pub const MAX_RSS_BYTES: &str = "max_rss_bytes";
    /// Gave up the CPU waiting (I/O, a lock).
    pub const VOLUNTARY_SWITCHES: &str = "voluntary_switches";
    /// Was preempted: the machine was busy, which is the confounder a LATENCY signal must survive.
    pub const INVOLUNTARY_SWITCHES: &str = "involuntary_switches";
}

/// The stream a resource exemplar names. Nothing was read from a file: the numbers come from the
/// wait, so this stream has no capture and never joins a run's `streams`.
pub const STREAM: &str = "rusage";

/// Identity of the one run-level behavior. A literal, so the values never enter the behavior id.
const TEMPLATE: &[u8] = b"siftr: what the kernel charged this run";

static BEHAVIOR: LazyLock<BehaviorId> = LazyLock::new(|| BehaviorId::of(Kind::Resources, TEMPLATE));

/// The behavior every run's accounting is recorded on.
pub fn behavior() -> BehaviorId {
    *BEHAVIOR
}

/// One run's accounting, in units that can't be misread: the syscall's platform quirks are
/// normalized away before a `Resources` exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Resources {
    pub cpu_user: Duration,
    pub cpu_system: Duration,
    pub max_rss_bytes: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
}

impl Resources {
    /// The headline: all CPU the command burned, user and kernel together.
    pub fn cpu(&self) -> Duration {
        self.cpu_user + self.cpu_system
    }

    /// Records this run's accounting as one event. CPU is rounded to whole milliseconds where it is
    /// built: the kernel reports microseconds but accounts in ticks, so the extra digits aren't real.
    pub fn record(&self, sink: &mut Aggregator) {
        let measures = [
            (measure::CPU_USER_MS, whole_ms(self.cpu_user)),
            (measure::CPU_SYSTEM_MS, whole_ms(self.cpu_system)),
            (measure::MAX_RSS_BYTES, self.max_rss_bytes as f64),
            (measure::VOLUNTARY_SWITCHES, self.voluntary_switches as f64),
            (
                measure::INVOLUNTARY_SWITCHES,
                self.involuntary_switches as f64,
            ),
        ];
        // The exemplar line is the evidence for these measures. It holds kernel counters only, so it
        // bypasses the redactor that every captured line goes through without carrying user data.
        let line = self.to_string();
        let stream = Stream::File(STREAM.into());
        sink.record(&Event {
            kind: Kind::Resources,
            template: literal(TEMPLATE),
            input: TEMPLATE,
            source: Observation {
                stream: &stream,
                seq: 1,
                line: line.as_bytes(),
                raw_len: line.len() as u64 + 1,
            },
            duration: None,
            outcome: None,
            scope: None,
            measures: &measures,
        });
    }

    /// What `run` recorded, or `None` for a run recorded before siftr measured it. The two switch
    /// counts default to zero so a platform that stops reporting them doesn't hide the CPU and RSS.
    pub fn of(run: &RunStats) -> Option<Self> {
        let b = run.get(*BEHAVIOR)?;
        let sum = |name| b.measure(name).map(|m| m.sum);
        Some(Resources {
            cpu_user: from_ms(sum(measure::CPU_USER_MS)?),
            cpu_system: from_ms(sum(measure::CPU_SYSTEM_MS)?),
            max_rss_bytes: count(sum(measure::MAX_RSS_BYTES)?),
            voluntary_switches: count(sum(measure::VOLUNTARY_SWITCHES).unwrap_or(0.0)),
            involuntary_switches: count(sum(measure::INVOLUNTARY_SWITCHES).unwrap_or(0.0)),
        })
    }

    /// Typical usage across `runs`, field by field, for comparing a run with its baseline. `None`
    /// when no run recorded any: a median of nothing would read as zero.
    pub fn median(runs: impl IntoIterator<Item = Resources>) -> Option<Self> {
        let runs: Vec<Resources> = runs.into_iter().collect();
        let mid = |f: fn(&Resources) -> f64| {
            let values: Vec<f64> = runs.iter().map(f).collect();
            median(&values)
        };
        Some(Resources {
            cpu_user: from_ms(mid(|r| whole_ms(r.cpu_user))?),
            cpu_system: from_ms(mid(|r| whole_ms(r.cpu_system))?),
            max_rss_bytes: count(mid(|r| r.max_rss_bytes as f64)?),
            voluntary_switches: count(mid(|r| r.voluntary_switches as f64)?),
            involuntary_switches: count(mid(|r| r.involuntary_switches as f64)?),
        })
    }
}

/// `cpu 3.42s (3.1s user, 0.32s sys), max rss 412 MB, 1230 voluntary and 56 involuntary switches`.
/// Rounded for reading; the measures keep what the kernel gave.
impl fmt::Display for Resources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cpu {} ({} user, {} sys), max rss {}, {} voluntary and {} involuntary switches",
            secs(self.cpu()),
            secs(self.cpu_user),
            secs(self.cpu_system),
            megabytes(self.max_rss_bytes),
            self.voluntary_switches,
            self.involuntary_switches,
        )
    }
}

fn whole_ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1e3).round()
}

/// Never panics as `Duration::from_secs_f64` does on a negative or non-finite value.
fn from_ms(ms: f64) -> Duration {
    match ms.is_finite() && ms > 0.0 {
        true => Duration::from_secs_f64(ms / 1e3),
        false => Duration::ZERO,
    }
}

fn count(value: f64) -> u64 {
    match value.is_finite() && value > 0.0 {
        true => value as u64,
        false => 0,
    }
}

fn secs(d: Duration) -> String {
    format!("{}s", round_sig(d.as_secs_f64(), 3))
}

fn megabytes(bytes: u64) -> String {
    format!("{} MB", round_sig(bytes as f64 / 1e6, 3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregate::Aggregator;

    fn sample() -> Resources {
        Resources {
            cpu_user: Duration::from_millis(3_100),
            cpu_system: Duration::from_millis(320),
            max_rss_bytes: 412_000_000,
            voluntary_switches: 1_230,
            involuntary_switches: 56,
        }
    }

    fn recorded(resources: &Resources) -> RunStats {
        let mut aggregator = Aggregator::new();
        resources.record(&mut aggregator);
        RunStats::from_aggregates(&aggregator.finish())
    }

    #[test]
    fn one_run_level_behavior_carries_every_measure_once() {
        let run = recorded(&sample());
        assert_eq!(run.len(), 1, "one behavior, never one per line");
        let b = run.get(behavior()).expect("the resources behavior");
        assert_eq!(b.stats.count, 1);
        assert_eq!(b.behavior.kind, Kind::Resources);
        assert_eq!(b.stats.duration, None, "a run's CPU is not a latency");

        let mut names: Vec<&str> = b.measures.iter().map(|m| m.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "cpu_system_ms",
                "cpu_user_ms",
                "involuntary_switches",
                "max_rss_bytes",
                "voluntary_switches",
            ]
        );
        assert!(
            names.len() <= crate::aggregate::MAX_MEASURES,
            "every measure must fit under the per-behavior cap"
        );
    }

    /// The values are evidence, so they must survive the round trip through a run's stats intact.
    #[test]
    fn what_was_recorded_reads_back_unchanged() {
        assert_eq!(Resources::of(&recorded(&sample())), Some(sample()));
        assert_eq!(
            Resources::of(&RunStats::default()),
            None,
            "a run recorded before siftr measured this has none"
        );
    }

    /// The template never carries the values: two runs that burned wildly different CPU are the
    /// same behavior, so nothing reads as NEW or DISAPPEARED.
    #[test]
    fn the_behavior_is_the_same_however_much_the_run_burned() {
        let idle = Resources::default();
        let busy = sample();
        assert_eq!(
            recorded(&idle).get(behavior()).unwrap().behavior.id,
            recorded(&busy).get(behavior()).unwrap().behavior.id
        );
    }

    #[test]
    fn typical_usage_is_the_median_of_each_field() {
        let at = |cpu_ms, rss| Resources {
            cpu_user: Duration::from_millis(cpu_ms),
            max_rss_bytes: rss,
            ..Resources::default()
        };
        let median = Resources::median([at(100, 30), at(900, 10), at(500, 20)]).unwrap();
        assert_eq!(median.cpu_user, Duration::from_millis(500));
        assert_eq!(median.max_rss_bytes, 20);
        assert_eq!(Resources::median([]), None, "no runs, no typical value");
    }

    #[test]
    fn the_evidence_line_reads_without_a_unit_lookup() {
        assert_eq!(
            sample().to_string(),
            "cpu 3.42s (3.1s user, 0.32s sys), max rss 412 MB, 1230 voluntary and 56 involuntary switches"
        );
    }
}
