//! Rendering shared by commands. JSON field names are a contract; human text is not.
//!
//! JSON shapes (every number is already rounded where it was built):
//!
//! - run: `id`, `project`, `context`, `command`, `cwd`, `started_at_ms`, `finished`, `wall_ms`,
//!   `exit_code`, `lines`, `overflow_events` (events past the per-run behavior cap), `interrupted`
//!   (the signal number, or null; interrupted runs are never compared or used as a baseline), `complete` (false
//!   when the run is unfinished, interrupted, or signalled INCOMPLETE: it didn't run what its baseline runs did).
//! - behavior: `id` (16 hex), `kind`, `template`, `roles` (what its paths are, from their names and where they lie:
//!   database|lock|manifest|log|test|source|view|config|dependency|temp; information, no signal reads them).
//! - signal: `id`, `run`, `kind` (error|new|disappeared|frequency|latency|incomplete), `confidence` (number in
//!   [0, 1)), `measure` (count|queries|duration_ms|failed|examples|errors_outside_of_examples), `current`,
//!   `baseline` {`runs`, `present_in`, `median`, `min`, `max`, `failures`}, `exception`, `attribution`
//!   {`scope` (the example's behavior, or null outside examples), `phase` (setup|example|between|teardown:
//!   before the first example, in one, between two, after the last), `setup` (phase is setup), `current`,
//!   `baseline`} or null, `tier` (1 error … 5 outside examples), `group` (rank), `headline`, `evidence_lines`,
//!   `behavior`.
//! - changes (`changes`, `run -j`, `ingest -j`): `run`, `behaviors`, `baseline_runs`, `skipped_runs` [{`run`,
//!   `reason` (no_test_summary|errors_outside_examples|stopped|subset)}] (recent runs of the context left out of
//!   the baseline because they didn't run what this run did, most recent first), `changes` (code-level groups),
//!   `groups` [{`rank`, `setup` (changed outside every example), `headline` (signal id), `signals` (ids),
//!   `disappeared_examples` (null, or {`file`, `examples`} when the group is DISAPPEARED examples of one spec
//!   file collapsed together)}], `signals` (rank order), `open_signals` (signals of earlier runs in
//!   `baseline_runs` still open at this run and not raised again by it, oldest first, one per behavior and
//!   measure; dismissed changes are left out, and so is any change headed by DISAPPEARED).
//! - feedback (`ack -j`, `dismiss -j`): `kind` (surfaced|investigated|evidence_requested|dismissed|acked),
//!   `at_ms`, `command` (the siftr command that recorded it; for surfaced, where it was shown), `interface`
//!   (human|json), `run` (whose data was shown), `behavior` (16 hex), `signal` (id, or null when a
//!   behavior was named, as by `evidence`), `note`.
//! - signal outcomes (`history --signals`), newest run first: `signal`, `outcome` (open|resolved|recurred, or
//!   unknown), `unknown_reason` (null, or why: today's rules no longer reproduce the signal on its own run, or
//!   retention pruned its baseline runs, naming the setting), `resolved_in` and `recurred_in`
//!   (run ids or null), `later_runs` (finished runs of the context judged against the signal's own baseline),
//!   `investigated` (an explain, evidence or ack on its behavior before it resolved), `dismissed`, `feedback`
//!   (on its behavior, from its run until the run it resolved in, oldest first).
//! - exemplar (`explain`, `evidence`): `stream`, `seq` (line number in the run's capture of that stream), `line`
//!   (the kept line, cut at 1024 bytes, credentials masked as `<TOKEN_1>` and, under `SIFTR_REDACT=pii`, emails, IPs
//!   and home directories too), `exception` ({`class`, `message`}, the message whole, read from the capture, when the
//!   line is a test listener event that carries one; else null).
//! - explain (`explain -j`): `signal`, `rule`, `runs` [{`run`, `value`}], `scope` (behavior or null), `scope_runs`,
//!   `evidence` {`run` (this run, or for a disappearance the latest baseline run that had the behavior; null when none
//!   did), `pruned` (null, or the retention setting that pruned that run's lines, e.g. `SIFTR_KEEP_EVIDENCE`),
//!   `exemplars`}, `group` (signal ids).
//! - nothing found (exit 1) is still the command's document: `changes` with `run` null and empty arrays,
//!   `summary` with `run` null, `evidence` with `run` null, `history` an empty array.
//! - not recorded (`run -j` when the store was unusable or busy, or analysis failed; the exit code is still the
//!   command's): that same empty `changes` document plus `not_recorded` {`code`, `message`}, `code` as for an error.
//! - error (exit 2; `run` keeps its own codes), on stdout: {`error`: {`code`, `message`}}. `code` is `usage`
//!   (bad arguments, including an id of the wrong kind), `not_found` (a named run, signal, behavior or context
//!   doesn't exist), `busy` (another siftr held the store past its wait; worth retrying) or `failed` (anything
//!   else).

