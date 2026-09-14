//! Recording a run: begun before output arrives, finished in one transaction after analysis.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::params;
use siftr_core::aggregate::Stats;
use siftr_core::analyze::Analysis;
use siftr_core::signal::Signal;

use crate::{NewRun, RunEnd, RunId, Store};

/// Everything a finished run persists.
#[derive(Debug, Clone, Copy)]
pub struct Finished<'a> {
    pub end: RunEnd,
    pub analysis: &'a Analysis,
    /// The runs `signals` were judged against.
    pub baseline_runs: &'a [RunId],
    pub signals: &'a [Signal],
}

impl Store {
    pub fn begin_run(&self, run: &NewRun<'_>) -> Result<RunId> {
        self.conn.execute(
            "INSERT INTO runs (project, context, command, cwd, started_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run.context.project(),
                run.context.name(),
                run.command,
                run.cwd,
                unix_ms(run.started_at),
            ],
        )?;
        Ok(RunId(self.conn.last_insert_rowid()))
    }

    pub fn finish_run(&mut self, run: RunId, finished: &Finished<'_>) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE runs SET wall_ms = ?1, exit_code = ?2, lines = ?3 WHERE id = ?4",
            params![
                micros(finished.end.wall) / 1000,
                finished.end.exit_code,
                int(finished.end.lines),
                run.0
            ],
        )?;
        {
            let mut behavior = tx.prepare(
                "INSERT OR IGNORE INTO behaviors (id, kind, template) VALUES (?1, ?2, ?3)",
            )?;
            let mut aggregate = tx.prepare(
                "INSERT INTO aggregates (run_id, behavior_id, count, errors, duration_count, duration_total_us, p50_us, p95_us, max_us)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut exemplar = tx.prepare(
                "INSERT INTO exemplars (run_id, behavior_id, position, stream, seq, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for agg in &finished.analysis.aggregates {
                let id = agg.behavior.id.to_string();
                behavior.execute(params![
                    id,
                    agg.behavior.kind.as_str(),
                    agg.behavior.template
                ])?;
                let Stats {
                    count,
                    errors,
                    duration,
                } = agg.stats;
                aggregate.execute(params![
                    run.0,
                    id,
                    int(count),
                    int(errors),
                    duration.map(|d| int(d.count)),
                    duration.map(|d| micros(d.total)),
                    duration.map(|d| micros(d.p50)),
                    duration.map(|d| micros(d.p95)),
                    duration.map(|d| micros(d.max)),
                ])?;
                for (position, e) in (0_i64..).zip(&agg.exemplars) {
                    exemplar.execute(params![
                        run.0,
                        id,
                        position,
                        e.stream.to_string(),
                        int(e.seq),
                        e.line
                    ])?;
                }
            }
            let mut baseline =
                tx.prepare("INSERT INTO run_baselines (run_id, baseline_run_id) VALUES (?1, ?2)")?;
            for baseline_run in finished.baseline_runs {
                baseline.execute(params![run.0, baseline_run.0])?;
            }
            let mut signal = tx.prepare(
                "INSERT INTO signals (run_id, behavior_id, kind, count, baseline_runs, present_in, baseline_mean, baseline_spread, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for s in finished.signals {
                signal.execute(params![
                    run.0,
                    s.behavior.to_string(),
                    s.kind.as_str(),
                    int(s.count),
                    s.baseline_runs,
                    s.baseline.present_in,
                    s.baseline.mean_count,
                    s.baseline.count_spread,
                    s.confidence,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// SQLite integers are signed; counts this large don't happen, so saturate rather than fail.
pub(crate) fn int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

pub(crate) fn micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

fn unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
    })
}
