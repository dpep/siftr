# siftr

Wrap your test command. siftr turns its output into behaviors: an example, a request, an SQL statement, a log message. It compares them with the last few runs of the same command and tells you what changed, why it matters, and which raw lines prove it. It isn't log search. You never grep. You get one line saying `UsersController#show` went from 3 queries to 10, with the evidence one command away.

Local only: a Rust binary and a SQLite file. Built for RSpec + Rails first.

## 30 seconds

Three normal runs of a Rails suite, then one where `UsersController#show` drops its `includes(:comments)`:

```
$ siftr run -- bundle exec rspec      # three times
$ SIFTR_DEMO_N_PLUS_ONE=1 siftr run -- bundle exec rspec
.*........
...
10 examples, 0 failures, 1 pending

r4 vs 3 baseline runs (r1 r2 r3): 1 change
  s1   FREQUENCY   conf 0.8   GET UsersController#show 2xx  queries 3 → 10
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
$ SIFTR_DEMO_FAIL=1 siftr run -q -- bundle exec rspec
r5 vs 4 baseline runs (r1…r4): 1 change
  s5   ERROR       conf 0.83  ./spec/models/user_spec.rb # User requires an email  failed with RSpec::Expectations::ExpectationNotMetError; passed in 4 of 4 baseline runs
       evidence: 1 line
next: siftr explain s5
$ echo $?
1
```

### `siftr changes [RUN]`

The same report for any run (default: the latest in this project). `--context NAME` picks the latest run of a context instead.

### `siftr explain <SIGNAL>`

A signal's numbers run by run, the rule that fired, and its evidence:

```
$ siftr explain s1
s1  FREQUENCY  conf 0.8  in r4, group 1 headline
behavior  f035378415  http.request  GET UsersController#show 2xx
change    queries 3 → 10
rule      identical in all 3 baseline runs, so any change counts; confidence (n+1)/(n+2) = 0.8
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

The raw lines kept for a behavior, each tagged with its stream and line number, plus the path to the run's full capture. Takes a behavior id or a unique prefix of 4+ hex digits. `--run` picks the run, `-n` limits the lines (default 8).

```
$ siftr evidence 27d0 -n 3
27d08b314a  db.query  Comment Load SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?
r4: 9 occurrences, 0 errors; 3 lines kept
  file:log/test.log:33   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 1]]
  file:log/test.log:207   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 1]]
  file:log/test.log:208   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 2]]
capture file:log/test.log: /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/scribe-home/runs/r4/file-log_test.log.log
next: siftr summary r4
```

### `siftr summary [RUN]`

A run's top behaviors, by count (default) or `--by time`. `-n` limits rows (default 20).

```
$ siftr summary --by time -n 5
r4: 253 lines, 46 behaviors, bundle exec rspec
    COUNT ERRORS      P50      P95    TOTAL  BEHAVIOR
        1      0     53ms     53ms   55.7ms  21991bc714  log  Finished in <duration> (files took <duration> to load)
        1      0     53ms     53ms   55.6ms  332a3a42c0  test.summary  rspec
        1      0     26ms     26ms   26.3ms  224151e20c  test.example  ./spec/requests/users_spec.rb # Users lists users
        1      0    9.2ms    9.2ms   10.2ms  872cda219e  test.example  ./spec/requests/users_spec.rb # Users shows a user with posts and comments
        1      0    9.2ms    9.2ms     10ms  0b6cf2d542  http.request  GET UsersController#index 2xx
next: siftr evidence 21991bc714 --run r4
```

### `siftr history`

Runs recorded in this project, newest first. `--context NAME` filters, `-n` limits.

```
$ siftr history
runs in ~/src/siftr
  r4  10s ago  exit 0        253 lines  1 changes  bundle exec rspec
  r3  15s ago  exit 0        246 lines  0 changes  bundle exec rspec
  r2  22s ago  exit 0        246 lines  0 changes  bundle exec rspec
  r1  29s ago  exit 0        246 lines  0 changes  bundle exec rspec
next: siftr changes r4
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

- `-j` JSON on stdout, on every command. Errors go to stderr as `{"error": …}`.
- `--home DIR` or `SIFTR_HOME`: the data directory. Default `$XDG_DATA_HOME/siftr`, else `~/.local/share/siftr`. It holds `siftr.db` and each run's raw capture under `runs/<run>/`.
- Every human report ends with a `next:` line: the command to drill down with.

| Command | Exit |
|---|---|
| `run` | the command's own code; 125 if siftr fails before starting it, 126 if it can't be executed, 127 if not found |
| `ingest` | 0 recorded, 2 error |
| `changes`, `explain`, `evidence`, `summary`, `history` | 0 results, 1 nothing found, 2 error |

## For coding agents

