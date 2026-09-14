//! `siftr run -- CMD…`: pass the command's output through untouched, record it, and exit with its code.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::process::{Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc::{self, SyncSender};
use std::thread::{self, JoinHandle};

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

    let mut command = Command::new(&args.command[0]);
    // TODO(pty): when siftr's stdout is a terminal, give the child a PTY for stdout (see CLAUDE.md Traps).
    command
        .args(&args.command[1..])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut channels = sidechannel::for_command(&argv);
    channels.retain_mut(|channel| {
        channel
            .prepare(&mut command)
            .inspect_err(|error| output::warn(format_args!("side channel skipped: {error:#}")))
            .is_ok()
    });

    let mut child = match command.spawn() {
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

    let context = Context::for_command(location.project, &argv);
    let command_line = context.name().to_owned();
    let mut recording = Recording::begin(store, context, &command_line, &location.cwd)
        .inspect_err(|error| output::warn(format_args!("not recording this run: {error:#}")))
        .ok();

    let passthrough = !(args.quiet || globals.json);
    let (chunks, received) = mpsc::sync_channel(QUEUED_CHUNKS);
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let relays = [
        relay(
            stdout,
            passthrough.then(io::stdout),
            Stream::Stdout,
            chunks.clone(),
        ),
        relay(stderr, passthrough.then(io::stderr), Stream::Stderr, chunks),
    ];
    for (stream, chunk) in received {
        if let Some(recording) = &mut recording {
            recording.chunk(&stream, &chunk);
        }
    }
    if relays.into_iter().any(|relay| relay.join().is_err()) {
        output::warn("an output relay failed; the capture may be incomplete");
    }

    let code = match child.wait() {
        Ok(status) => exit_code(status),
        Err(error) => {
            output::error(
                &anyhow!(error).context("waiting for the command"),
                globals.json,
            );
            return ExitCode::from(125);
        }
    };

    if let Some(mut recording) = recording {
        for channel in &mut channels {
            if let Err(error) = channel.collect(&mut recording) {
                output::warn(format_args!("side channel lost: {error:#}"));
            }
        }
        match recording.finish(Some(code)) {
            Ok(recorded) => report(&recorded, globals.json),
            Err(error) => output::warn(format_args!(
                "analysis failed; the command's result is unaffected: {error:#}"
            )),
        }
    }
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// Copies `from` to `to` as bytes arrive, and queues each chunk for analysis.
fn relay<R, W>(
    mut from: R,
    mut to: Option<W>,
    stream: Stream,
    chunks: SyncSender<(Stream, Vec<u8>)>,
) -> JoinHandle<()>
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = vec![0; CHUNK_BYTES];
        let mut queueing = true;
        loop {
            let read = match from.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let bytes = &buf[..read];
            // If our own output closes (piped into `head`), keep draining so the child never blocks on a full pipe.
            if let Some(out) = &mut to
                && out.write_all(bytes).and_then(|()| out.flush()).is_err()
            {
                to = None;
            }
            if queueing && chunks.send((stream.clone(), bytes.to_vec())).is_err() {
                queueing = false;
            }
        }
    })
}

/// A signal-killed child reports 128 + signal, as a shell would.
fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
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