use std::collections::BTreeMap;
use std::fmt::{self, Display};
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{Value, json};
use siftr::aggregate::{DurationSummary, Exemplar, MAX_BEHAVIORS, Phase, Stats};
use siftr::baseline::Ineligible;
use siftr::behavior::{Behavior, Kind};
use siftr::normalize::{PathRole, PathRoles};
use siftr::num::round_sig;
use siftr::signal::{MIN_BASELINE_RUNS, Signal, SignalKind, disappeared_examples};
use siftr::store::{Feedback, RunId, RunRecord, StoredSignal};

/// Never panics as `eprintln!` does on a closed stderr (`2>&1 | head`): siftr may still be recording.
pub fn warn(message: impl Display) {
    let _ = writeln!(io::stderr(), "siftr: warning: {message}");
}

/// What kind of failure an error is, so a `-j` consumer can branch without reading the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    Usage,
    NotFound,
    Busy,
    Failed,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Usage => "usage",
            ErrorCode::NotFound => "not_found",
            ErrorCode::Busy => "busy",
            ErrorCode::Failed => "failed",
        }
    }
}

/// An error whose code is known where it is raised.
#[derive(Debug)]
struct Coded {
    code: ErrorCode,
    message: String,
}

impl Display for Coded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Coded {}

pub fn usage(message: impl Into<String>) -> anyhow::Error {
    Coded {
        code: ErrorCode::Usage,
        message: message.into(),
    }
    .into()
}

pub fn not_found(message: impl Into<String>) -> anyhow::Error {
    Coded {
        code: ErrorCode::NotFound,
        message: message.into(),
    }
    .into()
}

fn code_of(error: &anyhow::Error) -> ErrorCode {
    error
        .chain()
        .find_map(|cause| match cause.downcast_ref::<Coded>() {
            Some(coded) => Some(coded.code),
            None => cause
                .is::<siftr::store::StoreBusy>()
                .then_some(ErrorCode::Busy),
        })
        .unwrap_or(ErrorCode::Failed)
}

pub fn error(error: &anyhow::Error, json: bool) {
    report_error(code_of(error), &format!("{error:#}"), json);
}

/// Under `-j` the error is the document on stdout, so a consumer reading stdout always gets one.
pub fn report_error(code: ErrorCode, message: &str, json: bool) {
    let _ = if json {
        let document = json!({ "error": { "code": code.as_str(), "message": message } });
        writeln!(
            io::stdout(),
            "{}",
            serde_json::to_string_pretty(&document).unwrap_or_default()
        )
    } else {
        writeln!(io::stderr(), "siftr: error: {message}")
    };
}

/// `run -j` when nothing was recorded: the empty `changes` document, and why.
pub fn not_recorded_json(error: &anyhow::Error) -> Value {
    let mut document = Changes::empty_json();
    document["not_recorded"] =
        json!({ "code": code_of(error).as_str(), "message": format!("{error:#}") });
    document
}

/// Prints to stdout as JSON under `-j`, else as human text.
pub fn emit(
    json: bool,
    as_json: impl FnOnce() -> Value,
    as_human: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<()> {
    let mut out = io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &as_json())?;
        writeln!(out)?;
    } else {
        as_human(&mut out)?;
    }
    Ok(())
}

/// `1 change`, `2 changes`.
pub fn plural(n: u64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// A phase as JSON names it.
pub const fn phase_str(phase: Phase) -> &'static str {
    match phase {
        Phase::Setup => "setup",
        Phase::Example(_) => "example",
        Phase::Between => "between",
        Phase::Teardown => "teardown",
    }
}

/// Where a change outside every example happened, in words.
const fn outside_words(phase: Phase) -> &'static str {
    match phase {
        Phase::Setup => "before the first example",
        Phase::Example(_) => "in an example",
        Phase::Between => "between examples",
        Phase::Teardown => "after the last example",
    }
}

/// Why a recent run wasn't compared, in words whose counts agree with their nouns.
fn ineligible_words(why: Ineligible) -> String {
    match why {
        Ineligible::NoTestSummary => "no test summary".to_owned(),
        Ineligible::ErrorsOutsideExamples { errors, compared } => {
            format!(
                "{} outside examples, {compared} now",
                plural(errors, "error")
            )
        }
        Ineligible::Stopped { ran, loaded } => format!("stopped after {ran} of {loaded} examples"),
        Ineligible::Subset { ran, compared } => {
            format!("ran {}, {compared} now", plural(ran, "example"))
        }
    }
}

