//! Masks incidental variation in a line into typed slots, producing a template.
//!
//! Placeholder: the `siftr-normalize` crate replaces this module behind the same interface
//! (`Normalizer::normalize(&[u8]) -> Normalized`), so callers must not rely on the masking rules here.

/// What a masked value looked like.
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
    Url,
    Version,
    Quoted,
}

/// A masked value: its kind and byte span in the line passed to [`Normalizer::normalize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    pub kind: SlotKind,
    pub start: u32,
    pub end: u32,
}

/// A borrowed view of the last normalization; valid until the next call.
#[derive(Debug, Clone, Copy)]
pub struct Normalized<'n> {
    /// Masked and ANSI-stripped.
    pub template: &'n [u8],
    pub template_hash: u64,
    pub slots: &'n [Slot],
}

/// Reuses its buffers, so normalizing a line allocates nothing once warm.
#[derive(Debug, Default)]
pub struct Normalizer {
    template: Vec<u8>,
    slots: Vec<Slot>,
}

impl Normalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn normalize(&mut self, line: &[u8]) -> Normalized<'_> {
        self.template.clear();
        self.slots.clear();
        // Spans are u32; nothing longer reaches here from a LineSplitter anyway.
        let line = &line[..line.len().min(u32::MAX as usize)];
        let mut i = 0;
        let mut prev = b' ';
        while i < line.len() {
            let byte = line[i];
            if byte == ESC {
                i = skip_escape(line, i);
                continue;
            }
            if !is_word(prev)
                && let Some((kind, end)) = scan_slot(line, i)
            {
                self.template.extend_from_slice(marker(kind));
                self.slots.push(Slot {
                    kind,
                    start: i as u32,
                    end: end as u32,
                });
                prev = line[end - 1];
                i = end;
                continue;
            }
            self.template.push(byte);
            prev = byte;
            i += 1;
        }
        Normalized {
            template: &self.template,
            template_hash: fnv1a(&self.template),
            slots: &self.slots,
        }
    }
}

const ESC: u8 = 0x1b;

fn marker(kind: SlotKind) -> &'static [u8] {
    match kind {
        SlotKind::Int => b"<int>",
        SlotKind::Float => b"<float>",
        SlotKind::Hex => b"<hex>",
        SlotKind::Uuid => b"<uuid>",
        SlotKind::Timestamp => b"<timestamp>",
        SlotKind::Duration => b"<duration>",
        SlotKind::Size => b"<size>",
        SlotKind::Ip => b"<ip>",
        SlotKind::Email => b"<email>",
        SlotKind::Url => b"<url>",
        SlotKind::Version => b"<version>",
        SlotKind::Quoted => b"<quoted>",
    }
}

/// Non-ASCII counts as word, so we never mask inside a non-English identifier.
fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

fn at_boundary(line: &[u8], at: usize) -> bool {
    line.get(at).is_none_or(|&b| !is_word(b))
}

/// Skips a CSI sequence (`ESC [ params final`) or a two-byte escape.
fn skip_escape(line: &[u8], at: usize) -> usize {
    if line.get(at + 1) != Some(&b'[') {
        return (at + 2).min(line.len());
    }
    line[at + 2..]
        .iter()
        .position(|b| (0x40..=0x7e).contains(b))
        .map_or(line.len(), |final_byte| at + 3 + final_byte)
}

fn scan_slot(line: &[u8], at: usize) -> Option<(SlotKind, usize)> {
    match line[at] {
        b'"' | b'\'' => scan_quoted(line, at),
        b if b.is_ascii_hexdigit() => scan_uuid(line, at)
            .or_else(|| scan_hex(line, at))
            .or_else(|| scan_number(line, at)),
        _ => None,
    }
}

fn scan_quoted(line: &[u8], at: usize) -> Option<(SlotKind, usize)> {
    let quote = line[at];
    let end = at + 2 + line[at + 1..].iter().position(|&b| b == quote)?;
    // An apostrophe inside a word ("don't") is not a closing quote.
    if quote == b'\'' && !at_boundary(line, end) {
        return None;
    }
    Some((SlotKind::Quoted, end))
}

fn hex_run(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|b| b.is_ascii_hexdigit()).count()
}

fn digit_run(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|b| b.is_ascii_digit()).count()
}

