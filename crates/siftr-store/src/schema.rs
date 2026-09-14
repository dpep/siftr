//! Schema migrations, tracked by SQLite's `user_version`.

use anyhow::{Result, bail};
use rusqlite::Connection;

/// Append a migration to change the schema; never edit one that has shipped.
const MIGRATIONS: &[&str] = &[r"
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
"];

pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;",
    )?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let known = MIGRATIONS.len() as i64;
    if version > known {
        bail!("database schema version {version} is newer than this siftr understands ({known})");
    }
    for (applied, migration) in (0..).zip(MIGRATIONS).skip(version.unsigned_abs() as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", applied + 1_i64)?;
        tx.commit()?;
    }
    Ok(())
}
