# Per-source partitioning does not reach a Rails log yet

Measured 2026-09-20 against 0.1.7 (`60e9025`). `log-contexts.md` §8 makes two
recommendations for log input, both measured on a macOS unified log partitioned by the
syslog `host proc[pid]` header. A Rails log has no such header, so this asks whether the
recommendations transfer to the logs siftr is aiming at next, and what would play the part
of `proc[pid]` there.

**The effect question is unanswered: the corpus to answer it is not on this machine.**
What is answered, and answered against expectation, is the question underneath it: on a
Rails log in its **default production format**, siftr today recognises no requests and no
SQL at all. Partitioning by endpoint presumes a parse that does not happen.

## 1. The corpus is not here

`log-contexts.md` §6 needs a log wide enough to flood — three disjoint windows of ~194k
lines, from a process mix wide enough that partitioning has something to separate. Searched
for one:

| | |
|---|---|
| Rails app logs in checkouts on this machine, outside repositories declared off limits | 9 |
| largest, by lines | 14,859 — and it carries **2** `Started` request lines: a schema load, not traffic |
| largest by request volume | 1,876 lines, 80 requests |
| next three, by request volume | 44 · 39 · 32 requests |
| logs with more than 100 requests | 0 |
| `dogfood/rails_demo/log/test.log`, accumulated over many dogfood runs | 183,213 lines |

The two repositories that would plausibly hold a production-scale Rails log are off limits
for this work, and the 556k-line Rails log `CLAUDE.md` cites is not reachable from here.

The demo log is the only Rails corpus at volume, and it cannot answer this question:
§7b established that **width** — how many sources a context mixes — is the variable that
decides whether partitioning is needed, and a demo app with a handful of controllers is
narrow by construction. Measuring partitioning on it would measure the fixture.

**So recommendations 1 and 2 are untested on Rails, and nothing below claims otherwise.**

## 2. What could play the part of `host proc[pid]`

The syslog key works because it is **lexically present on every line**, **stable across
runs and machines**, and **low cardinality** — 291 sources for a whole laptop, 28 for
`install.log`. Rails' candidates, against those three properties:

| candidate | on the line? | stable? | cardinality |
|---|---|---|---|
| request id (`log_tags`) | yes, by default in production | no — one per request | = traffic |
| severity | only under `Logger::Formatter` | yes | 3–5, and one level carries nearly all lines |
| pid / worker | only if `log_tags` names it | no — new every deploy and restart | = workers |
| `controller#action` | no — derived from `Processing by` | yes | ~10³ (below) |
| controller | no — derived | yes | ~10² (below) |