fn scan_uuid(line: &[u8], at: usize) -> Option<(SlotKind, usize)> {
    let mut end = at;
    for (group, len) in [8, 4, 4, 4, 12].into_iter().enumerate() {
        if group > 0 {
            if line.get(end) != Some(&b'-') {
                return None;
            }
            end += 1;
        }
        if hex_run(line.get(end..)?) != len {
            return None;
        }
        end += len;
    }
    at_boundary(line, end).then_some((SlotKind::Uuid, end))
}

fn scan_hex(line: &[u8], at: usize) -> Option<(SlotKind, usize)> {
    let run = &line[at..at + hex_run(&line[at..])];
    let mixed = run.iter().any(u8::is_ascii_digit) && run.iter().any(u8::is_ascii_alphabetic);
    (run.len() >= 8 && mixed && at_boundary(line, at + run.len()))
        .then_some((SlotKind::Hex, at + run.len()))
}

fn scan_number(line: &[u8], at: usize) -> Option<(SlotKind, usize)> {
    let mut end = at + digit_run(&line[at..]);
    if end == at {
        return None;
    }
    let mut kind = SlotKind::Int;
    if line.get(end) == Some(&b'.') && line.get(end + 1).is_some_and(u8::is_ascii_digit) {
        end += 1 + digit_run(&line[end + 1..]);
        kind = SlotKind::Float;
    }
    if let Some(unit_end) = duration_unit(line, end) {
        return Some((SlotKind::Duration, unit_end));
    }
    at_boundary(line, end).then_some((kind, end))
}

fn duration_unit(line: &[u8], at: usize) -> Option<usize> {
    const ATTACHED: [&[u8]; 8] = [
        b"ns",
        b"us",
        "µs".as_bytes(),
        b"ms",
        b"s",
        b"sec",
        b"secs",
        b"min",
    ];
    const WORDS: [&[u8]; 4] = [b"seconds", b"second", b"milliseconds", b"minutes"];
    let rest = &line[at..];
    let attached = ATTACHED.iter().map(|unit| (rest, *unit, at));
    let spaced = rest.strip_prefix(b" ").unwrap_or(rest);
    let offset = at + rest.len() - spaced.len();
    let words = WORDS.iter().map(|unit| (spaced, *unit, offset));
    attached
        .chain(words)
        .find(|(bytes, unit, start)| {
            bytes.starts_with(unit) && at_boundary(line, start + unit.len())
        })
        .map(|(_, unit, start)| start + unit.len())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(line: &str) -> String {
        String::from_utf8(
            Normalizer::new()
                .normalize(line.as_bytes())
                .template
                .to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn masks_incidental_values() {
        let cases = [
            (
                "GET /users/123 200 in 12.3ms",
                "GET /users/<int> <int> in <duration>",
            ),
            (
                "job 3fa85f64-5717-4562-b3fc-2c963f66afa6 done",
                "job <uuid> done",
            ),
            ("commit deadbeef12 pushed", "commit <hex> pushed"),
            ("Finished in 1.5 seconds", "Finished in <duration>"),
            ("ratio 0.75, took 3µs", "ratio <float>, took <duration>"),
            ("name = 'bob' and \"x y\"", "name = <quoted> and <quoted>"),
            (
                "don't mask user123 or v2 or 5m",
                "don't mask user123 or v2 or 5m",
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(template(line), expected, "{line}");
        }
    }

    #[test]
    fn strips_ansi_so_colored_and_plain_lines_share_a_template() {
        let colored = "\x1b[1m\x1b[36mUser Load (0.3ms)\x1b[0m  SELECT * FROM users WHERE id = 5";
        let plain = "User Load (1.9ms)  SELECT * FROM users WHERE id = 77";
        assert_eq!(
            template(colored),
            "User Load (<duration>)  SELECT * FROM users WHERE id = <int>"
        );
        let mut normalizer = Normalizer::new();
        let colored_hash = normalizer.normalize(colored.as_bytes()).template_hash;
        assert_eq!(
            normalizer.normalize(plain.as_bytes()).template_hash,
            colored_hash
        );
    }

    #[test]
    fn slot_spans_index_the_original_line() {
        let line = b"\x1b[36mid=42\x1b[0m took 7ms";
        let mut normalizer = Normalizer::new();
        let normalized = normalizer.normalize(line);
        let values: Vec<_> = normalized
            .slots
            .iter()
            .map(|s| (s.kind, &line[s.start as usize..s.end as usize]))
            .collect();
        assert_eq!(
            values,
            [(SlotKind::Int, &b"42"[..]), (SlotKind::Duration, b"7ms")]
        );
    }
}
