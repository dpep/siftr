# siftr's JSON output

`-j` prints exactly one JSON document on stdout, on every command. This file says
what shape each one is. **JSON field names are a contract; human text is not.**

Every example here was produced by running the binary, not written by hand.

## The three rules a consumer needs first

1. **Most commands return an object. Three return a bare array** — `history`,
   `history --signals` and `history --sources`. There is no envelope and no
   top-level count on those: the array *is* the document.
2. **Nothing found is still that command's document**, with exit code `1`. An
   empty `history` is `[]`; an empty `changes` has `"run": null` and empty arrays.
   Don't treat exit 1 as failure — treat it as "no rows".
3. **An error is `{"error": {"code", "message"}}` on stdout**, with exit code `2`,
   including argument errors. `code` is `usage`, `not_found`, `busy` or `failed`.

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

Exit codes: `0` results, `1` nothing found, `2` error. `run` exits with the
wrapped command's own code instead; `status` exits `1` when something needs
attention.

## Numbers

Numbers are already rounded where they were built, to the precision the evidence
supports — don't re-round, and don't print more digits than arrive. Durations are
integer microseconds in `*_us`, milliseconds in `wall_ms`, and timestamps are
Unix milliseconds in `*_ms`. `confidence` is a number in `[0, 1)`.

## Shared objects

These appear inside several commands' documents.

### `run`

```json
{
  "id": "r5",
  "project": "/path/to/project",
  "context": "demo",
  "command": "siftr ingest --context demo --dir …",
  "cwd": "/path/to/project",
  "started_at_ms": 1789623164816,
  "finished": true,
  "wall_ms": 3,
  "exit_code": 0,
  "lines": 246,
  "overflow_events": 0,
  "interrupted": null,
  "uncompared": null,
  "complete": true
}
```

`interrupted` is the signal number, or null; an interrupted run is never compared
and never becomes a baseline. `complete` is false when the run is unfinished,
interrupted, truncated past the behavior cap (`overflow_events` above zero), or
signalled INCOMPLETE — it didn't run what its baseline runs did, or couldn't
record what it saw — so its changes don't mean what a whole run's do.
`overflow_events` above zero is the second case: past the 20,000-behavior cap a
behavior is admitted on the arrival order of its first occurrence, so such a run
raises nothing from its absences — a behavior it lacks may simply not have
fitted — and reports INCOMPLETE with measure `events_past_cap` whenever it had a
baseline to say that against. Truncation belongs to the run rather than to its
signals, so `complete` is false even when no signal carries it: a truncated first
run of a context has no baseline to compare with, and a truncated run whose
comparison was refused (below) recorded no signals at all. Its counts are still
exact, so FREQUENCY, ERROR and LATENCY stand, and it still baselines normally.

`uncompared` is null for a run that was compared. Otherwise it is how many
**signals** the comparison produced when that was past `signal::MAX_SIGNALS`
(1000), in which case **no changes were recorded**: that many is a statement that
the behaviors don't recur, not a set of findings. The threshold counts signals
rather than the changes they group into, and the two differ by design — the N+1
fixture is 4 signals in 1 change — so a run refused at 1017 signals might have
shown far fewer changes. Such a run is still `complete`
unless it was truncated as well — its behaviors, exemplars and capture are kept,
and it baselines normally. It has no verdict, so it also reports no
`open_signals`: judging those would mean re-running the comparison siftr just
refused.

### `behavior`

```json
{
  "id": "3e0ff9b46496a578",
  "kind": "db.query",
  "template": "TRANSACTION SAVEPOINT active_record_<int>",
  "roles": []
}
```

`id` is 16 hex digits and is stable across runs and machines. `kind` is one of
`test.example`, `test.summary`, `db.query`, `http.request`, `exception`, `log`,
`run.resources`. `roles` says what a template's paths are (`database`, `log`,
`test`, `source`, `config`, `temp`, …) — information only; no signal rule reads it.