/// `r4: no test summary`; runs left out for the same reason share one: `r1 r2: no test summary; r4: …`.
/// `named` is every run the header names, compared or skipped.
fn skipped_label(skipped: &[(RunId, Ineligible)], named: &[RunId]) -> String {
    let mut reasons: Vec<(String, Vec<RunId>)> = Vec::new();
    for &(run, why) in skipped {
        let reason = ineligible_words(why);
        match reasons.iter_mut().find(|(known, _)| *known == reason) {
            Some((_, runs)) => runs.push(run),
            None => reasons.push((reason, vec![run])),
        }
    }
    reasons
        .iter()
        .map(|(reason, runs)| format!("{}: {reason}", runs_label(runs, named)))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Groups shown in full; the rest are counted.
const SHOWN_GROUPS: usize = 3;

/// Signals that share a group, headline first.
pub struct Group<'a> {
    pub rank: u32,
    /// Changed outside every example: the environment or suite hooks, not the code under test.
    pub setup: bool,
    pub members: Vec<&'a StoredSignal>,
}

impl Group<'_> {
    pub fn headline(&self) -> &StoredSignal {
        self.members[0]
    }
}

/// In rank order.
pub fn groups(signals: &[StoredSignal]) -> Vec<Group<'_>> {
    let mut by_rank: BTreeMap<u32, Vec<&StoredSignal>> = BTreeMap::new();
    for s in signals {
        by_rank.entry(s.signal.group).or_default().push(s);
    }
    by_rank
        .into_iter()
        .map(|(rank, mut members)| {
            members.sort_by_key(|s| (!s.signal.headline, s.id));
            Group {
                rank,
                setup: members[0].signal.outside_examples(),
                members,
            }
        })
        .collect()
}

/// Open signals by the change they belonged to, their run's group, in the order given.
fn open_groups(open: &[StoredSignal]) -> Vec<Vec<&StoredSignal>> {
    let mut changes: Vec<Vec<&StoredSignal>> = Vec::new();
    for s in open {
        match changes
            .iter_mut()
            .find(|c| (c[0].run, c[0].signal.group) == (s.run, s.signal.group))
        {
            Some(change) => change.push(s),
            None => changes.push(vec![s]),
        }
    }
    changes
}

/// A run's signals against its baseline: the output of `run`, `ingest` and `changes`.
pub struct Changes<'a> {
    pub run: &'a RunRecord,
    pub behaviors: u64,
    pub baseline_runs: &'a [RunId],
    /// Recent runs left out of the baseline, and why, most recent first.
    pub skipped_runs: &'a [(RunId, Ineligible)],
    pub signals: &'a [StoredSignal],
    /// Earlier signals the baseline has absorbed without their change going away.
    pub open_signals: &'a [StoredSignal],
}

