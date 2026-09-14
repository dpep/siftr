//! Queries behind the CLI's read commands.

use std::error::Error;
use std::str::FromStr;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, bail};
use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, params};
use siftr_core::aggregate::{DurationSummary, Exemplar, RunStats, Stats};
use siftr_core::baseline::BehaviorBaseline;
use siftr_core::behavior::{Behavior, BehaviorId};
use siftr_core::context::Context;
use siftr_core::signal::Signal;

use crate::write::int;
use crate::{RunEnd, RunId, RunRecord, SignalId, Store, StoredSignal};

/// How to rank a run's behaviors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Count,
    /// Total duration across occurrences.
    Time,
}

const RUN_COLUMNS: &str =
    "id, project, context, command, cwd, started_at_ms, wall_ms, exit_code, lines";
const STATS_COLUMNS: &str =
    "a.count, a.errors, a.duration_count, a.duration_total_us, a.p50_us, a.p95_us, a.max_us";

impl Store {
    pub fn run(&self, id: RunId) -> Result<Option<RunRecord>> {
        let sql = format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1");
        Ok(self.conn.query_row(&sql, [id.0], run_record).optional()?)
    }

    /// The most recent finished run in `project`, in any context.
    pub fn latest_run(&self, project: &str) -> Result<Option<RunRecord>> {
        let sql = format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE project = ?1 AND wall_ms IS NOT NULL ORDER BY id DESC LIMIT 1"
        );
        Ok(self
            .conn
            .query_row(&sql, [project], run_record)
            .optional()?)
    }

    /// Runs in `project`, newest first, finished or not.
    pub fn runs(&self, project: &str, limit: usize) -> Result<Vec<RunRecord>> {
        let sql =
            format!("SELECT {RUN_COLUMNS} FROM runs WHERE project = ?1 ORDER BY id DESC LIMIT ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![project, int(limit as u64)], run_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Up to `limit` finished runs of `context` before `before`, newest first, with their stats.
    pub fn baseline_runs(
        &self,
        context: &Context,
        before: RunId,
        limit: usize,
    ) -> Result<Vec<(RunId, RunStats)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM runs WHERE project = ?1 AND context = ?2 AND id < ?3 AND wall_ms IS NOT NULL
             ORDER BY id DESC LIMIT ?4",
        )?;
        let ids = stmt
            .query_map(
                params![
                    context.project(),
                    context.name(),
                    before.0,
                    int(limit as u64)
                ],
                |row| row.get(0).map(RunId),
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter()
            .map(|id| Ok((id, self.run_stats(id)?)))
            .collect()
    }

    fn run_stats(&self, run: RunId) -> Result<RunStats> {
        let sql =
            format!("SELECT a.behavior_id, {STATS_COLUMNS} FROM aggregates a WHERE a.run_id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([run.0], |row| Ok((parsed(row, 0)?, stats(row, 1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The runs `run`'s signals were judged against, newest first.
    pub fn baseline_of(&self, run: RunId) -> Result<Vec<RunId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT baseline_run_id FROM run_baselines WHERE run_id = ?1 ORDER BY baseline_run_id DESC")?;
        let rows = stmt.query_map([run.0], |row| row.get(0).map(RunId))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Most confident first.
    pub fn signals(&self, run: RunId) -> Result<Vec<StoredSignal>> {
        let sql = format!("{SIGNAL_SELECT} WHERE s.run_id = ?1 ORDER BY s.confidence DESC, s.id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([run.0], stored_signal)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn signal(&self, id: SignalId) -> Result<Option<StoredSignal>> {
        let sql = format!("{SIGNAL_SELECT} WHERE s.id = ?1");
        Ok(self
            .conn
            .query_row(&sql, [id.0], stored_signal)
            .optional()?)
    }

    pub fn behavior_count(&self, run: RunId) -> Result<u64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM aggregates WHERE run_id = ?1",
            [run.0],
            |row| row.get(0),
        )?;
        Ok(count.unsigned_abs())
    }

    pub fn behaviors(
        &self,
        run: RunId,
        order: Order,
        limit: usize,
    ) -> Result<Vec<(Behavior, Stats)>> {
        let order_by = match order {
            Order::Count => "a.count DESC",
            Order::Time => "COALESCE(a.duration_total_us, 0) DESC, a.count DESC",
        };
        let sql = format!(
            "SELECT b.id, b.kind, b.template, {STATS_COLUMNS} FROM aggregates a JOIN behaviors b ON b.id = a.behavior_id
             WHERE a.run_id = ?1 ORDER BY {order_by}, b.id LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![run.0, int(limit as u64)], |row| {
            Ok((behavior(row, 0)?, stats(row, 3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Resolves a behavior id or unique prefix of at least 4 hex digits. Ambiguity is an error.
    pub fn resolve_behavior(&self, prefix: &str) -> Result<Option<Behavior>> {
        let hex = (4..=16).contains(&prefix.len()) && prefix.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex {
            bail!("invalid behavior id {prefix:?} (expected 4 to 16 hex digits)");
        }
        let pattern = format!("{}%", prefix.to_ascii_lowercase());
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, template FROM behaviors WHERE id LIKE ?1 ORDER BY id LIMIT 2",
        )?;
        let mut matches = stmt
            .query_map([pattern], |row| behavior(row, 0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if matches.len() > 1 {
            bail!("behavior id {prefix} is ambiguous; use more digits");
        }
        Ok(matches.pop())
    }

    /// `behavior`'s stats in each of `runs`, zero where it didn't occur.
    pub fn stats_in(&self, behavior: BehaviorId, runs: &[RunId]) -> Result<Vec<(RunId, Stats)>> {
        let sql = format!(
            "SELECT {STATS_COLUMNS} FROM aggregates a WHERE a.run_id = ?1 AND a.behavior_id = ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let id = behavior.to_string();
        runs.iter()
            .map(|&run| {
                let found = stmt
                    .query_row(params![run.0, id], |row| stats(row, 0))
                    .optional()?;
                Ok((run, found.unwrap_or_default()))
            })
            .collect()
    }

    /// The most recent finished run in `project` where `behavior` occurred.
    pub fn latest_run_with(&self, project: &str, behavior: BehaviorId) -> Result<Option<RunId>> {
        let id = self
            .conn
            .query_row(
                "SELECT r.id FROM runs r JOIN aggregates a ON a.run_id = r.id
                 WHERE r.project = ?1 AND a.behavior_id = ?2 AND r.wall_ms IS NOT NULL ORDER BY r.id DESC LIMIT 1",
                params![project, behavior.to_string()],
                |row| row.get(0).map(RunId),
            )
            .optional()?;
        Ok(id)
    }

    /// Kept exemplars in stream order.
    pub fn exemplars(
        &self,
        run: RunId,
        behavior: BehaviorId,
        limit: usize,
    ) -> Result<Vec<Exemplar>> {
        let mut stmt = self.conn.prepare(
            "SELECT stream, seq, line FROM exemplars WHERE run_id = ?1 AND behavior_id = ?2 ORDER BY position LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![run.0, behavior.to_string(), int(limit as u64)],
            |row| {
                Ok(Exemplar {
                    stream: parsed(row, 0)?,
                    seq: row.get::<_, i64>(1)?.unsigned_abs(),
                    line: row.get(2)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

const SIGNAL_SELECT: &str =
    "SELECT s.id, s.run_id, s.kind, s.count, s.baseline_runs, s.present_in, s.baseline_mean,
    s.baseline_spread, s.confidence, b.id, b.kind, b.template
    FROM signals s JOIN behaviors b ON b.id = s.behavior_id";

fn stored_signal(row: &Row<'_>) -> rusqlite::Result<StoredSignal> {
    let behavior = behavior(row, 9)?;
    Ok(StoredSignal {
        id: SignalId(row.get(0)?),
        run: RunId(row.get(1)?),
        signal: Signal {
            kind: parsed(row, 2)?,
            behavior: behavior.id,
            count: row.get::<_, i64>(3)?.unsigned_abs(),
            baseline_runs: row.get(4)?,
            baseline: BehaviorBaseline {
                present_in: row.get(5)?,
                mean_count: row.get(6)?,
                count_spread: row.get(7)?,
            },
            confidence: row.get(8)?,
        },
        behavior,
    })
}

fn run_record(row: &Row<'_>) -> rusqlite::Result<RunRecord> {
    let wall_ms: Option<i64> = row.get(6)?;
    let end = match wall_ms {
        Some(wall_ms) => Some(RunEnd {
            wall: Duration::from_millis(wall_ms.unsigned_abs()),
            exit_code: row.get(7)?,
            lines: row.get::<_, Option<i64>>(8)?.unwrap_or(0).unsigned_abs(),
        }),
        None => None,
    };
    Ok(RunRecord {
        id: RunId(row.get(0)?),
        context: Context::named(row.get::<_, String>(1)?, row.get::<_, String>(2)?),
        command: row.get(3)?,
        cwd: row.get(4)?,
        started_at: UNIX_EPOCH + Duration::from_millis(row.get::<_, i64>(5)?.unsigned_abs()),
        end,
    })
}

fn behavior(row: &Row<'_>, at: usize) -> rusqlite::Result<Behavior> {
    Ok(Behavior {
        id: parsed(row, at)?,
        kind: parsed(row, at + 1)?,
        template: row.get(at + 2)?,
    })
}

/// Reads the seven `STATS_COLUMNS` starting at `at`.
fn stats(row: &Row<'_>, at: usize) -> rusqlite::Result<Stats> {
    let us = |offset| -> rusqlite::Result<Duration> {
        Ok(Duration::from_micros(
            row.get::<_, i64>(at + offset)?.unsigned_abs(),
        ))
    };
    let duration = match row.get::<_, Option<i64>>(at + 2)? {
        Some(count) => Some(DurationSummary {
            count: count.unsigned_abs(),
            total: us(3)?,
            p50: us(4)?,
            p95: us(5)?,
            max: us(6)?,
        }),
        None => None,
    };
    Ok(Stats {
        count: row.get::<_, i64>(at)?.unsigned_abs(),
        errors: row.get::<_, i64>(at + 1)?.unsigned_abs(),
        duration,
    })
}

fn parsed<T>(row: &Row<'_>, at: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    let text: String = row.get(at)?;
    text.parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(at, Type::Text, Box::new(e)))
}
