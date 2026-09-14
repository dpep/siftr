//! Interpreters turn observations into events: a semantic kind, a template, and extracted measures.
//!
//! Each interpreter lives in its own submodule and is registered in [`default_interpreters`].

pub mod generic;

use std::time::Duration;

use crate::aggregate::Aggregator;
use crate::behavior::Kind;
use crate::normalize::{Normalized, Normalizer};
use crate::observation::Observation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    Skipped,
}

/// An interpreted observation. Borrowed: it lives only as long as the call that records it.
#[derive(Debug, Clone, Copy)]
pub struct Event<'a> {
    pub kind: Kind,
    pub template: Normalized<'a>,
    /// The bytes `template` was normalized from. Its slot spans index into these, which need not be `source.line`.
    pub input: &'a [u8],
    /// The observation kept as evidence for this event.
    pub source: Observation<'a>,
    pub duration: Option<Duration>,
    pub outcome: Option<Outcome>,
}

#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// Later interpreters don't see this observation, whether or not an event was recorded yet.
    Claimed,
    Declined,
}

pub trait Interpreter {
    /// Offers one observation from any stream, including side channels (`Stream::File`).
    /// An interpreter may record events now, or buffer and record them from [`Interpreter::finish`].
    fn observe(
        &mut self,
        obs: Observation<'_>,
        normalizer: &mut Normalizer,
        sink: &mut Aggregator,
    ) -> Claim;

    /// Called once after the last observation.
    fn finish(&mut self, _normalizer: &mut Normalizer, _sink: &mut Aggregator) {}
}

/// The interpreter chain, most specific first. `generic` claims everything, so it stays last.
pub fn default_interpreters() -> Vec<Box<dyn Interpreter>> {
    vec![Box::new(generic::Generic)]
}

/// Parses a duration such as `12.3ms`, `0.5 seconds` or `40µs`.
pub fn parse_duration(text: &[u8]) -> Option<Duration> {
    let number_len = text
        .iter()
        .take_while(|b| b.is_ascii_digit() || **b == b'.')
        .count();
    let (number, unit) = text.split_at(number_len);
    let value: f64 = std::str::from_utf8(number).ok()?.parse().ok()?;
    let seconds_per_unit = match unit.strip_prefix(b" ").unwrap_or(unit) {
        b"ns" => 1e-9,
        b"us" => 1e-6,
        b"ms" | b"milliseconds" => 1e-3,
        b"s" | b"sec" | b"secs" | b"second" | b"seconds" => 1.0,
        b"min" | b"minutes" => 60.0,
        unit if unit == "µs".as_bytes() => 1e-6,
        _ => return None,
    };
    Duration::try_from_secs_f64(value * seconds_per_unit).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_durations() {
        let cases: [(&str, Option<Duration>); 6] = [
            ("12.5ms", Some(Duration::from_micros(12_500))),
            ("1.5 seconds", Some(Duration::from_millis(1_500))),
            ("40µs", Some(Duration::from_micros(40))),
            ("2min", Some(Duration::from_secs(120))),
            ("12", None),
            ("fast", None),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_duration(text.as_bytes()), expected, "{text}");
        }
    }
}
