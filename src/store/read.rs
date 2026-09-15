//! Queries behind the CLI's read commands.

use std::collections::HashMap;
use std::error::Error;
use std::str::FromStr;
use std::time::{Duration, UNIX_EPOCH};

use anyhow::{Result, bail};
use rusqlite::types::Type;
use rusqlite::{OptionalExtension, Row, params};

use crate::aggregate::{
    BehaviorStats, DurationSummary, Exemplar, Measure, MeasureStats, Phase, RunStats, ScopeStats,
    Stats, overflow_behavior,
};
use crate::behavior::{Behavior, BehaviorId};
use crate::context::Context;
use crate::observation::Stream;
use crate::signal::{Attribution, BaselineNumbers, Signal};
use crate::store::write::int;
use crate::store::{RunEnd, RunId, RunRecord, SignalId, Store, StoredSignal, Tier};

/// How to rank a run's behaviors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Count,
    /// Total duration across occurrences.
    Time,
}

/// The overflow behavior's id is a fixed hex hash, so it is safe to inline.
fn run_columns() -> String {
    format!(
        "id, project, context, command, cwd, started_at_ms, wall_ms, exit_code, lines, interrupted,
         (SELECT count FROM aggregates o WHERE o.run_id = runs.id AND o.behavior_id = '{}')",
        overflow_behavior().id
    )
}
/// What [`behavior`] reads, from `behaviors b`.
const BEHAVIOR_COLUMNS: &str = "b.id, b.kind, b.template, b.roles";
const STATS_COLUMNS: &str =
    "a.count, a.errors, a.duration_count, a.duration_total_us, a.p50_us, a.p95_us, a.max_us";

impl Store {
    pub fn run(&self, id: RunId) -> Result<Option<RunRecord>> {
        let sql = format!("SELECT {} FROM runs WHERE id = ?1", run_columns());
        Ok(self.conn.query_row(&sql, [id.0], run_record).optional()?)
    }

