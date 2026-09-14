//! `siftr run -- CMD…`: pass the command's output through untouched, record it, and exit as it did.

mod terminal;

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use siftr_core::context::Context;
use siftr_core::observation::Stream;
use siftr_store::Store;

use super::Globals;
use crate::output::{self, Changes};
use crate::project::{self, Location};
use crate::record::{Recorded, Recording};
use crate::sidechannel;

#[derive(clap::Args)]
pub struct Args {
    /// Don't pass the command's output through (implied by -j)
    #[arg(short, long)]
    quiet: bool,

    /// The command and its arguments
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "CMD"
    )]
    command: Vec<OsString>,
}

/// Bounds memory if analysis falls behind; passthrough happens before a chunk is queued, so it never waits.
const QUEUED_CHUNKS: usize = 256;
const CHUNK_BYTES: usize = 64 * 1024;
/// After the command exits, how long its output may stay open with nothing to read: a background process it
/// started holds it, and siftr must not wait for that process as a terminal wouldn't.
const ORPHAN_GRACE: Duration = Duration::from_secs(1);
const DRAIN_TICK: Duration = Duration::from_millis(100);

enum Event {
    Chunk(Stream, Vec<u8>),
    Exited(io::Result<ExitStatus>),
}

pub fn run(args: Args, globals: &Globals) -> ExitCode {
    let setup =
        || -> Result<(Store, Location)> { Ok((globals.open_store()?, project::current()?)) };
    let (store, location) = match setup() {
        Ok(setup) => setup,
        Err(error) => {
            output::error(&error, globals.json);
            return ExitCode::from(125);
        }
    };
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
    let mut channels = sidechannel::for_command(&argv);
    channels.retain_mut(|channel| {
        channel
            .prepare(&mut command)
            .inspect_err(|error| output::warn(format_args!("side channel skipped: {error:#}")))
            .is_ok()
    });

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

    let context = Context::for_command(location.project, &argv);
    let command_line = context.name().to_owned();
    let mut recording = Recording::begin(store, context, &command_line, &location.cwd)
        .inspect_err(|error| output::warn(format_args!("not recording this run: {error:#}")))
        .ok();

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
    let mut quiet_since = Instant::now();
    loop {
        let event = match status {
            None => received.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(_) => received.recv_timeout(DRAIN_TICK),
        };
        match event {
            Ok(Event::Chunk(stream, chunk)) => {
                if let Some(recording) = &mut recording {
                    recording.chunk(&stream, &chunk);
                }
                quiet_since = Instant::now();
            }
            Ok(Event::Exited(exited)) => {
                status = Some(exited);
                quiet_since = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                // A relay still writing to our output (a paused pager) is progress, not an orphan.
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

    if let Some(mut recording) = recording {
        for channel in &mut channels {
            if let Err(error) = channel.collect(&mut recording) {
                output::warn(format_args!("side channel lost: {error:#}"));
            }
        }
        // Either siftr itself was interrupted (and may or may not have forwarded it), or the child died
        // from a signal on its own (`kill -9`, the OOM killer) without siftr ever seeing one.
        let interrupted = interrupts
            .and_then(|interrupts| interrupts.received())
            .or_else(|| status.signal());
        match interrupted {
            // A partial run would read as behaviors disappearing, next to every baseline it joined.
            Some(signal) => match recording.finish_interrupted(Some(code), signal) {
                Ok(recorded) => report(&recorded, globals.json),
                Err(error) => output::warn(format_args!(
                    "analysis failed; the command's result is unaffected: {error:#}"
                )),
            },
            None => match recording.finish(Some(code)) {
                Ok(recorded) => report(&recorded, globals.json),
                Err(error) => output::warn(format_args!(
                    "analysis failed; the command's result is unaffected: {error:#}"
                )),
            },
        }
    }
    exit_as(status, code)
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
            // If our own output closes (piped into `head`), keep draining so the child never blocks on a full pipe.
            if let Some(out) = &mut to
                && out.write_all(bytes).and_then(|()| out.flush()).is_err()
            {
                to = None;
            }
            if queueing
                && events
                    .send(Event::Chunk(stream.clone(), bytes.to_vec()))
                    .is_err()
            {
                queueing = false;
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

/// Exits as the child did. A child killed by SIGINT or SIGTERM takes siftr down the same way, so a calling
/// shell sees the interruption itself (bash stops a loop for it), not just the code.
fn exit_as(status: ExitStatus, code: i32) -> ExitCode {
    if let Some(signal) = status.signal()
        && matches!(
            signal,
            signal_hook::consts::SIGINT | signal_hook::consts::SIGTERM
        )
    {
        let _ = signal_hook::low_level::emulate_default_handler(signal);
    }
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Human summary to stderr, which the command's own output doesn't use for data; JSON to stdout.
fn report(recorded: &Recorded, json: bool) {
    let changes = Changes {
        run: &recorded.run,
        behaviors: recorded.behaviors,
        baseline_runs: &recorded.baseline_runs,
        signals: &recorded.signals,
    };
    let printed = if json {
        output::emit(true, || changes.json(), |_| Ok(()))
    } else {
        changes.human(&mut io::stderr().lock()).map_err(Into::into)
    };
    if let Err(error) = printed {
        output::warn(format_args!("could not print the summary: {error:#}"));
    }
}
