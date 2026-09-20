# Signals: measured noise, decision rules, backtest

When does a behavioral change earn a signal, and how confident should siftr be?
Answered from measured run-to-run noise on two real suites, 2026-09-13, Ruby
3.4.9, an 8-core arm64 Mac. **Load average was 9–20 throughout** (other agents
were building and testing). That is a pessimistic machine, and also the one a
coding agent actually runs on.

The rules have since been extended: see §7 for what changed and what verified it.

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
run. The fix is to mask `_+\d+` as one slot. The normalizer needs a test for
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
| LATENCY (any timed behavior) | n ≥ 2; c is the behavior's mean duration per occurrence this run; Δ = c − median > need = max(**100ms**, **3·median**), i.e. ≥ +100ms **and** ≥ 4x; c > max(baseline); not an **adjacent** example (execution order; examples only) slowed by ≥ 0.5·Δ; not a **stall** (window excess − own excess > own excess and > 3·1.4826·MAD(window), all in total ms) | E(n)·e/(1+e), e = Δ/need |
| LATENCY (suite duration) | never standalone; supporting evidence only | — |

The **window** is the run's total time in the behavior's own kind of work — the suite for an
example, the run's request time for a request — never across kinds, since a query's time is
inside its request. Thresholds above were fitted to example timing; what they do to a second
population, and which guard carries over to it, is measured in `latency.md` (2026-09-19).

FREQUENCY measures: count per template per run, query count per request action
(Rails 8.1 `Completed … (N queries, M cached)`), and query total per example.
Only the log bytes between an example's `example_started` and finish offsets
are attributed to it.

Measures no rule reads: the `run.resources` behavior's CPU, peak RSS and
context switches (2026-09-15). §1's noise makes them a varying measure that
would leave its range on most runs, so they are excluded by kind rather than by
threshold — `signal::Comparison::class` returns `None`, and `judge` stops there
before any rule runs. They are kept as evidence and shown by `explain` beside
the baseline's, which is what tells a slow run from a loaded machine.

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

**Result (2026-09-20): the prediction fails, and the clause that fails is *at
any load*.** `noise-floor.md` §6 first exceeded it (`iriq` 6 of 50,
`network_resiliency` 26 of 50) and showed the 150ms remedy does not close it.
`latency-cohort.md` collected each suite twice in two machine states and found
the rate is a property of the machine rather than of the rule or the suite: the
same `network_resiliency` gives **19 of 50 at 1-minute load 14–127 and 0 of 50
at load 4.6**, and `rails_demo` — which met this budget — gives 7 of 50 loaded
and 0 of 50 quiet. Pooled over 264 comparisons of three suites, 1 false positive
below load 20 and 33 of 100 above it.

The floor was not raised. What shipped instead is a second stall guard on the
evidence a busy run actually leaves: an example LATENCY is vetoed when
`COHORT_PEERS = 3` other examples of the same run each moved by at least
`NEIGHBOUR_SHARE` of its slowdown. It is inert on a quiet machine — identical
signals and identical recall on both quiet corpora — and takes a loaded
`network_resiliency` from 13 of 18 comparisons to 3 on replays of identical
bytes. §2's thresholds and confidence formulas are untouched; §3's sweep and
recall tables stand. The prediction is re-registered per load regime in
`latency-cohort.md` §7.

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
| 31 | LATENCY | [10, 11, 12]; window [100, 105, 110] → 977, own share 872 | 120 | LATENCY 0.42 (the window moved by this behavior's own 8 occurrences; charging it one occurrence's 109 would veto it) |
| 30 | grouping | show request queries [3 ×5], example queries [28 ×5], sql `post_id = ?` [1 ×5] | 10, 35, 9 | 1 group, headline FREQUENCY request 3→10 exact 0.86, 2 supporting (example queries, sql template) |

## 7. Revisions since the backtest (2026-09-13)

§2's thresholds and confidence formulas stand. What changed is everything
around them, once real suites broke two assumptions the backtest never tested:
every run ran the whole suite, and one group per example suits every change.
Sections 1–6 are left as measured. Code: `src/baseline.rs`, `src/signal.rs`,
`src/bin/siftr/cmd/history.rs`.

**Baseline eligibility, v2** (3586139, replacing 2ac34c9). §3 only ever used
clean runs as a baseline. v1 dropped a recent run by counts: no test summary,
more errors outside examples than now, fewer examples run than loaded, or under
half of the examples loaded now. Suites that change shape defeated it. A suite
that grew past 2x lost its history, one that shrank read as a focus run, a red
`--fail-fast` suite skipped every run, and fixing a spec file that never loaded
skipped all earlier runs. Each hid a regression landing in that run.

v2 judges by which examples ran. A recent run with no test summary is skipped.
Any other recent run is skipped only when both hold:

- its own counts say it may have skipped existing examples: errors outside
  examples, fewer run than loaded, fewer loaded than defined, or no defined
  count (recorded by an older siftr);
- it lacks an example this run ran that another recent run also ran.

