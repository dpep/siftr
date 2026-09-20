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
  s1   FREQUENCY   3 baseline runs  GET UsersController#show 2xx  queries 3 → 10
       supporting: FREQUENCY Comment Load SELECT "comments".* FROM "comments"…  count 1 → 9 (0 → 8 in this example) · FREQUENCY ./spec/requests/users_spec.rb # Users shows a us…  queries 28 → 35 · DISAPPEARED Comment Load SELECT "comments".* FROM "comments"…  gone: 1 → 0, in all 3 baseline runs
       in: ./spec/requests/users_spec.rb # Users shows a user with posts and comments
       evidence: 10 lines, and the baseline runs for what disappeared
next: siftr explain s1
```

The suite passed, and the N+1 costs under a millisecond, so neither the exit code nor the timings would have caught it. RSpec's output (trimmed here) goes to stdout untouched. siftr's report goes to stderr.

**A baseline is keyed on the project and the command as you typed it.** This is the one fact to take away before anything else: `bundle exec rspec` and `bundle exec rspec spec/models` are two different contexts with two separate baselines, and the second starts from nothing. Use the same command line every time, or siftr has nothing to compare against. (The project is the nearest directory with a manifest — `Gemfile`, `Cargo.toml`, `package.json` — and [Limitations](#limitations) has the corner cases.)

**Practising? Point siftr at a throwaway data directory.** Every run you make becomes baseline for the next one, so replaying the examples below a few times genuinely changes what siftr says about them — that is the tool working, not a bug, and it means a directory you have been experimenting in will not reproduce them. `siftr --home /tmp/siftr-practice run -- …` (or `SIFTR_HOME=/tmp/siftr-practice`) keeps practice runs out of your real history, and `rm -rf /tmp/siftr-practice` is a clean slate.

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
siftr: error: 'statu' is not a command, preset or existing file; did you mean 'status'? commands: run, ingest, changes, summary, evidence, explain, ack, dismiss, history, sources, status, gc; presets: cron
exit=2
```

### `siftr run -- CMD…`

Runs `CMD`, passes its output through, records the run, and prints changes to stderr. Exits with `CMD`'s exit code.

- `-q` hides the command's output and keeps only siftr's report.
- `-j` prints the changes as JSON on stdout (implies `-q`).
- `--quiet-unless-changed` prints siftr's report only when there's something to read: a change, or one still open. Otherwise siftr adds nothing, so a job wrapped for cron, CI or a git hook prints only what the command did. It can't be combined with `-j`, which always prints its document.
- `--no-report` prints nothing of siftr's own, ever — the run is still recorded, only silently. Warnings and errors still print. Use it when a wrapped command must look untouched to whoever runs it, not just quiet when nothing changed. It can't be combined with `-j` or `--quiet-unless-changed`.

siftr stays out of the command's way. `siftr run -- … | head` stops the command just as it would unwrapped, and the truncated run never becomes a baseline. If the data directory is busy (another siftr holding it) or unusable, the command runs anyway, unrecorded, with one warning. Under `-j` you still get a document, with `run: null` and `not_recorded: {code, message}`.

A failing example, with `-q`:

```
$ SIFTR_DEMO_FAIL=1 siftr run -q -- bundle exec rspec; echo "exit=$?"
r6 vs 5 baseline runs (r1…r5): 1 change
  s5   ERROR       5 baseline runs  ./spec/models/user_spec.rb # User requires an email  failed with RSpec::Expectations::ExpectationNotMetError; passed in 5 of 5 baseline runs
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

A reminder is reported on every run the change is actually present in, including one where you fixed it in between and broke it again. It is bounded by the baseline window, though: it lasts while the run that raised the change is still among the last 10 runs of the context. Leave the N+1 in place and keep running, and that run eventually ages out — after which the regression is simply what siftr has always seen here:

```
r14 vs 10 baseline runs (r4…r13): no new changes · 1 still open
r15 vs 10 baseline runs (r5…r14): 0 changes
```

Nothing was fixed between r14 and r15, and the N+1 is still there. This is the one case where `0 changes` doesn't mean "nothing is wrong", and it is the other reason to practise in a throwaway data directory.

#### Unattended: cron, CI, git hooks

Cron mails whatever a job prints, so wrap a scheduled job with `--quiet-unless-changed`:

```
0 3 * * * /usr/local/bin/siftr --quiet-unless-changed -- /usr/local/bin/backup.sh --full
```

The job's own output and exit code pass through as always. siftr adds:

| The run | siftr prints |
|---|---|
| nothing changed, or the first runs, with too little baseline to compare | nothing |
| a change outside every example only (the environment or suite hooks) | nothing |
| interrupted or killed, so not compared | nothing |
| a change, INCOMPLETE included | the report |
| an earlier change still open | the report, every run until it's fixed or `siftr dismiss`ed |
| not recorded (the data directory busy or unusable), or siftr failing | its warning or error |

A reminder repeats on purpose: a regression left in place should keep nagging. `siftr dismiss sN -m why` stops it. `siftr cron` prints each crontab job in this form.

#### When a run is incomplete

A run that skipped examples its baseline ran (a spec file failed to load, `--fail-fast` stopped it, a focus filter) says so in its first line. It reports the reason as one INCOMPLETE change instead of everything it never got to:

```
$ siftr run -q -- bundle exec rspec      # with an unclosed `RSpec.describe "broken" do` appended to a spec
r7 (incomplete: 1 error outside examples) vs 6 baseline runs (r1…r6): 1 change
  s6   INCOMPLETE  6 baseline runs  ./spec/requests/users_spec.rb failed to load: SyntaxError: unexpected end-of-inp…  failed outside examples: 1 now, 0 in baseline runs
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
  s7   NEW         7 baseline runs  ./spec/requests/users_spec.rb failed to load: SyntaxError: compile error  new: 1 now, in none of 7 baseline runs
       evidence: 1 line
next: siftr explain s7
```

#### A deleted spec file

Its examples collapse into one change, which never outranks a real regression:

```
$ siftr ingest --context hunt --dir fixtures/rspec_hunt/a20_warn1   # three times
$ siftr ingest --context hunt --dir fixtures/rspec_hunt/a4_warn3
r4 vs 3 baseline runs (r1 r2 r3): 2 changes
  s1   FREQUENCY   3 baseline runs  DEPRECATION: old api  count 1 → 3
       evidence: 3 lines
  s2   DISAPPEARED 3 baseline runs  16 examples of ./spec/b_spec.rb  gone, in all 3 baseline runs
       evidence: in the baseline runs, not this one
next: siftr explain s1
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
resources cpu 0.878s (0.604s user, 0.274s sys), max rss 104 MB, 0 voluntary and 506 involuntary switches  |  baseline cpu 0.87s (0.601s user, 0.269s sys), max rss 104 MB, 0 voluntary and 334 involuntary switches
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

The `resources` line appears for any run that recorded what the kernel charged it, the baseline being the median of its baseline runs. No rule reads those numbers — they are there to tell a loaded machine from a code change.

### `siftr evidence <BEHAVIOR>`

The raw lines kept for a behavior, each tagged with its stream and line number, plus the path to each of the run's captures that is still on disk — a run recorded with `SIFTR_CAPTURE=off`, or one whose captures retention has pruned, lists none. Takes a behavior id or a unique prefix of 4+ hex digits. `--run` picks the run (default: the latest where the behavior occurred), `-n` limits the lines (default 8).

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

A run's top behaviors, by count (default) or `--by time`. `-n` limits rows (default 20). The two orders answer different questions, and on a Rails suite the default answers the less interesting one:

```
$ siftr summary r4 -n 5
r4: 253 lines, 47 behaviors, bundle exec rspec
    COUNT ERRORS      P50      P95    TOTAL  BEHAVIOR
       55      0      0µs      0µs      0µs  3e0ff9b464  db.query  TRANSACTION SAVEPOINT active_record_<int>
       55      0      0µs      0µs      0µs  dcb25f6085  db.query  TRANSACTION RELEASE SAVEPOINT active_record_<int>
       32      0      0µs    100µs    1.5ms  3b6fe14cfe  db.query  Comment Create INSERT INTO "comments" ("body", "created_at", "post_id", "updated_at") VALUES (?) RET…
       17      0    100µs    100µs    1.4ms  1bc855df6d  db.query  Post Create INSERT INTO "posts" ("body", "created_at", "title", "updated_at", "user_id") VALUES (?) …
       10      0      0µs      0µs      0µs  1b4ca5c07e  db.query  TRANSACTION ROLLBACK TRANSACTION
next: siftr evidence 3e0ff9b464 --run r4
```

By count, a test suite is mostly transaction bookkeeping: `SAVEPOINT` and `RELEASE SAVEPOINT` take the top two rows at 55 each, and in this run the default's whole 20 rows hold **no test example at all**. Each example occurs exactly once, so the count-1 rows tie and are ordered by behavior id, which leaves the examples just past the cut. So use the default to ask "what does this run do most of", and **`--by time` to find a slow test**:

```
$ siftr summary r4 --by time -n 5
r4: 253 lines, 47 behaviors, bundle exec rspec
    COUNT ERRORS      P50      P95    TOTAL  BEHAVIOR
        1      0   54.7ms   54.7ms   54.7ms  332a3a42c0  test.summary  rspec
        1      0   54.7ms   54.7ms   54.7ms  21991bc714  log  Finished in <duration> (files took <duration> to load)
        1      0     25ms     25ms     25ms  224151e20c  test.example  ./spec/requests/users_spec.rb # Users lists users
        1      0   10.2ms   10.2ms   10.2ms  872cda219e  test.example  ./spec/requests/users_spec.rb # Users shows a user with posts and comments
        1      0     10ms     10ms     10ms  0b6cf2d542  http.request  GET UsersController#index 2xx
next: siftr evidence 332a3a42c0 --run r4
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

`--sources` lists what each of those runs actually read. `siftr sources` says what siftr *can* read here, before running anything; this says what the recording observed, per run, afterwards. Two runs of the same command in the same directory, with `rails_log` switched off in `.siftr.toml` between r2 and r3:

```
$ siftr history --sources
runs in /tmp/proj_run, and what each read
  r4      8m ago  rusage stderr stdout                bin/rspec
  r3      8m ago  rusage stderr stdout                bin/rspec
  r2      8m ago  rails_log rusage stderr stdout      bin/rspec
  r1      8m ago  rails_log rusage stderr stdout      bin/rspec
next: siftr summary r4
```

A run that recorded no sources reads `not recorded`, which means siftr can't say what it read — not that it read nothing. `ingest` replays a capture rather than choosing sources, so an ingested run always reads that way.

### `siftr ack <SIGNAL>` and `siftr dismiss <SIGNAL>`

`ack` marks a signal as being acted on; `dismiss` marks it as not worth acting on, which also stops its `still open:` reminder. `-m TEXT` says why.

```
$ siftr ack s5 -m 'restoring the email validation'
s5 acked: ERROR ./spec/models/user_spec.rb # User requires an email
note: restoring the email validation
next: siftr changes r6
```

### `siftr ingest [FILE]`

Records a file, or stdin, as a run's stdout, for output you already have. `--context NAME` groups comparable inputs (default `ingest`). `--dir DIR` replays a captured scenario: any of `stdout.txt`, `stderr.txt`, `rspec.ndjson`, `test.log`, `exit_code.txt`. `--quiet-unless-changed` and `--no-report` work as for `run`.

```
$ siftr ingest --context demo --dir fixtures/rails_demo/baseline      # and baseline_2
$ siftr ingest --context demo --dir fixtures/rails_demo/slow
r15 vs 2 baseline runs (r13 r14): 1 change
  s9   LATENCY     2 baseline runs  ./spec/models/post_spec.rb # Post summarizes the body  5.37ms → 311ms
       evidence: 1 line
