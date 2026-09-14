//! `siftr history`: runs recorded in this project, newest first. `--signals`: their signals instead, each
//! with what became of it.

use std::collections::HashSet;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::{Value, json};
use siftr_core::aggregate::RunStats;
use siftr_core::baseline::Baseline;
use siftr_core::behavior::BehaviorId;
use siftr_core::context::Context;
use siftr_core::signal::{Signal, SignalKind, detect};
use siftr_store::{Feedback, FeedbackKind, Pruned, RunId, RunRecord, Store, StoredSignal};

use super::{Globals, found};
use crate::output::{
    self, age, feedback_json, groups, label, plural, printable, run_json, signal_json,
};
use crate::project;

#[derive(clap::Args)]
pub struct Args {
    /// How many runs to show
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,

    /// Only runs of this context (the command, or an `ingest --context` name)
    #[arg(long, value_name = "NAME")]
    context: Option<String>,

    /// These runs' signals instead, each with what became of it: open, resolved, or recurred
    #[arg(long)]
    signals: bool,
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
    // An unknown context is an error, as in `changes`; an empty project is just empty.
    if let (true, Some(name)) = (runs.is_empty(), &args.context) {
        return Err(output::not_found(format!(
            "no runs of context {name:?} in this project; siftr history lists them"
        )));
    }
    if args.signals {
        return signals(&store, &runs, globals);
    }
    // (code-level changes, signals, complete) per run.
    let counts = runs
        .iter()
        .map(|run| {
            let signals = store.signals(run.id)?;
            let changes = groups(&signals).iter().filter(|g| !g.setup).count();
            Ok((changes, signals.len(), output::complete(run, &signals)))
        })
        .collect::<Result<Vec<_>>>()?;

