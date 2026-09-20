//! `siftr history`: runs recorded in this project, newest first. `--signals`: their signals instead, each
//! with what became of it.

use std::collections::HashSet;
use std::process::ExitCode;

use anyhow::Result;
use serde_json::{Value, json};
use siftr::aggregate::RunStats;
use siftr::analyze::RunSource;
use siftr::baseline::Baseline;
use siftr::behavior::BehaviorId;
use siftr::context::Context;
use siftr::observation::Stream;
use siftr::signal::{Signal, SignalKind, detect};
use siftr::store::{Feedback, FeedbackKind, Pruned, RunId, RunRecord, Store, StoredSignal};

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

    /// These runs' signals instead, each with what became of it: open, resolved, recurred, or unknown
    #[arg(long)]
    signals: bool,

    /// What each of these runs read instead: the sources it recorded, or that it recorded none
    #[arg(long, conflicts_with = "signals")]
    sources: bool,

    /// What became of these runs' signals instead, totalled by kind: how many were examined, and how many
    /// resolved with no siftr command ever run against them
    #[arg(long, conflicts_with_all = ["signals", "sources"])]
    scorecard: bool,
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
    if args.sources {
        return sources(&store, &runs, project.as_str(), globals);
    }
    if args.scorecard {
        return super::scorecard::render(&store, &runs, project.as_str(), globals);
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
            // Not `plural`: the count stays right-aligned in its own column.
            let lines = |n: u64| format!("{n:>8} {:<5}", if n == 1 { "line" } else { "lines" });
            let status = match run.end {
                Some(end) if run.interrupted.is_some() => format!(
                    "interrupted (signal {}) {}",
                    run.interrupted.unwrap_or_default(),
                    lines(end.lines).trim_end()
                ),
                Some(end) => {
                    let exit = end
                        .exit_code
                        .map_or_else(|| "-".to_owned(), |code| code.to_string());
                    // Its changes don't mean what a whole run's do.
                    let marker = if complete { "" } else { "  incomplete" };
                    format!(
                        "exit {exit:<3} {}  {:<10}{marker}",
                        lines(end.lines),
                        plural(changes as u64, "change")
                    )
                }
                None => "unfinished".to_owned(),
            };
            writeln!(
                w,
                "  {:<5} {:>8}  {status}  {}",
                // RunId's Display ignores width, so pad the rendered string.
                run.id.to_string(),
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

/// What each run recorded reading, newest first. A run that recorded none can't say what it read — `ingest`
/// replays a capture rather than choosing sources, and neither did siftr before it kept them — so that reads
/// as unknown (`null`, `not recorded`) rather than as having read nothing.
fn sources(
    store: &Store,
    runs: &[RunRecord],
    project: &str,
    globals: &Globals,
) -> Result<ExitCode> {
    let rows: Vec<(&RunRecord, Vec<RunSource>)> = runs
        .iter()
        .map(|run| Ok((run, store.run_sources(run.id)?)))
        .collect::<Result<_>>()?;

    let as_json = || -> Value {
        rows.iter()
            .map(|(run, sources)| {
                let listed = (!sources.is_empty()).then(|| {
                    sources
                        .iter()
                        .map(|source| {
                            json!({
                                "name": source.name,
                                "stream": source.stream.as_ref().map(Stream::to_string),
                            })
                        })
                        .collect::<Vec<_>>()
                });
                json!({ "run": run.id.to_string(), "sources": listed })
            })
            .collect()
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(w, "runs in {project}, and what each read")?;
        for (run, sources) in &rows {
            let read = match sources.is_empty() {
                true => "not recorded".to_owned(),
                false => sources
                    .iter()
                    .map(|source| source.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            writeln!(
                w,
                "  {:<5} {:>8}  {read:<34}  {}",
                run.id.to_string(),
                age(run.started_at),
                printable(&run.command, 60)
            )?;
        }
        match rows.first() {
            Some((run, _)) => writeln!(w, "next: siftr summary {}", run.id),
            None => writeln!(w, "next: siftr run -- CMD"),
        }
    })?;
    Ok(found(!rows.is_empty()))
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
                    "recurrences": outcome.recurrences,
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

/// Signals of earlier runs of the context that are still open at `run` — every later run through `run` still shows
/// the change against the signal's own baseline — and that `run` didn't raise again. The rolling baseline absorbs a
/// change that stays, so without these an unfixed regression reads as no change.
///
/// A reminder expires when the change has become what this context does, which is [`Outcome::settled`]: present in
/// every run since its own, and its own run gone from the baseline window. A change that keeps coming back never
/// settles, so it is reminded for as long as it returns — bounding it by the window instead reported a present
/// regression as normal on the sixth return. Only `dismiss` and a fix end that.
///
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
    // Nor can a run whose comparison was refused: the judgement below re-runs the rules on this run's stats,
    // which is the comparison that produced too many signals to report, and nearly everything fires in it.
    // Reminding from it would surface as fact what siftr just declined to say.
    if run.interrupted.is_some() || run.end.is_none() || incomplete || run.uncompared.is_some() {
        return Ok(Vec::new());
    }
    let mut seen: HashSet<Key> = signals.iter().map(|s| key(&s.signal)).collect();
    // The window says which changes have had time to become normal, not which ones may still be judged: a
    // change that returns outlives its own run's place in it.
    let window: HashSet<RunId> = store.baseline_of(run.id)?.into_iter().collect();
    // Every earlier run of the context siftr still keeps stats for. Past `SIFTR_KEEP_RUNS` the judgement reads
    // pruned runs and gives no verdict, so a change older than that goes unreminded either way.
    let mut earlier: Vec<RunId> = store
        .runs_of(&run.context, store.retention().runs.value as usize)?
        .into_iter()
        .map(|record| record.id)
        .filter(|&id| id < run.id)
        .collect();
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
            if outcome.present_at(run.id)
                && !outcome.settled(window.contains(&stored.run))
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
pub(super) struct Outcome {
    /// Whether today's rules still produce the signal on its own run; if not, later runs can't be judged.
    reproducible: bool,
    /// The setting that pruned runs the judgement needs; then there is no judgement.
    pruned: Option<String>,
    later_runs: usize,
    resolved_in: Option<RunId>,
    recurred_in: Option<RunId>,
    /// How many times the change came back after being resolved. `resolved_in` and `recurred_in` name only the
    /// first of each, so a regression that cycles twice is otherwise unrepresentable.
    recurrences: usize,
    /// Whether [`Self::latest`] still fires the signal: the change is there as of the newest run judged.
    firing: bool,
    /// The newest later run that gave a verdict; `None` when none did.
    latest: Option<RunId>,
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
            recurrences: 0,
            firing: false,
            latest: None,
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

    /// The latest verdict, not the first: a change that recurred and was fixed again reads as resolved, and one
    /// that recurred is only "recurred" while it is actually there.
    pub(super) fn status(&self) -> &'static str {
        match (self.reproducible, self.firing, self.resolved_in) {
            (false, _, _) => "unknown",
            (true, true, Some(_)) => "recurred",
            (true, false, Some(_)) => "resolved",
            // Never resolved, or no later run gave a verdict.
            (true, _, None) => "open",
        }
    }

    /// Whether the change is there at `run`: never resolved, or resolved earlier and back again by `run`'s own
    /// verdict. The live baseline absorbs a value it has already seen — a repeated regression sits inside its
    /// own baseline range — so a recurrence fires nowhere else.
    fn present_at(&self, run: RunId) -> bool {
        self.status() == "open" || (self.firing && self.latest == Some(run))
    }

    /// Whether the change has become what this context does, rather than a change left in place: it has been
    /// there on every run since its own, and its own run has left `in_window`, so every run siftr now compares
    /// against shows it. A change that went away and came back is never settled however old it is — the runs
    /// without it are the evidence that this is not normal, and the rules can't fire on it because those same
    /// runs put it inside the rolling baseline's range.
    fn settled(&self, in_window: bool) -> bool {
        self.recurrences == 0 && !in_window
    }

    pub(super) fn investigated(&self) -> bool {
        self.feedback.iter().any(|f| {
            matches!(
                f.kind,
                FeedbackKind::Investigated | FeedbackKind::EvidenceRequested | FeedbackKind::Acked
            )
        })
    }

    pub(super) fn dismissed(&self) -> bool {
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
            // Naming only the first cycle froze the story: a change fixed and broken twice more read as
            // "recurred in r6" forever, mentioning neither the later fix nor the later break.
            (true, Some(resolved), Some(again)) => {
                match (self.recurrences, self.firing, self.latest) {
                    (n, true, Some(latest)) if n > 1 => format!(
                        "recurred {n} times, last in {latest}, after first resolving in {resolved} {how}"
                    ),
                    (n, false, Some(latest)) if n > 0 => format!(
                        "resolved again in {latest}, after {} since {resolved} {how}",
                        plural(n as u64, "recurrence")
                    ),
                    _ => format!("recurred in {again}, after resolving in {resolved} {how}"),
                }
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
pub(super) fn outcomes(
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
    // Each run is judged against the signal's own baseline runs, as its own run was. `None`: the run has no
    // verdict to give, so it does not get a vote. It skipped examples those runs ran, or the behavior cap cut
    // what it saw — and a truncated run's absences raise nothing, since admission goes by the arrival order of
    // a behavior's first occurrence. Letting it vote would read its silence about a DISAPPEARED as the
    // behavior coming back, when all that changed is siftr's willingness to say it is gone.
    let fires = |stats: &RunStats, verdict: bool| -> Option<HashSet<Key>> {
        let baseline = Baseline::from_runs(stats, baseline_stats.iter().enumerate());
        let speaks = baseline.incomplete().is_none() && stats.events_past_cap() == 0;
        (!verdict || speaks).then(|| detect(stats, &baseline).iter().map(key).collect())
    };
    let own = fires(&store.run_stats(run.id)?, false).unwrap_or_default();
    let mut later: Vec<RunRecord> = Vec::new();
    let mut later_fires: Vec<HashSet<Key>> = Vec::new();
    for record in store.runs_after(&run.context, run.id)? {
        if until.is_some_and(|until| record.id > until) {
            continue;
        }
        // Nor has a run whose comparison was refused for producing too many signals.
        if record.uncompared.is_some() {
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
            // Each step from not-firing to firing is the change coming back. Counting every one is what makes a
            // second cycle representable at all; two positions can express exactly one.
            let recurrences = still.windows(2).filter(|step| !step[0] && step[1]).count();
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
                recurrences,
                firing: still.last() == Some(&true),
                latest: later.last().map(|record| record.id),
                feedback,
            })
        })
        .collect()
}
