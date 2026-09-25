//! Reading: the one way a log gets into siftr, whether it is named (`siftr app.log`), piped
//! (`cat app.log | siftr`) or still being written (`tail -f app.log | siftr`). The three differ in one thing
//! only — whether the input ends — so they take one path:
//!
//! 1. each behavior streams the first time it is seen, while the input is still open;
//! 2. when the input ends, the run is recorded, compared with earlier runs of the same context, and reported;
//! 3. Ctrl-C ends the input too: the run is kept as evidence and reported the same way, but never compared
//!    and never allowed into a later baseline — a partial run reads as mass DISAPPEARED.
//!
//! Streaming is bounded rather than a firehose: `docs/findings/log-contexts.md` §1 measured 100 templates
//! carrying 70% of a real log's lines, so a follow falls quiet after a short warm-up.
//!
//! `siftr DIR` is the exception, and replays a captured scenario — its stdout, stderr and side-channel
//! streams — without streaming: the replay feeds whole files one after another, so a first-seen order taken
//! from it would describe the directory layout rather than the run.
//!
//! No word reaches this module: [`crate::dispatch`] is what routes every spelling of a read here, and the
//! `ingest`/`--dir` tokens it emits are the run's own record of what it did, not a line to type.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::Instant;

use anyhow::{Context as _, Result, bail};
use serde_json::json;
use siftr::aggregate::{Exemplar, MAX_BEHAVIORS, overflow_behavior};
use siftr::behavior::{Behavior, BehaviorId};
use siftr::context::{Context, shell_join};
use siftr::observation::Stream;
use siftr::store::{Order, Store};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use super::{Globals, record_shown};
use crate::output::{self, Changes, Described};
use crate::project;
use crate::record::{Recorded, Recording};
use crate::sources;

#[derive(clap::Args)]
pub struct Args {
    /// File to record [default: stdin]
    file: Option<PathBuf>,

    /// Replay a captured scenario. Dispatch's spelling of `siftr DIR`, and what the run records having read
    #[arg(long, value_name = "DIR", conflicts_with = "file")]
    dir: Option<PathBuf>,

    /// Context to compare this input within [default: ingest, shared by every unnamed pipe in this project]
    #[arg(long, value_name = "NAME")]
    context: Option<String>,

    /// One compact JSON object per behavior as it is first seen, then the whole report as one final object
    #[arg(short = 'J', long)]
    ndjson: bool,

    #[command(flatten)]
    report: super::run::Report,
}

const CHUNK_BYTES: usize = 256 * 1024;

pub fn run(args: Args, globals: &Globals) -> Result<ExitCode> {
    args.report.check(globals.json)?;
    if args.ndjson && globals.json {
        return Err(output::usage(
            "-j and -J can't be used together: -j prints one document when the input ends, -J prints one \
             object per line as it arrives",
        ));
    }
    let location = project::current()?;
    // An unnamed pipe: siftr has no way to know which log it is, so it shares one context with every other.
    let named_context = args.context.clone();
    let context_name = named_context.clone().unwrap_or_else(|| "ingest".to_owned());
    let mut argv = vec![
        "siftr".to_owned(),
        "ingest".to_owned(),
        "--context".to_owned(),
        context_name.clone(),
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
            (vec![(Stream::Stdout, Box::new(file) as Input)], None)
        }
        // `Stdin` rather than its lock: the read blocks on a thread of its own, and `StdinLock` can't cross one.
        (None, None) => (vec![(Stream::Stdout, Box::new(io::stdin()) as Input)], None),
    };

    let context = Context::named(location.project, context_name);
    let mut recording = Recording::begin(
        globals.open_store()?,
        context,
        &shell_join(&argv),
        &location.cwd,
        Instant::now(),
    )?;
    let interrupted = match args.dir {
        // A scenario is replayed whole file by whole file.
        Some(_) => {
            replay(&mut recording, inputs)?;
            None
        }
        // A file or stdin is one stream, and may be one that never ends.
        None => {
            let (stream, input) = inputs.into_iter().next().expect("a file or stdin");
            // Never under `-j`: its one pretty document has one beginning and one end, so it cannot carry a
            // stream. `-J` is the machine-readable form that can.
            let streaming = args.report.streams() && !globals.json;
            let mut shapes = streaming.then(|| Shapes::new(args.ndjson));
            pump(&mut recording, &stream, input, shapes.as_mut())?
        }
    };

    let recorded = match interrupted {
        // The child's own code has no meaning here — siftr read the input itself — so only the signal is kept.
        Some(signal) => recording.finish_interrupted(None, signal)?,
        None => recording.finish(exit_code)?,
    };
    let read = Reading {
        // A scenario replay is a reconstruction of a run that already happened, and `run` itself describes
        // nothing; only an input siftr has just read as a stream is an input it can describe.
        streamed: args.dir.is_none(),
        unnamed: args.file.is_none() && args.dir.is_none() && named_context.is_none(),
        ndjson: args.ndjson,
    };
    report(globals, &args.report, read, &recorded)?;
    // Only now that the run is recorded and reported: siftr dies by the signal it was sent, so a calling
    // shell sees the interrupt itself (bash stops a loop for SIGINT) rather than a command that exited 0.
    if let Some(signal) = interrupted {
        let _ = signal_hook::low_level::emulate_default_handler(signal);
        return Ok(ExitCode::from(u8::try_from(128 + signal).unwrap_or(1)));
    }
    Ok(ExitCode::SUCCESS)
}

