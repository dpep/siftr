//! `siftr gc [--dry-run]`: prune what's past the retention limits now, remove captures an interrupted prune left
//! behind, and return the database's free pages to the file system. Runs prune as they finish, a bounded amount
//! each; this is for a backlog (after upgrading, or lowering a limit) and for reclaiming the space.

use std::process::ExitCode;

use anyhow::Result;
use serde_json::json;
use siftr_core::num::round_sig;
use siftr_store::Tier;

use super::Globals;
use crate::output::{self, plural, printable};

#[derive(clap::Args)]
pub struct Args {
    /// Show what would be removed, and remove nothing
    #[arg(long)]
    dry_run: bool,
}

/// Steps listed one per line; the rest are counted.
const LISTED: usize = 20;

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let mut store = globals.open_store()?;
    let (steps, pending, capture_bytes) = match args.dry_run {
        true => {
            let steps = store.prune_plan()?;
            let bytes = steps
                .iter()
                .map(|step| store.capture_bytes(step.run))
                .sum::<Result<u64>>()?;
            (steps, 0, bytes)
        }
        false => {
            let pruning = store.prune(None)?;
            (pruning.done, pruning.pending, pruning.capture_bytes)
        }
    };
    let orphans = store.orphaned_captures()?.len();
    let orphan_bytes = match args.dry_run {
        true => 0,
        false => store.remove_orphaned_captures()?,
    };
    let (before, free) = store.database_bytes()?;
    let vacuum = !args.dry_run && worth_vacuuming(before, free);
    if vacuum {
        store.vacuum()?;
    }
    let after = store.database_bytes()?.0;

    let as_json = || {
        json!({
            "dry_run": args.dry_run,
            "steps": steps.iter().map(|step| json!({
                "run": step.run.to_string(),
                "tier": tier(step.tier),
                "by": step.by,
                "project": step.context.project(),
                "context": step.context.name(),
            })).collect::<Vec<_>>(),
            "pending": pending,
            "capture_bytes": capture_bytes,
            "orphaned_captures": orphans,
            "orphan_bytes": orphan_bytes,
            "database": {
                "bytes": before,
                "free_bytes": free,
                "vacuumed": vacuum,
                "bytes_after": after,
            },
        })
    };
    output::emit(globals.json, as_json, |w| {
        let (would, done) = match args.dry_run {
            true => ("would ", "would free"),
            false => ("", "freed"),
        };
        let count = |wanted| steps.iter().filter(|step| step.tier == wanted).count() as u64;
        match steps.is_empty() {
            true => writeln!(w, "nothing past the retention limits")?,
            false => writeln!(
                w,
                "{would}prune stats of {} and evidence of {}; {done} {} of captures",
                plural(count(Tier::Stats), "run"),
                plural(count(Tier::Evidence), "run"),
                bytes(capture_bytes)
            )?,
        }
        for step in steps.iter().take(LISTED) {
            writeln!(
                w,
                "  {:<6} {:<9} {:<24} {}",
                step.run.to_string(),
                tier(step.tier),
                step.by,
                printable(step.context.name(), 60)
            )?;
        }
        if steps.len() > LISTED {
            writeln!(w, "  … and {} more", steps.len() - LISTED)?;
        }
        if pending > 0 {
            writeln!(
                w,
                "{} still due: siftr gc again",
                plural(pending as u64, "step")
            )?;
        }
        if orphans > 0 {
            writeln!(
                w,
                "{would}remove {} an interrupted prune left behind{}",
                plural(orphans as u64, "capture"),
                match args.dry_run {
                    true => String::new(),
                    false => format!(", {}", bytes(orphan_bytes)),
                }
            )?;
        }
        match (vacuum, args.dry_run) {
            (true, _) => writeln!(
                w,
                "vacuumed the database: {} → {}",
                bytes(before),
                bytes(after)
            )?,
            (false, true) => writeln!(
                w,
                "database {}, {} of it free; gc vacuums it once a quarter is free",
                bytes(before),
                bytes(free)
            )?,
            (false, false) => {
                writeln!(w, "database {}, {} of it free", bytes(before), bytes(free))?
            }
        }
        match args.dry_run {
            true => writeln!(w, "next: siftr gc"),
            false => writeln!(w, "next: siftr status"),
        }
    })?;
    Ok(ExitCode::SUCCESS)
}

/// A vacuum rewrites the whole database holding its write lock (a run finishing meanwhile waits, and past its
/// busy timeout goes unrecorded), so only when it returns at least a quarter of the file.
pub fn worth_vacuuming(bytes: u64, free: u64) -> bool {
    free > 0 && free.saturating_mul(4) >= bytes
}

/// Three significant figures, in powers of 1024.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{n} B"),
        _ => format!("{} {}", round_sig(value, 3), UNITS[unit]),
    }
}

fn tier(tier: Tier) -> &'static str {
    match tier {
        Tier::Stats => "stats",
        Tier::Evidence => "evidence",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_at_three_significant_figures() {
        let cases = [
            (512, "512 B"),
            (1536, "1.5 KB"),
            (918_142_976, "876 MB"),
            (2_147_483_648, "2 GB"),
        ];
        for (n, text) in cases {
            assert_eq!(bytes(n), text);
        }
    }
}
