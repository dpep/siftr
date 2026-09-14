# Changelog

## Unreleased

First version. Nothing to migrate.

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
