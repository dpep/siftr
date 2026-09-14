# siftr

Wrap your test command. siftr turns its output into behaviors: an example, a request, an SQL statement, a log message. It compares them with the last few runs of the same command and tells you what changed, why it matters, and which raw lines prove it. It isn't log search. You never grep. You get one line saying `UsersController#show` went from 3 queries to 10, with the evidence one command away.

Local only: a Rust binary and a SQLite file. Built for RSpec + Rails first.

## 30 seconds

Three normal runs of a Rails suite, then one where `UsersController#show` drops its `includes(:comments)`:

```
$ siftr run -q -- bundle exec rspec      # three times
$ SIFTR_DEMO_N_PLUS_ONE=1 siftr run -- bundle exec rspec
.*........
…
10 examples, 0 failures, 1 pending

r4 vs 3 baseline runs (r1 r2 r3): 1 change
  s1   FREQUENCY   conf 0.80  GET UsersController#show 2xx  queries 3 → 10
       supporting: FREQUENCY Comment Load SELECT "comments".* FROM "comments"…  count 1 → 9 (0 → 8 in this example) · FREQUENCY ./spec/requests/users_spec.rb # Users shows a us…  queries 28 → 35 · DISAPPEARED Comment Load SELECT "comments".* FROM "comments"…  gone: 1 → 0, in all 3 baseline runs
       in: ./spec/requests/users_spec.rb # Users shows a user with posts and comments
       evidence: 10 lines
next: siftr explain s1
```

The suite passed, and the N+1 costs under a millisecond, so neither the exit code nor the timings would have caught it. RSpec's output (trimmed here) goes to stdout untouched. siftr's report goes to stderr.

## Install

No release or Homebrew formula yet. Build from source (Rust 1.98+):

```
git clone <this repo> && cd siftr
cargo build --release
cp target/release/siftr ~/.local/bin/    # or anywhere on PATH
```

## Usage

### `siftr run -- CMD…`

Runs `CMD`, passes its output through, records the run, and prints changes to stderr. Exits with `CMD`'s exit code.

- `-q` hides the command's output and keeps only siftr's report.
- `-j` prints the changes as JSON on stdout (implies `-q`).

A failing example, with `-q`:

```
$ SIFTR_DEMO_FAIL=1 siftr run -q -- bundle exec rspec; echo "exit=$?"
r6 vs 5 baseline runs (r1…r5): 1 change
  s5   ERROR       conf 0.86  ./spec/models/user_spec.rb # User requires an email  failed with RSpec::Expectations::ExpectationNotMetError; passed in 5 of 5 baseline runs
       evidence: 1 line
next: siftr explain s5
exit=1
```

#### Still open

Repeat a regression and the rolling baseline absorbs it: here one N+1 run was enough for the next to count 0 changes. siftr keeps reminding you anyway, until it's fixed or dismissed:

```
$ SIFTR_DEMO_N_PLUS_ONE=1 siftr run -q -- bundle exec rspec
r5 vs 4 baseline runs (r1…r4): 0 changes
  still open: s1 (r4) FREQUENCY GET UsersController#show 2xx  queries 3 → 10 (+3 supporting) · siftr explain s1
next: siftr explain s1
```

#### When a run is incomplete

A run that didn't run what its baseline runs did (a spec file failed to load, `--fail-fast` stopped it, a focus filter ran a subset) says so in its first line. It reports the reason as one INCOMPLETE change instead of everything it never got to:

```
$ siftr run -q -- bundle exec rspec      # with `raise SyntaxError` appended to a spec
r7 (incomplete: 1 error outside examples) vs 6 baseline runs (r1…r6): 1 change
  s6   INCOMPLETE  conf 0.88  SyntaxError: While loading ./spec/requests/users_spec.rb a `raise SyntaxError` o…  failed outside examples: 1 now, 0 in baseline runs
       evidence: 1 line
next: siftr explain s6
```

`siftr explain s6` prints the error itself (`SyntaxError: compile error`). The incomplete run never becomes a baseline, and later reports name what they skipped:

```
r8 vs 6 baseline runs (r1…r6; skipped r7: 1 error outside examples, 0 now): 0 changes
```

### `siftr changes [RUN]`

The same report for any run (default: the latest in this project). `--context NAME` picks the latest run of a context instead.

