//! `siftr sources`: what siftr can read here — the command's own output and the side channels a command writes
//! elsewhere — whether each is on, and whether it applies. Read-only: it prepares nothing and records nothing.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use siftr::context::shell_join;
use siftr::observation::Stream;

use super::Globals;
use crate::output;
use crate::sources::{Listed, enabled, survey};

#[derive(clap::Args)]
pub struct Args {
    /// The command to judge, as `siftr run` would wrap it [default: judge the directory alone]
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "CMD"
    )]
    command: Vec<OsString>,
}

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let argv: Vec<String> = args
        .command
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let here = std::env::current_dir().context("reading the current directory")?;
    let listed = survey(&argv, &here, &enabled());
    let command = (!argv.is_empty()).then(|| shell_join(&argv));
    output::emit(
        globals.json,
        || document(&listed, command.as_deref(), &here),
        |w| human(w, &listed, command.as_deref(), &here),
    )?;
    Ok(ExitCode::SUCCESS)
}

fn document(listed: &[Listed], command: Option<&str>, here: &Path) -> Value {
    json!({
        "cwd": here.to_string_lossy(),
        "command": command,
        "sources": listed.iter().map(|source| json!({
            "name": source.name,
            "stream": source.stream.as_ref().map(Stream::to_string),
            "about": source.about,
            "on": source.on,
            "applies": source.applies,
            "why": source.why,
        })).collect::<Vec<_>>(),
    })
}

fn human(
    w: &mut dyn Write,
    listed: &[Listed],
    command: Option<&str>,
    here: &Path,
) -> io::Result<()> {
    match command {
        Some(command) => writeln!(w, "sources for {command} in {}", here.display())?,
        None => writeln!(w, "sources in {}", here.display())?,
    }
    for source in listed {
        // The command's own output is named after its stream, so naming it twice would say nothing; a source
        // that reads no bytes has no stream for evidence to point at, and an em dash says so rather than
        // naming a file that doesn't exist.
        let stream = match source.stream.as_ref().map(Stream::to_string) {
            None => "— ".to_owned(),
            Some(same) if same == source.name => String::new(),
            Some(stream) => format!("{stream} — "),
        };
        writeln!(
            w,
            "  {:<9}  {:<3}  {:<14}  {stream}{} ({})",
            source.name,
            if source.on { "on" } else { "off" },
            if source.applies {
                "applies"
            } else {
                "does not apply"
            },
            source.about,
            source.why,
        )?;
    }
    match command {
        // Without a command only the directory was judged, so say how to ask the precise question.
        Some(command) => writeln!(w, "next: siftr run -- {command}"),
        None => writeln!(w, "next: siftr sources -- CMD"),
    }
}
