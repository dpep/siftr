//! Recording a run: begun before output arrives, finished in one transaction after analysis.

use std::fs::File;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use rusqlite::params;

use crate::aggregate::Stats;
use crate::analyze::Analysis;
use crate::normalize::secrets::redact_text;
use crate::observation::Stream;
use crate::signal::Signal;
use crate::store::{BUSY_WAIT, NewRun, RECORDING_LOCK, RunEnd, RunId, Store, busy, lock_within};

/// A whole run's data is worth a longer wait on another siftr than its start is.
const FINISH_WAIT: Duration = Duration::from_secs(5);

/// Everything a finished run persists.
#[derive(Debug, Clone, Copy)]
pub struct Finished<'a> {
    pub end: RunEnd,
    pub analysis: &'a Analysis,
    /// The runs `signals` were judged against.
    pub baseline_runs: &'a [RunId],
    /// In rank order, as `detect` returns them: signal ids follow it.
    pub signals: &'a [Signal],
}

impl Store {
    /// Records that a run began, and holds its run dir's recording lock until it finishes, so no prune takes it
    /// meanwhile. Waits at most `BUSY_WAIT` on another siftr's writes, then fails with `StoreBusy`.
    pub fn begin_run(&self, run: &NewRun<'_>) -> Result<RunId> {
        self.conn
            .execute(
                "INSERT INTO runs (project, context, command, cwd, started_at_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    run.context.project(),
                    run.context.name(),
                    redact_text(run.command),
                    redact_text(run.cwd),
                    unix_ms(run.started_at),
                ],
            )
            .map_err(|error| busy(&self.home, error.into()))?;
        let id = RunId(self.conn.last_insert_rowid());
        let lock = self.run_dir(id).join(RECORDING_LOCK);
        std::fs::create_dir_all(self.run_dir(id))
            .and_then(|()| File::create(&lock))
            .and_then(|file| {
                // Only a prune checking whether the run is live contends, and only for an instant.
                if lock_within(&file, BUSY_WAIT)? {
                    self.recording.borrow_mut().push((id, file));
                }
                Ok(())
            })
            .with_context(|| format!("locking {}", lock.display()))?;
        Ok(id)
    }

    /// From here a prune may take `run`.
    fn stop_recording(&self, run: RunId) {
        self.recording.borrow_mut().retain(|(held, _)| *held != run);
        let _ = std::fs::remove_file(self.run_dir(run).join(RECORDING_LOCK));
    }

    pub fn finish_run(&mut self, run: RunId, finished: &Finished<'_>) -> Result<()> {
        self.finish(run, finished, None, None)
    }

    /// Records a run whose comparison produced `signals` signals, more than [`crate::signal::MAX_SIGNALS`], so
    /// none of them are kept: that many is a statement that its behaviors don't recur rather than a set of
    /// findings. Everything else is a normal finished run — its evidence stays, and it baselines like any other.
    pub fn finish_run_uncompared(
        &mut self,
        run: RunId,
        end: RunEnd,
        analysis: &Analysis,
        baseline_runs: &[RunId],
        signals: u64,
    ) -> Result<()> {
        let finished = Finished {
            end,
            analysis,
            baseline_runs,
            signals: &[],
        };
        self.finish(run, &finished, None, Some(signals))
    }

    /// Records what an interrupted run saw, as evidence only: no baseline, no signals, and it never
    /// joins a later run's baseline, where its missing tail would read as mass DISAPPEARED.
    pub fn finish_interrupted_run(
        &mut self,
        run: RunId,
        end: RunEnd,
        analysis: &Analysis,
        signal: i32,
    ) -> Result<()> {
        let finished = Finished {
            end,
            analysis,
            baseline_runs: &[],
            signals: &[],
        };
        self.finish(run, &finished, Some(signal), None)
    }

    fn finish(
        &mut self,
        run: RunId,
        finished: &Finished<'_>,
        interrupted: Option<i32>,
        uncompared: Option<u64>,
    ) -> Result<()> {
        self.conn.busy_timeout(FINISH_WAIT)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE runs SET wall_ms = ?1, exit_code = ?2, lines = ?3, interrupted = ?4, uncompared = ?5
             WHERE id = ?6",
            params![
                micros(finished.end.wall) / 1000,
                finished.end.exit_code,
                int(finished.end.lines),
                interrupted,
                uncompared.map(int),
                run.0
            ],
        )?;
        {
            // A behavior recorded before roles existed gains them.
            let mut behavior = tx.prepare(
                "INSERT INTO behaviors (id, kind, template, roles) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET roles = excluded.roles WHERE roles <> excluded.roles",
            )?;
            let mut aggregate = tx.prepare(
                "INSERT INTO aggregates (run_id, behavior_id, count, errors, duration_count, duration_total_us, p50_us, p95_us, max_us, unattributed, first_stream, first_seq)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            )?;
            let mut measure = tx.prepare(
                "INSERT INTO aggregate_measures (run_id, behavior_id, name, count, sum, min, max) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut scope = tx.prepare(
                "INSERT INTO aggregate_scopes (run_id, behavior_id, scope_id, count) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut scope_sum = tx.prepare(
                "INSERT INTO aggregate_scope_sums (run_id, behavior_id, scope_id, name, sum) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut exemplar = tx.prepare(
                "INSERT INTO exemplars (run_id, behavior_id, position, stream, seq, line) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for agg in &finished.analysis.aggregates {
                let id = agg.behavior.id.to_string();
                behavior.execute(params![
                    id,
                    agg.behavior.kind.as_str(),
                    agg.behavior.template,
                    agg.behavior.roles.to_string(),
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
                    int(agg.unattributed),
                    agg.exemplars.first().map(|e| e.stream.to_string()),
                    agg.exemplars.first().map(|e| int(e.seq)),
                ])?;
                for m in &agg.measures {
                    let s = m.stats;
                    measure.execute(params![
                        run.0,
                        id,
                        m.name,
                        int(s.count),
                        s.sum,
                        s.min,
                        s.max
                    ])?;
                }
                for s in &agg.scopes {
                    let scope_id = s.scope.to_string();
                    scope.execute(params![run.0, id, scope_id, int(s.count)])?;
                    for (name, sum) in &s.sums {
                        scope_sum.execute(params![run.0, id, scope_id, name, sum])?;
                    }
                }
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
            // In the same transaction as the aggregates: what was read and what it produced are one fact.
            let mut source =
                tx.prepare("INSERT INTO run_sources (run_id, name, stream) VALUES (?1, ?2, ?3)")?;
            for s in &finished.analysis.sources {
                source.execute(params![
                    run.0,
                    s.name,
                    s.stream.as_ref().map(Stream::to_string)
                ])?;
            }
            let mut signal = tx.prepare(
                "INSERT INTO signals (run_id, behavior_id, kind, measure, current, baseline_runs, present_in,
                    baseline_median, baseline_min, baseline_max, baseline_failures, exception,
                    attributed, scope_id, scope_current, scope_baseline, confidence, tier, group_rank, headline)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            )?;
            for s in finished.signals {
                let b = s.baseline;
                let a = s.attribution;
                signal.execute(params![
                    run.0,
                    s.behavior.to_string(),
                    s.kind.as_str(),
                    s.measure,
                    s.current,
                    b.runs,
                    b.present_in,
                    b.median,
                    b.min,
                    b.max,
                    b.failures,
                    s.exception,
                    a.is_some(),
                    a.and_then(|a| a.scope.scope_id()).map(|id| id.to_string()),
                    a.map(|a| a.current),
                    a.map(|a| a.baseline),
                    s.confidence,
                    s.tier,
                    s.group,
                    s.headline,
                ])?;
            }
        }
        tx.commit()?;
        self.stop_recording(run);
        self.prune_after_finish();
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

pub(crate) fn unix_ms(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
    })
}
