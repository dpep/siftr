//! Feedback: what people and agents did with what siftr showed them, kept so siftr can learn later which
//! signals matter. Only facts are stored; outcomes such as "resolved after investigation" are derived from
//! runs, signals and these rows, so a better derivation later needs no migration.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::types::Type;
use rusqlite::{Row, params};

use crate::behavior::BehaviorId;
use crate::store::read::parsed;
use crate::store::write::unix_ms;
use crate::store::{RunId, SignalId, Store, StoredSignal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeedbackKind {
    /// A signal was shown: by `run`'s summary, `changes`, or their JSON.
    Surfaced,
    /// `explain` walked a signal to its evidence.
    Investigated,
    /// `evidence` printed a behavior's raw lines.
    EvidenceRequested,
    /// Judged not worth acting on.
    Dismissed,
    /// Being acted on.
    Acked,
}

impl FeedbackKind {
    pub const ALL: [FeedbackKind; 5] = [
        FeedbackKind::Surfaced,
        FeedbackKind::Investigated,
        FeedbackKind::EvidenceRequested,
        FeedbackKind::Dismissed,
        FeedbackKind::Acked,
    ];

    /// Persisted and printed in JSON: stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            FeedbackKind::Surfaced => "surfaced",
            FeedbackKind::Investigated => "investigated",
            FeedbackKind::EvidenceRequested => "evidence_requested",
            FeedbackKind::Dismissed => "dismissed",
            FeedbackKind::Acked => "acked",
        }
    }
}

/// Who was reading: a person, or (almost always) an agent asking for JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interface {
    Human,
    Json,
}

impl Interface {
    pub const ALL: [Interface; 2] = [Interface::Human, Interface::Json];

    /// Persisted and printed in JSON: stable.
    pub const fn as_str(self) -> &'static str {
        match self {
            Interface::Human => "human",
            Interface::Json => "json",
        }
    }
}

/// One thing done with a signal or a behavior.
#[derive(Debug, Clone, PartialEq)]
pub struct Feedback {
    pub at: SystemTime,
    pub kind: FeedbackKind,
    /// The siftr command that recorded it; for `Surfaced`, where the signal was shown.
    pub command: String,
    pub interface: Interface,
    /// The run whose data was shown.
    pub run: RunId,
    pub behavior: BehaviorId,
    /// `None` when the command named a behavior, as `evidence` does. Which signal that served is left to
    /// the reader to derive, rather than guessed here and stored as fact.
    pub signal: Option<SignalId>,
    pub note: Option<String>,
}

impl Feedback {
    /// Now, about `signal`.
    pub fn on_signal(
        kind: FeedbackKind,
        command: &str,
        interface: Interface,
        signal: &StoredSignal,
    ) -> Self {
        Feedback {
            signal: Some(signal.id),
            ..Self::on_behavior(kind, command, interface, signal.run, signal.behavior.id)
        }
    }

    /// Now, about `behavior` as shown from `run`.
    pub fn on_behavior(
        kind: FeedbackKind,
        command: &str,
        interface: Interface,
        run: RunId,
        behavior: BehaviorId,
    ) -> Self {
        Feedback {
            at: SystemTime::now(),
            kind,
            command: command.to_owned(),
            interface,
            run,
            behavior,
            signal: None,
            note: None,
        }
    }
}

impl Store {
    /// Appends `feedback`, all or nothing.
    pub fn record_feedback(&self, feedback: &[Feedback]) -> Result<()> {
        if feedback.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO feedback (at_ms, kind, command, interface, run_id, behavior_id, signal_id, note)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for f in feedback {
                insert.execute(params![
                    unix_ms(f.at),
                    f.kind.as_str(),
                    f.command,
                    f.interface.as_str(),
                    f.run.0,
                    f.behavior.to_string(),
                    f.signal.map(|s| s.0),
                    f.note,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Feedback on `behavior` recorded at or after `since`, oldest first.
    pub fn feedback_on(&self, behavior: BehaviorId, since: SystemTime) -> Result<Vec<Feedback>> {
        let mut stmt = self.conn.prepare(
            "SELECT at_ms, kind, command, interface, run_id, behavior_id, signal_id, note FROM feedback
             WHERE behavior_id = ?1 AND at_ms >= ?2 ORDER BY at_ms, id",
        )?;
        let rows = stmt.query_map(params![behavior.to_string(), unix_ms(since)], feedback)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn feedback(row: &Row<'_>) -> rusqlite::Result<Feedback> {
    Ok(Feedback {
        at: UNIX_EPOCH + Duration::from_millis(row.get::<_, i64>(0)?.unsigned_abs()),
        kind: named(row, 1, &FeedbackKind::ALL, FeedbackKind::as_str)?,
        command: row.get(2)?,
        interface: named(row, 3, &Interface::ALL, Interface::as_str)?,
        run: RunId(row.get(4)?),
        behavior: parsed(row, 5)?,
        signal: row.get::<_, Option<i64>>(6)?.map(SignalId),
        note: row.get(7)?,
    })
}

fn named<T: Copy>(
    row: &Row<'_>,
    at: usize,
    all: &[T],
    name: fn(T) -> &'static str,
) -> rusqlite::Result<T> {
    let text: String = row.get(at)?;
    all.iter()
        .copied()
        .find(|&value| name(value) == text)
        .ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                at,
                Type::Text,
                format!("unknown value {text:?}").into(),
            )
        })
}
