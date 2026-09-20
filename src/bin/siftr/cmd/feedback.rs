//! `siftr ack <SIGNAL>`: answer a signal, so siftr stops reminding you of it and can later learn which signals
//! were worth raising.
//!
//! One verb, because the developer's action is one: *I have dealt with this, stop telling me.* What differs is
//! only the verdict on siftr — `--wrong` says the signal should not have been raised — and that difference is
//! kept as two ledger kinds, `acked` and `dismissed`, because a precision measurement can be built from nothing
//! else. The interface is one command; the evidence is still two facts.

use std::process::ExitCode;

use anyhow::Result;
use siftr::store::{Feedback, FeedbackKind, SignalId};

use super::Globals;
use crate::output::{self, feedback_json, label, printable};

#[derive(clap::Args)]
pub struct Args {
    /// Signal id, like s3
    signal: SignalId,

    /// siftr was wrong: this signal is noise, not a change worth raising
    #[arg(long)]
    wrong: bool,

    /// Why, in a few words
    #[arg(short = 'm', long, value_name = "TEXT")]
    note: Option<String>,
}

/// Recording is this command's whole job, so unlike feedback recorded in passing, a failure is an error.
pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let kind = match args.wrong {
        true => FeedbackKind::Dismissed,
        false => FeedbackKind::Acked,
    };
    let store = globals.open_store()?;
    let stored = store.signal(args.signal)?.ok_or_else(|| {
        output::not_found(format!(
            "no signal {}; siftr history --signals lists recent signals",
            args.signal
        ))
    })?;
    let feedback = Feedback {
        note: args.note,
        ..Feedback::on_signal(kind, "ack", globals.interface(), &stored)
    };
    store.record_feedback(std::slice::from_ref(&feedback))?;
    output::emit(
        globals.json,
        || feedback_json(&feedback),
        |w| {
            writeln!(
                w,
                "{} {}: {} {}",
                stored.id,
                kind.as_str(),
                label(stored.signal.kind),
                printable(&stored.behavior.template, 80)
            )?;
            if let Some(note) = &feedback.note {
                writeln!(w, "note: {}", printable(note, 200))?;
            }
            // The visible consequence, said where the developer is looking: the reminder was the only thing
            // this command changed and the only thing they could have noticed it failing to change.
            writeln!(w, "no longer reminded of this change")?;
            writeln!(w, "next: siftr history --signals")
        },
    )?;
    Ok(ExitCode::SUCCESS)
}
