//! `siftr explain <SIGNAL>`: signal → behavior → per-run numbers → scope → exemplar raw lines.

use std::process::ExitCode;

use anyhow::Result;
use serde_json::{Value, json};
use siftr::aggregate::{Phase, RunStats};
use siftr::signal::{self, measure};
use siftr::store::{Feedback, FeedbackKind, Pruned, RunId, SignalId};

use super::{Globals, record_feedback};
use crate::output::{
    self, behavior_json, change, exception, exemplar_json, groups, label, printable, rule,
    signal_json,
};

#[derive(clap::Args)]
pub struct Args {
    /// Signal id, like s3
    signal: SignalId,
}

const EXEMPLARS: usize = 5;

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let stored = store.signal(args.signal)?.ok_or_else(|| {
        output::not_found(format!(
            "no signal {}; siftr history --signals lists recent signals",
            args.signal
        ))
    })?;
    let behavior = &stored.behavior;
    let s = &stored.signal;
    let mut runs: Vec<(RunId, RunStats)> = vec![(stored.run, store.run_stats(stored.run)?)];
    for run in store.baseline_of(stored.run)? {
        runs.push((run, store.run_stats(run)?));
    }
    let rounded = |v: f64| siftr::num::round_sig(v, 3);
    // Absent is zero for counts; for other measures there is no number to show.
    let per_run: Vec<(RunId, Option<f64>)> = runs
        .iter()
        .map(|(run, stats)| {
            let v = signal::value(stats, behavior.id, &s.measure);
            let v = match (v, s.measure.as_str()) {
                (None, measure::COUNT) => Some(0.0),
                (v, _) => v.map(rounded),
            };
            (*run, v)
        })
        .collect();
    let scope = s.attribution.map(|a| a.scope);
    let per_scope: Option<Vec<(RunId, f64)>> = scope.map(|scope| {
        runs.iter()
            .map(|(run, stats)| {
                (
                    *run,
                    signal::scoped_value(stats, behavior.id, scope, &s.measure),
                )
            })
            .collect()
    });
    // Evidence from where the behavior occurred: this run, or for a disappearance the latest baseline run that had it.
    let evidence_run = runs
        .iter()
        .find(|(_, stats)| stats.count(behavior.id) > 0)
        .map(|(run, _)| *run);
    // Pruned evidence costs the lines, not the explanation: the numbers above don't need it.
    let (exemplars, evidence_pruned) = match evidence_run {
        Some(run) => match store.exemplars(run, behavior.id, EXEMPLARS) {
            Ok(exemplars) => (exemplars, None),
            Err(error) => (Vec::new(), Some(error.downcast::<Pruned>()?)),
        },
        None => (Vec::new(), None),
    };
    let signals = store.signals(stored.run)?;
    let group = groups(&signals)
        .into_iter()
        .find(|g| g.rank == s.group)
        .map(|g| {
            g.members
                .into_iter()
                .filter(|m| m.id != stored.id)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let events: Vec<Option<String>> = match evidence_run {
        Some(run) => exemplars
            .iter()
            .map(|e| super::listener_event(&store, run, e))
            .collect(),
        None => Vec::new(),
    };
    let values = |rows: &[(RunId, Option<f64>)]| -> Value {
        rows.iter()
            .map(|(run, v)| json!({ "run": run.to_string(), "value": v }))
            .collect()
    };
    let as_json = || {
        let scoped: Option<Vec<(RunId, Option<f64>)>> = per_scope
            .as_ref()
            .map(|rows| rows.iter().map(|(r, v)| (*r, Some(*v))).collect());
        json!({
            "signal": signal_json(&stored),
            "rule": rule(s),
            "runs": values(&per_run),
            "scope": stored.scope.as_ref().map(behavior_json),
            "scope_runs": scoped.as_deref().map(values),
            "evidence": {
                "run": evidence_run.map(|run| run.to_string()),
                "pruned": evidence_pruned.as_ref().map(|pruned| pruned.setting()),
                "exemplars": exemplars
                    .iter()
                    .zip(&events)
                    .map(|(e, event)| exemplar_json(e, event.as_deref()))
                    .collect::<Vec<_>>(),
            },
            "group": group.iter().map(|m| m.id.to_string()).collect::<Vec<_>>(),
        })
    };
    output::emit(globals.json, as_json, |w| {
        let role = if s.headline { "headline" } else { "supporting" };
        writeln!(
            w,
            "{}  {}  conf {:.2}  in {}, group {} {role}",
            stored.id,
            label(s.kind),
            s.confidence,
            stored.run,
            s.group
        )?;
        writeln!(
            w,
            "behavior  {}  {}  {}",
            behavior.id.short(),
            behavior.kind,
            printable(&behavior.template, 160)
        )?;
        writeln!(w, "change    {}", change(&stored))?;
        writeln!(w, "rule      {}", rule(s))?;
        let show = |rows: &[(RunId, Option<f64>)]| -> String {
            let cell = |(run, v): &(RunId, Option<f64>)| {
                format!(
                    "{run} {}",
                    v.map_or_else(|| "-".to_owned(), |v| v.to_string())
                )
            };
            let (now, baseline) = rows.split_first().expect("the signal's own run");
            let baseline: Vec<String> = baseline.iter().map(cell).collect();
            format!("{}  |  baseline {}", cell(now), baseline.join("  "))
        };
        writeln!(w, "{:<9} {}", s.measure, show(&per_run))?;
        if let (Some(rows), Some(phase)) = (&per_scope, scope) {
            let rows: Vec<(RunId, Option<f64>)> =
                rows.iter().map(|(r, v)| (*r, Some(*v))).collect();
            // The phases outside examples have reserved ids no behavior has, so `stored.scope` can't tell them apart.
            match (phase, &stored.scope) {
                (Phase::Example(_), Some(example)) => writeln!(
                    w,
                    "scope     {}  {}",
                    example.id.short(),
                    printable(&example.template, 140)
                )?,
                (Phase::Example(id), None) => writeln!(w, "scope     {}", id.short())?,
                (Phase::Setup, _) => writeln!(w, "scope     before the first example (setup)")?,
                (Phase::Between, _) => writeln!(w, "scope     between examples")?,
                (Phase::Teardown, _) => writeln!(w, "scope     after the last example (teardown)")?,
            }
            writeln!(w, "          {}", show(&rows))?;
        }
        match (evidence_run, &evidence_pruned) {
            (Some(run), Some(pruned)) => {
                writeln!(w, "evidence  {run} pruned ({})", pruned.setting())?;
            }
            (Some(run), None) => writeln!(w, "evidence  {run}")?,
            (None, _) => writeln!(w, "evidence  none kept")?,
        }
        for (exemplar, event) in exemplars.iter().zip(&events) {
            for line in event.as_deref().and_then(exception).unwrap_or_default() {
                writeln!(w, "          {}", printable(&line, 240))?;
            }
            let at = format!("{}:{}", exemplar.stream, exemplar.seq);
            writeln!(w, "          {at:<20} {}", printable(&exemplar.line, 160))?;
        }
        for member in &group {
            writeln!(
                w,
                "group     {} {} {}  {}",
                member.id,
                label(member.signal.kind),
                printable(&member.behavior.template, 60),
                change(member)
            )?;
        }
        let run = evidence_run.unwrap_or(stored.run);
        match evidence_pruned {
            Some(_) => writeln!(w, "next: siftr summary {run}"),
            None => writeln!(
                w,
                "next: siftr evidence {} --run {run}",
                behavior.id.short()
            ),
        }
    })?;
    record_feedback(
        &store,
        &[Feedback::on_signal(
            FeedbackKind::Investigated,
            "explain",
            globals.interface(),
            &stored,
        )],
    );
    Ok(ExitCode::SUCCESS)
}
