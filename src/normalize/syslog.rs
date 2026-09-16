//! A timestamp at the start of a line, and the `host proc[pid]:` a syslog line
//! writes after it:
//!
//! - BSD syslog (RFC 3164, macOS `system.log`): `Sep 14 10:21:07 host proc[pid]: message`
//! - ctime (macOS `wifi.log`): `Thu Sep 14 10:21:07.607 [subsys]/pid message`
//! - ISO-8601 (`log show --style syslog`): `2026-09-14 10:21:07.123456-0700 host proc[pid]: message`
//!
//! Only the whole shape counts, so a month or weekday word anywhere else stays
//! prose. A stamp with no weekday needs the `host proc[pid]:` tail to be a
//! header at all: `May 14 10:21:07 deploy started` is a sentence.

use crate::normalize::recognize::{is_date_only, is_time, leading_digits};

/// Offsets into the line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Header {
    /// End of the timestamp, its weekday included.
    pub time_end: usize,
    /// The `host proc[pid]:` after it, when the line has one.
    pub tail: Option<Tail>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Tail {
    /// The host starts one space after the timestamp.
    pub host_end: usize,
    /// End of the process name, where its `[pid]` starts; one past the host's
    /// space for syslogd's own `--- last message repeated` line.
    pub proc_end: usize,
}

