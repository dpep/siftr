//! Recognizers for one token. Each is a pure byte check; the caller takes the
//! first match, so two recognizers can never claim overlapping bytes.

use crate::normalize::SlotKind;

pub(crate) fn recognize(t: &[u8]) -> Option<SlotKind> {
    let &first = t.first()?;
    if first == b'-' {
        // A sign only on a number: `-20.0` is a value, `--format` is a flag.
        return match t.get(1) {
            Some(d) if d.is_ascii_digit() => number(&t[1..]),
            _ => None,
        };
    }
    if first.is_ascii_digit() {
        if let Some(kind) = number(t) {
            return Some(kind);
        }
        if first == b'0' && t.len() > 2 && t[1] | 0x20 == b'x' && all_hex(&t[2..]) {
            return Some(SlotKind::Hex);
        }
        if is_timestamp(t) {
            return Some(SlotKind::Timestamp);
        }
        if is_ipv4(t) {
            return Some(SlotKind::Ip);
        }
        if is_version(t, false) {
            return Some(SlotKind::Version);
        }
    } else if first | 0x20 == b'v' && is_version(&t[1..], true) {
        return Some(SlotKind::Version);
    }
    if is_uuid(t) {
        return Some(SlotKind::Uuid);
    }
    if t.contains(&b'@') {
        return is_email(t).then_some(SlotKind::Email);
    }
    if is_ipv6(t) {
        return Some(SlotKind::Ip);
    }
    if is_hex(t) {
        return Some(SlotKind::Hex);
    }
    None
}

/// `123`, `1.5`, or either with an attached unit (`0.3ms`, `1481KB`).
fn number(t: &[u8]) -> Option<SlotKind> {
    let mut i = leading_digits(t);
    if i == 0 {
        return None;
    }
    let mut float = false;
    if t.get(i) == Some(&b'.') {
        let frac = leading_digits(&t[i + 1..]);
        if frac > 0 {
            i += 1 + frac;
            float = true;
        }
    }
    let unit = &t[i..];
    if unit.is_empty() {
        Some(if float {
            SlotKind::Float
        } else {
            SlotKind::Int
        })
    } else if duration_ms(unit).is_some() {
        Some(SlotKind::Duration)
    } else if size_bytes(unit).is_some() {
        Some(SlotKind::Size)
    } else {
        None
    }
}

/// Milliseconds per unit.
pub(crate) fn duration_ms(unit: &[u8]) -> Option<f64> {
    Some(match unit {
        b"ns" => 1e-6,
        b"us" => 1e-3,
        b"ms" | b"millisecond" | b"milliseconds" => 1.0,
        b"s" | b"sec" | b"secs" | b"second" | b"seconds" => 1e3,
        b"min" | b"mins" | b"minute" | b"minutes" => 6e4,
        b"h" | b"hour" | b"hours" => 3.6e6,
        _ => return None,
    })
}

/// Bytes per unit. Binary multiples, as Rails' `number_to_human_size` uses.
pub(crate) fn size_bytes(unit: &[u8]) -> Option<f64> {
    const K: f64 = 1024.0;
    Some(match unit {
        b"B" | b"byte" | b"bytes" => 1.0,
        b"K" | b"KB" | b"kB" | b"kb" | b"KiB" => K,
        b"M" | b"MB" | b"mb" | b"MiB" => K * K,
        b"G" | b"GB" | b"gb" | b"GiB" => K * K * K,
        b"TB" | b"TiB" => K * K * K * K,
        _ => return None,
    })
}

/// A unit written after a space (`10.9 seconds`, `512 bytes`). Single letters
/// and `us`/`ns` read as ordinary words there, so they don't count.
pub(crate) fn spaced_unit(word: &[u8]) -> Option<SlotKind> {
    if word.len() < 2 || word == b"us" || word == b"ns" {
        None
    } else if duration_ms(word).is_some() {
        Some(SlotKind::Duration)
    } else if size_bytes(word).is_some() {
        Some(SlotKind::Size)
    } else {
        None
    }
}

/// `2024-01-15`, `2024-01-15T10:00[:00[.123]][Z|+05:30]`, or a bare `10:00:00[.123]`.
pub(crate) fn is_timestamp(t: &[u8]) -> bool {
    if is_date(t) {
        return match t.get(10) {
            None => true,
            Some(b'T') => is_time(&t[11..], false),
            Some(_) => false,
        };
    }
    is_time(t, true)
}

pub(crate) fn is_date_only(t: &[u8]) -> bool {
    t.len() == 10 && is_date(t)
}