next: siftr explain s9
```

### `siftr cron`

What runs on a schedule here, where cron's output goes, and the line that records each crontab job through `siftr --quiet-unless-changed --`. It reads your crontab, `/etc/crontab`, `/etc/cron.d`, launchd user agents on a calendar or interval, the mail spool, `/var/log/cron`, syslog and the macOS unified log. It edits nothing, runs no job and records nothing. With a synthetic crontab and agent:

```
$ siftr cron
scheduled jobs
  crontab -l               2 jobs
    0 3 * * *  /usr/local/bin/backup.sh --full
      record it: 0 3 * * * /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/readme-cron/bin/siftr --quiet-unless-changed -- /usr/local/bin/backup.sh --full
    */15 * * * *  cd ~/notes && git pull -q
      record it: */15 * * * * /private/tmp/claude-501/-Users-dpepper-code-lib-rust-siftr/297ebebd-ecce-49be-9b17-dbab814edaf1/scratchpad/readme-cron/bin/siftr --quiet-unless-changed -- sh -c 'cd ~/notes && git pull -q'
  /etc/crontab             absent
  /etc/cron.d              absent
  ~/Library/LaunchAgents   1 job
    StartCalendarInterval  com.example.sync
where cron's output goes
  /var/mail/me             absent
  /var/log/cron            absent
  /var/log/syslog          absent
  unified log, last 7d     no lines from cron
note: a wrapped job adds nothing to cron's mail unless something changed or is still open
next: crontab -e, and replace a job with its record-it line
```

A job that needs a shell (`cd`, `&&`, `~`, `%`, redirections) keeps it through `sh -c`. siftr names itself by absolute path because cron's `PATH` is minimal. A launchd agent gets no line: only its plist could change, and siftr doesn't touch it.

### `siftr sources`

What siftr can read here: the command's own output, plus the side channels a command writes somewhere else. Each row gives the source's name, whether it's on, whether it applies to the command you name, and why either way. Read-only — it prepares nothing, runs nothing and records nothing.

```
$ siftr sources -- bundle exec rspec
sources for bundle exec rspec in ~/src/siftr/dogfood/rails_demo
  stdout     on   applies         the command's own output (always read)
  stderr     on   applies         the command's own output (always read)
  rspec      on   applies         file:rspec-events — RSpec's per-example results, from a listener added to SPEC_OPTS (the command runs rspec)
  rails_log  on   applies         file:log/test.log — the SQL and request lines the run appends to the Rails test log (log/test.log is there)
  rusage     on   applies         — the CPU, peak memory and context switches the kernel charged the run (evidence, never a signal) (every run siftr wraps has a child to measure)
