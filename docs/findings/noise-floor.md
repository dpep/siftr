# The false-positive floor: what siftr says when nothing changed

Measured 2026-09-19 at `6715345`, Ruby 3.4.9, an 8-core arm64 Mac shared with
other agents (1-minute load 5–84 across the collection). "Precision over recall"
is bounded by one number — how many signals a clean run earns — and until now
the only measurement of it was `signals.md` §3's backtest, which ran on the same
runs the thresholds were fitted to.

**Headline: the floor is not zero on a real suite.** Traffic is clean (0 of 158
comparisons). An rspec context is not: 6 of 50 comparisons on one real suite,
50 of 50 on another. Two distinct generators are behind it, and only one of them
is about timing.

Reproduce: `noise-floor/traffic.sh` and `noise-floor/suite.sh` collect,
`noise-floor/replay.sh` and `noise-floor/fixed_n.sh` compare,
`noise-floor/tally.rb`, `exposure.rb` and `headroom.rb` reduce.

## 1. What a floor cannot be measured on

Two corpora in this repo will return zero whatever the rules do, and a zero from
either would be a lie assembled from true numbers.

**A replayed fixture.** `fixtures/rails_demo/baseline` ingested 52 times in a row:
**0 signals in 50 comparisons**. The bytes are identical, so the aggregates are
identical, so nothing can move. This measures that replay is deterministic,
which `latency.md` §6 already established. It is in the table below only as the
control that proves the harness would report zero correctly.

**A ten-example suite.** `dogfood/rails_demo` has 10 examples and 48 behaviors.
It is small enough that a stall has almost nothing to hit, and its 2 of 50 below
was collected at load 13–16, after the other collections had finished and the
machine had quieted. It is a floor for a demo, not for a suite.

So every number below that matters comes from **real repeated execution**: a
Rails stack dispatching the same requests over and over, or a real suite run 52
times.

## 2. The corpora

| corpus | what actually varies | runs | comparisons | load |
|---|---|---|---|---|
| **traffic, spaced** | 52 batches of 16 identical requests through the dogfood dev stack, 20s apart over 17 minutes | 52 | 50 | 5–84 |
| **traffic, dense** | the same 52 batches back to back, 3.6s end to end | 52 | 50 | 19–20 |
| **traffic, captured** | 10 clean batches against a running HTTP server, captured by another lane on another day | 10 | 8 | — |
| **iriq** | a real 1008-example suite, 1002 example behaviors, no DB, under `siftr run` | 52 | 50 | 5–81 |
| **network_resiliency** | a real 378-example suite, 352 example behaviors, heavy on sleeps and mock servers | 52 | 50 | 19–83 |
| **rails_demo** | the 10-example dogfood suite | 52 | 50 | 13–16 |
| *fixture replay* | *nothing — control* | 52 | 50 | — |

A comparison counts only once siftr holds n ≥ 2 baseline runs, since every rule
declines below that; the first two runs of each corpus are excluded rather than
folded in as free zeroes. Every suite run exited 0 and every repository was at a
fixed commit with a clean working tree for the whole collection, so a signal in
any of these is a false positive by construction.

**The dense traffic corpus is the weaker of the two**, and worth keeping only as
a contrast: 52 batches fired back to back finish in 3.6 seconds and sample one
machine state. Spacing them 20s apart made the per-batch mean request duration
span **1.00–9.94ms (9.9x)** instead of 2.88x, and pushed the load the corpus
crossed from 19–20 to 5–84. Everything said below about traffic rests on the
spaced corpus.

## 3. The floor, per signal kind

Comparisons carrying at least one signal of that kind, out of 50 (out of 8 for
the captured traffic):

| corpus | any | NEW | DISAPP | FREQ | ERROR | LATENCY (example) | LATENCY (other) |
|---|---|---|---|---|---|---|---|
| traffic, spaced | **0** | 0 | 0 | 0 | *n/a* | *n/a* | **0** |
| traffic, dense | **0** | 0 | 0 | 0 | *n/a* | *n/a* | **0** |
| traffic, captured | **0** | 0 | 0 | 0 | *n/a* | *n/a* | **0** |
| iriq | **6** | 0 | 0 | 0 | 0 | **6** | 0 |
| network_resiliency | **50** | **50** | 0 | 0 | 0 | **26** | 0 |
| rails_demo | **2** | 0 | 0 | 0 | 0 | **2** | 0 |
| *fixture replay* | *0* | *0* | *0* | *0* | *0* | *0* | *0* |

