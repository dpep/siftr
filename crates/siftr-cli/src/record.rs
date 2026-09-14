//! One recorded run, shared by `run` and `ingest`: raw bytes to the capture, lines through the analyzer,
//! then aggregates, baseline and signals persisted together.

use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use siftr_core::analyze::{Analysis, Analyzer};
use siftr_core::baseline::{Baseline, Ineligible, MAX_RUNS};
use siftr_core::context::Context;
use siftr_core::observation::{LineSplitter, Stream};
use siftr_core::signal::detect;
use siftr_store::{Capture, Finished, NewRun, RunEnd, RunId, RunRecord, Store, StoredSignal};

use crate::output;

pub struct Recording {
    store: Store,
    run: RunId,
    context: Context,
    capture: Option<Capture>,
    streams: Vec<(Stream, LineSplitter)>,
    analyzer: Analyzer,
    started: Instant,
}

pub struct Recorded {
    pub run: RunRecord,
    pub behaviors: u64,
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
    capture: Capture,
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
        let run = store.begin_run(&new)?;
        let capture = store.capture(run)?;
        Ok(Begun {
            store,
            run,
            context,
            capture,
            started,
        })
    }
}

impl From<Begun> for Recording {
    fn from(begun: Begun) -> Self {
        Recording {
            store: begun.store,
            run: begun.run,
            context: begun.context,
            capture: Some(begun.capture),
            streams: Vec::new(),
            analyzer: Analyzer::new(),
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

    /// Raw bytes from `stream`, in arrival order. A capture write failure warns once and stops the capture only.
    pub fn chunk(&mut self, stream: &Stream, bytes: &[u8]) {
        if let Some(capture) = &mut self.capture
            && let Err(error) = capture.write(stream, bytes)
        {
            output::warn(format_args!("raw capture stopped: {error}"));
            self.capture = None;
        }
        let index = match self.streams.iter().position(|(known, _)| known == stream) {
            Some(index) => index,
            None => {
                self.streams.push((stream.clone(), LineSplitter::new()));
                self.streams.len() - 1
            }
        };
        let (stream, splitter) = &mut self.streams[index];
        let analyzer = &mut self.analyzer;
        splitter.feed(stream, bytes, |obs| analyzer.observe(obs));
    }

    pub fn finish(self, exit_code: Option<i32>) -> Result<Recorded> {
        let Recording {
            mut store,
            run,
            context,
            capture,
            streams,
            analyzer,
            started,
        } = self;
        let (wall, analysis) = analyze(capture, streams, analyzer, started);

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
        let Recording {
            mut store,
            run,
            capture,
            streams,
            analyzer,
            started,
            ..
        } = self;
        let (wall, analysis) = analyze(capture, streams, analyzer, started);

        let end = RunEnd {
            wall,
            exit_code,
            lines: analysis.observations,
        };
        store.finish_interrupted_run(run, end, &analysis, signal)?;
        Ok(Recorded {
            run: store.run(run)?.context("the finished run is missing")?,
            behaviors: analysis.aggregates.len() as u64,
            baseline_runs: Vec::new(),
            skipped_runs: Vec::new(),
            signals: Vec::new(),
        })
    }
}

/// Drains buffered lines through the analyzer and closes the capture: shared tail of `finish` and
/// `finish_interrupted`.
fn analyze(
    capture: Option<Capture>,
    mut streams: Vec<(Stream, LineSplitter)>,
    mut analyzer: Analyzer,
    started: Instant,
) -> (Duration, Analysis) {
    let wall = started.elapsed();
    for (stream, splitter) in &mut streams {
        splitter.finish(stream, |obs| analyzer.observe(obs));
    }
    if let Some(capture) = capture
        && let Err(error) = capture.finish()
    {
        output::warn(format_args!("raw capture incomplete: {error}"));
    }
    (wall, analyzer.finish())
}