Only the last two have all the stability a context needs, and **neither is on the line**:
both are derived from a `Processing by` line and then carried forward to the lines that
follow it. That is a real difference from syslog, not a detail. It means the partition
depends on grouping a request's lines correctly, which in a threaded production server
(Puma's default is multi-threaded) cannot be done by block order — concurrent requests
interleave. It requires the request-id tag, i.e. exactly the field §2's table rules out as
a *key* is required as the *grouping*.

Cardinality, bounded on a public app rather than guessed — **discourse** (`config/routes.rb`,
`app/controllers/`), an upper bound on what its log could name, since not every route is hit:

| | |
|---|---|
| route-declaring lines (`get`/`post`/…/`resources`) | 895, and `resources` expands to up to 7 actions each |
| top-level controller files | 90 |

So a per-`controller#action` partition of one real app's log creates contexts on the order
of a thousand — **finer than the whole-machine partition that §6 measured**, where the
benefit came from 8 serviceable sources. Each context would then carry a fraction of the
traffic and need its own baseline of runs before any rule fires. The controller is the
closer analogue of a program name: ~10², coarse enough to accumulate baseline, stable
across deploys. **The action is a scope, not a source** — which is how siftr already treats
it, as the scope of a request block's lines.

## 3. The mechanism isn't reachable from a production-format log

Before any of that can be measured, the parse has to happen. Rails 7.1 and 8.1 both
generate a production environment that sets:

```ruby
config.log_tags = [ :request_id ]          # every line prefixed "[<uuid>] "
config.log_level = ENV.fetch("RAILS_LOG_LEVEL", "info")
```

and ActiveRecord's log subscriber emits SQL at **debug**, so a default production log has
no SQL lines in it at all. `src/interpret/rails.rs` anchors every shape at the start of the
line (`strip_prefix(b"Started ")`, `b"Processing by "`, `b"Completed "`; a query's name must
be alphanumeric up to its `(`), so a tag defeats all four.

Measured on the committed `fixtures/rails_demo/baseline/test.log` (211 lines, 2 request
blocks), rewritten into each format a Rails app can emit — same bytes, plus what the
production defaults do to them:

| variant | lines | behaviors | `http.request` | `db.query` | `log` |
|---|---|---|---|---|---|
| as captured (test env: untagged, debug) | 211 | 26 | **2** | 17 (196 events) | 7 (9) |
| + request-id tag | 211 | 38 | **0** | 14 (114) | 24 (97) |
| info level only, untagged | 15 | 9 | **2** | 0 | 7 (9) |
| **production default** (info + tagged) | 15 | 13 | **0** | 0 | 13 (15) |
| as captured, ingested as stdout | 211 | 30 | **0** | 0 | 30 (211) |

Three things fall out.

**The tag is not partial damage.** Of the fixture's 196 SQL-shaped lines, 114 precede the
first request (a test log's schema and fixture loading, untagged even in production) and 82
sit inside request blocks. The tagged variant keeps exactly the 114 and loses exactly the
82: **every tagged line, 82 of 82, stops being a query** and falls through to a generic
`log` behavior. Behaviors go *up*, 26 → 38, because shapes that used to key on query name
and statement now key on whatever the masker makes of the whole line.

**Production silences the part siftr reasons about.** The info-level subset is 15 of 211
lines — 93% of this log is debug-level, all of it the SQL and partial renders that the
`db.query` kind, the N+1 finding, and `latency.md` are built on. (The level filter is a
regex proxy for Rails' own dispatch, so treat 93% as illustrative of a test log's shape;
the zero in the `db.query` column is categorical and does not depend on it.)

**The ingest path never reaches the Rails interpreter anyway.** `rails::Log` is reachable
only from the rspec interpreter, gated on `Stream::File("log/test.log")`. `siftr ingest FILE`
reads the file as `Stream::Stdout`: the last row is the *untagged, debug-level* fixture,
and it still yields 0 requests and 0 queries. And `siftr sources -- bin/rails server`, run
inside the demo Rails app, reports `rails_log  on  does not apply … (the command isn't a
Ruby test run)`, consistent with `source-detection.md`.

So there is no path today by which a production Rails log becomes anything but `log`
behaviors — which is the same state `dogfood-system-logs.md` found the unified log in, and
for the same reason: no interpreter claims it.

## 4. Pre-registered check, for when a corpus exists

Stated now so that the lane that finds a corpus runs it rather than redesigns it. Given a
real web-tier Rails log of ≥ 400k lines from an app with more than ~20 endpoints, split
into three disjoint chronological windows and ingested as three runs of one context
(`log-contexts.md` §9's method):

1. **Width.** The unpartitioned log's third window exceeds the 1,000-signal refusal. *If it
   does not, Rails is already narrow — §7b's `install.log` outcome — and recommendation 1
   is moot for this source rather than confirmed.*
2. **Attributability.** ≥ 60% of lines fall inside a request block that named a
   `controller#action` — the analogue of §7's 67%. Report the residue separately; it is
   boot, jobs, mailers and background threads, and §7's open question lands on it again.
3. **Effect.** Partitioned by controller, each of the top eight by volume stays under the
   refusal threshold, as seven of eight did in §6.
4. **Complementarity.** Any partition still over the line loses ≥ 80% of its NEW to
   one-off suppression, as S5 did in §5.
5. **Interleaving.** Attribution by block order and attribution by request-id tag agree on
   ≥ 99% of lines, as §7's two syslog forms agreed on 12,928 of 12,928. *A log from a
   threaded server will fail this, and that failure is the measurement: it says the tag is
   load-bearing, not optional.*

Fail 1 and the recommendation is unnecessary here; fail 2 or 3 and it does not transfer as
stated; fail 5 and it transfers only for logs carrying request ids.

## 5. Verdicts

- **Recommendation 1 (per-source, not per-file): untested on Rails, and its premise is
  weaker here than on syslog.** The key is derived rather than lexical, it needs the
  request-id tag to survive concurrency, and the obvious key (`controller#action`) is an
  order of magnitude finer than the partition §6 measured. The controller is the better
  candidate. None of this contradicts §6 — it says §6's mechanism is not transferable as a
  mechanism, only as a principle.
- **Recommendation 2 (never-recurring behaviors raise no NEW): untouched by this, and its
  case is if anything stronger.** It keys on recurrence across runs, not on any header, so
  it transfers to any source, including the lines §7 could not attribute and the residue in
  check 2 above.
- **Neither can ship for production Rails logs on the strength of a Rails measurement,
  because no such measurement is possible here yet.**

## 6. What going further needs

1. **A corpus.** ≥ 400k lines of one app's web-tier log, more than ~20 endpoints, ideally
   both a tagged production slice and an untagged one of the same traffic (check 5 needs
   both). A staging tier is enough. Failing access to one, a load generator against a real
   OSS app — discourse is already checked out here — replayed at its production log
   settings, which buys checks 1–4 but makes the traffic mix synthetic and so cannot settle
   width.
2. **A parse, before the partition.** §3 is a code finding, not a corpus finding, and it
   is cheap: tolerate a leading `[tag]…` sequence (and a `Logger::Formatter` prefix) before
   each anchored shape, and make the Rails interpreter reachable from a stream other than
   `log/test.log`. Not attempted here — this lane does not touch `src/`.
3. **A decision about what a production run even is.** Contexts are (project root,
   normalized command) and baselines are recent runs of one; a long-lived server has no
   runs. Whatever slicing answers that — per deploy, per hour — is upstream of partitioning,
   and it is not written down anywhere yet.

## 7. Reproducing

`docs/findings/rails-partition/probe.sh [SCRATCH]` regenerates §3's table from the committed
fixture and a release build, into a scratch `SIFTR_HOME`; `variants.py` is the rewrite into
each production format, and documents the level filter's approximation. The route and
controller counts in §2 are `grep -cE` and `ls | wc -l` over a public discourse checkout.
§1's search covered `*.log` files on this machine outside the off-limits repositories; no
log content, path or host from any of them appears above.
