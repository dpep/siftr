//! `siftr run -- CMD…`: pass the command's output through untouched, record it, and exit as it did.

mod terminal;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use siftr::context::Context;
use siftr::observation::Stream;
use siftr::store::{Store, StoredSignal};
use signal_hook::consts::{SIGINT, SIGPIPE, SIGTERM};

use super::Globals;
use crate::output::{self, Changes, FirstRun, Neighbour};
use crate::project;
use crate::record::{Begun, Recorded, Recording};
use crate::sources;

#[derive(clap::Args)]
pub struct Args {
    /// Quiet the COMMAND, not siftr: don't pass its output through (implied by -j). For less of siftr's own
    /// output use --quiet-unless-changed or --no-report
    #[arg(short, long)]
    quiet: bool,

    #[command(flatten)]
    report: Report,

    /// The command and its arguments
    // `last`, not `trailing_var_arg`: only what follows `--` is the command, so a flag siftr doesn't know is a
    // usage error rather than a program name it tries to execute.
    #[arg(
        required = true,
        last = true,
        allow_hyphen_values = true,
        value_name = "CMD"
    )]
    command: Vec<OsString>,
}

/// When siftr's report prints, shared by `run` and `ingest`.
#[derive(clap::Args)]
pub struct Report {
    /// Print siftr's report only when something changed or is still open; warnings and errors always print
    #[arg(long, conflicts_with = "no_report")]
    quiet_unless_changed: bool,

    /// Print nothing of siftr's own, ever; warnings and errors still print. The run is still recorded
    #[arg(long)]
    no_report: bool,
}

impl Report {
    /// `-j` always prints its one document, so either flag would silently do nothing there.
    pub fn check(&self, json: bool) -> Result<()> {
        if self.quiet_unless_changed && json {
            return Err(output::usage(
                "--quiet-unless-changed can't be used with -j, which always prints its document; \
                 read its changes and open_signals instead",
            ));
        }
        if self.no_report && json {
            return Err(output::usage(
                "--no-report can't be used with -j, which always prints its document; \
                 read its changes and open_signals instead",
            ));
        }
        Ok(())
    }

    /// Worth reading: a change `changes` counts, or one still open. A first run, a setup-only change and an
    /// interrupted run aren't. `--no-report` overrides all of that: nothing of siftr's own prints, ever.
    pub fn shows(&self, signals: &[StoredSignal], open: &[StoredSignal]) -> bool {
        !self.no_report
            && (!self.quiet_unless_changed
                || !open.is_empty()
                || output::groups(signals).iter().any(|group| !group.setup))
    }
}

/// Bounds memory if analysis falls behind; passthrough happens before a chunk is queued, so it never waits.
const QUEUED_CHUNKS: usize = 256;
const CHUNK_BYTES: usize = 64 * 1024;
/// Output held while the store gets ready, bounded as output queued behind analysis is.
const PENDING_BYTES: usize = QUEUED_CHUNKS * CHUNK_BYTES;
/// How long recording may take to start: a backstop past the store's own bounds, which wait `BUSY_WAIT` at most
/// for each of opening, the run's row and its recording lock. So the store's error is the one reported, and a
/// run is never begun after siftr gave up on it.
const BEGIN_DEADLINE: Duration = Duration::from_secs(siftr::store::BUSY_WAIT.as_secs() * 4);
/// After the command exits, how long its output may stay open with nothing to read: a background process it
/// started holds it, and siftr must not wait for that process as a terminal wouldn't.
const ORPHAN_GRACE: Duration = Duration::from_secs(1);
const DRAIN_TICK: Duration = Duration::from_millis(100);

enum Event {
    Chunk(Stream, Vec<u8>),
    /// Whoever read siftr's own output went away (`| head`).
    ReaderGone,
    Exited(io::Result<ExitStatus>),
}