impl Changes<'_> {
    pub fn json(&self) -> Value {
        let groups = groups(self.signals);
        json!({
            "run": run_json(self.run, complete(self.run, self.signals)),
            "behaviors": self.behaviors,
            "baseline_runs": ids(self.baseline_runs),
            "skipped_runs": self.skipped_runs.iter().map(|(run, why)| json!({
                "run": run.to_string(),
                "reason": why.as_str(),
            })).collect::<Vec<_>>(),
            "changes": groups.iter().filter(|g| !g.setup).count(),
            "groups": groups.iter().map(|g| json!({
                "rank": g.rank,
                "setup": g.setup,
                "headline": g.headline().id.to_string(),
                "signals": g.members.iter().map(|s| s.id.to_string()).collect::<Vec<_>>(),
                "disappeared_examples": disappeared_examples(g.members.iter().map(|s| (&s.signal, &s.behavior)))
                    .map(|d| json!({"file": d.file, "examples": d.examples})),
            })).collect::<Vec<_>>(),
            "signals": self.signals.iter().map(signal_json).collect::<Vec<_>>(),
            "open_signals": self.open_signals.iter().map(signal_json).collect::<Vec<_>>(),
        })
    }

    /// The document when there is no run to show.
    pub fn empty_json() -> Value {
        json!({
            "run": null,
            "behaviors": 0,
            "baseline_runs": [],
            "skipped_runs": [],
            "changes": 0,
            "groups": [],
            "signals": [],
            "open_signals": [],
        })
    }

    pub fn human(&self, w: &mut dyn Write) -> io::Result<()> {
        let groups = groups(self.signals);
        let (setup, code): (Vec<&Group<'_>>, Vec<&Group<'_>>) =
            groups.iter().partition(|g| g.setup);
        let run = self.run.id;
        let n = self.baseline_runs.len() as u64;
        let open = open_groups(self.open_signals);
        let named: Vec<RunId> = (self.baseline_runs.iter().copied())
            .chain(self.skipped_runs.iter().map(|&(run, _)| run))
            .collect();
        if let Some(signal) = self.run.interrupted {
            writeln!(
                w,
                "{run}: interrupted by signal {signal}; kept as evidence, not compared, never a baseline"
            )?;
        } else if n == 0 && !self.skipped_runs.is_empty() {
            writeln!(
                w,
                "{run}: no comparable earlier runs (skipped {})",
                skipped_label(self.skipped_runs, &named)
            )?;
        } else if n == 0 {
            let lines = self.run.end.map_or(0, |end| end.lines);
            writeln!(
                w,
                "{run}: {}, {}; no earlier runs of this context to compare with",
                plural(lines, "line"),
                plural(self.behaviors, "behavior")
            )?;
        } else {
            let incomplete = incomplete_reason(&groups, self.signals)
                .map_or_else(String::new, |why| format!(" (incomplete: {why})"));
            let skipped = match self.skipped_runs {
                [] => String::new(),
                skipped => format!("; skipped {}", skipped_label(skipped, &named)),
            };
            // "0 changes" beside a still-open reminder reads as "did it change or not?".
            let moved = match (code.len() as u64, open.len()) {
                (0, 0) => plural(0, "change"),
                (0, open) => format!("no new changes · {open} still open"),
                (changes, 0) => plural(changes, "change"),
                (changes, open) => format!("{} · {open} still open", plural(changes, "change")),
            };
            write!(
                w,
                "{run}{incomplete} vs {} ({}{skipped}): {moved}",
                plural(n, "baseline run"),
                runs_label(self.baseline_runs, &named),
            )?;
            if n < u64::from(MIN_BASELINE_RUNS) {
                write!(
                    w,
                    "; only ERROR can fire until there are {MIN_BASELINE_RUNS} baseline runs"
                )?;
            }
            writeln!(w)?;
        }
        for group in code.iter().take(SHOWN_GROUPS) {
            group_lines(w, group)?;
        }
        if code.len() > SHOWN_GROUPS {
            writeln!(
                w,
                "  … {}, ranked lower: siftr changes {run} -j",
                plural((code.len() - SHOWN_GROUPS) as u64, "more change")
            )?;
        }
        for group in setup {
            let head = group.headline();
            let phase = head.signal.attribution.map_or(Phase::Setup, |a| a.scope);
            writeln!(
                w,
                "  changed {}: {}, the environment or suite hooks rather than the code under test ({})",
                outside_words(phase),
                plural(group.members.len() as u64, "signal"),
                head.id
            )?;
        }
        if self.run.overflow_events > 0 {
            writeln!(
                w,
                "  note: {} events of behaviors past the {MAX_BEHAVIORS}-behavior cap were counted but not told apart",
                self.run.overflow_events
            )?;
        }
        for change in open.iter().take(SHOWN_GROUPS) {
            let head = change[0];
            let supporting = match change.len() {
                1 => String::new(),
                k => format!(" (+{} supporting)", k - 1),
            };
            writeln!(
                w,
                "  still open: {} ({}) {} {}  {}{supporting} · siftr explain {}",
                head.id,
                head.run,
                label(head.signal.kind),
                printable(&head.behavior.template, 60),
                self::change(head),
                head.id
            )?;
        }
        if open.len() > SHOWN_GROUPS {
            writeln!(
                w,
                "  … {} more still open: siftr history --signals",
                open.len() - SHOWN_GROUPS
            )?;
        }
        let next = code
            .first()
            .or(groups.first().as_ref())
            .map(|g| g.headline().id)
            .or(open.first().map(|change| change[0].id));
        match next {
            Some(signal) => writeln!(w, "next: siftr explain {signal}"),
            None => writeln!(w, "next: siftr summary {run}"),
        }
    }
}

