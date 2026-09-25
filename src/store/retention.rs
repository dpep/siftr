//! Retention: how much siftr keeps, so a data dir left alone for months stays bounded.
//!
//! Per context, newest runs first: stats for the last `SIFTR_KEEP_RUNS`, evidence (exemplar lines and the raw
//! capture, most of the bytes) for the last `SIFTR_KEEP_EVIDENCE`. A context idle for `SIFTR_KEEP_DAYS` keeps
//! neither: every distinct command line is its own context, as is every `--context` name a read is given, so
//! their number grows too, not just their runs.
//!
//! Past the limits, still kept: a run still recording (it holds a lock in its run dir until it finishes, which
//! the OS drops if siftr dies), and what answers about the latest usable run read. `changes` reads its baseline
//! and re-judges those runs' signals against their own baselines, so all of those keep stats; `explain` of a
//! reminded signal shows evidence from its run, or for a disappearance from the run just before, so those keep
//! evidence. An unfinished run whose lock is free was abandoned, and goes by the same rules as any other.
//!
//! A pruned run stays a row with its signals, baselines and feedback, so ids are never reused and reading
//! what was pruned fails with [`Pruned`], naming the setting, instead of reading as empty.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Statement, TransactionBehavior, params};

use crate::baseline::MAX_RUNS;
use crate::context::Context;
use crate::store::write::unix_ms;
use crate::store::{BUSY_WAIT, RunId, Store, is_busy, schema};

/// How long a run's finish may keep starting prune steps; the rest waits for the next run or `siftr gc`.
pub(crate) const FINISH_BUDGET: Duration = Duration::from_millis(500);
/// Usable runs whose stats a live answer reads: the latest, its baseline, and theirs, since `changes`
/// re-judges the baseline runs' own signals to keep an unfixed regression in view.
const LIVE_STATS: u64 = 2 * MAX_RUNS as u64 + 1;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// An unfinished run without a recording lock (recorded before there was one) younger than this may still be
/// recording.
const LIVE_MS: i64 = DAY_MS;
/// Held by the process recording a run, in the run's dir, until the run finishes.
pub(crate) const RECORDING_LOCK: &str = "recording.lock";
/// Children before parents: foreign keys are on.
const STATS_TABLES: [&str; 5] = [
    "exemplars",
    "aggregate_scope_sums",
    "aggregate_scopes",
    "aggregate_measures",
    "aggregates",
];

/// A retention limit and where its value came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    /// The environment variable that sets it.
    pub env: &'static str,
    pub value: u64,
    pub source: Source,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Default,
    Env,
    /// Read from a config file.
    File {
        path: PathBuf,
    },
    /// Set, but unusable as given: `value` is what siftr uses instead.
    Adjusted {
        given: String,
        why: String,
    },
}

impl fmt::Display for Setting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={}", self.env, self.value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retention {
    /// Runs per context whose stats are kept: what baselines, `explain` and signal outcomes read.
    pub runs: Setting,
    /// Runs per context whose evidence is kept: exemplar lines and raw captures.
    pub evidence: Setting,
    /// Days a context may go unrun before all its runs are pruned.
    pub days: Setting,
}

