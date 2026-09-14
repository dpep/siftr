//! `siftr evidence <BEHAVIOR>`: the raw lines kept for a behavior in one run.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use serde_json::json;
use siftr_store::{Feedback, FeedbackKind, RunId};

use super::{Globals, found, record_feedback};
use crate::output::{self, behavior_json, exemplar_json, printable, stats_json};
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// Behavior id, or a unique prefix of at least 4 hex digits
    behavior: String,

    /// Run to take evidence from [default: the latest run in this project where the behavior occurred]
    #[arg(long)]
    run: Option<RunId>,

    /// How many lines to show
    #[arg(short = 'n', long, default_value_t = 8)]
    limit: usize,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let behavior = store
        .resolve_behavior(&args.behavior)?
        .with_context(|| format!("no behavior matches {}", args.behavior))?;
    let run = match args.run {
        Some(run) if store.run(run)?.is_none() => bail!("no run {run}"),
        Some(run) => run,
        None => match store.latest_run_with(&project::current()?.project, behavior.id)? {
            Some(run) => run,
            None => {
                eprintln!(
                    "siftr: behavior {} has not occurred in this project",
                    behavior.id.short()
                );
                return Ok(ExitCode::FAILURE);
            }
        },
    };
    let stats = store
        .stats_in(behavior.id, &[run])?
        .pop()
        .map(|(_, stats)| stats)
        .unwrap_or_default();
    let exemplars = store.exemplars(run, behavior.id, args.limit)?;
    let captures: BTreeMap<String, String> = exemplars
        .iter()
        .map(|e| {
            (
                e.stream.to_string(),
                store.capture_file(run, &e.stream).display().to_string(),
            )
        })
        .collect();

    let as_json = || {
        json!({
            "behavior": behavior_json(&behavior),
            "run": run.to_string(),
            "stats": stats_json(&stats),
            "exemplars": exemplars.iter().map(exemplar_json).collect::<Vec<_>>(),
            "captures": captures,
        })
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(
            w,
            "{}  {}  {}",
            behavior.id.short(),
            behavior.kind,
            printable(&behavior.template, 200)
        )?;
        writeln!(
            w,
            "{run}: {} occurrences, {} errors; {} lines kept",
            stats.count,
            stats.errors,
            exemplars.len()
        )?;
        for exemplar in &exemplars {
            let at = format!("{}:{}", exemplar.stream, exemplar.seq);
            writeln!(w, "  {at:<12} {}", printable(&exemplar.line, 200))?;
        }
        for (stream, path) in &captures {
            writeln!(w, "capture {stream}: {path}")?;
        }
        writeln!(w, "next: siftr summary {run}")
    })?;
    record_feedback(
        &store,
        &[Feedback::on_behavior(
            FeedbackKind::EvidenceRequested,
            "evidence",
            globals.interface(),
            run,
            behavior.id,
        )],
    );
    Ok(found(!exemplars.is_empty()))
}