fn group_lines(w: &mut dyn Write, group: &Group<'_>) -> io::Result<()> {
    let head = group.headline();
    let s = &head.signal;
    let collapsed = disappeared_examples(group.members.iter().map(|m| (&m.signal, &m.behavior)));
    match &collapsed {
        Some(d) => writeln!(
            w,
            "  {:<4} {:<11} conf {:.2}  {} examples of {}  gone, in all {} baseline runs",
            // Store ids implement Display without honoring width, so pad the rendered string.
            head.id.to_string(),
            label(s.kind),
            s.confidence,
            d.examples,
            d.file,
            s.baseline.runs,
        )?,
        None => writeln!(
            w,
            "  {:<4} {:<11} conf {:.2}  {}  {}",
            head.id.to_string(),
            label(s.kind),
            s.confidence,
            printable(&head.behavior.template, 80),
            change(head),
        )?,
    }
    // In a collapsed group, the other examples are what disappeared, not evidence for it: list only what was
    // attributed to them (e.g. a query scoped to one), never another gone example.
    let supporting: Vec<String> = group.members[1..]
        .iter()
        .filter(|m| {
            collapsed.is_none()
                || !(m.signal.kind == SignalKind::Disappeared
                    && m.behavior.kind == Kind::TestExample)
        })
        .map(|m| {
            format!(
                "{} {}  {}",
                label(m.signal.kind),
                printable(&m.behavior.template, 48),
                change(m)
            )
        })
        .collect();
    if !supporting.is_empty() {
        writeln!(w, "       supporting: {}", supporting.join(" · "))?;
    }
    if let Some(scope) = &head.scope {
        writeln!(w, "       in: {}", printable(&scope.template, 100))?;
    }
    let lines: u64 = group.members.iter().map(|m| m.exemplars).sum();
    // What disappeared has no lines in this run: its evidence is in the baseline runs that had it, as `explain` shows.
    let gone = group
        .members
        .iter()
        .any(|m| matches!(m.signal.kind, SignalKind::Disappeared));
    match (lines, gone) {
        (0, true) => writeln!(w, "       evidence: in the baseline runs, not this one"),
        (lines, true) => writeln!(
            w,
            "       evidence: {}, and the baseline runs for what disappeared",
            plural(lines, "line")
        ),
        (lines, false) => writeln!(w, "       evidence: {}", plural(lines, "line")),
    }
}

