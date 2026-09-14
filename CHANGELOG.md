# Changelog

## Unreleased

First version. If you ran a pre-release build, see **Upgrading** below.

- `siftr run -- CMD` runs a command, passes its output through unchanged (colour included, via a PTY), exits with its code, and prints what changed since recent runs of the same command to stderr.
- RSpec + Rails: per-example results come from a reporter listener added through `SPEC_OPTS` (your formatters are untouched), and SQL and request lines from the slice of `log/test.log` the run wrote.
- Signals: ERROR, NEW, DISAPPEARED, FREQUENCY (SQL counts, per-request and per-example query counts) and LATENCY (a single example, at least +100ms and 4x). Related signals are grouped under one headline. Confidence comes from the size of the baseline.
- `changes`, `explain`, `evidence`, `summary`, `history` to drill from a change to the raw lines behind it. Every command takes `-j`.
- `siftr ingest` records a file, stdin, or a captured scenario directory (`--dir`) as a run.
- Ctrl-C'd runs are kept for evidence but never used as a baseline.
- `siftr ack <signal> [-m TEXT]` and `siftr dismiss <signal> [-m TEXT]` record whether a signal is being acted on or isn't worth it.
- `siftr history --signals` shows what became of recent signals — open, resolved (after investigation or without), recurred — each judged against its original baseline, so a regression the rolling baseline has absorbed still reads as open.
- siftr keeps a local record of how its signals are used (shown, explained, evidence requested, acked, dismissed) so it can later learn which signals matter. Nothing leaves the machine.
- Two siftr processes opening a new data directory at once no longer race.
- siftr deletes old data. Per command it keeps stats for the last 100 runs (`SIFTR_KEEP_RUNS`) and exemplar lines plus raw captures for the last 20 (`SIFTR_KEEP_EVIDENCE`); a command not run for 30 days is forgotten (`SIFTR_KEEP_DAYS`). Reading something pruned says so and names the setting.
- `siftr status` shows what the data dir holds and what retention will do; `siftr gc [--dry-run]` prunes now and reclaims space.
- A log side stream's capture is `file-log_test.log`, not `file-log_test.log.log`.
- A run that was killed, failed to load a spec file, stopped early (`--fail-fast`) or ran a focused subset is never used as a baseline — nor is a run recorded by an older siftr — so one bad run can't hide the next regression.
- An incomplete run reports one INCOMPLETE change (e.g. the spec file that failed to load, with its error) instead of everything it didn't get to run.
- A command killed by a signal counts as interrupted. siftr stops waiting on leftover background output about a second after the command exits. If the data directory can't be used, the command still runs — unrecorded, with a warning.
- SQL logged between or after examples is attributed as such rather than as setup, and attribution no longer drifts after CRLF or very long log lines.

### Upgrading

- The first command after upgrading migrates the database — seconds on a large store. Then run `siftr gc` once to prune the backlog and shrink the file; until you do, each run prunes a bounded amount.
