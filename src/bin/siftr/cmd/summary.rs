//! `siftr summary [RUN]`: a run's top behaviors by count or total time.

use std::process::ExitCode;

use anyhow::Result;
use clap::ValueEnum;
use serde_json::json;
use siftr::store::{Order, RunId};

use super::{Globals, found, no_runs, resolve_run};
use crate::output::{
    self, behavior_json, duration, exact, plural, printable, roles_label, run_json, stats_json,
};

#[derive(clap::Args)]
pub struct Args {
    /// Run id, like r42 [default: the latest run in this project]
    run: Option<RunId>,

    /// How many behaviors to show
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,

    /// Rank by occurrences or by total duration
    #[arg(long, value_enum, default_value_t = By::Count)]
    by: By,
}

#[derive(Clone, Copy, ValueEnum)]
enum By {
    Count,
    Time,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let Some(run) = resolve_run(&store, args.run)? else {
        return no_runs(
            globals,
            || json!({ "run": null, "behaviors_total": 0, "behaviors": [] }),
        );
    };
    let order = match args.by {
        By::Count => Order::Count,
        By::Time => Order::Time,
    };
    let rows = store.behaviors(run.id, order, args.limit)?;
    let total = store.behavior_count(run.id)?;
    let complete = output::complete(&run, &store.signals(run.id)?);

    let as_json = || {
        let behaviors: Vec<_> = rows
            .iter()
            .map(|(behavior, stats)| json!({ "behavior": behavior_json(behavior), "stats": stats_json(stats) }))
            .collect();
        json!({ "run": run_json(&run, complete), "behaviors_total": total, "behaviors": behaviors })
    };
    output::emit(globals.json, as_json, |w| {
        let lines = run.end.map_or(0, |end| end.lines);
        writeln!(
            w,
            "{}: {}, {}, {}",
            run.id,
            plural(lines, "line"),
            plural(total, "behavior"),
            run.command
        )?;
        if run.overflow_events > 0 {
            writeln!(
                w,
                "note: {} of behaviors past the {}-behavior cap, counted as one overflow behavior",
                plural(run.overflow_events, "event"),
                siftr::aggregate::MAX_BEHAVIORS
            )?;
        }
        writeln!(
            w,
            "  {:>7} {:>6} {:>8} {:>8} {:>8}  BEHAVIOR",
            "COUNT", "ERRORS", "P50", "P95", "TOTAL"
        )?;
        for (behavior, stats) in &rows {
            let time = |f: fn(&siftr::aggregate::DurationSummary) -> std::time::Duration| {
                stats
                    .duration
                    .map_or_else(|| "-".to_owned(), |d| duration(f(&exact(d))))
            };
            writeln!(
                w,
                "  {:>7} {:>6} {:>8} {:>8} {:>8}  {}  {}{}  {}",
                stats.count,
                stats.errors,
                time(|d| d.p50),
                time(|d| d.p95),
                time(|d| d.total),
                behavior.id.short(),
                behavior.kind,
                roles_label(behavior.roles),
                printable(&behavior.template, 100),
            )?;
        }
        match rows.first() {
            Some((behavior, _)) => writeln!(
                w,
                "next: siftr evidence {} --run {}",
                behavior.id.short(),
                run.id
            ),
            None => writeln!(w, "next: siftr history"),
        }
    })?;
    Ok(found(!rows.is_empty()))
}
