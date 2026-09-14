# Siftr

Local Rust CLI that turns a command's telemetry into **behaviors**, compares
them with previous runs of the same context, and surfaces **behavioral changes**
backed by concrete evidence. First workflow: `siftr run -- bundle exec rspec`,
consumed by a developer or a coding agent asking "what changed, why does it
matter, show me the evidence".

Not a cloud service, dashboard, config language, or log search tool.

## First principles

1. **Behavior is the unit, the log line is evidence.** Everything a user or
   agent reads is about behaviors; raw lines appear only as exemplars.
2. **Provenance or it doesn't ship.** signal → behavior → aggregate →
   observations → raw line. Any number printed must be traceable to the runs
   and lines that produced it.
3. **Precision over recall.** A signal the developer ignores is a cost. When in
   doubt, don't surface it; say how much baseline backs each claim.
4. **Evidence-derived confidence.** Severity/confidence come from effect size,
   baseline spread and the number of baseline runs — never a constant. Round
   where the value is built (2–3 significant figures), not at display.
5. **Streaming, bounded, fast.** One pass per line, no per-line regex cascade,
   no per-line `String` allocation on the hot path. Per-behavior state is
   bounded (sketches, capped exemplars, capped distinct-value sets).
   1M lines is a normal workload.
6. **Never break the wrapped command.** `siftr run` passes output through and
   exits with the child's exit code. An analysis/storage failure warns on
   stderr; it never changes the child's result.
7. **Grow the model from use.** Add a type, crate, or signal kind when a real
   run needs it — not for symmetry.

## Pipeline & domain model

```
Source ──Observation──▶ Interpreter ──Event──▶ Aggregator ──Aggregate──▶ Store
(process, file tail,     (rspec, rails,        (per Behavior,           (runs, behaviors,
 stdin)                   sql, generic)          bounded state)           aggregates, exemplars)
                                                                              │
                                     Baseline (last N runs of same Context) ◀─┘
                                                     │
                                                  Signals ──▶ explain / evidence
```

- **Observation** — one raw record (a line) with stream (`stdout`, `stderr`,
  `file:<path>`), sequence number and run-relative time.
- **Template** — an observation with incidental variation masked into typed
  slots (`<uuid>`, `<int>`, `<duration>`, `<path>`, …) plus the slot values.
- **Event** — an interpreted observation: semantic kind (`test.example`,
  `test.summary`, `db.query`, `http.request`, `exception`, `log`), template,
  and extracted measures (duration, status, outcome).
- **Behavior** — a recurring pattern identified by a **stable** id derived from
  (semantic kind, template). Stable across runs and machines: use a fixed hash
  (never `std` `DefaultHasher`/`RandomState`).
- **Aggregate** — a behavior's stats within one run: count, error count,
  duration sketch, per-slot cardinality/enum distribution, exemplar refs.
- **Context** — what makes baselines comparable: project root + normalized
  command. An rspec suite and a dev server are different contexts.
- **Baseline** — derived from recent runs of the same context: occurrence
  ratio, typical count, typical latency, run-to-run spread.
- **Signal** — a behavioral change worth attention: kind (NEW, DISAPPEARED,
  FREQUENCY, LATENCY, ERROR, …), magnitude, confidence, evidence refs.
- **Evidence** — exemplar raw lines kept per aggregate (bounded), plus the
  run's raw capture on disk.

## Layout

```
crates/
  siftr-normalize  per-line masker → template + typed slots; slot stats + identifier/enum classification. Zero deps.
  siftr-core    pure: domain types, interpret, aggregate, baseline, signal. No I/O.
  siftr-store   persistence behind store traits; SQLite (rusqlite, bundled) today.
  siftr-cli     the `siftr` binary: capture, commands, rendering.
dogfood/        real projects/scripts used to exercise siftr end to end
fixtures/       committed captured outputs used by tests (no private data)
docs/findings/  measurements and experiments that justified a design choice
```

Split a crate only at a real boundary (a heavy dependency, a separately
consumable surface). `siftr-normalize` (the per-line masker, zero runtime
deps, own benchmarks) is one: it is the hot path and a candidate to share with
iriq later.

## Reuse of sibling projects

- **iriq** (`~/code/lib/ruby/iriq/rust/iriq`, `default-features = false`): a
  dependency for URL/route shaping only, in the HTTP interpreter on request
  lines — never on the per-line hot path (regex-based, ~µs per URL). Its
  identifier-vs-enum slot rules (`cluster.rs` `is_enum`, `corpus.rs`
  `classify_segment`) are *ported* into `siftr-normalize`, not depended on:
  `PositionStats::observe` allocates per observation and its `SegmentType` is
  URL-specific. Its `SegmentClassifier` misfires on bare log words (`ms` →
  locale), so never apply it to them.
- **launder**, **pattern_engine**: not dependencies. launder's regex cascade
  measured ~10x the CPU of a single-pass byte scanner while masking less.

## CLI conventions

House CLI conventions apply (flag spine `-j/-J/-v/-q`, stdout is data,
logging to stderr, flag > env > XDG for paths).

- Data dir: `--home` > `SIFTR_HOME` > `$XDG_DATA_HOME/siftr` > `~/.local/share/siftr`.
  Every test sets `SIFTR_HOME` to a temp dir.
- `siftr run -- CMD…` exits with CMD's code; siftr's own failure before the
  child starts exits 125.
- Query commands: `0` results, `1` empty, `2` error.
- IDs are short and copyable; every command's human output ends with the next
  command to run for drill-down.

## Gate

Before every commit:

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

## Traps

- Rails logs carry ANSI color codes (`\e[1m\e[36m (0.3ms)\e[0m`); strip before
  templating or identical queries split into two behaviors.
- Private logs on this machine (anything outside this repository)
  are fine as local benchmark inputs but must never be committed, excerpted into
  fixtures, or quoted in docs.
- Parallel agents share one working tree: `git add`/commit only your own paths.
- `--format` in `SPEC_OPTS` *replaces* the user's `.rspec` formatters (last
  source wins; only `--require`/`-I` accumulate). Inject data with `--require`
  of a reporter *listener* (not a formatter — that also empties stdout),
  appended to any existing `SPEC_OPTS`. See `docs/findings/capture.md`.
- Rails writes SQL/request logs to `log/test.log`, not stdout; a run's slice is
  the bytes appended between start and end offsets. Deprecation warnings in the
  test env go to stderr, not the log. Rotation/truncation are detectable, not
  always recoverable — say so rather than silently mis-attributing.
- RSpec (and most tools) colour only when stdout is a TTY, so capturing through
  a pipe changes what the user sees. `siftr run` gives the child a PTY for
  stdout when siftr's own stdout is a terminal, keeps stderr a pipe (the
  stream split is signal), and strips ANSI before analysis.
- Test timing is noisy (the same demo example varied 9x across two baseline
  runs): latency signals need an absolute floor as well as a ratio.