/// `HH:MM[:SS[.frac]]` plus an optional zone, covering all of `t`.
pub(crate) fn is_time(t: &[u8], need_seconds: bool) -> bool {
    time_len(t, need_seconds).is_some_and(|n| n + zone_len(&t[n..]) == t.len())
}

/// A zone written as its own word after a time: `+0000`, `-05:00`, `UTC`.
pub(crate) fn is_zone_word(w: &[u8]) -> bool {
    w == b"UTC" || w == b"GMT" || (!w.is_empty() && zone_len(w) == w.len())
}

fn is_date(t: &[u8]) -> bool {
    t.len() >= 10
        && t[4] == b'-'
        && t[7] == b'-'
        && all_digits(&t[..4])
        && two_digits(&t[5..7]).is_some_and(|m| (1..=12).contains(&m))
        && two_digits(&t[8..10]).is_some_and(|d| (1..=31).contains(&d))
}

fn time_len(t: &[u8], need_seconds: bool) -> Option<usize> {
    if t.len() < 5 || t[2] != b':' || two_digits(&t[..2])? > 23 || two_digits(&t[3..5])? > 59 {
        return None;
    }
    if t.len() < 8 || t[5] != b':' {
        return (!need_seconds).then_some(5);
    }
    if two_digits(&t[6..8])? > 60 {
        return None;
    }
    let frac = if t.get(8) == Some(&b'.') {
        leading_digits(&t[9..])
    } else {
        0
    };
    Some(if frac > 0 { 9 + frac } else { 8 })
}

fn zone_len(t: &[u8]) -> usize {
    match t.first() {
        Some(b'Z') => 1,
        Some(b'+' | b'-') => {
            let d = &t[1..];
            if d.len() >= 5 && d[2] == b':' && all_digits(&d[..2]) && all_digits(&d[3..5]) {
                6
            } else if d.len() >= 4 && all_digits(&d[..4]) {
                5
            } else if d.len() >= 2 && all_digits(&d[..2]) {
                3
            } else {
                0
            }
        }
        _ => 0,
    }
}

fn is_ipv4(t: &[u8]) -> bool {
    let mut parts = 0;
    for part in t.split(|&b| b == b'.') {
        parts += 1;
        if parts > 4 || part.is_empty() || part.len() > 3 || !all_digits(part) {
            return false;
        }
        let octet = part
            .iter()
            .fold(0u16, |acc, &b| acc * 10 + u16::from(b - b'0'));
        if octet > 255 {
            return false;
        }
    }
    parts == 4
}

