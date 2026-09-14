# Signals: measured noise, decision rules, backtest

When does a behavioral change earn a signal, and how confident should siftr be?
Answered from measured run-to-run noise on two real suites, 2026-09-13, Ruby
3.4.9, an 8-core arm64 Mac. **Load average was 9–20 throughout** (other agents
were building and testing). That is a pessimistic machine, and also the one a
coding agent actually runs on.

Reproduce: `noise/collect.sh <raw_dir>` (interleaves demo baseline, toggles and
iriq runs so load drift hits all scenarios alike), `noise/extract.rb <raw_dir>`
(reduces to `noise/data/`), `noise/analyze.rb [noise|sweep|backtest|inject]`.
The cold-DB runs moved `db/test.sqlite3` aside before each run and restored it
afterwards. Random-order runs are the `demorand` suite (`--order rand`).

## 1. Noise

Runs: demo 25 clean, 5 per toggle, 8 random-order, 2 cold-DB, 12 hold-out.
iriq (934 examples, no DB) 13 clean, 6 hold-out.

**Suite duration.** demo: median 105ms, MAD/median 0.15, range 68–707ms (**10x**).
iriq: median 11.3s, MAD/median 0.21, range 8.3–16.3s (2.0x). The 1-minute load
average does not predict it (r = −0.09 demo, 0.29 iriq), so it can't gate
anything.

**Per-example run_time**, by the example's median (max/min is over the clean
runs; excess is max − median):

| suite | median bin | examples | max/min p50 | max/min worst | MAD/med p50 | excess p50 | excess worst |
|---|---|---|---|---|---|---|---|
| demo | <1ms | 4 | 4.0 | 30 | 0.12 | 0.9ms | 21ms |
| demo | 1–10ms | 3 | 3.9 | 13 | 0.15 | 11ms | 35ms |
| demo | 10–100ms | 3 | 13 | 18 | 0.23 | 220ms | 230ms |
| iriq | <1ms | 836 | 2.4 | 280 | 0.08 | 0.05ms | 72ms |
| iriq | 1–10ms | 57 | 2.8 | 14 | 0.09 | 2.7ms | 29ms |
| iriq | 10–100ms | 11 | 2.2 | 4.2 | 0.15 | 20ms | 57ms |
| iriq | ≥100ms | 30 | 2.4 | 7.7 | 0.13 | 260ms | 2300ms |

Relative noise does **not** shrink for slower examples: MAD/median stays at
0.08–0.23 and the typical max/min at 2–4x in every bin. Only the extreme ratio
shrinks, because fast examples are hit by absolute pauses of up to about 70ms
(GC, scheduler). A 1ms example can read 30x or 280x. Slow examples, like iriq's
subprocess-spawning CLI tests, swing about 2.4x, and by up to 2.3s.

Noise is **correlated in time**. The worst demo run stalled for about 600ms,
which put +100ms, +220ms and +230ms on three consecutive examples. Independent
per-example thresholds would fire three times on that run.

**Counts are exactly deterministic.** All 24 count behaviors in the demo (SQL
template counts, per-request `Completed (N queries)`, per-example query totals)
were identical in all 25 clean runs, within each toggle's 5 runs, and in all 8
random-order runs. Zero behaviors varied. The exception is a cold DB: 10 SQL
behaviors changed, all before the first example (schema load,
`ar_internal_metadata`, sqlite table-copy `INSERT INTO "acomments"`). The next
warm run was identical again.

**A masking hole.** Rails' compiled-view method names carry a per-boot
`String#hash`, with `-` rewritten to `_`:
`_app_views_users_show_html_erb__3584299121442748424_5232` vs
`…erb___129572542000730357_5232`. A digits-only mask produced **3 templates for
1 warning** across 5 runs, which would mean a NEW and a DISAPPEARED on every
run. The fix is to mask `_+\d+` as one slot. `siftr-normalize` needs a test for
this.

## 2. Rules

`n` is the number of baseline runs (the last N ≤ 10 of the context). Evidence
from baseline size uses the rule of succession: after `n` runs without an
event, P(event) ≈ 1/(n+2), so **E(n) = (n+1)/(n+2)** (0.75, 0.80, 0.86 and 0.92
for n = 2, 3, 5 and 10). Confidence means "probability this isn't noise",
rounded to 2 significant figures where it is built. Magnitude feeds ranking,
not confidence, except for LATENCY, whose noise is continuous.

| kind | fires when | confidence |
|---|---|---|
| ERROR | example failed now; ≥1 baseline run has it. Suppressed if a baseline run failed with the **same** exception class (known flaky) | 1 − (j+1)/(n+2), j = baseline failures |
| NEW | present now, absent in all n ≥ 2 baseline runs | E(n) |
| DISAPPEARED | absent now, present in all n ≥ 2 | E(n) |
| (intermittent) | present in 0 < k < n baseline runs: **never** NEW/DISAPPEARED; show k/n as context | — |
| FREQUENCY exact | measure identical in all n ≥ 2 runs, any change | E(n) |
| FREQUENCY varying | c outside [min, max] **and** \|c − median\| > 2·(max − min) | E(n)·(1 − (max − min)/\|c − median\|) |
| LATENCY (example) | n ≥ 2; Δ = c − median > need = max(**100ms**, **3·median**), i.e. ≥ +100ms **and** ≥ 4x; c > max(baseline); not an **adjacent** example (execution order) slowed by ≥ 0.5·Δ; not a **stall** (suite excess − Δ > Δ and > 3·1.4826·MAD(suite)) | E(n)·e/(1+e), e = Δ/need |
| LATENCY (suite duration) | never standalone; supporting evidence only | — |

