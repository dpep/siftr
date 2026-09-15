//! Credentials in a line, found in one pass without allocating, and replaced by placeholders numbered per
//! run (`<TOKEN_1>`, the same number for the same value) before anything is stored.
//!
//! The rules follow launder's secret detectors (`src/detect/secrets.rs`: token prefixes, JWTs, private-key
//! blocks, `Authorization:` values, URL passwords, cookie values, high-entropy values under a credential key),
//! plus shapes it still misses: JSON-escaped keys (JSON inside an RSpec event), Rails SQL binds, `sk-proj-` style
//! tokens and credentials split across a `Bearer`-less quoted header. Keys are checked at their separator, not at
//! every word, which keeps a clean line cheap.
//!
//! [`pii`] adds emails, public IPs and home-directory prefixes. It is for stored evidence only, never templates,
//! so behavior ids don't depend on it.

use std::borrow::Cow;
use std::collections::HashMap;
use std::net::Ipv6Addr;

use super::hash::fnv1a64;
use super::skip_ansi;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Token,
    Jwt,
    Password,
    Secret,
    PrivateKey,
    Email,
    Ip,
    /// A home directory prefix, written as `~`.
    Home,
}

impl Kind {
    const ALL: [Kind; 8] = [
        Kind::Token,
        Kind::Jwt,
        Kind::Password,
        Kind::Secret,
        Kind::PrivateKey,
        Kind::Email,
        Kind::Ip,
        Kind::Home,
    ];

    /// The placeholder's name: `<TOKEN_1>`.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Token => "TOKEN",
            Kind::Jwt => "JWT",
            Kind::Password => "PASSWORD",
            Kind::Secret => "SECRET",
            Kind::PrivateKey => "PRIVATE_KEY",
            Kind::Email => "EMAIL",
            Kind::Ip => "IP",
            Kind::Home => "HOME",
        }
    }
}

/// `[start, end)` of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

/// What stored evidence (raw captures, kept lines) keeps. Templates mask credentials under every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Secrets,
    /// Credentials, emails, public IPs and home-directory prefixes.
    Pii,
    /// Raw captures. Kept lines stay masked: they come from what templates are built from.
    Off,
}

impl Mode {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "secrets" => Some(Mode::Secrets),
            "pii" => Some(Mode::Pii),
            "off" => Some(Mode::Off),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Secrets => "secrets",
            Mode::Pii => "pii",
            Mode::Off => "off",
        }
    }
}

/// Lines a private key block may span before the scanner stops masking, so a block that never ends can't
/// swallow the rest of a stream. A 4096-bit key is about 50 lines.
const MAX_KEY_LINES: u32 = 100;
/// launder's keyed floor: shorter values stay readable (`locale=en`).
const MIN_VALUE: usize = 8;

/// One stream's scanning state: a private key block spans lines.
#[derive(Debug, Clone, Default)]
pub struct Scanner {
    key_lines: u32,
}

const W: u16 = 1; // regex \w, ASCII
const B64: u16 = 2; // [A-Za-z0-9_-]
const ALNUM: u16 = 4;
const UPNUM: u16 = 8; // [A-Z0-9]
const AUTHV: u16 = 16; // [A-Za-z0-9._\-+/=~]
const KEYC: u16 = 32; // [A-Za-z0-9_.-]
const SPACE: u16 = 64;
const VSTOP: u16 = 128; // ends an unquoted value
const SLACK: u16 = 512; // [A-Za-z0-9-]
const LOCAL: u16 = 1024; // email local part: [A-Za-z0-9._%+-]
const TRIG: u16 = 2048; // where a rule can be recognized: a separator, or a token prefix's marker byte

static T: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        let mut c = 0;
        if b.is_ascii_alphanumeric() {
            c |= W | B64 | ALNUM | AUTHV | KEYC | SLACK | LOCAL;
        }
        if b.is_ascii_uppercase() || b.is_ascii_digit() {
            c |= UPNUM;
        }
        if b == b'_' {
            c |= W | B64 | AUTHV | KEYC | LOCAL;
        }
        if b == b'-' {
            c |= B64 | AUTHV | KEYC | SLACK | LOCAL;
        }
        if b == b'.' {
            c |= AUTHV | KEYC | LOCAL;
        }
        if matches!(b, b'+' | b'/' | b'=' | b'~') {
            c |= AUTHV;
        }
        if matches!(b, b'%' | b'+') {
            c |= LOCAL;
        }
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c) {
            c |= SPACE | VSTOP;
        }
        if matches!(
            b,
            b',' | b';' | b'"' | b'\'' | b'\\' | b'&' | b'<' | b'>' | b'|' | 0x1b
        ) {
            c |= VSTOP;
        }
        if matches!(
            b,
            b':' | b'=' | b',' | b'-' | b'_' | b'.' | b'J' | b'K' | b'S' | b'I'
        ) {
            c |= TRIG;
        }
        t[i] = c;
        i += 1;
    }
    t
};

