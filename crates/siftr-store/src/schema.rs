//! Schema migrations, tracked by SQLite's `user_version`.

use std::fs::File;
use std::path::Path;

use anyhow::{Context as _, Result, bail};
use rusqlite::{Connection, TransactionBehavior};

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
];

/// Brings the database at `conn` to WAL and the latest schema. `lock` serializes processes doing so.
pub(crate) fn migrate(conn: &mut Connection, lock: &Path) -> Result<()> {
    conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;")?;
    if is_current(conn)? {
        return Ok(());
    }
    // Switching to WAL needs an exclusive lock, and SQLite fails the switch at once, without the busy
    // timeout, when another connection is escalating too. So first opens and upgrades take turns.
    let file = File::create(lock).with_context(|| format!("creating {}", lock.display()))?;
    file.lock()
        .with_context(|| format!("locking {}", lock.display()))?;
    conn.execute_batch("PRAGMA journal_mode = WAL")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version = checked_version(&tx)?;
    for (applied, migration) in (0..).zip(MIGRATIONS).skip(version.unsigned_abs() as usize) {
        tx.execute_batch(migration)?;
        tx.pragma_update(None, "user_version", applied + 1_i64)?;
    }
    tx.commit()?;
    Ok(())
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
}
