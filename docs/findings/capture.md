# Capture: RSpec and Rails side channels

How `siftr run -- bundle exec rspec` gets per-example and per-request telemetry
without changing what the user sees. Measured 2026-09-13 on Ruby 3.4.9,
rspec-core 3.13.6, Rails 8.1.3.1, rspec-rails 8.0.4.

## 1. RSpec: per-example data via `SPEC_OPTS`

Default progress output carries no per-example data. The question is whether
`SPEC_OPTS` can add a machine-readable channel and leave stdout untouched.

Harness: run the suite with no `SPEC_OPTS`, then with each variant, and diff
stdout and stderr with durations normalized. `.rspec` is set to one of three
states each time: absent, `--format progress`, or `--format documentation`.
Projects: a scratch suite (pass, 50ms sleep, pending, failure), `dogfood/rails_demo`,
and a real 934-example gem suite.

| `SPEC_OPTS` added | no `.rspec` | `.rspec` progress | `.rspec` documentation |
|---|---|---|---|
| `--format json --out F` | stdout **empty** | stdout **empty** | documentation **dropped** |
| `--format progress --format json --out F` | identical | identical | documentation **replaced by progress** |
| `--require siftr_rspec_listener.rb` | identical | identical | identical |

Why: `ConfigurationOptions#organize_options` merges `~/.rspec`, `.rspec`,
`.rspec-local`, the CLI args and `SPEC_OPTS` so that the later source wins. Only
`:requires` and `:libs` are concatenated. A `--format` in `SPEC_OPTS` therefore
replaces every other formatter. `Formatters::Loader#setup_default` adds progress
only when no formatter is set. No `--format` combination is safe unless siftr
re-derives the user's formatters from every source.

`--require` concatenates, so the channel is a required file. That file
registers a **reporter listener**, not a formatter. Calling
`config.add_formatter` from a required file also empties stdout (verified).

More checks:

- **User's `SPEC_OPTS`**: with `SPEC_OPTS="--format documentation"` already set,
  appending `--require …` leaves stdout identical. Overwriting their
  `SPEC_OPTS` would drop their formatter, so siftr must append.
- **`--profile`** adds a "Top N slowest examples" section to stdout. siftr must
  not add it; the listener's `run_time` carries the same data.
- **Real suite, 934 examples**: stdout and stderr are identical, exit code is
  unchanged, and the listener wrote 1870 events.
- **`.rspec` with `--out file`**: RSpec's output still goes to the file, and
  the listener still works.
- **TTY**: under a pty (`script -q`), RSpec colors its output (ANSI). Piped, it
  doesn't. Output was identical with the listener in both modes. So siftr must
  run the child under a pty to preserve what the user sees, whatever
  `SPEC_OPTS` it sets.
- The built-in JSON formatter writes its whole document in `close`. A killed
  or crashed run leaves nothing. The listener writes synced NDJSON line by line.

**Recommendation.** Write `siftr_rspec_listener.rb` (in this directory) to the
run's temp dir, then set:

```
SPEC_OPTS="$SPEC_OPTS --require '<tmp>/siftr_rspec.rb'"   # append; value is Shellwords-split
SIFTR_RSPEC_EVENTS=<tmp>/rspec.ndjson
SIFTR_RSPEC_LOG=<project>/log/test.log                      # only if it exists
```

Events: `start`, `example_started`, `example` (id, description,
full_description, file_path, line_number, status, run_time, pending_message,
exception class and message), and `summary`. Each event carries `log_offset`
when `SIFTR_RSPEC_LOG` is set.

Untested: parallel_tests (several processes appending to one file), spring and
binstubs, `rake spec`, RSpec < 3.13.

## 2. Rails: `log/test.log` by byte offset

SQL and request lines go to `log/test.log`, not stdout.

**Appended bytes are exactly one run's log.** Run A started on a truncated log
and wrote 28863 bytes. Run B started on top of it, taking the size from 28863 to
57726. After normalizing durations and timestamps, `[28863, 57726)` of the file
is **identical** to run A's log.

**Per-example slices.** The log has no example boundaries and no timestamps
except on `Started`. The listener runs in the test process and Rails flushes
every write (`autoflush_log`). So the log size stamped at `example_started` and
at the example's finish bounds exactly that example's lines. Verified on the
demo: the show request's slice holds its `Started … Completed` block and its
setup inserts.

Raw line formats (`\e` = ESC):

