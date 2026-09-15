//! Interpreters turn observations into events: a semantic kind, a template, and extracted measures.
//!
//! Each interpreter lives in its own submodule and is registered in [`default_interpreters`].
//! Numeric slot values (durations in ms, sizes in bytes) come from [`crate::normalize::slot_value_f64`].

#[cfg(test)]
mod fixtures;
pub mod generic;
mod rails;
pub mod rspec;

use std::time::Duration;

use crate::aggregate::Aggregator;
use crate::behavior::{BehaviorId, Kind};
use crate::normalize::{Normalized, Normalizer, PathRoles, fnv1a64};
use crate::observation::Observation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Failure,
    Skipped,
}

/// An interpreted observation. Borrowed: it lives only as long as the call that records it.
pub struct Event<'a> {
    pub kind: Kind,
    pub template: Normalized<'a>,
    /// The bytes `template` was normalized from. Its slot spans index into these, which need not be `source.line`.
    pub input: &'a [u8],
    /// The observation kept as evidence for this event.
    pub source: Observation<'a>,
    pub duration: Option<Duration>,
    pub outcome: Option<Outcome>,
    /// The enclosing test example's behavior, when known (e.g. SQL attributed via log offsets).
    pub scope: Option<BehaviorId>,
    /// Named measures beyond duration, e.g. `("queries", 3.0)` on a request.
    pub measures: &'a [(&'static str, f64)],
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
    vec![
        Box::new(rspec::Rspec::default()),
        Box::new(generic::Generic),
    ]
}

/// A template taken verbatim, with no slots. For names people wrote (example descriptions, class
/// names, controller actions), a digit is identity: masking it would merge `page 1` with `page 2`.
fn literal(template: &[u8]) -> Normalized<'_> {
    Normalized {
        template,
        template_hash: fnv1a64(template),
        slots: &[],
        roles: PathRoles::default(),
    }
}
