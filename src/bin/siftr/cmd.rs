//! One module per subcommand, plus what they share.

pub mod changes;
pub mod cron;
pub mod evidence;
pub mod explain;
pub mod feedback;
pub mod follow;
pub mod gc;
pub mod history;
pub mod ingest;
pub mod run;
pub mod sources;
pub mod status;
pub mod summary;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::Value;
use siftr::store::{Feedback, FeedbackKind, Interface, RunId, RunRecord, Store, StoredSignal};

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

/// Records that `command` showed `signals`. Opens its own store: `run`'s is spent by then.
pub fn record_shown(globals: &Globals, command: &str, signals: Vec<&StoredSignal>) {
    let feedback: Vec<Feedback> = signals
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

/// Earlier signals still open at `run` ([`history::still_open`]). Only a reminder, so a failure warns and shows none.
pub fn still_open(
    globals: &Globals,
    run: &RunRecord,
    signals: &[StoredSignal],
) -> Vec<StoredSignal> {
    globals
        .open_store()
        .and_then(|store| history::still_open(&store, run, signals))
        .unwrap_or_else(|error| {
            output::warn(format_args!("open signals not checked: {error:#}"));
            Vec::new()
        })
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
        Some(id) => match store.run(id)? {
            Some(run) => Ok(Some(run)),
            None => Err(output::not_found(format!(
                "no run {id}; siftr history lists this project's runs"
            ))),
        },
        None => store.latest_run(&project::current()?.project),
    }
}

/// The recent runs `run`'s baseline left out, and why. The store keeps only the runs that were compared, so this
/// judges the candidates again as recording did; a run pruned since then no longer shows.
pub fn skipped_runs(
    store: &Store,
    run: &RunRecord,
) -> Result<Vec<(RunId, siftr::baseline::Ineligible)>> {
    if run.interrupted.is_some() || run.end.is_none() {
        return Ok(Vec::new());
    }
    let recent = store.baseline_runs(&run.context, run.id, siftr::baseline::MAX_RUNS)?;
    let current = store.run_stats(run.id)?;
    let baseline = siftr::baseline::Baseline::from_runs(
        &current,
        recent.iter().map(|(id, stats)| (*id, stats)),
    );
    Ok(baseline.skipped().to_vec())
}

/// The whole listener event an exemplar was kept from. An exemplar keeps only a line's first bytes, and an exception's
/// message is often longer, so this reads the run's raw capture at the exemplar's line, falling back to the kept line
/// when the capture is gone.
pub fn listener_event(
    store: &Store,
    run: RunId,
    exemplar: &siftr::aggregate::Exemplar,
) -> Option<String> {
    use std::io::BufRead as _;
    if exemplar.stream != crate::sources::rspec_events() {
        return None;
    }
    let whole = std::fs::File::open(store.capture_file(run, &exemplar.stream))
        .ok()
        .and_then(|file| {
            let index = usize::try_from(exemplar.seq).ok()?.checked_sub(1)?;
            std::io::BufReader::new(file).split(b'\n').nth(index)?.ok()
        })
        .map(|line| {
            String::from_utf8_lossy(&line)
                .trim_end_matches('\r')
                .to_owned()
        });
    Some(whole.unwrap_or_else(|| exemplar.line.clone()))
}

/// No run to show: under `-j` the command's `empty` document, else a hint on stderr.
pub fn no_runs(globals: &Globals, empty: impl FnOnce() -> Value) -> Result<ExitCode> {
    if globals.json {
        output::emit(true, empty, |_| Ok(()))?;
    } else {
        eprintln!("siftr: no finished runs in this project yet");
        eprintln!("next: siftr run -- CMD");
    }
    Ok(ExitCode::FAILURE)
}