```
  \e[1m\e[36mUser Load (0.0ms)\e[0m  \e[1m\e[34mSELECT "users".* FROM "users" WHERE "users"."id" = ? LIMIT ?\e[0m  [["id", 1], ["LIMIT", 1]]
  \e[1m\e[36mUser Create (0.2ms)\e[0m  \e[1m\e[32mINSERT INTO "users" (…) VALUES (?, ?, ?, ?) RETURNING "id"\e[0m  [["created_at", "2026-09-14 00:00:06.391348"], ["email", "[FILTERED]"], …]
  \e[1m\e[36mTRANSACTION (0.0ms)\e[0m  \e[1m\e[35mSAVEPOINT active_record_1\e[0m
Started GET "/users/1" for 127.0.0.1 at 2026-09-13 17:00:06 -0700
Processing by UsersController#show as HTML
  Parameters: {"id" => "1"}
  Rendering users/show.html.erb within layouts/application
  Rendered users/show.html.erb within layouts/application (Duration: 3.5ms | GC: 0.0ms)
Completed 200 OK in 4ms (Views: 3.6ms | ActiveRecord: 0.1ms (3 queries, 0 cached) | GC: 0.0ms)
```

- The SQL color depends on the statement: SELECT 34, INSERT 32, DELETE and
  ROLLBACK 31, SAVEPOINT 35, BEGIN 36. Strip ANSI before templating.
- Bind values trail the SQL as an array. Masking must cover them, including
  timestamps.
- Rails 8.1's `Completed` line already reports `(N queries, M cached)`.
- 130 of 211 baseline lines are `TRANSACTION` begin, savepoint and rollback
  noise from transactional fixtures.
- **Deprecation warnings go to stderr, not the log**, because the test env
  sets `active_support.deprecation = :stderr`. Example:
  `DEPRECATION WARNING: … (called from … at /abs/path.rb:13)`.

**Rotation.** `load_defaults` 7.1+ sets `log_file_size = 100MB` in local envs,
which means `ActiveSupport::Logger.new(path, 1, 100MB)`. Ruby's Logger renames
`test.log` to `test.log.0` (keeping one) and opens a new file whose first line
is `# Logfile created on … by logger.rb/v1.7.0`. Verified with a 2000-byte
limit:

- no rotation: same inode, and `[start, end)` is exact
- one rotation: the inode changed and `test.log.0` has the starting inode.
  The run's log is `test.log.0[start..]` plus `test.log` minus its header line,
  which is exact.
- two or more rotations: `test.log.0` is not the starting inode. Bytes are lost
  and this is detectable, so report the log evidence as partial.

Inode numbers are identity only while the file exists. ext4 gives a freed
number to the next file created, so after two rotations the new `test.log`
can carry the starting inode (APFS doesn't reuse this fast). siftr holds
`test.log` and `test.log.0` open from start to end, so neither can be freed and
no new file can take their numbers.

**Truncation.** `rails log:clear` truncates in place (same inode). The logger
opens the file O_APPEND, so writes continue from offset 0. If the end size is
below the start size, the truncation is detectable. If the run writes more than
`start` bytes after truncating, size alone misses it. Proposed and not built:
hash the bytes just before `start`, and re-check that hash at the end.

**Recommendation.** Before spawning, record the size and inode of
`<project>/log/test.log`. After exit, read the run's bytes by the rules above.
Treat them as `file:log/test.log` observations and attribute them to examples
by `log_offset`. If the log is missing, skip it.

## Fixtures

`fixtures/rails_demo/<scenario>/` holds `stdout.txt`, `stderr.txt` and
`exit_code.txt`, captured through a pipe (no ANSI). It also holds
`rspec.ndjson` (the listener events, with `log_offset` rebased to this
`test.log`) and `test.log` (the run's bytes, ANSI kept). Regenerate with
`dogfood/rails_demo/bin/capture_fixtures`.

| Scenario | Env | Measured change vs baseline |
|---|---|---|
| `baseline`, `baseline_2` | none | 10 examples, 0 failures, 1 pending. Show request 3 queries. Suite 0.093s and 0.098s |
| `baseline_documentation` | `SPEC_OPTS=--format documentation` | stdout format only |
| `n_plus_one` | `SIFTR_DEMO_N_PLUS_ONE=1` | show request 3 → 10 queries (8 `Comment Load … "post_id" = ?`), log +810 bytes |
| `slow` | `SIFTR_DEMO_SLOW=1` | `Post summarizes the body` 0.0097s and 0.0011s → 0.311s |
| `warn` | `SIFTR_DEMO_WARN=1` | stderr gains 2 `DEPRECATION WARNING` lines (baseline stderr is empty), log unchanged |
| `fail` | `SIFTR_DEMO_FAIL=1` | exit 1. `User requires an email` fails with `ExpectationNotMetError` |

Timing noise: the same millisecond-scale example took 0.0097s in one baseline
run and 0.0011s in the other, a 9x spread. Latency signals need an absolute
floor as well as a ratio.

`fixtures/cargo_test/shelf/` holds a non-Ruby run of `cargo test`. The test
harness lines are on stdout; compiler warnings and `Running …` lines are on
stderr.