*n/a* marks a cell the corpus could not fill: a traffic context has no examples
at all, so ERROR and example-LATENCY were never put to it. §7 has the full
exposure.

As rates, to the precision 50 counts carry:

- **NEW** — 0% everywhere but `network_resiliency`, where it is **100% of
  comparisons, exactly 5.0 signals each, 250 in total**, at confidence 0.88. §5.
- **example LATENCY** — `rails_demo` 4.0%, `iriq` 12%, `network_resiliency`
  52%. §6.
- **non-example LATENCY** — 0 of 158 traffic comparisons. §4.
- **DISAPPEARED, FREQUENCY, INCOMPLETE** — 0 of 258 comparisons, all corpora.
  Rule of three: below **1.2%** per comparison, 95%. FREQUENCY's zero holds even
  where a behavior's identity churns (§5), because the counts themselves stay
  exact, as `signals.md` §1 found.
- **ERROR** — 0, on the 150 comparisons that had an example to fail at all.
  Below 2.0% per comparison.

What a developer actually reads is groups, not signals, and only the top 3 are
shown. Per comparison, `network_resiliency` produced a median of **2 false
groups (max 23, 166 in total)**, `iriq` 0 (max 2), `rails_demo` 0 (max 1). So a
clean `network_resiliency` run fills two of the three slots on a median day and
overflows them on a bad one.

## 4. `latency.md` §5, discharged: **PASS**

> over the next 50 clean runs of a traffic context, at most 2 comparisons carry
> a LATENCY signal on a non-example behavior.

**0 of 50** on the spaced traffic corpus. The same on the dense corpus, and 0 of
8 on the captured one — **0 of 158 traffic comparisons**, which bounds the rate
below 1.9% per comparison (rule of three, 95%). The floor is not raised to
150ms; §5's trigger did not fire.

The zero is a measured zero, not an untested one. Each of the three non-example
populations was made to fire on the same corpus by injecting a regression into
the last batch and comparing against its three predecessors:

| injected into batch 52 | reported |
|---|---|
| +400ms on every `Completed …` line | 2 LATENCY, `GET UsersController#index 2xx` 1ms → 401ms and `#show 2xx` 2.38ms → 403ms |
| +300ms on one SQL template | 1 LATENCY, that `db.query` 0.0375ms → 300ms |
| +300ms on one view-render line | 1 LATENCY, that `log` behavior 1.19ms → 302ms |
| +300ms on **every** SQL template | **0** — the whole window moved, and §4's stall guard vetoed it, which is what it is for |

The rule ran on all 9 timed non-example behaviors, 50 comparisons each — **450
non-example latency judgements, none of which fired**.

Re-slicing the same 52 batches so that every comparison sits at a fixed small
baseline (`fixed_n.sh`) gives 0 of 50 at n=2, 0 of 49 at n=3 and 0 of 47 at n=5,
so the zero is not an artefact of the sequential replay spending 42 of its 50
comparisons at n=10.

## 5. Generator one: an example description is not always a name a person wrote

`interpret/rspec.rs` builds a `test.example`'s identity as
`<spec file> # <full description>` and passes it through `literal()` — "a
template taken verbatim, with no slots. For names people wrote". RSpec's
one-liner syntax generates the description from the matcher, and what it
generates is not stable:

```
is expected to eq #<Demo::Stats:0x000000010b76ee70 @lock=#<Thread::Mutex:0x000000010ae37a78>, @n=0>
is expected to eq 28596.38450779924            # computed from random sample data
is expected to be > 850354997.6860961
```

Two runs of one spec file, diffed: **5 of 23 descriptions differ**, by object
address and by a float derived from random data. Since the behavior id is
derived from the template, every run mints 5 behaviors absent from all n
baseline runs — **5 NEW signals, every comparison, all 50, all from the same
file**, at confidence 0.88 because the rule is correctly confident that these
were not there before.

The churn is one-directional: the 5 that vanish were present in only k < n
baseline runs, so the intermittent rule declines to call them DISAPPEARED. So
the report gains a group every run and never a matching removal.

Grouping contains the damage. All 5 collapse into **one** tier-4 group, and
tier 4 loses to every LATENCY beside it — in a comparison carrying both, the
NEW group ranks last of four and falls below the three shown. Its cost is one
of the three slots on the 24 comparisons that carry nothing else, and a
permanently drifting behavior table underneath.

**The normalizer already handles all three shapes.** `siftr follow` on those
lines returns `#<Demo::Stats:<hex> @lock=#<Thread::Mutex:<hex>>, @n=<int>>` and
`is expected to eq <float>`. The masking is not missing; the example path
deliberately does not use it.

