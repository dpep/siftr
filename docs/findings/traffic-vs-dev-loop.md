# The same regression: invisible in the dev loop, headlined in traffic

Measured 2026-09-19 against 0.1.7 + `--no-report`. One Rails app, one N+1, two telemetry
sources, back to back. Companion to `dogfood-junior-loop.md`, and it changes what that
finding means.

## The experiment

A junior agent had built a posts index with comments, leaving 14 examples and eight runs of
baseline. The suite was then **stable** — no new specs. Dropping `.includes(:comments)` from
`PostsController#index` seeds a textbook N+1 against real baselines: precisely the case
siftr exists for.

The same regression was then measured a second way: eight identical HTTP requests against a
running development server, three times for baseline and once more with the N+1 in place,
slicing `log/development.log` per batch and replaying each slice with `ingest`.

## Result 1 — the dev loop misses it completely

```
r9 vs 6 baseline runs (r1…r8): no new changes · 22 still open
```

The `Comment Load` behavior went **6 → 19** and siftr said nothing. What it *did* print was
22 still-open reminders about transaction bookkeeping from the development phase, so the
regression was not merely missed, it was buried under stale noise.

### Why: the baseline blind window

The baseline runs are r1, r2, r3, r5, r7, r8. The behavior exists in **two of six**, because
comments were only added partway through development. That lands it between every rule:

| rule | requires | this behavior |
|---|---|---|
| NEW | absent from **all** baseline runs | present in 2 → declines |
| DISAPPEARED | present in **all** baseline runs | present in 2 → declines |
| FREQUENCY | a measure from **every** baseline run, else disqualified | missing in 4 → declines |

*"Intermittent is never news"* is right for flaky behavior. It also means:

> **A behavior introduced partway through the baseline window is immune to every rule until
> the window fills.**

Both halves of the N+1 signature fall in that gap — the new `WHERE post_id = ?` query and
the vanished `IN (…)` preload were each present in only 2 of 6 runs. Active development
*manufactures* this state for every behavior just written, so the blind window opens exactly
where new code is. It is a sibling of the known "a regression present before siftr's first
run is invisible" gap, and worse, because it is self-inflicted.

### Demonstrated, not inferred

Five text files and the binary reproduce it with no Rails app. Two behaviors, identical
spikes, differing only in baseline presence:

```
beta  "sometimes"  baseline [0, 0, 5, 5]  (2 of 4)  -> 50
gamma "always"     baseline [5, 5, 5, 5]  (4 of 4)  -> 50
```
```
r5 vs 4 baseline runs (r1…r4): 1 change · 1 still open
  s2   FREQUENCY   4 baseline runs  gamma always line  count 5 → 50
  still open: s1 (r3) NEW beta sometimes line  new: 5 now, in none of 2 baseline runs
```

`gamma` is reported; **`beta`'s identical 10× spike produces no signal at all.** The control
fires, so the machinery works — the silence comes purely from which runs the behavior
happened to appear in.

### A second defect: reminders freeze their numbers

The reminder above replays r3's signal, so the number reads `new: 5 now` while the behavior
sits at 50. Confirmed at an unmistakable magnitude with a sixth run at **500**: the line
still says `new: 5 now`, while `siftr summary r6` counts 500. The provenance is labelled
honestly (`s1 (r3)`), but the only line mentioning the behavior understates it 100×, which is
worse than saying nothing. **Ship a fix**: a still-open reminder should carry the behavior's
current number, not the one it had when the signal was raised.

## Result 2 — traffic gets it right, and ranks it right

```
r4 vs 3 baseline runs (r1 r2 r3): 4 changes
  s1  FREQUENCY   GET PostsController#index 2xx           queries 16 → 48
  s2  FREQUENCY   ↳ app/views/posts/index.html.erb:<int>  count 16 → 48
  s3  NEW         Comment Load … WHERE "post_id" = <int>  new: 40, in none of 3 baseline runs
  s4  DISAPPEARED Comment Load … WHERE "post_id" IN (…)   8 → 0
```

Rank 1 is the finding a developer wants; s3 names the offending query and s4 the preload that
vanished — the cause. Four signals, all true, all about one thing, zero noise.

Why traffic works where the dev loop doesn't:

1. **The workload repeats.** Identical requests per batch, so a behavior is present in all
   runs or none. No blind window.
