//! Schema migrations, tracked by SQLite's `user_version`.

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, TransactionBehavior};

use crate::store::{BUSY_WAIT, StoreBusy, lock_within, scrub};

/// Append a migration to change the schema; never edit one that has shipped.
const MIGRATIONS: &[&str] = &[
    r"
CREATE TABLE runs (
    id            INTEGER PRIMARY KEY,
    project       TEXT NOT NULL,
    context       TEXT NOT NULL,
    command       TEXT NOT NULL,
    cwd           TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    -- NULL until the run finishes.
    wall_ms       INTEGER,
    exit_code     INTEGER,
    lines         INTEGER
);
CREATE INDEX runs_by_context ON runs (project, context, id);

CREATE TABLE behaviors (
    id       TEXT PRIMARY KEY,
    kind     TEXT NOT NULL,
    template TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE aggregates (
    run_id            INTEGER NOT NULL REFERENCES runs (id),
    behavior_id       TEXT NOT NULL REFERENCES behaviors (id),
    count             INTEGER NOT NULL,
    errors            INTEGER NOT NULL,
    -- NULL when no event carried a duration.
    duration_count    INTEGER,
    duration_total_us INTEGER,
    p50_us            INTEGER,
    p95_us            INTEGER,
    max_us            INTEGER,
    PRIMARY KEY (run_id, behavior_id)
) WITHOUT ROWID;
CREATE INDEX aggregates_by_behavior ON aggregates (behavior_id, run_id);

CREATE TABLE exemplars (
    run_id      INTEGER NOT NULL,
    behavior_id TEXT NOT NULL,
    position    INTEGER NOT NULL,
    stream      TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    line        TEXT NOT NULL,
    PRIMARY KEY (run_id, behavior_id, position),
    FOREIGN KEY (run_id, behavior_id) REFERENCES aggregates (run_id, behavior_id)
) WITHOUT ROWID;

-- Which runs a run's signals were judged against.
CREATE TABLE run_baselines (
    run_id          INTEGER NOT NULL REFERENCES runs (id),
    baseline_run_id INTEGER NOT NULL REFERENCES runs (id),
    PRIMARY KEY (run_id, baseline_run_id)
) WITHOUT ROWID;

CREATE TABLE signals (
    id              INTEGER PRIMARY KEY,
    run_id          INTEGER NOT NULL REFERENCES runs (id),
    behavior_id     TEXT NOT NULL REFERENCES behaviors (id),
    kind            TEXT NOT NULL,
    count           INTEGER NOT NULL,
    baseline_runs   INTEGER NOT NULL,
    present_in      INTEGER NOT NULL,
    baseline_mean   REAL NOT NULL,
    baseline_spread REAL NOT NULL,
    confidence      REAL NOT NULL
);
CREATE INDEX signals_by_run ON signals (run_id);
",
    r"
-- Scoped events past the per-run attribution cap: counted in `count`, not in any scope.
ALTER TABLE aggregates ADD COLUMN unattributed INTEGER NOT NULL DEFAULT 0;

CREATE TABLE aggregate_measures (
    run_id      INTEGER NOT NULL,
    behavior_id TEXT NOT NULL,
    name        TEXT NOT NULL,
    count       INTEGER NOT NULL,
    sum         REAL NOT NULL,
    min         REAL NOT NULL,
    max         REAL NOT NULL,
    PRIMARY KEY (run_id, behavior_id, name),
    FOREIGN KEY (run_id, behavior_id) REFERENCES aggregates (run_id, behavior_id)
) WITHOUT ROWID;

-- A behavior's occurrences inside one scope (a test example); unscoped ones are the remainder.
CREATE TABLE aggregate_scopes (
    run_id      INTEGER NOT NULL,
    behavior_id TEXT NOT NULL,
    scope_id    TEXT NOT NULL,
    count       INTEGER NOT NULL,
    PRIMARY KEY (run_id, behavior_id, scope_id),
    FOREIGN KEY (run_id, behavior_id) REFERENCES aggregates (run_id, behavior_id)
) WITHOUT ROWID;

CREATE TABLE aggregate_scope_sums (
    run_id      INTEGER NOT NULL,
    behavior_id TEXT NOT NULL,
    scope_id    TEXT NOT NULL,
    name        TEXT NOT NULL,
    sum         REAL NOT NULL,
    PRIMARY KEY (run_id, behavior_id, scope_id, name),
    FOREIGN KEY (run_id, behavior_id, scope_id) REFERENCES aggregate_scopes (run_id, behavior_id, scope_id)
) WITHOUT ROWID;

-- The skeleton's uncalibrated signals stay readable in signals_v1; nothing reads them.
DROP INDEX signals_by_run;
ALTER TABLE signals RENAME TO signals_v1;

CREATE TABLE signals (
    id                INTEGER PRIMARY KEY,
    run_id            INTEGER NOT NULL REFERENCES runs (id),
    behavior_id       TEXT NOT NULL REFERENCES behaviors (id),
    kind              TEXT NOT NULL,
    measure           TEXT NOT NULL,
    current           REAL NOT NULL,
    baseline_runs     INTEGER NOT NULL,
    present_in        INTEGER NOT NULL,
    -- NULL when no baseline run had the measure.
    baseline_median   REAL,
    baseline_min      REAL,
    baseline_max      REAL,
    baseline_failures INTEGER,
    exception         TEXT,
    -- attributed = 1 with a NULL scope_id is the setup phase, before the first example.
    attributed        INTEGER NOT NULL,
    scope_id          TEXT,
    scope_current     REAL,
    scope_baseline    REAL,
    confidence        REAL NOT NULL,
    tier              INTEGER NOT NULL,
    group_rank        INTEGER NOT NULL,
    headline          INTEGER NOT NULL
);
CREATE INDEX signals_by_run ON signals (run_id, group_rank);
",
    r"
-- The signal that interrupted the run. A partial run would read as mass DISAPPEARED, so it never joins a baseline.
ALTER TABLE runs ADD COLUMN interrupted INTEGER;
",
    r"
-- What people and agents did with what siftr showed them. Facts only: outcomes are derived, never stored.
CREATE TABLE feedback (
    id          INTEGER PRIMARY KEY,
    at_ms       INTEGER NOT NULL,
    -- surfaced | investigated | evidence_requested | dismissed | acked
    kind        TEXT NOT NULL,
    -- The siftr command that recorded it; for surfaced, where the signal was shown.
    command     TEXT NOT NULL,
    -- human | json
    interface   TEXT NOT NULL,
    -- The run whose data was shown; the context is the run's.
    run_id      INTEGER NOT NULL REFERENCES runs (id),
    behavior_id TEXT NOT NULL REFERENCES behaviors (id),
    -- NULL when the command named a behavior, not a signal.
    signal_id   INTEGER REFERENCES signals (id),
    note        TEXT
);
CREATE INDEX feedback_by_behavior ON feedback (behavior_id, at_ms);
",
    r"
-- Where each aggregate first occurred (its first exemplar), so reading a baseline needn't touch exemplars.
ALTER TABLE aggregates ADD COLUMN first_stream TEXT;
ALTER TABLE aggregates ADD COLUMN first_seq INTEGER;
UPDATE aggregates SET first_stream = e.stream, first_seq = e.seq
FROM exemplars e
WHERE e.run_id = aggregates.run_id AND e.behavior_id = aggregates.behavior_id AND e.position = 0;
",
    r"
-- Retention: the setting that pruned a run's evidence (exemplars, raw capture) or its stats (aggregates and
-- everything under them), like SIFTR_KEEP_RUNS=100. The run, its signals, baselines and feedback stay.
ALTER TABLE runs ADD COLUMN evidence_pruned_by TEXT;
ALTER TABLE runs ADD COLUMN stats_pruned_by TEXT;
",
    r"
-- Until e6904cb (2026-09-13T18:22:43-07:00), a child killed by a signal siftr never saw (kill -9, the OOM killer)
-- was recorded as a plain exit 128+signal, so its truncated run joined baselines and hid what disappeared. A run
-- started before then can't be from a build with the fix; after it, such a code is the command's own. `ingest`
-- has no child.
UPDATE runs SET interrupted = exit_code - 128
WHERE interrupted IS NULL AND wall_ms IS NOT NULL AND exit_code BETWEEN 129 AND 159
  AND started_at_ms < 1789348963000 AND command NOT LIKE 'siftr ingest%';
",
    r"
-- Migration 7's time cutoff missed the case it was for: an old binary keeps recording after the fix exists, and
-- nothing says which build wrote a run. So every finished run that exited 128+signal leaves baselines. A command
-- that exits so itself usually passes on a killed child, whose output is as partial.
UPDATE runs SET interrupted = exit_code - 128
WHERE interrupted IS NULL AND wall_ms IS NOT NULL AND exit_code BETWEEN 129 AND 159
  AND command NOT LIKE 'siftr ingest%';
",
    r"
-- What a behavior's paths are (database, lock, temp, …), comma-separated; empty when it has none.
ALTER TABLE behaviors ADD COLUMN roles TEXT NOT NULL DEFAULT '';
",
    r"
-- siftr 0.1.0 stored credentials verbatim, which no SQL can find: `scrub::credentials` runs right after this, in the
-- same transaction. It redacts templates (ids kept), kept lines, commands, contexts, notes and exceptions, and
-- deletes raw captures.
",
    r"
-- What a run read: each source that was on in `.siftr.toml` AND applied to its command, so turning one off reads
-- as a change to what siftr looked at rather than as the behaviors it fed disappearing. No rows means the run
-- didn't record it — one from before this migration, or `ingest`, which replays a capture instead of choosing
-- sources — and that is unknown, not none: it suppresses nothing.
CREATE TABLE run_sources (
    run_id INTEGER NOT NULL REFERENCES runs (id),
    -- Its configuration key, as `siftr sources` lists it.
    name   TEXT NOT NULL,
    -- The stream it fed, as an exemplar's `stream` spells it; NULL for a source that opens none (rusage).
    stream TEXT,
    PRIMARY KEY (run_id, name)
) WITHOUT ROWID;
",
];

