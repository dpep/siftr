//! What the kernel's accounting costs, which of its fields this platform leaves empty, and how far
//! the ones it fills move run to run: the measurements behind `docs/findings/resources.md`.
//!
//!   cargo run --release --example rusage_cost -- cost [--iters N]
//!   cargo run --release --example rusage_cost -- io [--mib N]
//!   cargo run --release --example rusage_cost -- spread [--runs N] [--dir D] -- CMD…
//!
//! `spread` re-runs this example once per measurement (`one`), and `io` once for the writing
//! (`io-child`). `RUSAGE_CHILDREN`'s `ru_maxrss` is a maximum over every child a process has
//! reaped, not a counter to difference, so one child per process is the only shape that reads a
//! single run's peak — and it is the shape `siftr run` has.

use std::fs::File;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Instant;

use siftr::num::{mad, median, round_sig};

/// As `src/bin/siftr/sources/rusage.rs` normalizes it: `ru_maxrss` is bytes on macOS, KiB elsewhere.
#[cfg(target_vendor = "apple")]
const MAXRSS_TO_BYTES: u64 = 1;
#[cfg(not(target_vendor = "apple"))]
const MAXRSS_TO_BYTES: u64 = 1024;

const MIB: usize = 1 << 20;

/// What `one` prints and `spread` summarizes, in order.
const METRICS: [&str; 7] = [
    "wall ms",
    "cpu ms",
    "cpu user ms",
    "cpu system ms",
    "max rss bytes",
    "voluntary switches",
    "involuntary switches",
];

/// Baseline runs a leave-one-out window uses, as `siftr` would (`docs/findings/signals.md` §2).
const BASELINE_RUNS: usize = 5;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("cost") => cost(num(rest, "--iters", 1_000_000)),
        Some("io") => io(num(rest, "--mib", 64)),
        Some("io-child") => io_child(&rest[0], num(rest, "--mib", 64)),
        Some("spread") => spread(num(rest, "--runs", 20), opt(rest, "--dir"), argv(rest)),
        Some("one") => one(opt(rest, "--dir"), argv(rest)),
        _ => eprintln!("usage: rusage_cost cost|io|spread …  — see the top of this file"),
    }
}

/// One `getrusage` call in a tight loop: the whole per-run cost of the `rusage` source.
fn cost(iters: u64) {
    println!("iterations       {iters} per pass, best of 5 passes");
    println!("load average     {} before", round_sig(load(), 2));
    for (label, who) in [
        ("RUSAGE_CHILDREN", libc::RUSAGE_CHILDREN),
        ("RUSAGE_SELF", libc::RUSAGE_SELF),
    ] {
        let mut best = f64::MAX;
        for _ in 0..5 {
            let start = Instant::now();
            let mut acc = 0i64;
            for _ in 0..iters {
                // Reading a field keeps both the call and the struct it filled from being elided.
                acc ^= usage(who).ru_minflt;
            }
            let elapsed = start.elapsed().as_secs_f64();
            std::hint::black_box(acc);
            best = best.min(elapsed);
        }
        let per_call = best * 1e6 / iters as f64;
        println!(
            "{label:<16} {} µs/call ({} ns)",
            round_sig(per_call, 2),
            round_sig(per_call * 1e3, 2)
        );
    }
}

/// Every field `RUSAGE_CHILDREN` reports for a child that did real, fsync'd, uncached disk I/O.
fn io(mib: u64) {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let path = dir.path().join("scratch");
    let exe = std::env::current_exe().expect("this example's path");
    let before = usage(libc::RUSAGE_CHILDREN);
    let status = Command::new(exe)
        .args(["io-child", path.to_str().expect("a UTF-8 temp path")])
        .args(["--mib", &mib.to_string()])
        .status()
        .expect("re-running this example");
    assert!(status.success(), "the child failed: {status}");
    let after = usage(libc::RUSAGE_CHILDREN);

    println!("a child wrote, fsync'd and re-read {mib} MiB with the page cache off");
    println!(
        "cpu                                          {} ms",
        round_sig(
            millis(after.ru_utime) + millis(after.ru_stime)
                - millis(before.ru_utime)
                - millis(before.ru_stime),
            3
        )
    );
    let fields = [
        ("ru_oublock   block output operations", after.ru_oublock),
        ("ru_inblock   block input operations", after.ru_inblock),
        ("ru_majflt    major page faults", after.ru_majflt),
        ("ru_minflt    minor page faults", after.ru_minflt),
        ("ru_nvcsw     voluntary context switches", after.ru_nvcsw),
        ("ru_nivcsw    involuntary context switches", after.ru_nivcsw),
        ("ru_nswap     swaps", after.ru_nswap),
        ("ru_msgsnd    messages sent", after.ru_msgsnd),
        ("ru_msgrcv    messages received", after.ru_msgrcv),
        ("ru_nsignals  signals received", after.ru_nsignals),
        ("ru_ixrss     shared memory size", after.ru_ixrss),
        ("ru_idrss     unshared data size", after.ru_idrss),
        ("ru_isrss     unshared stack size", after.ru_isrss),
    ];
    for (name, value) in fields {
        println!("{name:<44} {value}");
    }
    println!(
        "max rss                                      {} bytes",
        nonnegative(after.ru_maxrss) * MAXRSS_TO_BYTES
    );
}

