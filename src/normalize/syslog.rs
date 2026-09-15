//! A BSD syslog header at the start of a line (RFC 3164, as macOS `system.log`
//! and Linux `/var/log/syslog` write it): `Sep 14 10:21:07 host proc[pid]: message`.
//! Only the whole shape counts, so a month word anywhere else stays prose.

use crate::normalize::recognize::{is_time, leading_digits};

/// Offsets into the line.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Header {
    /// End of `Sep 14 10:21:07`.
    pub time_end: usize,
    /// The host starts one space after `time_end`.
    pub host_end: usize,
    /// End of the process name, where its `[pid]` starts; one past the host's
    /// space for syslogd's own `--- last message repeated` line.
    pub proc_end: usize,
}

const MONTHS: [&[u8]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

pub(crate) fn header(line: &[u8], start: usize) -> Option<Header> {
    let t = &line[start..];
    // Almost every line fails on its first bytes.
    if t.len() < 20 || !t[0].is_ascii_uppercase() || t[3] != b' ' || !MONTHS.contains(&&t[..3]) {
        return None;
    }
    // The day is space-padded (`Sep  4`) by the RFC, unpadded by some writers.
    let mut i = 4 + usize::from(t[4] == b' ');
    let digits = leading_digits(&t[i..]);
    let day = t[i..i + digits]
        .iter()
        .fold(0u32, |n, &b| n * 10 + u32::from(b - b'0'));
    if !(1..=2).contains(&digits) || !(1..=31).contains(&day) {
        return None;
    }
    i = space_after(t, i + digits)?;
    let time_len = t[i..].iter().take_while(|&&b| b != b' ').count();
    if !is_time(&t[i..i + time_len], true) {
        return None;
    }
    let time_end = i + time_len;
    i = space_after(t, time_end)?;
    let host_len = t[i..]
        .iter()
        .take_while(|&&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':'))
        .count();
    let host_end = i + host_len;
    if host_len == 0 {
        return None;
    }
    i = space_after(t, host_end)?;
    let proc_end = if t[i..].starts_with(b"--- last message repeated ") {
        i
    } else {
        process_end(t, i)?
    };
    Some(Header {
        time_end: start + time_end,
        host_end: start + host_end,
        proc_end: start + proc_end,
    })
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
                host_end: 21,
                proc_end: 42
            })
        );
        let repeated = b"Oct 14 00:00:00 mbp-a --- last message repeated 2 times ---";
        assert_eq!(header(repeated, 0).map(|h| h.proc_end), Some(22));
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
        ] {
            assert_eq!(header(line.as_bytes(), 0), None, "{line}");
        }
    }
}
