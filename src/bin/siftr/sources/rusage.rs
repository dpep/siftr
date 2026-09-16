//! The kernel's own accounting for the command siftr just ran, taken from the wait.
//!
//! One `getrusage(RUSAGE_CHILDREN)` costs ~0.19µs and needs no sampler, no extra wait and no change
//! to how the child is reaped — so it can't perturb the exit code, the PTY passthrough or the drain
//! (CLAUDE.md principle 6). It is read after the child is reaped, which covers a normal exit, a
//! non-zero exit and a death by signal alike.
//!
//! Only fields the platform actually fills are emitted. macOS leaves the I/O counters
//! (`ru_inblock`, `ru_oublock`, `ru_majflt`) at zero even for a child that fsync'd 64 MiB, so
//! reporting them would be zeros dressed as data. Disk-I/O bytes exist there only through
//! `proc_pid_rusage` while the process is still alive, which is a sampler's job, not this one's.

use std::process::Command;
use std::time::Duration;

use anyhow::{Context as _, Result};
use siftr::interpret::resources::Resources;

use super::Source;
use crate::record::Recording;

/// `ru_maxrss` is bytes on macOS and kibibytes on Linux and the BSDs. Normalizing here is the whole
/// reason a `Resources` can be trusted: the raw field silently lies by 1024x on one of them.
#[cfg(target_vendor = "apple")]
const MAXRSS_TO_BYTES: u64 = 1;
#[cfg(not(target_vendor = "apple"))]
const MAXRSS_TO_BYTES: u64 = 1024;

#[derive(Default)]
pub struct Rusage;

impl Source for Rusage {
    fn name(&self) -> &'static str {
        super::RUSAGE
    }

    /// Nothing to set up: the kernel is already counting.
    fn prepare(&mut self, _command: &mut Command) -> Result<()> {
        Ok(())
    }

    fn collect(&mut self, recording: &mut Recording) -> Result<()> {
        recording.resources(&children()?);
        Ok(())
    }
}

/// What the kernel charged every child this process has reaped. `siftr run` spawns exactly one —
/// the wrapped command — so this is that command's usage, the descendants it reaped included.
fn children() -> Result<Resources> {
    // SAFETY: `libc::rusage` is a C struct of integers and timevals, for which all-zero is a valid
    // value, so `zeroed` produces an initialized one. `getrusage` then writes through the pointer to
    // that live local and reads nothing else; the only documented failures are EINVAL on an unknown
    // `who` and EFAULT on a bad pointer, neither reachable with a constant `who` and a stack local.
    let (usage, code) = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        let code = libc::getrusage(libc::RUSAGE_CHILDREN, &raw mut usage);
        (usage, code)
    };
    if code != 0 {
        return Err(std::io::Error::last_os_error()).context("getrusage(RUSAGE_CHILDREN)");
    }
    Ok(Resources {
        cpu_user: elapsed(usage.ru_utime),
        cpu_system: elapsed(usage.ru_stime),
        max_rss_bytes: nonnegative(usage.ru_maxrss).saturating_mul(MAXRSS_TO_BYTES),
        voluntary_switches: nonnegative(usage.ru_nvcsw),
        involuntary_switches: nonnegative(usage.ru_nivcsw),
    })
}

fn elapsed(time: libc::timeval) -> Duration {
    Duration::from_secs(nonnegative(time.tv_sec)) + Duration::from_micros(nonnegative(time.tv_usec))
}

/// A kernel counter is never negative; a platform that returns one has said nothing, not less
/// than nothing. Generic because `timeval` microseconds are `i32` on macOS and `i64` on Linux:
/// converting at the call site is required on one and a clippy error on the other.
fn nonnegative<T: Into<i64>>(value: T) -> u64 {
    u64::try_from(value.into()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The syscall itself, against this test binary's own reaped children.
    #[test]
    fn a_child_that_burned_cpu_is_charged_more_than_one_that_slept() {
        let before = children().expect("getrusage");
        let run = |script: &str| {
            let status = Command::new("sh").args(["-c", script]).status().unwrap();
            assert!(status.success(), "{script}");
            children().expect("getrusage")
        };
        // RUSAGE_CHILDREN accumulates, so each step is judged against the one before it.
        let slept = run("sleep 0.2");
        let busy = run("i=0; while [ $i -lt 200000 ]; do i=$((i+1)); done");

        let cpu = |a: Resources, b: Resources| b.cpu().saturating_sub(a.cpu());
        assert!(
            cpu(before, slept) < Duration::from_millis(150),
            "sleeping burns little CPU: {:?}",
            cpu(before, slept)
        );
        assert!(
            cpu(slept, busy) > cpu(before, slept),
            "a busy loop burns more than a sleep: {:?} vs {:?}",
            cpu(slept, busy),
            cpu(before, slept)
        );
        // Bytes on both platforms by the time it reaches here, so a shell is megabytes, not kilobytes.
        assert!(
            busy.max_rss_bytes >= 100_000,
            "peak rss normalized to bytes: {}",
            busy.max_rss_bytes
        );
    }
}