/// The I/O itself, in a child so `RUSAGE_CHILDREN` is charged for it.
fn io_child(path: &str, mib: u64) {
    let block = vec![0x5au8; MIB];
    let mut file = File::create(path).expect("creating the scratch file");
    uncached(&file);
    for _ in 0..mib {
        file.write_all(&block).expect("writing");
    }
    file.sync_all().expect("fsync");
    drop(file);

    let mut file = File::open(path).expect("reopening the scratch file");
    uncached(&file);
    let mut buffer = vec![0u8; MIB];
    let mut read = 0usize;
    loop {
        match file.read(&mut buffer).expect("reading") {
            0 => break,
            n => read += n,
        }
    }
    assert_eq!(read, mib as usize * MIB, "read back what was written");
}

/// Reads and writes on this handle bypass the page cache, so they have to reach the device.
#[cfg(target_vendor = "apple")]
fn uncached(file: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: `fcntl` with a constant command on a live borrowed fd; `F_NOCACHE` takes an int and
    // touches nothing but the file description.
    let code = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
    assert_eq!(code, 0, "F_NOCACHE");
}

#[cfg(not(target_vendor = "apple"))]
fn uncached(_file: &File) {}

/// N identical runs of one command, each measured as `siftr run` measures it, then the spread of
/// each metric and what `signals.md` §2's FREQUENCY rules would make of it.
fn spread(runs: u64, dir: Option<&str>, argv: &[String]) {
    assert!(!argv.is_empty(), "spread needs a command after --");
    let exe = std::env::current_exe().expect("this example's path");
    println!("command          {}", argv.join(" "));
    println!("load average     {} before", round_sig(load(), 2));
    let mut samples: Vec<Vec<f64>> = Vec::new();
    for _ in 0..runs {
        let mut command = Command::new(&exe);
        command.arg("one");
        if let Some(dir) = dir {
            command.args(["--dir", dir]);
        }
        let output = command
            .arg("--")
            .args(argv)
            .output()
            .expect("re-running this example");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let row: Vec<f64> = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .map(|word| word.parse().expect("a number"))
            .collect();
        assert_eq!(row.len(), METRICS.len(), "one number per metric");
        samples.push(row);
    }
    println!("load average     {} after", round_sig(load(), 2));
    println!("runs             {runs}\n");

    println!(
        "{:<22} {:>12} {:>12} {:>12} {:>8} {:>8} {:>10} {:>8} {:>8} {:>8}",
        "metric", "median", "min", "max", "max/min", "MAD/med", "floor/med", "n=2", "n=5", "n=10"
    );
    for (column, name) in METRICS.iter().enumerate() {
        let values: Vec<f64> = samples.iter().map(|row| row[column]).collect();
        let med = median(&values).expect("a median");
        let (low, high) = range(&values);
        let (.., floor) = backtest(&values, BASELINE_RUNS);
        let fires = |n| {
            let (fired, windows, _) = backtest(&values, n);
            format!("{fired}/{windows}")
        };
        println!(
            "{name:<22} {:>12} {:>12} {:>12} {:>8} {:>8} {:>10} {:>8} {:>8} {:>8}",
            sig(med),
            sig(low),
            sig(high),
            sig(high / low),
            sig(mad(&values).expect("a MAD") / med),
            sig(floor / med),
            fires(2),
            fires(5),
            fires(10)
        );
    }
    println!(
        "\nn=N: FREQUENCY firings / leave-one-out windows, on runs that changed nothing.\nfloor/med: the smallest move the varying branch could call, as a fraction of the median (n=5)."
    );
}

