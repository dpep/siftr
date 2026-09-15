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
    /// Bytes the line took in the stream, terminator included. `line` can be shorter: `\r` is stripped
    /// and overlong lines are clipped, so byte offsets into the stream must count this instead.
    pub raw_len: u64,
}

/// Longer lines are truncated, so a stream with no newlines can't grow memory without bound.
/// Truncating rather than splitting keeps `seq` equal to the line number in the capture.
pub const MAX_LINE: usize = 1 << 20;

/// One line as it arrived, before [`Observation`] drops its `\r`: enough to write the line back out.
#[derive(Debug, Clone, Copy)]
pub struct RawLine<'a> {
    pub seq: u64,
    /// At most [`MAX_LINE`] bytes, a trailing `\r` included, without the `\n`.
    pub bytes: &'a [u8],
    /// Bytes the line took in the stream, terminator included.
    pub raw_len: u64,
    /// Whether a `\n` ended it: only a stream's last line may lack one.
    pub terminated: bool,
}

impl<'a> RawLine<'a> {
    /// The line without its `\r`, and whether it had one.
    pub fn body(&self) -> (&'a [u8], bool) {
        match self.bytes.strip_suffix(b"\r") {
            Some(body) => (body, true),
            None => (self.bytes, false),
        }
    }

    fn observation(self, stream: &'a Stream) -> Observation<'a> {
        Observation {
            stream,
            seq: self.seq,
            line: self.body().0,
            raw_len: self.raw_len,
        }
    }
}

/// Splits one stream's byte chunks into numbered lines, carrying a partial line across chunks.
#[derive(Debug, Default)]
pub struct LineSplitter {
    carry: Vec<u8>,
    /// Bytes of the partial line so far, including any past what `carry` keeps.
    carry_len: u64,
    seq: u64,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Calls `on_line` for every line of `stream` completed by `chunk`.
    pub fn feed(
        &mut self,
        stream: &Stream,
        chunk: &[u8],
        mut on_line: impl FnMut(Observation<'_>),
    ) {
        self.feed_raw(chunk, |raw| on_line(raw.observation(stream)));
    }

    /// [`LineSplitter::feed`], each line as it arrived.
    pub fn feed_raw(&mut self, chunk: &[u8], mut on_line: impl FnMut(RawLine<'_>)) {
        let mut rest = chunk;
        while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
            let (head, tail) = rest.split_at(newline);
            rest = &tail[1..];
            self.seq += 1;
            let raw_len = self.carry_len + head.len() as u64 + 1;
            let line = if self.carry.is_empty() {
                head
            } else {
                self.keep(head);
                &self.carry
            };
            on_line(RawLine {
                seq: self.seq,
                bytes: &line[..line.len().min(MAX_LINE)],
                raw_len,
                terminated: true,
            });
            self.carry.clear();
            self.carry_len = 0;
        }
        self.keep(rest);
    }

    fn keep(&mut self, bytes: &[u8]) {
        let room = MAX_LINE.saturating_sub(self.carry.len());
        self.carry
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.carry_len += bytes.len() as u64;
    }

    /// Emits the trailing unterminated line, if any. Call at end of stream.
    pub fn finish(&mut self, stream: &Stream, mut on_line: impl FnMut(Observation<'_>)) {
        self.finish_raw(|raw| on_line(raw.observation(stream)));
    }

    /// [`LineSplitter::finish`], the line as it arrived.
    pub fn finish_raw(&mut self, mut on_line: impl FnMut(RawLine<'_>)) {
        if self.carry_len > 0 {
            self.seq += 1;
            on_line(RawLine {
                seq: self.seq,
                bytes: &self.carry,
                raw_len: self.carry_len,
                terminated: false,
            });
            self.carry.clear();
            self.carry_len = 0;
        }
    }

    /// Lines emitted so far.
    pub fn lines(&self) -> u64 {
        self.seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(seq, line, raw_len)` per line.
    fn split(chunks: &[&[u8]]) -> Vec<(u64, String, u64)> {
        let stream = Stream::Stdout;
        let mut splitter = LineSplitter::new();
        let mut lines = Vec::new();
        let mut push = |obs: Observation<'_>| {
            let line = String::from_utf8_lossy(obs.line).into_owned();
            lines.push((obs.seq, line, obs.raw_len));
        };
        for chunk in chunks {
            splitter.feed(&stream, chunk, &mut push);
        }
        splitter.finish(&stream, &mut push);
        lines
    }

    #[test]
    fn joins_lines_across_chunks_and_numbers_them() {
        let lines = split(&[b"one\ntw", b"o\r\n\nthr", b"ee"]);
        let expected = [(1, "one", 4), (2, "two", 5), (3, "", 1), (4, "three", 5)];
        assert_eq!(
            lines,
            expected.map(|(seq, l, raw)| (seq, l.to_owned(), raw))
        );
    }

    #[test]
    fn truncates_overlong_lines_without_renumbering_or_losing_their_length() {
        let long = vec![b'x'; MAX_LINE + 10];
        let lines = split(&[&long[..MAX_LINE / 2], &long[MAX_LINE / 2..], b"\nnext\n"]);
        let shape: Vec<_> = lines
            .iter()
            .map(|(seq, line, raw)| (*seq, line.len(), *raw))
            .collect();
        assert_eq!(shape, [(1, MAX_LINE, MAX_LINE as u64 + 11), (2, 4, 5)]);
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
