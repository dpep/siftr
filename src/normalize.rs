//! Log line normalizer: strips ANSI escapes and masks incidental variation into
//! typed slots in one pass, so recurring lines share a template.
//!
//! Each line is split into chunks (runs of token bytes). A chunk is classified
//! whole first; only if that fails is it split, level by level, on `/?&=`, then
//! `:`, then `-_+`, and each piece is classified. Words are never masked, so a
//! path keeps its literal segments (`/users/<int>`) and `User Load` never
//! merges with `Post Load`.
//!
//! A filesystem path's machine-specific prefix is canonicalized first (`path`):
//! where a run happened is not part of a behavior, what the path names is. So is
//! a syslog header's time and host (`syslog`).

mod hash;
mod path;
mod recognize;
pub mod secrets;
mod stats;
mod syslog;

pub use hash::fnv1a64;
pub use path::{PathRole, PathRoles, Roots, UnknownPathRole};
pub use stats::{DEFAULT_MAX_TRACKED_VALUES, SlotClass, SlotStats, classify};

use path::Prefix;
use recognize::{
    duration_ms, is_date_only, is_time, is_zone_word, leading_digits, recognize, size_bytes,
    spaced_unit,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotKind {
    Int,
    Float,
    Hex,
    Uuid,
    Timestamp,
    Duration,
    Size,
    Ip,
    Email,
    /// Reserved; never emitted. URLs are split on `/?&=` so the host and route
    /// literals stay in the template and only their variable parts are masked.
    Url,
    Version,
    Quoted,
    /// A filesystem path. It annotates rather than masks: the template keeps the
    /// path's canonical text, and the slots inside the path mask as usual.
    Path,
    /// A temp dir or file stem something generated (`d20260915-123-abc`).
    TempName,
    /// The host field of a syslog header.
    Host,
}

impl SlotKind {
    /// Placeholder text written into the template. `Path` never writes its own.
    pub fn placeholder(self) -> &'static str {
        match self {
            SlotKind::Int => "<int>",
            SlotKind::Float => "<float>",
            SlotKind::Hex => "<hex>",
            SlotKind::Uuid => "<uuid>",
            SlotKind::Timestamp => "<timestamp>",
            SlotKind::Duration => "<duration>",
            SlotKind::Size => "<size>",
            SlotKind::Ip => "<ip>",
            SlotKind::Email => "<email>",
            SlotKind::Url => "<url>",
            SlotKind::Version => "<version>",
            SlotKind::Quoted => "<quoted>",
            SlotKind::Path => "<path>",
            SlotKind::TempName => "<tmpname>",
            SlotKind::Host => "<host>",
        }
    }
}

/// A masked value: byte span into the original line (ANSI escapes included).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub kind: SlotKind,
    pub start: u32,
    pub end: u32,
}

impl Slot {
    /// The raw value this slot masked.
    pub fn text<'l>(&self, line: &'l [u8]) -> &'l [u8] {
        &line[self.start as usize..self.end as usize]
    }
}

pub struct Normalized<'n> {
    /// Masked, ANSI-stripped, whitespace runs collapsed to one space, trimmed.
    pub template: &'n [u8],
    /// FNV-1a 64 of `template`; stable across processes and machines.
    pub template_hash: u64,
    pub slots: &'n [Slot],
    /// What this line's paths are. A function of `template`, never part of its hash.
    pub roles: PathRoles,
}

/// Numeric value of a slot: `Int`/`Float` as written, `Duration` in
/// milliseconds, `Size` in bytes. `None` for other kinds and for collapsed
/// lists (`1, 2, 3`).
pub fn slot_value_f64(line: &[u8], slot: &Slot) -> Option<f64> {
    value_f64(slot.kind, slot.text(line))
}

pub(crate) fn value_f64(kind: SlotKind, v: &[u8]) -> Option<f64> {
    match kind {
        SlotKind::Int | SlotKind::Float => parse_f64(v),
        SlotKind::Duration => sum_units(v, duration_ms),
        SlotKind::Size => sum_units(v, size_bytes),
        _ => None,
    }
}

fn parse_f64(v: &[u8]) -> Option<f64> {
    std::str::from_utf8(v).ok()?.parse().ok()
}

