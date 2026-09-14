//! Siftr persistence: runs, behaviors, aggregates, exemplars and signals in SQLite at `<home>/siftr.db`,
//! plus each run's raw capture, byte for byte, under `<home>/runs/<run>/`.

mod capture;
mod ids;
mod read;
mod schema;
mod write;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result};
use rusqlite::Connection;
use siftr_core::behavior::Behavior;
use siftr_core::context::Context;
use siftr_core::signal::Signal;

pub use capture::Capture;
pub use ids::{InvalidId, RunId, SignalId};
pub use read::Order;
pub use write::Finished;

/// The one store. A trait arrives with a second backend, not before.
pub struct Store {
    conn: Connection,
    home: PathBuf,
}

impl Store {
    /// Opens (creating if needed) the store rooted at `home`.
    pub fn open(home: &Path) -> Result<Self> {
        std::fs::create_dir_all(home)
            .with_context(|| format!("creating data dir {}", home.display()))?;
        let db = home.join("siftr.db");
        let mut conn =
            Connection::open(&db).with_context(|| format!("opening {}", db.display()))?;
        schema::migrate(&mut conn).with_context(|| format!("migrating {}", db.display()))?;
        Ok(Store {
            conn,
            home: home.to_owned(),
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    fn run_dir(&self, run: RunId) -> PathBuf {
        self.home.join("runs").join(run.to_string())
    }
}

/// A run as recorded. `end` is `None` until the run is finished; unfinished runs never join a baseline.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    pub id: RunId,
    pub context: Context,
    /// Shell-quoted, as the user would retype it.
    pub command: String,
    pub cwd: String,
    pub started_at: SystemTime,
    pub end: Option<RunEnd>,
    /// Events of behaviors past the per-run cap, counted into the overflow behavior.
    pub overflow_events: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunEnd {
    pub wall: Duration,
    /// `None` when there was no child process, as for `siftr ingest`.
    pub exit_code: Option<i32>,
    pub lines: u64,
}

/// What a run starts with.
#[derive(Debug, Clone, Copy)]
pub struct NewRun<'a> {
    pub context: &'a Context,
    pub command: &'a str,
    pub cwd: &'a str,
    pub started_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredSignal {
    pub id: SignalId,
    pub run: RunId,
    pub behavior: Behavior,
    pub signal: Signal,
    /// The example the signal is attributed to, when the store knows it.
    pub scope: Option<Behavior>,
    /// Evidence lines kept for the behavior in this run.
    pub exemplars: u64,
}
