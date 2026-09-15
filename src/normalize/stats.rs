//! Per-slot value statistics and the evidence rules that classify a slot.
//! Rules and thresholds mirror iriq (`cluster.rs`, `corpus.rs`).

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use crate::normalize::SlotKind;
use crate::normalize::hash::{fnv1a64, mix64};

/// iriq's `DEFAULT_MAX_VALUES_PER_POSITION`.
pub const DEFAULT_MAX_TRACKED_VALUES: usize = 5_000;
const MAX_SAMPLES: usize = 5;

// iriq cluster.rs: ENUM_MIN_OBSERVATIONS, ENUM_MIN_VALUE_COUNT, ENUM_MIN_MEMBERS,
// ENUM_MAX_CARDINALITY, ENUM_MIN_COVERAGE.
const ENUM_MIN_OBSERVATIONS: u64 = 20;
const ENUM_MIN_VALUE_COUNT: u32 = 3;
const ENUM_MIN_MEMBERS: usize = 2;
const ENUM_MAX_CARDINALITY: usize = 10;
const ENUM_MIN_COVERAGE: f64 = 0.9;

// iriq corpus.rs: MIN_OBSERVATIONS_FOR_INFERENCE, LITERAL_UNIQUENESS_THRESHOLD,
// LITERAL_UNIQUENESS_MODERATE_THRESHOLD, MIN_CARDINALITY_FOR_INFERENCE.
const IDENTIFIER_MIN_OBSERVATIONS: u64 = 5;
const IDENTIFIER_UNIQUENESS: f64 = 0.8;
const IDENTIFIER_MODERATE_UNIQUENESS: f64 = 0.5;
const IDENTIFIER_MIN_DISTINCT: usize = 20;

// iriq cluster.rs: CONFIDENCE_SMOOTHING.
const CONFIDENCE_SMOOTHING: f64 = 15.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotClass {
    /// One value ever seen.
    Constant,
    /// A small, well-supported set of values.
    Enum,
    /// Mostly distinct values: ids, keys, timestamps.
    Identifier,
    /// A quantity (`Duration`, `Size`, `Float`) whose distribution matters.
    Measure,
    Unknown,
}

/// Statistics for one slot position of one template. Memory is bounded by the
/// value cap; observing an already-tracked value does not allocate.
#[derive(Debug, Clone)]
pub struct SlotStats {
    kind: SlotKind,
    cap: usize,
    total: u64,
    counts: HashMap<u64, u32, BuildHasherDefault<PreHashed>>,
    overflowed: bool,
    numeric_count: u64,
    min: f64,
    max: f64,
    sum: f64,
    samples: Vec<Box<[u8]>>,
}

impl SlotStats {
    pub fn new(kind: SlotKind) -> Self {
        Self::with_cap(kind, DEFAULT_MAX_TRACKED_VALUES)
    }

    /// `cap` bounds how many distinct values are counted; later new values set
    /// the overflow flag instead.
    pub fn with_cap(kind: SlotKind, cap: usize) -> Self {
        Self {
            kind,
            cap,
            total: 0,
            counts: HashMap::default(),
            overflowed: false,
            numeric_count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            sum: 0.0,
            samples: Vec::new(),
        }
    }

    /// Records one raw slot value (`Slot::text`).
    pub fn observe(&mut self, value: &[u8]) {
        self.total += 1;
        let key = mix64(fnv1a64(value));
        if let Some(n) = self.counts.get_mut(&key) {
            *n = n.saturating_add(1);
        } else if self.counts.len() < self.cap {
            self.counts.insert(key, 1);
            if self.samples.len() < MAX_SAMPLES {
                self.samples.push(value.into());
            }
        } else {
            self.overflowed = true;
        }
        if let Some(v) = crate::normalize::value_f64(self.kind, value) {
            self.numeric_count += 1;
            self.min = self.min.min(v);
            self.max = self.max.max(v);
            self.sum += v;
        }
    }

    pub fn kind(&self) -> SlotKind {
        self.kind
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// Distinct values tracked; a lower bound once `overflowed()`.
    pub fn distinct(&self) -> usize {
        self.counts.len()
    }

    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Observations of `value`, if it is tracked.
    pub fn count_of(&self, value: &[u8]) -> u32 {
        self.counts
            .get(&mix64(fnv1a64(value)))
            .copied()
            .unwrap_or(0)
    }

    /// Values that parsed as numbers (see `slot_value_f64` for units).
    pub fn numeric_count(&self) -> u64 {
        self.numeric_count
    }

    pub fn min(&self) -> Option<f64> {
        (self.numeric_count > 0).then_some(self.min)
    }

    pub fn max(&self) -> Option<f64> {
        (self.numeric_count > 0).then_some(self.max)
    }

    pub fn sum(&self) -> f64 {
        self.sum
    }

    /// The first few distinct raw values, in first-seen order.
    pub fn samples(&self) -> impl Iterator<Item = &[u8]> {
        self.samples.iter().map(|s| &**s)
    }
}

/// Classifies a slot from its evidence. Confidence is `n / (n + 15)`, rounded
/// to two decimals (iriq cluster.rs `param_confidence`).
pub fn classify(stats: &SlotStats) -> (SlotClass, f64) {
    if stats.total == 0 {
        return (SlotClass::Unknown, 0.0);
    }
    let n = stats.total as f64;
    let confidence = (n / (n + CONFIDENCE_SMOOTHING) * 100.0).round() / 100.0;
    let class = if matches!(
        stats.kind,
        SlotKind::Duration | SlotKind::Size | SlotKind::Float
    ) {
        SlotClass::Measure
    } else if stats.overflowed || is_identifier(stats) {
        SlotClass::Identifier
    } else if stats.counts.len() == 1 {
        SlotClass::Constant
    } else if is_enum(stats) {
        SlotClass::Enum
    } else {
        SlotClass::Unknown
    };
    (class, confidence)
}

/// Mirrors iriq cluster.rs `is_enum`: established values (seen at least 3
/// times) number 2..=10 and cover at least 90% of observations.
fn is_enum(stats: &SlotStats) -> bool {
    if stats.total < ENUM_MIN_OBSERVATIONS {
        return false;
    }
    let (mut established, mut covered) = (0usize, 0u64);
    for &n in stats.counts.values() {
        if n >= ENUM_MIN_VALUE_COUNT {
            established += 1;
            covered += u64::from(n);
        }
    }
    (ENUM_MIN_MEMBERS..=ENUM_MAX_CARDINALITY).contains(&established)
        && covered as f64 / stats.total as f64 >= ENUM_MIN_COVERAGE
}

/// Mirrors iriq corpus.rs `high_cardinality_literal_position`, gated on
/// `MIN_OBSERVATIONS_FOR_INFERENCE` as `classify_segment` does.
fn is_identifier(stats: &SlotStats) -> bool {
    if stats.total < IDENTIFIER_MIN_OBSERVATIONS {
        return false;
    }
    let distinct = stats.counts.len();
    let ratio = distinct as f64 / stats.total as f64;
    ratio >= IDENTIFIER_UNIQUENESS
        || (ratio >= IDENTIFIER_MODERATE_UNIQUENESS && distinct >= IDENTIFIER_MIN_DISTINCT)
}

/// Keys are already well-mixed hashes; hashing them again is wasted work.
#[derive(Default)]
struct PreHashed(u64);

impl Hasher for PreHashed {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0 = fnv1a64(bytes);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}
