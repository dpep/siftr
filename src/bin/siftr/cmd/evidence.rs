//! `siftr evidence <BEHAVIOR>`: the raw lines kept for a behavior in one run.

use std::collections::BTreeMap;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;
use siftr::aggregate::Stats;
use siftr::store::{Feedback, FeedbackKind, RunId};

use super::{Globals, found, record_feedback};
use crate::output::{self, behavior_json, exception, exemplar_json, plural, printable, stats_json};
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
    check_id(&args.behavior)?;
    let store = globals.open_store()?;
    let behavior = store.resolve_behavior(&args.behavior)?.ok_or_else(|| {
        output::not_found(format!(
            "no behavior matches {}; siftr summary lists a run's behaviors",
            args.behavior
        ))
    })?;
    let run = match args.run {
        Some(run) if store.run(run)?.is_none() => {
            return Err(output::not_found(format!(
                "no run {run}; siftr history lists this project's runs"
            )));
        }
        Some(run) => run,
        None => match store.latest_run_with(&project::current()?.project, behavior.id)? {
            Some(run) => run,
            None => {
                if globals.json {
                    let empty = || {
                        json!({
                            "behavior": behavior_json(&behavior),
                            "run": null,
                            "stats": stats_json(&Stats::default()),
                            "exemplars": [],
                            "captures": {},
                        })
                    };
                    output::emit(true, empty, |_| Ok(()))?;
                } else {
                    eprintln!(
                        "siftr: behavior {} has not occurred in this project",
                        behavior.id.short()
                    );
                }
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

    let events: Vec<Option<String>> = exemplars
        .iter()
        .map(|e| super::listener_event(&store, run, e))
        .collect();
    let as_json = || {
        json!({
            "behavior": behavior_json(&behavior),
            "run": run.to_string(),
            "stats": stats_json(&stats),
            "exemplars": exemplars
                .iter()
                .zip(&events)
                .map(|(e, event)| exemplar_json(e, event.as_deref()))
                .collect::<Vec<_>>(),
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
            "{run}: {}, {}; {} kept",
            plural(stats.count, "occurrence"),
            plural(stats.errors, "error"),
            plural(exemplars.len() as u64, "line")
        )?;
        for (exemplar, event) in exemplars.iter().zip(&events) {
            for line in event.as_deref().and_then(exception).unwrap_or_default() {
                writeln!(w, "  {}", printable(&line, 200))?;
            }
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

/// Behavior ids are hex. A signal id is the likeliest thing to land here, so that error names the command taking one.
fn check_id(id: &str) -> Result<()> {
    let signal = id
        .strip_prefix('s')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if signal {
        return Err(output::usage(format!(
            "{id} is a signal id, and evidence takes a behavior id; siftr explain {id} shows the signal's behavior and evidence"
        )));
    }
    if !((4..=16).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err(output::usage(format!(
            "invalid behavior id {id:?} (expected 4 to 16 hex digits, as siftr summary shows)"
        )));
    }
    Ok(())
}