### `stats`

```json
{
  "count": 55,
  "errors": 0,
  "duration": { "count": 55, "total_us": 0, "p50_us": 0, "p95_us": 0, "max_us": 0 }
}
```

`duration` is null for a behavior with no timing. For a single occurrence the
percentiles are that occurrence's own duration, not a histogram estimate.

### `signal`

```json
{
  "id": "s5",
  "run": "r5",
  "kind": "latency",
  "confidence": 0.63,
  "measure": "duration_ms",
  "current": 311.0,
  "baseline": { "runs": 4, "present_in": 4, "median": 5.42, "min": 1.06, "max": 9.68, "failures": null },
  "exception": null,
  "attribution": null,
  "tier": 2,
  "group": 1,
  "headline": true,
  "evidence_lines": 1,
  "behavior": { "id": "39b38737bf780ac5", "kind": "test.example", "template": "./spec/models/post_spec.rb # Post summarizes the body", "roles": [] }
}
```

`kind` is `error`, `new`, `disappeared`, `frequency`, `latency` or `incomplete`
(lowercase in JSON; human output upper-cases it). `measure` is `count`,
`queries`, `duration_ms`, `failed`, `examples`, `errors_outside_of_examples` or
`events_past_cap` (only on `incomplete`: how many events belonged to behaviors
that didn't fit under the cap, so this run's absences went unjudged).
`attribution`, when not null, is `{scope, phase, setup, current, baseline}` where
`scope` is the enclosing example's behavior and `phase` is `setup`, `example`,
`between` or `teardown`. Signals that share a `group` are one change; the one
with `headline: true` is its head.

`confidence` says how much baseline backs the claim, not how much it matters.
For every kind but `latency` it is exactly `(n+1)/(n+2)` over `baseline.runs` —
a bijection of that count, carrying no effect size — so it ranks nothing and
should not be thresholded. Rank on `tier`, and judge size from `current`
against `baseline`. Human output prints the baseline run count in its place for
this reason; the measurement behind that is
[findings/confidence.md](findings/confidence.md).

### `exemplar`

```json
{
  "stream": "file:log/test.log",
  "seq": 8,
  "line": "  [1m[36mTRANSACTION (0.0ms)[0m  …",
  "exception": null
}
```

`seq` is the line number within the run's capture of that `stream`. `line` is cut
at 1024 bytes with credentials masked; raw ANSI is preserved as captured.
`exception` is `{class, message}` when the line is a listener event carrying one.

## Per command

### `changes` — also `run -j` and `ingest -j`

An object. All three emit the same document.

```json
{
  "run": { "…": "the run object" },
  "behaviors": 47,
  "streams": null,
  "baseline_runs": ["r4", "r3", "r2", "r1"],
  "skipped_runs": [],
  "changes": 1,
  "groups_total": 1,
  "signals_total": 1,
  "groups": [ { "rank": 1, "setup": false, "headline": "s5", "signals": ["s5"], "disappeared_examples": null } ],
  "signals": [ { "…": "signal objects, rank order" } ],
  "open_signals": []
}
```

- `changes` is the number of code-level groups — groups with `setup: true`
  (the environment or suite hooks) are excluded from it but still listed.
- `groups_total` and `signals_total` count every group and signal the run
  raised, whatever this document lists. `changes -n N` lists at most N groups
  (highest-ranked first) and the signals those groups name, so
  **`groups_total` greater than `groups | length` is what a limit left out.**
  `changes` and both totals are the run's own numbers and never shrink with
  `-n`. Without `-n` the document is complete, and `run -j` and `ingest -j`
  always are.
- A run with `uncompared` set recorded no signals at all: `signals`, `groups`
  and `open_signals` are empty and `signals_total` is 0, while
  `run.uncompared` says how many signals were refused — signals, not the
  changes they would have grouped into.