/// How the input was read, which is what the report says more than the bare run does.
#[derive(Clone, Copy)]
struct Reading {
    /// Read as one stream, and so describable; a `--dir` replay is not.
    streamed: bool,
    /// Read from a pipe siftr can't name, and so filed in the context every other unnamed pipe shares.
    unnamed: bool,
    ndjson: bool,
}

/// The report once the input has ended, however it ended.
fn report(
    globals: &Globals,
    wanted: &super::run::Report,
    read: Reading,
    recorded: &Recorded,
) -> Result<()> {
    let open = super::still_open(globals, &recorded.run, &recorded.signals);
    if !wanted.shows(&recorded.signals, &open) {
        return Ok(());
    }
    // Reading the stored aggregates back, rather than keeping the analysis, is what makes every number in
    // the block traceable to the rows behind it — and a failure here is a report that says less, never a
    // lost run.
    let described = read.streamed.then(|| describe(globals, recorded)).flatten();
    let changes = Changes {
        run: &recorded.run,
        behaviors: recorded.behaviors,
        streams: Some(&recorded.streams),
        baseline_runs: &recorded.baseline_runs,
        skipped_runs: &recorded.skipped_runs,
        signals: &recorded.signals,
        open_signals: &open,
        described: described.as_ref(),
    };
    if read.ndjson {
        // The last line of the stream: the same document `-j` prints, compact, so a consumer that read the
        // behaviors as they arrived gets the comparison from the same pipe.
        let mut out = io::stdout().lock();
        writeln!(out, "{}", changes.json())?;
    } else {
        output::emit(globals.json, || changes.json(), |w| changes.human(w))?;
    }
    if read.unnamed && recorded.baseline_runs.is_empty() {
        note_unnamed(recorded.run.id);
    }
    let shown = output::surfaced(&recorded.signals, globals.json)
        .into_iter()
        .chain(output::reminded(&open, globals.json))
        .collect();
    record_shown(globals, "ingest", shown);
    Ok(())
}

/// Said once, on the first run of the context an unnamed pipe lands in.
///
/// siftr cannot see where an anonymous pipe's bytes came from: `cat a.log | siftr` and `cat b.log | siftr`
/// are the same file descriptor. So every unnamed pipe in a project shares one context, and `siftr app.log`
/// — which *does* have a name — gets its own. Filing two different logs under one context is the failure
/// `docs/findings/log-contexts.md` §5 measured (one context mixing many sources floods and the comparison is
/// refused); filing one log under two contexts costs a "no earlier runs" line and is fixed by naming it. The
/// cheap, visible error over the expensive, invisible one — and this is the moment the rule is learnable.
fn note_unnamed(run: siftr::store::RunId) {
    let _ = writeln!(
        io::stderr(),
        "siftr: {run}: stdin has no name, so this compares with other unnamed pipes here rather than with \
         `siftr FILE` of the same log; --context NAME gives it one"
    );
}