/// Sums `number unit` pairs, so a collapsed `1 minute 3.5 seconds` is one value.
fn sum_units(v: &[u8], factor: fn(&[u8]) -> Option<f64>) -> Option<f64> {
    let (mut total, mut any, mut i) = (0.0, false, 0);
    while i < v.len() {
        if matches!(v[i], b' ' | b',') {
            i += 1;
            continue;
        }
        let start = i;
        if v[i] == b'-' {
            i += 1;
        }
        i += leading_digits(&v[i..]);
        if v.get(i) == Some(&b'.') {
            i += 1 + leading_digits(&v[i + 1..]);
        }
        let n = parse_f64(&v[start..i])?;
        while v.get(i) == Some(&b' ') {
            i += 1;
        }
        let unit = i;
        while i < v.len() && v[i].is_ascii_alphabetic() {
            i += 1;
        }
        total += n * factor(&v[unit..i])?;
        any = true;
    }
    any.then_some(total)
}

const TOKEN: u8 = 1;
const SEP_PATH: u8 = 2;
const SEP_COLON: u8 = 4;
const SEP_WORD: u8 = 8;
const DIGIT: u8 = 16;
const SPACE: u8 = 32;
const AT: u8 = 64;
const SLASH: u8 = 128;

/// Split levels tried in order once a chunk fails whole-token recognition.
const SPLIT_LEVELS: [u8; 3] = [SEP_PATH, SEP_COLON, SEP_WORD];

static CLASS: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        let mut c = 0;
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'@' | b'+') {
            c |= TOKEN;
        }
        if matches!(b, b'/' | b'?' | b'&' | b'=') {
            c |= TOKEN | SEP_PATH;
        }
        if b == b':' {
            c |= SEP_COLON;
        }
        if matches!(b, b'-' | b'_' | b'+') {
            c |= SEP_WORD;
        }
        if b.is_ascii_digit() {
            c |= DIGIT;
        }
        if b == b'@' {
            c |= AT;
        }
        if b == b'/' {
            c |= SLASH;
        }
        if matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            c |= SPACE;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// Reusable normalizer. Buffers grow to the longest line seen and are reused,
/// so steady-state normalization does not allocate.
#[derive(Default)]
pub struct Normalizer {
    template: Vec<u8>,
    slots: Vec<Slot>,
    /// Template length just after the last placeholder, for list collapsing.
    last_slot_end: usize,
    roots: Roots,
    roles: PathRoles,
}

impl Normalizer {
    pub fn new() -> Self {
        Self::with_roots(Roots::default())
    }

    /// Also canonicalizes paths under this machine's project, home and temp dir.
    pub fn with_roots(roots: Roots) -> Self {
        Self {
            template: Vec::with_capacity(256),
            slots: Vec::with_capacity(16),
            last_slot_end: 0,
            roots,
            roles: PathRoles::default(),
        }
    }

    /// Normalizes one line (a trailing newline is fine). Lines longer than
    /// `u32::MAX` bytes are truncated so spans fit a `u32`.
    pub fn normalize(&mut self, line: &[u8]) -> Normalized<'_> {
        let line = &line[..line.len().min(u32::MAX as usize)];
        self.template.clear();
        self.slots.clear();
        self.last_slot_end = 0;
        self.roles = PathRoles::default();