- `skipped_runs` is `[{run, reason}]` with `reason` one of `no_test_summary`,
  `errors_outside_examples`, `stopped`, `subset`: recent runs left out of the
  baseline because they didn't run what this run did.
- `open_signals` are earlier runs' signals still open at this run and not raised
  again by it — a regression the rolling baseline has absorbed. Each is re-judged
  against its own original baseline, so a change that was fixed and came back is
  listed on every run it is present in, however old the signal is. One listed here
  stops being listed when it is fixed, when it is dismissed, or when it has been
  present on every run since it was raised and its own run has left the baseline
  window — at which point it is what this context does, and `history --signals`
  is where it still reads as `open`.
- `run -j` adds `not_recorded: {code, message}` when the store was unusable or
  busy. The exit code is still the wrapped command's.

**`streams` is the trap.** See below.

### `summary`

An object. Note the two levels of nesting.

```json
{
  "run": { "…": "the run object" },
  "behaviors_total": 47,
  "behaviors": [
    {
      "behavior": { "id": "3e0ff9b46496a578", "kind": "db.query", "template": "TRANSACTION SAVEPOINT active_record_<int>", "roles": [] },
      "stats": { "count": 55, "errors": 0, "duration": { "count": 55, "total_us": 0, "p50_us": 0, "p95_us": 0, "max_us": 0 } }
    }
  ]
}
```

A behavior's template is `.behaviors[].behavior.template`, not
`.behaviors[].template`; its count is `.behaviors[].stats.count`.
`behaviors_total` is the run's whole count — `behaviors` is limited by `-n`
(default 20), so **never take `behaviors | length` as the total.**

### `history`

A **bare array** of run objects, newest first, each with two extra fields:

```json
[
  {
    "id": "r5",
    "changes": 1,
    "signals": 1,
    "…": "the rest of the run object"
  }
]
```

`changes` counts code-level groups, `signals` counts individual signals — they
differ, because one change groups several signals. Limited by `-n` (default 20)
with no total field.

### `history --signals`

A **bare array**, and the element is *not* a signal — the signal is nested under
`signal`, with the outcome fields beside it. There is no top-level `kind`.

```json
[
  {
    "signal": { "…": "the signal object, including its kind" },
    "outcome": "resolved",
    "unknown_reason": null,
    "resolved_in": "r5",
    "recurred_in": null,
    "recurrences": 0,
    "later_runs": 1,
    "investigated": false,
    "dismissed": false,
    "feedback": [ { "…": "feedback objects" } ]
  }
]
```

`outcome` is `open`, `resolved`, `recurred` or `unknown`; when `unknown`,
`unknown_reason` says why (today's rules no longer reproduce it, or retention
pruned the runs the judgement needs, naming the setting).

**`outcome` is the latest verdict, and the two `*_in` fields are the first.**
A change can be fixed and come back more than once, and the three fields answer
different questions about that history:

- `outcome` is where the change stands as of the newest run that gave a verdict.
  One fixed, broken and fixed again reads `resolved`, not `recurred` —
  `recurred` means it is there now.
- `resolved_in` and `recurred_in` name only the **first** fix and the **first**
  return. They do not move as later cycles happen.
- `recurrences` counts every time the change came back after being resolved. It
  is the only field that grows with a second cycle.

So the fixed → broken → fixed → broken sequence below is `recurred` with
`recurrences: 2`, while `resolved_in` and `recurred_in` still point at the first
cycle. Fix it once more and `outcome` becomes `resolved` with `recurrences`
still 2:

```
$ siftr history --signals -j | jq '[.[] | select(.signal.id=="s1")] | .[0]
    | {outcome, resolved_in, recurred_in, recurrences, later_runs}'
{
  "outcome": "recurred",
  "resolved_in": "r6",
  "recurred_in": "r7",
  "recurrences": 2,
  "later_runs": 5
}
```

To ask "is this change there now", test `outcome == "open" or outcome ==
"recurred"`, or read a current run's `open_signals` — never `recurred_in`, which
is set for a change that has since been fixed.

