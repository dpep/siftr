# Resources: what the kernel's accounting costs, leaves empty, and is worth

Why `siftr run` reads `getrusage` at all, why it reports only some of its fields, and why
`run.resources` is the one behavior kind no rule ever judges. Measured 2026-09-16 on an 8-core
Apple M2 (macOS 26.4, 24 GB), release builds, rustc 1.98.1. **Load average was 17–41 throughout**
(other agents were building and testing), which is the machine a coding agent actually runs on.

Reproduce: `cargo run --release --example rusage_cost -- cost|io|spread` (see the top of
`examples/rusage_cost.rs`). The probe re-runs itself once per measurement, because
`RUSAGE_CHILDREN`'s `ru_maxrss` is a maximum over every child a process has reaped rather than a
counter to difference — one child per process is the only shape that reads a single run's peak, and
it is the shape `siftr run` has.

## 1. What one call costs

`cost` times a tight loop of `getrusage`, best of 5 passes over 1M calls, repeated 3 times.

| | pass 1 | pass 2 | pass 3 |
|---|---|---|---|
| `RUSAGE_CHILDREN` | 0.22 µs | 0.13 µs | 0.12 µs |
| `RUSAGE_SELF` | 0.51 µs | 0.25 µs | 0.22 µs |

So **0.12 µs per call warm, 0.25 µs cold**, and the ~0.19 µs the source claims sits inside that
band. An earlier repetition at load 18 measured 0.25 µs while these three ran at load 41, so the
spread here is warm-up, not contention — the same thing §1 of `signals.md` found when load average
failed to predict suite duration.

One call happens per run, after the child is reaped. Against a suite whose own CPU is about 1000 ms
(§3) that is roughly one part in 10 million, which is why the source needs no sampler, no extra
wait and no change to how the child is reaped (CLAUDE.md principle 6).

## 2. What macOS does not fill

`io` spawns a child that writes 64 MiB, `fsync`s it, then reopens and re-reads all of it with
`F_NOCACHE` set, so neither direction can be served from the page cache. Then it reads every field
`RUSAGE_CHILDREN` offers.

| field | after 64 MiB written, fsync'd and re-read uncached |
|---|---|
| `ru_oublock` block output operations | **0** |
| `ru_inblock` block input operations | **0** |
| `ru_majflt` major page faults | **0** |
| `ru_minflt` minor page faults | 321 |
| `ru_nvcsw` voluntary context switches | 206 |
| `ru_nivcsw` involuntary context switches | 5 |
| `ru_nswap`, `ru_msgsnd`, `ru_msgrcv`, `ru_nsignals` | 0 |
| `ru_ixrss`, `ru_idrss`, `ru_isrss` | 0 |
| `ru_utime` + `ru_stime` | 8.01 ms |
| `ru_maxrss` | 3702784 bytes |

The claim holds exactly, and uncached reads make it stronger than the original probe: the disk I/O
counters stay at zero for I/O that provably reached the device. The non-zero rows are the control —
`ru_minflt`, `ru_nvcsw` and `ru_nivcsw` prove the struct is being filled, so the zeros are a
platform that does not count these, not a call that failed.

macOS fills exactly `ru_utime`, `ru_stime`, `ru_maxrss`, `ru_minflt`, `ru_nvcsw` and `ru_nivcsw`,
and `Resources` emits exactly the subset of those it uses. Reporting the rest would be zeros
dressed as data. Disk-I/O bytes exist on macOS only through `proc_pid_rusage` while the process is
still alive, which is a sampler's job and not this source's.

## 3. Why it is evidence and never a signal

`spread` runs one command 40 times, changing nothing between runs, measuring each the way
`siftr run` does. It then applies `signals.md` §2's FREQUENCY rules leave-one-out — baseline = the n
consecutive runs before this one — and counts how often each would fire on a run that changed
nothing. `floor/med` is the smallest move the varying branch could ever call (`2·(max − min)`) as a
fraction of the median, at n = 5.

**`bundle exec rspec` on `dogfood/rails_demo`, 40 runs:**