    /// The most recent finished run in `project`, in any context.
    pub fn latest_run(&self, project: &str) -> Result<Option<RunRecord>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE project = ?1 AND wall_ms IS NOT NULL ORDER BY id DESC LIMIT 1",
            run_columns()
        );
        Ok(self
            .conn
            .query_row(&sql, params![project], run_record)
            .optional()?)
    }

    /// The most recent finished run of `context`.
    pub fn latest_run_of(&self, context: &Context) -> Result<Option<RunRecord>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE project = ?1 AND context = ?2 AND wall_ms IS NOT NULL
             ORDER BY id DESC LIMIT 1",
            run_columns()
        );
        let args = params![context.project(), context.name()];
        Ok(self.conn.query_row(&sql, args, run_record).optional()?)
    }

    /// Runs in `project`, newest first, finished or not.
    pub fn runs(&self, project: &str, limit: usize) -> Result<Vec<RunRecord>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE project = ?1 ORDER BY id DESC LIMIT ?2",
            run_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![project, int(limit as u64)], run_record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Runs of `context`, newest first, finished or not.
    pub fn runs_of(&self, context: &Context, limit: usize) -> Result<Vec<RunRecord>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE project = ?1 AND context = ?2 ORDER BY id DESC LIMIT ?3",
            run_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![context.project(), context.name(), int(limit as u64)],
            run_record,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Finished, uninterrupted runs of `context` after `after`, oldest first: where a signal of `after` can resolve.
    pub fn runs_after(&self, context: &Context, after: RunId) -> Result<Vec<RunRecord>> {
        let sql = format!(
            "SELECT {} FROM runs WHERE project = ?1 AND context = ?2 AND id > ?3 AND wall_ms IS NOT NULL
             AND interrupted IS NULL ORDER BY id",
            run_columns()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![context.project(), context.name(), after.0],
            run_record,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Up to `limit` finished, uninterrupted runs of `context` before `before`, newest first, with their stats.
    pub fn baseline_runs(
        &self,
        context: &Context,
        before: RunId,
        limit: usize,
    ) -> Result<Vec<(RunId, RunStats)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM runs WHERE project = ?1 AND context = ?2 AND id < ?3 AND wall_ms IS NOT NULL
             AND interrupted IS NULL AND stats_pruned_by IS NULL ORDER BY id DESC LIMIT ?4",
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

    /// Everything the signal rules read about `run`.
    pub fn run_stats(&self, run: RunId) -> Result<RunStats> {
        self.require(run, Tier::Stats)?;
        let mut measures: HashMap<BehaviorId, Vec<Measure>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT behavior_id, name, count, sum, min, max FROM aggregate_measures WHERE run_id = ?1 ORDER BY behavior_id, name",
        )?;
        let rows = stmt.query_map([run.0], |row| {
            let measure = Measure {
                name: row.get(1)?,
                stats: MeasureStats {
                    count: row.get::<_, i64>(2)?.unsigned_abs(),
                    sum: row.get(3)?,
                    min: row.get(4)?,
                    max: row.get(5)?,
                },
            };
            Ok((parsed::<BehaviorId>(row, 0)?, measure))
        })?;
        for row in rows {
            let (id, measure) = row?;
            measures.entry(id).or_default().push(measure);
        }

        let mut sums: HashMap<(BehaviorId, BehaviorId), Vec<(String, f64)>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT behavior_id, scope_id, name, sum FROM aggregate_scope_sums WHERE run_id = ?1 ORDER BY behavior_id, scope_id, name",
        )?;
        let rows = stmt.query_map([run.0], |row| {
            Ok((
                (parsed(row, 0)?, parsed(row, 1)?),
                (row.get::<_, String>(2)?, row.get::<_, f64>(3)?),
            ))
        })?;
        for row in rows {
            let (key, sum) = row?;
            sums.entry(key).or_default().push(sum);
        }

        let mut scopes: HashMap<BehaviorId, Vec<ScopeStats>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT behavior_id, scope_id, count FROM aggregate_scopes WHERE run_id = ?1 ORDER BY behavior_id, scope_id",
        )?;
        let rows = stmt.query_map([run.0], |row| {
            Ok((
                parsed::<BehaviorId>(row, 0)?,
                parsed::<BehaviorId>(row, 1)?,
                row.get::<_, i64>(2)?.unsigned_abs(),
            ))
        })?;
        for row in rows {
            let (id, scope, count) = row?;
            scopes.entry(id).or_default().push(ScopeStats {
                scope,
                count,
                sums: sums.remove(&(id, scope)).unwrap_or_default(),
            });
        }

        let sql = format!(
            "SELECT {BEHAVIOR_COLUMNS}, {STATS_COLUMNS}, a.unattributed, a.first_stream, a.first_seq
             FROM aggregates a JOIN behaviors b ON b.id = a.behavior_id
             WHERE a.run_id = ?1"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([run.0], |row| {
            let behavior = behavior(row, 0)?;
            let first = match row.get::<_, Option<String>>(12)? {
                Some(_) => Some((
                    parsed::<Stream>(row, 12)?,
                    row.get::<_, i64>(13)?.unsigned_abs(),
                )),
                None => None,
            };
            Ok(BehaviorStats {
                first,
                stats: stats(row, 4)?,
                measures: Vec::new(),
                scopes: Vec::new(),
                unattributed: row.get::<_, i64>(11)?.unsigned_abs(),
                behavior,
            })
        })?;
        rows.map(|row| {
            let mut b = row?;
            b.measures = measures.remove(&b.behavior.id).unwrap_or_default();
            b.scopes = scopes.remove(&b.behavior.id).unwrap_or_default();
            Ok(b)
        })
        .collect()
    }

    /// The runs `run`'s signals were judged against, newest first.
    pub fn baseline_of(&self, run: RunId) -> Result<Vec<RunId>> {
        let mut stmt = self
            .conn
            .prepare("SELECT baseline_run_id FROM run_baselines WHERE run_id = ?1 ORDER BY baseline_run_id DESC")?;
        let rows = stmt.query_map([run.0], |row| row.get(0).map(RunId))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// In rank order: by group, each group's headline first.
    pub fn signals(&self, run: RunId) -> Result<Vec<StoredSignal>> {
        let sql = format!("{SIGNAL_SELECT} WHERE s.run_id = ?1 ORDER BY s.id");
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
        self.require(run, Tier::Stats)?;
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
        self.require(run, Tier::Stats)?;
        let order_by = match order {
            Order::Count => "a.count DESC",
            Order::Time => "COALESCE(a.duration_total_us, 0) DESC, a.count DESC",
        };
        let sql = format!(
            "SELECT {BEHAVIOR_COLUMNS}, {STATS_COLUMNS} FROM aggregates a JOIN behaviors b ON b.id = a.behavior_id
             WHERE a.run_id = ?1 ORDER BY {order_by}, b.id LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![run.0, int(limit as u64)], |row| {
            Ok((behavior(row, 0)?, stats(row, 4)?))
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
            "SELECT id, kind, template, roles FROM behaviors WHERE id LIKE ?1 ORDER BY id LIMIT 2",
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
        for &run in runs {
            self.require(run, Tier::Stats)?;
        }
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
        self.require(run, Tier::Evidence)?;
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

const SIGNAL_SELECT: &str = "SELECT s.id, s.run_id, s.kind, s.measure, s.current, s.baseline_runs, s.present_in,
    s.baseline_median, s.baseline_min, s.baseline_max, s.baseline_failures, s.exception,
    s.attributed, s.scope_id, s.scope_current, s.scope_baseline, s.confidence, s.tier, s.group_rank, s.headline,
    b.id, b.kind, b.template, b.roles,
    sb.id, sb.kind, sb.template, sb.roles,
    (SELECT COUNT(*) FROM exemplars e WHERE e.run_id = s.run_id AND e.behavior_id = s.behavior_id)
    FROM signals s JOIN behaviors b ON b.id = s.behavior_id
    LEFT JOIN behaviors sb ON sb.id = s.scope_id";

fn stored_signal(row: &Row<'_>) -> rusqlite::Result<StoredSignal> {
    let behavior = behavior(row, 20)?;
    let scope = match row.get::<_, Option<String>>(24)? {
        Some(_) => Some(self::behavior(row, 24)?),
        None => None,
    };
    let attribution = match row.get::<_, bool>(12)? {
        true => Some(Attribution {
            scope: Phase::from_scope_id(match row.get::<_, Option<String>>(13)? {
                Some(_) => Some(parsed(row, 13)?),
                None => None,
            }),
            current: row.get::<_, Option<f64>>(14)?.unwrap_or(0.0),
            baseline: row.get::<_, Option<f64>>(15)?.unwrap_or(0.0),
        }),
        false => None,
    };
    Ok(StoredSignal {
        id: SignalId(row.get(0)?),
        run: RunId(row.get(1)?),
        signal: Signal {
            kind: parsed(row, 2)?,
            behavior: behavior.id,
            measure: row.get(3)?,
            current: row.get(4)?,
            baseline: BaselineNumbers {
                runs: row.get(5)?,
                present_in: row.get(6)?,
                median: row.get(7)?,
                min: row.get(8)?,
                max: row.get(9)?,
                failures: row.get(10)?,
            },
            exception: row.get(11)?,
            attribution,
            confidence: row.get(16)?,
            tier: row.get(17)?,
            group: row.get(18)?,
            headline: row.get(19)?,
        },
        behavior,
        scope,
        exemplars: row.get::<_, i64>(28)?.unsigned_abs(),
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
        interrupted: row.get(9)?,
        overflow_events: row.get::<_, Option<i64>>(10)?.unwrap_or(0).unsigned_abs(),
    })
}

fn behavior(row: &Row<'_>, at: usize) -> rusqlite::Result<Behavior> {
    Ok(Behavior {
        id: parsed(row, at)?,
        kind: parsed(row, at + 1)?,
        template: row.get(at + 2)?,
        roles: parsed(row, at + 3)?,
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

pub(crate) fn parsed<T>(row: &Row<'_>, at: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: Error + Send + Sync + 'static,
{
    let text: String = row.get(at)?;
    text.parse()
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(at, Type::Text, Box::new(e)))
}
