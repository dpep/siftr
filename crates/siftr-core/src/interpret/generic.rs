//! Fallback interpreter: every line is a `log` behavior keyed by its normalized template.

use super::{Claim, Event, Interpreter, Outcome, parse_duration};
use crate::aggregate::Aggregator;
use crate::behavior::Kind;
use crate::normalize::{Normalizer, SlotKind};
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
            .and_then(|slot| parse_duration(&obs.line[slot.start as usize..slot.end as usize]));
        let outcome = has_error_level(template.template).then_some(Outcome::Failure);
        sink.record(&Event {
            kind: Kind::Log,
            template,
            input: obs.line,
            source: obs,
            duration,
            outcome,
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
}