        let mut i = 0;
        while line.get(i) == Some(&0x1b) {
            i = skip_ansi(line, i);
        }
        if let Some(header) = syslog::header(line, i) {
            i = self.syslog_header(line, i, header);
        }
        while i < line.len() {
            let b = line[i];
            let class = CLASS[b as usize];
            // `~/` opens a path, though `~` isn't a token byte.
            i = if class & TOKEN != 0 || (b == b'~' && line.get(i + 1) == Some(&b'/')) {
                self.chunk(line, i)
            } else if class & SPACE != 0 {
                if self.template.last().is_some_and(|&l| l != b' ') {
                    self.template.push(b' ');
                }
                i + 1
            } else {
                match b {
                    0x1b => skip_ansi(line, i),
                    b'\'' => self.single_quote(line, i),
                    b'"' => self.double_quote(line, i),
                    b'<' if let Some((kind, end)) = secrets::placeholder(line, i) => {
                        // Numbered per run, so the number must not reach identity.
                        self.template.push(b'<');
                        self.template.extend_from_slice(kind.name().as_bytes());
                        self.template.push(b'>');
                        end
                    }
                    _ => {
                        self.template.push(b);
                        i + 1
                    }
                }
            };
        }
        if self.template.last() == Some(&b' ') {
            self.template.pop();
        }
        Normalized {
            template: &self.template,
            template_hash: fnv1a64(&self.template),
            slots: &self.slots,
            roles: self.roles,
        }
    }

    /// `start` is a token byte, or the `~` of `~/`.
    fn chunk(&mut self, line: &[u8], start: usize) -> usize {
        let mut end = start + usize::from(line[start] == b'~');
        let mut flags = 0;
        while end < line.len() && CLASS[line[end] as usize] & TOKEN != 0 {
            flags |= CLASS[line[end] as usize];
            end += 1;
        }
        if flags & SLASH != 0
            && let Some(next) = self.path(line, start, end, flags)
        {
            return next;
        }
        self.token(line, start, end, flags)
    }

    /// A path in the chunk `line[start..end]`, canonicalized: where masking resumes.
    /// `None` leaves the chunk to `token`, a path in it only annotated, so its
    /// template can't change.
    fn path(&mut self, line: &[u8], start: usize, end: usize, flags: u8) -> Option<usize> {
        let (ps, pe) = path_span(line, start, end, flags)?;
        let path = &line[ps..pe];
        let Some((prefix, tail)) = path::canonical(path, &self.roots) else {
            if let Some(roles) = path::file_path_roles(path) {
                self.roles |= roles;
                self.path_slot(ps, pe);
            }
            return None;
        };
        if ps > start {
            self.token(line, start, ps, span_flags(&line[start..ps]));
        }
        self.canonical_path(line, ps, pe, prefix, ps + tail);
        Some(match pe < end {
            true => self.token(line, pe, end, span_flags(&line[pe..end])),
            false => end,
        })
    }

    /// Masks `line[start..end]`, a run of token bytes whose classes OR to `flags`.
    fn token(&mut self, line: &[u8], start: usize, end: usize, flags: u8) -> usize {
        if flags & (DIGIT | AT) == 0 {
            self.template.extend_from_slice(&line[start..end]);
            return end;
        }
        // Trailing dots end a sentence, not a value (`retrying in 5 seconds.`).
        let mut e = end;
        while e > start + 1 && line[e - 1] == b'.' {
            e -= 1;
        }
        match recognize(&line[start..e]) {
            Some(kind) if e == end => {
                let (kind, span_end) = extend(line, kind, start, end);
                self.slot(kind, start, span_end);
                span_end
            }
            Some(kind) => {
                self.slot(kind, start, e);
                self.template.extend_from_slice(&line[e..end]);
                end
            }
            None => {
                self.split(line, start, e, 0);
                self.template.extend_from_slice(&line[e..end]);
                end
            }
        }
    }

    /// `line[s..e]` already failed recognition as a whole: split it on this
    /// level's separators, or fall through to the next level if it has none.
    fn split(&mut self, line: &[u8], s: usize, e: usize, level: usize) {
        let Some(&sep) = SPLIT_LEVELS.get(level) else {
            return self.dotted_hex(line, s, e);
        };
        if !line[s..e].iter().any(|&b| CLASS[b as usize] & sep != 0) {
            return self.split(line, s, e, level + 1);
        }
        let mut piece = s;
        let mut i = s;
        while i < e {
            // A UUID inside a dash-joined token would otherwise be cut into its groups.
            if sep == SEP_WORD
                && i == piece
                && e - i >= 36
                && recognize::is_uuid(&line[i..i + 36])
                && (i + 36 == e || CLASS[line[i + 36] as usize] & SEP_WORD != 0)
            {
                self.slot(SlotKind::Uuid, i, i + 36);
                i += 36;
                piece = i;
                continue;
            }
            if CLASS[line[i] as usize] & sep != 0 {
                self.piece(line, piece, i, level);
                // Rails view methods are `erb__<hash>`, or `erb___<hash>` when the per-boot hash is negative.
                let run = if sep == SEP_WORD {
                    underscores_before_digits(&line[i..e])
                } else {
                    0
                };
                if run >= 2 {
                    self.template.extend_from_slice(b"__");
                    i += run;
                    piece = i;
                    continue;
                }
                self.template.push(line[i]);
                piece = i + 1;
            }
            i += 1;
        }
        self.piece(line, piece, e, level);
    }

    /// Writes an unsplittable piece literally, except `0x` hex a dot glues on
    /// (`peer[3].0x7fa1c2`): `.` is a token byte, so no other rule sees it.
    fn dotted_hex(&mut self, line: &[u8], s: usize, e: usize) {
        let (mut from, mut i) = (s, s);
        while let Some(dot) = line[i..e].iter().position(|&b| b == b'.') {
            let at = i + dot + 1;
            i = at;
            if e - at <= 2 || line[at] != b'0' || line[at + 1] | 0x20 != b'x' {
                continue;
            }
            let end = at + 2 + leading_hex(&line[at + 2..e]);
            if end > at + 2 && (end == e || line[end] == b'.') {
                self.template.extend_from_slice(&line[from..at]);
                self.slot(SlotKind::Hex, at, end);
                (from, i) = (end, end);
            }
        }
        self.template.extend_from_slice(&line[from..e]);
    }

    /// When and where a syslog line was written are slots; the process is who
    /// wrote it, so its name stays literal whatever it spells (`postgres-14`).
    fn syslog_header(&mut self, line: &[u8], start: usize, header: syslog::Header) -> usize {
        self.slot(SlotKind::Timestamp, start, header.time_end);
        let Some(tail) = header.tail else {
            return header.time_end;
        };
        self.template.push(b' ');
        self.slot(SlotKind::Host, header.time_end + 1, tail.host_end);
        self.template.push(b' ');
        self.template
            .extend_from_slice(&line[tail.host_end + 1..tail.proc_end]);
        tail.proc_end
    }

    fn piece(&mut self, line: &[u8], s: usize, e: usize, level: usize) {
        let t = &line[s..e];
        if !t.iter().any(|&b| CLASS[b as usize] & (DIGIT | AT) != 0) {
            self.template.extend_from_slice(t);
        } else if let Some(kind) = recognize(t) {
            self.slot(kind, s, e);
        } else {
            self.split(line, s, e, level + 1);
        }
    }

    /// Writes a path's canonical prefix, then its tail masked as any other text,
    /// except that a temp dir's generated names become one slot each.
    fn canonical_path(
        &mut self,
        line: &[u8],
        ps: usize,
        pe: usize,
        prefix: Prefix<'_>,
        tail: usize,
    ) {
        self.path_slot(ps, pe);
        prefix.write(&mut self.template);
        self.roles |= path::roles(prefix, &line[tail..pe]);
        if prefix == Prefix::Tmp {
            self.temp_tail(line, tail, pe);
        } else if tail < pe {
            self.split(line, tail, pe, 0);
        }
    }

    /// `line[at..pe]` is empty or starts at a `/`.
    fn temp_tail(&mut self, line: &[u8], mut at: usize, pe: usize) {
        while at < pe {
            self.template.push(b'/');
            let start = at + 1;
            let end = start
                + line[start..pe]
                    .iter()
                    .position(|&b| b == b'/')
                    .unwrap_or(pe - start);
            let segment = &line[start..end];
            // A file keeps its extension: `<tmpname>.bin`.
            let stem = match end == pe {
                true => segment
                    .iter()
                    .skip(1)
                    .position(|&b| b == b'.')
                    .map_or(segment.len(), |p| p + 1),
                false => segment.len(),
            };
            if path::is_generated(&segment[..stem]) {
                self.slot(SlotKind::TempName, start, start + stem);
                self.template.extend_from_slice(&segment[stem..]);
            } else {
                self.piece(line, start, end, 0);
            }
            at = end;
        }
    }

    /// Paths annotate: no placeholder, and never a neighbour to collapse with.
    fn path_slot(&mut self, start: usize, end: usize) {
        self.slots.push(Slot {
            kind: SlotKind::Path,
            start: start as u32,
            end: end as u32,
        });
    }

    fn slot(&mut self, kind: SlotKind, start: usize, end: usize) {
        // Collapse same-kind runs so `IN (1, 2, 3)` and `IN (4)` share a template,
        // as do `1 minute 3.5 seconds` and `10.9 seconds`.
        if let Some(prev) = self.slots.last_mut() {
            let gap = &self.template[self.last_slot_end..];
            if prev.kind == kind
                && !gap.is_empty()
                && gap.iter().all(|&b| b == b',' || b == b' ')
                && (gap.contains(&b',') || kind == SlotKind::Duration)
            {
                self.template.truncate(self.last_slot_end);
                prev.end = end as u32;
                return;
            }
        }
        self.template
            .extend_from_slice(kind.placeholder().as_bytes());
        self.last_slot_end = self.template.len();
        self.slots.push(Slot {
            kind,
            start: start as u32,
            end: end as u32,
        });
    }

    fn single_quote(&mut self, line: &[u8], open: usize) -> usize {
        if sql_value_context(&line[..open])
            && let Some(close) = closing_quote(line, open)
        {
            self.slot(SlotKind::Quoted, open, close + 1);
            return close + 1;
        }
        self.template.push(b'\'');
        open + 1
    }

    /// Only values are masked: a bind (`[["name", "Bob"]]`), a Ruby hash value
    /// (`"name"=>"Bob"`) or a JSON value (`"name":"Bob"`). Other double-quoted
    /// text is an identifier (`"users"."id"`), a key, or a path to split.
    fn double_quote(&mut self, line: &[u8], open: usize) -> usize {
        let before = trim_spaces_end(&line[..open]);
        let bind = before.last() == Some(&b',');
        if (bind || before.ends_with(b"=>") || before.ends_with(b"\":"))
            && let Some(close) = closing_quote(line, open)
            && (!bind || line.get(close + 1) == Some(&b']'))
        {
            self.slot(SlotKind::Quoted, open, close + 1);
            return close + 1;
        }
        self.template.push(b'"');
        open + 1
    }
}