FREQUENCY measures: count per template per run, query count per request action
(Rails 8.1 `Completed … (N queries, M cached)`), and query total per example.
Only the log bytes between an example's `example_started` and finish offsets
are attributed to it.

**Ranking.** Tiers, lowest first: 1 ERROR; 2 FREQUENCY on a request's query
count, NEW stderr line, LATENCY on an example; 3 FREQUENCY on an SQL template,
an example's query total, or a stderr count; 4 NEW/DISAPPEARED of an SQL
template or an example; 5 setup-only changes and suite duration. Ties break on
confidence. The top 3 groups are shown.

**Grouping.** A signal belongs to the example whose own count moved against the
baseline median (from its per-example attribution), or to its own example. One
group per example. The headline is the member with the lowest tier and then
the highest confidence. The others are supporting evidence. Suite duration
attaches to the top group. Stderr has no per-example offsets, so its signals
group by message prefix, a heuristic until the normalizer exposes slots.
Changes seen only before the first example form one tier-5 "setup changed"
group.

## 3. Backtest (leave-one-out)

Each run is "current". Its baseline is the n clean runs nearest before it in
time, or after it when too few precede. n ∈ {2, 3, 5, 10}.

**False positives: 0 signals in 256 clean comparisons.** That covers demo 100,
iriq 52 (×934 examples, about 49k example-latency tests), random order 24,
after-cold 8, and hold-out 72 (demo 48, iriq 24). The hold-out runs were not
used for tuning, but they ran at a lower load (5.6–6.5), so they are weak
out-of-sample evidence. 0/184 in-sample puts the 95% upper bound at about 1.6%
per comparison.