`defined` is the listener's count of examples in the files that loaded, before
filters. It is what separates a focus run (loaded < defined) from a deleted spec
file (fewer examples, none filtered). When every baseline run skipped existing
examples, as in a red `--fail-fast` suite, NEW on an example none of them ran is
dropped. Skipped runs are reported with a reason: `no_test_summary`,
`errors_outside_examples`, `stopped` or `subset`.

**INCOMPLETE, a signal kind** (171c740, 3586139). The same judgement is turned
on the current run. It is incomplete when it lacks examples its baseline ran and
its counts say why: more errors outside examples than any baseline run, stopped,
or filtered. Otherwise the missing examples were deleted. An incomplete run
keeps NEW and ERROR. It drops DISAPPEARED and FREQUENCY unless every occurrence
lies in an example that ran now, and drops LATENCY, since a partial suite can't
veto a stall. It adds one tier-1 group, on the behavior that shows why: each new
error outside examples (named `<file> failed to load: <class>: <message>`), else
the test summary. Confidence is E(n) from n ≥ 1, like ERROR. Before this, one
spec file that failed to load read as 9 changes.

**A new error outside examples is tier-1 NEW** (8557049). Errors the listener
reports outside every example (a `raise` after a describe block, a suite hook)
form their own class. Behaviors from the reporter's event stream used to get no
rules in a run with examples. Once v2 stopped calling such a run incomplete, an
exit-1 run reported 0 changes. Now it gets NEW at tier 1 with E(n), under the
usual presence rule (n ≥ 2), and FREQUENCY and DISAPPEARED under their usual
rules. When the error did leave the run incomplete, it is named once, as
INCOMPLETE.

**A DISAPPEARED example heads its group, and a file's collapse into one**
(dc2e2d6). Under §2's grouping the lowest tier headed the group, so SQL
attributed only to a deleted example headlined as tier-3 FREQUENCY. That ranked
the deletion beside real regressions and reminded it like one. Now a DISAPPEARED
example always heads its own group, because what was attributed to it moved when
it went. Two or more DISAPPEARED examples of one spec file (the part of the
template before ` # `) form one group with everything attributed to them. The
group ranks at its headline's tier, 4. A single one stays its own group.
Renderers print it as `N examples of <file>  gone` (JSON:
`groups[].disappeared_examples`). On the hunt fixture (`b_spec.rb` deleted, a
warning 1 → 3), changes went from 17 to 2, FREQUENCY first.

**Still-open reminders.** §5's baseline contamination is handled by re-judging
signals, not by excluding runs. A run reminds of signals from its own baseline
runs, oldest first, that are still open. A signal is still open when today's
rules reproduce it on its own run, and every later run through this one still
fires it against the signal's original baseline. A later run that skipped
examples that baseline ran gives no verdict, so a focus or load-error run can't
resolve a failure. Left out:

- signals this run raised again (same kind, behavior and measure), and repeats
  of one key;
- INCOMPLETE, which is about its own run;
- any group with a dismissed signal;
- any group headed by DISAPPEARED (dc2e2d6). A disappearance that stays is the
  new normal. A DISAPPEARED signal supporting another headline still rides with
  that reminder.

An interrupted, unfinished or incomplete run reminds of nothing. Reminders last
only as long as the window: once the signal's run leaves the baseline, the
change is what siftr calls normal.

**A count that only tracks the suite's size is tier 5** (this change). §2 gives
tier 5 to changes outside every example; a second population belongs there.
Development makes changes diffuse: adding examples moves whatever runs once in
each of them — rspec's transactional `BEGIN`/`ROLLBACK`, a query every example
makes — in lockstep with the example count.
`docs/findings/dogfood-junior-loop.md` watched a developer over eight runs, and
`TRANSACTION ROLLBACK TRANSACTION` headlined both runs that reported anything,
having gone 1 → 7 by adding exactly one occurrence to each of seven examples. No
example owns that, so attribution declines it and grouping cannot engage, which
makes every such signal its own headline.

A FREQUENCY on `count` is now demoted when all three hold: the suite's example
count changed, the behavior occurs inside at least two examples now, and the same
`rules::frequency` raises nothing on the count *per example*. Demotion, not
suppression — the count is real, and one that outgrew the suite (two rollbacks
per example where there was one) keeps its tier, as does one a single example
owns, which is a change there is somewhere to look.

**No threshold was added, by construction.** `rules::frequency` is invariant
under scaling every run's value by one factor: `min == max`, membership of
`[min, max]` and `distance / width` all survive it. So on a suite holding its
size the per-example rate would return the absolute rule's own verdict, and the
demotion is gated on the size having *moved* — making it inert wherever the suite
is constant rather than merely unlikely to fire. Verified by replaying all 19
committed fixture sequences before and after: every one is byte-identical,
including the N+1's single group headed by `GET UsersController#show 2xx`
queries 3 → 10. Of the sequences whose suite does change size, the two that grow
(4 → 10, 10 → 11) raise no FREQUENCY on a count at all, and the one that shrinks
(20 → 4, a warning 1 → 3) keeps tier 3 twice over: its rate moved 0.05 → 0.75,
and a stderr count carries no per-example attribution to call diffuse. The rule,
a rate that outgrew the suite and a change one example owns are pinned in
`src/signal/tests.rs`.

