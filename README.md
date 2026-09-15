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
       evidence: 10 lines, and the baseline runs for what disappeared
next: siftr explain s1
```

The suite passed, and the N+1 costs under a millisecond, so neither the exit code nor the timings would have caught it. RSpec's output (trimmed here) goes to stdout untouched. siftr's report goes to stderr.

## Install

From crates.io (Rust 1.98+):

```
cargo install siftr
```

Or from a clone: `cargo install --path .`

## Usage

### Without a subcommand

```
siftr -- CMD…     same as siftr run -- CMD…
siftr FILE        same as siftr ingest FILE, compared only with earlier ingests of that file
siftr -           ingest stdin; so does a bare siftr when stdin is piped or redirected
```

A subcommand or a preset always wins over a file of the same name: `siftr status` is the command, `siftr ./status` the file. A dated or rotated file compares with nothing until you name its context: `siftr app-0915.log --context app`. Any other word is an error, never a file name:

```
$ siftr statu; echo "exit=$?"
siftr: error: 'statu' is not a command, preset or existing file; did you mean 'status'? commands: run, ingest, changes, summary, evidence, explain, ack, dismiss, history, status, gc; presets: cron
exit=2
```

### `siftr run -- CMD…`

Runs `CMD`, passes its output through, records the run, and prints changes to stderr. Exits with `CMD`'s exit code.

- `-q` hides the command's output and keeps only siftr's report.
- `-j` prints the changes as JSON on stdout (implies `-q`).

siftr stays out of the command's way. `siftr run -- … | head` stops the command just as it would unwrapped, and the truncated run never becomes a baseline. If the data directory is busy (another siftr holding it) or unusable, the command runs anyway, unrecorded, with one warning. Under `-j` you still get a document, with `run: null` and `not_recorded: {code, message}`.

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

Repeat a regression and the rolling baseline absorbs it: here one N+1 run was enough for the next to find nothing new. siftr keeps reminding you anyway, until it's fixed or dismissed:

```
$ SIFTR_DEMO_N_PLUS_ONE=1 siftr run -q -- bundle exec rspec
r5 vs 4 baseline runs (r1…r4): no new changes · 1 still open
  still open: s1 (r4) FREQUENCY GET UsersController#show 2xx  queries 3 → 10 (+3 supporting) · siftr explain s1
next: siftr explain s1
```

A disappearance is never reminded. A query or spec you removed on purpose isn't a regression left in place.

#### When a run is incomplete

A run that skipped examples its baseline ran (a spec file failed to load, `--fail-fast` stopped it, a focus filter) says so in its first line. It reports the reason as one INCOMPLETE change instead of everything it never got to:

```
$ siftr run -q -- bundle exec rspec      # with an unclosed `RSpec.describe "broken" do` appended to a spec
r7 (incomplete: 1 error outside examples) vs 6 baseline runs (r1…r6): 1 change
  s6   INCOMPLETE  conf 0.88  ./spec/requests/users_spec.rb failed to load: SyntaxError: unexpected end-of-inp…  failed outside examples: 1 now, 0 in baseline runs
       evidence: 1 line
next: siftr explain s6
```

`siftr explain s6` shows Ruby's whole message (trimmed here):

```
$ siftr explain s6
s6  INCOMPLETE  conf 0.88  in r7, group 1 headline
behavior  db25427afc  exception  ./spec/requests/users_spec.rb failed to load: SyntaxError: unexpected end-of-input, assuming it is closing the parent top level context
…
evidence  r7
          SyntaxError: ~/src/siftr/dogfood/rails_demo/spec/requests/users_spec.rb:26: syntax errors found
            24 | end
            25 | raise SyntaxError, "compile error" if ENV["SIFTR_DEMO_RAISE_AFTER"]
          > 26 | RSpec.describe "broken" do
               |                           ^ expected a block beginning with `do` to end with `end`