/// Extends a whole-chunk match across following words: `10.9 seconds`,
/// `2024-01-15 10:00:00 +0000`.
fn extend(line: &[u8], kind: SlotKind, start: usize, end: usize) -> (SlotKind, usize) {
    match kind {
        SlotKind::Int | SlotKind::Float => {
            if let Some((word_start, word_end)) = next_word(line, end)
                && line[word_start..word_end]
                    .iter()
                    .all(u8::is_ascii_alphabetic)
                && let Some(unit_kind) = spaced_unit(&line[word_start..word_end])
            {
                return (unit_kind, word_end);
            }
            (kind, end)
        }
        SlotKind::Timestamp => {
            let mut end = end;
            let token = &line[start..end];
            if is_date_only(token) {
                match next_word(line, end) {
                    Some((ws, we)) if is_time(&line[ws..we], true) => end = we,
                    _ => return (kind, end),
                }
            } else if is_time(token, true)
                && let Some((ws, we)) = next_word(line, end)
                && is_date_only(&line[ws..we])
            {
                // Time then date, as Resque logs it: `[10:39:04 2018-12-19]`.
                end = we;
            }
            if let Some((ws, we)) = next_word(line, end)
                && is_zone_word(&line[ws..we])
            {
                end = we;
            }
            (kind, end)
        }
        _ => (kind, end),
    }
}

