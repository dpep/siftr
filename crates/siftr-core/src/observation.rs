//! Raw records as they arrive from a source, before any interpretation.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Where an observation came from. Exemplars keep it so evidence points back into the right capture.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
    /// A side channel read alongside the command, e.g. `log/test.log` or an RSpec JSON report.
    File(Arc<str>),
}

impl fmt::Display for Stream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Stream::Stdout => f.write_str("stdout"),
            Stream::Stderr => f.write_str("stderr"),
            Stream::File(path) => write!(f, "file:{path}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownStream(pub String);

impl fmt::Display for UnknownStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unknown stream {:?} (expected stdout, stderr or file:<path>)",
            self.0
        )
    }
}

impl std::error::Error for UnknownStream {}

impl FromStr for Stream {
    type Err = UnknownStream;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stdout" => Ok(Stream::Stdout),
            "stderr" => Ok(Stream::Stderr),
            _ => s
                .strip_prefix("file:")
                .filter(|path| !path.is_empty())
                .map(|path| Stream::File(path.into()))
                .ok_or_else(|| UnknownStream(s.to_owned())),
        }
    }
}

/// One raw line from one stream.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    pub stream: &'a Stream,
    /// 1-based line number within `stream`, so it addresses the same line in that stream's capture.
    pub seq: u64,
    /// The line without its terminator. Bytes, because logs are not always UTF-8.
    pub line: &'a [u8],
}

/// Longer lines are truncated, so a stream with no newlines can't grow memory without bound.
/// Truncating rather than splitting keeps `seq` equal to the line number in the capture.
pub const MAX_LINE: usize = 1 << 20;

/// Splits one stream's byte chunks into numbered lines, carrying a partial line across chunks.
#[derive(Debug, Default)]
pub struct LineSplitter {
    carry: Vec<u8>,
    seq: u64,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Calls `on_line(seq, line)` for every line completed by `chunk`.
    pub fn feed(&mut self, chunk: &[u8], mut on_line: impl FnMut(u64, &[u8])) {
        let mut rest = chunk;
        while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
            let (head, tail) = rest.split_at(newline);
            rest = &tail[1..];
            self.seq += 1;
            if self.carry.is_empty() {
                on_line(self.seq, clip(head));
            } else {
                self.keep(head);
                on_line(self.seq, clip(&self.carry));
                self.carry.clear();
            }
        }
        self.keep(rest);
    }

    fn keep(&mut self, bytes: &[u8]) {
        let room = MAX_LINE.saturating_sub(self.carry.len());
        self.carry
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    /// Emits the trailing unterminated line, if any. Call at end of stream.
    pub fn finish(&mut self, mut on_line: impl FnMut(u64, &[u8])) {
        if !self.carry.is_empty() {
            self.seq += 1;
            on_line(self.seq, clip(&self.carry));
            self.carry.clear();
        }
    }

    /// Lines emitted so far.
    pub fn lines(&self) -> u64 {
        self.seq
    }
}

fn clip(line: &[u8]) -> &[u8] {
    let line = &line[..line.len().min(MAX_LINE)];
    line.strip_suffix(b"\r").unwrap_or(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(chunks: &[&[u8]]) -> Vec<(u64, String)> {
        let mut splitter = LineSplitter::new();
        let mut lines = Vec::new();
        let mut push =
            |seq, line: &[u8]| lines.push((seq, String::from_utf8_lossy(line).into_owned()));
        for chunk in chunks {
            splitter.feed(chunk, &mut push);
        }
        splitter.finish(&mut push);
        lines
    }

    #[test]
    fn joins_lines_across_chunks_and_numbers_them() {
        let lines = split(&[b"one\ntw", b"o\r\n\nthr", b"ee"]);
        let expected = [(1, "one"), (2, "two"), (3, ""), (4, "three")];
        assert_eq!(lines, expected.map(|(seq, l)| (seq, l.to_owned())));
    }

    #[test]
    fn truncates_overlong_lines_without_renumbering() {
        let long = vec![b'x'; MAX_LINE + 10];
        let lines = split(&[&long[..MAX_LINE / 2], &long[MAX_LINE / 2..], b"\nnext\n"]);
        let shape: Vec<_> = lines.iter().map(|(seq, line)| (*seq, line.len())).collect();
        assert_eq!(shape, [(1, MAX_LINE), (2, 4)]);
    }

    #[test]
    fn stream_names_round_trip() {
        for stream in [
            Stream::Stdout,
            Stream::Stderr,
            Stream::File("log/test.log".into()),
        ] {
            assert_eq!(stream.to_string().parse::<Stream>(), Ok(stream));
        }
        assert!("file:".parse::<Stream>().is_err());
    }
}