pub fn run(args: Args, globals: &Globals) -> ExitCode {
    if let Err(error) = args.report.check(globals.json) {
        output::error(&error, globals.json);
        return ExitCode::from(2);
    }
    let argv: Vec<String> = args
        .command
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();

    let passthrough = !(args.quiet || globals.json);
    // Most tools colour only for a terminal, so the child must see one whenever the user does.
    let pty = if passthrough && io::stdout().is_terminal() {
        terminal::pty()
            .inspect_err(|error| {
                output::warn(format_args!(
                    "no PTY, so the command may not colour: {error}"
                ))
            })
            .ok()
    } else {
        None
    };
    let foreground = terminal::foreground();
    let signals = terminal::catch()
        .inspect_err(|error| {
            output::warn(format_args!(
                "an interrupt will end siftr unrecorded: {error}"
            ))
        })
        .ok();

    let mut command = Command::new(&args.command[0]);
    command
        .args(&args.command[1..])
        .stdin(Stdio::inherit())
        .stderr(Stdio::piped());
    let master = match pty {
        Some(terminal::Pty { master, slave }) => {
            command.stdout(slave);
            Some(master)
        }
        None => {
            command.stdout(Stdio::piped());
            None
        }
    };
    if !foreground {
        command.process_group(0);
    }
    let here = std::env::current_dir().unwrap_or_default();
    let on = sources::enabled();
    let mut sources = sources::for_command(&argv, &here, &on);
    sources.retain_mut(|source| {
        let name = source.name();
        source
            .prepare(&mut command)
            .inspect_err(|error| output::warn(format_args!("{name} skipped: {error:#}")))
            .is_ok()
    });
    // After `prepare`, so a source whose setup failed is reported as read by nothing.
    let prepared: Vec<&str> = (sources.iter())
        .flat_map(|source| [Some(source.name()), source.also_reads()])
        .flatten()
        .collect();
    let reading = sources::reading(&argv, &here, &on, &prepared);

    let started = Instant::now();
    let spawned = command.spawn();
    // Closes our copy of the PTY's slave; reading the master ends only once every copy is closed.
    drop(command);
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            let code = match error.kind() {
                io::ErrorKind::NotFound => 127,
                io::ErrorKind::PermissionDenied => 126,
                _ => 125,
            };
            output::error(
                &anyhow!(error).context(format!("cannot run {}", argv[0])),
                globals.json,
            );
            return ExitCode::from(code);
        }
    };
    let resize = master.as_ref().and_then(|master| master.try_clone().ok());
    let interrupts =
        signals.map(|signals| terminal::forward(signals, child.id(), foreground, resize));
    let mut recorder = Recorder::begin(globals, &argv, started);

    let (events, received) = mpsc::sync_channel(QUEUED_CHUNKS);
    let stdout: Box<dyn Read + Send> = match master {
        Some(master) => Box::new(master),
        None => Box::new(child.stdout.take().expect("stdout is piped")),
    };
    let stderr = child.stderr.take().expect("stderr is piped");
    let relays = [
        relay(
            stdout,
            passthrough.then(io::stdout),
            Stream::Stdout,
            events.clone(),
        ),
        relay(
            stderr,
            passthrough.then(io::stderr),
            Stream::Stderr,
            events.clone(),
        ),
    ];
    thread::spawn(move || {
        let _ = events.send(Event::Exited(child.wait()));
    });

    let mut status = None;
    let mut reader_gone = false;
    let mut quiet_since = Instant::now();
    loop {
        let event = match status {
            None if !recorder.is_starting() => {
                received.recv().map_err(|_| RecvTimeoutError::Disconnected)
            }
            _ => received.recv_timeout(DRAIN_TICK),
        };
        match event {
            Ok(Event::Chunk(stream, chunk)) => recorder.chunk(stream, chunk),
            Ok(Event::ReaderGone) => reader_gone = true,
            Ok(Event::Exited(exited)) => {
                status = Some(exited);
                quiet_since = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        recorder.poll();
        // Once the child has exited, bound the drain by wall clock, not by silence: a chatty background
        // process it started (or the OOM killer's next victim) must not hold siftr open by staying noisy.
        // A relay still blocked writing to our own output (a paused pager) is progress, not an orphan.
        if status.is_some() {
            if !relays.iter().all(Relay::waiting_for_input) {
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= ORPHAN_GRACE {
                output::warn(
                    "the command exited but a background process it started still holds its output; \
                     siftr stopped capturing it",
                );
                break;
            }
        }
    }
    if relays
        .into_iter()
        .filter(|relay| relay.handle.is_finished())
        .any(|relay| relay.handle.join().is_err())
    {
        output::warn("an output relay failed; the capture may be incomplete");
    }

    let status = match status {
        Some(Ok(status)) => status,
        Some(Err(error)) => {
            output::error(
                &anyhow!(error).context("waiting for the command"),
                globals.json,
            );
            return ExitCode::from(125);
        }
        None => {
            output::error(&anyhow!("lost track of the command"), globals.json);
            return ExitCode::from(125);
        }
    };
    let code = exit_code(status);

    let (mut recording, near) = match recorder.into_recording() {
        Ok(started) => started,
        Err(error) => {
            not_recorded(&error, globals.json);
            return exit_as(status, code);
        }
    };
    for source in &mut sources {
        let name = source.name();
        if let Err(error) = source.collect(&mut recording) {
            output::warn(format_args!("{name} lost: {error:#}"));
        }
    }
    // Siftr itself was interrupted (and may or may not have forwarded it), its reader went away and the capture
    // stops short, or the child died from a signal on its own (`kill -9`, the OOM killer).
    let interrupted = interrupts
        .and_then(|interrupts| interrupts.received())
        .or(reader_gone.then_some(SIGPIPE))
        .or_else(|| status.signal());
    let recorded = match interrupted {
        // A partial run would read as behaviors disappearing, next to every baseline it joined.
        Some(signal) => recording
            .finish_interrupted(Some(code), signal)
            .map(|recorded| {
                if args.report.shows(&recorded.signals, &[]) {
                    report(&recorded, &[], globals.json, None);
                }
            }),
        None => recording.finish(Some(code)).map(|recorded| {
            let open = super::still_open(globals, &recorded.run, &recorded.signals);
            // Both quiet flags still mean silence: the note is siftr's own output like any other line.
            if !args.report.shows(&recorded.signals, &open) {
                return;
            }
            // The stored command is the shell-quoted line that names the context, so it pastes back verbatim.
            let first = first_run(&recorded).then(|| FirstRun {
                read: &reading.read,
                unread: &reading.unread,
                neighbour: near.as_ref(),
                command: &recorded.run.command,
            });
            report(&recorded, &open, globals.json, first.as_ref());
            let shown = output::surfaced(&recorded.signals, globals.json)
                .into_iter()
                .chain(output::reminded(&open, globals.json))
                .collect();
            super::record_shown(globals, "run", shown);
        }),
    };
    if let Err(error) = recorded {
        output::warn(format_args!(
            "analysis failed; the command's result is unaffected: {error:#}"
        ));
        not_recorded(&error, globals.json);
    }
    exit_as(status, code)
}

/// Recording, set up beside the command rather than ahead of it: a busy store must delay neither the command's
/// start nor its output.
enum Recorder {
    /// Waiting for the store. Output is held here, bounded, and passthrough never waits on it.
    Starting {
        began: Receiver<Result<Started>>,
        pending: Vec<(Stream, Vec<u8>)>,
        held: usize,
        deadline: Instant,
    },
    Recording(Box<Recording>, Option<Neighbour>),
    /// Not recording this run, and why.
    Off(anyhow::Error),
}

/// A run begun, and what the store knew about this project before it existed.
type Started = (Begun, Option<Neighbour>);

impl Recorder {
    fn begin(globals: &Globals, argv: &[String], started: Instant) -> Self {
        let globals = Globals {
            home: globals.home.clone(),
            json: globals.json,
        };
        let argv = argv.to_vec();
        let (answer, began) = mpsc::channel();
        thread::spawn(move || {
            let begin = || -> Result<Started> {
                let (store, location) = (globals.open_store()?, project::current()?);
                let context = Context::for_command(location.project, &argv);
                let command_line = context.name().to_owned();
                // Before this run exists, so it can't be its own neighbour, and while the store is still open.
                let near = neighbour(&store, &context);
                Begun::new(store, context, &command_line, &location.cwd, started)
                    .map(|begun| (begun, near))
            };
            // If siftr gave up waiting, a run begun this late stays unfinished, and so out of every baseline.
            let _ = answer.send(begin());
        });
        Recorder::Starting {
            began,
            pending: Vec::new(),
            held: 0,
            deadline: started + BEGIN_DEADLINE,
        }
    }

    fn is_starting(&self) -> bool {
        matches!(self, Recorder::Starting { .. })
    }

    fn chunk(&mut self, stream: Stream, bytes: Vec<u8>) {
        self.poll();
        if let Recorder::Starting { held, .. } = self
            && *held + bytes.len() > PENDING_BYTES
        {
            self.settle(Err(anyhow!(
                "the store wasn't ready before the command's output outgrew {} MiB",
                PENDING_BYTES >> 20
            )));
        }
        match self {
            Recorder::Starting { pending, held, .. } => {
                *held += bytes.len();
                pending.push((stream, bytes));
            }
            Recorder::Recording(recording, _) => recording.chunk(&stream, &bytes),
            Recorder::Off(_) => {}
        }
    }

    /// Takes the store's answer if it has come, and gives up on it past the deadline.
    fn poll(&mut self) {
        let Recorder::Starting {
            began, deadline, ..
        } = self
        else {
            return;
        };
        let late = Instant::now() >= *deadline;
        match began.try_recv() {
            Ok(answer) => self.settle(answer),
            Err(TryRecvError::Empty) if late => self.settle(Err(late_error())),
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.settle(Err(stopped_error())),
        }
    }

    /// The recording, once the store answers. The command has exited by now, so waiting holds up only siftr's exit.
    fn into_recording(mut self) -> Result<(Recording, Option<Neighbour>)> {
        if let Recorder::Starting {
            began, deadline, ..
        } = &self
        {
            let answer =
                match began.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(answer) => answer,
                    Err(RecvTimeoutError::Timeout) => Err(late_error()),
                    Err(RecvTimeoutError::Disconnected) => Err(stopped_error()),
                };
            self.settle(answer);
        }
        match self {
            Recorder::Recording(recording, near) => Ok((*recording, near)),
            Recorder::Off(error) => Err(error),
            Recorder::Starting { .. } => Err(late_error()),
        }
    }

    /// Leaves `Starting`: the held output goes into the recording, or is dropped with one warning.
    fn settle(&mut self, answer: Result<Started>) {
        let Recorder::Starting { pending, .. } = self else {
            return;
        };
        let pending = std::mem::take(pending);
        *self = match answer {
            Ok((begun, near)) => {
                let mut recording = Recording::from(begun);
                for (stream, bytes) in pending {
                    recording.chunk(&stream, &bytes);
                }
                Recorder::Recording(Box::new(recording), near)
            }
            Err(error) => {
                output::warn(format_args!("not recording this run: {error:#}"));
                Recorder::Off(error)
            }
        };
    }
}

fn late_error() -> anyhow::Error {
    anyhow!(
        "the store wasn't ready within {}s",
        BEGIN_DEADLINE.as_secs()
    )
}

fn stopped_error() -> anyhow::Error {
    anyhow!("setting up the recording stopped unexpectedly")
}

/// Under `-j` a run that recorded nothing still prints its document; the warning already said why.
fn not_recorded(error: &anyhow::Error, json: bool) {
    if json
        && let Err(printing) = output::emit(true, || output::not_recorded_json(error), |_| Ok(()))
    {
        output::warn(format_args!("could not print the summary: {printing:#}"));
    }
}

struct Relay {
    handle: JoinHandle<()>,
    reading: Arc<AtomicBool>,
}

impl Relay {
    fn waiting_for_input(&self) -> bool {
        self.handle.is_finished() || self.reading.load(Ordering::Relaxed)
    }
}

/// Copies `from` to `to` as bytes arrive, and queues each chunk for analysis.
fn relay<R, W>(mut from: R, mut to: Option<W>, stream: Stream, events: SyncSender<Event>) -> Relay
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    let reading = Arc::new(AtomicBool::new(true));
    let state = Arc::clone(&reading);
    let handle = thread::spawn(move || {
        let mut buf = vec![0; CHUNK_BYTES];
        let mut queueing = true;
        loop {
            state.store(true, Ordering::Relaxed);
            let read = match from.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                // A PTY master reports EIO, not EOF, once the child side is closed on Linux.
                Err(_) => break,
            };
            state.store(false, Ordering::Relaxed);
            let bytes = &buf[..read];
            let mut reader_gone = false;
            // Any other failure keeps draining, so the child never blocks on a full pipe.
            if let Some(out) = &mut to
                && let Err(error) = out.write_all(bytes).and_then(|()| out.flush())
            {
                reader_gone = error.kind() == io::ErrorKind::BrokenPipe;
                to = None;
            }
            if queueing
                && events
                    .send(Event::Chunk(stream.clone(), bytes.to_vec()))
                    .is_err()
            {
                queueing = false;
            }
            // Stop reading so the child's next write fails as it would unwrapped (SIGPIPE). A terminal never
            // reports a broken pipe, so this never has to close a PTY master.
            if reader_gone {
                let _ = events.send(Event::ReaderGone);
                break;
            }
        }
    });
    Relay { handle, reading }
}