/// The token right after a single space at `at`, minus trailing `.`/`:`
/// (`10.9 seconds.`).
fn next_word(line: &[u8], at: usize) -> Option<(usize, usize)> {
    if line.get(at) != Some(&b' ') {
        return None;
    }
    let start = at + 1;
    let mut end = start
        + line[start..]
            .iter()
            .take_while(|&&b| CLASS[b as usize] & TOKEN != 0)
            .count();
    while end > start && matches!(line[end - 1], b'.' | b':') {
        end -= 1;
    }
    (end > start).then_some((start, end))
}

/// The path in the chunk `line[start..end]`: after any `key=`, before a `:12`
/// line suffix, without a sentence's trailing dots. A query means a URL.
fn path_span(line: &[u8], start: usize, end: usize, flags: u8) -> Option<(usize, usize)> {
    let (mut ps, mut pe, mut slash) = (start, end, false);
    for (i, &b) in (start..).zip(&line[start..end]) {
        match b {
            b'?' | b'&' => return None,
            b'=' => (ps, pe, slash) = (i + 1, end, false),
            b':' if pe == end => pe = i,
            b'/' if pe == end => slash = true,
            _ => {}
        }
    }
    while pe > ps && line[pe - 1] == b'.' {
        pe -= 1;
    }
    let ats = flags & AT == 0 || path::ats_open_segments(&line[ps..pe]);
    (slash && ats).then_some((ps, pe))
}