…
```

Runs are only compared with runs that ran what they ran. A recent run that skipped examples this run ran is left out of the baseline, and the header names it and why. A suite that grows or shrinks is compared normally: deleting a spec file is a change, not a partial run.

```
r8 vs 6 baseline runs (r1…r6; skipped r7: 1 error outside examples, 0 now): 0 changes
```

#### A new error outside examples

An error raised outside every example (a `raise` after a `describe` block, a suite hook that raises) is a change even when every example ran. RSpec exits 1 on it:

```
$ SIFTR_DEMO_RAISE_AFTER=1 siftr run -q -- bundle exec rspec
r9 vs 7 baseline runs (r1…r6 r8; skipped r7: ran 8 examples, 10 now): 1 change
  s7   NEW         conf 0.89  ./spec/requests/users_spec.rb failed to load: SyntaxError: compile error  new: 1 now, in none of 7 baseline runs
       evidence: 1 line
next: siftr explain s7
```

#### A deleted spec file

Its examples collapse into one change, which never outranks a real regression:

```
$ siftr ingest --context hunt --dir fixtures/rspec_hunt/a4_warn3
r19 vs 3 baseline runs (r16 r17 r18): 2 changes
  s10  FREQUENCY   conf 0.80  DEPRECATION: old api  count 1 → 3
       evidence: 3 lines
  s11  DISAPPEARED conf 0.80  16 examples of ./spec/b_spec.rb  gone, in all 3 baseline runs
       evidence: in the baseline runs, not this one
next: siftr explain s10
```

### `siftr changes [RUN]`

The same report for any run (default: the latest in this project). `--context NAME` picks the latest run of a context instead.

### `siftr explain <SIGNAL>`

A signal's numbers run by run, the rule that fired, and its evidence. For a failure, the evidence starts with the exception class and its whole message. For a disappearance, the evidence comes from the latest baseline run that had it.

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
          file:log/test.log:217 Completed 200 OK in 2ms (Views: 1.5ms | ActiveRecord: 0.2ms (10 queries, 0 cached) | GC: 0.0ms)
group     s2 FREQUENCY Comment Load SELECT "comments".* FROM "comments" WHERE "comm…  count 1 → 9 (0 → 8 in this example)
group     s3 FREQUENCY ./spec/requests/users_spec.rb # Users shows a user with post…  queries 28 → 35
group     s4 DISAPPEARED Comment Load SELECT "comments".* FROM "comments" WHERE "comm…  gone: 1 → 0, in all 3 baseline runs
next: siftr evidence f035378415 --run r4
```

If retention has pruned the run's lines, `explain` still shows the numbers and says so: `evidence  r22 pruned (SIFTR_KEEP_EVIDENCE)`.

### `siftr evidence <BEHAVIOR>`

The raw lines kept for a behavior, each tagged with its stream and line number, plus the path to the run's full capture. Takes a behavior id or a unique prefix of 4+ hex digits. `--run` picks the run (default: the latest where the behavior occurred), `-n` limits the lines (default 8).

```
$ siftr evidence 27d0 --run r4 -n 3
27d08b314a  db.query  Comment Load SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?
r4: 9 occurrences, 0 errors; 3 lines kept
  file:log/test.log:33   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 1]]
  file:log/test.log:207   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 1]]
  file:log/test.log:208   Comment Load (0.0ms)  SELECT "comments".* FROM "comments" WHERE "comments"."post_id" = ?  [["post_id", 2]]
capture file:log/test.log: /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/scribe3-home/runs/r4/file-log_test.log
next: siftr summary r4
```

### `siftr summary [RUN]`

A run's top behaviors, by count (default) or `--by time`. `-n` limits rows (default 20).

