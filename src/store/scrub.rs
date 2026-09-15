//! Stores written by siftr 0.1.0 hold credentials verbatim. This migration step redacts what the database keeps,
//! in place, and deletes earlier runs' raw captures.
//!
//! Captures are deleted, not rewritten: rewriting reads every captured byte while the migration holds the store,
//! and another siftr waits only `BUSY_WAIT` for it; deleting is one unlink per file. Kept lines stay, redacted,
//! so `explain` and `evidence` still show old runs, without a failure's whole message. Retention would have pruned
//! those captures within `SIFTR_KEEP_EVIDENCE` runs anyway. See `docs/findings/redaction.md`.

use std::borrow::Cow;
use std::io;
use std::path::Path;

use anyhow::{Context as _, Result};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::normalize::secrets::{redact_text, unnumber};
use crate::store::RECORDING_LOCK;

/// A text column and the key that addresses its row.
struct Column {
    table: &'static str,
    key: &'static [&'static str],
    text: &'static str,
}

const COLUMNS: &[Column] = &[
    Column {
        table: "behaviors",
        key: &["id"],
        text: "template",
    },
    Column {
        table: "exemplars",
        key: &["run_id", "behavior_id", "position"],
        text: "line",
    },
    Column {
        table: "runs",
        key: &["id"],
        text: "command",
    },
    Column {
        table: "runs",
        key: &["id"],
        text: "cwd",
    },
    // Redacted as `Context::named` does, so old runs stay in the baselines of new ones.
    Column {
        table: "runs",
        key: &["id"],
        text: "context",
    },
    Column {
        table: "feedback",
        key: &["id"],
        text: "note",
    },
    Column {
        table: "signals",
        key: &["id"],
        text: "exception",
    },
];

/// Runs inside the migration's transaction, so a failure leaves the store at its old version, to retry. Captures
/// are deleted before the commit for the same reason: once committed, nothing would delete them.
pub(crate) fn credentials(conn: &Connection, home: &Path) -> Result<()> {
    for column in COLUMNS {
        rewrite(conn, column)
            .with_context(|| format!("redacting {}.{}", column.table, column.text))?;
    }
    remove_captures(home).context("removing raw captures recorded before redaction")
}

/// A behavior keeps its id, a hash of its old template: its next run mints the masked template's id.
fn rewrite(conn: &Connection, column: &Column) -> Result<()> {
    let Column { table, key, text } = column;
    let select = format!(
        "SELECT {}, {text} FROM {table} WHERE {text} IS NOT NULL",
        key.join(", ")
    );
    let mut changed: Vec<(Vec<Value>, String)> = Vec::new();
    {
        let mut stmt = conn.prepare(&select)?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let before: String = row.get(key.len())?;
            let Cow::Owned(mut after) = redact_text(&before) else {
                continue;
            };
            if *table == "behaviors" {
                let mut template = Vec::with_capacity(after.len());
                unnumber(after.as_bytes(), &mut template);
                after = String::from_utf8_lossy(&template).into_owned();
            }
            let row_key = (0..key.len())
                .map(|i| row.get(i))
                .collect::<rusqlite::Result<_>>()?;
            changed.push((row_key, after));
        }
    }
    let matches: Vec<String> = key.iter().map(|k| format!("{k} = ?")).collect();
    let update = format!(
        "UPDATE {table} SET {text} = ? WHERE {}",
        matches.join(" AND ")
    );
    let mut stmt = conn.prepare(&update)?;
    for (row_key, after) in changed {
        stmt.execute(params_from_iter(
            std::iter::once(Value::Text(after)).chain(row_key),
        ))?;
    }
    Ok(())
}

/// Every stream file under `runs/`. Run directories and recording locks stay: retention and a live run use them.
fn remove_captures(home: &Path) -> io::Result<()> {
    let runs = match std::fs::read_dir(home.join("runs")) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        runs => runs?,
    };
    for dir in runs {
        let dir = dir?;
        if !dir.file_type()?.is_dir() {
            continue;
        }
        for file in std::fs::read_dir(dir.path())? {
            let file = file?;
            if file.file_name() == RECORDING_LOCK || !file.file_type()?.is_file() {
                continue;
            }
            match std::fs::remove_file(file.path()) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                removed => removed?,
            }
        }
    }
    Ok(())
}