| toggle | per n | headline (group #1) | supporting |
|---|---|---|---|
| n_plus_one | 5/5 each n (random order 2/2) | FREQUENCY `UsersController#show` queries 3→10, conf 0.75/0.80/0.86/0.92 | FREQUENCY sql `Comment Load … "post_id" = ?` 1→9; FREQUENCY example "Users shows a user…" queries 28→35; DISAPPEARED sql `Comment Load … IN (…)`. No latency: +7 queries cost under 1ms |
| slow | 5/5 each n | LATENCY "Post summarizes the body" ~1ms→303–315ms, conf 0.56–0.57 (n=2), 0.60–0.61 (3), 0.64–0.65 (5), 0.69–0.70 (10) | none |
| warn | 5/5 | NEW stderr `DEPRECATION WARNING: User#display_name…`, conf E(n) | NEW stderr, same message from the view call site |
| fail | 5/5 | ERROR "User requires an email" `ExpectationNotMetError`, conf E(n) | none |
| cold DB (environment, not code) | 2/2 fire | one tier-5 setup group of 10 behaviors (NEW/FREQUENCY) | — |

Each regression produced exactly one group, and the headline was correct in
every comparison.

**The LATENCY trade-off is sharp.** Threshold sweep, FP = comparisons with any
example LATENCY signal on clean runs (of 176):

| floor | ratio | isolate | stall | FP | slow TP |
|---|---|---|---|---|---|
| 100ms | 4x | yes | yes | **0** | 20/20 |
| 150ms | 4x | yes | — | 0 | 20/20 |
| 100ms | 4x | yes | — | 4 (all from the one demo stall run) | 20/20 |
| 100ms | 4x | — | yes | 7 | 20/20 |
| 100ms | 3x | yes | yes | 4 | 20/20 |
| 50ms | 4x | yes | yes | 4 | 20/20 |
| 50ms | 2x | — | — | 30 | 20/20 |

The zero-FP rule sits one step from 4–7 FPs on every axis. These thresholds
were fitted to this data, so treat them as a floor.

Recall, for a synthetic slowdown of one example (every example of every clean
run, n=5):

| +Δ | median <100ms | median ≥100ms |
|---|---|---|
| +50ms | 0.00 | 0.00 |
| +100ms | ~0.4 (at the floor) | 0.00 |
| +300ms | 0.96–1.0 | 0.02 |
| +1000ms | 1.0 | 0.70 |

LATENCY therefore catches large regressions of a single example (≥ +100ms and
≥ 4x) and misses a 30→90ms change or +50% on a 1s test. That is the price of
precision at this noise level.

**Pre-registered prediction.** Over the next 50 clean local runs of each suite,
at any load, at most 2 comparisons carry an example LATENCY signal. If that is
exceeded, raise the floor to 150ms (zero FP in the sweep, same slow TP) before
anything else.

## 4. Don't ship yet

- **Suite-duration LATENCY.** Clean runs spanned 10x (demo) and 2x (iriq). It
  fired 3–5 FPs in every sweep config. Keep it as supporting evidence only.
- **Per-query latency** (the `(0.1ms)` slot). It has 0.1ms resolution and a
  0.0–0.3ms range, far below any defensible floor.
- **DISTRIBUTION CHANGE / DRIFT.** With 2–10 runs, MAD/median of 0.1–0.2 and
  single-sample tails of 30–280x, there is nothing to estimate a distribution
  shape from. Revisit with ≥ 30 runs per context, and only for examples ≥ 100ms.
- **CORRELATED CHANGE as a kind.** Implement it as the grouping above, not as
  another signal.
- **Setup-tier signals as regressions.** A cold DB fired 20 of them. Render
  them as one "environment changed (before first example)" line, never in the
  top 3 while a code-level group exists.

## 5. Risks not covered by this data

- **Baseline contamination.** A regression run stays in the last N. One N+1 run
  turns `[3,3,3,3]` into `[3,3,3,10]`, a varying measure, so a later return to
  10 is silent. Consider excluding runs whose signals were shown, or baselining
  on the most recent streak of exact values.
- **Subset runs** (`rspec spec/models`, `--only-failures`, `fit`) are a
  different context only if the normalized command includes those arguments.
  A focus filter does not show up in the command. If more than half of a run's
  examples are DISAPPEARED, render it as a subset run, not as signals.
- **Other apps' lazy queries** (schema cache, `PRAGMA`) would make the first
  example that touches a model differ under random order. The demo logged none.
  Verify on a larger Rails app before trusting exact per-example counts there.
- **Confidence calibration** is untested beyond ordering. One latency toggle
  can't calibrate e/(1+e).

## 6. Test vectors

Computed by the rule functions in `noise/analyze.rb` with the thresholds above.
Latency values are in ms. "n/a" means no signal.

| # | kind | baseline | current | expected |
|---|---|---|---|---|
| 1 | LATENCY | [1.0, 1.1, 0.9] | 304 | LATENCY 0.60 |
| 2 | LATENCY | [1.0, 1.1] | 304 | LATENCY 0.56 |
| 3 | LATENCY | [1.0] | 304 | n/a (n < 2) |
| 4 | LATENCY | [1.0 ×10] | 304 | LATENCY 0.69 |
| 5 | LATENCY | [1.0, 1.1, 0.9] | 150 | LATENCY 0.48 |
| 6 | LATENCY | [20, 22, 250] | 240 | n/a (≤ baseline max) |
| 7 | LATENCY | [50, 55, 60] | 180 | n/a (need 165 = 3·median) |
| 8 | LATENCY | [50, 55, 60] | 260 | LATENCY 0.44 |
| 9 | LATENCY | [1000, 1100, 900] | 2500 | n/a (need 3000) |
| 10 | LATENCY | [1000, 1100, 900] | 4200 | LATENCY 0.41 |
| 11 | LATENCY | [1.0, 1.1, 0.9]; next example median 1 → 201 | 304 | n/a (adjacent +200 ≥ 0.5·303) |
| 12 | LATENCY | [10, 11, 12]; suite [100, 105, 110] → 707 | 120 | n/a (stall: rest 493 > 109 and > 22) |
| 13 | LATENCY | [10, 11, 12]; suite [100, 105, 110] → 215 | 120 | LATENCY 0.42 |
| 14 | FREQUENCY | [3, 3, 3] | 10 | FREQUENCY exact 0.80 |
| 15 | FREQUENCY | [3, 3, 3, 3, 3] | 2 | FREQUENCY exact 0.86 |
| 16 | FREQUENCY | [3, 3] | 3 | n/a |
| 17 | FREQUENCY | [3] | 10 | n/a (n < 2) |
| 18 | FREQUENCY | [10, 12, 11] | 13 | n/a (\|2\| ≤ 2·2) |
| 19 | FREQUENCY | [10, 12, 11] | 20 | FREQUENCY varying 0.62 |
| 20 | NEW | present in 0/5 | present | NEW 0.86 |
| 21 | NEW | present in 2/5 | present | n/a (intermittent) |
| 22 | NEW | present in 0/1 | present | n/a (n < 2) |
| 23 | DISAPPEARED | present in 5/5 | absent | DISAPPEARED 0.86 |
| 24 | DISAPPEARED | present in 4/5 | absent | n/a (intermittent) |
| 25 | ERROR | passed ×5 | failed E | ERROR 0.86 |
| 26 | ERROR | passed ×1 | failed E | ERROR 0.67 |
| 27 | ERROR | failed E ×1, passed ×4 | failed E | n/a (known flaky) |
| 28 | ERROR | failed F ×1, passed ×4 | failed E | ERROR 0.71 |
| 29 | ERROR | none | failed E | n/a (no baseline) |
| 30 | grouping | show request queries [3 ×5], example queries [28 ×5], sql `post_id = ?` [1 ×5] | 10, 35, 9 | 1 group, headline FREQUENCY request 3→10 exact 0.86, 2 supporting (example queries, sql template) |
