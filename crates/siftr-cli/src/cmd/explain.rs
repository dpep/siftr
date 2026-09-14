//! `siftr explain <SIGNAL>`: signal → behavior → current vs baseline numbers → exemplars.

use std::process::ExitCode;

use anyhow::{Context as _, Result};
use serde_json::json;
use siftr_store::SignalId;

use super::Globals;
use crate::output::{self, duration, exemplar_json, label, printable, signal_json, stats_json};

#[derive(clap::Args)]
pub struct Args {
    /// Signal id, like s3
    signal: SignalId,
}

const EXEMPLARS: usize = 5;

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let stored = store
        .signal(args.signal)?
        .with_context(|| format!("no signal {}", args.signal))?;
    let behavior = &stored.behavior;
    let signal = &stored.signal;
    let now = store
        .stats_in(behavior.id, &[stored.run])?
        .pop()
        .map(|(_, stats)| stats)
        .unwrap_or_default();
    let baseline = store.stats_in(behavior.id, &store.baseline_of(stored.run)?)?;
    // Evidence from where the behavior occurred: this run, or for a disappearance the latest baseline run that had it.
    let evidence_run = if now.count > 0 {
        Some(stored.run)
    } else {
        baseline
            .iter()
            .find(|(_, stats)| stats.count > 0)
            .map(|(run, _)| *run)
    };
    let exemplars = match evidence_run {
        Some(run) => store.exemplars(run, behavior.id, EXEMPLARS)?,
        None => Vec::new(),
    };

    let as_json = || {
        json!({
            "signal": signal_json(&stored),
            "now": { "run": stored.run.to_string(), "stats": stats_json(&now) },
            "baseline": baseline.iter().map(|(run, stats)| json!({ "run": run.to_string(), "stats": stats_json(stats) })).collect::<Vec<_>>(),
            "evidence": { "run": evidence_run.map(|run| run.to_string()), "exemplars": exemplars.iter().map(exemplar_json).collect::<Vec<_>>() },
        })
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(
            w,
            "{}  {}  confidence {}  in {}",
            stored.id,
            label(signal.kind),
            signal.confidence,
            stored.run
        )?;
        writeln!(w, "behavior  {}  {}", behavior.id.short(), behavior.kind)?;
        writeln!(w, "          {}", printable(&behavior.template, 200))?;
        let p50 = now
            .duration
            .map_or_else(String::new, |d| format!(", p50 {}", duration(d.p50)));
        writeln!(
            w,
            "now       {}: {} occurrences, {} errors{p50}",
            stored.run, now.count, now.errors
        )?;
        writeln!(
            w,
            "baseline  {} runs, present in {}: mean {}, spread {}",
            signal.baseline_runs,
            signal.baseline.present_in,
            signal.baseline.mean_count,
            signal.baseline.count_spread
        )?;
        let counts: Vec<String> = baseline
            .iter()
            .map(|(run, stats)| format!("{run}: {}", stats.count))
            .collect();
        writeln!(w, "          {}", counts.join("  "))?;
        match evidence_run {
            Some(run) => writeln!(w, "evidence  from {run}")?,
            None => writeln!(w, "evidence  none kept")?,
        }
        for exemplar in &exemplars {
            let at = format!("{}:{}", exemplar.stream, exemplar.seq);
            writeln!(w, "          {at:<12} {}", printable(&exemplar.line, 160))?;
        }
        let run = evidence_run.unwrap_or(stored.run);
        writeln!(
            w,
            "next: siftr evidence {} --run {run}",
            behavior.id.short()
        )
    })?;
    Ok(ExitCode::SUCCESS)
}