    let as_json = || {
        let rows = runs
            .iter()
            .zip(&counts)
            .map(|(run, &(changes, signals, complete))| {
                let mut row = run_json(run, complete);
                row["changes"] = Value::from(changes);
                row["signals"] = Value::from(signals);
                row
            });
        Value::Array(rows.collect())
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(w, "runs in {project}")?;
        for (run, &(changes, _, complete)) in runs.iter().zip(&counts) {
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
                    // Its changes don't mean what a whole run's do.
                    let marker = if complete { "" } else { "  incomplete" };
                    format!(
                        "exit {exit:<3} {:>8} lines  {:<10}{marker}",
                        end.lines,
                        plural(changes as u64, "change")
                    )
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

fn signals(store: &Store, runs: &[RunRecord], globals: &Globals) -> Result<ExitCode> {
    let mut rows: Vec<(StoredSignal, Outcome)> = Vec::new();
    for run in runs {
        let signals = store.signals(run.id)?;
        if !signals.is_empty() {
            let outcomes = outcomes(store, run, &signals, None)?;
            rows.extend(signals.into_iter().zip(outcomes));
        }
    }

    let as_json = || -> Value {
        rows.iter()
            .map(|(stored, outcome)| {
                json!({
                    "signal": signal_json(stored),
                    "outcome": outcome.status(),
                    "unknown_reason": outcome.unknown_reason(),
                    "resolved_in": outcome.resolved_in.map(|run| run.to_string()),
                    "recurred_in": outcome.recurred_in.map(|run| run.to_string()),
                    "later_runs": outcome.later_runs,
                    "investigated": outcome.investigated(),
                    "dismissed": outcome.dismissed(),
                    "feedback": outcome.feedback.iter().map(feedback_json).collect::<Vec<_>>(),
                })
            })
            .collect()
    };
    output::emit(globals.json, as_json, |w| {
        for (stored, outcome) in &rows {
            writeln!(
                w,
                "  {:<4} {:<4} {:<11} {}  {}",
                stored.id.to_string(),
                stored.run.to_string(),
                label(stored.signal.kind),
                printable(&stored.behavior.template, 60),
                outcome.describe()
            )?;
        }
        match rows.iter().find(|(_, outcome)| outcome.status() == "open") {
            Some((stored, _)) => writeln!(w, "next: siftr explain {}", stored.id),
            None => writeln!(w, "next: siftr history"),
        }
    })?;
    Ok(found(!rows.is_empty()))
}

/// Signals of the runs `run` was judged against that are still open at `run` — every later run of the context
/// through `run` still shows the change against the signal's own baseline — and that `run` didn't raise again.
/// The rolling baseline absorbs a change that stays, so without these an unfixed regression reads as no change.
/// Bounded by that window: once the signal's run ages out of the baseline, the change is what siftr calls normal.
/// Oldest first, one per behavior and measure; a change with any dismissed signal is left out, and so is one headed
/// by DISAPPEARED: a disappearance that stays is the new normal, not a regression left in place.
pub fn still_open(
    store: &Store,
    run: &RunRecord,
    signals: &[StoredSignal],
) -> Result<Vec<StoredSignal>> {
    // An incomplete run's one change is its incompleteness; what it didn't run can't say what is still open.
    let incomplete = signals
        .iter()
        .any(|s| s.signal.kind == SignalKind::Incomplete);
    if run.interrupted.is_some() || run.end.is_none() || incomplete {
        return Ok(Vec::new());
    }
    let mut seen: HashSet<Key> = signals.iter().map(|s| key(&s.signal)).collect();
    let mut earlier = store.baseline_of(run.id)?;
    earlier.sort();
    let mut open = Vec::new();
    for id in earlier {
        let mut signals = store.signals(id)?;
        let normal: HashSet<u32> = signals
            .iter()
            .filter(|s| s.signal.headline && s.signal.kind == SignalKind::Disappeared)
            .map(|s| s.signal.group)
            .collect();
        signals.retain(|s| !normal.contains(&s.signal.group));
        if signals.is_empty() {
            continue;
        }
        let Some(earlier) = store.run(id)? else {
            continue;
        };
        let outcomes = outcomes(store, &earlier, &signals, Some(run.id))?;
        let dismissed: HashSet<u32> = signals
            .iter()
            .zip(&outcomes)
            .filter(|(_, outcome)| outcome.dismissed())
            .map(|(stored, _)| stored.signal.group)
            .collect();
        for (stored, outcome) in signals.into_iter().zip(outcomes) {
            // INCOMPLETE is about its own run: a later run doesn't leave it open.
            if outcome.status() == "open"
                && stored.signal.kind != SignalKind::Incomplete
                && !dismissed.contains(&stored.signal.group)
                && seen.insert(key(&stored.signal))
            {
                open.push(stored);
            }
        }
    }
    Ok(open)
}

/// What became of a signal, judged by re-running its own rule against the baseline it was judged against, on
/// each later run of its context. The live baseline can't say: it absorbs a change that stays, so the signal
/// stops firing whether or not anything was fixed.
struct Outcome {
    /// Whether today's rules still produce the signal on its own run; if not, later runs can't be judged.
    reproducible: bool,
    /// The setting that pruned runs the judgement needs; then there is no judgement.
    pruned: Option<String>,
    later_runs: usize,
    resolved_in: Option<RunId>,
    recurred_in: Option<RunId>,
    /// Feedback on the signal's behavior from its run until the run it resolved in.
    feedback: Vec<Feedback>,
}

impl Outcome {
    fn pruned(pruned: &Pruned) -> Self {
        Outcome {
            reproducible: false,
            pruned: Some(pruned.setting().to_owned()),
            later_runs: 0,
            resolved_in: None,
            recurred_in: None,
            feedback: Vec::new(),
        }
    }

    /// Why the outcome is unknown, when it is.
    fn unknown_reason(&self) -> Option<String> {
        match (&self.pruned, self.reproducible) {
            (Some(setting), _) => Some(format!("baseline runs pruned ({setting})")),
            (None, false) => Some("today's rules don't reproduce it on its own run".to_owned()),
            (None, true) => None,
        }
    }

    fn status(&self) -> &'static str {
        match (self.reproducible, self.resolved_in, self.recurred_in) {
            (false, _, _) => "unknown",
            (true, _, Some(_)) => "recurred",
            (true, Some(_), None) => "resolved",
            (true, None, None) => "open",
        }
    }

    fn investigated(&self) -> bool {
        self.feedback.iter().any(|f| {
            matches!(
                f.kind,
                FeedbackKind::Investigated | FeedbackKind::EvidenceRequested | FeedbackKind::Acked
            )
        })
    }

    fn dismissed(&self) -> bool {
        self.feedback
            .iter()
            .any(|f| f.kind == FeedbackKind::Dismissed)
    }

    fn describe(&self) -> String {
        let how = if self.investigated() {
            "after investigation"
        } else {
            "without investigation"
        };
        let mut text = match (self.reproducible, self.resolved_in, self.recurred_in) {
            (false, _, _) => format!("unknown: {}", self.unknown_reason().unwrap_or_default()),
            (true, Some(resolved), Some(again)) => {
                format!("recurred in {again}, after resolving in {resolved} {how}")
            }
            (true, Some(resolved), None) => format!("resolved in {resolved} {how}"),
            (true, None, _) => {
                format!("open after {}", plural(self.later_runs as u64, "later run"))
            }
        };
        if self.dismissed() {
            text.push_str("; dismissed");
        }
        text
    }
}

/// What makes signals in different runs the same change; their ids are per run.
type Key = (SignalKind, BehaviorId, String);

fn key(signal: &Signal) -> Key {
    (signal.kind, signal.behavior, signal.measure.clone())
}

/// Judged on the later runs of `run`'s context, up to and including `until` when given. Unknown, never a
/// verdict from partial data, when retention pruned a run the judgement reads.
fn outcomes(
    store: &Store,
    run: &RunRecord,
    signals: &[StoredSignal],
    until: Option<RunId>,
) -> Result<Vec<Outcome>> {
    match judge(store, run, signals, until) {
        Err(error) => match error.downcast_ref::<Pruned>() {
            Some(pruned) => Ok(signals.iter().map(|_| Outcome::pruned(pruned)).collect()),
            None => Err(error),
        },
        judged => judged,
    }
}

fn judge(
    store: &Store,
    run: &RunRecord,
    signals: &[StoredSignal],
    until: Option<RunId>,
) -> Result<Vec<Outcome>> {
    let baseline_stats = store
        .baseline_of(run.id)?
        .into_iter()
        .map(|id| store.run_stats(id))
        .collect::<Result<Vec<RunStats>>>()?;
    // Each run is judged against the signal's own baseline runs, as its own run was. `None`: the run skipped
    // examples those runs ran, so it can't say whether a change is still there.
    let fires = |stats: &RunStats, verdict: bool| -> Option<HashSet<Key>> {
        let baseline = Baseline::from_runs(stats, baseline_stats.iter().enumerate());
        (!verdict || baseline.incomplete().is_none())
            .then(|| detect(stats, &baseline).iter().map(key).collect())
    };
    let own = fires(&store.run_stats(run.id)?, false).unwrap_or_default();
    let mut later: Vec<RunRecord> = Vec::new();
    let mut later_fires: Vec<HashSet<Key>> = Vec::new();
    for record in store.runs_after(&run.context, run.id)? {
        if until.is_some_and(|until| record.id > until) {
            continue;
        }
        if let Some(fired) = fires(&store.run_stats(record.id)?, true) {
            later_fires.push(fired);
            later.push(record);
        }
    }

    signals
        .iter()
        .map(|stored| {
            let key = key(&stored.signal);
            let still: Vec<bool> = later_fires
                .iter()
                .map(|fired| fired.contains(&key))
                .collect();
            let resolved = still.iter().position(|&fires| !fires);
            let recurred =
                resolved.and_then(|at| still[at..].iter().position(|&fires| fires).map(|n| at + n));
            // Millisecond timestamps: feedback in the resolving run's first millisecond counts as before it.
            let until = resolved.map(|at| later[at].started_at);
            let feedback = store
                .feedback_on(stored.behavior.id, run.started_at)?
                .into_iter()
                .filter(|f| until.is_none_or(|until| f.at <= until))
                .collect();
            Ok(Outcome {
                reproducible: own.contains(&key),
                pruned: None,
                later_runs: later.len(),
                resolved_in: resolved.map(|at| later[at].id),
                recurred_in: recurred.map(|at| later[at].id),
                feedback,
            })
        })
        .collect()
}
