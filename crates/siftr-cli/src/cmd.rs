//! One module per subcommand, plus what they share.

pub mod changes;
pub mod evidence;
pub mod explain;
pub mod history;
pub mod ingest;
pub mod run;
pub mod summary;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use siftr_store::{RunId, RunRecord, Store};

use crate::{home, project};

/// Flags every command accepts.
pub struct Globals {
    pub home: Option<PathBuf>,
    pub json: bool,
}

impl Globals {
    pub fn open_store(&self) -> Result<Store> {
        Store::open(&home::resolve(self.home.clone())?)
    }
}

/// Query convention: 0 when something was found, 1 when nothing was.
pub fn found(any: bool) -> ExitCode {
    if any {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The run a query is about: `id` if given (it must exist), else the latest finished run in this project.
pub fn resolve_run(store: &Store, id: Option<RunId>) -> Result<Option<RunRecord>> {
    match id {
        Some(id) => store
            .run(id)?
            .map(Some)
            .ok_or_else(|| anyhow!("no run {id}")),
        None => store.latest_run(&project::current()?.project),
    }
}

pub fn no_runs() -> ExitCode {
    eprintln!("siftr: no finished runs in this project yet");
    eprintln!("next: siftr run -- CMD");
    ExitCode::FAILURE
}