/// One run, measured as `siftr run` measures one: this process spawns exactly one child, so
/// `RUSAGE_CHILDREN` after the reap is that child's alone.
fn one(dir: Option<&str>, argv: &[String]) {
    let (program, rest) = argv.split_first().expect("a command after --");
    let mut command = Command::new(program);
    command
        .args(rest)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let start = Instant::now();
    let status = command.status().expect("spawning the command");
    let wall = start.elapsed().as_secs_f64() * 1e3;
    assert!(status.success(), "the command failed: {status}");

    let usage = usage(libc::RUSAGE_CHILDREN);
    let (user, system) = (millis(usage.ru_utime), millis(usage.ru_stime));
    println!(
        "{wall} {} {user} {system} {} {} {}",
        user + system,
        nonnegative(usage.ru_maxrss) * MAXRSS_TO_BYTES,
        nonnegative(usage.ru_nvcsw),
        nonnegative(usage.ru_nivcsw),
    );
}

/// Leave-one-out over the runs: how often a FREQUENCY rule reading this metric would fire on runs
/// that changed nothing, and the smallest move it could have called (`2·(max − min)`, the varying
/// branch's width). Windows are consecutive, as a real baseline is.
fn backtest(values: &[f64], baseline_runs: usize) -> (usize, usize, f64) {
    let n = baseline_runs.min(values.len().saturating_sub(1));
    let mut fired = 0;
    let mut floors = Vec::new();
    for current in n..values.len() {
        let baseline = &values[current - n..current];
        let (low, high) = range(baseline);
        let med = median(baseline).expect("a median");
        let exact = baseline.iter().all(|value| *value == baseline[0]);
        let moved = (values[current] - med).abs();
        let outside = values[current] < low || values[current] > high;
        if exact {
            floors.push(0.0);
            fired += usize::from(values[current] != baseline[0]);
        } else {
            floors.push(2.0 * (high - low));
            fired += usize::from(outside && moved > 2.0 * (high - low));
        }
    }
    (
        fired,
        values.len() - n,
        median(&floors).expect("a floor per window"),
    )
}

fn range(values: &[f64]) -> (f64, f64) {
    let low = values.iter().copied().fold(f64::MAX, f64::min);
    let high = values.iter().copied().fold(f64::MIN, f64::max);
    (low, high)
}

/// Two significant figures, as this repo rounds where a number is built.
fn sig(value: f64) -> String {
    round_sig(value, 2).to_string()
}

/// The syscall, as `src/bin/siftr/sources/rusage.rs` makes it.
fn usage(who: libc::c_int) -> libc::rusage {
    // SAFETY: all-zero is a valid `libc::rusage` (integers and timevals), so `zeroed` produces an
    // initialized one; `getrusage` writes through the pointer to that live local and reads nothing
    // else. Both `who` values here are the documented constants.
    unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        assert_eq!(libc::getrusage(who, &raw mut usage), 0, "getrusage");
        usage
    }
}

fn millis(time: libc::timeval) -> f64 {
    nonnegative(time.tv_sec) as f64 * 1e3 + nonnegative(time.tv_usec) as f64 / 1e3
}

/// Generic for the same reason the source is: `tv_usec` is `i32` on macOS and `i64` on Linux.
fn nonnegative<T: Into<i64>>(value: T) -> u64 {
    u64::try_from(value.into()).unwrap_or(0)
}

/// The 1-minute load average, which `docs/findings/signals.md` §1 reports beside its noise.
fn load() -> f64 {
    let mut averages = [0.0f64; 3];
    // SAFETY: `getloadavg` writes at most the 3 doubles asked for, through a pointer to this live
    // array, and returns how many it wrote.
    let got = unsafe { libc::getloadavg(averages.as_mut_ptr(), 3) };
    if got >= 1 { averages[0] } else { f64::NAN }
}

fn num(args: &[String], name: &str, default: u64) -> u64 {
    args.iter()
        .position(|arg| arg == name)
        .map_or(default, |at| args[at + 1].parse().expect("a number"))
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let at = args.iter().position(|arg| arg == name)?;
    Some(&args[at + 1])
}

/// Everything after `--`.
fn argv(args: &[String]) -> &[String] {
    args.iter()
        .position(|arg| arg == "--")
        .map_or(&[], |at| &args[at + 1..])
}
