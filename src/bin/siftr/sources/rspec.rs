//! RSpec's per-example events, from a reporter listener loaded through `SPEC_OPTS`. See `docs/findings/capture.md`.

use std::borrow::Cow;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::process::Command;

use anyhow::{Context as _, Result};
use siftr::context::shell_join;
use tempfile::TempDir;

use super::Source;
use super::rails_log::{RailsLog, Slice};
use crate::output;
use crate::record::Recording;

const LISTENER: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/siftr_rspec_listener.rb"
));
const EVENTS: &str = "rspec.ndjson";
/// The listener writes these as every event's last keys.
const OFFSET_KEY: &[u8] = br#","log_offset":"#;
const INO_KEY: &[u8] = br#","log_ino":"#;

pub struct Rspec {
    log: Option<RailsLog>,
    dir: Option<TempDir>,
}

impl Rspec {
    pub fn new(log: Option<RailsLog>) -> Self {
        Rspec { log, dir: None }
    }
}

impl Source for Rspec {
    fn name(&self) -> &'static str {
        super::RSPEC
    }

    fn prepare(&mut self, command: &mut Command) -> Result<()> {
        let dir = tempfile::Builder::new()
            .prefix("siftr-rspec-")
            .tempdir()
            .context("creating a directory for the listener")?;
        let listener = dir.path().join("siftr_rspec_listener.rb");
        std::fs::write(&listener, LISTENER).context("writing the listener")?;
        let listener = listener
            .to_str()
            .context("the temp directory is not UTF-8")?;
        // Append: RSpec lets a later `--format` replace the user's, but `--require`s accumulate.
        let mut spec_opts = std::env::var_os("SPEC_OPTS").unwrap_or_default();
        if !spec_opts.is_empty() {
            spec_opts.push(" ");
        }
        spec_opts.push(format!("--require {}", shell_join(&[listener])));
        command
            .env("SPEC_OPTS", spec_opts)
            .env("SIFTR_RSPEC_EVENTS", dir.path().join(EVENTS))
            .env_remove("SIFTR_RSPEC_LOG");
        if let Some(log) = &mut self.log {
            match log.snapshot() {
                Ok(()) => {
                    command.env("SIFTR_RSPEC_LOG", log.path());
                }
                Err(error) => {
                    output::warn(format_args!("log/test.log skipped: {error:#}"));
                    self.log = None;
                }
            }
        }
        self.dir = Some(dir);
        Ok(())
    }

    fn collect(&mut self, recording: &mut Recording) -> Result<()> {
        let slice = self.log.as_mut().and_then(|log| {
            log.measure()
                .inspect_err(|error| output::warn(format_args!("log/test.log skipped: {error:#}")))
                .ok()
        });
        let dir = self.dir.take().context("the listener was never prepared")?;
        match File::open(dir.path().join(EVENTS)) {
            Ok(file) => {
                if let Err(error) = feed_events(BufReader::new(file), slice.as_ref(), recording) {
                    output::warn(format_args!("rspec events incomplete: {error}"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => output::warn(
                "no rspec events: the suite didn't start, or a preloader such as spring ignored SPEC_OPTS",
            ),
            Err(error) => output::warn(format_args!("rspec events lost: {error}")),
        }
        slice.map_or(Ok(()), |slice| slice.feed(recording))
    }
}

fn feed_events(
    mut events: impl BufRead,
    slice: Option<&Slice>,
    recording: &mut Recording,
) -> io::Result<()> {
    let stream = super::rspec_events();
    let mut line = Vec::new();
    loop {
        line.clear();
        if events.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let rebased = rebase(&line, |ino, offset| slice?.place(ino, offset));
        recording.chunk(&stream, &rebased);
    }
}

/// Rewrites the listener's trailing `,"log_offset":N,"log_ino":I}` into an offset into the run's log slice,
/// or removes it when `place` can't put it there. Lines without that suffix pass through untouched.
fn rebase(line: &[u8], place: impl Fn(u64, u64) -> Option<u64>) -> Cow<'_, [u8]> {
    let newline = line.ends_with(b"\n");
    let body = line.strip_suffix(b"\n").unwrap_or(line);
    let Some(head) = body
        .windows(OFFSET_KEY.len())
        .rposition(|w| w == OFFSET_KEY)
    else {
        return Cow::Borrowed(line);
    };
    let Some(tail) = body[head + OFFSET_KEY.len()..].strip_suffix(b"}") else {
        return Cow::Borrowed(line);
    };
    let Some(split) = tail.windows(INO_KEY.len()).position(|w| w == INO_KEY) else {
        return Cow::Borrowed(line);
    };
    let (Some(offset), Some(ino)) = (
        number(&tail[..split]),
        number(&tail[split + INO_KEY.len()..]),
    ) else {
        return Cow::Borrowed(line);
    };
    let mut out = body[..head].to_vec();
    if let Some(offset) = place(ino, offset) {
        out.extend_from_slice(OFFSET_KEY);
        out.extend_from_slice(offset.to_string().as_bytes());
    }
    out.push(b'}');
    if newline {
        out.push(b'\n');
    }
    Cow::Owned(out)
}

fn number(digits: &[u8]) -> Option<u64> {
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rebased(line: &str) -> String {
        let place = |ino, offset| (ino == 7 && offset >= 100).then(|| offset - 100);
        String::from_utf8(rebase(line.as_bytes(), place).into_owned()).unwrap()
    }

    #[test]
    fn offsets_are_rebased_onto_the_slice_or_dropped() {
        assert_eq!(
            rebased("{\"event\":\"start\",\"log_offset\":142,\"log_ino\":7}\n"),
            "{\"event\":\"start\",\"log_offset\":42}\n"
        );
        assert_eq!(
            rebased("{\"event\":\"start\",\"log_offset\":142,\"log_ino\":8}\n"),
            "{\"event\":\"start\"}\n",
            "another file's offset can't be placed"
        );
    }

    #[test]
    fn other_lines_pass_through() {
        for line in [
            "{\"event\":\"start\"}\n",
            // A key-like string inside a value is escaped, so it never matches.
            "{\"description\":\",\\\"log_offset\\\":1,\\\"log_ino\\\":7}\"}\n",
            // A killed run's torn last line.
            "{\"event\":\"example\",\"log_offset\":14",
        ] {
            assert_eq!(rebased(line), line);
        }
    }
}
