//! `siftr status`: what the data dir holds, how big it is, and what retention does about it. Read-only: it
//! neither migrates nor creates anything. Exits 1 when something needs attention.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{Value, json};
use siftr_store::{ContextRuns, Inspection, Retention, RunId, Setting, Source, Store, Tier};

use super::Globals;
use super::gc::{bytes, worth_vacuuming};
use crate::home;
use crate::output::{self, age, plural, printable};

#[derive(clap::Args)]
pub struct Args {
    /// How many commands to list, most runs first
    #[arg(short = 'n', long, default_value_t = 5)]
    limit: usize,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let home = home::resolve(globals.home.clone())?;
    let inspection = Store::inspect(&home, Retention::from_env())?;
    let problems = problems(&inspection);
    output::emit(
        globals.json,
        || as_json(&inspection, &problems, args.limit),
        |w| human(w, &inspection, &problems, args.limit),
    )?;
    Ok(match problems.is_empty() {
        true => ExitCode::SUCCESS,
        false => ExitCode::FAILURE,
    })
}

/// What needs attention, each saying what to do about it.
fn problems(inspection: &Inspection) -> Vec<String> {
    let mut problems: Vec<String> = inspection
        .retention
        .settings()
        .iter()
        .filter_map(|setting| match &setting.source {
            Source::Adjusted { given, why } => Some(format!(
                "{}={given} is {why}; siftr uses {}",
                setting.env, setting.value
            )),
            _ => None,
        })
        .collect();
    if let Some(db) = &inspection.database {
        if db.schema > db.supported {
            problems.push(format!(
                "the database is from a newer siftr (schema {}; this one reads {}): upgrade siftr",
                db.schema, db.supported
            ));
        }
        if !db.orphaned.is_empty() {
            problems.push(format!(
                "{} outlived their run's evidence, from an interrupted prune: siftr gc removes them",
                plural(db.orphaned.len() as u64, "capture")
            ));
        }
    }
    problems
}

fn human(
    w: &mut dyn Write,
    inspection: &Inspection,
    problems: &[String],
    limit: usize,
) -> io::Result<()> {
    writeln!(w, "data      {}", tilde(&inspection.home))?;
    let captures = &inspection.captures;
    let Some(db) = &inspection.database else {
        writeln!(w, "database  none yet")?;
        keep(w, &inspection.retention)?;
        for problem in problems {
            writeln!(w, "problem   {problem}")?;
        }
        return writeln!(w, "next: siftr run -- CMD");
    };
    let free = match db.free_bytes {
        0 => String::new(),
        free => format!(", {} of it free", bytes(free)),
    };
    writeln!(
        w,
        "database  {}{free}, schema {}",
        bytes(db.bytes),
        db.schema
    )?;
    if db.schema < db.supported {
        writeln!(
            w,
            "          siftr migrates it to schema {} on its next run, and reads it only then",
            db.supported
        )?;
    }
    writeln!(
        w,
        "captures  {} for {}",
        bytes(captures.bytes),
        plural(captures.runs, "run")
    )?;
    if let Some((oldest, newest)) = span(&db.contexts) {
        let runs: u64 = db.contexts.iter().map(|c| c.runs).sum();
        writeln!(
            w,
            "runs      {} of {}; oldest {} {}, newest {} {}",
            plural(runs, "run"),
            plural(db.contexts.len() as u64, "command"),
            oldest.0,
            age(oldest.1),
            newest.0,
            age(newest.1)
        )?;
    }
    keep(w, &inspection.retention)?;
    let (stats, evidence) = pending(inspection);
    if stats + evidence > 0 {
        writeln!(
            w,
            "pending   stats of {} and evidence of {} past the limits: pruned as runs finish, or now by siftr gc",
            plural(stats, "run"),
            plural(evidence, "run")
        )?;
    }
    if !db.contexts.is_empty() {
        writeln!(
            w,
            "  {:>6} {:>6} {:>9} {:>9}  {:<8}  COMMAND",
            "RUNS", "STATS", "EVIDENCE", "CAPTURES", "NEWEST"
        )?;
        for c in db.contexts.iter().take(limit) {
            writeln!(
                w,
                "  {:>6} {:>6} {:>9} {:>9}  {:<8}  {}",
                c.runs,
                c.with_stats,
                c.with_evidence,
                bytes(c.capture_bytes),
                age(c.newest.1),
                printable(c.context.name(), 60)
            )?;
        }
        if db.contexts.len() > limit {
            writeln!(
                w,
                "  … and {} more",
                plural((db.contexts.len() - limit) as u64, "command")
            )?;
        }
    }
    for problem in problems {
        writeln!(w, "problem   {problem}")?;
    }
    let tidy =
        stats + evidence > 0 || !db.orphaned.is_empty() || worth_vacuuming(db.bytes, db.free_bytes);
    match tidy {
        true => writeln!(w, "next: siftr gc --dry-run"),
        false => writeln!(w, "next: siftr history"),
    }
}