```
$ siftr summary --by time -n 5
r4: 253 lines, 46 behaviors, bundle exec rspec
    COUNT ERRORS      P50      P95    TOTAL  BEHAVIOR
        1      0   56.2ms   56.2ms   56.2ms  21991bc714  log  Finished in <duration> (files took <duration> to load)
        1      0   56.2ms   56.2ms   56.2ms  332a3a42c0  test.summary  rspec
        1      0   25.5ms   25.5ms   25.5ms  224151e20c  test.example  ./spec/requests/users_spec.rb # Users lists users
        1      0   10.9ms   10.9ms   10.9ms  872cda219e  test.example  ./spec/requests/users_spec.rb # Users shows a user with posts and comments
        1      0     10ms     10ms     10ms  0b6cf2d542  http.request  GET UsersController#index 2xx
next: siftr evidence 21991bc714 --run r4
```

### `siftr history`

Runs recorded in this project, newest first. `--context NAME` filters (a context with no runs is an error, exit 2), `-n` limits. `incomplete` marks a run that skipped examples its baseline ran, so its changes don't mean what a whole run's do.

```
$ siftr history
runs in ~/src/siftr/dogfood/rails_demo
  r11   0s ago  exit 0        246 lines  0 changes   bundle exec rspec
  r10   1s ago  exit 1        254 lines  1 change    bundle exec rspec
  r9   2s ago  exit 1        254 lines  1 change    bundle exec rspec
  r8   3s ago  exit 0        246 lines  0 changes   bundle exec rspec
  r7   5s ago  exit 1        106 lines  1 change    incomplete  bundle exec rspec
  r6  34s ago  exit 1        257 lines  1 change    bundle exec rspec
  r5  35s ago  exit 0        253 lines  0 changes   bundle exec rspec
  r4  36s ago  exit 0        253 lines  1 change    bundle exec rspec
  r3  37s ago  exit 0        246 lines  0 changes   bundle exec rspec
  r2  38s ago  exit 0        246 lines  0 changes   bundle exec rspec
  r1  39s ago  exit 0        246 lines  0 changes   bundle exec rspec
next: siftr changes r11
```

`--signals` lists those runs' signals instead, with what became of each: open, resolved, or recurred (or unknown, when retention pruned what the judgement needs). Each is judged against its own original baseline, so a regression the rolling baseline has absorbed still reads as open. A run that skipped examples can't resolve anything. "After investigation" means someone ran `explain`, `evidence` or `ack` on it first. The signals from the runs above:

```
$ siftr history --signals
  s8   r10  NEW         An error occurred in an `after(:suite)` hook: RuntimeError: …  resolved in r11 without investigation
  s7   r9   NEW         ./spec/requests/users_spec.rb failed to load: SyntaxError: c…  resolved in r10 after investigation
  s6   r7   INCOMPLETE  ./spec/requests/users_spec.rb failed to load: SyntaxError: u…  resolved in r8 after investigation
  s5   r6   ERROR       ./spec/models/user_spec.rb # User requires an email  resolved in r8 after investigation
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
r15 vs 2 baseline runs (r13 r14): 1 change
  s9   LATENCY     conf 0.57  ./spec/models/post_spec.rb # Post summarizes the body  5.37ms → 311ms
       evidence: 1 line
next: siftr explain s9
```

### `siftr cron`

What runs on a schedule here, where cron's output goes, and the line that records each crontab job through `siftr --`. It reads your crontab, `/etc/crontab`, `/etc/cron.d`, launchd user agents on a calendar or interval, the mail spool, `/var/log/cron`, syslog and the macOS unified log. It edits nothing, runs no job and records nothing. With a synthetic crontab and agent:

```
$ siftr cron
scheduled jobs
  crontab -l               2 jobs
    0 3 * * *  /usr/local/bin/backup.sh --full
      record it: 0 3 * * * /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/readme-cron/bin/siftr -- /usr/local/bin/backup.sh --full
    */15 * * * *  cd ~/notes && git pull -q
      record it: */15 * * * * /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/readme-cron/bin/siftr -- sh -c 'cd ~/notes && git pull -q'
  /etc/crontab             absent
  /etc/cron.d              absent
  ~/Library/LaunchAgents   1 job
    StartCalendarInterval  com.example.sync
where cron's output goes
  /var/mail/me             absent
  /var/log/cron            absent
  /var/log/syslog          absent
  unified log, last 7d     no lines from cron
note: a wrapped job's report goes to stderr, so cron mails it after every run
next: crontab -e, and replace a job with its record-it line
```

