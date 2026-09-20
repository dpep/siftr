# LATENCY off the example path: one population, one threshold, one guard fewer

Measured 2026-09-19. `docs/findings/traffic-vs-dev-loop.md` Result 3 found that a 400ms
regression siftr had **measured** correctly — `GET PostsController#index 2xx` at P50 960µs →
430ms, total 9ms → 3.32s — was reported as `0 changes`, because `Comparison::latency` built its
candidate set from `Kind::TestExample` alone. This is what it took to let the rule read the rest.

The thresholds in `signals.md` §2 were fitted to RSpec example timing. Widening the rule puts them
in front of a second population, and the two guards around them were built out of facts about
examples. Both had to be answered with numbers rather than symmetry.

## 1. The second population is quieter than the first

The batches behind Result 3: eight identical `GET /posts` against a running dev server, sliced
from the Rails log per batch. Seven healthy batches (three of them recorded for the N+1
experiment, four for this one) and one with `sleep 0.4` in the controller action.

| batches | mean of the 8 requests, per run |
|---|---|
| healthy ×7 | 1.000, 1.125, 1.125, 1.125, 1.375, 2.000, 2.000 ms |
| `sleep 0.4` | 414.6 ms |

Across the healthy runs: **median 1.125ms, MAD 0.125ms → MAD/median 0.11, max/min 2.0x.**

`signals.md` §1 measured per-example noise at MAD/median 0.08–0.23 with a typical max/min of
2.4–4.0 in every speed bin. A request's per-run mean sits at the quiet end of that same range —
the same noise process, not a new one. Two things explain it: the requests are identical by
construction, and a per-run mean over 8 occurrences divides a one-off pause by 8, where an
example's duration is a single sample.

## 2. Thresholds: one pair, unchanged

`LATENCY_FLOOR_MS = 100` and `LATENCY_RATIO = 4.0` stand for every kind, because:

- the second population is **quieter** than the one they were fitted on, so they are conservative
  here rather than loose. The largest run-to-run move of a healthy mean is 2.000 − 1.000 =
  **0.875ms; the floor is 114x that**;
- seven runs of one endpoint cannot fit a threshold. §2's were fitted over 256 comparisons and
  swept on four axes. A second constant from this data would be a guess wearing a measurement's
  clothes — the mistake `docs/findings/confidence.md` already records;
- there is no measurement at all for `db.query` or log-line durations, which the widened rule also
  admits. A constant per kind would need one per kind.

The cost is the documented one. `signals.md` §3's recall table applies unchanged: a 10ms → 50ms
endpoint moves nothing, exactly as a 30ms → 90ms example doesn't. The regression it does catch:
414.6ms against a baseline median of 1.13ms, Δ 413.5ms, e = 4.13, **confidence 0.64**, one signal.

## 3. The adjacent-example veto does not generalize, and is not dropped

`NEIGHBOUR_SHARE` vetoes an example whose neighbour in execution order slowed by ≥ 0.5·Δ. It works
because of a fact about examples: they run **one at a time**, so the example before and after is
the same machine moments earlier *and* is causally unrelated to this one. Both halves are needed.

A behavior that recurs through a run has no adjacent occurrence in its aggregate. The tempting
analogue — "something else slowed at the same time" — fails the second half: the behaviors nearest
a slow request are its own queries and views, and those are what a **true** regression moves too.
The N+1 in this very corpus slowed a request and its SQL together. A veto built on causal
neighbours would suppress the finding it was meant to protect.

So the veto is scoped to examples, deliberately, and what stands in for it is arithmetic rather
than a second guard: the per-run mean divides a one-off pause by the occurrence count. The worst
single request in a healthy batch here is 9ms against a 1ms batch; for one stalled request to push
an 8-request mean past the +100ms floor it would have to carry **~800ms** of excess.

**Considered and rejected:** reading the sketch's P50 instead of the mean for recurring behaviors.
It would blunt a stall confined to a minority of occurrences, but not the case in §5, where the
stall covers the whole batch — and the stored P50 is rounded to two significant figures, so every
existing example signal would move. Not worth a special case that doesn't close the hole.

## 4. The stall guard carries over, with the unit fixed

"Did the whole window slow down" is the guard that does transfer. Two things had to change, and
one bug came out of it.

