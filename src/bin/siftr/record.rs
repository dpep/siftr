//! One recorded run, shared by `run` and `ingest`: each line redacted once, then to the capture and through the
//! analyzer, then aggregates, baseline and signals persisted together.

use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use siftr::analyze::{Analysis, Analyzer};
use siftr::baseline::{Baseline, Ineligible, MAX_RUNS};
use siftr::context::Context;
use siftr::normalize::secrets::{Mode, Redactor, Scanner};
use siftr::observation::{LineSplitter, Observation, RawLine, Stream};
use siftr::signal::detect;
use siftr::store::{Capture, Finished, NewRun, RunEnd, RunId, RunRecord, Store, StoredSignal};

use crate::output;
use crate::privacy::Privacy;

pub struct Recording {
    store: Store,
    run: RunId,
    context: Context,
    capture: Option<Capture>,
    streams: Vec<(Stream, LineSplitter, Scanner)>,
    redactor: Redactor,
    redact: Mode,
    analyzer: Analyzer,
    started: Instant,
}

pub struct Recorded {
    pub run: RunRecord,
    pub behaviors: u64,
    /// What the run captured, as [`crate::sources`] names each stream.
    pub sources: Vec<String>,
    pub baseline_runs: Vec<RunId>,
    /// Recent runs of the context left out of the baseline, and why, most recent first.
    pub skipped_runs: Vec<(RunId, Ineligible)>,
    pub signals: Vec<StoredSignal>,
}

/// A run begun in the store, before any analysis: unlike a `Recording` (its interpreters aren't `Send`), it can be
/// begun on another thread.
pub struct Begun {
    store: Store,
    run: RunId,
    context: Context,
    capture: Option<Capture>,
    redact: Mode,
    started: Instant,
}

impl Begun {
    /// `started` is when the input began, not now: `run` begins recording while the command already runs.
    pub fn new(
        store: Store,
        context: Context,
        command: &str,
        cwd: &str,
        started: Instant,
    ) -> Result<Self> {
        let now = SystemTime::now();
        let new = NewRun {
            context: &context,
            command,
            cwd,
            started_at: now.checked_sub(started.elapsed()).unwrap_or(now),
        };
        let privacy = Privacy::from_env();
        let run = store.begin_run(&new)?;
        let capture = match privacy.capture {
            true => Some(store.capture(run)?),
            false => None,
        };
        Ok(Begun {
            store,
            run,
            context,
            capture,
            redact: privacy.redact,
            started,
        })
    }
}

impl From<Begun> for Recording {
    fn from(begun: Begun) -> Self {
        let roots = crate::project::roots(begun.context.project());
        let home = std::env::var_os("HOME");
        Recording {
            store: begun.store,
            run: begun.run,
            context: begun.context,
            capture: begun.capture,
            streams: Vec::new(),
            redactor: Redactor::new(home.as_ref().map(|home| home.as_bytes())),
            redact: begun.redact,
            analyzer: Analyzer::with_roots(roots),
            started: begun.started,
        }
    }
}

impl Recording {
    pub fn begin(
        store: Store,
        context: Context,
        command: &str,
        cwd: &str,
        started: Instant,
    ) -> Result<Self> {
        Begun::new(store, context, command, cwd, started).map(Recording::from)
    }

    /// Raw bytes from `stream`, in arrival order.
    pub fn chunk(&mut self, stream: &Stream, bytes: &[u8]) {
        let index = match self.streams.iter().position(|(known, ..)| known == stream) {
            Some(index) => index,
            None => {
                let fresh = (stream.clone(), LineSplitter::new(), Scanner::default());
                self.streams.push(fresh);
                self.streams.len() - 1
            }
        };
        let (stream, splitter, scanner) = &mut self.streams[index];
        let mut sink = Sink {
            capture: &mut self.capture,
            redactor: &mut self.redactor,
            redact: self.redact,
            analyzer: &mut self.analyzer,
        };
        splitter.feed_raw(bytes, |raw| sink.line(stream, scanner, raw));
    }

    pub fn finish(self, exit_code: Option<i32>) -> Result<Recorded> {
        let (mut store, run, context, wall, analysis, sources) = self.analyze();

        let recent = store.baseline_runs(&context, run, MAX_RUNS)?;
        let current = analysis.stats();
        let baseline = Baseline::from_runs(&current, recent.iter().map(|(id, stats)| (*id, stats)));
        let signals = detect(&current, &baseline);
        let baseline_runs: Vec<RunId> = baseline.keys().copied().collect();
        let skipped_runs = baseline.skipped().to_vec();
        let end = RunEnd {
            wall,
            exit_code,
            lines: analysis.observations,
        };
        store.finish_run(
            run,
            &Finished {
                end,
                analysis: &analysis,
                baseline_runs: &baseline_runs,
                signals: &signals,
            },
        )?;
        Ok(Recorded {
            run: store.run(run)?.context("the finished run is missing")?,
            behaviors: analysis.aggregates.len() as u64,
            sources,
            baseline_runs,
            skipped_runs,
            signals: store.signals(run)?,
        })
    }