1. Run the tests with `siftr run -q -- bundle exec rspec`. The exit code is the suite's. The report on stderr is short: zero or more change groups, each headed by one signal id.
2. `0 changes` against 2 or more baseline runs means nothing moved. Stop there. The first two runs say they have too little baseline, and that isn't the same as "nothing changed".
3. For structure, run `siftr changes -j`. For more detail on one signal, `siftr explain <signal>`. For the raw lines, `siftr evidence <behavior> --run <run>`. `explain` prints the exact `evidence` command on its `next:` line.
4. Use the same command line every time. The baseline is keyed on the command as typed (`bundle exec rspec spec/models` is a different context).

The `-j` fields that matter (full schema: top of [`crates/siftr-cli/src/output.rs`](crates/siftr-cli/src/output.rs)):

- `changes`: number of code-level groups. `baseline_runs`: the run ids compared against.
- `groups[]`: `rank` (1 is most important), `headline` (a signal id), `signals` (ids in the group), `setup` (true when the change happened before the first example, which usually means the environment changed, not the code).
- `signals[]`, in rank order:
  - `kind`: `error`, `new`, `disappeared`, `frequency` or `latency`.
  - `measure`: `count`, `queries`, `duration_ms` or `failed`.
  - `current`, compared with `baseline` {`runs`, `present_in`, `median`, `min`, `max`, `failures`}.
  - `confidence` in [0, 1): how likely it is that this isn't noise. It grows with the number of baseline runs.
  - `tier`: 1 error through 5 setup.
  - `behavior` {`id`, `kind`, `template`}: what changed. Pass `id` to `evidence`.
  - `attribution.scope`: the test example it happened in, when known.
  - `exception`: the exception class, for `error`.

Trimmed with `jq '{changes, baseline_runs, groups, signals: [.signals[0]]}'`, output unedited:

```json
{
  "changes": 1,
  "baseline_runs": [
    "r3",
    "r2",
    "r1"
  ],
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
  ]
}
```

## What it captures (RSpec + Rails)

`siftr run -- bundle exec rspec` reads three channels:

- **Per-example results** from an RSpec reporter listener, added by appending `--require` to `SPEC_OPTS`. It isn't a formatter, so your `.rspec` formatters and any `SPEC_OPTS` you've set keep working.
- **SQL and request lines** from the bytes the run appended to `log/test.log`. Each line is attributed to the example that was running when it was written. One log rotation during a run is handled exactly. With two or more, bytes are lost.
- **stdout and stderr**. When siftr's stdout is a terminal, the child gets a PTY, so RSpec's colours survive. stderr stays a separate pipe, because deprecation warnings land there.

Why it works this way: [docs/findings/capture.md](docs/findings/capture.md).

Signals that exist today:

| Kind | Fires when |
|---|---|
| ERROR | an example fails that passed in baseline runs. It stays quiet if a baseline run failed with the same exception, since that's known flaky |
| NEW / DISAPPEARED | a behavior is present now and absent from every baseline run, or the reverse. Behaviors that come and go in the baseline never fire |
| FREQUENCY | a count moved: SQL statements by template, queries per request, queries per example |
| LATENCY | one example got at least 100ms **and** 4x slower than its baseline median, and it isn't just a machine-wide stall |

Related signals collapse into one group per example, and the top 3 groups are shown. The baseline is the last runs (up to 10) of the same project and command.

siftr needs **2 earlier runs** of a command before NEW, DISAPPEARED, FREQUENCY or LATENCY can fire. With 1, only ERROR can. Interrupted (Ctrl-C) runs are recorded but never used as a baseline.

Deliberately not built yet, because measured noise says they'd mostly cry wolf: suite-duration LATENCY, per-query latency, distribution drift, and treating setup-only changes (for example a cold database) as regressions. Details and the backtest behind the thresholds: [docs/findings/signals.md](docs/findings/signals.md).

## Limitations

- **RSpec + Rails is the tested path.** Other commands get generic templated lines and their counts. parallel_tests, spring, `rake spec` and RSpec older than 3.13 are untested.
- **LATENCY is blunt on purpose.** It catches a single example slowing down by at least 100ms and 4x. It misses 30ms → 90ms, and it misses +50% on a 1-second test.
- **A regression stays in the baseline.** After one N+1 run, the baseline is no longer uniform, so a later repeat of the same regression can go unreported.
- **Context is the project root plus the command line.** The project root is the nearest `.git` ancestor, and your working directory isn't part of it. Two apps in one repo that both run `bundle exec rspec` share a baseline. Focus filters in code (`fit`, `focus: true`) don't change the command line, so a focused run is compared with full runs.
- **Local only.** One SQLite file per data directory, no sharing between machines.

## Development

Design principles, the domain model and the crate layout are in [CLAUDE.md](CLAUDE.md). Before committing:

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
