//! `siftr ingest [FILE]`: record a file or stdin as a run's stdout, through the same analysis as `run`.
//! `siftr ingest --dir DIR` replays a captured scenario: its stdout, stderr and side-channel streams.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use siftr_core::context::{Context, shell_join};
use siftr_core::observation::Stream;

use super::Globals;
use crate::output::{self, Changes};
use crate::project;
use crate::record::Recording;
use crate::sidechannel;

#[derive(clap::Args)]
pub struct Args {
    /// File to record [default: stdin]
    file: Option<PathBuf>,

    /// Replay a captured scenario: stdout.txt, stderr.txt, rspec.ndjson, test.log, exit_code.txt (each optional)
    #[arg(long, value_name = "DIR", conflicts_with = "file")]
    dir: Option<PathBuf>,

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
    // Open every input before starting a run, so a bad path leaves no half-recorded run behind.
    let (inputs, exit_code) = match (&args.file, &args.dir) {
        (_, Some(dir)) => {
            argv.extend(["--dir".to_owned(), dir.to_string_lossy().into_owned()]);
            scenario(dir)?
        }
        (Some(path), None) => {
            argv.push(path.to_string_lossy().into_owned());
            let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
            (
                vec![(Stream::Stdout, Box::new(file) as Box<dyn Read>)],
                None,
            )
        }
        (None, None) => (
            vec![(Stream::Stdout, Box::new(io::stdin().lock()) as _)],
            None,
        ),
    };

    let context = Context::named(location.project, args.context);
    let mut recording = Recording::begin(
        globals.open_store()?,
        context,
        &shell_join(&argv),
        &location.cwd,
    )?;
    for (stream, mut input) in inputs {
        let mut buf = vec![0; CHUNK_BYTES];
        loop {
            let read = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error).context(format!("reading {stream}")),
            };
            recording.chunk(&stream, &buf[..read]);
        }
    }
    let recorded = recording.finish(exit_code)?;

    let changes = Changes {
        run: &recorded.run,
        behaviors: recorded.behaviors,
        baseline_runs: &recorded.baseline_runs,
        signals: &recorded.signals,
    };
    output::emit(globals.json, || changes.json(), |w| changes.human(w))?;
    Ok(ExitCode::SUCCESS)
}

type Inputs = Vec<(Stream, Box<dyn Read>)>;

/// A scenario's streams in the order `run` feeds them: rspec events must precede the log they index.
fn scenario(dir: &Path) -> Result<(Inputs, Option<i32>)> {
    let layout = [
        ("stdout.txt", Stream::Stdout),
        ("stderr.txt", Stream::Stderr),
        ("rspec.ndjson", sidechannel::rspec_events()),
        ("test.log", sidechannel::rails_log()),
    ];
    let mut inputs: Inputs = Vec::new();
    for (name, stream) in layout {
        let path = dir.join(name);
        match File::open(&path) {
            Ok(file) => inputs.push((stream, Box::new(file))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context(format!("opening {}", path.display())),
        }
    }
    let exit_code = match std::fs::read_to_string(dir.join("exit_code.txt")) {
        Ok(text) => Some(
            text.trim()
                .parse()
                .with_context(|| format!("{}/exit_code.txt is not an exit code", dir.display()))?,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context(format!("reading {}/exit_code.txt", dir.display()));
        }
    };
    if inputs.is_empty() && exit_code.is_none() {
        bail!("no captured streams in {}", dir.display());
    }
    Ok((inputs, exit_code))
}