fn keep(w: &mut dyn Write, retention: &Retention) -> io::Result<()> {
    writeln!(
        w,
        "keep      stats of the last {} runs of each command ({})",
        retention.runs.value,
        source(&retention.runs)
    )?;
    writeln!(
        w,
        "          evidence, raw lines and captures, of the last {} ({})",
        retention.evidence.value,
        source(&retention.evidence)
    )?;
    writeln!(
        w,
        "          nothing of a command not run for {} days ({})",
        retention.days.value,
        source(&retention.days)
    )
}

fn source(setting: &Setting) -> String {
    match &setting.source {
        Source::Default => format!("default; set {}", setting.env),
        Source::Env => setting.env.to_owned(),
        Source::Adjusted { given, .. } => format!("{}={given}, adjusted", setting.env),
    }
}

/// Runs past the limits whose (stats, evidence) are due to be pruned.
fn pending(inspection: &Inspection) -> (u64, u64) {
    let steps = inspection.database.iter().flat_map(|db| &db.pending);
    steps.fold((0, 0), |(stats, evidence), step| match step.tier {
        Tier::Stats => (stats + 1, evidence),
        Tier::Evidence => (stats, evidence + 1),
    })
}

type At = (RunId, SystemTime);

fn span(contexts: &[ContextRuns]) -> Option<(At, At)> {
    let oldest = contexts
        .iter()
        .map(|c| c.oldest)
        .min_by_key(|(id, _)| *id)?;
    let newest = contexts
        .iter()
        .map(|c| c.newest)
        .max_by_key(|(id, _)| *id)?;
    Some((oldest, newest))
}

fn as_json(inspection: &Inspection, problems: &[String], limit: usize) -> Value {
    let at = |(id, time): At| json!({ "id": id.to_string(), "started_at_ms": unix_ms(time) });
    let setting = |s: &Setting| {
        let (source, given, why) = match &s.source {
            Source::Default => ("default", None, None),
            Source::Env => ("env", None, None),
            Source::Adjusted { given, why } => ("adjusted", Some(given), Some(why)),
        };
        json!({ "env": s.env, "value": s.value, "source": source, "given": given, "why": why })
    };
    let (stats, evidence) = pending(inspection);
    let retention = &inspection.retention;
    let database = inspection.database.as_ref().map(|db| {
        json!({
            "bytes": db.bytes,
            "free_bytes": db.free_bytes,
            "schema": db.schema,
            "supported_schema": db.supported,
        })
    });
    let contexts = inspection.database.iter().flat_map(|db| &db.contexts);
    json!({
        "home": tilde(&inspection.home),
        "database": database,
        "captures": { "bytes": inspection.captures.bytes, "runs": inspection.captures.runs },
        "runs": inspection.database.as_ref().and_then(|db| span(&db.contexts)).map(|(oldest, newest)| json!({
            "total": contexts.clone().map(|c| c.runs).sum::<u64>(),
            "oldest": at(oldest),
            "newest": at(newest),
        })),
        "retention": {
            "runs": setting(&retention.runs),
            "evidence": setting(&retention.evidence),
            "days": setting(&retention.days),
        },
        "pending": { "stats": stats, "evidence": evidence },
        "commands_total": contexts.clone().count(),
        "commands": contexts.take(limit).map(|c| json!({
            "project": c.context.project(),
            "context": c.context.name(),
            "runs": c.runs,
            "with_stats": c.with_stats,
            "with_evidence": c.with_evidence,
            "capture_bytes": c.capture_bytes,
            "oldest": at(c.oldest),
            "newest": at(c.newest),
        })).collect::<Vec<_>>(),
        "orphaned_captures": inspection.database.iter().flat_map(|db| &db.orphaned)
            .map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "problems": problems,
    })
}

/// The data dir as the user would type it: `~` stays unexpanded.
fn tilde(path: &Path) -> String {
    let rest = std::env::var_os("HOME").and_then(|home| path.strip_prefix(home).ok());
    match rest {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}