#[inline]
fn is(b: u8, class: u16) -> bool {
    T[b as usize] & class != 0
}

fn run(l: &[u8], from: usize, class: u16) -> usize {
    l.get(from..)
        .map_or(0, |rest| rest.iter().take_while(|&&b| is(b, class)).count())
}

fn run_until(l: &[u8], from: usize, stop: impl Fn(u8) -> bool) -> usize {
    l.get(from..)
        .map_or(0, |rest| rest.iter().take_while(|&&b| !stop(b)).count())
}

/// Regex `\b` at `p`.
fn boundary(l: &[u8], p: usize) -> bool {
    let before = p > 0 && is(l[p - 1], W);
    let after = p < l.len() && is(l[p], W);
    before != after
}

/// `class{min,max}\b` from `from`, backtracking as a regex would: the longest length ending on a boundary.
fn word_end(l: &[u8], from: usize, class: u16, min: usize, max: usize) -> Option<usize> {
    let most = run(l, from, class).min(max);
    (min..=most)
        .rev()
        .find(|&k| boundary(l, from + k))
        .map(|k| from + k)
}

fn skip_space(l: &[u8], mut p: usize) -> usize {
    while p < l.len() && is(l[p], SPACE) {
        p += 1;
    }
    p
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn span(start: usize, end: usize, kind: Kind) -> Span {
    Span { start, end, kind }
}

impl Scanner {
    /// Appends the credentials in `l` (one line, no terminator) to `out`, left to right, non-overlapping.
    pub fn secrets(&mut self, l: &[u8], out: &mut Vec<Span>) {
        let n = l.len();
        let mut i = 0;
        if self.key_lines > 0 {
            match key_end(l, 0) {
                Some(end) => {
                    self.key_lines = 0;
                    out.push(span(0, end, Kind::PrivateKey));
                    i = end;
                }
                None => {
                    self.key_lines -= 1;
                    if n > 0 {
                        out.push(span(0, n, Kind::PrivateKey));
                    }
                    return;
                }
            }
        }
        // Most bytes cost one table lookup: this loop is most of the cost of a clean line, so the rules are
        // `inline(never)` and a token prefix is found from the marker byte it carries (`_` of `ghp_`), not by
        // checking every word.
        let mut floor = i;
        while i < n {
            let b = l[i];
            if !is(b, TRIG) {
                i += 1;
                continue;
            }
            let hit = match b {
                b':' if l[i + 1..].starts_with(b"//") => url_password(l, i, out),
                b'=' | b':' | b',' if may_close_key(l, i) => keyed(l, i, out),
                b'-' if l.get(i + 1) == Some(&b'-') && l[i..].starts_with(b"-----BEGIN") => {
                    self.private_key(l, i, out)
                }
                _ => match prefix_start(l, i) {
                    Some(start) if start >= floor && word_starts(l, start) => {
                        token(l, start).map(|(end, kind)| {
                            out.push(span(start, end, kind));
                            end
                        })
                    }
                    _ => None,
                },
            };
            match hit {
                Some(end) => {
                    i = end.max(i + 1);
                    floor = i;
                }
                None => i += 1,
            }
        }
    }

    #[inline(never)]
    fn private_key(&mut self, l: &[u8], i: usize, out: &mut Vec<Span>) -> Option<usize> {
        let label = i + b"-----BEGIN".len();
        let close = label + find(&l[label..], b"-----")?;
        if !l[label..close].ends_with(b"PRIVATE KEY") {
            return None;
        }
        let body = close + 5;
        let end = match key_end(l, body) {
            Some(end) => end,
            // Inside a JSON string (`\n`-escaped), the block ends with the string, not on a later line.
            None => match (body..l.len()).find(|&j| l[j] == b'"' && l[j - 1] != b'\\') {
                Some(stop) => stop,
                None => {
                    self.key_lines = MAX_KEY_LINES;
                    l.len()
                }
            },
        };
        out.push(span(i, end, Kind::PrivateKey));
        Some(end)
    }
}

/// Past the `-----END … PRIVATE KEY-----` at or after `from`.
fn key_end(l: &[u8], from: usize) -> Option<usize> {
    let label = from + find(&l[from..], b"-----END")? + b"-----END".len();
    let close = label + find(&l[label..], b"-----")?;
    l[label..close]
        .ends_with(b"PRIVATE KEY")
        .then_some(close + 5)
}

/// A known token prefix, or a JWT, starting at word start `i`.
#[inline(never)]
fn token(l: &[u8], i: usize) -> Option<(usize, Kind)> {
    let r = &l[i..];
    let end = match r[0] {
        b'g' if r.starts_with(b"github_pat_") => word_end(l, i + 11, W, 20, usize::MAX),
        b'g' if r.len() > 3
            && r[1] == b'h'
            && matches!(r[2], b'p' | b'o' | b'u' | b's' | b'r')
            && r[3] == b'_' =>
        {
            word_end(l, i + 4, ALNUM, 16, usize::MAX)
        }
        b'g' if r.starts_with(b"glpat-") => word_end(l, i + 6, B64, 20, usize::MAX),
        // `sk-proj-…` and `sk-ant-…` carry dashes, which launder's `sk-[A-Za-z0-9]{20,}` stops at.
        b's' if r.starts_with(b"sk-") => word_end(l, i + 3, B64, 20, usize::MAX),
        b's' | b'p' | b'r' if r.len() > 3 && r[1] == b'k' && r[2] == b'_' => {
            let live = r[3..].starts_with(b"live_");
            let test = r[0] != b'p' && r[3..].starts_with(b"test_");
            if live || test {
                word_end(l, i + 8, ALNUM, 16, usize::MAX)
            } else {
                None
            }
        }
        b'A' if r.starts_with(b"AKIA") || r.starts_with(b"ASIA") => {
            word_end(l, i + 4, UPNUM, 16, 16)
        }
        b'A' if r.starts_with(b"AIza") => word_end(l, i + 4, B64, 35, 35),
        b'x' if r.len() > 4
            && r.starts_with(b"xox")
            && matches!(r[3], b'b' | b'a' | b'p' | b'r' | b's')
            && r[4] == b'-' =>
        {
            word_end(l, i + 5, SLACK, 10, usize::MAX)
        }
        b'n' if r.starts_with(b"npm_") => word_end(l, i + 4, ALNUM, 36, 36),
        b'S' if r.starts_with(b"SG.") => sendgrid(l, i + 3),
        b'e' if r.starts_with(b"eyJ") => return jwt(l, i + 3).map(|end| (end, Kind::Jwt)),
        _ => None,
    };
    end.map(|end| (end, Kind::Token))
}

fn sendgrid(l: &[u8], from: usize) -> Option<usize> {
    if run(l, from, B64) != 22 || l.get(from + 22) != Some(&b'.') {
        return None;
    }
    word_end(l, from + 23, B64, 43, 43)
}

fn jwt(l: &[u8], from: usize) -> Option<usize> {
    let a = from + run(l, from, B64);
    if a == from || l.get(a) != Some(&b'.') {
        return None;
    }
    let b = a + 1 + run(l, a + 1, B64);
    if b == a + 1 || l.get(b) != Some(&b'.') {
        return None;
    }
    word_end(l, b + 1, B64, 1, usize::MAX)
}

/// The password of `scheme://user:password@`, the user possibly empty (`redis://:password@`).
#[inline(never)]
fn url_password(l: &[u8], colon: usize, out: &mut Vec<Span>) -> Option<usize> {
    let scheme = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-');
    let s = l[..colon].iter().rev().take_while(|&&b| scheme(b)).count();
    if !l[colon - s..colon].iter().any(u8::is_ascii_alphabetic) {
        return None;
    }
    let part = |from: usize, stop: &[u8]| run_until(l, from, |b| stop.contains(&b) || is(b, SPACE));
    let sep = colon + 3 + part(colon + 3, b":/@'\"\\");
    if l.get(sep) != Some(&b':') {
        return None;
    }
    let end = sep + 1 + part(sep + 1, b"/@'\"\\");
    if end == sep + 1 || l.get(end) != Some(&b'@') || is_marker(&l[sep + 1..end]) {
        return None;
    }
    out.push(span(sep + 1, end, Kind::Password));
    Some(end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    /// `Authorization`: an optional scheme, then the credential, however short.
    Auth,
    /// `Cookie`, `Set-Cookie`: each pair's value, opaque, so no entropy floor.
    Cookie,
    /// A high-entropy value. `pwd` is also the shell's working directory, whose value is a path.
    Keyed { pwd: bool },
}

/// The credential after the separator at `sep` (`key=`, `key:`, `"key"=>`, `"key":`, `\"key\":` for JSON inside
/// JSON, or a Rails bind `["key", `), pushed to `out`. Where scanning resumes.
/// Where a token prefix would start if `l[i]` is its marker byte: `ghp_` at its `_`, `eyJ` at its `J`, `AKIA` at
/// its `K`. [`token`] checks the rest.
#[inline(always)]
fn prefix_start(l: &[u8], i: usize) -> Option<usize> {
    let before = |lit: &[u8]| i >= lit.len() && &l[i - lit.len()..i] == lit;
    let start = match l[i] {
        b'_' if i >= 3
            && l[i - 3] == b'g'
            && l[i - 2] == b'h'
            && matches!(l[i - 1], b'p' | b'o' | b'u' | b's' | b'r') =>
        {
            i - 3
        }
        b'_' if before(b"npm") => i - 3,
        b'_' if i >= 2 && l[i - 1] == b'k' && matches!(l[i - 2], b's' | b'p' | b'r') => i - 2,
        b'_' if before(b"github") => i - 6,
        b'-' if before(b"sk") => i - 2,
        b'-' if before(b"glpat") => i - 5,
        b'-' if i >= 4 && &l[i - 4..i - 1] == b"xox" => i - 4,
        b'.' if before(b"SG") => i - 2,
        b'J' if before(b"ey") => i - 2,
        b'K' | b'S' | b'I' if before(b"A") => i - 1,
        _ => return None,
    };
    Some(start)
}

/// A regex `\b` before `start`, where an ANSI escape's final letter counts as one too: `\e[31mghp_…`.
fn word_starts(l: &[u8], start: usize) -> bool {
    if start == 0 || !is(l[start - 1], W) {
        return true;
    }
    let mut k = start - 1;
    while k > 0 && matches!(l[k - 1], b'0'..=b'9' | b';') {
        k -= 1;
    }
    k >= 2 && l[k - 1] == b'[' && l[k - 2] == 0x1b
}

/// A cheap first look at a separator, before [`keyed`]'s call: most separators follow a number or a word no
/// credential word ends like (`10:59`, `Views:`), and a bind's comma always follows a quote.
#[inline(always)]
fn may_close_key(l: &[u8], sep: usize) -> bool {
    let mut k = sep;
    if l[sep] == b',' {
        return k > 0 && l[k - 1] == b'"';
    }
    while k > 0 && l[k - 1] == b' ' {
        k -= 1;
    }
    if k > 0 && matches!(l[k - 1], b'"' | b'\'') {
        k -= 1;
        if k > 0 && l[k - 1] == b'\\' {
            k -= 1;
        }
    }
    k > 0
        && matches!(
            l[k - 1] | 0x20,
            b'd' | b'e' | b't' | b'n' | b'h' | b'y' | b'l' | b's'
        )
}

#[inline(never)]
fn keyed(l: &[u8], sep: usize, out: &mut Vec<Span>) -> Option<usize> {
    let sep_byte = l[sep];
    let next = l.get(sep + 1).copied();
    let before = sep.checked_sub(1).map(|p| l[p]);
    match sep_byte {
        b':' if next == Some(b':') || before == Some(b':') => return None,
        b'=' if next == Some(b'=') || matches!(before, Some(b'=' | b'!' | b'<' | b'>')) => {
            return None;
        }
        // Only a bind's quoted key ends right before its comma.
        b',' if before != Some(b'"') => return None,
        _ => {}
    }
    let mut k = sep;
    if sep_byte != b',' {
        while k > 0 && is(l[k - 1], SPACE) {
            k -= 1;
        }
    }
    let quoted = k > 0 && matches!(l[k - 1], b'"' | b'\'');
    if quoted {
        k -= 1;
        if k > 0 && l[k - 1] == b'\\' {
            k -= 1;
        }
    }
    let key_end = k;
    // Every credential word ends in one of these letters; a time (`10:59`) or most keys end otherwise.
    if key_end == 0
        || !matches!(
            l[key_end - 1] | 0x20,
            b'd' | b'e' | b't' | b'n' | b'h' | b'y' | b'l' | b's'
        )
    {
        return None;
    }
    while k > 0 && is(l[k - 1], KEYC) {
        k -= 1;
    }
    if key_end - k < 3 || (sep_byte == b',' && !(quoted && opens_bind(l, k))) {
        return None;
    }
    let rule = credential_key(&l[after_escape(l, k, key_end)..key_end])?;
    let rocket = sep_byte == b'=' && next == Some(b'>');
    value(l, skip_space(l, sep + 1 + usize::from(rocket)), rule, out)
}

/// Whether the quoted key ending just before `k` opens a bind: `["` or, JSON-escaped, `[\"`.
fn opens_bind(l: &[u8], k: usize) -> bool {
    let mut j = k;
    if j == 0 || l[j - 1] != b'"' {
        return false;
    }
    j -= 1;
    if j > 0 && l[j - 1] == b'\\' {
        j -= 1;
    }
    j > 0 && l[j - 1] == b'['
}

/// Walking back over key bytes from a separator also takes an escape's parameters and final letter
/// (`\e[1mpassword`): start after the escape instead.
fn after_escape(l: &[u8], start: usize, end: usize) -> usize {
    let mut j = start;
    while j > 0 && (l[j - 1].is_ascii_digit() || l[j - 1] == b';') {
        j -= 1;
    }
    if j >= 2 && l[j - 1] == b'[' && l[j - 2] == 0x1b {
        let past = skip_ansi(l, j - 2);
        if past > start && past <= end {
            return past;
        }
    }
    start
}

/// launder's credential words, as the key's last words: a prefix is fine (`access_token`, `refreshToken`,
/// `RAILS_MASTER_KEY`), a suffix is not (`token_type`, `tokenizer`), and bare `key` never is (`sort_key`).
fn credential_key(key: &[u8]) -> Option<Rule> {
    let (last, rest) = last_word(key);
    let eq = |w: &[u8], s: &[u8]| w.eq_ignore_ascii_case(s);
    let before = |rest: &[u8], s: &[u8]| eq(last_word(rest).0, s);
    const KEY_OF: [&[u8]; 8] = [
        b"api",
        b"access",
        b"private",
        b"secret",
        b"signing",
        b"encryption",
        b"master",
        b"client",
    ];
    // Length first: almost every key a log line carries (`id`, `created_at`, `LIMIT`) fails there.
    let keyed = match last.len() {
        3 if eq(last, b"pwd") => return Some(Rule::Keyed { pwd: true }),
        3 if eq(last, b"key") => {
            let word = last_word(rest).0;
            KEY_OF.iter().any(|s| eq(word, s))
        }
        4 if eq(last, b"base") => {
            let (word, rest) = last_word(rest);
            eq(word, b"key") && before(rest, b"secret")
        }
        4 => eq(last, b"auth"),
        5 => eq(last, b"token"),
        6 if eq(last, b"cookie") => return Some(Rule::Cookie),
        6 => eq(last, b"secret") || eq(last, b"passwd") || eq(last, b"apikey"),
        8 => eq(last, b"password"),
        9 => eq(last, b"accesskey") || eq(last, b"secretkey"),
        10 => eq(last, b"passphrase") || eq(last, b"privatekey") || eq(last, b"credential"),
        11 => eq(last, b"credentials"),
        12 => eq(last, b"confirmation") && before(rest, b"password"),
        13 if eq(last, b"authorization") => return Some(Rule::Auth),
        _ => false,
    };
    keyed.then_some(Rule::Keyed { pwd: false })
}

/// A key's last word and what precedes it, split at `_ - .` and camel case: `X-Api-Key` and `xApiKey` end in `Key`.
fn last_word(key: &[u8]) -> (&[u8], &[u8]) {
    let end = key
        .iter()
        .rposition(u8::is_ascii_alphanumeric)
        .map_or(0, |p| p + 1);
    let mut start = end;
    while start > 0 && key[start - 1].is_ascii_alphanumeric() {
        start -= 1;
        let camel = start > 0
            && key[start].is_ascii_uppercase()
            && (key[start - 1].is_ascii_lowercase()
                || (key[start - 1].is_ascii_uppercase()
                    && start + 1 < end
                    && key[start + 1].is_ascii_lowercase()));
        if camel {
            break;
        }
    }
    (&key[start..end], &key[..start])
}

#[inline(never)]
fn value(l: &[u8], p: usize, rule: Rule, out: &mut Vec<Span>) -> Option<usize> {
    let (quote, escaped, start) = match (l.get(p), l.get(p + 1)) {
        (Some(&q @ (b'"' | b'\'')), _) => (Some(q), false, p + 1),
        (Some(b'\\'), Some(&q @ (b'"' | b'\''))) => (Some(q), true, p + 2),
        _ => (None, false, p),
    };
    let limit = match (quote, rule) {
        (Some(q), _) => quoted_end(l, start, q, escaped),
        (None, Rule::Keyed { .. }) => {
            start
                + trim_unbalanced_closers(&l[start..start + run_until(l, start, |b| is(b, VSTOP))])
        }
        // Header values have spaces; only a quote, a JSON or an ANSI escape ends them early.
        (None, _) => start + run_until(l, start, |b| matches!(b, b'"' | b'\\' | 0x1b)),
    };
    if start >= limit {
        return None;
    }
    match rule {
        Rule::Auth => {
            let mut s = start;
            for scheme in [&b"bearer"[..], b"basic", b"token", b"digest"] {
                let after = s + scheme.len();
                if after <= limit && l[s..after].eq_ignore_ascii_case(scheme) {
                    // The scheme alone is not a credential.
                    if after == limit || !is(l[after], AUTHV) {
                        s = skip_space(l, after).min(limit);
                    }
                    break;
                }
            }
            let v = run(&l[..limit], s, AUTHV);
            if v == 0 || is_marker(&l[s..s + v]) {
                return None;
            }
            out.push(span(s, s + v, Kind::Token));
            Some(s + v)
        }
        Rule::Cookie => {
            cookies(l, start, limit, out);
            Some(limit)
        }
        Rule::Keyed { pwd } => {
            let v = &l[start..limit];
            let path = pwd && matches!(v[0], b'/' | b'~');
            if v.len() < MIN_VALUE || path || is_marker(v) || is_uuid(v) || entropy(v) < 3.0 {
                return None;
            }
            let kind = match token(l, start) {
                Some((end, kind)) if end == limit => kind,
                _ => Kind::Secret,
            };
            out.push(span(start, limit, kind));
            Some(limit)
        }
    }
}

/// Where a value opened by `q` closes. JSON-escaped (`\"…\"`), the text is read as escape pairs: `\\` is a
/// backslash of the inner string and `\q` its quote, which closes it unless that backslash escaped it.
fn quoted_end(l: &[u8], start: usize, q: u8, escaped: bool) -> usize {
    let mut j = start;
    let mut inner_escape = false;
    while j < l.len() {
        match (l[j], l.get(j + 1)) {
            (b'\\', Some(&next)) if escaped => {
                if next == q && !inner_escape {
                    return j;
                }
                inner_escape = next == b'\\' && !inner_escape;
                j += 2;
            }
            (b'\\', _) => j += 2,
            (b, _) if b == q => return j,
            _ => {
                inner_escape = false;
                j += 1;
            }
        }
    }
    l.len()
}

/// Length of `v` without closing brackets that end it but opened outside it: `(--token=abc)` keeps its paren,
/// and a bracket inside the value can't split it and leak the tail.
fn trim_unbalanced_closers(v: &[u8]) -> usize {
    let mut len = v.len();
    while let Some(&close) = v[..len].last() {
        let open = match close {
            b')' => b'(',
            b']' => b'[',
            b'}' => b'{',
            _ => break,
        };
        let count = |b: u8| v[..len].iter().filter(|&&c| c == b).count();
        if count(close) <= count(open) {
            break;
        }
        len -= 1;
    }
    len
}

/// Each `name=value` value in a cookie header's `l[start..limit]`, except `Set-Cookie` attributes.
#[inline(never)]
fn cookies(l: &[u8], start: usize, limit: usize, out: &mut Vec<Span>) {
    const ATTRIBUTES: [&[u8]; 5] = [b"path", b"domain", b"expires", b"max-age", b"samesite"];
    let pair_byte = |b: u8| b != b';' && !is(b, SPACE);
    let mut i = start;
    while i < limit {
        if !pair_byte(l[i]) {
            i += 1;
            continue;
        }
        let end = i + l[i..limit].iter().take_while(|&&b| pair_byte(b)).count();
        if let Some(eq) = l[i..end].iter().position(|&b| b == b'=') {
            let (name, v) = (&l[i..i + eq], &l[i + eq + 1..end]);
            let attribute = ATTRIBUTES.iter().any(|a| name.eq_ignore_ascii_case(a));
            if eq > 0 && v.len() >= MIN_VALUE && !attribute && !is_marker(v) {
                out.push(span(i + eq + 1, end, Kind::Token));
            }
        }
        i = end;
    }
}

/// A value already redacted: Rails' `[FILTERED]`, or a placeholder like `<SECRET_1>`.
fn is_marker(v: &[u8]) -> bool {
    let inner = match (v.first(), v.last()) {
        (Some(b'['), Some(b']')) | (Some(b'<'), Some(b'>')) if v.len() > 2 => &v[1..v.len() - 1],
        _ => return false,
    };
    inner
        .iter()
        .all(|&b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

fn is_uuid(v: &[u8]) -> bool {
    v.len() == 36
        && v.iter().enumerate().all(|(i, &b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Shannon entropy in bits per byte, as launder's keyed rule measures it.
fn entropy(v: &[u8]) -> f64 {
    let mut counts = [0u32; 256];
    for &b in v {
        counts[b as usize] += 1;
    }
    let total = v.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / total;
            -p * p.log2()
        })
        .sum()
}

/// Appends the emails, public IPs and home-directory prefixes in `l` to `out`. `home` is `$HOME`, for a home
/// outside `/Users` and `/home`.
pub fn pii(l: &[u8], home: Option<&[u8]>, out: &mut Vec<Span>) {
    let n = l.len();
    let mut i = 0;
    while i < n {
        let b = l[i];
        if b == 0x1b {
            i = skip_ansi(l, i);
            continue;
        }
        if b == b'<'
            && let Some((_, end)) = placeholder(l, i)
        {
            i = end;
            continue;
        }
        if b == b'/'
            && (i == 0 || !(is(l[i - 1], KEYC) || l[i - 1] == b'/'))
            && let Some(end) = home_prefix(l, i, home)
        {
            out.push(span(i, end, Kind::Home));
            i = end;
            continue;
        }
        if !is(b, LOCAL) {
            i += 1;
            continue;
        }
        let end = i + run(l, i, LOCAL);
        let hit = match l.get(end) {
            Some(b'@') => email(l, i, end),
            Some(b':') => ipv6(l, i),
            _ => ipv4(l, i, end),
        };
        match hit {
            Some(s) => {
                out.push(s);
                i = s.end;
            }
            None => i = end,
        }
    }
}

fn home_prefix(l: &[u8], i: usize, home: Option<&[u8]>) -> Option<usize> {
    if let Some(home) = home
        && home.len() > 1
        && l[i..].starts_with(home)
        && l.get(i + home.len()).is_none_or(|&b| !is(b, KEYC))
    {
        return Some(i + home.len());
    }
    for root in [&b"/Users/"[..], b"/home/"] {
        if l[i..].starts_with(root) {
            let user = run(l, i + root.len(), KEYC);
            if user > 0 {
                return Some(i + root.len() + user);
            }
        }
    }
    None
}

fn email(l: &[u8], start: usize, at: usize) -> Option<Span> {
    let domain = at
        + 1
        + run_until(l, at + 1, |b| {
            !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        });
    let trailing = l[at + 1..domain]
        .iter()
        .rev()
        .take_while(|&&b| b == b'.' || b == b'-')
        .count();
    let host = &l[at + 1..domain - trailing];
    let tld = host
        .iter()
        .rposition(|&b| b == b'.')
        .map(|dot| &host[dot + 1..])?;
    (tld.len() >= 2 && tld.iter().all(u8::is_ascii_alphabetic)).then_some(span(
        start,
        domain - trailing,
        Kind::Email,
    ))
}

fn ipv4(l: &[u8], start: usize, end: usize) -> Option<Span> {
    let end = end
        - l[start..end]
            .iter()
            .rev()
            .take_while(|&&b| b == b'.')
            .count();
    let mut octets = [0u32; 4];
    let mut parts = 0;
    for part in l[start..end].split(|&b| b == b'.') {
        if parts == 4 || part.is_empty() || part.len() > 3 || !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        octets[parts] = part.iter().fold(0, |n, &d| n * 10 + u32::from(d - b'0'));
        parts += 1;
    }
    if parts != 4 || octets.iter().any(|&o| o > 255) {
        return None;
    }
    let private = match octets {
        [0 | 10 | 127, ..] | [169, 254, ..] | [192, 168, ..] | [255, 255, 255, 255] => true,
        [172, b, ..] => (16..=31).contains(&b),
        _ => false,
    };
    (!private).then_some(span(start, end, Kind::Ip))
}

/// launder's rule: a colon-and-hex run that stands alone (`Api::V1::X` and `ActiveRecord::Base` don't), has two
/// or more groups, and parses; a trailing single `:` is punctuation.
fn ipv6(l: &[u8], start: usize) -> Option<Span> {
    let mut end = start + run_until(l, start, |b| !(b.is_ascii_hexdigit() || b == b':'));
    let joins = |b: u8| is(b, W) || b == b':';
    if l.get(end).is_some_and(|&b| joins(b) || b == b'.') {
        return None;
    }
    if end >= start + 2 && l[end - 1] == b':' && l[end - 2] != b':' {
        end -= 1;
    }
    let text = std::str::from_utf8(&l[start..end]).ok()?;
    if text.split(':').filter(|g| !g.is_empty()).count() < 2 {
        return None;
    }
    let addr: Ipv6Addr = text.parse().ok()?;
    let first = addr.segments()[0];
    let private = addr.is_loopback()
        || addr.is_unspecified()
        || first & 0xffc0 == 0xfe80 // link-local
        || first & 0xfe00 == 0xfc00; // unique-local
    (!private).then_some(span(start, end, Kind::Ip))
}

/// A placeholder this module writes, `<TOKEN_12>`, at `at`: its kind and where it ends.
pub fn placeholder(l: &[u8], at: usize) -> Option<(Kind, usize)> {
    let body = l.get(at + 1..)?;
    let close = body.iter().take(24).position(|&b| b == b'>')?;
    let inner = &body[..close];
    let underscore = inner.iter().rposition(|&b| b == b'_')?;
    let (name, number) = (&inner[..underscore], &inner[underscore + 1..]);
    if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let kind = Kind::ALL
        .into_iter()
        .find(|kind| kind.name().as_bytes() == name)?;
    Some((kind, at + 1 + close + 1))
}

/// Appends `text` to `out` with each placeholder's number dropped (`<TOKEN_12>` → `<TOKEN>`): identity must not
/// depend on the order credentials appeared in a run.
pub fn unnumber(text: &[u8], out: &mut Vec<u8>) {
    let (mut from, mut i) = (0, 0);
    while let Some(open) = text[i..].iter().position(|&b| b == b'<') {
        let at = i + open;
        match placeholder(text, at) {
            Some((kind, end)) => {
                out.extend_from_slice(&text[from..at]);
                out.push(b'<');
                out.extend_from_slice(kind.name().as_bytes());
                out.push(b'>');
                (from, i) = (end, end);
            }
            None => i = at + 1,
        }
    }
    out.extend_from_slice(&text[from..]);
}

/// Distinct values a run remembers the number of. Past it a new value still gets a new number, just not the same
/// one next time. Only a hash of each value is kept.
const MAX_REMEMBERED: usize = 1 << 16;

#[derive(Debug, Default)]
struct Numbers {
    seen: HashMap<u64, u32>,
    next: [u32; Kind::ALL.len()],
}

impl Numbers {
    fn number(&mut self, kind: Kind, value: &[u8]) -> u32 {
        let key = fnv1a64(value) ^ (kind as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        if let Some(&n) = self.seen.get(&key) {
            return n;
        }
        let next = &mut self.next[kind as usize];
        *next += 1;
        if self.seen.len() < MAX_REMEMBERED {
            self.seen.insert(key, *next);
        }
        *next
    }

    fn render(&mut self, line: &[u8], spans: &[Span], out: &mut Vec<u8>) {
        out.clear();
        let mut prev = 0;
        for s in spans {
            out.extend_from_slice(&line[prev..s.start]);
            if s.kind == Kind::Home {
                out.push(b'~');
            } else {
                let n = self.number(s.kind, &line[s.start..s.end]);
                out.push(b'<');
                out.extend_from_slice(s.kind.name().as_bytes());
                out.push(b'_');
                push_decimal(out, n);
                out.push(b'>');
            }
            prev = s.end;
        }
        out.extend_from_slice(&line[prev..]);
    }
}

fn push_decimal(out: &mut Vec<u8>, mut n: u32) {
    let mut digits = [0u8; 10];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[at..]);
}

/// A run's redaction: one numbering shared by all its streams, and buffers reused line to line.
#[derive(Debug, Default)]
pub struct Redactor {
    spans: Vec<Span>,
    masked: Vec<u8>,
    evidence: Vec<u8>,
    numbers: Numbers,
    home: Option<Vec<u8>>,
}

/// One line, redacted two ways. Each borrows the input line when nothing in it needed redacting.
#[derive(Debug, Clone, Copy)]
pub struct Views<'a> {
    /// Credentials masked, under every mode: what templates are built from.
    pub masked: &'a [u8],
    /// What the mode stores as evidence.
    pub evidence: &'a [u8],
}

impl Redactor {
    /// `home` is `$HOME`, for [`Mode::Pii`].
    pub fn new(home: Option<&[u8]>) -> Self {
        Redactor {
            home: home.map(<[u8]>::to_vec),
            ..Self::default()
        }
    }

    /// `line` (no terminator) from the stream `scanner` belongs to.
    pub fn line<'a>(&'a mut self, scanner: &mut Scanner, line: &'a [u8], mode: Mode) -> Views<'a> {
        let Redactor {
            spans,
            masked,
            evidence,
            numbers,
            home,
        } = self;
        spans.clear();
        scanner.secrets(line, spans);
        if !spans.is_empty() {
            numbers.render(line, spans, masked);
        }
        let masked: &'a [u8] = if spans.is_empty() { line } else { masked };
        let evidence: &'a [u8] = match mode {
            Mode::Secrets => masked,
            Mode::Off => line,
            Mode::Pii => {
                spans.clear();
                pii(masked, home.as_deref(), spans);
                if spans.is_empty() {
                    masked
                } else {
                    numbers.render(masked, spans, evidence);
                    evidence
                }
            }
        };
        Views { masked, evidence }
    }
}

/// `text` with its credentials masked, numbered from 1 within it, for values stored whole: a command line, a
/// note. The same text always redacts the same way, so a redacted context name is still a stable key.
pub fn redact_text(text: &str) -> Cow<'_, str> {
    let mut scanner = Scanner::default();
    let mut numbers = Numbers::default();
    let (mut spans, mut line_out) = (Vec::new(), Vec::new());
    let mut out: Option<Vec<u8>> = None;
    let mut done = 0;
    for line in text.as_bytes().split_inclusive(|&b| b == b'\n') {
        let body = line.strip_suffix(b"\n").unwrap_or(line);
        spans.clear();
        scanner.secrets(body, &mut spans);
        if !spans.is_empty() {
            let out = out.get_or_insert_with(|| text.as_bytes()[..done].to_vec());
            numbers.render(body, &spans, &mut line_out);
            out.extend_from_slice(&line_out);
            out.extend_from_slice(&line[body.len()..]);
        } else if let Some(out) = &mut out {
            out.extend_from_slice(line);
        }
        done += line.len();
    }
    match out {
        None => Cow::Borrowed(text),
        // Spans start and end on ASCII bytes, so this is always UTF-8.
        Some(bytes) => Cow::Owned(String::from_utf8_lossy(&bytes).into_owned()),
    }
}
