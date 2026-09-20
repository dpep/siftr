//! `siftr history --scorecard`: what became of this project's signals, totalled by kind. The rows it counts are
//! exactly the ones `history --signals` prints, over the same window, so every number drills down to signals and
//! from there to runs and raw lines.
//!
//! The one thing to keep straight before reading a number here: *examined* means a siftr command was run against
//! the signal's behavior, which the feedback ledger records as fact. It does not mean "somebody paid attention" —
//! a change read in the run summary and fixed without asking siftr for more is recorded as unexamined. So the
//! examined rate is a floor on attention, never a measure of what was ignored.

use std::process::ExitCode;

use anyhow::Result;
use serde_json::{Value, json};
use siftr::num::round_sig;
use siftr::signal::SignalKind;
use siftr::store::{RunRecord, Store, StoredSignal};

use super::history::{Outcome, outcomes};
use super::{Globals, found};
use crate::output::{self, label, plural};

/// What became of the signals of one kind. Every field is a count of signals; the rates are derived at the edge.
#[derive(Default)]
struct Tally {
    raised: usize,
    /// Reproducible and backed by unpruned runs, so a later run could give a verdict: open, resolved or recurred.
    judged: usize,
    examined: usize,
    dismissed: usize,
    resolved: usize,
    resolved_unexamined: usize,
    recurred: usize,
    open: usize,
}

impl Tally {
    fn add(&mut self, outcome: &Outcome) {
        self.raised += 1;
        // An unjudged signal is not a signal that went nowhere: retention may have pruned the runs its verdict
        // reads, which also takes its feedback window with it. Counting it in a rate would file "we can't tell"
        // under whichever answer flattered the total.
        match outcome.status() {
            "resolved" => self.resolved += 1,
            "recurred" => self.recurred += 1,
            "open" => self.open += 1,
            _ => return,
        }
        self.judged += 1;
        if outcome.investigated() {
            self.examined += 1;
        } else if outcome.status() == "resolved" {
            self.resolved_unexamined += 1;
        }
        if outcome.dismissed() {
            self.dismissed += 1;
        }
    }

    fn merge(&mut self, other: &Tally) {
        self.raised += other.raised;
        self.judged += other.judged;
        self.examined += other.examined;
        self.dismissed += other.dismissed;
        self.resolved += other.resolved;
        self.resolved_unexamined += other.resolved_unexamined;
        self.recurred += other.recurred;
        self.open += other.open;
    }

    fn unjudged(&self) -> usize {
        self.raised - self.judged
    }

    fn json(&self, kind: Option<SignalKind>) -> Value {
        json!({
            "kind": kind.map(SignalKind::as_str),
            "raised": self.raised,
            "judged": self.judged,
            "unjudged": self.unjudged(),
            "examined": self.examined,
            "examined_rate": rate(self.examined, self.judged),
            "dismissed": self.dismissed,
            "resolved": self.resolved,
            "resolved_unexamined": self.resolved_unexamined,
            "unexamined_rate": rate(self.resolved_unexamined, self.resolved),
            "recurred": self.recurred,
            "open": self.open,
        })
    }
}

/// A proportion rounded where it is built, to the significant figures its denominator backs: 3 of 4 is 0.8, not
/// 0.75 — the fourth signal would move it by a quarter. `None` rather than 0 when nothing backs it at all.
fn rate(numerator: usize, denominator: usize) -> Option<f64> {
    let digits = match denominator {
        0 => return None,
        1..=9 => 1,
        10..=99 => 2,
        _ => 3,
    };
    Some(round_sig(numerator as f64 / denominator as f64, digits))
}

/// `2  0.67`, or the count alone when no rate is defined. The rate keeps exactly the decimals [`rate`] left it,
/// so `1.0` and `0.3` read as rates rather than as a second count, and neither grows a digit it hasn't earned.
fn counted(count: usize, of: usize) -> String {
    match rate(count, of) {
        Some(rate) => format!("{count}  {}", decimals(rate)),
        None => count.to_string(),
    }
}