impl Retention {
    pub fn from_env() -> Self {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Self {
        let runs = setting(&var, "SIFTR_KEEP_RUNS", 100, LIVE_STATS, u64::MAX);
        // The latest run and the run a DISAPPEARED signal's evidence comes from.
        let evidence = setting(&var, "SIFTR_KEEP_EVIDENCE", 20, 2, runs.value);
        let days = setting(&var, "SIFTR_KEEP_DAYS", 30, 1, u64::MAX);
        Retention {
            runs,
            evidence,
            days,
        }
    }

    pub fn settings(&self) -> [&Setting; 3] {
        [&self.runs, &self.evidence, &self.days]
    }
}

fn setting(
    var: &impl Fn(&str) -> Option<String>,
    env: &'static str,
    default: u64,
    min: u64,
    max: u64,
) -> Setting {
    let default = default.clamp(min, max);
    let Some(given) = var(env) else {
        return Setting {
            env,
            value: default,
            source: Source::Default,
        };
    };
    let (value, source) = match given.trim().parse::<u64>() {
        Ok(n) if n < min => {
            let why = format!("below the minimum, {min}");
            (min, Source::Adjusted { given, why })
        }
        Ok(n) if n > max => {
            let why = format!("more than SIFTR_KEEP_RUNS, {max}");
            (max, Source::Adjusted { given, why })
        }
        Ok(n) => (n, Source::Env),
        Err(_) => {
            let why = "not a whole number".to_owned();
            (default, Source::Adjusted { given, why })
        }
    };
    Setting { env, value, source }
}

/// What a pruned run no longer has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Exemplar lines and the raw capture.
    Evidence,
    /// Aggregates and everything under them; the evidence goes with them.
    Stats,
}

/// Reading what retention pruned. `by` is the setting as it was then, like `SIFTR_KEEP_RUNS=100`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pruned {
    pub run: RunId,
    pub tier: Tier,
    pub by: String,
}

impl Pruned {
    /// The environment variable that pruned it.
    pub fn setting(&self) -> &str {
        self.by.split_once('=').map_or(&self.by, |(env, _)| env)
    }
}

impl fmt::Display for Pruned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (env, n) = self.by.split_once('=').unwrap_or((&self.by, "?"));
        let what = match self.tier {
            Tier::Evidence => "evidence was",
            Tier::Stats => "stats and evidence were",
        };
        let why = match env {
            "SIFTR_KEEP_DAYS" => format!("its context hadn't run for {n} days"),
            "SIFTR_KEEP_EVIDENCE" => {
                format!("siftr keeps evidence for the last {n} runs of each context")
            }
            _ => format!("siftr keeps the last {n} runs of each context"),
        };
        write!(f, "{}'s {what} pruned: {why} ({env})", self.run)
    }
}

impl std::error::Error for Pruned {}

/// One run to prune, and the setting that calls for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub run: RunId,
    pub context: Context,
    pub tier: Tier,
    pub by: String,
}

/// What a prune did.
#[derive(Debug, Default)]
pub struct Pruning {
    pub done: Vec<Step>,
    /// Steps left for later: the budget ran out.
    pub pending: usize,
    pub capture_bytes: u64,
}

impl Store {
    pub fn retention(&self) -> &Retention {
        &self.retention
    }

    pub fn set_retention(&mut self, retention: Retention) {
        self.retention = retention;
    }

