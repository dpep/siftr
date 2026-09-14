//! Rendering shared by commands. JSON field names are a contract; human text is not.
//!
//! JSON shapes (every number is already rounded where it was built):
//!
//! - run: `id`, `project`, `context`, `command`, `cwd`, `started_at_ms`, `finished`, `wall_ms`,
//!   `exit_code`, `lines`, `overflow_events` (events past the per-run behavior cap), `interrupted`
//!   (the signal number, or null; interrupted runs are never compared or used as a baseline).
//! - behavior: `id` (16 hex), `kind`, `template`.
//! - signal: `id`, `run`, `kind` (error|new|disappeared|frequency|latency), `confidence` (number in
//!   [0, 1)), `measure` (count|queries|duration_ms|failed), `current`, `baseline` {`runs`,
//!   `present_in`, `median`, `min`, `max`, `failures`}, `exception`, `attribution` {`scope`
//!   (behavior or null), `setup` (changed before the first example), `current`, `baseline`} or null,
//!   `tier` (1 error … 5 setup), `group` (rank), `headline`, `evidence_lines`, `behavior`.
//! - changes (`changes`, `run -j`, `ingest -j`): `run`, `behaviors`, `baseline_runs`, `changes`
//!   (code-level groups), `groups` [{`rank`, `setup`, `headline` (signal id), `signals` (ids)}],
//!   `signals` (rank order).

use std::collections::BTreeMap;
use std::fmt::Display;
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{Value, json};
use siftr_core::aggregate::{Exemplar, MAX_BEHAVIORS, Stats};
use siftr_core::behavior::Behavior;
use siftr_core::num::round_sig;
use siftr_core::signal::{MIN_BASELINE_RUNS, Signal, SignalKind};
use siftr_store::{RunId, RunRecord, StoredSignal};

pub fn warn(message: impl Display) {
    eprintln!("siftr: warning: {message}");
}

pub fn error(error: &anyhow::Error, json: bool) {
    if json {
        eprintln!("{}", json!({ "error": format!("{error:#}") }));
    } else {
        eprintln!("siftr: error: {error:#}");
    }
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

/// Groups shown in full; the rest are counted.
const SHOWN_GROUPS: usize = 3;

/// Signals that share a group, headline first.
pub struct Group<'a> {
    pub rank: u32,
    /// Changed before the first example: the environment, not the code.
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
                setup: members[0].signal.setup(),
                members,
            }
        })
        .collect()
}

/// A run's signals against its baseline: the output of `run`, `ingest` and `changes`.
pub struct Changes<'a> {
    pub run: &'a RunRecord,
    pub behaviors: u64,
    pub baseline_runs: &'a [RunId],
    pub signals: &'a [StoredSignal],
}

impl Changes<'_> {
    pub fn json(&self) -> Value {
        let groups = groups(self.signals);
        json!({
            "run": run_json(self.run),
            "behaviors": self.behaviors,
            "baseline_runs": ids(self.baseline_runs),
            "changes": groups.iter().filter(|g| !g.setup).count(),
            "groups": groups.iter().map(|g| json!({
                "rank": g.rank,
                "setup": g.setup,
                "headline": g.headline().id.to_string(),
                "signals": g.members.iter().map(|s| s.id.to_string()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "signals": self.signals.iter().map(signal_json).collect::<Vec<_>>(),
        })
    }

    pub fn human(&self, w: &mut dyn Write) -> io::Result<()> {
        let groups = groups(self.signals);
        let (setup, code): (Vec<&Group<'_>>, Vec<&Group<'_>>) =
            groups.iter().partition(|g| g.setup);
        let run = self.run.id;
        let n = self.baseline_runs.len();
        if let Some(signal) = self.run.interrupted {
            writeln!(
                w,
                "{run}: interrupted by signal {signal}; kept as evidence, not compared, never a baseline"
            )?;
        } else if n == 0 {
            let lines = self.run.end.map_or(0, |end| end.lines);
            writeln!(
                w,
                "{run}: {lines} lines, {} behaviors; no earlier runs of this context to compare with",
                self.behaviors
            )?;
        } else {
            let plural =
                |k: usize, word: &str| format!("{k} {word}{}", if k == 1 { "" } else { "s" });
            write!(
                w,
                "{run} vs {} ({}): {}",
                plural(n, "baseline run"),
                runs_label(self.baseline_runs),
                plural(code.len(), "change")
            )?;
            if n < MIN_BASELINE_RUNS as usize {
                write!(
                    w,
                    "; only ERROR can fire until there are {MIN_BASELINE_RUNS}"
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
                "  … {} more changes, ranked lower: siftr changes {run} -j",
                code.len() - SHOWN_GROUPS
            )?;
        }
        for group in setup {
            let head = group.headline();
            writeln!(
                w,
                "  environment changed before the first example: {} signals, not the code under test ({})",
                group.members.len(),
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
        match code.first().or(groups.first().as_ref()) {
            Some(group) => writeln!(w, "next: siftr explain {}", group.headline().id),
            None => writeln!(w, "next: siftr summary {run}"),
        }
    }
}

fn group_lines(w: &mut dyn Write, group: &Group<'_>) -> io::Result<()> {
    let head = group.headline();
    let s = &head.signal;
    writeln!(
        w,
        "  {:<4} {:<11} conf {:<4}  {}  {}",
        // Store ids implement Display without honoring width, so pad the rendered string.
        head.id.to_string(),
        label(s.kind),
        s.confidence,
        printable(&head.behavior.template, 80),
        change(head),
    )?;
    let supporting: Vec<String> = group.members[1..]
        .iter()
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
    let plural = if lines == 1 { "" } else { "s" };
    writeln!(w, "       evidence: {lines} line{plural}")
}

/// The baseline runs, oldest to newest.
fn runs_label(runs: &[RunId]) -> String {
    let mut sorted = runs.to_vec();
    sorted.sort();
    match sorted.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [first, .., last] if sorted.len() > 3 => format!("{first}…{last}"),
        all => ids(all).join(" "),
    }
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
        if a.scope.is_none() {
            text.push_str(" (before the first example)");
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
    let c = s.confidence;
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
    }
}

pub fn label(kind: SignalKind) -> String {
    kind.as_str().to_ascii_uppercase()
}

pub fn ids(runs: &[RunId]) -> Vec<String> {
    runs.iter().map(RunId::to_string).collect()
}

pub fn run_json(run: &RunRecord) -> Value {
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
    })
}

pub fn behavior_json(behavior: &Behavior) -> Value {
    json!({
        "id": behavior.id.to_string(),
        "kind": behavior.kind.as_str(),
        "template": behavior.template,
    })
}

pub fn stats_json(stats: &Stats) -> Value {
    json!({
        "count": stats.count,
        "errors": stats.errors,
        "duration": stats.duration.map(|d| json!({
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
            "setup": a.scope.is_none(),
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

pub fn exemplar_json(exemplar: &Exemplar) -> Value {
    json!({ "stream": exemplar.stream.to_string(), "seq": exemplar.seq, "line": exemplar.line })
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
        assert_eq!(runs_label(&runs), "r7…r11");
        assert_eq!(runs_label(&runs[..2]), "r10 r11");
    }
}