### `siftr explain <SIGNAL>`

A signal's numbers run by run, the rule that fired, and its evidence. For a failure, the evidence starts with the exception class and its whole message.

```
$ siftr explain s1
s1  FREQUENCY  conf 0.80  in r4, group 1 headline
behavior  f035378415  http.request  GET UsersController#show 2xx
change    queries 3 → 10
rule      identical in all 3 baseline runs, so any change counts; confidence (n+1)/(n+2) = 0.80
queries   r4 10  |  baseline r3 3  r2 3  r1 3
scope     872cda219e  ./spec/requests/users_spec.rb # Users shows a user with posts and comments
          r4 10  |  baseline r3 3  r2 3  r1 3
evidence  r4
          file:log/test.log:217 Completed 200 OK in 2ms (Views: 1.3ms | ActiveRecord: 0.1ms (10 queries, 0 cached) | GC: 0.0ms)
group     s2 FREQUENCY Comment Load SELECT "comments".* FROM "comments" WHERE "comm…  count 1 → 9 (0 → 8 in this example)
group     s3 FREQUENCY ./spec/requests/users_spec.rb # Users shows a user with post…  queries 28 → 35
group     s4 DISAPPEARED Comment Load SELECT "comments".* FROM "comments" WHERE "comm…  gone: 1 → 0, in all 3 baseline runs
next: siftr evidence f035378415 --run r4
```

### `siftr evidence <BEHAVIOR>`

The raw lines kept for a behavior, each tagged with its stream and line number, plus the path to the run's full capture. Takes a behavior id or a unique prefix of 4+ hex digits. `--run` picks the run (default: the latest where the behavior occurred), `-n` limits the lines (default 8).

```
$ siftr evidence 27d0 --run r4 -n 3
27d08b314a  db.query  Comment Load SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?
r4: 9 occurrences, 0 errors; 3 lines kept
  file:log/test.log:33   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 1]]
  file:log/test.log:207   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 1]]
  file:log/test.log:208   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 2]]
capture file:log/test.log: /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/scribe2-home/runs/r4/file-log_test.log
next: siftr summary r4
```

### `siftr summary [RUN]`

A run's top behaviors, by count (default) or `--by time`. `-n` limits rows (default 20).

```
$ siftr summary --by time -n 5
r4: 253 lines, 46 behaviors, bundle exec rspec
    COUNT ERRORS      P50      P95    TOTAL  BEHAVIOR
        1      0   56.1ms   56.1ms   56.1ms  332a3a42c0  test.summary  rspec
        1      0   56.1ms   56.1ms   56.1ms  21991bc714  log  Finished in <duration> (files took <duration> to load)
        1      0   25.3ms   25.3ms   25.3ms  224151e20c  test.example  ./spec/requests/users_spec.rb # Users lists users
        1      0   10.2ms   10.2ms   10.2ms  872cda219e  test.example  ./spec/requests/users_spec.rb # Users shows a user with posts and comments
        1      0     10ms     10ms     10ms  0b6cf2d542  http.request  GET UsersController#index 2xx
next: siftr evidence 332a3a42c0 --run r4
```

### `siftr history`

Runs recorded in this project, newest first. `--context NAME` filters, `-n` limits.

```
$ siftr history
runs in ~/src/siftr/dogfood/rails_demo
  r8  14s ago  exit 0        246 lines  0 changes   bundle exec rspec
  r7  23s ago  exit 1        254 lines  1 change    bundle exec rspec
  r6  40s ago  exit 1        257 lines  1 change    bundle exec rspec
  r5  50s ago  exit 0        253 lines  0 changes   bundle exec rspec
  r4   1m ago  exit 0        253 lines  1 change    bundle exec rspec
  r3   1m ago  exit 0        246 lines  0 changes   bundle exec rspec
  r2   1m ago  exit 0        246 lines  0 changes   bundle exec rspec
  r1   1m ago  exit 0        246 lines  0 changes   bundle exec rspec
next: siftr changes r8
```