const MONTHS: [&[u8]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

const DAYS: [&[u8]; 7] = [b"Mon", b"Tue", b"Wed", b"Thu", b"Fri", b"Sat", b"Sun"];

pub(crate) fn header(line: &[u8], start: usize) -> Option<Header> {
    let t = &line[start..];
    // Almost every line fails on its first bytes; 18 is the shortest header,
    // `Thu Sep 4 10:21:07`.
    if t.len() < 18 {
        return None;
    }
    let (time_end, weekday) = if t[0].is_ascii_digit() {
        (iso(t)?, false)
    } else if t[0].is_ascii_uppercase() {
        ctime(t)?
    } else {
        return None;
    };
    let tail = host_and_process(t, time_end);
    // A bare `Mmm DD HH:MM:SS` or date is prose until the syslog tail confirms
    // it; a weekday is that confirmation, so ctime needs no tail.
    if tail.is_none() && !weekday {
        return None;
    }
    Some(Header {
        time_end: start + time_end,
        tail: tail.map(|t| Tail {
            host_end: start + t.host_end,
            proc_end: start + t.proc_end,
        }),
    })
}

/// `[Www ]Mmm DD HH:MM:SS[.frac]`: where it ends, and whether a weekday opened
/// it. No word is both a weekday and a month, so the two can't be confused.
fn ctime(t: &[u8]) -> Option<(usize, bool)> {
    let weekday = t[3] == b' ' && DAYS.contains(&&t[..3]);
    let m = if weekday { 4 } else { 0 };
    if t[m + 3] != b' ' || !MONTHS.contains(&&t[m..m + 3]) {
        return None;
    }
    // The day is space-padded (`Sep  4`) by the RFC, unpadded by some writers.
    let mut i = m + 4 + usize::from(t[m + 4] == b' ');
    let digits = leading_digits(&t[i..]);
    let day = t[i..i + digits]
        .iter()
        .fold(0u32, |n, &b| n * 10 + u32::from(b - b'0'));
    if !(1..=2).contains(&digits) || !(1..=31).contains(&day) {
        return None;
    }
    i = space_after(t, i + digits)?;
    let end = i + word_len(&t[i..]);
    is_time(&t[i..end], true).then_some((end, weekday))
}

/// `YYYY-MM-DD HH:MM:SS[.frac][±ZZZZ]`, whose date and time are separate words.
fn iso(t: &[u8]) -> Option<usize> {
    if !is_date_only(&t[..10]) {
        return None;
    }
    let i = space_after(t, 10)?;
    let end = i + word_len(&t[i..]);
    is_time(&t[i..end], true).then_some(end)
}

fn host_and_process(t: &[u8], time_end: usize) -> Option<Tail> {
    let i = space_after(t, time_end)?;
    let host_len = t[i..]
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
        .count();
    let host_end = i + host_len;
    if host_len == 0 {
        return None;
    }
    let i = space_after(t, host_end)?;
    let proc_end = if t[i..].starts_with(b"--- last message repeated ") {
        i
    } else {
        process_end(t, i)?
    };
    Some(Tail { host_end, proc_end })
}

fn word_len(t: &[u8]) -> usize {
    t.iter().take_while(|&&b| b != b' ').count()
}

/// The index after a single space at `at`.
fn space_after(t: &[u8], at: usize) -> Option<usize> {
    (t.get(at) == Some(&b' ') && t.get(at + 1) != Some(&b' ')).then_some(at + 1)
}

/// `proc[pid]:` at `at`, where a name may hold single spaces (`Google Chrome Helper`):
/// the index of its `[`.
fn process_end(t: &[u8], at: usize) -> Option<usize> {
    let mut i = at;
    while let Some(&b) = t.get(i) {
        if b == b'[' || !(b.is_ascii_graphic() || b == b' ') || matches!(b, b']' | b':') {
            break;
        }
        if b == b' ' && t.get(i + 1).is_none_or(|&n| matches!(n, b' ' | b'[')) {
            return None;
        }
        i += 1;
    }
    let pid = leading_digits(t.get(i + 1..)?);
    let rest = &t[i + 1 + pid..];
    let ok = i > at
        && t[i] == b'['
        && pid > 0
        && rest.starts_with(b"]:")
        && rest.get(2).is_none_or(|&b| b == b' ');
    ok.then_some(i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans() {
        let line = b"Sep  4 10:21:07 mbp-a Google Chrome Helper[12]: hi";
        assert_eq!(
            header(line, 0),
            Some(Header {
                time_end: 15,
                tail: Some(Tail {
                    host_end: 21,
                    proc_end: 42
                })
            })
        );
        let ctime = b"Thu Sep  4 10:21:07.607 [airport]/637 ready";
        assert_eq!(
            header(ctime, 0),
            Some(Header {
                time_end: 23,
                tail: None
            })
        );
        let iso = b"2026-09-04 10:21:07.123456-0700 mbp-a backupd[1]: hi";
        assert_eq!(
            header(iso, 0),
            Some(Header {
                time_end: 31,
                tail: Some(Tail {
                    host_end: 37,
                    proc_end: 45
                })
            })
        );
        let repeated = b"Oct 14 00:00:00 mbp-a --- last message repeated 2 times ---";
        assert_eq!(
            header(repeated, 0).and_then(|h| h.tail).map(|t| t.proc_end),
            Some(22)
        );
    }

    #[test]
    fn rejects_near_misses() {
        for line in [
            "Sep 14 10:21:07 mbp-a backupd: no pid",
            "Sep 14 10:21:07 mbp-a backupd[]: empty pid",
            "Sep 14 10:21:07 mbp-a backupd[1]:glued",
            "Sep 14 10:21:07 mbp-a backupd [1]: space before pid",
            "Sep 14 10:21:07  mbp-a backupd[1]: two spaces",
            "Sep 0 10:21:07 mbp-a backupd[1]: day zero",
            "Sep 123 10:21:07 mbp-a backupd[1]: three digit day",
            "Sep 14 24:00:00 mbp-a backupd[1]: bad hour",
            "Sep 14 10:21:07 mbp-a [1]: no name",
            "SEP 14 10:21:07 mbp-a backupd[1]: shouted",
            "Sep 14",
            // A stamp with no tail is only a header when a weekday opens it.
            "Sep 14 10:21:07 deploy started",
            "2026-09-14 10:21:07 deploy started",
            "2026-13-01 10:21:07 mbp-a backupd[1]: bad month",
            "2026-09-14 10:21:07.123456-0700  0x1a2b  Default  0x0  1  0  backupd: columns",
            // A leading word that only spells a weekday.
            "Sat down and waited for the build",
            "Sun Jan brochure printed twice",
            "Thu Sep 14 10:21 no seconds here",
            "Thu Sep 44 10:21:07 [airport]/1 bad day",
            "Thu 14 Sep 10:21:07 [airport]/1 wrong order",
            "Thur Sep 14 10:21:07 [airport]/1 four letters",
            "thu Sep 14 10:21:07 [airport]/1 lowercase",
        ] {
            assert_eq!(header(line.as_bytes(), 0), None, "{line}");
        }
    }
}
