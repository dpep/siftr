//! Schema migrations, tracked by SQLite's `user_version`.

use anyhow::{Result, bail};
use rusqlite::Connection;

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
];

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