    /// Fails with [`Pruned`] when retention removed what reading `tier` of `run` needs.
    pub(crate) fn require(&self, run: RunId, tier: Tier) -> Result<()> {
        let pruned: Option<(Option<String>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT stats_pruned_by, evidence_pruned_by FROM runs WHERE id = ?1",
                [run.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (stats, evidence) = pruned.unwrap_or_default();
        let pruned = match (tier, stats, evidence) {
            (_, Some(by), _) => Pruned {
                run,
                tier: Tier::Stats,
                by,
            },
            (Tier::Evidence, None, Some(by)) => Pruned {
                run,
                tier: Tier::Evidence,
                by,
            },
            _ => return Ok(()),
        };
        Err(pruned.into())
    }

    /// What a prune would do now.
    pub fn prune_plan(&self) -> Result<Vec<Step>> {
        plan(&self.conn, &self.home, &self.retention, SystemTime::now())
    }

    /// Prunes runs past the retention limits, oldest first. With a `budget`, one batch that starts no step once
    /// it has elapsed. Without, batches until nothing is due: each its own transaction, so a run finishing
    /// meanwhile waits for one batch, not the whole backlog, and stays within its busy timeout.
    pub fn prune(&mut self, budget: Option<Duration>) -> Result<Pruning> {
        if let Some(budget) = budget {
            return self.prune_batch(budget);
        }
        let mut total = Pruning::default();
        loop {
            let batch = self.prune_batch(FINISH_BUDGET)?;
            let progressed = !batch.done.is_empty();
            total.done.extend(batch.done);
            total.capture_bytes += batch.capture_bytes;
            total.pending = batch.pending;
            if total.pending == 0 || !progressed {
                return Ok(total);
            }
        }
    }

    fn prune_batch(&mut self, budget: Duration) -> Result<Pruning> {
        let started = Instant::now();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let steps = plan(&tx, &self.home, &self.retention, SystemTime::now())?;
        let mut done = Vec::new();
        for step in steps {
            if started.elapsed() >= budget {
                break;
            }
            let tables: &[&str] = match step.tier {
                Tier::Evidence => &STATS_TABLES[..1],
                Tier::Stats => &STATS_TABLES,
            };
            for table in tables {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE run_id = ?1"),
                    [step.run.0],
                )?;
            }
            let pruned_by = match step.tier {
                Tier::Evidence => "UPDATE runs SET evidence_pruned_by = ?1 WHERE id = ?2",
                Tier::Stats => {
                    "UPDATE runs SET stats_pruned_by = ?1, evidence_pruned_by = COALESCE(evidence_pruned_by, ?1) WHERE id = ?2"
                }
            };
            tx.execute(pruned_by, params![step.by, step.run.0])?;
            done.push(step);
        }
        let pending = plan(&tx, &self.home, &self.retention, SystemTime::now())?.len();
        tx.commit()?;
        // Only after the commit: a crash in between leaves a capture `gc` removes, never a run missing one.
        let mut capture_bytes = 0;
        for step in &done {
            capture_bytes += remove_capture(&self.run_dir(step.run))?;
        }
        Ok(Pruning {
            done,
            pending,
            capture_bytes,
        })
    }

    /// A finished run's upkeep: bounded, and never the run's failure, since the run is recorded by now. It doesn't
    /// wait on another siftr's writes: the next finish, or `siftr gc`, prunes instead.
    pub(crate) fn prune_after_finish(&mut self) {
        for setting in self.retention.settings() {
            if let Source::Adjusted { given, why } = &setting.source {
                warn(format_args!(
                    "{}={given} is {why}; using {}",
                    setting.env, setting.value
                ));
            }
        }
        let _ = self.conn.busy_timeout(Duration::ZERO);
        match self.prune(Some(FINISH_BUDGET)) {
            Err(error) if is_busy(&error) => {}
            Err(error) => warn(format_args!("pruning old runs failed: {error:#}")),
            Ok(_) => {}
        }
        let _ = self.conn.busy_timeout(BUSY_WAIT);
    }

    /// Captures left behind by a prune interrupted between its commit and removing them.
    pub fn orphaned_captures(&self) -> Result<Vec<PathBuf>> {
        orphans(&self.conn, &capture_dirs(&self.home)?)
    }

    /// Removes [`Store::orphaned_captures`], returning the bytes freed.
    pub fn remove_orphaned_captures(&self) -> Result<u64> {
        self.orphaned_captures()?
            .iter()
            .map(|dir| Ok(remove_capture(dir)?))
            .sum()
    }

    /// Bytes in `run`'s raw capture.
    pub fn capture_bytes(&self, run: RunId) -> Result<u64> {
        Ok(dir_bytes(&self.run_dir(run))?)
    }

    /// The database's size with its write-ahead log, and how much of it is free pages.
    pub fn database_bytes(&self) -> Result<(u64, u64)> {
        let free = free_bytes(&self.conn)?;
        let db = self.home.join("siftr.db");
        Ok((file_len(&db)? + file_len(&wal(&db))?, free))
    }

    /// Returns free pages to the file system. Rewrites the database, holding its write lock throughout.
    pub fn vacuum(&self) -> Result<()> {
        self.conn.execute_batch("VACUUM")?;
        self.conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        Ok(())
    }

    /// The data dir at `home` as `siftr status` reports it: read-only, so it neither migrates nor creates.
    pub fn inspect(home: &Path, retention: Retention) -> Result<Inspection> {
        let dirs = capture_dirs(home)?;
        let captures = Captures {
            bytes: dirs.iter().map(|d| d.bytes).sum(),
            runs: dirs.len() as u64,
        };
        let db = home.join("siftr.db");
        let database = match db.try_exists()? {
            true => Some(database(home, &db, &retention, &dirs)?),
            false => None,
        };
        Ok(Inspection {
            home: home.to_owned(),
            retention,
            captures,
            database,
        })
    }
}

/// A data dir, as `siftr status` reports it.
#[derive(Debug)]
pub struct Inspection {
    pub home: PathBuf,
    pub retention: Retention,
    pub captures: Captures,
    /// `None` when nothing has been recorded yet.
    pub database: Option<Database>,
}

#[derive(Debug, Default)]
pub struct Captures {
    pub bytes: u64,
    /// Runs with a capture directory.
    pub runs: u64,
}

#[derive(Debug)]
pub struct Database {
    /// With its write-ahead log.
    pub bytes: u64,
    /// Free pages: space a vacuum returns to the file system.
    pub free_bytes: u64,
    pub schema: i64,
    /// The schema this siftr reads and writes.
    pub supported: i64,
    /// Read only when `schema == supported`; empty otherwise.
    pub contexts: Vec<ContextRuns>,
    pub pending: Vec<Step>,
    pub orphaned: Vec<PathBuf>,
}

/// One context's runs.
#[derive(Debug)]
pub struct ContextRuns {
    pub context: Context,
    pub runs: u64,
    pub with_stats: u64,
    pub with_evidence: u64,
    pub oldest: (RunId, SystemTime),
    pub newest: (RunId, SystemTime),
    pub capture_bytes: u64,
}

fn database(
    home: &Path,
    db: &Path,
    retention: &Retention,
    dirs: &[CaptureDir],
) -> Result<Database> {
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {} read-only", db.display()))?;
    let schema: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let supported = schema::supported();
    let mut database = Database {
        bytes: file_len(db)? + file_len(&wal(db))?,
        free_bytes: free_bytes(&conn)?,
        schema,
        supported,
        contexts: Vec::new(),
        pending: Vec::new(),
        orphaned: Vec::new(),
    };
    if schema == supported {
        database.contexts = contexts(&conn, dirs)?;
        database.pending = plan(&conn, home, retention, SystemTime::now())?;
        database.orphaned = orphans(&conn, dirs)?;
    }
    Ok(database)
}

fn contexts(conn: &Connection, dirs: &[CaptureDir]) -> Result<Vec<ContextRuns>> {
    let mut stmt = conn.prepare("SELECT id, project, context FROM runs")?;
    let context_of: HashMap<i64, (String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, (row.get(1)?, row.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?;
    let mut capture_bytes: HashMap<&(String, String), u64> = HashMap::new();
    for dir in dirs {
        if let Some(key) = dir.run.and_then(|run| context_of.get(&run.0)) {
            *capture_bytes.entry(key).or_default() += dir.bytes;
        }
    }
    let mut stmt = conn.prepare(
        "SELECT project, context, COUNT(*), SUM(stats_pruned_by IS NULL), SUM(evidence_pruned_by IS NULL),
                MIN(id), MIN(started_at_ms), MAX(id), MAX(started_at_ms)
         FROM runs GROUP BY project, context ORDER BY COUNT(*) DESC, MAX(id) DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        let key: (String, String) = (row.get(0)?, row.get(1)?);
        let count = |at| row.get::<_, i64>(at).map(i64::unsigned_abs);
        let at = |id, ms| -> rusqlite::Result<(RunId, SystemTime)> {
            Ok((RunId(row.get(id)?), from_ms(row.get(ms)?)))
        };
        Ok(ContextRuns {
            capture_bytes: capture_bytes.get(&key).copied().unwrap_or_default(),
            context: Context::named(key.0, key.1),
            runs: count(2)?,
            with_stats: count(3)?,
            with_evidence: count(4)?,
            oldest: at(5, 6)?,
            newest: at(7, 8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

struct Candidate {
    id: i64,
    project: String,
    context: String,
    started_ms: i64,
    finished: bool,
    interrupted: bool,
    evidence_kept: bool,
}

/// Steps past the limits, oldest run first. Never a run still recording, nor what answers about the latest usable
/// run read (see the module docs).
fn plan(
    conn: &Connection,
    home: &Path,
    retention: &Retention,
    now: SystemTime,
) -> Result<Vec<Step>> {
    let mut stmt = conn.prepare(
        "SELECT id, project, context, started_at_ms, wall_ms IS NOT NULL, interrupted IS NOT NULL,
                evidence_pruned_by IS NULL
         FROM runs WHERE stats_pruned_by IS NULL ORDER BY project, context, id DESC",
    )?;
    let runs = stmt
        .query_map([], |row| {
            Ok(Candidate {
                id: row.get(0)?,
                project: row.get(1)?,
                context: row.get(2)?,
                started_ms: row.get(3)?,
                finished: row.get(4)?,
                interrupted: row.get(5)?,
                evidence_kept: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let now = unix_ms(now);
    let idle_after = i64::try_from(retention.days.value)
        .unwrap_or(i64::MAX)
        .saturating_mul(DAY_MS);
    let mut baselines =
        conn.prepare("SELECT baseline_run_id FROM run_baselines WHERE run_id = ?1")?;
    let mut steps = Vec::new();
    for context in runs.chunk_by(|a, b| (&a.project, &a.context) == (&b.project, &b.context)) {
        let idle = now.saturating_sub(context[0].started_ms) > idle_after;
        let (stats_from, evidence_from) =
            match context.iter().find(|run| run.finished && !run.interrupted) {
                Some(latest) => live_reads(&mut baselines, latest.id)?,
                None => (i64::MAX, i64::MAX),
            };
        let mut usable = 0;
        for (position, run) in (0_u64..).zip(context) {
            let is_usable = run.finished && !run.interrupted;
            usable += u64::from(is_usable);
            // However idle: a dev server left running for days is still recording.
            if !run.finished && recording(home, RunId(run.id), run.started_ms, now) {
                continue;
            }
            let (tier, setting) = if idle {
                (Tier::Stats, &retention.days)
            } else if position >= retention.runs.value
                && run.id < stats_from
                && !(is_usable && usable <= LIVE_STATS)
            {
                (Tier::Stats, &retention.runs)
            } else if position >= retention.evidence.value
                && run.evidence_kept
                && run.id < evidence_from
            {
                (Tier::Evidence, &retention.evidence)
            } else {
                continue;
            };
            steps.push(Step {
                run: RunId(run.id),
                context: Context::named(run.project.as_str(), run.context.as_str()),
                tier,
                by: setting.to_string(),
            });
        }
    }
    steps.sort_by_key(|step| step.run);
    Ok(steps)
}

/// The oldest runs whose (stats, evidence) answers about `latest` read: its baseline and those runs' baselines
/// for stats; its baseline and the newest run each of those was judged against for evidence.
fn live_reads(baselines: &mut Statement<'_>, latest: i64) -> rusqlite::Result<(i64, i64)> {
    let (mut stats, mut evidence) = (latest, latest);
    for run in baselines_of(baselines, latest)? {
        let theirs = baselines_of(baselines, run)?;
        stats = stats
            .min(run)
            .min(theirs.iter().copied().min().unwrap_or(run));
        evidence = evidence
            .min(run)
            .min(theirs.iter().copied().max().unwrap_or(run));
    }
    Ok((stats, evidence))
}

fn baselines_of(baselines: &mut Statement<'_>, run: i64) -> rusqlite::Result<Vec<i64>> {
    baselines.query_map([run], |row| row.get(0))?.collect()
}

/// Whether unfinished `run` may still be recording: its recorder holds the lock until it finishes, and the OS
/// drops it when siftr dies. A run recorded before that lock existed counts as live for a day.
fn recording(home: &Path, run: RunId, started_ms: i64, now: i64) -> bool {
    let lock = home.join("runs").join(run.to_string()).join(RECORDING_LOCK);
    match File::open(lock) {
        Ok(file) => file.try_lock().is_err(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            now.saturating_sub(started_ms) < LIVE_MS
        }
        Err(_) => true,
    }
}

struct CaptureDir {
    path: PathBuf,
    run: Option<RunId>,
    bytes: u64,
}

fn capture_dirs(home: &Path) -> Result<Vec<CaptureDir>> {
    let runs = home.join("runs");
    let entries = match std::fs::read_dir(&runs) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        entries => entries.with_context(|| format!("reading {}", runs.display()))?,
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let run = name
            .to_str()
            .filter(|name| name.starts_with('r'))
            .and_then(|name| name.parse().ok());
        let path = entry.path();
        dirs.push(CaptureDir {
            bytes: dir_bytes(&path)?,
            path,
            run,
        });
    }
    dirs.sort_by_key(|dir| dir.run);
    Ok(dirs)
}

/// Capture dirs of runs whose evidence was pruned. A dir with no run at all is left alone: after a reset
/// database, a new run may be about to take its id.
fn orphans(conn: &Connection, dirs: &[CaptureDir]) -> Result<Vec<PathBuf>> {
    let mut stmt = conn.prepare("SELECT evidence_pruned_by IS NOT NULL FROM runs WHERE id = ?1")?;
    let mut orphans = Vec::new();
    for dir in dirs {
        let Some(run) = dir.run else { continue };
        if stmt.query_row([run.0], |row| row.get(0)).optional()? == Some(true) {
            orphans.push(dir.path.clone());
        }
    }
    Ok(orphans)
}

fn remove_capture(dir: &Path) -> io::Result<u64> {
    let bytes = dir_bytes(dir)?;
    match std::fs::remove_dir_all(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        removed => removed.map(|()| bytes),
    }
}

/// A capture dir holds one file per stream, no subdirectories.
fn dir_bytes(dir: &Path) -> io::Result<u64> {
    match std::fs::read_dir(dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
        Ok(entries) => entries.map(|entry| Ok(entry?.metadata()?.len())).sum(),
    }
}

fn file_len(path: &Path) -> io::Result<u64> {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        metadata => metadata.map(|m| m.len()),
    }
}

fn wal(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

fn free_bytes(conn: &Connection) -> rusqlite::Result<u64> {
    let pragma =
        |name: &str| conn.query_row(&format!("PRAGMA {name}"), [], |row| row.get::<_, i64>(0));
    Ok((pragma("freelist_count")? * pragma("page_size")?).unsigned_abs())
}

fn from_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.unsigned_abs())
}

/// The CLI's warning format: the run is already recorded, so upkeep can only say what went wrong.
fn warn(message: fmt::Arguments<'_>) {
    eprintln!("siftr: warning: {message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_come_from_the_environment_within_their_floors() {
        let vars = |pairs: &'static [(&str, &str)]| {
            Retention::from_vars(move |name| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            })
        };
        let defaults = vars(&[]);
        assert_eq!(
            defaults.settings().map(|s| (s.value, s.source.clone())),
            [100, 20, 30].map(|v| (v, Source::Default))
        );

        let set = vars(&[
            ("SIFTR_KEEP_RUNS", "3"),
            ("SIFTR_KEEP_EVIDENCE", "50"),
            ("SIFTR_KEEP_DAYS", "x"),
        ]);
        assert_eq!(
            set.runs.value, 21,
            "never below what the latest run's changes read"
        );
        assert_eq!(set.evidence.value, 21, "evidence never outlives stats");
        assert_eq!(set.days.value, 30, "unparseable keeps the default");
        assert!(
            set.settings()
                .iter()
                .all(|s| matches!(s.source, Source::Adjusted { .. }))
        );

        assert_eq!(
            vars(&[("SIFTR_KEEP_EVIDENCE", "5")]).evidence.source,
            Source::Env
        );
    }
}