/// A signal-killed child reports 128 + signal, as a shell would.
fn exit_code(status: ExitStatus) -> i32 {
    match status.signal() {
        Some(signal) => 128 + signal,
        None => status.code().unwrap_or(1),
    }
}

/// Exits as the child did. A child killed by SIGINT, SIGTERM or SIGPIPE takes siftr down the same way, so a
/// calling shell sees the signal itself (bash stops a loop for SIGINT), not just the code.
fn exit_as(status: ExitStatus, code: i32) -> ExitCode {
    if let Some(signal) = status.signal()
        && matches!(signal, SIGINT | SIGTERM | SIGPIPE)
    {
        let _ = signal_hook::low_level::emulate_default_handler(signal);
    }
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Whether this run is the one that started the context's history: the report's own "nothing to compare with"
/// branch, which is where the first-run note belongs.
fn first_run(recorded: &Recorded) -> bool {
    recorded.baseline_runs.is_empty()
        && recorded.skipped_runs.is_empty()
        && recorded.run.uncompared.is_none()
}

/// How many of a project's recent runs are read to find a context that already has history. A window, not a
/// census: the answer has to name the nearest neighbour and say how much of it was counted.
const NEIGHBOUR_WINDOW: usize = 50;

/// The context in this project whose history a first run of `context` won't share, if there is one. Nearest
/// first — a command sharing leading words is the one the user thought they were adding to — then the one with
/// the most runs. `None` where nothing else has run here: a new project's first run is not a surprise.
fn neighbour(store: &Store, context: &Context) -> Option<Neighbour> {
    let recent = store.runs(context.project(), NEIGHBOUR_WINDOW).ok()?;
    let more = recent.len() == NEIGHBOUR_WINDOW;
    let mut counted: Vec<(&str, u64, siftr::store::RunId)> = Vec::new();
    for run in &recent {
        if run.context == *context || run.end.is_none() || run.interrupted.is_some() {
            continue;
        }
        match counted
            .iter_mut()
            .find(|(name, ..)| *name == run.context.name())
        {
            Some((_, runs, _)) => *runs += 1,
            // Newest first, so the first run seen for a context is also its most recent.
            None => counted.push((run.context.name(), 1, run.id)),
        }
    }
    let (command, runs, _) = counted
        .into_iter()
        .max_by_key(|&(name, runs, id)| (shared_words(context.name(), name), runs, id))?;
    Some(Neighbour {
        command: command.to_owned(),
        runs,
        more,
    })
}

/// Leading words two command lines share: how near a miss one context is for another.
fn shared_words(a: &str, b: &str) -> usize {
    (a.split(' ').zip(b.split(' ')))
        .take_while(|(a, b)| a == b)
        .count()
}

/// Human summary to stderr, which the command's own output doesn't use for data; JSON to stdout.
fn report(
    recorded: &Recorded,
    open: &[siftr::store::StoredSignal],
    json: bool,
    first: Option<&FirstRun<'_>>,
) {
    let changes = Changes {
        run: &recorded.run,
        behaviors: recorded.behaviors,
        streams: Some(&recorded.streams),
        baseline_runs: &recorded.baseline_runs,
        skipped_runs: &recorded.skipped_runs,
        signals: &recorded.signals,
        open_signals: open,
    };
    let printed = match (json, first) {
        // The document on stdout is a contract shared with `changes`, which can't reconstruct the note; it
        // goes to stderr instead, where every other thing siftr says about a run already goes.
        (true, first) => {
            let printed = output::emit(true, || changes.json(), |_| Ok(()));
            if let Some(first) = first {
                output::note_first_run(recorded.run.id, first);
            }
            printed
        }
        (false, Some(first)) => changes
            .human_first_run(&mut io::stderr().lock(), first)
            .map_err(Into::into),
        (false, None) => changes.human(&mut io::stderr().lock()).map_err(Into::into),
    };
    if let Err(error) = printed {
        output::warn(format_args!("could not print the summary: {error:#}"));
    }
}
