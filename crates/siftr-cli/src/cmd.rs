//! One module per subcommand, plus what they share.

pub mod changes;
pub mod evidence;
pub mod explain;
pub mod feedback;
pub mod history;
pub mod ingest;
pub mod run;
pub mod summary;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Result, anyhow};
use siftr_store::{Feedback, FeedbackKind, Interface, RunId, RunRecord, Store, StoredSignal};

use crate::{home, output, project};

/// Flags every command accepts.
pub struct Globals {
    pub home: Option<PathBuf>,
    pub json: bool,
}

impl Globals {
    pub fn open_store(&self) -> Result<Store> {
        Store::open(&home::resolve(self.home.clone())?)
    }

    pub fn interface(&self) -> Interface {
        if self.json {
            Interface::Json
        } else {
            Interface::Human
        }
    }
}

/// Feedback recorded in passing: failing to record it warns and never fails the command.
pub fn record_feedback(store: &Store, feedback: &[Feedback]) {
    if let Err(error) = store.record_feedback(feedback) {
        output::warn(format_args!("feedback not recorded: {error:#}"));
    }
}

/// Records the signals a changes summary printed by `command` showed. Opens its own store: `run`'s is spent by then.
pub fn record_surfaced(globals: &Globals, command: &str, signals: &[StoredSignal]) {
    let feedback: Vec<Feedback> = output::surfaced(signals, globals.json)
        .into_iter()
        .map(|s| Feedback::on_signal(FeedbackKind::Surfaced, command, globals.interface(), s))
        .collect();
    if feedback.is_empty() {
        return;
    }
    match globals.open_store() {
        Ok(store) => record_feedback(&store, &feedback),
        Err(error) => output::warn(format_args!("feedback not recorded: {error:#}")),
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