**Recommendation, needs evidence.** Masking the description would fix this and
cost something: `handles <int> retry` would merge two genuinely different
examples, which is the merge hazard `CLAUDE.md` names. A narrower change — mask
only `<hex>`, which cannot carry meaning a person wrote — closes the object-address
half with no merge risk, and leaves the computed-float half. Neither is built
here; this lane changed no `src/`. A pre-registered check for whichever is
tried: `network_resiliency` must go from 250 NEW in 50 comparisons to 0, and
`iriq`'s 1002 example behaviors and `rails_demo`'s 10 must all keep their ids.

**Result (2026-09-19).** Neither shape was built. Another lane closed the same
generator from the other end: the listener now reports whether the example
declared a description at all, so identity uses the enclosing group's words and
marks an undeclared one `<unnamed example>`. Nothing is masked, so the merge
hazard above never arises.

Re-ran this section's own harness against it — `suite.sh` then `tally.rb`, 8
runs of `network_resiliency`, 6 judged comparisons:

```
  comparisons with >= 1 signal: 0 of 6
  signals: none, in any kind
  behaviors per run: 335-335
```

The NEW half of the prediction is met, and `335-335` is the structural half of
the answer: the behavior table is now identical run to run, where its drift was
the generator.

**What this does not show.** These 6 comparisons ran at 1-minute load 5.4–9.1;
the 50 above ran at 19–83. So the 0 example-LATENCY signals here cannot be
attributed between the fix — which did not touch latency at all — and the
quieter machine. §6 stands unaffected, and its remedy is still unfound. The
`iriq` half of the prediction was also not re-run; the fix's own suite and all
24 fixture scenarios are byte-identical, but every example in those is named, so
they cannot exercise the change either way.

## 6. Generator two: example LATENCY, and why the pre-registered remedy is not enough

`signals.md` §3 pre-registered: *"Over the next 50 clean local runs of each
suite, at any load, at most 2 comparisons carry an example LATENCY signal."*

| suite | comparisons with an example LATENCY | verdict |
|---|---|---|
| rails_demo | 2 of 50 | **pass**, exactly at the limit |
| iriq | 6 of 50 | **fail** |
| network_resiliency | 26 of 50 | **fail** |

**It is not simply a loaded machine.** Splitting `iriq`'s 50 comparisons at the
load average `signals.md` itself worked under: **3 of 25 below load 20, 3 of 25
above**. The 1-minute load average does not separate them, exactly as §1 found
(r = −0.09, 0.29); across `network_resiliency`'s 50 comparisons the correlation
between load and FP count is 0.20. Even confined to the band the thresholds were
fitted in, `iriq` sits at 12% against a pre-registered 4%.

**The stall guard has almost nothing to work with.** Suite wall time over 52
clean runs: `iriq` median 27.3s, MAD/median 0.32, range 4.1x; `network_resiliency`
median 16.0s, MAD/median 0.48, range **7.7x** (4.6s to 35.2s). §4's guard vetoes
a candidate when the window moved by much more than the candidate's own share
*and* by more than 3·1.4826·MAD of the window. On a window whose own MAD is half
its median, that second term is several seconds, and a stall that puts +200ms on
twenty examples never reaches it. The worst single comparison — load 78.6,
23 groups — reported **22 false example LATENCY signals at once**, which is
`signals.md` §1's "noise is correlated in time" at ten times the scale it was
observed at.

**The pre-registered remedy does not close it.** §3 committed to raising the
floor to 150ms. Re-evaluating every observed FP against a raised floor (Δ must
exceed `max(floor, 3·median)`), and the canonical `slow` fixture alongside:

| floor | iriq | network_resiliency | rails_demo | `slow` fixture (Δ = 301.3ms) |
|---|---|---|---|---|
| 100ms (today) | 7 sig / 6 cmp | 116 / 26 | 2 / 2 | fires |
| 150ms | 2 / 2 | 38 / 12 | 1 / 1 | fires |
| 200ms | 1 / 1 | 10 / 8 | 0 / 0 | fires |
| 300ms | 1 / 1 | 2 / 2 | 0 / 0 | fires, by 1.3ms |
| 500ms | 0 / 0 | 0 / 0 | 0 / 0 | **silent** |

150ms brings `iriq` to the limit and leaves `network_resiliency` at 12 of 50.
The floor that would bring `network_resiliency` to the limit is 300ms, and
there the reference regression this repo tests against clears by 1.3ms. **The
threshold axis is spent**: there is no floor that is both clean on a real suite
and safe for the regression siftr exists to catch. Whatever fixes this is a
run-level judgement — the evidence that the machine stalled is already recorded
as `run.resources` and suite duration, and is currently read by nothing — not
another constant.