/// The fewest decimals that still print `value` exactly.
fn decimals(value: f64) -> String {
    (1..=4)
        .map(|places| format!("{value:.places$}"))
        .find(|text| text.parse::<f64>() == Ok(value))
        .unwrap_or_else(|| value.to_string())
}

pub fn render(
    store: &Store,
    runs: &[RunRecord],
    project: &str,
    globals: &Globals,
) -> Result<ExitCode> {
    let mut by_kind: Vec<(SignalKind, Tally)> =
        SignalKind::ALL.map(|kind| (kind, Tally::default())).into();
    for run in runs {
        let signals: Vec<StoredSignal> = store.signals(run.id)?;
        if signals.is_empty() {
            continue;
        }
        for (stored, outcome) in signals.iter().zip(outcomes(store, run, &signals, None)?) {
            let slot = by_kind
                .iter_mut()
                .find(|(kind, _)| *kind == stored.signal.kind);
            if let Some((_, tally)) = slot {
                tally.add(&outcome);
            }
        }
    }
    let mut total = Tally::default();
    for (_, tally) in &by_kind {
        total.merge(tally);
    }
    // A kind this project never raised says nothing; printing a row of zeros for it would pad the table with six
    // rows of noise on a store that has only seen one.
    by_kind.retain(|(_, tally)| tally.raised > 0);

    let as_json = || {
        json!({
            "project": project,
            "runs": runs.len(),
            "kinds": by_kind
                .iter()
                .map(|(kind, tally)| tally.json(Some(*kind)))
                .collect::<Vec<_>>(),
            "total": total.json(None),
        })
    };
    output::emit(globals.json, as_json, |w| {
        writeln!(
            w,
            "what became of {} over {} in {project}",
            plural(total.raised as u64, "signal"),
            plural(runs.len() as u64, "run")
        )?;
        writeln!(
            w,
            "  {:<12} {:>6}  {:>6}  {:<11} {:>8}  {:<11} {:>4}  {:>8}",
            "kind", "raised", "judged", "examined", "resolved", "unexamined", "open", "recurred"
        )?;
        let row = |w: &mut dyn std::io::Write, name: &str, t: &Tally| {
            writeln!(
                w,
                "  {name:<12} {:>6}  {:>6}  {:<11} {:>8}  {:<11} {:>4}  {:>8}",
                t.raised,
                t.judged,
                counted(t.examined, t.judged),
                t.resolved,
                counted(t.resolved_unexamined, t.resolved),
                t.open,
                t.recurred,
            )
        };
        for (kind, tally) in &by_kind {
            row(w, &label(*kind), tally)?;
        }
        if by_kind.len() > 1 {
            row(w, "all", &total)?;
        }
        if total.unjudged() > 0 {
            writeln!(
                w,
                "{} left out of the rates: pruned baseline runs, or today's rules don't reproduce them",
                plural(total.unjudged() as u64, "signal")
            )?;
        }
        if total.dismissed > 0 {
            writeln!(w, "{} dismissed", total.dismissed)?;
        }
        writeln!(
            w,
            "examined = siftr explain, evidence or ack ran on the signal's behavior before it resolved. A change\n\
             read in the run summary and fixed without asking siftr for more reads as unexamined, so this is a\n\
             floor on attention, not a measure of what was ignored."
        )?;
        writeln!(w, "next: siftr history --signals")
    })?;
    Ok(found(total.raised > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_carries_only_the_figures_its_denominator_backs() {
        let cases = [
            (3, 4, Some(0.8)),
            (0, 0, None),
            (8, 13, Some(0.62)),
            (1, 3, Some(0.3)),
            (61, 100, Some(0.61)),
            (2, 2, Some(1.0)),
        ];
        for (numerator, denominator, expected) in cases {
            assert_eq!(
                rate(numerator, denominator),
                expected,
                "{numerator}/{denominator}"
            );
        }
    }

    #[test]
    fn a_rate_prints_the_decimals_it_kept_and_no_more() {
        let cases = [(1.0, "1.0"), (0.0, "0.0"), (0.3, "0.3"), (0.62, "0.62")];
        for (value, expected) in cases {
            assert_eq!(decimals(value), expected);
        }
    }
}