next: siftr run -- bundle exec rspec
```

After the name comes the stream it feeds, which is what a piece of evidence points at: an exemplar's `stream` and a run's `streams` use that spelling. A source that reads no bytes shows an em dash instead: `rusage` takes its numbers from the wait rather than from a file, so it opens no stream and joins no run's `streams`.

What applies depends on the command as much as on the directory, so name the command you'd wrap. Elsewhere, or for a command that isn't a test run:

```
$ siftr sources -- make test
sources for make test in ~/src/notes
  stdout     on   applies         the command's own output (always read)
  stderr     on   applies         the command's own output (always read)
  rspec      on   does not apply  file:rspec-events — RSpec's per-example results, from a listener added to SPEC_OPTS (the command isn't an rspec run)
  rails_log  on   does not apply  file:log/test.log — the SQL and request lines the run appends to the Rails test log (the command isn't a Ruby test run)
  rusage     on   applies         — the CPU, peak memory and context switches the kernel charged the run (evidence, never a signal) (every run siftr wraps has a child to measure)
next: siftr run -- make test
```

With no command it judges the directory alone, and says that's what it did. This is what siftr *can* read; to see what a run actually did read, use `streams` in `run -j` or `ingest -j` as it happens, or `siftr history --sources` for any recorded run afterwards. The two can differ: a source can be on and apply and still feed a run nothing.

### Common flags and exit codes

- `-j` prints exactly one JSON document on stdout, on every command. Empty results are still that command's document (exit 1). Errors, argument errors included, are `{"error": {"code", "message"}}` (exit 2), where `code` is `usage`, `not_found`, `busy` (another siftr held the data directory too long; retry) or `failed`. **[docs/json.md](docs/json.md) describes every command's document field by field**, including the shapes that differ between commands — three commands return a bare array, and `streams` is null in `changes` but a list in `run -j`.
- `--home DIR` or `SIFTR_HOME`: the data directory. Default `$XDG_DATA_HOME/siftr`, else `~/.local/share/siftr`.
- Every human report ends with a `next:` line: the command to drill down with.

| Command | Exit |
|---|---|
| `run` | the command's own code; 125 if siftr fails before starting it, 126 if it can't be executed, 127 if not found |
| `ingest` | 0 recorded, 2 error |
| `cron` | 0 found a job or cron output, 1 found neither, 2 error |
| `sources` | 0 listed, 2 error |
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

## Configuration

There are two or three things worth setting per project, so the file stays that small. `.siftr.toml`, found by walking up from where you run siftr and stopping at the project root, says which of siftr's **sources** run:

```toml
# This app's log/test.log is shared with a dev server, so don't read a run's lines out of it.
[sources.rails_log]
enabled = false
```

| Source | What it reads |
|---|---|
| `rspec` | per-example results, from the reporter listener siftr adds through `SPEC_OPTS` |
| `rails_log` | the slice of `log/test.log` the run appended: SQL and request lines |
| `rusage` | what the kernel charged the run: CPU time, peak memory, context switches |

All three are on until a file turns one off. `~/.config/siftr/config.toml` (or `$XDG_CONFIG_HOME/siftr/config.toml`) takes the same keys for every project, and the project file wins.

That is the whole language. Retention (`SIFTR_KEEP_*`) and privacy (`SIFTR_REDACT`, `SIFTR_CAPTURE`) stay environment-only: they're about your machine and the data directory every project shares, not about one project, and two projects can't give one data directory two answers.

A file siftr can't understand never stops your command. An unknown key, an unknown source, a value that isn't `true` or `false`, or a file that isn't TOML at all warns once on stderr and leaves the default standing:

```
siftr: warning: ~/code/app/.siftr.toml: rspce is not a source (rspec, rails_log, rusage); ignoring it
siftr: warning: ~/code/app/.siftr.toml: sources.rspec.enabled is not true or false (integer); using the default
```

`siftr status` says what each source is set to, which file set it, and which files siftr looked for — so a file that isn't taking effect says so instead of being quietly ignored.

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
data      ~/.local/share/siftr
database  104 KB, schema 12
captures  32.3 KB for 1 run
runs      1 run of 1 command; oldest r1 0s ago, newest r1 0s ago
keep      stats of the last 100 runs of each command (default; set SIFTR_KEEP_RUNS)
          evidence, raw lines and captures, of the last 20 (default; set SIFTR_KEEP_EVIDENCE)
          nothing of a command not run for 30 days (default; set SIFTR_KEEP_DAYS)
config    rails_log off (~/code/app/.siftr.toml)
          rspec on (default)
          rusage on (default)
          looked in ~/code/app/.siftr.toml, ~/.config/siftr/config.toml (nothing to read)
    RUNS  STATS  EVIDENCE  CAPTURES  NEWEST    COMMAND
       1      1         1   32.3 KB  0s ago    demo
next: siftr history
```

The `looked in` line is a search path, not a problem report. A path marked `(nothing to read)` is one siftr checked and found nothing usable at — nearly always because no file is there; a file that *is* there but isn't valid TOML warned on stderr when it was read. A file that set something carries no marker and is named again beside each source it set, as `~/code/app/.siftr.toml` is above.

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

3. For structure, use `siftr changes -j` (or `siftr run -j -- …`). The same order applies: if `run.complete` is false, fix the `incomplete` signal first. The gate condition is three fields: `changes == 0`, `open_signals == []`, and `baseline_runs` with 2 or more ids. Test those, not `signals[0]` — on a clean run there is no first signal, and a trimmed document can hand you a `null` that reads as "no id" rather than failing. Otherwise read `groups[0].headline`, then `open_signals`. A non-null `not_recorded` means the run wasn't recorded; rerun for a report.
4. With `-j`, exit 2 means `error.code` tells you what went wrong: `usage` (fix the arguments), `not_found` (the run, signal, behavior or context doesn't exist), `busy` (retry) or `failed`. Exit 1 means nothing was found, and you still get the command's normal document, empty (`run: null`; `[]` for `history`).
5. Once you act, record it: `siftr ack <signal> -m '…'` when you're fixing it, `siftr dismiss <signal> -m '…'` when it's intended. `siftr history --signals` shows what became of each.

**`open_signals` versus `history --signals`.** They answer different questions, and a gate wants the first:

- **`open_signals`, in the current run's document, is what is wrong right now.** It lists changes from earlier runs that this run still shows, judged against each signal's own original baseline rather than the rolling one — which is why a regression the baseline has absorbed still appears. A change that was fixed and came back is listed again on every run it is present in, however long ago it was first raised. This is the field to gate on, together with `changes`.
- **`history --signals` is the story of each signal, not the state of the suite.** `outcome` is its latest verdict, `recurrences` how many times it came back, `resolved_in` and `recurred_in` the first of each. Use it to report and to review, not to decide whether the tree is clean. Its fields are in [docs/json.md](docs/json.md).

**What a clean gate promises, and what it doesn't.** `changes == 0` with `open_signals == []` says nothing moved against the baseline *and* no earlier change is still here — including one the rolling baseline has absorbed, and one that has been fixed and has come back any number of times. A change that keeps returning is reported on every run it is present in, for as long as it keeps returning.

Three things that still read as clean, in falling order of how likely you are to meet them:

- **A change nobody ever fixed stops being reported** about 10 runs after it was raised, once every run siftr compares against has it: it is then what this context does, and no comparison can see it. You will have been told on each of those runs. Fix it or `siftr dismiss` it before then; `siftr history --signals` still lists it as `open` afterwards.
- **A change that was already there before siftr's first run of this context was never a change**, so nothing will ever report it. Baselines are built from what siftr has seen, and it has always seen this.
- **Past `SIFTR_KEEP_RUNS` runs** (default 100) the runs a reminder is judged against are pruned, and it lapses to `unknown` rather than being reported.

The `-j` fields that matter (full schema: top of [`src/bin/siftr/output.rs`](src/bin/siftr/output.rs)):

- `run.complete`: false when the run was unfinished, interrupted, or INCOMPLETE.
- `streams`: what this run actually captured — `stdout`, `stderr`, `file:rspec-events`, `file:log/test.log` — in `run -j` and `ingest -j`, spelled as an exemplar's `stream` is, so evidence joins straight to it. A stream opens on its first byte, so a command that wrote nothing to stderr doesn't list it. `changes -j` reports null: the store doesn't hold what a run read. `siftr sources` says what *could* apply here.
- `changes`: number of code-level groups. `baseline_runs`: the run ids compared against. `skipped_runs[]`: {`run`, `reason`}, where `reason` is `no_test_summary`, `errors_outside_examples`, `stopped` or `subset`.
- `groups[]`: `rank` (1 is most important), `headline` (a signal id), `signals` (ids in the group), `setup` (true when the change happened outside every example: the environment or suite hooks, not the code), `disappeared_examples` (null, or {`file`, `examples`} for a deleted spec file's examples collapsed into one group).
- `signals[]`, in rank order:
  - `kind`: `error`, `new`, `disappeared`, `frequency`, `latency` or `incomplete`.
  - `measure`: `count`, `queries`, `duration_ms`, `failed`, `examples` or `errors_outside_of_examples`.
  - `current`, compared with `baseline` {`runs`, `present_in`, `median`, `min`, `max`, `failures`}.
  - `confidence` in [0, 1): how much baseline backs the claim, **not** how much it matters. For every kind but LATENCY it is exactly `(n+1)/(n+2)` over `baseline.runs`, so it carries no effect size and discriminates nothing — rank on `tier` and read `current` against `baseline`. The human report prints the run count itself (`3 baseline runs`) for that reason; the measurement is in [docs/findings/confidence.md](docs/findings/confidence.md).
  - `tier`: 1 error through 5 outside examples.
  - `behavior` {`id`, `kind`, `template`}: what changed. Pass `id` to `evidence`.
  - `attribution.scope`: the test example it happened in, or null outside examples. `attribution.phase`: `setup`, `example`, `between` or `teardown` (before the first example, in one, between two, after the last). `attribution.setup` is true only for `setup`.
  - `exception`: the exception class, for `error`.
- `open_signals[]`: signals from earlier runs that are still open and weren't raised again, in the same shape as `signals[]`.
- `not_recorded` (`run -j` only): null, or {`code`, `message`} when the command ran but siftr couldn't record it.

Trimmed with `jq '{run: {id: .run.id, complete: .run.complete}, changes, baseline_runs, skipped_runs, groups, signals: (.signals[:1]), open_signals}'`, output unedited:

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
          "roles": [],
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
        "roles": [],
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

Use `.signals[:1]`, not `[.signals[0]]`. On a run with no signals the slice yields `[]` while the index yields `[null]`, and a downstream `.signals[0].id` check then reads a null instead of failing — a gate built that way passes a broken build silently. The same recipe on the clean run that follows, output unedited:

```json
{
  "run": {
    "id": "r6",
    "complete": true
  },
  "changes": 0,
  "baseline_runs": [
    "r5",
    "r4",
    "r3",
    "r2",
    "r1"
  ],
  "skipped_runs": [],
  "groups": [],
  "signals": [],
  "open_signals": []
}
```

## What it captures (RSpec + Rails)

`siftr run -- bundle exec rspec` reads three channels, and measures the run itself:

- **Per-example results** from an RSpec reporter listener, added by appending `--require` to `SPEC_OPTS`. It isn't a formatter, so your `.rspec` formatters and any `SPEC_OPTS` you've set keep working. It also sees errors outside examples, such as a spec file that fails to load or a hook that raises.
- **SQL and request lines** from the bytes the run appended to `log/test.log`. Each line is attributed to the example that was running when it was written, or to before, between or after examples. One log rotation during a run is handled exactly. With two or more, bytes are lost.
- **stdout and stderr**. When siftr's stdout is a terminal, the child gets a PTY, so RSpec's colours survive. stderr stays a separate pipe, because deprecation warnings land there.
- **What the kernel charged the run** — CPU time, peak memory, and voluntary and involuntary context switches, from one `getrusage` of the reaped child. It costs ~0.19µs, needs no sampler, and doesn't touch how your command runs or exits. This is **evidence only**: no signal ever fires on it, because CPU and memory vary far too much run to run to judge. `siftr explain` shows it beside the baseline's, so you can tell a slower run from a busier machine. On macOS the kernel leaves the disk-I/O counters at zero, so siftr reports none.

`siftr sources` says which of these apply to a command here; a run's `-j` `streams` says which it actually captured. `rusage` is in that listing but never in `streams`: it reads no bytes.

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
- **Context is the project plus the command line, as typed** — the fact stated under [30 seconds](#30-seconds), with its corner cases. `bundle exec rspec spec/models/user_spec.rb:12` has its own baseline, separate from the full suite's. The project is the nearest directory with a manifest (`Gemfile`, `Cargo.toml`, `package.json`, …), looking no higher than the git root; with none, it's the working directory. Two apps with their own Gemfiles in one repo get separate baselines, but in a repository with no manifest every directory is its own project.
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
