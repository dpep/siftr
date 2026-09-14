//! `siftr history`: runs recorded in this project, newest first.

use std::process::ExitCode;

use anyhow::Result;
use serde_json::Value;
use siftr_core::context::Context;

use super::{Globals, found};
use crate::output::{self, age, groups, printable, run_json};
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// How many runs to show
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,

    /// Only runs of this context (the command, or an `ingest --context` name)
    #[arg(long, value_name = "NAME")]
    context: Option<String>,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let project = project::current()?.project;
    let runs = match &args.context {
        Some(name) => {
            store.runs_of(&Context::named(project.as_str(), name.as_str()), args.limit)?
        }
        None => store.runs(&project, args.limit)?,
    };
    // (code-level changes, signals) per run.
    let counts = runs
        .iter()
        .map(|run| {
            let signals = store.signals(run.id)?;
            let changes = groups(&signals).iter().filter(|g| !g.setup).count();
            Ok((changes, signals.len()))
        })
        .collect::<Result<Vec<_>>>()?;

    let as_json = || {
        let rows = runs.iter().zip(&counts).map(|(run, &(changes, signals))| {
            let mut row = run_json(run);
            row["changes"] = Value::from(changes);
            row["signals"] = Value::from(signals);
            row
        });
        Value::Array(rows.collect())
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(w, "runs in {project}")?;
        for (run, (changes, _)) in runs.iter().zip(&counts) {
            let status = match run.end {
                Some(end) if run.interrupted.is_some() => format!(
                    "interrupted (signal {}) {:>8} lines",
                    run.interrupted.unwrap_or_default(),
                    end.lines
                ),
                Some(end) => {
                    let exit = end
                        .exit_code
                        .map_or_else(|| "-".to_owned(), |code| code.to_string());
                    format!("exit {exit:<3} {:>8} lines  {changes} changes", end.lines)
                }
                None => "unfinished".to_owned(),
            };
            writeln!(
                w,
                "  {:<5} {:>8}  {status}  {}",
                run.id,
                age(run.started_at),
                printable(&run.command, 80)
            )?;
        }
        match runs.iter().find(|run| run.end.is_some()) {
            Some(run) => writeln!(w, "next: siftr changes {}", run.id),
            None => writeln!(w, "next: siftr run -- CMD"),
        }
    })?;
    Ok(found(!runs.is_empty()))
}
