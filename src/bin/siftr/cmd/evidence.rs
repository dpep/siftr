//! `siftr explain <BEHAVIOR>`: the raw lines kept for a behavior in one run.
//!
//! The behavior half of [`super::explain`], which owns the arguments and decides which half an id asks for.

use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;
use siftr::aggregate::Stats;
use siftr::store::{Feedback, FeedbackKind};

use super::explain::{Args, captures};
use super::{Globals, found, record_feedback};
use crate::output::{self, behavior_json, exception, exemplar_json, plural, printable, stats_json};
use crate::project;

pub fn show(id: &str, args: &Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let behavior = store.resolve_behavior(id)?.ok_or_else(|| {
        output::not_found(format!(
            "no behavior matches {id}; siftr summary lists a run's behaviors"
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
    let captures = captures(&store, run, &exemplars);
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
            "explain",
            globals.interface(),
            run,
            behavior.id,
        )],
    );
    Ok(found(!exemplars.is_empty()))
}
