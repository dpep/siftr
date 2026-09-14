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
- A killed or interrupted run is never used as a baseline, nor is a run recorded by an older siftr, so one bad run can't hide the next regression. (Runs that skipped examples: see below.)
- An incomplete run reports one INCOMPLETE change (e.g. the spec file that failed to load, with its error) instead of everything it didn't get to run.
- A command killed by a signal counts as interrupted. siftr stops waiting on leftover background output about a second after the command exits. If the data directory can't be used, the command still runs — unrecorded, with a warning.
- SQL logged between or after examples is attributed as such rather than as setup, and attribution no longer drifts after CRLF or very long log lines.
- `-j` always prints one JSON document: empty results (exit 1) and errors, argument errors included (`{"error":{"code","message"}}`, exit 2).
- An unfixed regression the rolling baseline has absorbed shows as `still open:` in `run`, `ingest` and `changes` (`open_signals` in `-j`) until it's fixed or dismissed.
- Reports name skipped baseline runs and why (`skipped_runs`), and say when a run was incomplete (`run.complete`). `explain` and `evidence` show a failure's whole message.
- Changes outside examples say whether they happened before, between or after them. In `-j`, `attribution.setup` is now true only for setup; use `attribution.phase`.
- Two apps in one repository no longer share a baseline: the project is the nearest directory with a manifest (Gemfile, Cargo.toml, package.json, …), else the current directory.
- A run is left out of a baseline only when it skipped examples this run ran (a failed spec file, `--fail-fast`, a focus filter). A suite that grows or shrinks, a red `--fail-fast` suite, or fixing a spec file that never loaded no longer hides a regression landing in that run.
- A run is INCOMPLETE only when it skipped examples its baseline ran; deleting spec files is a real change, not a partial run. A load error names the file and Ruby's cause: `./spec/x_spec.rb failed to load: SyntaxError: …`.
- `history --signals`: a focus or load-error run no longer counts as resolving a signal, and INCOMPLETE is never listed as still open. `history` marks incomplete runs, and `history --context <unknown>` exits 2 like `changes`.
- A busy data directory no longer holds up `siftr run` indefinitely: if another siftr holds it for more than 2s, the command runs unrecorded, with a warning saying why.
- Runs whose command was killed by a signal (exit 129–159) never join baselines, including runs recorded by older builds.
- Retention never prunes a run that is still recording, and beyond the limits it keeps whatever the latest run still compares against: its baseline runs, their own baselines, and the evidence a still-open reminder points `explain` to. When a signal's evidence was pruned anyway, `explain` shows its numbers and says so rather than failing.
- `siftr run -- CMD | head` stops the command as it would unwrapped: the command gets SIGPIPE, siftr exits the same way, and the truncated run is kept out of baselines.
- A busy store never delays the command's start or its output; if it stays busy, the run passes through unrecorded with one warning. `siftr run -j` prints a JSON document even then (`run: null`, `not_recorded: {code, message}`), and `busy` is a JSON error code for every command.
- When earlier changes are still open, the run header says `no new changes · N still open` instead of `0 changes`.
- A new error outside examples — a `raise` after a describe block, a suite hook that raises — is a NEW change even when every example ran.
- Deleting or renaming a spec file is one change (`N examples of <file>`), not one per example, and never outranks a real regression. Still-open reminders no longer repeat examples or queries that disappeared on purpose; a DISAPPEARED signal supporting a regression still rides with it.

### Upgrading

- An app below its repository root, or a directory in a repository with no manifest, starts a fresh baseline once ("no earlier runs"). Runs at a repository root with a manifest keep theirs.
- Runs recorded before this version carry no count of defined examples; a smaller one is still judged by which examples it ran, so right after a suite grew it may be skipped until it ages out of the last 10 runs.
- The first command after upgrading migrates the database — seconds on a large store. Then run `siftr gc` once to prune the backlog and shrink the file; until you do, each run prunes a bounded amount.