2. **No test bookkeeping.** A real server has no transactional fixtures, so the
   `TRANSACTION BEGIN/ROLLBACK` behaviors that headlined both dev runs do not exist.
3. **The unit genuinely recurs.** A request is the same thing each time; a suite under
   development is a different thing each time.

The dev loop violates siftr's central assumption — that a context's behavior is stable
between runs — every time someone writes a spec. Traffic satisfies it by construction.

### The cycle closes

A fifth batch taken after the fix produced a slice byte-identical in size to the healthy
baselines, `0 changes`, and all four signals marked `resolved in r5 without investigation`.
Detect, rank, name the cause, confirm the repair.

The blind window cuts both ways even here: the N+1 query was in 1 of 4 baseline runs and the
preload in 3 of 4, so neither recovery could fire as its own signal. The resolution came from
the signals' own bookkeeping, not from a rule.

## Result 3 — but traffic latency is structurally invisible

A second regression through the same lane, on a fresh context: `sleep 0.4` in the action.

| | healthy | slow |
|---|---|---|
| the log says | `Completed 200 OK in 2ms` | `429ms`, `409ms`, `414ms` |
| siftr **measured** | P50 960µs, total 9ms | **P50 430ms, total 3.32s** |
| siftr **reported** | — | **`0 changes`** |

The duration is captured correctly and visible in `siftr summary`, and no rule can read it.
`Comparison::latency` builds its candidate set as

```rust
.filter(|b| b.behavior.kind == Kind::TestExample)
```

so **LATENCY is computed only for test examples.** An `http.request` behavior can never
produce one however slow it becomes. `siftr summary` will show 430ms while `siftr changes`
says nothing changed — a confident silence over data siftr already holds.

This sharpens rather than refutes Result 2. The count-shaped rules — NEW, DISAPPEARED,
FREQUENCY, ERROR — work on any behavior kind, which is why the N+1 came through. The one
duration rule is wired to rspec. For production traffic, latency is usually *the* question.

## Corroboration from someone who didn't know siftr existed

A junior agent was given the same regression as a vague ticket ("posts index feels slow"),
blind to siftr. It reached for the same evidence siftr computes, by hand, as its confirmation
step:

> log/development.log before the fix showed 4 separate `Comment Load … WHERE post_id = N`
> queries (one per post); after the fix it's a single `… WHERE post_id IN (5, 4, 3, 2, 1)`.
> Query count per request dropped from 6 to 2 regardless of post count.

That is exactly s3 and s4, and the same measurement as `queries 16 → 48` over eight requests.
Asked what it wished it had had:

> nothing here flagged the regression automatically; … would have caught this at commit time
> rather than needing a bug report first.

**Caveat, and it is the experimenter's:** the seeded commit message said "Drop the
eager-load", so the bug was found by `git log` in the first step and the developer never
floundered. Whether a *relayed* signal would have helped is still untested — the seed has to
be planted without confessing in its own commit message.

## Recommendations

1. **Point siftr at traffic, not at the inner dev loop.** **Ship as guidance now.** The dev
   loop is the worst case for every assumption siftr makes; recurring traffic is the best.

2. **Let LATENCY judge any behavior that carries a duration.** **Needs evidence**, but the
   gap is in the highest-value direction for the lane that works. The neighbour-veto and
   suite-stall guards exist because *test* timing is noisy; the traffic analogue is "did the
   whole window slow down", which is the same `Suite` idea computed over requests.
   Pre-registered check: the 400ms run above must raise a LATENCY signal on
   `GET PostsController#index 2xx`; `rails_demo`'s scenarios must not change.

3. **Scope log events to the enclosing request when there is no example.** **Needs
   evidence.** All four traffic signals carry `attribution: null` and land in four groups,
   each its own headline — correct findings, still shattered. `interpret/rails.rs` already
   tracks the open request. Pre-registered check: the traffic run must go from 4 groups to 1
   headed by `GET PostsController#index 2xx`; `rails_demo`'s N+1 must stay 1 group with its
   current headline; other fixture scenarios unchanged.

4. **Say when a behavior is in the blind window.** **Needs evidence.** A behavior present in
   some but not all baseline runs is silent in both directions and nothing says so. Even a
   count — "3 behaviors not yet comparable" — turns a confident silence into an honest one.

5. **Reminders must not crowd out new findings, and must not carry stale numbers.** 22
   still-open lines accompanied a run whose real news was a tripled query count.
