//! Siftr persistence: runs, behaviors, aggregates, exemplars and signals in SQLite at `<home>/siftr.db`,
//! plus each run's raw capture, line for line with credentials masked, under `<home>/runs/<run>/`.

mod capture;
mod feedback;
mod ids;
mod read;
mod retention;
mod schema;
mod scrub;
mod write;

use std::cell::RefCell;
use std::fmt;
use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, ErrorCode};

use crate::behavior::Behavior;
use crate::context::Context;
use crate::signal::Signal;

pub use capture::Capture;
pub use feedback::{Feedback, FeedbackKind, Interface};
pub use ids::{InvalidId, RunId, SignalId};
pub use read::Order;
pub(crate) use retention::RECORDING_LOCK;
pub use retention::{
    Captures, ContextRuns, Database, Inspection, Pruned, Pruning, Retention, Setting, Source, Step,
    Tier,
};
pub use write::Finished;

/// The one store. A trait arrives with a second backend, not before.
pub struct Store {
    conn: Connection,
    home: PathBuf,
    retention: Retention,
    /// The recording lock of each run this store began and hasn't finished.
    recording: RefCell<Vec<(RunId, File)>>,
}

/// How long the store waits on another siftr: for its migration lock, or SQLite's write lock while opening and
/// beginning a run. Past it a run goes unrecorded, not late. The holders measured, a run's finish and a prune
/// batch, hold the write lock under 1.5s.
pub const BUSY_WAIT: Duration = Duration::from_secs(2);

/// Another siftr held the store for longer than [`BUSY_WAIT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreBusy {
    pub home: PathBuf,
    /// `migration lock` or `write lock`.
    pub held: &'static str,
}

impl fmt::Display for StoreBusy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is busy: another siftr held its {} for over {}s",
            self.home.display(),
            self.held,
            BUSY_WAIT.as_secs()
        )
    }
}

impl std::error::Error for StoreBusy {}

/// Whether `error` is another siftr holding the store.
pub(crate) fn is_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<StoreBusy>().is_some()
            || cause
                .downcast_ref::<rusqlite::Error>()
                .and_then(rusqlite::Error::sqlite_error_code)
                == Some(ErrorCode::DatabaseBusy)
    })
}

/// SQLite's busy error, which its busy timeout ended, as a [`StoreBusy`]; anything else unchanged.
pub(crate) fn busy(home: &Path, error: anyhow::Error) -> anyhow::Error {
    match is_busy(&error) && error.downcast_ref::<StoreBusy>().is_none() {
        true => StoreBusy {
            home: home.to_owned(),
            held: "write lock",
        }
        .into(),
        false => error,
    }
}

/// Locks `file` exclusively, waiting at most `wait`; `false` when another process still holds it.
pub(crate) fn lock_within(file: &File, wait: Duration) -> io::Result<bool> {
    let started = Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) if started.elapsed() < wait => {
                // Holders are usually brief (a first open racing another): std has no timed lock, so poll, and
                // only slowly once the holder has proved slow.
                let slow = started.elapsed() > Duration::from_millis(100);
                thread::sleep(Duration::from_millis(if slow { 20 } else { 1 }));
            }
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

impl Store {
    /// Opens (creating if needed) the store rooted at `home`, with retention from the environment.
    pub fn open(home: &Path) -> Result<Self> {
        std::fs::create_dir_all(home)
            .with_context(|| format!("creating data dir {}", home.display()))?;
        let db = home.join("siftr.db");
        let mut conn =
            Connection::open(&db).with_context(|| format!("opening {}", db.display()))?;
        conn.busy_timeout(BUSY_WAIT)?;
        schema::migrate(&mut conn, &home.join("siftr.lock"))
            .map_err(|error| busy(home, error))
            .with_context(|| format!("migrating {}", db.display()))?;
        Ok(Store {
            conn,
            home: home.to_owned(),
            retention: Retention::from_env(),
            recording: RefCell::default(),
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
    /// The signal that interrupted the run. Interrupted runs keep their evidence but never join a baseline.
    pub interrupted: Option<i32>,
    /// How many changes the comparison produced, when that was more than [`crate::signal::MAX_CHANGES`] and so
    /// none were recorded; `None` when the run was compared. Unlike an interrupted run, this one is complete
    /// unless it was truncated too (`overflow_events`): its evidence is kept and it baselines normally. It was
    /// the comparison that said nothing, not the run.
    pub uncompared: Option<u64>,
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
