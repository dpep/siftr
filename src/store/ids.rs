//! Short, copyable ids for stored records: `r42` for runs, `s17` for signals.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunId(pub(crate) i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalId(pub(crate) i64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidId {
    expected: &'static str,
    got: String,
}

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid id {:?} (expected {})", self.got, self.expected)
    }
}

impl std::error::Error for InvalidId {}

/// Accepts `r42` or bare `42`.
fn parse(s: &str, prefix: char, expected: &'static str) -> Result<i64, InvalidId> {
    s.strip_prefix(prefix)
        .unwrap_or(s)
        .parse::<i64>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| InvalidId {
            expected,
            got: s.to_owned(),
        })
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "r{}", self.0)
    }
}

impl FromStr for RunId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse(s, 'r', "a run id like r42").map(RunId)
    }
}

impl fmt::Display for SignalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

impl FromStr for SignalId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse(s, 's', "a signal id like s17").map(SignalId)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_parse_with_or_without_prefix() {
        assert_eq!("r42".parse::<RunId>(), Ok(RunId(42)));
        assert_eq!("42".parse::<RunId>(), Ok(RunId(42)));
        assert_eq!(RunId(42).to_string(), "r42");
        assert_eq!("s7".parse::<SignalId>(), Ok(SignalId(7)));
        for bad in ["s7", "r0", "r-1", "rx", ""] {
            assert!(bad.parse::<RunId>().is_err(), "{bad}");
        }
    }
}