/// `1.2.3`, `1.2.3-beta.1`; with a `v` prefix, `v1.2` is enough. A bare `v1`
/// stays literal so `/api/v1` and `/api/v2` remain distinct routes.
fn is_version(t: &[u8], prefixed: bool) -> bool {
    let mut i = leading_digits(t);
    if i == 0 {
        return false;
    }
    let mut dots = 0;
    while t.get(i) == Some(&b'.') {
        let d = leading_digits(&t[i + 1..]);
        if d == 0 {
            break;
        }
        i += 1 + d;
        dots += 1;
    }
    if dots < if prefixed { 1 } else { 2 } {
        return false;
    }
    let rest = &t[i..];
    rest.is_empty()
        || (rest.len() > 1
            && matches!(rest[0], b'-' | b'+' | b'.')
            && rest[1].is_ascii_alphanumeric()
            && rest[1..]
                .iter()
                .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+')))
}

pub(crate) fn is_uuid(t: &[u8]) -> bool {
    t.len() == 36
        && t.iter().enumerate().all(|(i, &b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn is_email(t: &[u8]) -> bool {
    let Some(at) = t.iter().position(|&b| b == b'@') else {
        return false;
    };
    let (local, domain) = (&t[..at], &t[at + 1..]);
    if local.is_empty()
        || !local
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    {
        return false;
    }
    let mut labels = 0;
    let mut tld: &[u8] = &[];
    for label in domain.split(|&b| b == b'.') {
        if label.is_empty()
            || !label
                .iter()
                .all(|&b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return false;
        }
        labels += 1;
        tld = label;
    }
    labels >= 2 && tld.len() >= 2 && tld.iter().all(u8::is_ascii_alphabetic)
}

fn is_ipv6(t: &[u8]) -> bool {
    let (mut colons, mut group) = (0, 0);
    let (mut digit, mut upper, mut lower) = (false, false, false);
    for &b in t {
        match b {
            b':' => {
                colons += 1;
                group = 0;
                continue;
            }
            b'0'..=b'9' => digit = true,
            b'a'..=b'f' => lower = true,
            b'A'..=b'F' => upper = true,
            _ => return false,
        }
        group += 1;
        if group > 4 {
            return false;
        }
    }
    let compressed = t.windows(2).any(|w| w == b"::");
    // Mixed case reads as a Ruby constant path (`Abc::Def1`), not an address.
    digit && !(upper && lower) && (colons == 7 || (compressed && (2..=7).contains(&colons)))
}

/// Needs a digit and a letter: `deadbeef`/`facade` are words and all-digit runs
/// are numbers, so neither flips a template to `<hex>`.
fn is_hex(t: &[u8]) -> bool {
    t.len() >= 7
        && all_hex(t)
        && t.iter().any(u8::is_ascii_digit)
        && t.iter().any(u8::is_ascii_alphabetic)
}

pub(crate) fn leading_digits(t: &[u8]) -> usize {
    t.iter().take_while(|b| b.is_ascii_digit()).count()
}

fn all_digits(t: &[u8]) -> bool {
    t.iter().all(u8::is_ascii_digit)
}

fn all_hex(t: &[u8]) -> bool {
    t.iter().all(u8::is_ascii_hexdigit)
}

fn two_digits(t: &[u8]) -> Option<u8> {
    (t.len() == 2 && all_digits(t)).then(|| (t[0] - b'0') * 10 + (t[1] - b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use SlotKind::*;

    fn kind(t: &str) -> Option<SlotKind> {
        recognize(t.as_bytes())
    }

    #[test]
    fn numbers_and_units() {
        assert_eq!(kind("42"), Some(Int));
        assert_eq!(kind("-20.0"), Some(Float));
        assert_eq!(kind("0.3ms"), Some(Duration));
        assert_eq!(kind("5s"), Some(Duration));
        assert_eq!(kind("1481KB"), Some(Size));
        assert_eq!(kind("12MB"), Some(Size));
        assert_eq!(kind("--format"), None);
        assert_eq!(kind("5th"), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(kind("2024-01-15"), Some(Timestamp));
        assert_eq!(kind("2026-09-13T12:00:00.000Z"), Some(Timestamp));
        assert_eq!(kind("2024-01-15T10:00:00+05:30"), Some(Timestamp));
        assert_eq!(kind("2024-01-15T10:00Z"), Some(Timestamp));
        assert_eq!(kind("10:00:00.123"), Some(Timestamp));
        assert_eq!(kind("2024-13-01"), None);
        assert_eq!(kind("25:00:00"), None);
    }

    #[test]
    fn epoch_seconds_stay_int() {
        // iriq calls 10/13-digit ints timestamps; here that would flip an id
        // column's template when its values cross 1e9.
        assert_eq!(kind("1700000000"), Some(Int));
    }

    #[test]
    fn network() {
        assert_eq!(kind("10.0.24.37"), Some(Ip));
        assert_eq!(kind("255.255.255.255"), Some(Ip));
        assert_eq!(kind("256.1.1.1"), Some(Version));
        assert_eq!(kind("::1"), Some(Ip));
        assert_eq!(kind("fe80::1ff:fe23:4567:890a"), Some(Ip));
        assert_eq!(kind("2001:0db8:85a3:0000:0000:8a2e:0370:7334"), Some(Ip));
        // MAC-shaped (6 groups, no `::`) is not IPv6.
        assert_eq!(kind("00:1a:2b:3c:4d:5e"), None);
        assert_eq!(kind("Abc::Def1"), None);
    }

    #[test]
    fn identifiers() {
        assert_eq!(kind("6513270e-269e-0d37-f2a7-4de452e6b438"), Some(Uuid));
        assert_eq!(kind("6595e60af5"), Some(Hex));
        assert_eq!(kind("0x00007f8b1c8a2b10"), Some(Hex));
        assert_eq!(kind("deadbeef"), None);
        assert_eq!(kind("facade"), None);
        assert_eq!(kind("abc123"), None);
        assert_eq!(kind("1234567"), Some(Int));
    }

    #[test]
    fn emails() {
        assert_eq!(kind("alice@example.com"), Some(Email));
        assert_eq!(kind("a.b+tag@mail.example.co"), Some(Email));
        assert_eq!(kind("user@localhost"), None);
        assert_eq!(kind("@example.com"), None);
    }

    #[test]
    fn versions() {
        assert_eq!(kind("v8.1.0"), Some(Version));
        assert_eq!(kind("v1.2"), Some(Version));
        assert_eq!(kind("1.2.3"), Some(Version));
        assert_eq!(kind("7.1.0-beta.1"), Some(Version));
        assert_eq!(kind("v1"), None);
        assert_eq!(kind("1.2"), Some(Float));
    }
}