/// Index of the migration [`scrub::credentials`] completes.
const SCRUB: i64 = 9;

/// The schema version this siftr reads and writes.
pub(crate) fn supported() -> i64 {
    MIGRATIONS.len() as i64
}

/// Brings the database at `conn` to WAL and the latest schema. `lock` serializes processes doing so.
pub(crate) fn migrate(conn: &mut Connection, lock: &Path) -> Result<()> {
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
    if is_current(conn)? {
        return Ok(());
    }
    // Switching to WAL needs an exclusive lock, and SQLite fails the switch at once, without the busy
    // timeout, when another connection is escalating too. So first opens and upgrades take turns, but wait
    // only so long: a holder that never lets go (suspended mid-migration, a stale lock) mustn't stall a run.
    let file = File::create(lock).with_context(|| format!("creating {}", lock.display()))?;
    if !take_turn(conn, &file, lock)? {
        return Ok(());
    }
    conn.execute_batch("PRAGMA journal_mode = WAL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = checked_version(&tx)?;
    for (applied, migration) in (0..).zip(MIGRATIONS).skip(version.unsigned_abs() as usize) {
        tx.execute_batch(migration)?;
        if applied == SCRUB {
            scrub::credentials(&tx, lock.parent().unwrap_or(Path::new(".")))?;
        }
        tx.pragma_update(None, "user_version", applied + 1_i64)?;
    }
    tx.commit()?;
    Ok(())
}

/// Waits about `BUSY_WAIT` at most to hold the migration lock. `false` when whoever holds it brought the database
/// current meanwhile: then there's nothing to wait for, and queueing behind every other opener would be slow.
fn take_turn(conn: &Connection, file: &File, lock: &Path) -> Result<bool> {
    let deadline = Instant::now() + BUSY_WAIT;
    loop {
        let locked = lock_within(file, Duration::from_millis(20))
            .with_context(|| format!("locking {}", lock.display()))?;
        if locked {
            return Ok(true);
        }
        if is_current(conn)? {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            let home = lock.parent().unwrap_or(lock).to_owned();
            let held = "migration lock";
            return Err(StoreBusy { home, held }.into());
        }
    }
}

/// Read-only, so concurrent opens of a migrated database never contend.
fn is_current(conn: &Connection) -> Result<bool> {
    let version = checked_version(conn)?;
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    Ok(version == MIGRATIONS.len() as i64 && mode.eq_ignore_ascii_case("wal"))
}

fn checked_version(conn: &Connection) -> Result<i64> {
    let (version, known) = (user_version(conn)?, MIGRATIONS.len() as i64);
    if version > known {
        bail!("database schema version {version} is newer than this siftr understands ({known})");
    }
    Ok(version)
}

fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database left at `version` by an older siftr.
    fn at_version(home: &Path, version: usize) -> Connection {
        let conn = Connection::open(home.join("siftr.db")).unwrap();
        for migration in &MIGRATIONS[..version] {
            conn.execute_batch(migration).unwrap();
        }
        conn.pragma_update(None, "user_version", version as i64)
            .unwrap();
        conn
    }

    #[test]
    fn runs_an_old_build_recorded_as_killed_by_a_signal_leave_baselines() {
        let home = tempfile::tempdir().unwrap();
        // Already past migration 7, whose time cutoff these runs slip past.
        let mut conn = at_version(home.path(), 7);
        // (command, exit code, interrupted as recorded); no exit code is a run that never finished. All recorded
        // now: an old binary keeps recording after the fix exists, so when a run started can't tell them apart.
        let runs: [(&str, Option<i32>, Option<i32>); 7] = [
            ("sh step.sh", Some(137), None),
            ("sh step.sh", Some(0), None),
            ("sh step.sh", Some(130), Some(2)),
            ("siftr ingest --dir scenario", Some(137), None),
            ("sh step.sh", Some(255), None),
            ("sh step.sh", Some(143), None),
            ("sh step.sh", None, None),
        ];
        for (command, exit, interrupted) in runs {
            conn.execute(
                "INSERT INTO runs (project, context, command, cwd, started_at_ms, wall_ms, exit_code, lines, interrupted)
                 VALUES ('p', ?1, ?1, '/', CAST(strftime('%s', 'now') AS INTEGER) * 1000,
                         CASE WHEN ?2 IS NULL THEN NULL ELSE 1 END, ?2, 1, ?3)",
                rusqlite::params![command, exit, interrupted],
            )
            .unwrap();
        }
        migrate(&mut conn, &home.path().join("siftr.lock")).unwrap();

        let interrupted: Vec<Option<i32>> = conn
            .prepare("SELECT interrupted FROM runs ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            interrupted,
            [Some(9), None, Some(2), None, None, Some(15), None]
        );
    }

    #[test]
    fn first_occurrence_is_backfilled_from_the_first_exemplar() {
        let home = tempfile::tempdir().unwrap();
        let mut conn = at_version(home.path(), 4);
        conn.execute_batch(
            "INSERT INTO runs (id, project, context, command, cwd, started_at_ms) VALUES (1, 'p', 'c', 'c', '/', 0);
             INSERT INTO behaviors (id, kind, template) VALUES ('a', 'log', 'a'), ('b', 'log', 'b');
             INSERT INTO aggregates (run_id, behavior_id, count, errors) VALUES (1, 'a', 2, 0), (1, 'b', 1, 0);
             INSERT INTO exemplars (run_id, behavior_id, position, stream, seq, line)
                 VALUES (1, 'a', 1, 'stdout', 9, 'a'), (1, 'a', 0, 'file:log/test.log', 4, 'a');",
        )
        .unwrap();
        migrate(&mut conn, &home.path().join("siftr.lock")).unwrap();

        let first = |id: &str| -> (Option<String>, Option<i64>) {
            conn.query_row(
                "SELECT first_stream, first_seq FROM aggregates WHERE behavior_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        };
        assert_eq!(first("a"), (Some("file:log/test.log".into()), Some(4)));
        assert_eq!(
            first("b"),
            (None, None),
            "no exemplar kept, no first occurrence"
        );
    }

    #[test]
    fn credentials_an_older_siftr_stored_are_redacted_in_place_and_its_captures_removed() {
        let home = tempfile::tempdir().unwrap();
        let mut conn = at_version(home.path(), SCRUB as usize);
        let ghp = concat!("ghp_", "Q7m2Xk9Lp4Rz8Wv1Tn6Ys3Hb5Jd0Fc2GaK8x");
        conn.execute_batch(&format!(
            "INSERT INTO runs (id, project, context, command, cwd, started_at_ms)
                 VALUES (1, 'p', 'deploy --token={ghp}', 'deploy --token={ghp}', '/', 0);
             INSERT INTO behaviors (id, kind, template) VALUES ('a', 'log', 'Authorization: Bearer {ghp}'), ('b', 'log', 'plain');
             INSERT INTO aggregates (run_id, behavior_id, count, errors) VALUES (1, 'a', 1, 0), (1, 'b', 1, 0);
             INSERT INTO exemplars (run_id, behavior_id, position, stream, seq, line)
                 VALUES (1, 'a', 0, 'stdout', 1, 'Authorization: Bearer {ghp}'), (1, 'b', 0, 'stdout', 2, 'plain');
             INSERT INTO feedback (at_ms, kind, command, interface, run_id, behavior_id, note)
                 VALUES (0, 'acked', 'ack', 'human', 1, 'a', 'leaked {ghp}');"
        ))
        .unwrap();
        let run_dir = home.path().join("runs/r1");
        std::fs::create_dir_all(&run_dir).unwrap();
        std::fs::write(
            run_dir.join("stdout.log"),
            format!("Authorization: Bearer {ghp}\n"),
        )
        .unwrap();
        std::fs::write(run_dir.join(crate::store::RECORDING_LOCK), "").unwrap();

        migrate(&mut conn, &home.path().join("siftr.lock")).unwrap();

        let rows = |sql: &str| -> Vec<String> {
            conn.prepare(sql)
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            rows("SELECT id || ' ' || template FROM behaviors ORDER BY id"),
            ["a Authorization: Bearer <TOKEN>", "b plain"],
            "ids kept, templates unnumbered"
        );
        assert_eq!(
            rows("SELECT line FROM exemplars ORDER BY behavior_id"),
            ["Authorization: Bearer <TOKEN_1>", "plain"]
        );
        assert_eq!(
            rows("SELECT command || ' | ' || context FROM runs"),
            ["deploy --token=<TOKEN_1> | deploy --token=<TOKEN_1>"]
        );
        assert_eq!(rows("SELECT note FROM feedback"), ["leaked <TOKEN_1>"]);
        assert!(
            !run_dir.join("stdout.log").exists(),
            "the raw capture is gone"
        );
        assert!(run_dir.join(crate::store::RECORDING_LOCK).exists());
        assert_eq!(
            crate::context::Context::named("p", "deploy --token=".to_owned() + ghp).name(),
            rows("SELECT context FROM runs")[0],
            "a scrubbed run stays in the baseline of the same command run again"
        );
    }
}