**The window is per kind of work.** The run's total time in the candidate's own kind — the suite
for an example (the reporter's own duration when it reported one, as before), the run's request
time for a request. Never across kinds: a query's time is *inside* its request, so charging the
request's move as "the rest of the run" would veto the query that caused it.

**It is charged in totals, not per occurrence.** For an example, count is 1 a run, so mean ==
total and the arithmetic is unchanged byte for byte. For a behavior occurring k times, a delta of
Δ per occurrence moves the window by k·Δ; charging it Δ leaves (k−1)·Δ looking like the rest of
the run stalling. t4 would have been vetoed by its own slowdown. `rules::Window` therefore carries
the candidate's own share of the window's move, and vector 31 pins it.

**The bug this surfaced:** the guard was silently inert off the example path already. The old
window was the test summary's duration, else the summed example time — both 0ms in a run with no
examples, so its median was `None` and the guard never ran. Any traffic run had *no* stall guard.
That is fixed here, not inherited.

## 5. What is still unguarded, stated plainly

In a window-wide stall the **largest** mover is not vetoed, because it is a majority of the
window's move by construction. Examples have the adjacent veto as a second net; off the example
path there is none, and §3 says why no honest analogue exists. Pinned as a test
(`signal::tests::a_recurring_behavior_is_vetoed_only_when_its_window_moved_without_it`): the
smaller mover is vetoed, the larger is reported.

For this to fire falsely, a stall must add ≥ 100ms to a behavior's *mean* — ~800ms of total time
over 8 occurrences — while adding less to every other behavior of its kind. That is possible where
a workload is batched per endpoint, as this corpus is.

**Pre-registered check**, in the shape of §3's: over the next 50 clean runs of a traffic context,
at most 2 comparisons carry a LATENCY signal on a non-example behavior. If that is exceeded, the
first move is §3's too — raise the floor to 150ms, which cost no true positive in the sweep.

## 6. False positives: the fixture replay

Every scenario of `fixtures/rails_demo` (11) and `fixtures/rspec_hunt` (13) replayed in sequence,
plus each rails_demo scenario against the three clean captures: **35 ingests**, each compared with
everything recorded before it. Each rails_demo run carries **16 behaviors with a duration, 12 of
them `db.query` or `http.request`** — judged for the first time by this change. The remaining four
are log lines on a reporter's stdout, which `Comparison::class` still declines.

Transcripts are **byte-identical** before and after, `slow`'s single example LATENCY at 311ms
included. Zero new signals, zero changed ones.

This is a weaker false-positive test than §3's backtest — it is a replay of fixed captures, not
256 independent comparisons — but it is the one that covers the newly admitted population.

## 7. What this exposed once log lines scope to their request

Measured after both changes landed, because the defect exists only at their intersection: neither
could produce it alone, and each passed its own acceptance checks.

With log lines scoped to the enclosing HTTP request, a run carrying two regressions on one endpoint
reports **two headlines**:

```
r4 vs 3 baseline runs (r1 r2 r3): 2 changes
  s1   FREQUENCY   GET PostsController#index 2xx  queries 16 → 48
       supporting: the view line, the new query, the preload that vanished
  s5   LATENCY     GET PostsController#index 2xx  2ms → 414ms
```

Five signals in two groups. Four collapse correctly under the request, each carrying
`phase: request`. The fifth is a LATENCY on **the same behavior**, and it carries
`attribution: null` and stands alone.

Corpus: three healthy batches of eight identical requests against a running server, then one batch
with the eager-load removed **and** a 0.4s sleep in the action. Local, not committed.

**Mechanism.** `Comparison::owners` asks `scoped_value` for a scope's own value, and for any measure
other than `count` that reads the per-scope **sums**. `Aggregator` records `event.measures` into
scope cells while `event.duration` goes to the histogram, so `scoped_value(…, "duration_ms")` is
`0.0` in every run. The endpoint scope fails the `median != now` filter, `owners` comes back empty,
and the signal falls to `Key::Behavior(id)`.

Neither signal is wrong: both are true and both rank tier 2. It is a readability defect — one cause,
two headlines — and it is the kind that only an integration test of two changes can find.

**Recommendation. Needs evidence.** Let a signal on an `http.request` behavior key on its own
endpoint scope when `owners` yields nothing. Pre-registered check: the two-regression run above must
go from 2 groups to 1, headed by the request with the latency as a member; a run carrying only the
N+1 must stay 1 group; a run carrying only the slowdown must stay 1 signal; and `rails_demo`'s N+1
must stay 1 group with its example attribution intact.
