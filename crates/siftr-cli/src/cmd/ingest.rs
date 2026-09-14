//! `siftr ingest [FILE]`: record a file or stdin as a run's stdout, through the same analysis as `run`.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use siftr_core::context::{Context, shell_join};
use siftr_core::observation::Stream;

use super::Globals;
use crate::output::{self, Changes};
use crate::project;
use crate::record::Recording;

#[derive(clap::Args)]
pub struct Args {
    /// File to record [default: stdin]
    file: Option<PathBuf>,

    /// Context to compare this input within
    #[arg(long, value_name = "NAME", default_value = "ingest")]
    context: String,
}

const CHUNK_BYTES: usize = 256 * 1024;

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    let location = project::current()?;
    let mut argv = vec![
        "siftr".to_owned(),
        "ingest".to_owned(),
        "--context".to_owned(),
        args.context.clone(),
    ];
    // Open the input before starting a run, so a bad path leaves no half-recorded run behind.
    let mut input: Box<dyn Read> = match &args.file {
        Some(path) => {
            argv.push(path.to_string_lossy().into_owned());
            Box::new(File::open(path).with_context(|| format!("opening {}", path.display()))?)
        }
        None => Box::new(io::stdin().lock()),
    };

    let context = Context::named(location.project, args.context);
    let mut recording = Recording::begin(
        globals.open_store()?,
        context,
        &shell_join(&argv),
        &location.cwd,
    )?;
    let mut buf = vec![0; CHUNK_BYTES];
    loop {
        let read = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("reading input"),
        };
        recording.chunk(&Stream::Stdout, &buf[..read]);
    }
    let recorded = recording.finish(None)?;

    let changes = Changes {
        run: &recorded.run,
        behaviors: recorded.behaviors,
        baseline_runs: &recorded.baseline_runs,
        signals: &recorded.signals,
    };
    output::emit(globals.json, || changes.json(), |w| changes.human(w))?;
    Ok(ExitCode::SUCCESS)
}