fn leading_hex(t: &[u8]) -> usize {
    t.iter().take_while(|b| b.is_ascii_hexdigit()).count()
}

fn span_flags(t: &[u8]) -> u8 {
    t.iter().fold(0, |flags, &b| flags | CLASS[b as usize])
}

/// Length of the `_` run starting `t` when an all-digit piece follows it, else 0.
fn underscores_before_digits(t: &[u8]) -> usize {
    let run = t.iter().take_while(|&&b| b == b'_').count();
    let piece = &t[run..];
    let digits = leading_digits(piece);
    let ends = piece
        .get(digits)
        .is_none_or(|&b| CLASS[b as usize] & SEP_WORD != 0);
    if digits > 0 && ends { run } else { 0 }
}

/// Copies `line` into `out` without its ANSI escape sequences, for parsing a line's structure.
pub fn strip_ansi(line: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let mut i = 0;
    while let Some(esc) = line[i..].iter().position(|&b| b == 0x1b) {
        out.extend_from_slice(&line[i..i + esc]);
        i = skip_ansi(line, i + esc);
    }
    out.extend_from_slice(&line[i..]);
}

fn trim_spaces_end(t: &[u8]) -> &[u8] {
    &t[..t.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1)]
}

fn skip_ansi(line: &[u8], esc: usize) -> usize {
    match line.get(esc + 1) {
        // CSI: parameter and intermediate bytes, then one final byte in 0x40..=0x7e.
        Some(b'[') => line[esc + 2..]
            .iter()
            .position(|b| (0x40..=0x7e).contains(b))
            .map_or(line.len(), |p| esc + 2 + p + 1),
        // OSC: terminated by BEL or ESC \.
        Some(b']') => {
            let body = &line[esc + 2..];
            match body.iter().position(|&b| b == 0x07 || b == 0x1b) {
                Some(p) if body[p] == 0x1b => (esc + 2 + p + 2).min(line.len()),
                Some(p) => esc + 2 + p + 1,
                None => line.len(),
            }
        }
        Some(_) => esc + 2,
        None => esc + 1,
    }
}

/// True when a `'` at the end of `before` opens an SQL value: after an operator
/// or list punctuation, or a value keyword. Ruby's `in 'save'` stays literal.
fn sql_value_context(before: &[u8]) -> bool {
    let Some(last) = last_non_space(before) else {
        return false;
    };
    if matches!(last, b'=' | b'(' | b',' | b'<' | b'>' | b'[') {
        return true;
    }
    let trimmed = &before[..before.iter().rposition(|&b| b != b' ').map_or(0, |p| p + 1)];
    let word_start = trimmed
        .iter()
        .rposition(|b| !b.is_ascii_uppercase())
        .map_or(0, |p| p + 1);
    if word_start > 0 && trimmed[word_start - 1].is_ascii_alphanumeric() {
        return false;
    }
    matches!(
        &trimmed[word_start..],
        b"IN"
            | b"LIKE"
            | b"ILIKE"
            | b"BINARY"
            | b"BETWEEN"
            | b"AND"
            | b"OR"
            | b"THEN"
            | b"ELSE"
            | b"WHEN"
            | b"VALUES"
            | b"NOT"
            | b"REGEXP"
    )
}

fn last_non_space(before: &[u8]) -> Option<u8> {
    before.iter().rev().copied().find(|&b| b != b' ')
}

/// Index of the matching close quote, honoring `\x` escapes and doubled quotes.
fn closing_quote(line: &[u8], open: usize) -> Option<usize> {
    let q = line[open];
    let mut j = open + 1;
    while j < line.len() {
        match line[j] {
            b'\\' => j += 2,
            b if b == q => {
                if line.get(j + 1) == Some(&q) && j > open + 1 {
                    j += 2;
                } else {
                    return Some(j);
                }
            }
            _ => j += 1,
        }
    }
    None
}
