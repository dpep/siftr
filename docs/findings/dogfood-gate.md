# Dogfooding: which of this repo's own commands can host a real baseline

`docs/findings/attention.md` closes on the thing this repo cannot currently do:
"Daily use of one real context. The scorecard needs runs a person actually
waited for and signals a person actually read." Every one of the 929 signals on
this machine came from a harness replaying fixtures. This is what siftr's own
test suite can and cannot supply, which invocation is the right host, and what
the first real baseline turned out to say.

## 1. `script/verify` is the wrong host, and the reason generalizes

A context is the project root plus the normalized command
(`src/context.rs`), and the root is the nearest manifest inside the repository
(`src/bin/siftr/project.rs`). `script/verify` verifies a SHA in a throwaway
worktree at `$SCRATCH/wt-$SHA`, so wrapping its gate would mint a project root
siftr has never seen on every run: one hundred contexts of one run each, no
baseline anywhere, and a `history` listing that is pure noise.

Giving `run` a `--context` flag would paper over the root but not fix the
judgement: verify's runs are not the corpus anyway. It runs once per change on
code the developer has already stopped editing, it redirects the suite's output
into a log file it greps itself, and its dogfood loop deliberately wipes
`SIFTR_HOME` so its scenarios stay reproducible. A baseline built from it would
measure the verification harness, not the work.

The generalization: **wrap the invocation that already has a stable root and a
stable command line.** For this repo that is the pre-commit gate run in the
working tree — the same `cargo test --no-fail-fast` CLAUDE.md already asks for
before every commit, run many times a day against code that actually changes.
No new CLI surface is needed for it, and `siftr sources -- cargo test
--no-fail-fast` confirms what it can read: stdout, stderr and rusage.

## 2. `script/gate`, and why siftr cannot break it

`script/gate` runs the three gate steps in the working tree and records the test
step. Principle 6 is absolute here and it is the *wrapper's* job, not siftr's:
siftr's own contract covers a siftr that is running, and the gate has to survive
a siftr that is missing, unbuilt, or broken mid-refactor.

Three layers, in order:

1. **Preflight.** A candidate binary is used only if it answers `--version`. A
   missing file, a half-written build and a binary that aborts at startup are
   all settled before any step runs, and the gate says so and runs bare.
2. **Reserved codes.** siftr documents exactly three codes for its own failure —
   125 before the spawn, 126 cannot execute, 127 not found — and each means the
   command never ran. The wrapper re-runs it unwrapped and reports *that* code,
   so the gate never reports a status no command produced.
3. **`SIFTR_GATE=off`.** One environment variable takes siftr out of the path
   entirely, for the case nobody predicted.

The one failure this cannot catch is a siftr that exits 0 for a failing child,
and no wrapper can: that is what `tests/capture_run.rs` is for. What the wrapper
*can* be held to is checked from the outside, by the very suite the gate runs —
`script/gate --self-test` stands up a missing binary, one that fails
`--version`, and one that passes `--version` and then refuses to spawn anything,
and asserts that each still hands back both the command's exit code (0 and 7)
and what it printed. `tests/cli_gate.rs` runs it under `cargo test`, and
`script/verify` runs it again against the release binary it just built.

The real gate was put through the same three, end to end: with the binary
missing it printed `gate: not recording (no usable siftr)` and exited 0 on a
green tree and **1 on a tree with a broken test**; with a siftr that refuses to
spawn it printed `gate: siftr did not run the command (125); running it
unwrapped` and still reported `tests: ok`.

## 3. Cost: 12 ms against a 33-second suite

Wall clock could not measure it. Three alternating pairs on a machine running
another agent's `script/verify` (load average 41) gave bare 230s, 159s, 85s
against wrapped 135s, 85s, 87s — the wrapped run was *faster* twice, because the
suite's own spread under load is a hundred seconds wide and any wrapping cost is
lost inside it. Two more pairs on a quiet machine, charged in CPU rather than
wall clock, said the same thing: 27.8s and 30.9s of CPU bare against 25.6s and
27.1s wrapped.

So the cost was measured directly instead, as the work siftr does rather than
the difference between two noisy totals: ingesting one run's entire 713-line
capture — normalize, aggregate, compare against the baseline, store — takes
**12 ms** (five runs, 11–12 ms after the first). Against a 33-second suite that
is 0.04%, and the suite's own run-to-run spread on an idle machine (32.7s to
43.8s in adjacent runs) is a thousand times larger.

Nothing is run twice. The gate wraps the `cargo test` the developer was already
going to run.

## 4. The first real corpus

Fourteen runs of `cargo test --no-fail-fast` in the default data dir, recorded by
`script/gate` across the work that produced this document. Each run is 713–736
lines and 427 behaviors — one per `test <name> ... ok` line, plus cargo's own
progress lines and the output of siftr's integration tests, which capture
siftr's own reports and so end up templated as behaviors of their own.

