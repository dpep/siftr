//! `siftr history`: runs recorded in this project, newest first.

use std::process::ExitCode;

use anyhow::Result;
use serde_json::Value;

use super::{Globals, found};
use crate::output::{self, age, printable, run_json};
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// How many runs to show
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let project = project::current()?.project;
    let runs = store.runs(&project, args.limit)?;
    let changes = runs
        .iter()
        .map(|run| Ok(store.signals(run.id)?.len()))
        .collect::<Result<Vec<_>>>()?;

    let as_json = || {
        let rows = runs.iter().zip(&changes).map(|(run, &changes)| {
            let mut row = run_json(run);
            row["changes"] = Value::from(changes);
            row
        });
        Value::Array(rows.collect())
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(w, "runs in {project}")?;
        for (run, changes) in runs.iter().zip(&changes) {
            let status = match run.end {
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