/// Runs oldest to newest. A range never spans a run in `named` that isn't one of `runs`: `r1…r7` beside
/// `skipped r7` reads as though r7 was compared.
fn runs_label(runs: &[RunId], named: &[RunId]) -> String {
    let mut sorted = runs.to_vec();
    sorted.sort();
    sorted
        .chunk_by(|a, b| !named.iter().any(|run| a < run && run < b))
        .map(|segment| match segment {
            [first, .., last] if segment.len() > 3 => format!("{first}…{last}"),
            all => ids(all).join(" "),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What moved, in one phrase: `queries 3 → 10`.
pub fn change(stored: &StoredSignal) -> String {
    let s = &stored.signal;
    let b = &s.baseline;
    let at = |v: Option<f64>| v.map_or_else(|| "?".to_owned(), |v| v.to_string());
    let mut text = match s.kind {
        SignalKind::Frequency => {
            let range = if b.min == b.max {
                String::new()
            } else {
                format!(" (baseline {}–{})", at(b.min), at(b.max))
            };
            format!("{} {} → {}{range}", s.measure, at(b.median), s.current)
        }
        SignalKind::Latency => format!("{}ms → {}ms", at(b.median), s.current),
        SignalKind::Incomplete => {
            use siftr::behavior::Kind;
            use siftr::interpret::rspec::summary::EXAMPLES;
            use siftr::signal::measure::COUNT;
            match (s.measure.as_str(), stored.behavior.kind) {
                (COUNT, Kind::Exception) => format!(
                    "failed outside examples: {} now, {} in baseline runs",
                    s.current,
                    at(b.median)
                ),
                (EXAMPLES, _) if b.min == b.max => format!(
                    "ran {}, {} in every baseline run",
                    plural(s.current as u64, "example"),
                    at(b.median)
                ),
                (EXAMPLES, _) => format!(
                    "ran {}, {}–{} in baseline runs",
                    plural(s.current as u64, "example"),
                    at(b.min),
                    at(b.max)
                ),
                (COUNT, _) => "no test summary: stopped before the reporter finished".to_owned(),
                _ => format!(
                    "{} {} now, {} in baseline runs",
                    s.measure,
                    s.current,
                    at(b.median)
                ),
            }
        }
        SignalKind::New => format!(
            "new: {} now, in none of {} baseline runs",
            s.current, b.runs
        ),
        SignalKind::Disappeared => {
            format!(
                "gone: {} → 0, in all {} baseline runs",
                at(b.median),
                b.runs
            )
        }
        SignalKind::Error => {
            let failures = b.failures.unwrap_or(0);
            let with = s
                .exception
                .as_ref()
                .map_or_else(String::new, |e| format!(" with {e}"));
            format!(
                "failed{with}; passed in {} of {} baseline runs",
                b.runs - failures,
                b.runs
            )
        }
    };
    if let Some(a) = s.attribution {
        if a.scope.outside_examples() {
            text.push_str(&format!(" ({})", outside_words(a.scope)));
        } else if (a.current, Some(a.baseline)) != (s.current, b.median) {
            text.push_str(&format!(
                " ({} → {} in this example)",
                a.baseline, a.current
            ));
        }
    }
    text
}

/// Why the rule fired and how its confidence was built.
pub fn rule(s: &Signal) -> String {
    let b = &s.baseline;
    let n = b.runs;
    let c = format!("{:.2}", s.confidence);
    match s.kind {
        SignalKind::Frequency if b.min == b.max => format!(
            "identical in all {n} baseline runs, so any change counts; confidence (n+1)/(n+2) = {c}"
        ),
        SignalKind::Frequency => format!(
            "outside the baseline range by more than twice its width; confidence (n+1)/(n+2) x (1 - width/distance) = {c}"
        ),
        SignalKind::New => {
            format!("absent from all {n} baseline runs; confidence (n+1)/(n+2) = {c}")
        }
        SignalKind::Disappeared => {
            format!("present in all {n} baseline runs; confidence (n+1)/(n+2) = {c}")
        }
        SignalKind::Latency => format!(
            "slower than every baseline run by more than max(100ms, 3x median), with no neighbouring example or suite stall to explain it; confidence (n+1)/(n+2) x e/(1+e) = {c}"
        ),
        SignalKind::Error => format!(
            "failed now; no baseline failure had the same exception; confidence 1 - (failures+1)/(n+2) = {c}"
        ),
        SignalKind::Incomplete => format!(
            "didn't run what all {n} baseline runs did, so what it lacks isn't signalled; confidence (n+1)/(n+2) = {c}"
        ),
    }
}

pub fn label(kind: SignalKind) -> String {
    kind.as_str().to_ascii_uppercase()
}

pub fn ids(runs: &[RunId]) -> Vec<String> {
    runs.iter().map(RunId::to_string).collect()
}

/// Whether a run ran what it set out to: finished, not interrupted, and not signalled INCOMPLETE.
pub fn complete(run: &RunRecord, signals: &[StoredSignal]) -> bool {
    run.end.is_some()
        && run.interrupted.is_none()
        && !signals
            .iter()
            .any(|s| s.signal.kind == SignalKind::Incomplete)
}

/// What an incomplete run didn't do, in a few words, when its first group is headed by INCOMPLETE.
fn incomplete_reason(groups: &[Group<'_>], signals: &[StoredSignal]) -> Option<String> {
    use siftr::behavior::Kind;
    use siftr::interpret::rspec::summary::EXAMPLES;
    use siftr::signal::measure::COUNT;
    let head = groups.first()?.headline();
    if head.signal.kind != SignalKind::Incomplete {
        return None;
    }
    let s = &head.signal;
    Some(match (s.measure.as_str(), head.behavior.kind) {
        (COUNT, Kind::Exception) => {
            let errors: f64 = signals
                .iter()
                .filter(|m| {
                    m.signal.kind == SignalKind::Incomplete && m.behavior.kind == Kind::Exception
                })
                .map(|m| m.signal.current)
                .sum();
            format!("{} outside examples", plural(errors as u64, "error"))
        }
        (EXAMPLES, _) => format!(
            "ran {} of {} examples",
            s.current,
            s.baseline
                .median
                .map_or_else(|| "?".to_owned(), |m| m.to_string())
        ),
        (COUNT, _) => "no test summary".to_owned(),
        _ => format!("{} {}", s.measure, s.current),
    })
}

pub fn run_json(run: &RunRecord, complete: bool) -> Value {
    json!({
        "id": run.id.to_string(),
        "project": run.context.project(),
        "context": run.context.name(),
        "command": run.command,
        "cwd": run.cwd,
        "started_at_ms": run.started_at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
        "finished": run.end.is_some(),
        "wall_ms": run.end.map(|end| end.wall.as_millis() as u64),
        "exit_code": run.end.and_then(|end| end.exit_code),
        "lines": run.end.map(|end| end.lines),
        "overflow_events": run.overflow_events,
        "interrupted": run.interrupted,
        "complete": complete,
    })
}

pub fn behavior_json(behavior: &Behavior) -> Value {
    json!({
        "id": behavior.id.to_string(),
        "kind": behavior.kind.as_str(),
        "template": behavior.template,
        "roles": behavior.roles.iter().map(PathRole::as_str).collect::<Vec<_>>(),
    })
}

/// ` [database, temp]` after a behavior's kind, or nothing when its paths have no role.
pub fn roles_label(roles: PathRoles) -> String {
    if roles.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = roles.iter().map(PathRole::as_str).collect();
    format!(" [{}]", names.join(", "))
}

/// A single occurrence's percentiles are its own duration: the histogram's rounded estimate would disagree with
/// the total printed beside it.
pub fn exact(d: DurationSummary) -> DurationSummary {
    if d.count == 1 {
        DurationSummary {
            p50: d.total,
            p95: d.total,
            max: d.total,
            ..d
        }
    } else {
        d
    }
}

pub fn stats_json(stats: &Stats) -> Value {
    json!({
        "count": stats.count,
        "errors": stats.errors,
        "duration": stats.duration.map(exact).map(|d| json!({
            "count": d.count,
            "total_us": micros(d.total),
            "p50_us": micros(d.p50),
            "p95_us": micros(d.p95),
            "max_us": micros(d.max),
        })),
    })
}

pub fn signal_json(stored: &StoredSignal) -> Value {
    let s = &stored.signal;
    let b = &s.baseline;
    json!({
        "id": stored.id.to_string(),
        "run": stored.run.to_string(),
        "kind": s.kind.as_str(),
        "confidence": s.confidence,
        "measure": s.measure,
        "current": s.current,
        "baseline": {
            "runs": b.runs,
            "present_in": b.present_in,
            "median": b.median,
            "min": b.min,
            "max": b.max,
            "failures": b.failures,
        },
        "exception": s.exception,
        "attribution": s.attribution.map(|a| json!({
            "scope": stored.scope.as_ref().map(behavior_json),
            "phase": phase_str(a.scope),
            "setup": a.scope == Phase::Setup,
            "current": a.current,
            "baseline": a.baseline,
        })),
        "tier": s.tier,
        "group": s.group,
        "headline": s.headline,
        "evidence_lines": stored.exemplars,
        "behavior": behavior_json(&stored.behavior),
    })
}

/// The exception a test listener event carries, class and whole message: a failed example's, or an error outside
/// examples'.
fn failure(event: &str) -> Option<(String, String)> {
    let event: Value = serde_json::from_str(event).ok()?;
    let source = event.get("exception").unwrap_or(&event);
    let class = source.get("class")?.as_str()?.to_owned();
    let message = source
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    Some((class, message.to_owned()))
}

pub fn exemplar_json(exemplar: &Exemplar, event: Option<&str>) -> Value {
    json!({
        "stream": exemplar.stream.to_string(),
        "seq": exemplar.seq,
        "line": exemplar.line,
        "exception": event.and_then(failure).map(|(class, message)| json!({ "class": class, "message": message })),
    })
}

/// `Class: the message's first line`, then its other lines as written, for printing before the raw event.
pub fn exception(event: &str) -> Option<Vec<String>> {
    let (class, message) = failure(event)?;
    // Matcher messages open with a blank line ("\nexpected: 1\n     got: 2").
    let mut lines: Vec<&str> = message
        .lines()
        .map(str::trim_end)
        .skip_while(|line| line.is_empty())
        .collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let first = match lines.first() {
        Some(first) => format!("{class}: {}", first.trim_start()),
        None => class,
    };
    Some(
        std::iter::once(first)
            .chain(lines.iter().skip(1).map(|line| (*line).to_owned()))
            .collect(),
    )
}

/// The signals a changes output shows: all of them as JSON; for a person, only what [`Changes::human`] prints.
pub fn surfaced(signals: &[StoredSignal], json: bool) -> Vec<&StoredSignal> {
    if json {
        return signals.iter().collect();
    }
    let (setup, code): (Vec<Group<'_>>, Vec<Group<'_>>) =
        groups(signals).into_iter().partition(|g| g.setup);
    // As `human` prints them: the first code groups in full, and each setup group by its headline's id.
    code.into_iter()
        .take(SHOWN_GROUPS)
        .flat_map(|g| g.members)
        .chain(setup.into_iter().map(|g| g.members[0]))
        .collect()
}

/// The open signals a changes output shows: all of them as JSON; for a person, the one each printed line names.
pub fn reminded(open: &[StoredSignal], json: bool) -> Vec<&StoredSignal> {
    if json {
        return open.iter().collect();
    }
    open_groups(open)
        .into_iter()
        .take(SHOWN_GROUPS)
        .map(|change| change[0])
        .collect()
}

pub fn feedback_json(feedback: &Feedback) -> Value {
    json!({
        "kind": feedback.kind.as_str(),
        "at_ms": feedback.at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64),
        "command": feedback.command,
        "interface": feedback.interface.as_str(),
        "run": feedback.run.to_string(),
        "behavior": feedback.behavior.to_string(),
        "signal": feedback.signal.map(|s| s.to_string()),
        "note": feedback.note,
    })
}

fn micros(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

pub fn duration(duration: Duration) -> String {
    let us = duration.as_micros() as f64;
    if us < 1e3 {
        format!("{us}µs")
    } else if us < 1e6 {
        format!("{}ms", round_sig(us / 1e3, 3))
    } else {
        format!("{}s", round_sig(us / 1e6, 3))
    }
}

pub fn age(time: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(time)
        .unwrap_or_default()
        .as_secs();
    match secs {
        0..60 => format!("{secs}s ago"),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// For a terminal: drops ANSI escapes and control characters, and truncates to `max_chars`.
pub fn printable(text: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max_chars + 3));
    let mut kept = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                for c in chars.by_ref().skip(1) {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if c.is_control() {
            continue;
        }
        if kept == max_chars {
            out.push('…');
            break;
        }
        out.push(c);
        kept += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_strips_escapes_and_truncates() {
        assert_eq!(
            printable("\x1b[1m\x1b[36mUser Load\x1b[0m\tok", 100),
            "User Loadok"
        );
        assert_eq!(printable("abcdef", 3), "abc…");
        assert_eq!(printable("abc", 3), "abc");
    }

    #[test]
    fn durations_read_at_a_useful_scale() {
        let cases = [
            (Duration::from_micros(450), "450µs"),
            (Duration::from_micros(3_250), "3.25ms"),
            (Duration::from_millis(12_345), "12.3s"),
        ];
        for (d, expected) in cases {
            assert_eq!(duration(d), expected);
        }
    }

    #[test]
    fn baseline_runs_read_oldest_to_newest() {
        let runs: Vec<RunId> = ["r11", "r10", "r9", "r7"]
            .map(|r| r.parse().unwrap())
            .to_vec();
        assert_eq!(runs_label(&runs, &runs), "r7…r11");
        assert_eq!(runs_label(&runs[..2], &runs), "r10 r11");
    }

    #[test]
    fn a_range_never_spans_a_run_named_elsewhere() {
        let run = |r: &str| r.parse::<RunId>().unwrap();
        let compared: Vec<RunId> = ["r1", "r2", "r3", "r4", "r6"].map(run).to_vec();
        let named = [compared.clone(), vec![run("r5"), run("r7")]].concat();
        assert_eq!(runs_label(&compared, &named), "r1…r4 r6");
        assert_eq!(
            runs_label(&compared, &compared),
            "r1…r6",
            "no other run named"
        );
        let skipped = [
            (run("r9"), Ineligible::NoTestSummary),
            (run("r5"), Ineligible::NoTestSummary),
            (run("r7"), Ineligible::NoTestSummary),
            (run("r8"), Ineligible::NoTestSummary),
        ];
        let named = [compared, vec![run("r5"), run("r7"), run("r8"), run("r9")]].concat();
        assert_eq!(
            skipped_label(&skipped, &named),
            "r5 r7 r8 r9: no test summary",
            "r6 was compared, so no r5…r9"
        );
    }

    #[test]
    fn a_disappearance_points_at_the_baseline_for_its_evidence() {
        let behavior = Behavior::new(siftr::behavior::Kind::Log, b"cache hit for key <hex>");
        let stored = StoredSignal {
            id: "s1".parse().unwrap(),
            run: "r4".parse().unwrap(),
            behavior: behavior.clone(),
            signal: Signal {
                kind: SignalKind::Disappeared,
                behavior: behavior.id,
                measure: "count".to_owned(),
                current: 0.0,
                baseline: siftr::signal::BaselineNumbers {
                    runs: 3,
                    present_in: 3,
                    median: Some(5.0),
                    min: Some(5.0),
                    max: Some(5.0),
                    failures: None,
                },
                exception: None,
                attribution: None,
                confidence: 0.8,
                tier: 4,
                group: 1,
                headline: true,
            },
            scope: None,
            exemplars: 0,
        };
        let signals = [stored];
        let mut rendered = Vec::new();
        group_lines(&mut rendered, &groups(&signals)[0]).unwrap();
        let rendered = String::from_utf8(rendered).unwrap();
        assert!(
            rendered.ends_with("       evidence: in the baseline runs, not this one\n"),
            "{rendered}"
        );
    }

    #[test]
    fn an_error_keeps_the_code_it_was_raised_with_through_added_context() {
        assert_eq!(
            code_of(&not_found("no run r9").context("reading")),
            ErrorCode::NotFound
        );
        assert_eq!(code_of(&anyhow::anyhow!("disk full")), ErrorCode::Failed);
    }

    #[test]
    fn skipped_runs_share_a_reason_and_agree_with_their_nouns() {
        let run = |r: &str| r.parse::<RunId>().unwrap();
        let skipped = [
            (run("r4"), Ineligible::NoTestSummary),
            (
                run("r3"),
                Ineligible::ErrorsOutsideExamples {
                    errors: 1,
                    compared: 0,
                },
            ),
            (run("r2"), Ineligible::NoTestSummary),
        ];
        assert_eq!(
            skipped_label(&skipped, &skipped.map(|(run, _)| run)),
            "r2 r4: no test summary; r3: 1 error outside examples, 0 now"
        );
    }
}
