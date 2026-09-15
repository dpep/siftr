//! Behavior identity: what a recurring pattern is, and the stable id that names it across runs.

use std::fmt;
use std::str::FromStr;

use crate::normalize::PathRoles;

/// The semantic kind of an event. Its name is part of every behavior id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    TestExample,
    TestSummary,
    DbQuery,
    HttpRequest,
    Exception,
    Log,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::TestExample,
        Kind::TestSummary,
        Kind::DbQuery,
        Kind::HttpRequest,
        Kind::Exception,
        Kind::Log,
    ];

    /// Hashed into behavior ids and persisted: renaming one orphans every stored behavior of that kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Kind::TestExample => "test.example",
            Kind::TestSummary => "test.summary",
            Kind::DbQuery => "db.query",
            Kind::HttpRequest => "http.request",
            Kind::Exception => "exception",
            Kind::Log => "log",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownKind(pub String);

impl fmt::Display for UnknownKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown behavior kind {:?}", self.0)
    }
}

impl std::error::Error for UnknownKind {}

impl FromStr for Kind {
    type Err = UnknownKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == s)
            .ok_or_else(|| UnknownKind(s.to_owned()))
    }
}

/// Stable across runs, machines and releases: a fixed hash of (kind, template), never `std`'s seeded hasher.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BehaviorId(u64);

impl BehaviorId {
    /// Hex digits shown to humans; any unique prefix of at least 4 resolves.
    pub const SHORT_LEN: usize = 10;

    pub fn of(kind: Kind, template: &[u8]) -> Self {
        let hash = fnv1a(FNV_OFFSET, kind.as_str().as_bytes());
        // Kinds never contain 0x1f, so no (kind, template) pair can alias another.
        let hash = fnv1a(hash, &[0x1f]);
        BehaviorId(fmix64(fnv1a(hash, template)))
    }

    pub fn short(self) -> String {
        let mut id = self.to_string();
        id.truncate(Self::SHORT_LEN);
        id
    }
}

impl fmt::Display for BehaviorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl fmt::Debug for BehaviorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BehaviorId({self})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidBehaviorId(pub String);

impl fmt::Display for InvalidBehaviorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid behavior id {:?} (expected 16 hex digits)",
            self.0
        )
    }
}

impl std::error::Error for InvalidBehaviorId {}

impl FromStr for BehaviorId {
    type Err = InvalidBehaviorId;

    /// Parses the full 16-digit form. Resolving a short prefix needs the store.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let full = s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
        full.then(|| u64::from_str_radix(s, 16).ok())
            .flatten()
            .map(BehaviorId)
            .ok_or_else(|| InvalidBehaviorId(s.to_owned()))
    }
}

/// A recurring pattern. The unit everything user-facing is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behavior {
    pub id: BehaviorId,
    pub kind: Kind,
    /// Lossy UTF-8 of the template bytes; `id` is derived from the bytes.
    pub template: String,
    /// What the paths in its template are. Follows from the template, so it never enters `id`.
    pub roles: PathRoles,
}

impl Behavior {
    pub fn new(kind: Kind, template: &[u8]) -> Self {
        Behavior {
            id: BehaviorId::of(kind, template),
            kind,
            template: String::from_utf8_lossy(template).into_owned(),
            roles: PathRoles::default(),
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn fnv1a(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// MurmurHash3's finalizer: FNV alone leaves the high bits, which short ids show, poorly mixed.
fn fmix64(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    hash ^ (hash >> 33)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn behavior_ids_are_pinned() {
        // If this fails, every stored behavior id just changed. That needs a migration, not a new constant.
        assert_eq!(
            fnv1a(FNV_OFFSET, b"a"),
            0xaf63_dc4c_8601_ec8c,
            "FNV-1a reference vector"
        );
        let id = BehaviorId::of(Kind::Log, b"GET /users/<int> in <duration>");
        assert_eq!(id.to_string(), "43824df88b747f75");
    }

    #[test]
    fn kind_and_template_both_distinguish() {
        let log = BehaviorId::of(Kind::Log, b"x");
        assert_ne!(log, BehaviorId::of(Kind::DbQuery, b"x"));
        assert_ne!(log, BehaviorId::of(Kind::Log, b"y"));
    }

    #[test]
    fn ids_and_kinds_round_trip_through_text() {
        let id = BehaviorId::of(Kind::Exception, b"boom");
        assert_eq!(id.to_string().parse::<BehaviorId>(), Ok(id));
        assert!(id.short().parse::<BehaviorId>().is_err());
        for kind in Kind::ALL {
            assert_eq!(kind.as_str().parse::<Kind>(), Ok(kind));
        }
    }
}