To count by kind, read `.[].signal.kind` — `.[].kind` does not exist:

```
$ siftr history --signals -j | jq -r '.[].signal.kind' | sort | uniq -c
```

### `history --sources`

A **bare array**: what each run recorded reading, newest first.

Two runs of the same command in the same directory, `rails_log` switched off in
`.siftr.toml` between them:

```json
[
  {
    "run": "r2",
    "sources": [
      { "name": "rusage", "stream": null },
      { "name": "stderr", "stream": "stderr" },
      { "name": "stdout", "stream": "stdout" }
    ]
  },
  {
    "run": "r1",
    "sources": [
      { "name": "rails_log", "stream": "file:log/test.log" },
      { "name": "rusage", "stream": null },
      { "name": "stderr", "stream": "stderr" },
      { "name": "stdout", "stream": "stdout" }
    ]
  }
]
```

`name` is the source's configuration key, as `siftr sources` lists it, ordered by
name. `stream` is the spelling that joins it to an exemplar's `stream`, and is
null for a source that opens none (`rusage` reads the kernel's accounting, not a
file).

`sources: null` means **the run recorded none, so it cannot say what it read** —
not that it read nothing. `ingest` replays a capture rather than choosing
sources, so an ingested run is always null:

```json
[
  { "run": "r5", "sources": null },
  { "run": "r4", "sources": null }
]
```

### `explain`

An object.

```json
{
  "signal": { "…": "the signal object" },
  "rule": "identical in all 3 baseline runs, so any change counts; confidence (n+1)/(n+2) = 0.80",
  "runs": [ { "run": "r4", "value": 10.0 }, { "run": "r3", "value": 3.0 },
            { "run": "r2", "value": 3.0 },  { "run": "r1", "value": 3.0 } ],
  "scope": { "id": "872cda219ee77788", "kind": "test.example",
             "template": "./spec/requests/users_spec.rb # Users shows a user with posts and comments",
             "roles": [] },
  "scope_runs": [ { "run": "r4", "value": 10.0 }, { "run": "r3", "value": 3.0 },
                  { "run": "r2", "value": 3.0 },  { "run": "r1", "value": 3.0 } ],
  "evidence": { "run": "r4", "pruned": null, "exemplars": [ { "…": "exemplar objects" } ] },
  "group": ["s2", "s3", "s4"],
  "resources": null
}
```

`runs` is the measure's value in this run followed by each baseline run, newest
first; a null `value` means the behavior was absent from that run. `scope` is the
enclosing example's behavior and `scope_runs` the same series within it — both
null for a signal not attributable to one example.

**`group` lists the change's *other* members**, not including this signal: the
signal above is `s1`, and its group is `["s2", "s3", "s4"]`.

`evidence.run` is this run, or for a disappearance the latest baseline run that
had the behavior, or null when none did. `evidence.pruned` names the retention
setting that removed the lines, when that is why there are none. `resources`,
when present, is `{current, baseline}` of what the kernel charged the run — CPU,
peak RSS and context switches. It is evidence only; no rule reads it.

### `evidence`

An object. `captures` maps a stream to the run's capture file **only for captures
still on disk**, so it is `{}` under `SIFTR_CAPTURE=off` or after retention.

```json
{
  "behavior": { "…": "the behavior object" },
  "run": "r5",
  "stats": { "…": "the stats object" },
  "exemplars": [ { "…": "exemplar objects, limited by -n, default 8" } ],
  "captures": { "file:log/test.log": "/path/to/home/runs/r5/file-log_test.log" }
}
```

### `sources`

An object: what siftr *can* read here, before running anything.

```json
{
  "cwd": "/path/to/project",
  "command": "bin/rspec",
  "sources": [
    { "name": "rails_log", "stream": "file:log/test.log",
      "about": "the SQL and request lines the run appends to the Rails test log",
      "on": false, "applies": true, "why": "log/test.log is there" }
  ]
}
```