**What verified these.** `noise/analyze.rb` models §2 as backtested: clean
baselines, one group per example, no INCOMPLETE, no errors outside examples, no
collapse and no reminders. So §3's numbers say nothing about the revisions.
§3's runs all ran the whole suite, so eligibility v2 shouldn't change their
baselines, but the backtest wasn't re-run to confirm it. The revisions are
checked instead by fixture tests that replay captured RSpec runs through the
binary:

- `tests/core_hunt.rs`: grown, shrunk and red `--fail-fast`
  suites; fixing a file that never loaded; a deleted spec file as one change
  and never a reminder; focus and load-error runs resolving nothing.
- `core_reminders.rs`: a reminder keeps the DISAPPEARED query that supports its
  headline.
- `cli_collapse.rs`: the collapsed line and `disappeared_examples`.

`script/verify` runs them end to end: the full gate, then `dogfood/rails_demo`
through an N+1, an unfixed rerun, a load error, a raise after describe, and
recovery.

## 8. Persistent failures in generic logs: don't ship yet (2026-09-15)

A failure that is in every run is in every baseline, so no rule fires on it.
Dogfooding found one: backupd's `Snapshot deletion failed … Code=<int>`, in
every window, never signalled. Should siftr say "still failing"? Only if the
lines it would surface are mostly worth acting on.

**Corpus.** This Mac, read-only, one context per source, windows ingested as
successive runs. The four sources, runs and lines:

- backupd, every level, 6 hourly windows: 4,048 lines
- imagent and contactsd, every level, 2 minutes of each hour: 14,740 lines in
  6 runs, one of them empty
- every process at error or fault level only, 3 minutes of each hour: 14,046
  lines in 6 runs
- `/var/log/system.log`, 8 daily rotations: 13,622 lines

`log show` style was the default. The normalizer doesn't yet mask thread and
activity ids (`0x<hex>`), so the same message split per thread: 358 changes in
one chatty run, 35 with the ids masked. The corpus was measured with those ids
masked first, as that fix will do.

**Detection comes first.** `has_error_level` marked **0** of these 46k lines.
macOS writes the level as `Error`/`Fault` in the `Type` column (`E`/`F` in
compact style), and syslog writes none. So no persistent-failure rule has
anything to read until the interpreter parses `log show`'s level. The
measurement below takes the level from that column instead.

**Behaviors present in every run** (the chatty source counts its 5 non-empty
runs):

| source | persistent | error level | actionable |
|---|---|---|---|
| backupd | 8 | 5 | 2 |
| imagent + contactsd | 7 | 0 | 0 |
| every process, error/fault only | 15 | 15 | 0 |
| system.log | 16 | 0 (ASL configuration boilerplate) | 0 |
| **total** | 46 | **20** | **2 (10%)** |

The `log show` column header is left out of the counts. It was persistent in
every source until this revision dropped it.

A behavior was judged **actionable** only if its line names an operation that
failed and its owner could do something about it: a disk, a configuration or a
permission they control. It was judged **chatter** if the failure is expected
or internal. That covers a capability probe (`… failed: Operation not
supported`), a connection torn down because the client exited (logged at
Error beside a Default line saying so), an entitlement or sandbox denial
between system daemons, or a state message logged at Error. By that test the
two actionable behaviors are one failure: Time Machine's snapshot deletion,
on two volumes.

**What might tell them apart**, among the 20 error-level persistent behaviors:

| candidate rule | surfaced | actionable | precision |
|---|---|---|---|
| any error level | 20 | 2 | 10% |
| a failure word plus an error code (`failed … Code=`, `error: <int>`, `failed with error 0x…`) | 4 | 2 | 50% |
| count above every earlier run (volume grows) | 13 | 1 | 8% |
| NSError shape (`Error Domain=… Code=`) | 2 | 2 | 1 failure: fitted to the example |

Volume says nothing. The actionable failure was constant at 2 per window on
one volume, while chatter ranged from 1 to 151. A missing success line can't
be judged from a generic log: nothing says which line would be the success.

**Decision.** The bar was ≥ 90% of surfaced items actionable, over more than
one distinct failure. The best general rule reaches 50%, and the only rule
above it is fitted to the one example. So nothing ships: no "still failing"
line, no persistent-errors section. What would change the answer:

- logs whose level is the owner's own: an app's `production.log`, a cron job's
  output, a service's error log. The corpus here is all Apple daemons, whose
  Error level is mostly diagnostics;
- more than one real persistent failure across machines, so a rule can be
  checked against failures it wasn't fitted to;
- `dismiss`/`ack` feedback on persistent errors surfaced behind an opt-in
  context flag, which would label the data instead of judging it by hand.

Shipped from this: `log show`/`log stream` column headers (default, compact
and syslog styles) are no longer a behavior, in `interpret::generic`.
