# LATENCY: two things the rule could not see

`docs/findings/noise-floor.md` §6 measured example LATENCY firing on clean,
unchanged suites — 6 of 50 comparisons on `iriq`, 26 of 50 on
`network_resiliency`, against `signals.md` §3's pre-registered budget of 2 of 50
— named a suspect without convicting it ("the stall guard has almost nothing to
work with"), and closed the threshold axis: no floor is both clean on a real
suite and safe for the 311ms regression siftr exists to catch.

This is the conviction, and the change that follows. Measured 2026-09-20 at
`34b97ce`, Ruby 3.4.9, an 8-core arm64 Mac shared with other agents, across
**two collections of each suite in two machine states** — 52 runs each, once at
1-minute load 14–129 and once at load 4.

Three findings, in the order they matter:

1. **The rate is a property of the machine, not of the suite.** The same
   `network_resiliency` that gives 19 of 50 at load 14–127 gives **0 of 50 at
   load 4.6**, of any signal kind. `rails_demo` likewise: 7 of 50 loaded, 0 of
   50 quiet. So `signals.md` §3's budget was not fitted on suites too small to
   generalise; it was fitted on a machine state siftr's docs never named.
2. **What lets a busy machine through is that the stall guard asks its question
   of the suite's wall clock.** On more than half the false positives no
   threshold on that clock could have helped, because the run did not take
   longer at all — §1. The fix is to count the disturbance where it lands: **how
   many other examples moved with the candidate** (§2).
3. **The rule reads a baseline's median and its max, never its width.** So a
   value 5% above a max that a 7.7x-wide baseline already held is news. That is
   a second, independent generator, it is not about examples, and `frequency`'s
   varying arm has always had the test that closes it (§3).

Together they are inert on a quiet machine — identical signals, recall within
two points — and over 50 comparisons of a loaded one they take
`network_resiliency` from 19 to 5 and `iriq` from 8 to 0.

Reproduce: `noise-floor/suite.sh` collects, `noise-floor/replay.sh` re-judges
the same captures with another build (set `SIFTR_KEEP_EVIDENCE=100` while
collecting, or retention prunes the captures first), `noise-floor/tally.rb`
reduces, `script/fault-matrix` scores detection.

## 1. Generator one: why no threshold on the window can close it

`rules::latency` vetoes a candidate as a stall when both hold:

```
rest = (window_current − window_median) − own_excess
rest > own_excess   and   rest > STALL_SPREADS · MAD_TO_SIGMA · MAD(window)
```

For an example the window is the suite's duration. Taking every false example
LATENCY the loaded collections produced and asking which arm let it through:

| suite | false LATENCY | the suite did **not** run longer than this example alone explains | it did, but by less than the spread bar |
|---|---|---|---|
| `network_resiliency` | 75 | **41 (55%)** | 34 (45%) |
| `iriq` | 21 | **11 (52%)** | 10 (48%) |
| `rails_demo` | 8 | **6 (75%)** | 2 (25%) |

**The first column is the important one.** On half to three-quarters of these,
the first arm never even engaged: the run's suite duration came in at or below
what the candidate's own slowdown accounts for — often *below* the baseline
median outright (`network_resiliency`'s false positives include runs at
0.53–0.77x). A suite's clock carries boot, file loading, `before(:suite)` and
every between-example cost; a few hundred milliseconds of scheduling pause is a
rounding error inside it, and is easily swamped by the run being cheaper
somewhere else. No threshold on a quantity that did not move can veto anything.

The remaining half is the arm §6 suspected, and its bar is set by the suite's
own dispersion rather than by the disturbance:

| suite (loaded) | guard's bar, median | disturbance present, median | worst comparison |
|---|---|---|---|
| `rails_demo` | 0.42s | 0.14s | 1.77s |
| `network_resiliency` | 23.0s | 4.62s | 70.8s |
| `iriq` | 27.3s | 0.47s | 104s |

"Disturbance present" is the sum of every example's slowdown (current − its own
baseline median, positives only) — the movement the guard is trying to detect,
measured where it lands. On `iriq` the guard demands 27 seconds of unexplained
movement before it will veto a 300ms claim, on a suite whose examples typically
move half a second in total.

Summing the examples' own durations instead of reading the reporter's summary
does not help: on all three corpora the two track each other to two significant
figures. The instrument is not wrong for the quantity; the quantity is wrong. A
stall is a fact about examples, so it has to be counted over examples.

## 2. The cohort

`signal.rs` already computes, for every example in the run, its slowdown against
its own baseline median — `excess`, built for the adjacent-example veto and
discarded after it. The change reads the rest of that vector:

> An example LATENCY is a stall when **`COHORT_PEERS` other examples of the same
> run each moved by at least `NEIGHBOUR_SHARE` of this one's slowdown.**