One thing works as designed: these FPs arrive hedged. `iriq`'s seven carry
confidence 0.44–0.64, against 0.88 for the identity churn in §5 and 0.75–0.92
for the true positives in `signals.md` §3. Confidence is doing its job; ranking
is not, because a tier-2 signal at confidence 0.47 still takes a headline slot.

## 7. What these numbers cannot say

A kind's zero is worth only the exposure behind it. Behaviors per run, from
`exposure.rb`:

| corpus | behaviors | timed | examples | db.query | http.request |
|---|---|---|---|---|---|
| traffic | 17 | 9 | **0** | 4 | 2 |
| iriq | 1010 | 1004 | 1002 | **0** | **0** |
| network_resiliency | 359 | 354 | 352 | **0** | **0** |
| rails_demo | 48 | 34 | 10 | 17 | 2 |

- **ERROR's zero** rests on 1364 example behaviors × 50 comparisons. It is a
  strong zero for a *green* suite, and says nothing about the suppression rule
  for a known-flaky failure, which needs a suite that actually flakes.
- **FREQUENCY's zero** is weaker than it looks. Its richest measures — a
  request's query count, an example's query total — exist only in the traffic
  corpus (17 behaviors) and `rails_demo` (48). **No corpus here has both a large
  suite and a database**, which is the shape of the workload siftr is for.
- **example LATENCY has no traffic number and non-example LATENCY has no suite
  number**, because neither population exists in the other's corpus.
- **DISAPPEARED's zero is partly structural**: §5 shows behaviors leaving on
  every run and the intermittent rule declining to report them. A corpus where
  something genuinely stops happening would test the rule; this one does not.
- The whole measurement is one machine, one day, one OS. `signals.md` §1's
  observation that load does not predict run time is confirmed here and is not
  reassuring — it means the thing that causes these FPs is not something we can
  currently see.

## 8. What the traffic zero is actually worth

`latency.md` §2 justified keeping the 100ms floor for non-example behaviors by
noting the largest run-to-run move of a healthy request mean was 0.875ms, "the
floor is 114x that". That was seven runs collected close together. Over 52
batches spanning 17 minutes and load 5–84, `headroom.rb` gives the worst move
per behavior against what the rule would need:

| behavior | runs | median | worst move | need | need / worst |
|---|---|---|---|---|---|
| `GET UsersController#show 2xx` | 52 | 2.625ms | +14.875ms | 100ms | **7x** |
| view render, `users/show` | 52 | 1.512ms | +14.350ms | 100ms | **7x** |
| `GET UsersController#index 2xx` | 52 | 1.250ms | +6.375ms | 100ms | 16x |
| view render, `users/index` | 52 | 0.700ms | +6.038ms | 100ms | 17x |
| view render, layout | 52 | 1.250ms | +7.531ms | 100ms | 13x |
| the four `db.query` behaviors | 52 | 0.025–0.050ms | +0.200 to +0.950ms | 100ms | 105–500x |

The margin is **7x, not 114x**. Still comfortable, and still zero — but an order
of magnitude less comfortable than the number in `latency.md`, and the
difference is entirely the collection window. A corpus is quiet in proportion to
how briefly you watched it.

## 9. What a better corpus would need

This one is good enough to falsify "the floor is zero" and not good enough to
put a number on the rate. To do better:

1. **A real suite of 500–2000 examples with a database**, run to completion at
   least 100 times. Nothing here has both, so the FREQUENCY and grouping rules —
   the ones carrying siftr's best true positives — are measured on 10 examples.
2. **Repeated execution, spread over hours.** §8 is the lesson: the same 52
   batches gave 2.9x spread in 3.6 seconds and 9.9x spread over 17 minutes.
   Batches collected close together will always look quiet.
3. **Recorded machine state per run**, richer than the 1-minute load average,
   which predicts nothing here. siftr already captures `run.resources`; a corpus
   that keeps it beside each comparison would let us ask whether the FP runs are
   separable at all — which is the precondition for the run-level gate §6 says
   is needed.
4. **A suite that flakes**, to exercise ERROR's suppression rule, and one where
   a behavior genuinely stops, to exercise DISAPPEARED. Both are currently
   zeroes with no test behind them.
5. **CI, not a laptop.** 100 runs on a shared dev machine cost two hours and
   crossed load 5 to 84 with other agents' work in the middle. The same 100 runs
   on a CI box would cost nothing and isolate the rules from the machine.