A job that needs a shell (`cd`, `&&`, `~`, `%`, redirections) keeps it through `sh -c`. siftr names itself by absolute path because cron's `PATH` is minimal. A launchd agent gets no line: only its plist could change, and siftr doesn't touch it.

### Common flags and exit codes

- `-j` prints exactly one JSON document on stdout, on every command. Empty results are still that command's document (exit 1). Errors, argument errors included, are `{"error": {"code", "message"}}` (exit 2), where `code` is `usage`, `not_found`, `busy` (another siftr held the data directory too long; retry) or `failed`.
- `--home DIR` or `SIFTR_HOME`: the data directory. Default `$XDG_DATA_HOME/siftr`, else `~/.local/share/siftr`.
- Every human report ends with a `next:` line: the command to drill down with.

| Command | Exit |
|---|---|
| `run` | the command's own code; 125 if siftr fails before starting it, 126 if it can't be executed, 127 if not found |
| `ingest` | 0 recorded, 2 error |
| `cron` | 0 found a job or cron output, 1 found neither, 2 error |
| `changes`, `explain`, `evidence`, `summary`, `history` | 0 results, 1 nothing found, 2 error |
| `status` | 0 healthy, 1 something needs attention, 2 error |
| `ack`, `dismiss` | 0 recorded, 2 error |
| `gc` | 0 done, 2 error |

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

Nothing siftr stores holds a credential it recognizes: tokens (GitHub, GitLab, AWS, Google, Slack, Stripe, npm, SendGrid, OpenAI), JWTs, private keys, `Authorization` values, URL passwords, cookie values, and high-entropy values under keys like `password`, `api_key` or `access_token`, including Rails SQL binds. They are masked line by line before anything is written, as `<TOKEN_1>`, `<SECRET_2>` (the same number for the same value within a run). What the command prints to your terminal is untouched. Two settings choose what else is kept:

- `SIFTR_REDACT=secrets` (default); `pii` also masks emails, public IPs and home directories in raw captures and kept lines; `off` keeps raw captures as the command wrote them. Kept lines and templates mask credentials under every setting, so behavior ids never depend on it.
- `SIFTR_CAPTURE=off` writes no raw capture. `explain` and `evidence` then show the kept lines (first 1024 bytes) instead of a failure's whole message.

siftr deletes old data as runs finish. Per command it keeps:

- stats for the last 100 runs (`SIFTR_KEEP_RUNS`);
- evidence, meaning kept lines and raw captures, for the last 20 (`SIFTR_KEEP_EVIDENCE`);
- nothing at all once the command hasn't run for 30 days (`SIFTR_KEEP_DAYS`).

Past those limits it still keeps a run that is recording, and whatever the latest run's report reads: its baseline runs, and the evidence a `still open:` reminder points `explain` to. Reading something that was pruned says so and names the setting. `siftr status` shows what's there and what retention will do:

```
$ siftr status
data      /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/scribe3-home
database  388 KB, schema 8
captures  337 KB for 11 runs
runs      11 runs of 1 command; oldest r1 39s ago, newest r11 0s ago
keep      stats of the last 100 runs of each command (default; set SIFTR_KEEP_RUNS)
          evidence, raw lines and captures, of the last 20 (default; set SIFTR_KEEP_EVIDENCE)
          nothing of a command not run for 30 days (default; set SIFTR_KEEP_DAYS)
    RUNS  STATS  EVIDENCE  CAPTURES  NEWEST    COMMAND
      11     11        11    337 KB  0s ago    bundle exec rspec
next: siftr history
```