`COHORT_PEERS = 3`. No new share: the bar is the 0.5 the neighbour rule already
uses, and it is scale-free, because the candidate's own delta sets it — a bigger
regression is correspondingly harder to explain away. The cohort is `None` for
every kind but `test.example`, as the neighbour already was.

**This is not the adjacent-example veto widened.** `latency.md` §3 grounds that
veto in two facts about examples: they run one at a time, and they are causally
unrelated. Both hold for *any* pair of examples, not only adjacent ones. What
adjacency adds is "the same machine **moments earlier**", which a distant
example does not give you — and that is exactly why one distant peer is not
enough and three are: the count buys back the locality that distance spends.
`latency.md` §3's warning is about the other direction — carrying the veto *off*
the example path, where the nearest behaviors are a regression's own
consequences — and nothing here does that.

It is also the same judgement the window guard was making, on a statistic that
can see it. `noise-floor.md` §4 already records siftr declining a uniform
slowdown — "+300ms on every SQL template → 0, the whole window moved, and the
stall guard vetoed it, which is what it is for". This extends that acceptance to
examples, where the window guard could not reach. §5 says what it costs.

## 3. Generator two: a baseline whose width the rule never reads

Found independently, on a different language, runner and behavior kind, by the
lane that wrapped siftr around this repo's own `cargo test` gate. Over 14 real
runs the only LATENCY from a run that did nothing different was on cargo's build
line, and its stored numbers are:

```
Finished `test` profile [unoptimized + debuginfo] target(s) in <duration>
kind log   current 1050ms   baseline median 245   min 130   max 1000   confidence 0.48
```

Every arm of the rule passes honestly. `delta = 805 > need = 735`, and
`current > max`. But the baseline spans **7.7x**, and 1050 is 5% above a value
it already held. `rules::latency` reads a baseline's median and its max and
never its width, so "higher than anything before" means nothing once the
baseline is wide.

It is the same shape as a large minority of the example false positives. Taking
every false example LATENCY the loaded collections stored and asking how far
above the baseline max it sat:

| suite | false LATENCY | median current/max | within 1.5x of max | baseline width, median |
|---|---|---|---|---|
| `network_resiliency` | 75 | 1.60 | **44%** | 20.6x |
| `iriq` | 21 | 2.21 | 29% | 7.3x |
| `rails_demo` | 8 | 2.54 | 12% | 5.7x |

**`frequency` has always had the test that closes this** and `latency` never
did: a varying count must leave its range by `VARYING_WIDTHS` range widths,
while a duration needed only to exceed the max at all. Closing the asymmetry
adds no constant:

> `delta > VARYING_WIDTHS · (max − min)`, alongside the floor and the ratio.

This one is kind-agnostic — it is about a baseline, not about examples — so it
covers the populations the cohort cannot reach. Every published latency vector
in `signals.md` §6 survives it unchanged; vectors 35 and 36 pin the cargo pair,
same n, same median, same current and the same 0.48, differing only in width.

## 4. False positives

52 clean runs per suite per collection into one fresh context, reduced by
`tally.rb`. Nothing changed between runs, so every signal is a false positive by
construction. **Before/after is measured by replaying the same captured bytes
through both builds**, so the two columns differ only by the rule. Collect with
`SIFTR_KEEP_EVIDENCE=100` or retention prunes the captures a replay needs.

| suite | examples | machine | before | after |
|---|---|---|---|---|
| `rails_demo` | 10 | quiet, load 3.7 | 0 of 50 | **0 of 50** |
| `network_resiliency` | 335 | quiet, load 4.6 | 0 of 50 | **0 of 50** |
| `iriq` | 1002 | quiet, load 5.2 | 1 of 50 (1 signal) | **1 of 50 (1)** |
| `rails_demo` | 10 | loaded, load 16–42 | 3 of 18 (3, two on non-examples) | **1 of 18 (1, none)** |
| `network_resiliency` | 335 | loaded, load 14–127 | 13 of 18 (37) | **3 of 18 (4)** |

The quiet rows are the safety claim: the change is not merely cheap there, it is
arithmetically inert. The loaded `rails_demo` row is §3 working — both
non-example LATENCY signals were a current inside a wide baseline's range.

Retention keeps captures for the last 20 runs, so a replay covers 18 of each
collection's 50 comparisons. The other 32 are still answerable exactly: neither
new test needs execution order, and the neighbour veto was already applied when
those signals were stored, so re-judging the stored signals gives the survivor
count directly. That method reproduces the replay ground truth above — 13 of 18
and 37 signals down to 3 and 4 — and over all 50 loaded comparisons it gives:

| suite (loaded) | before | after |
|---|---|---|
| `rails_demo` | 7 of 50 (8 signals) | **6 of 50 (6)** |
| `network_resiliency` | 19 of 50 (75 signals) | **5 of 50 (6)** |
| `iriq` | 8 of 50 (21 signals) | **0 of 50 (0)** |

`iriq`'s false positives were entirely the correlated kind. `rails_demo`'s were
entirely the isolated kind, which is §8.

Pooling both collections — 100 comparisons of each suite, load 3.6 to 129 —
against the pre-change rule:

| load band | `rails_demo` | `network_resiliency` | `iriq` |
|---|---|---|---|
| under 10 | 0 of 50 | 0 of 50 | 0 of 31 |
| 10–20 | 0 of 10 | 1 of 9 | 0 of 14 |
| 20–40 | 6 of 32 (19%) | 8 of 22 (36%) | 5 of 11 (45%) |
| 40 and up | 1 of 8 | 10 of 19 (53%) | 3 of 8 |

**1 false positive in 164 comparisons below load 20; 33 in 100 above it.** §6
looked for this and did not find it (`iriq` split 3 of 25 either side of load 20,
correlation 0.20 on `network_resiliency`), so the 1-minute load average is not a
dependable gate and nothing here makes it one — it is lagging, coarse, and
sampled before the run starts. The cohort is the same disturbance measured from
*inside* the run, which is why the rule uses that and not this table.

## 5. What it costs

Injecting +Δ into one example of every clean run and asking whether it is still
reported. On the quiet collections, where the guards should be inert:

| suite | build | +110ms | +150ms | +300ms | +700ms | +1500ms |
|---|---|---|---|---|---|---|
| `rails_demo` | before | 95% | 100% | 100% | 100% | 100% |
| `rails_demo` | after | 93% | 98% | **100%** | **100%** | **100%** |
| `network_resiliency` | before | 74% | 75% | 77% | 84% | 94% |
| `network_resiliency` | after | 73% | 74% | **77%** | **84%** | **94%** |

Nothing moves at +300ms and above; the two points lost at +110/+150ms are §3's
width term meeting an example whose own baseline is wide, which is the case it
exists for. (Recall is below 100% before the change too: an example whose
baseline median is large needs 3·median, not 100ms.)

On the loaded collection it is not inert, and the cost is real:

| suite | +110ms | +300ms | +1500ms |
|---|---|---|---|
| `network_resiliency` | 45% → 8% | 91% → 39% | 99% → 99% |

Stated structurally instead of as a rate — the cohort declines a +2B ms
regression exactly when four or more examples moved by ≥ B:

| suite (loaded) | declines +110ms | declines +300ms | declines +1500ms |
|---|---|---|---|
| `rails_demo` | 4 of 50 (8%) | **0 of 50** | **0 of 50** |
| `network_resiliency` | 36 of 50 (72%) | 27 of 50 (54%) | 5 of 50 (10%) |

That is the honest shape of it. On 54% of a busy machine's clean
`network_resiliency` comparisons, four or more examples really did move by 150ms
or more, because the machine moved them; on those runs a 300ms claim about one
example was never evidence about code. `rails_demo` declines none at 300ms,
which is why §6's fault matrix is untouched.

## 6. The fault matrix

`script/fault-matrix --reps 5`, both builds, same machine, same session. The
change can only ever *remove* a signal, so a cell that gains a detection is
machine noise and a cell that loses one is this change.

| | before | after |
|---|---|---|
| cells as expected | **25 of 25** | **25 of 25** |
| A3 `+110ms` past the floor | 5/5, tier 2, conf 0.44–0.45 | **5/5**, tier 2, conf 0.44–0.45 |
| A4 `+150ms` | 5/5, tier 2, conf 0.50–0.52 | **5/5**, tier 2, conf 0.51 |
| A5 `+300ms` | 5/5, tier 2, conf 0.62–0.63 | **5/5**, tier 2, conf 0.63 |
| B3 `+700ms on 200ms` | 5/5, tier 2, conf 0.44 | **5/5**, tier 2, conf 0.44 |
| B4 `+1500ms on 200ms` | 5/5, tier 2, conf 0.59 | **5/5**, tier 2, conf 0.59 |
| control trials carrying a signal | 2 of 10 | **0 of 10** |

Every other cell is unmoved, in both directions: A1, A2, B1, B2, D1, D2, E2, F2
and I2 still miss, and C, D3, E1, F1, G1, H1, H2 and I1 still hit at the same
rank, tier and confidence. An intermediate build (the cohort alone, measured
under load) showed A2 firing once of five at `1→113` — 80ms of injected sleep
plus 33ms of machine pause, over the floor. That is a *gain*, which neither
guard can cause; re-run on both builds at `--cells A2,A3,Z --reps 10` on a
quieter machine, **A2 is 0 of 10 on both and A3 is 10 of 10 on both**.

