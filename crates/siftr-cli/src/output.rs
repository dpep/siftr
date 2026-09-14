//! Rendering shared by commands. JSON field names are a contract; human text is not.

use std::fmt::Display;
use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{Value, json};
use siftr_core::aggregate::{Exemplar, Stats};
use siftr_core::behavior::Behavior;
use siftr_core::num::round_sig;
use siftr_core::signal::{MIN_BASELINE_RUNS, SignalKind};
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

/// A run's signals against its baseline: the output of `run`, `ingest` and `changes`.
pub struct Changes<'a> {
    pub run: &'a RunRecord,
    pub behaviors: u64,
    pub baseline_runs: &'a [RunId],
    pub signals: &'a [StoredSignal],
}

impl Changes<'_> {
    pub fn json(&self) -> Value {
        json!({
            "run": run_json(self.run),
            "behaviors": self.behaviors,
            "baseline_runs": ids(self.baseline_runs),
            "signals": self.signals.iter().map(signal_json).collect::<Vec<_>>(),
        })
    }

    pub fn human(&self, w: &mut dyn Write) -> io::Result<()> {
        let lines = self.run.end.map_or(0, |end| end.lines);
        let baseline = self.baseline_runs.len();
        write!(
            w,
            "{}: {lines} lines, {} behaviors, ",
            self.run.id, self.behaviors
        )?;
        if baseline < MIN_BASELINE_RUNS as usize {
            writeln!(
                w,
                "no changes: {baseline} earlier runs of this context (signals need {MIN_BASELINE_RUNS})"
            )?;
        } else {
            let runs: Vec<String> = ids(self.baseline_runs);
            writeln!(
                w,
                "{} changes vs {baseline} baseline runs ({})",
                self.signals.len(),
                runs.join(" ")
            )?;
        }
        for signal in self.signals {
            signal_lines(w, signal)?;
        }
        match self.signals.first() {
            Some(signal) => writeln!(w, "next: siftr explain {}", signal.id),
            None => writeln!(w, "next: siftr summary {}", self.run.id),
        }
    }
}

fn signal_lines(w: &mut dyn Write, stored: &StoredSignal) -> io::Result<()> {
    let signal = &stored.signal;
    let baseline = signal.baseline;
    let runs = signal.baseline_runs;
    writeln!(
        w,
        "  {:<4} {:<11} conf {:<4}  {}  {}  {}",
        // Store ids implement Display without honoring width, so pad the rendered string.
        stored.id.to_string(),
        label(signal.kind),
        signal.confidence,
        stored.behavior.id.short(),
        stored.behavior.kind,
        printable(&stored.behavior.template, 100),
    )?;
    let detail = match signal.kind {
        SignalKind::New => format!("{} now; absent from all {runs} baseline runs", signal.count),
        SignalKind::Disappeared => format!(
            "absent now; baseline mean {} per run, present in {}/{runs}",
            baseline.mean_count, baseline.present_in
        ),
        SignalKind::Frequency => format!(
            "{} now; baseline mean {}, spread {}, present in {}/{runs}",
            signal.count, baseline.mean_count, baseline.count_spread, baseline.present_in
        ),
    };
    writeln!(w, "       {detail}")
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
    let signal = &stored.signal;
    json!({
        "id": stored.id.to_string(),
        "run": stored.run.to_string(),
        "kind": signal.kind.as_str(),
        "confidence": signal.confidence,
        "count": signal.count,
        "baseline": {
            "runs": signal.baseline_runs,
            "present_in": signal.baseline.present_in,
            "mean_count": signal.baseline.mean_count,
            "count_spread": signal.baseline.count_spread,
        },
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
            out.push_str("...");
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
        assert_eq!(printable("abcdef", 3), "abc...");
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
}