    /// Same analysis as `finish`, but kept as evidence only: no baseline comparison, no signals, and the run
    /// itself is excluded from later baselines (its missing tail would read as mass DISAPPEARED).
    ///
    /// `exit_code` is the child's own exit, not assumed from `signal`: a trapping child (RSpec force-quits
    /// only on the second SIGINT) can still exit with its own code rather than dying by the signal.
    pub fn finish_interrupted(self, exit_code: Option<i32>, signal: i32) -> Result<Recorded> {
        let (mut store, run, _, wall, analysis, sources) = self.analyze();

        let end = RunEnd {
            wall,
            exit_code,
            lines: analysis.observations,
        };
        store.finish_interrupted_run(run, end, &analysis, signal)?;
        Ok(Recorded {
            run: store.run(run)?.context("the finished run is missing")?,
            behaviors: analysis.aggregates.len() as u64,
            sources,
            baseline_runs: Vec::new(),
            skipped_runs: Vec::new(),
            signals: Vec::new(),
        })
    }

    /// Drains buffered lines through the analyzer and closes the capture: shared tail of `finish` and
    /// `finish_interrupted`.
    fn analyze(self) -> (Store, RunId, Context, Duration, Analysis, Vec<String>) {
        let Recording {
            store,
            run,
            context,
            mut capture,
            mut streams,
            mut redactor,
            redact,
            mut analyzer,
            started,
        } = self;
        let wall = started.elapsed();
        let mut sink = Sink {
            capture: &mut capture,
            redactor: &mut redactor,
            redact,
            analyzer: &mut analyzer,
        };
        for (stream, splitter, scanner) in &mut streams {
            splitter.finish_raw(|raw| sink.line(stream, scanner, raw));
        }
        let sources = captured(&streams);
        if let Some(capture) = capture
            && let Err(error) = capture.finish()
        {
            output::warn(format_args!("raw capture incomplete: {error}"));
        }
        let mut analysis = analyzer.finish();
        // Kept lines come from the masked view templates need, so pii's extra masking reaches them here.
        if redact == Mode::Pii {
            let exemplars = analysis
                .aggregates
                .iter_mut()
                .flat_map(|a| &mut a.exemplars);
            for exemplar in exemplars {
                let views =
                    redactor.line(&mut Scanner::default(), exemplar.line.as_bytes(), redact);
                if views.evidence != exemplar.line.as_bytes() {
                    exemplar.line = String::from_utf8_lossy(views.evidence).into_owned();
                }
            }
        }
        (store, run, context, wall, analysis, sources)
    }
}

/// What the run captured: the command's own output first, then each side channel as it was fed. A stream opens
/// on its first byte, so this is what arrived rather than what was offered.
fn captured(streams: &[(Stream, LineSplitter, Scanner)]) -> Vec<String> {
    let mut order: Vec<&Stream> = streams.iter().map(|(stream, ..)| stream).collect();
    order.sort_by_key(|stream| match stream {
        Stream::Stdout => 0,
        Stream::Stderr => 1,
        Stream::File(_) => 2,
    });
    order
        .into_iter()
        .map(|stream| crate::sources::name_of(stream).to_owned())
        .collect()
}

/// Where each line goes once redacted.
struct Sink<'r> {
    capture: &'r mut Option<Capture>,
    redactor: &'r mut Redactor,
    redact: Mode,
    analyzer: &'r mut Analyzer,
}

impl Sink<'_> {
    /// A capture write failure warns once and stops the capture only.
    fn line(&mut self, stream: &Stream, scanner: &mut Scanner, raw: RawLine<'_>) {
        let (body, cr) = raw.body();
        let views = self.redactor.line(scanner, body, self.redact);
        // A line past `MAX_LINE` is captured clipped, as analyzed: an unscanned tail could hold a credential.
        if let Some(capture) = self.capture.as_mut() {
            let ending: &[u8] = match (cr, raw.terminated) {
                (true, true) => b"\r\n",
                (true, false) => b"\r",
                (false, true) => b"\n",
                (false, false) => b"",
            };
            if let Err(error) = capture.write_line(stream, views.evidence, ending) {
                output::warn(format_args!("raw capture stopped: {error}"));
                *self.capture = None;
            }
        }
        self.analyzer.observe(Observation {
            stream,
            seq: raw.seq,
            line: views.masked,
            // As the line arrived: the RSpec listener's log offsets count the real file's bytes.
            raw_len: raw.raw_len,
        });
    }
}