## 7. Considered and rejected

**Raising the floor.** `noise-floor.md` §6 killed it: 150ms leaves
`network_resiliency` at 12 of 50, and the 300ms that clears it clears the `slow`
fixture's 311ms regression by 1.3ms.

**Replacing the window guard's spread term with a relative one**
(`rest > K · own_excess`, dropping the MAD). Scale-free, one constant fewer, and
it keeps every published test vector for `1 ≤ K < 4.5`. It is worse on the
corpus that motivates it: at K=3 the loaded `rails_demo` goes from 7 false
signals to 11 and `network_resiliency` only from 164 to 71 — because §1's first
column is untouched by any threshold on that clock. The window guard is good at
what it is for, and `latency.md` §5's traffic zero (0 of 158) rests on it. Left
exactly as it is.

**The width term instead of the cohort.** It is the weaker of the two on
examples: alone, at two widths, it takes the loaded `network_resiliency` from 13
of 18 comparisons to 10 (37 signals to 23), against the cohort's 3 of 18 and 4.
Three widths reaches 8 of 18 and costs 4 points of quiet-machine recall at
+110ms. They address different generators and both ship.

**The cohort instead of the width term.** It cannot: the cohort is `None` off
the example path, so cargo's build line, a `db.query` and an `http.request` keep
firing. Two of the loaded `rails_demo` replay's three false LATENCY were on
non-examples, and the width term is what removed them.

**Scaling the peer count with the suite,** `peers ≥ max(3, f · examples)`, on
the argument that more examples means more tail draws, so a flat count is not
scale-free. It is the more principled shape and it does not reach the budget: at
f = 1% the loaded `iriq` stays at 5 of 20 comparisons and `network_resiliency`
at 10 of 48; at f = 2% `iriq` is 9 of 20, against the 10 it started at. A flat
three peers is what the evidence supports. If a corpus ever shows the guard
biting a *quiet* 2000-example suite, this is the shape to revisit, and §5's
quiet rows are the check that would catch it.

**Requiring the slowdown to persist across two runs.** It would close §8's
residual, and it costs a full run of detection latency: every A and B cell of
the fault matrix injects into one run and would flip to a miss. Not built.

**Gating example LATENCY on suite size,** as `signals.md` §7 demotes a FREQUENCY
that only tracks the suite's size. The evidence does not support it: the suite
that fails hardest has 335 examples and the one with 1002 fails less, and both
are clean at load 4. Size is not the variable.

## 8. What is left, and what a reader should do

Of the 37 false LATENCY in `network_resiliency`'s replayed loaded captures, 33
had three or more peers moving with them. What neither guard can touch is a
single scheduling pause on a single fast example with nothing else moving and a
tight baseline — `17.6ms → 303ms`, past the 100ms floor, 17x the median and 5x
the baseline width. The loaded `rails_demo`'s residual 1 of 18 and the quiet
`iriq`'s 1 of 50 are both exactly this. **With one run of evidence there is
nothing to tell it from a real regression**, and saying so is more useful than
another constant.

For a reader of a clean report: an example LATENCY alone in its group, at
confidence below about 0.6, on a suite you did not change, is more likely to be
your machine than your code. `siftr explain` shows the baseline spread and the
run's `run.resources` beside the baseline's, which is what tells a slow run from
a loaded machine. Re-run before chasing it — and if a clean run is filling two
of three slots, check what else is running, because below load 20 these corpora
produced 1 false positive in 164 comparisons.

**Worth building next, not built here:** the cohort size is a measured,
run-local disturbance number that nothing currently shows. Rendering it in
`explain` ("11 other examples slowed with this one") would let a reader make the
judgement above from the report rather than from advice.

**Pre-registered prediction, replacing `signals.md` §3's.** §3 predicted "over
the next 50 clean local runs of each suite, at any load, at most 2 comparisons
carry an example LATENCY signal". The clause that fails is *at any load*: the
same rule on the same suite gives 0 of 50 at load 4 and 19 of 50 at load 14–127.
The replacement names the condition:

> Over the next 50 clean local runs of any rspec context whose 1-minute load
> average stays **under 20**, at most **2 of 50** comparisons carry an example
> LATENCY signal, and at most **3 signals** in total. Above load 20 no budget is
> claimed, and siftr should be read as declining to judge latency there rather
> than as reporting none.

And for the population §3 never covered:

> Over the next 50 clean runs of any context, at most **1 of 50** comparisons
> carries a LATENCY on a behavior whose baseline min and max differ by more than
> 3x.

If the first is exceeded, the next move is §7's persistence requirement, with
its cost to the fault matrix measured first — not another threshold. If the
above-20 half ever needs a number, it needs a corpus collected on CI, which
`noise-floor.md` §9 already asks for.