`command` is null when no command was given, and then the sources that depend on
the command cannot be judged (`applies: false`, `why: "no command given"`).

### `status`

An object describing the data directory: `home`, `database`
`{bytes, free_bytes, schema, supported_schema}` (null when there is no database
yet), `captures {bytes, runs}`, `runs {total, oldest, newest}` (null when empty),
`retention` with a `{env, value, source, given, why}` setting per limit,
`config {sources, files}`, `pending {stats, evidence}`, `commands_total`,
`commands` (limited by `-n`, default 5), `orphaned_captures` and `problems`.
`problems` is the list that makes it exit 1; each entry says what to do.

`config.sources` is keyed by source name, each `{value, source, path}` — this is
what configuration *says*. What a given run actually read is
`history --sources`.

### `ack` and `dismiss`

The feedback object that was recorded:

```json
{
  "kind": "acked",
  "at_ms": 1789623317851,
  "command": "ack",
  "interface": "json",
  "run": "r5",
  "behavior": "39b38737bf780ac5",
  "signal": "s5",
  "note": "looking at it"
}
```

`kind` is `surfaced`, `investigated`, `evidence_requested`, `dismissed` or
`acked`. `signal` is null when a behavior was named rather than a signal, as by
`evidence`.

### `gc` and `cron`

`gc` is an object: `dry_run`, `steps` `[{run, tier, by, project, context}]`,
`pending`, `capture_bytes`, `orphaned_captures`, `orphan_bytes`, and `database`
`{bytes, bytes_after, free_bytes, vacuumed}`.

`cron` is an object: `jobs` `[{source, status, detail, entries: [{schedule, user,
command, wrapped}]}]` and `evidence` `[{source, status, detail, lines}]`.

## Traps

These are the places where a reasonable guess is wrong.

**`streams` is null in `changes`, a list in `run` and `ingest`.** Same field,
same document type, two different things:

```
$ siftr run -j -- bin/rspec | jq .streams
[
  "stdout"
]

$ siftr changes r4 -j | jq .streams
null
```

Null means *this command cannot say*, not "no streams". `streams` is what
**arrived**: a stream opens on its first byte, so a stream that stayed empty is
not listed, and only the recording ever knows that. The store keeps what the run
**read** — every source it listened to, empty or not — which is a different set
and is `history --sources`. In the run above, `streams` is `["stdout"]` while
`history --sources` reports `stdout`, `stderr` and `rusage`: stderr was read and
produced nothing. So `changes` reports null rather than answering the question it
can answer under the name of the one it can't. For a run's provenance after the
fact, use `history --sources`.

**"What siftr can read" and "what a run read" are different questions.**
`siftr sources` is prospective and configuration-shaped; `history --sources` is
what the recording observed. They legitimately disagree — a source can be `on`
and `applies: true` and still feed a run nothing, in which case it appears in
`sources` but not in that run's recorded sources.

**Absent is not zero.** `sources: null`, `streams: null`, `duration: null`,
`attribution: null`, `database: null` and `unknown_reason` all mean "no answer
available", never "the answer is none".

**`-n` truncates silently in most commands.** `changes` and `summary` carry
totals beside their limited arrays (`groups_total`/`signals_total`,
`behaviors_total`), so a slice is always detectable — and `changes -n` has no
default, so its document is complete unless you asked otherwise. `history`,
`history --signals`, `history --sources` and `evidence` carry no total and do
default to a limit, so for those a limited array cannot be distinguished from a
complete one.

**Counting changes vs signals.** `changes` (the number) counts code-level
groups; `signals` counts signals. A single N+1 is one change and four signals.
Use `groups` to count changes, `signals` to count signals, and don't mix them.

**`-j` on `run` implies `-q`**, and cannot be combined with
`--quiet-unless-changed` or `--no-report` — `-j` always prints its document.