`siftr gc` prunes everything past the limits now, and vacuums the database once 25% of it is free. `siftr gc --dry-run` lists what it would remove.

## For coding agents

Run the suite through siftr, read the first line of the report, and drill down only when that line tells you to.

1. Run `siftr run -q -- bundle exec rspec`. The exit code is the suite's; the report is on stderr. Use the same command line every time: the baseline is keyed on the project and the command as typed, so `bundle exec rspec spec/models` is a different context.
2. Read the first line and decide, in this order:
   - `rN (incomplete: …)`: the suite didn't fully run. Fix what the INCOMPLETE change names (usually `<file> failed to load: <Ruby error>`) and rerun. Nothing else in this run is a fair comparison.
   - `: 0 changes`, with 2 or more baseline runs and no `still open:` line: nothing moved. Stop.
   - Anything else: read group 1's headline (the first indented line) and any `still open:` lines. Run `siftr explain <id>` only if that line doesn't already tell you what to fix.

| You see | It means | Do |
|---|---|---|
| `INCOMPLETE …`, e.g. `INCOMPLETE  <file> failed to load: …` | the run skipped examples: a spec file didn't load, `--fail-fast` stopped it, or a focus filter | fix that and rerun |
| `no new changes · N still open`, `still open: sN (rM) …` | an earlier regression is still there | `siftr explain sN`; fix it, or `siftr dismiss sN -m why` if it's intended |
| `ERROR  <example>  failed with …` | an example that passed in the baseline fails | fix it; `explain` has the whole message |
| `NEW  <file> failed to load: …` or `NEW  An error occurred in an after(:suite) hook: …` | a new error outside examples: every example ran, but the suite exits 1 | fix the error |
| `FREQUENCY`, `LATENCY`, or `NEW` / `DISAPPEARED` on a query or log line | a count, a duration, or a behavior's presence moved | `siftr explain <id>`, then the `evidence` command on its `next:` line |
| `DISAPPEARED  N examples of <file>  gone` | a spec file was deleted or renamed | nothing, if you meant it; it's never reminded |
| `changed before the first example` (or between, after) | the environment or suite hooks changed, not the code under test; not counted in `changes` | look only if you changed setup |
| `rN: interrupted by signal …` | the run was killed and wasn't compared | rerun |
| `no earlier runs`, `no comparable earlier runs`, or `only ERROR can fire until there are 2 baseline runs` | too little baseline, which isn't the same as "nothing changed" | run the suite again |

`skipped rN: …` inside the parentheses just says which recent runs were left out of the baseline, and why.