/// What the input held, for a run with no comparison to report instead. A run that *was* compared has its
/// changes to say, and a description below them would compete with the answer.
fn describe(globals: &Globals, recorded: &Recorded) -> Option<Described> {
    let run = &recorded.run;
    let compared =
        run.interrupted.is_none() && run.uncompared.is_none() && !recorded.baseline_runs.is_empty();
    if compared {
        return None;
    }
    let rows = globals
        .open_store()
        .and_then(|store: Store| store.behaviors(run.id, Order::Count, MAX_BEHAVIORS));
    match rows {
        Ok(rows) => Described::of(rows),
        Err(error) => {
            output::warn(format_args!("input not described: {error:#}"));
            None
        }
    }
}

type Input = Box<dyn Read + Send>;
type Inputs = Vec<(Stream, Input)>;

/// What the reading thread and the signal thread have to say, in arrival order: a chunk queued before a
/// signal is therefore still analyzed, so Ctrl-C loses nothing that had already been read.
enum Msg {
    Chunk(Vec<u8>),
    Eof,
    Failed(io::Error),
    /// A signal arrived; the flag it set, not this message, is what says the run was interrupted.
    Interrupted,
}

/// Reads `input` to its end, or until a signal ends it, feeding `recording` and reporting each behavior the
/// first time it is seen. Returns the signal that interrupted it, if any.
///
/// The blocking read runs on a thread of its own rather than in the loop below. signal-hook installs its
/// handler with `SA_RESTART`, so an interrupted `read` resumes instead of returning — and a `tail -f` that
/// nobody is writing to would never come back to check a flag anyway.
fn pump(
    recording: &mut Recording,
    stream: &Stream,
    input: Input,
    mut shapes: Option<&mut Shapes>,
) -> Result<Option<i32>> {
    // Set inside the handler, so it is already true by the time anything else can observe the signal. A
    // terminal's Ctrl-C reaches the whole pipeline, so the writer feeding siftr dies of the same signal and
    // its EOF races the wake-up below — and an interrupted run read as a complete one would enter a baseline
    // it has no business in. The flag decides, and is read as late as the input allows.
    let caught = Arc::new(AtomicUsize::new(0));
    for signal in [SIGINT, SIGTERM] {
        let value = usize::try_from(signal).unwrap_or(0);
        signal_hook::flag::register_usize(signal, Arc::clone(&caught), value)
            .context("catching signals")?;
    }
    let (tx, rx) = mpsc::channel();
    // Installed before the first byte is read: a signal that arrives early is queued, not fatal.
    let signals = Signals::new([SIGINT, SIGTERM]).context("catching signals")?;
    let watching = tx.clone();
    std::thread::spawn(move || watch(signals, &watching));
    std::thread::spawn(move || read_into(input, &tx));

    for message in rx {
        match message {
            Msg::Chunk(bytes) => {
                recording.chunk(stream, &bytes);
                if let Some(shapes) = shapes.as_deref_mut() {
                    shapes.drain(recording)?;
                }
            }
            Msg::Eof | Msg::Interrupted => break,
            Msg::Failed(error) => return Err(error).context(format!("reading {stream}")),
        }
    }
    if let Some(shapes) = shapes {
        // The last line of an input may carry no newline, and the run records it: report it too.
        recording.flush();
        shapes.drain(recording)?;
    }
    Ok(i32::try_from(caught.load(Ordering::Relaxed))
        .ok()
        .filter(|&signal| signal != 0))
}

