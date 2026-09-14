//! `siftr changes [RUN]`: a run's behavioral changes against its baseline, grouped and ranked.

use std::process::ExitCode;

use anyhow::{Result, bail};
use siftr_core::context::Context;
use siftr_store::RunId;

use super::{Globals, found, no_runs, resolve_run};
use crate::output::{self, Changes};
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// Run id, like r42 [default: the latest run in this project, or in --context]
    run: Option<RunId>,

    /// The latest run of this context, as `siftr history` shows it (e.g. the `ingest --context` name)
    #[arg(long, value_name = "NAME", conflicts_with = "run")]
    context: Option<String>,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let run = match &args.context {
        Some(name) => {
            let context = Context::named(project::current()?.project, name.as_str());
            match store.latest_run_of(&context)? {
                Some(run) => Some(run),
                None => bail!("no finished runs of context {name:?} in this project"),
            }
        }
        None => resolve_run(&store, args.run)?,
    };
    let Some(run) = run else {
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
