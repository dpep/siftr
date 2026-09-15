//! `siftr dismiss <SIGNAL>` and `siftr ack <SIGNAL>`: say what a signal was worth, so siftr can learn which
//! signals matter.

use std::process::ExitCode;

use anyhow::Result;
use siftr_store::{Feedback, FeedbackKind, SignalId};

use super::Globals;
use crate::output::{self, feedback_json, label, printable};

#[derive(clap::Args)]
pub struct Args {
    /// Signal id, like s3
    signal: SignalId,

    /// Why, in a few words
    #[arg(short = 'm', long, value_name = "TEXT")]
    note: Option<String>,
}

/// Recording is this command's whole job, so unlike feedback recorded in passing, a failure is an error.
pub fn run(kind: FeedbackKind, command: &str, args: Args, globals: &Globals) -> Result<ExitCode> {
    let store = globals.open_store()?;
    let stored = store.signal(args.signal)?.ok_or_else(|| {
        output::not_found(format!(
            "no signal {}; siftr history --signals lists recent signals",
            args.signal
        ))
    })?;
    let feedback = Feedback {
        note: args.note,
        ..Feedback::on_signal(kind, command, globals.interface(), &stored)
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
            writeln!(w, "next: siftr changes {}", stored.run)
        },
    )?;
    Ok(ExitCode::SUCCESS)
}