fn read_into(mut input: Input, tx: &Sender<Msg>) {
    let mut buf = vec![0; CHUNK_BYTES];
    loop {
        let message = match input.read(&mut buf) {
            Ok(0) => Msg::Eof,
            Ok(read) => Msg::Chunk(buf[..read].to_vec()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => Msg::Failed(error),
        };
        let last = !matches!(message, Msg::Chunk(_));
        if tx.send(message).is_err() || last {
            return;
        }
    }
}

/// Wakes the loop, which would otherwise sit in a read nobody is writing to. The first signal ends the
/// input; a second is a user who has asked twice, and siftr stops rather than making itself the thing that
/// can't be interrupted — losing the run, which is what asking twice means.
fn watch(mut signals: Signals, tx: &Sender<Msg>) {
    let mut first = true;
    for signal in signals.forever() {
        if !first || tx.send(Msg::Interrupted).is_err() {
            std::process::exit(128 + signal);
        }
        first = false;
    }
}

/// A scenario replayed: whole files, one after another, no streaming.
fn replay(recording: &mut Recording, inputs: Inputs) -> Result<()> {
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
    Ok(())
}

/// The streaming half: each behavior the first time it is seen, on stdout as data.
struct Shapes {
    ndjson: bool,
    overflow: BehaviorId,
    capped: bool,
}

impl Shapes {
    fn new(ndjson: bool) -> Shapes {
        // What the lines that follow are, for a reader meeting them for the first time. Not under `-J`,
        // whose reader is a program.
        if !ndjson {
            let _ = writeln!(
                io::stderr(),
                "siftr: reading; each behavior is reported the first time it is seen"
            );
        }
        Shapes {
            ndjson,
            overflow: overflow_behavior().id,
            capped: false,
        }
    }

    /// Prints the behaviors first seen since the last call: at most one line each, and nothing at all for a
    /// line whose behavior has been seen before.
    fn drain(&mut self, recording: &mut Recording) -> io::Result<()> {
        // `io::stdout()` is a LineWriter, so each behavior reaches a pipe as it is written. Wrapping this in
        // a BufWriter for speed would make it block-buffered, and the stream would go silent until it filled.
        let mut out = io::stdout().lock();
        let mut wrote = Ok(());
        let mut hit_cap = false;
        let (ndjson, overflow) = (self.ndjson, self.overflow);
        recording.drain_new_behaviors(|behavior, first| {
            if wrote.is_err() {
                return;
            }
            if behavior.id == overflow {
                // siftr's own marker for events past the cap, not a behavior this input has.
                hit_cap = true;
                return;
            }
            wrote = shape(&mut out, ndjson, behavior, first);
        });
        if hit_cap && !self.capped {
            self.capped = true;
            output::warn(format_args!(
                "{MAX_BEHAVIORS} distinct behaviors seen; a new one is no longer reported"
            ));
        }
        wrote
    }
}

/// One behavior, as data on stdout. The template is all that streams: it is masked under every
/// `SIFTR_REDACT` setting, where a raw line is only masked as far as the setting in force asks.
fn shape(
    out: &mut impl Write,
    ndjson: bool,
    behavior: &Behavior,
    first: &Exemplar,
) -> io::Result<()> {
    if ndjson {
        let document = json!({
            "seq": first.seq,
            "stream": first.stream.to_string(),
            "behavior": behavior.id.to_string(),
            "kind": behavior.kind.as_str(),
            "template": behavior.template,
        });
        return writeln!(out, "{document}");
    }
    // Widest kind is `run.resources`.
    writeln!(
        out,
        "{:>7}  {:<13}  {}",
        first.seq,
        behavior.kind.as_str(),
        behavior.template
    )
}

/// A scenario's streams in the order `run` feeds them: rspec events must precede the log they index.
fn streams() -> [(&'static str, Stream); 4] {
    [
        ("stdout.txt", Stream::Stdout),
        ("stderr.txt", Stream::Stderr),
        ("rspec.ndjson", sources::rspec_events()),
        ("test.log", sources::rails_log()),
    ]
}

/// The one scenario file that is not a stream.
const EXIT_CODE: &str = "exit_code.txt";

/// What a captured scenario is made of, each file optional. Dispatch names these when it refuses a directory,
/// and [`is_scenario`] recognises one by them, so the list a replay reads is the only list.
pub fn scenario_files() -> Vec<&'static str> {
    streams()
        .iter()
        .map(|(name, _)| *name)
        .chain([EXIT_CODE])
        .collect()
}

/// Whether `dir` holds a captured scenario. Recognition rather than a guess: `siftr DIR` replays only a
/// directory that carries at least one of the files a replay reads.
pub fn is_scenario(dir: &Path) -> bool {
    scenario_files().iter().any(|name| dir.join(name).is_file())
}

fn scenario(dir: &Path) -> Result<(Inputs, Option<i32>)> {
    let mut inputs: Inputs = Vec::new();
    for (name, stream) in streams() {
        let path = dir.join(name);
        match File::open(&path) {
            Ok(file) => inputs.push((stream, Box::new(file))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context(format!("opening {}", path.display())),
        }
    }
    let exit_code = match std::fs::read_to_string(dir.join(EXIT_CODE)) {
        Ok(text) => Some(
            text.trim()
                .parse()
                .with_context(|| format!("{}/{EXIT_CODE} is not an exit code", dir.display()))?,
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).context(format!("reading {}/{EXIT_CODE}", dir.display()));
        }
    };
    if inputs.is_empty() && exit_code.is_none() {
        bail!("no captured streams in {}", dir.display());
    }
    Ok((inputs, exit_code))
}
