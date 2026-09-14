//! One recorded run, shared by `run` and `ingest`: raw bytes to the capture, lines through the analyzer,
//! then aggregates, baseline and signals persisted together.

use std::time::{Instant, SystemTime};

use anyhow::{Context as _, Result};
use siftr_core::analyze::Analyzer;
use siftr_core::baseline::{Baseline, MAX_RUNS};
use siftr_core::context::Context;
use siftr_core::observation::{LineSplitter, Observation, Stream};
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
    pub signals: Vec<StoredSignal>,
}

impl Recording {
    pub fn begin(store: Store, context: Context, command: &str, cwd: &str) -> Result<Self> {
        let new = NewRun {
            context: &context,
            command,
            cwd,
            started_at: SystemTime::now(),
        };
        let run = store.begin_run(&new)?;
        let capture = store.capture(run)?;
        Ok(Recording {
            store,
            run,
            context,
            capture: Some(capture),
            streams: Vec::new(),
            analyzer: Analyzer::new(),
            started: Instant::now(),
        })
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
        splitter.feed(bytes, |seq, line| {
            analyzer.observe(Observation { stream, seq, line });
        });
    }

    /// Keeps the raw capture but leaves the run unfinished; unfinished runs never join a baseline.
    pub fn abandon(self) -> RunId {
        if let Some(capture) = self.capture
            && let Err(error) = capture.finish()
        {
            output::warn(format_args!("raw capture incomplete: {error}"));
        }
        self.run
    }

    pub fn finish(self, exit_code: Option<i32>) -> Result<Recorded> {
        let Recording {
            mut store,
            run,
            context,
            capture,
            mut streams,
            mut analyzer,
            started,
        } = self;
        let wall = started.elapsed();
        for (stream, splitter) in &mut streams {
            splitter.finish(|seq, line| analyzer.observe(Observation { stream, seq, line }));
        }
        if let Some(capture) = capture
            && let Err(error) = capture.finish()
        {
            output::warn(format_args!("raw capture incomplete: {error}"));
        }
        let analysis = analyzer.finish();

        let baseline = store.baseline_runs(&context, run, MAX_RUNS)?;
        let signals = detect(
            &analysis.stats(),
            &Baseline::from_runs(baseline.iter().map(|(_, stats)| stats)),
        );
        let baseline_runs: Vec<RunId> = baseline.iter().map(|(id, _)| *id).collect();
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
            signals: store.signals(run)?,
        })
    }
}
