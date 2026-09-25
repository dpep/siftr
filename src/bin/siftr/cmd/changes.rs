//! `siftr changes [RUN]`: a run's behavioral changes against its baseline, grouped and ranked, and the earlier
//! changes still open at that run.

use std::process::ExitCode;

use anyhow::Result;
use siftr::context::Context;
use siftr::store::RunId;

use super::{Globals, found, no_runs, record_shown, resolve_run, skipped_runs, still_open};
use crate::output::{self, Changes};
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// Run id, like r42 [default: the latest run in this project, or in --context]
    run: Option<RunId>,

    /// The latest run of this context, as `siftr history` shows it (e.g. the `ingest --context` name)
    ///
    /// A context is one project plus one command line exactly as typed, and that is what makes two
    /// runs comparable: `bundle exec rspec` and `bundle exec rspec spec/models` are two contexts
    /// with two separate baselines.
    #[arg(long, value_name = "NAME", conflicts_with = "run")]
    context: Option<String>,

    /// How many changes to show, highest-ranked first [default: all of them]
    // No default, unlike `summary -n`: this report's completeness is load-bearing, and its own `-j` hint
    // promises the rest. `groups_total` and `signals_total` say what a limit left out.
    #[arg(short = 'n', long)]
    limit: Option<usize>,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let run = match &args.context {
        Some(name) => {
            let context = Context::named(project::current()?.project, name.as_str());
            match store.latest_run_of(&context)? {
                Some(run) => Some(run),
                None => {
                    return Err(output::not_found(format!(
                        "no finished runs of context {name:?} in this project; siftr history lists them"
                    )));
                }
            }
        }
        None => resolve_run(&store, args.run)?,
    };
    let Some(run) = run else {
        return no_runs(globals, Changes::empty_json);
    };
    let signals = store.signals(run.id)?;
    let baseline_runs = store.baseline_of(run.id)?;
    let skipped = skipped_runs(&store, &run)?;
    let open = still_open(globals, &run, &signals);
    let changes = Changes {
        run: &run,
        behaviors: store.behavior_count(run.id)?,
        // A stream opens on its first byte, so what *arrived* is knowable only while recording. The store
        // keeps what the run *read*, which is a different set (a source can be read and stay empty) and is
        // `siftr history --sources`; answering with it here would put that under a name that means arrived.
        streams: None,
        baseline_runs: &baseline_runs,
        skipped_runs: &skipped,
        signals: &signals,
        open_signals: &open,
        // `ingest` describes a run it just read; a run read back, and `run` itself, have no such block.
        described: None,
    };
    output::emit(
        globals.json,
        || changes.json_limited(args.limit),
        |w| changes.human_limited(w, args.limit),
    )?;
    let shown = output::surfaced_limited(&signals, globals.json, args.limit)
        .into_iter()
        .chain(output::reminded(&open, globals.json))
        .collect();
    record_shown(globals, "changes", shown);
    // A change still open is something to look at, even when this run raised nothing new.
    Ok(found(!signals.is_empty() || !open.is_empty()))
}