3. For structure, use `siftr changes -j` (or `siftr run -j -- …`). The same order applies: if `run.complete` is false, fix the `incomplete` signal first. Stop when `changes` is 0, `open_signals` is empty and `baseline_runs` has 2 or more ids. Otherwise read `groups[0].headline`, then `open_signals`. A non-null `not_recorded` means the run wasn't recorded; rerun for a report.
4. With `-j`, exit 2 means `error.code` tells you what went wrong: `usage` (fix the arguments), `not_found` (the run, signal, behavior or context doesn't exist), `busy` (retry) or `failed`. Exit 1 means nothing was found, and you still get the command's normal document, empty (`run: null`; `[]` for `history`).
5. Once you act, record it: `siftr ack <signal> -m '…'` when you're fixing it, `siftr dismiss <signal> -m '…'` when it's intended. `siftr history --signals` shows what became of each.

The `-j` fields that matter (full schema: top of [`src/bin/siftr/output.rs`](src/bin/siftr/output.rs)):

- `run.complete`: false when the run was unfinished, interrupted, or INCOMPLETE.
- `changes`: number of code-level groups. `baseline_runs`: the run ids compared against. `skipped_runs[]`: {`run`, `reason`}, where `reason` is `no_test_summary`, `errors_outside_examples`, `stopped` or `subset`.
- `groups[]`: `rank` (1 is most important), `headline` (a signal id), `signals` (ids in the group), `setup` (true when the change happened outside every example: the environment or suite hooks, not the code), `disappeared_examples` (null, or {`file`, `examples`} for a deleted spec file's examples collapsed into one group).
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
- `not_recorded` (`run -j` only): null, or {`code`, `message`} when the command ran but siftr couldn't record it.

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
      "disappeared_examples": null,
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

- **Per-example results** from an RSpec reporter listener, added by appending `--require` to `SPEC_OPTS`. It isn't a formatter, so your `.rspec` formatters and any `SPEC_OPTS` you've set keep working. It also sees errors outside examples, such as a spec file that fails to load or a hook that raises.
- **SQL and request lines** from the bytes the run appended to `log/test.log`. Each line is attributed to the example that was running when it was written, or to before, between or after examples. One log rotation during a run is handled exactly. With two or more, bytes are lost.
- **stdout and stderr**. When siftr's stdout is a terminal, the child gets a PTY, so RSpec's colours survive. stderr stays a separate pipe, because deprecation warnings land there.

Why it works this way: [docs/findings/capture.md](docs/findings/capture.md).

Signals that exist today:

| Kind | Fires when |
|---|---|
| ERROR | an example fails that passed in baseline runs. It stays quiet if a baseline run failed with the same exception, since that's known flaky |
| NEW / DISAPPEARED | a behavior is present now and absent from every baseline run, or the reverse. Behaviors that come and go in the baseline never fire. A new error outside examples is NEW and ranks with ERROR |
| FREQUENCY | a count moved: SQL statements by template, queries per request, queries per example |
| LATENCY | one example got at least 100ms **and** 4x slower than its baseline median, and it isn't just a machine-wide stall |
| INCOMPLETE | the run skipped examples its baseline ran: a spec file failed to load, `--fail-fast` stopped it, or a focus filter |

Related signals collapse into one group per example, and a deleted spec file's examples into one group. The top 3 groups are shown. The baseline is the last runs (up to 10) of the same project and command, minus runs that skipped examples the current run ran. Interrupted or killed runs are recorded for evidence but never used as a baseline.

siftr needs **2 earlier runs** of a command before NEW, DISAPPEARED, FREQUENCY or LATENCY can fire. With 1, only ERROR and INCOMPLETE can.

The thresholds come from measured noise and a backtest: [docs/findings/signals.md](docs/findings/signals.md).

## Limitations

- **Rich capture is RSpec + Rails only.** Other commands get generic templated lines and their counts. parallel_tests, spring, `rake spec` and RSpec older than 3.13 are untested.
- **LATENCY is blunt on purpose.** It catches a single example slowing down by at least 100ms and 4x. It misses 30ms → 90ms, and it misses +50% on a 1-second test.
- **Not every number is a signal.** Measured noise says these would mostly cry wolf, so they aren't built: suite-duration LATENCY, per-query latency (the `(0.1ms)` in a log line), distribution drift, and setup-only changes (a cold database) as regressions.
- **Context is the project plus the command line, as typed.** `bundle exec rspec spec/models/user_spec.rb:12` has its own baseline, separate from the full suite's. The project is the nearest directory with a manifest (`Gemfile`, `Cargo.toml`, `package.json`, …), looking no higher than the git root; with none, it's the working directory. Two apps with their own Gemfiles in one repo get separate baselines, but in a repository with no manifest every directory is its own project.
- **Focus filters in code don't change the command line.** A run with `fit` or `focus: true` is compared with full runs: it reports INCOMPLETE, and later full runs leave it out of their baseline.
- **Local only.** One SQLite file per data directory, no sharing between machines.

## Development

Design principles, the domain model and the source layout are in [CLAUDE.md](CLAUDE.md). Before committing:

```
cargo test --no-fail-fast
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

`script/verify [SHA]` runs that gate in a throwaway worktree, then drives the `dogfood/rails_demo` loop end to end.