`--signals` lists those runs' signals instead, with what became of each: open, resolved, or recurred. Each is judged against its own original baseline, so a regression the rolling baseline has absorbed still reads as open. "After investigation" means someone ran `explain`, `evidence` or `ack` on it first. The N+1's signals from the runs above (trimmed to r4's):

```
$ siftr history --signals
  s1   r4   FREQUENCY   GET UsersController#show 2xx  resolved in r6 after investigation
  s2   r4   FREQUENCY   Comment Load SELECT "comments".* FROM "comments" WHERE "comm…  resolved in r6 after investigation
  s3   r4   FREQUENCY   ./spec/requests/users_spec.rb # Users shows a user with post…  resolved in r6 without investigation
  s4   r4   DISAPPEARED Comment Load SELECT "comments".* FROM "comments" WHERE "comm…  resolved in r6 without investigation
next: siftr history
```

### `siftr ack <SIGNAL>` and `siftr dismiss <SIGNAL>`

`ack` marks a signal as being acted on; `dismiss` marks it as not worth acting on, which also stops its `still open:` reminder. `-m TEXT` says why.

```
$ siftr ack s5 -m 'restoring the email validation'
s5 acked: ERROR ./spec/models/user_spec.rb # User requires an email
note: restoring the email validation
next: siftr changes r6
```

### `siftr ingest [FILE]`

Records a file, or stdin, as a run's stdout, for output you already have. `--context NAME` groups comparable inputs (default `ingest`). `--dir DIR` replays a captured scenario: any of `stdout.txt`, `stderr.txt`, `rspec.ndjson`, `test.log`, `exit_code.txt`.

```
$ siftr ingest --context demo --dir fixtures/rails_demo/baseline      # and baseline_2
$ siftr ingest --context demo --dir fixtures/rails_demo/slow
r3 vs 2 baseline runs (r1 r2): 1 change
  s1   LATENCY     conf 0.57  ./spec/models/post_spec.rb # Post summarizes the body  5.37ms → 311ms
       evidence: 1 line
next: siftr explain s1
```

### Common flags and exit codes

- `-j` prints exactly one JSON document on stdout, on every command. Empty results are still that command's document (exit 1). Errors, argument errors included, are `{"error": {"code", "message"}}` (exit 2), where `code` is `usage`, `not_found` or `failed`.
- `--home DIR` or `SIFTR_HOME`: the data directory. Default `$XDG_DATA_HOME/siftr`, else `~/.local/share/siftr`.
- Every human report ends with a `next:` line: the command to drill down with.

| Command | Exit |
|---|---|
| `run` | the command's own code; 125 if siftr fails before starting it, 126 if it can't be executed, 127 if not found |
| `ingest` | 0 recorded, 2 error |
| `changes`, `explain`, `evidence`, `summary`, `history` | 0 results, 1 nothing found, 2 error |
| `status` | 0 healthy, 1 something needs attention, 2 error |
| `ack`, `dismiss`, `gc` | 0 done, 2 error |

An unknown id is an error, not an empty result:

```
$ siftr explain s99 -j; echo "exit=$?"
{
  "error": {
    "code": "not_found",
    "message": "no signal s99; siftr history --signals lists recent signals"
  }
}
exit=2
```

## Data and retention

The data directory holds `siftr.db` and each run's raw capture under `runs/<run>/`. The database also records how signals get used (shown, explained, acked, dismissed). Nothing leaves the machine.

siftr deletes old data as runs finish. Per command it keeps:

- stats for the last 100 runs (`SIFTR_KEEP_RUNS`);
- evidence, meaning kept lines and raw captures, for the last 20 (`SIFTR_KEEP_EVIDENCE`);
- nothing at all once the command hasn't run for 30 days (`SIFTR_KEEP_DAYS`).

Reading something that was pruned says so and names the setting. `siftr status` shows what's there and what retention will do:

```
$ siftr status
data      /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/scribe2-home
database  312 KB, schema 6
captures  261 KB for 8 runs
runs      8 runs of 1 command; oldest r1 1m ago, newest r8 15s ago
keep      stats of the last 100 runs of each command (default; set SIFTR_KEEP_RUNS)
          evidence, raw lines and captures, of the last 20 (default; set SIFTR_KEEP_EVIDENCE)
          nothing of a command not run for 30 days (default; set SIFTR_KEEP_DAYS)
    RUNS  STATS  EVIDENCE  CAPTURES  NEWEST    COMMAND
       8      8         8    261 KB  15s ago   bundle exec rspec
next: siftr history
```

`siftr gc` prunes everything past the limits now and reclaims the space. `siftr gc --dry-run` lists what it would remove.

## For coding agents

Run the suite through siftr, read the first line of the report, and drill down only when that line tells you to.

1. Run `siftr run -q -- bundle exec rspec`. The exit code is the suite's; the report is on stderr. Use the same command line every time: the baseline is keyed on the project and the command as typed, so `bundle exec rspec spec/models` is a different context.
2. Read the first line, then any indented lines under it:

| You see | It means | Do |
|---|---|---|
| `vs N baseline runs (…): 0 changes`, N ≥ 2, no `still open:` line | nothing moved | stop |
| `still open: sN (rM) …` | an earlier regression is still there | `siftr explain sN`; fix it, or `siftr dismiss sN -m why` if it's intended |
| `rN (incomplete: …)` | the suite didn't run what the baseline did; the INCOMPLETE change says why | fix that (usually a spec that fails to load) and rerun; nothing else in this run is a fair comparison |
| `rN: interrupted by signal …` | the run was killed and wasn't compared | rerun |
| `no earlier runs`, `no comparable earlier runs`, or `only ERROR can fire until there are 2 baseline runs` | too little baseline, which isn't the same as "nothing changed" | run the suite again |
| `N changes`, N ≥ 1 | something moved | `siftr explain <headline id>`, then the `evidence` command on its `next:` line |
| `changed before the first example` (or between, after) | the environment or suite hooks changed, not the code under test; not counted in `changes` | look only if you changed setup |

`skipped rN: …` inside the parentheses just says which recent runs were left out of the baseline, and why.

3. For structure, use `siftr changes -j` (or `siftr run -j -- …`). Stop when `run.complete` is true, `changes` is 0, `open_signals` is empty and `baseline_runs` has 2 or more ids. Otherwise drill into, in this order: the `incomplete` signal, then `open_signals`, then the `headline` of each group.
4. With `-j`, exit 2 means `error.code` tells you what went wrong: `usage` (fix the arguments), `not_found` (the run, signal, behavior or context doesn't exist) or `failed`. Exit 1 means nothing was found, and you still get the command's normal document, empty (`run: null`; `[]` for `history`).
5. Once you act, record it: `siftr ack <signal> -m '…'` when you're fixing it, `siftr dismiss <signal> -m '…'` when it's intended. `siftr history --signals` shows what became of each.

The `-j` fields that matter (full schema: top of [`crates/siftr-cli/src/output.rs`](crates/siftr-cli/src/output.rs)):

- `run.complete`: false when the run was unfinished, interrupted, or INCOMPLETE.
- `changes`: number of code-level groups. `baseline_runs`: the run ids compared against. `skipped_runs[]`: {`run`, `reason`}, where `reason` is `no_test_summary`, `errors_outside_examples`, `stopped` or `subset`.
- `groups[]`: `rank` (1 is most important), `headline` (a signal id), `signals` (ids in the group), `setup` (true when the change happened outside every example: the environment or suite hooks, not the code).
- `signals[]`, in rank order:
  - `kind`: `error`, `new`, `disappeared`, `frequency`, `latency` or `incomplete`.
  - `measure`: `count`, `queries`, `duration_ms`, `failed`, `examples` or `errors_outside_of_examples`.
  - `current`, compared with `baseline` {`runs`, `present_in`, `median`, `min`, `max`, `failures`}.
  - `confidence` in [0, 1): how likely it is that this isn't noise. It grows with the number of baseline runs.
  - `tier`: 1 error through 5 outside examples.
  - `behavior` {`id`, `kind`, `template`}: what changed. Pass `id` to `evidence`.
  - `attribution.scope`: the test example it happened in, or null outside examples. `attribution.phase`: `setup`, `example`, `between` or `teardown` (before the first example, in one, between two, after the last). `attribution.setup` is true only for `setup`.
  - `exception`: the exception class, for `error`.
- `open_signals[]`: signals from earlier runs that are still open and weren't raised again, in the same shape as `signals[]`.

Trimmed with `jq '{run: {id: .run.id, complete: .run.complete}, changes, baseline_runs, skipped_runs, groups, signals: [.signals[0]], open_signals}'`, output unedited:

```json
{
  "run": {
    "id": "r4",
    "complete": true
  },
  "changes": 1,
  "baseline_runs": [
    "r3",
    "r2",
    "r1"
  ],
  "skipped_runs": [],
  "groups": [
    {
      "headline": "s1",
      "rank": 1,
      "setup": false,
      "signals": [
        "s1",
        "s2",
        "s3",
        "s4"
      ]
    }
  ],
  "signals": [
    {
      "attribution": {
        "baseline": 3.0,
        "current": 10.0,
        "phase": "example",
        "scope": {
          "id": "872cda219ee77788",
          "kind": "test.example",
          "template": "./spec/requests/users_spec.rb # Users shows a user with posts and comments"
        },
        "setup": false
      },
      "baseline": {
        "failures": null,
        "max": 3.0,
        "median": 3.0,
        "min": 3.0,
        "present_in": 3,
        "runs": 3
      },
      "behavior": {
        "id": "f0353784155976cf",
        "kind": "http.request",
        "template": "GET UsersController#show 2xx"
      },
      "confidence": 0.8,
      "current": 10.0,
      "evidence_lines": 1,
      "exception": null,
      "group": 1,
      "headline": true,
      "id": "s1",
      "kind": "frequency",
      "measure": "queries",
      "run": "r4",
      "tier": 2
    }
  ],
  "open_signals": []
}
```

## What it captures (RSpec + Rails)

`siftr run -- bundle exec rspec` reads three channels:

- **Per-example results** from an RSpec reporter listener, added by appending `--require` to `SPEC_OPTS`. It isn't a formatter, so your `.rspec` formatters and any `SPEC_OPTS` you've set keep working.
- **SQL and request lines** from the bytes the run appended to `log/test.log`. Each line is attributed to the example that was running when it was written, or to before, between or after examples. One log rotation during a run is handled exactly. With two or more, bytes are lost.
- **stdout and stderr**. When siftr's stdout is a terminal, the child gets a PTY, so RSpec's colours survive. stderr stays a separate pipe, because deprecation warnings land there.

Why it works this way: [docs/findings/capture.md](docs/findings/capture.md).

Signals that exist today:

| Kind | Fires when |
|---|---|
| ERROR | an example fails that passed in baseline runs. It stays quiet if a baseline run failed with the same exception, since that's known flaky |
| NEW / DISAPPEARED | a behavior is present now and absent from every baseline run, or the reverse. Behaviors that come and go in the baseline never fire |
| FREQUENCY | a count moved: SQL statements by template, queries per request, queries per example |
| LATENCY | one example got at least 100ms **and** 4x slower than its baseline median, and it isn't just a machine-wide stall |
| INCOMPLETE | the run didn't run what its baseline runs did: a spec file failed to load, it stopped early, or it ran fewer examples |

Related signals collapse into one group per example, and the top 3 groups are shown. The baseline is the last runs (up to 10) of the same project and command that ran the whole suite. Runs that were interrupted or killed, failed to load a spec file, stopped early or ran a subset are recorded for evidence but never used as a baseline.

siftr needs **2 earlier runs** of a command before NEW, DISAPPEARED, FREQUENCY or LATENCY can fire. With 1, only ERROR can.

Deliberately not built yet, because measured noise says they'd mostly cry wolf: suite-duration LATENCY, per-query latency, distribution drift, and treating setup-only changes (for example a cold database) as regressions. Details and the backtest behind the thresholds: [docs/findings/signals.md](docs/findings/signals.md).

## Limitations

- **RSpec + Rails is the tested path.** Other commands get generic templated lines and their counts. parallel_tests, spring, `rake spec` and RSpec older than 3.13 are untested.
- **LATENCY is blunt on purpose.** It catches a single example slowing down by at least 100ms and 4x. It misses 30ms → 90ms, and it misses +50% on a 1-second test.
- **Context is the project plus the command line.** The project is the nearest directory with a manifest (`Gemfile`, `Cargo.toml`, `package.json`, …), looking no higher than the git root; with none, it's the working directory. Two apps with their own Gemfiles in one repo get separate baselines, but in a repository with no manifest every directory is its own project. Focus filters in code (`fit`, `focus: true`) don't change the command line, so a focused run is compared with full runs: it reports INCOMPLETE and never becomes a baseline.
- **Local only.** One SQLite file per data directory, no sharing between machines.

## Development

Design principles, the domain model and the crate layout are in [CLAUDE.md](CLAUDE.md). Before committing:

```
cargo test --workspace --no-fail-fast
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