| metric | median | min | max | max/min | MAD/med | floor/med | n=2 | n=5 | n=10 |
|---|---|---|---|---|---|---|---|---|---|
| wall ms | 1000 | 900 | 1500 | 1.7 | 0.059 | 0.28 | 14/38 | 1/35 | 0/30 |
| cpu ms | 1000 | 900 | 1400 | 1.5 | 0.053 | 0.28 | 14/38 | 1/35 | 0/30 |
| cpu user ms | 660 | 600 | 860 | 1.4 | 0.036 | 0.20 | 13/38 | 0/35 | 0/30 |
| cpu system ms | 350 | 300 | 600 | 2.0 | 0.086 | 0.43 | 13/38 | 1/35 | 0/30 |
| max rss bytes | 100M | 100M | 110M | 1.0 | 0.0015 | 0.0094 | 9/38 | 2/35 | 2/30 |
| voluntary switches | 0 | 0 | 160 | — | — | — | 0/38 | 0/35 | 0/30 |
| involuntary switches | 180 | 66 | 3000 | 46 | 0.56 | 6.2 | 11/38 | 2/35 | 2/30 |

**A busy shell loop, no Ruby, 40 runs**, to show none of this is an artifact of booting Rails:

| metric | median | min | max | max/min | MAD/med | floor/med | n=2 | n=5 | n=10 |
|---|---|---|---|---|---|---|---|---|---|
| wall ms | 670 | 650 | 710 | 1.1 | 0.017 | 0.10 | 11/38 | 0/35 | 0/30 |
| cpu ms | 670 | 650 | 700 | 1.1 | 0.013 | 0.084 | 12/38 | 0/35 | 0/30 |
| cpu system ms | 65 | 63 | 72 | 1.2 | 0.017 | 0.11 | 12/38 | 0/35 | 0/30 |
| max rss bytes | 2.1M | 2.1M | 2.2M | 1.0 | 0 | 0 | 1/38 | 1/35 | 1/30 |
| involuntary switches | 42 | 20 | 460 | 23 | 0.40 | 6.5 | 13/38 | 1/35 | 1/30 |

**No single threshold works, because the failure is at both ends at once.**

- **Too noisy at the smallest baseline.** At n = 2 — the minimum NEW, DISAPPEARED and FREQUENCY
  already require — a FREQUENCY rule on CPU fires on **14 of 38** clean comparisons, about 37%. A
  two-run baseline's range is too narrow to contain the third run.
- **Blind at the baselines that are quiet.** At n = 5 and n = 10 the same rule almost never
  misfires (0–2 of 35), but only because the width it needs has grown past anything worth saying:
  CPU must move by **20–43% of the median** before it can be called at all, and involuntary
  switches by **6.2×**. A regression that adds a fifth to a suite's CPU would pass unremarked.
- **Peak RSS fails the other way.** It is the most stable measure here (MAD/med 0.0015, max/min
  1.0), stable enough that a baseline window is often *exactly* equal — which drops it into the
  FREQUENCY **exact** branch, where any change at all fires. That is 2/35 and 2/30 on the suite and
  1/38 on the busy loop, every one of them a 100 KB move of peak memory that means nothing.

So the same behavior is simultaneously too noisy to judge at n = 2, too blind to be useful at
n = 10, and stable enough at one measure to trip the branch reserved for deterministic counts.
Contrast `signals.md` §1, where all 24 count behaviors were identical across 25 clean runs: counts
earn the exact branch, these do not.

That is why `run.resources` is excluded **by kind rather than by threshold**:
`signal::Comparison::class` maps `Kind::Resources` to `None`, and `judge` returns there before any
rule runs. It is one arm of one match, and it is what keeps every number in this document out of
every rule. `explain` shows the run's CPU and peak RSS beside its baseline's median, and `evidence`
shows the line they came from — which is what tells a slow run from a loaded machine.

### Corrections to the claims this replaces

- The comment in `src/interpret/resources.rs` says a measure rule reading these "would grow a
  FREQUENCY on nearly every run". That is true only at n = 2 (37% of comparisons). At n = 5 and
  n = 10 it fires on 0–6% — the rule's real failure there is blindness, not noise, and for peak RSS
  it is the exact branch rather than the varying one. The conclusion is right; this is the
  measurement behind it.
- `signals.md` §1 reports the demo suite's wall time spanning **10x** (68–707 ms). This document
  measures 1.7x (900–1500 ms) for the same suite, and does not contradict it: §1 timed the suite
  RSpec reports, while `getrusage` covers the whole child, about 1 s of which is bundler and Rails
  boot. The fixed startup cost dominates the total and damps the ratio. Both are the right number
  for what they measure, and the per-example figures in §1 remain the ones a LATENCY rule answers to.

### What this does not cover

One machine, one OS, two commands. Linux fills `ru_inblock`, `ru_oublock` and `ru_majflt`, so §2 is
a macOS finding and the fields siftr emits are deliberately the intersection. Nothing here measures
a machine that is quiet, or a suite whose CPU is dominated by its examples rather than by booting
Rails; both would narrow the spread in §3 without changing which branch each measure falls into.