**Eleven of the fourteen runs reported nothing at all.** Two of the three that did
were the two where the suite changed: one test added and one renamed (5
signals), and one test deliberately broken (18). The third is §5's last finding,
and is the only signal in the corpus raised by a run in which the suite did
nothing different. A separate three-run probe taken during the cost measurement,
while another agent's build had the machine at load average 41 and the same suite
took 85s to 230s, likewise reported `0 changes` on all three: the resource
evidence records the CPU and the context switches moving, and no rule reads them.

```
  kind         raised  judged   examined   resolved   unexamined  open  recurred
  NEW              18      18     2  0.11        16    16  1.0      1         1
  DISAPPEARED       1       1     1  1.0          0     0           1         0
  FREQUENCY         4       4     1  0.3          2     2  1.0      2         0
  LATENCY           1       1     1  1.0          0     0           1         0
  all              24      24     5  0.21        18    18  1.0      5         1
4 dismissed
```

Small as it is, this is the first corpus on this machine in which a person read
each signal and said what they thought of it. `dismiss` had never been written
once before it; five dismissals were issued here, and §5 explains why the
scorecard counts four.

## 5. What siftr got wrong about its own suite

**Pluralization splits one behavior into two, and moving a count between them
reads as two changes.** Adding a second test to a one-test binary turned
`running 1 test` into `running 2 tests`, and the normalizer masks the integer
but not the `s`. So one added test produced `running <int> tests  count 48 → 49`
and `running <int> test  count 9 → 8` — two FREQUENCY signals **ranked above**
the three true ones (two NEW, one DISAPPEARED) that said what actually happened.
Both were dismissed. This is not specific to cargo: any tool that pluralizes a
counted noun has the same pair of templates.

**A red suite is eighteen ungrouped NEW signals and no ERROR at all.** Breaking
one test produced 18 changes in 18 groups: `test … FAILED`, the panic line, the
`failures:` header, cargo's `error: test failed, to rerun pass …`, `error: <int>
target failed:`, the `test result: FAILED` line — *and one signal per line of
the assertion message*, because the failure printed the five lines the self-test
had produced. Not one of the eighteen was ERROR kind. The generic interpreter
has no notion of a Rust test or its outcome, so nothing is `test.example`,
nothing carries an outcome for `rules::error` to read, and nothing has a scope to
group under. The RSpec path got exactly these three things — a kind, an outcome,
and grouping by the file examples share; the path a `cargo test` takes has none
of them, and a failing suite is the case where a flood costs most.

**`dismiss` cannot silence a signal that has recurred.** s4 (NEW, a test added in
r5) was dismissed after r11 and was still reminded in r12, while s3 — dismissed
in the same breath — went quiet. `still_open` suppresses a group when
`Outcome::dismissed()` is true, and that reads the feedback ledger between the
signal's own run and the run it *resolved* in. s4 resolved in r7 (the broken run,
where its `… ok` line was absent for the obvious reason) and recurred in r8, so
its window had closed and a dismissal written four runs later fell outside it.
The row is stored, `history --signals` does not print it, and the scorecard counts
3 dismissed of the 4 issued. A signal that keeps coming back is precisely the one
a developer most wants to silence.

**Nothing else can silence one either.** `ack` does not suppress a reminder — it
means "being acted on", which is not the same claim — so `dismiss` is the only
lever, and it is the one that fails above. Meanwhile a NEW signal for a test
someone added on purpose is reminded on every run until it ages out of the
10-run window: s3 and s4 nagged through six consecutive green runs, each time
repeating `in none of 3 baseline runs`, which was true at r5 and had not been
true for six runs.

**A build tool's own compile time is judged as latency, and every edit trips
it.** The one signal raised by a run whose suite did nothing different was
LATENCY on `Finished \`test\` profile [unoptimized + debuginfo] target(s) in
<duration>`, 245ms → 1050ms: cargo's *build* step, which is slow exactly when
something was edited and fast when nothing was — the one quantity in a dev loop
guaranteed to move. It cleared the rule (3x the 245ms median, and over the 100ms
floor) while sitting 5% above a 1000ms value the baseline already held, because
the rule reads the median and the maximum but not the spread, and this baseline's
spread is 130ms to 1000ms, a factor of 7.7. Confidence came out 0.48, correctly
low, and it still headlined the run. The next run confirmed the diagnosis by
saying nothing: its build was a no-op, so the line was fast again.

None of these were tuned away. The first two are the normalizer and the
interpreter seeing a build tool for the first time; the last two are in
`src/signal/` and `cmd/history.rs` and belong to whoever holds those next.
