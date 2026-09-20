//! `siftr follow`: report each shape on stdin the first time it is seen, while the input is still open.
//!
//! No run, no baseline, nothing stored. A stream nobody ever closes has no run boundary to choose, and
//! choosing one is a windowing question `docs/findings/log-contexts.md` leaves open — on exactly this kind
//! of input a whole-log comparison produced 10,362 signals and so reported nothing at all. What such a
//! stream does have is novelty, and the same measurements say that is where the value is: 100 templates
//! carry 70% of a real log's lines, so a follow falls quiet after a short warmup.
//!
//! Shapes come from the same [`Analyzer`] a recorded run uses, so a follow and an `ingest` of the same
//! bytes can never disagree about what a template is.

use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use serde_json::json;
use siftr::aggregate::{Exemplar, MAX_BEHAVIORS, overflow_behavior};
use siftr::analyze::Analyzer;
use siftr::behavior::{Behavior, BehaviorId};
use siftr::normalize::secrets::{Mode, Redactor, Scanner};
use siftr::observation::{LineSplitter, Observation, RawLine, Stream};

use super::Globals;
use crate::output;
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// One compact JSON object per line: seq, stream, behavior, kind, template
    #[arg(short = 'J', long)]
    ndjson: bool,
}

const CHUNK_BYTES: usize = 64 * 1024;

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    if globals.json {
        return Err(output::usage(
            "-j can't be used with follow: a pretty JSON document has one beginning and one end, and a \
             follow has neither; -J prints one compact object per line",
        ));
    }
    let mut splitter = LineSplitter::new();
    let mut lines = Lines::new()?;
    // `io::stdout()` is a LineWriter, so each shape reaches a pipe as it is written. Wrapping this in a
    // BufWriter for speed would make it block-buffered, and a follow would go silent until it filled.
    let mut out = io::stdout().lock();
    let overflow = overflow_behavior().id;
    let mut capped = false;
    let _ = writeln!(
        io::stderr(),
        "siftr: following stdin; reporting each shape the first time it is seen"
    );

    let mut input = io::stdin().lock();
    let mut buf = vec![0; CHUNK_BYTES];
    loop {
        let read = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading stdin"),
        };
        splitter.feed_raw(&buf[..read], |raw| lines.observe(raw));
        report(
            &mut lines.analyzer,
            &mut out,
            args.ndjson,
            overflow,
            &mut capped,
        )?;
    }
    // The last line of a stream may have no newline, and `ingest` records it: report it too.
    splitter.finish_raw(|raw| lines.observe(raw));
    report(
        &mut lines.analyzer,
        &mut out,
        args.ndjson,
        overflow,
        &mut capped,
    )?;
    Ok(ExitCode::SUCCESS)
}

/// The per-line half of a follow: redact, then template. The two steps `record::Sink` takes before the
/// store, without the store — a follow keeps nothing.
struct Lines {
    stream: Stream,
    redactor: Redactor,
    scanner: Scanner,
    analyzer: Analyzer,
}

impl Lines {
    fn new() -> Result<Self> {
        let home = std::env::var_os("HOME");
        Ok(Lines {
            stream: Stream::Stdout,
            redactor: Redactor::new(home.as_ref().map(|home| home.as_bytes())),
            scanner: Scanner::default(),
            // The same roots a recorded run canonicalizes under; different ones would template paths
            // differently and a follow would disagree with `ingest` about the shape.
            analyzer: Analyzer::with_roots(project::roots(&project::current()?.project)),
        })
    }

    fn observe(&mut self, raw: RawLine<'_>) {
        // Always `Secrets`: a follow prints templates and never a raw line, and the masked view templates
        // are built from is the same under every mode. `Pii` would only scan for an evidence view that is
        // then thrown away, once per line.
        let views = self
            .redactor
            .line(&mut self.scanner, raw.body().0, Mode::Secrets);
        self.analyzer.observe(Observation {
            stream: &self.stream,
            seq: raw.seq,
            line: views.masked,
            raw_len: raw.raw_len,
        });
    }
}

/// Prints the shapes first seen since the last call: at most one line per new behavior, and nothing at all
/// for a line whose shape has been seen before.
fn report(
    analyzer: &mut Analyzer,
    out: &mut impl Write,
    ndjson: bool,
    overflow: BehaviorId,
    capped: &mut bool,
) -> io::Result<()> {
    let mut wrote = Ok(());
    let mut hit_cap = false;
    analyzer.drain_new_behaviors(|behavior, first| {
        if wrote.is_err() {
            return;
        }
        if behavior.id == overflow {
            // siftr's own marker for events past the cap, not a shape this input has.
            hit_cap = true;
            return;
        }
        wrote = shape(out, ndjson, behavior, first);
    });
    if hit_cap && !*capped {
        *capped = true;
        output::warn(format_args!(
            "{MAX_BEHAVIORS} distinct shapes seen; a new one is no longer reported"
        ));
    }
    wrote
}

/// One shape, as data on stdout. The template is all a follow prints: it is masked under every
/// `SIFTR_REDACT` setting, where a raw line is only masked as far as the setting in force asks.
fn shape(
    out: &mut impl Write,
    ndjson: bool,
    behavior: &Behavior,
    first: &Exemplar,
) -> io::Result<()> {
    if ndjson {
        let document = json!({
            "seq": first.seq,
            "stream": first.stream.to_string(),
            "behavior": behavior.id.to_string(),
            "kind": behavior.kind.as_str(),
            "template": behavior.template,
        });
        return writeln!(out, "{document}");
    }
    // Widest kind is `run.resources`.
    writeln!(
        out,
        "{:>7}  {:<13}  {}",
        first.seq,
        behavior.kind.as_str(),
        behavior.template
    )
}
