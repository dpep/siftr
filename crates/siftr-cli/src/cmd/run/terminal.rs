//! The child's view of the terminal: a PTY for its stdout, so it colours as it would unwrapped, and
//! interrupts that reach it exactly once.

use std::fs::File;
use std::io::{self, IsTerminal};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;

use rustix::fs::{Mode, OFlags};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, Signal, getpgrp, kill_process, kill_process_group};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{
    OptionalActions, OutputModes, tcgetattr, tcgetpgrp, tcgetwinsize, tcsetattr, tcsetwinsize,
};
use signal_hook::consts::{SIGINT, SIGTERM, SIGWINCH};
use signal_hook::iterator::Signals;

pub struct Pty {
    pub master: File,
    pub slave: OwnedFd,
}

/// A PTY sized like siftr's stdout that passes the child's bytes through unchanged.
pub fn pty() -> io::Result<Pty> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    // openpt ignores CLOEXEC outside Linux, and a child holding the master would outlive our reads.
    fcntl_setfd(&master, FdFlags::CLOEXEC)?;
    grantpt(&master)?;
    unlockpt(&master)?;
    let name = ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut termios = tcgetattr(&slave)?;
    // Otherwise the line discipline turns "\n" into "\r\n" and the capture no longer holds the child's bytes.
    termios.output_modes.remove(OutputModes::OPOST);
    tcsetattr(&slave, OptionalActions::Now, &termios)?;
    copy_size(&master);
    Ok(Pty {
        master: master.into(),
        slave,
    })
}

fn copy_size(pty: impl AsFd) {
    if let Ok(size) = tcgetwinsize(io::stdout()) {
        let _ = tcsetwinsize(pty, size);
    }
}

/// Whether siftr runs in the foreground of a terminal, which then signals the child itself (Ctrl-C, Ctrl-Z).
pub fn foreground() -> bool {
    let group = getpgrp();
    let (stdin, stdout, stderr) = (io::stdin(), io::stdout(), io::stderr());
    [stdin.as_fd(), stdout.as_fd(), stderr.as_fd()]
        .into_iter()
        .any(|fd| fd.is_terminal() && tcgetpgrp(fd).is_ok_and(|owner| owner == group))
}

/// Catches SIGINT and SIGTERM, so siftr outlives them and can keep the run, and SIGWINCH to resize the PTY.
/// Call before spawning: signals that arrive before `forward` starts are queued, not lost.
pub fn catch() -> io::Result<Signals> {
    Signals::new([SIGINT, SIGTERM, SIGWINCH])
}

/// The first SIGINT or SIGTERM siftr received, if any.
pub struct Interrupts(Arc<AtomicI32>);

impl Interrupts {
    pub fn received(&self) -> Option<i32> {
        Some(self.0.load(Ordering::Relaxed)).filter(|&signal| signal != 0)
    }
}

/// Relays caught signals to the child. In the terminal's foreground the child shares siftr's process group and
/// the terminal already delivered Ctrl-C to it: forwarding would deliver it twice, and RSpec force-quits on the
/// second. Otherwise the child leads its own group, which siftr alone signals.
pub fn forward(
    mut signals: Signals,
    child: u32,
    foreground: bool,
    pty: Option<File>,
) -> Interrupts {
    let first = Arc::new(AtomicI32::new(0));
    let received = Arc::clone(&first);
    let child = i32::try_from(child).ok().and_then(Pid::from_raw);
    thread::spawn(move || {
        for signal in signals.forever() {
            if signal == SIGWINCH {
                if let Some(pty) = &pty {
                    copy_size(pty);
                }
                continue;
            }
            let _ = received.compare_exchange(0, signal, Ordering::Relaxed, Ordering::Relaxed);
            let (Some(child), Some(relayed)) = (child, relayed(signal)) else {
                continue;
            };
            let _ = match (foreground, signal) {
                (true, SIGINT) => continue,
                (true, _) => kill_process(child, relayed),
                (false, _) => kill_process_group(child, relayed),
            };
        }
    });
    Interrupts(first)
}

fn relayed(signal: i32) -> Option<Signal> {
    match signal {
        SIGINT => Some(Signal::INT),
        SIGTERM => Some(Signal::TERM),
        _ => None,
    }
}
