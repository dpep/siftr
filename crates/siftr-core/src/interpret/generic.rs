//! Fallback interpreter: every line is a `log` behavior keyed by its normalized template.

use std::time::Duration;

use super::{Claim, Event, Interpreter, Outcome};
use crate::aggregate::Aggregator;
use crate::behavior::Kind;
use crate::normalize::{Normalizer, SlotKind, slot_value_f64};
use crate::observation::Observation;

pub struct Generic;

impl Interpreter for Generic {
    fn observe(
        &mut self,
        obs: Observation<'_>,
        normalizer: &mut Normalizer,
        sink: &mut Aggregator,
    ) -> Claim {
        let template = normalizer.normalize(obs.line);
        let duration = template
            .slots
            .iter()
            .find(|slot| slot.kind == SlotKind::Duration)
            .and_then(|slot| slot_value_f64(obs.line, slot))
            .and_then(|ms| Duration::try_from_secs_f64(ms / 1e3).ok());
        let outcome = has_error_level(template.template).then_some(Outcome::Failure);
        sink.record(&Event {
            kind: Kind::Log,
            template,
            input: obs.line,
            source: obs,
            duration,
            outcome,
            scope: None,
            measures: &[],
        });
        Claim::Claimed
    }
}

/// An uppercase `ERROR` or `FATAL` word near the start, where log formats put the level.
fn has_error_level(template: &[u8]) -> bool {
    template[..template.len().min(64)]
        .split(|b| !b.is_ascii_alphabetic())
        .any(|word| word == b"ERROR" || word == b"FATAL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_error_levels_only_as_words_near_the_start() {
        let cases = [
            ("E, [2026-09-13] ERROR -- : boom", true),
            ("[FATAL] disk full", true),
            ("Error: lowercase is prose, not a level", false),
            ("ERRORS were counted", false),
            (&format!("{} ERROR late", "x".repeat(80)), false),
        ];
        for (line, expected) in cases {
            assert_eq!(has_error_level(line.as_bytes()), expected, "{line}");
        }
    }

    #[test]
    fn takes_the_duration_from_the_first_duration_slot() {
        let stream = crate::observation::Stream::Stdout;
        let line = b"Completed 200 OK in 12.5ms (Views: 3ms)";
        let mut aggregator = Aggregator::new();
        let obs = Observation {
            stream: &stream,
            seq: 1,
            line,
        };
        let _ = Generic.observe(obs, &mut Normalizer::new(), &mut aggregator);
        let stats = aggregator.finish()[0].stats;
        assert_eq!(
            stats.duration.map(|d| d.max),
            Some(Duration::from_micros(12_500))
        );
    }
}
