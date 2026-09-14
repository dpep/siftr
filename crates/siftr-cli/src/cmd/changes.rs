//! `siftr changes [RUN]`: a run's behavioral changes against its baseline.

use std::process::ExitCode;

use anyhow::Result;
use siftr_store::RunId;

use super::{Globals, found, no_runs, resolve_run};
use crate::output::{self, Changes};

#[derive(clap::Args)]
pub struct Args {
    /// Run id, like r42 [default: the latest run in this project]
    run: Option<RunId>,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let Some(run) = resolve_run(&store, args.run)? else {
        return Ok(no_runs());
    };
    let signals = store.signals(run.id)?;
    let baseline_runs = store.baseline_of(run.id)?;
    let changes = Changes {
        run: &run,
        behaviors: store.behavior_count(run.id)?,
        baseline_runs: &baseline_runs,
        signals: &signals,
    };
    output::emit(globals.json, || changes.json(), |w| changes.human(w))?;
    Ok(found(!signals.is_empty()))
}
