# FREQUENCY at a small baseline: pricing the exact branch, 2026-09-25

A junior agent driving 0.3.0 hit a FREQUENCY false positive on the third read of
a synthetic log: runs 1 and 2 happened to report the same error-line count, so
run 3 fired `identical in all 2 baseline runs, so any change counts`, and the
signal then reminded on every later run because a randomly-varying count never
returns to its baseline. The mechanism is in `src/signal/rules.rs::frequency`:

```rust
let exact = min == max;
if !exact && distance <= VARYING_WIDTHS * width { return None; }
let value = evidence(n) * if exact { 1.0 } else { 1.0 - width / distance };
```

An exact baseline bypasses the tolerance entirely and takes the undiscounted
confidence, and `n = 2` is the first point the rule can fire at, so every new
context passes through the thinnest evidence it will ever have.

**The finding is real and the diagnosis is too narrow.** Priced against counts
that vary, the rule fires on **about a quarter of comparisons at n = 2 whatever
the count's size**; what changes with size is only which branch does it. Every
candidate confined to the exact branch therefore buys 0–6 of those 25 points and
pays for them in true positives on the deterministic counts siftr ships for. The
rule is unchanged. §6 says what the numbers do support, and what has to be
measured before it can ship.

Reproduce: `cargo test --test signals_backtest -- --ignored --nocapture
--test-threads 1`. Every table below is that harness's output at 200k
comparisons per cell, from fixed seeds; the numbers the prose quotes are pinned
by the tests in that file that run in the gate.

## 1. Why `signals.md` could not have caught this

`signals.md` §3 backtested FREQUENCY at **zero false positives in 256 clean
comparisons**, which stands. §1 says why it could not have found this one:
**counts were exactly deterministic** — all 24 count behaviors identical in all
25 clean demo runs, within each toggle's 5, and in all 8 random-order runs; zero
behaviors varied. A corpus with no variance cannot price a rule whose whole
question is how much variance to forgive. The thresholds were fitted where the
answer is "none, correctly".

So this is not a re-run of §3 on different data. It is the measurement §3 never
had an opportunity to take: **what the rule does when counts vary at all.**

## 2. The generators

Each is *stable* — its parameters never change from run to run — so every signal
raised against one is a false positive by construction. Synthetic only; no log
on this machine was read.

- **fixed v** — a test suite's counts as §1 measured them: identical every run.
- **poisson λ** — independent arrivals at a fixed rate. The *least* noisy way a
  count can actually vary, so it is a lower bound on log-shaped noise, not a
  model of any particular log.
- **over λ cv** — arrivals at a rate that itself wanders lognormally: what a
  window of a log looks like when the traffic behind it is not constant.

## 3. False positives per comparison, as shipped

| generator | n | identical baseline | fires exact | fires varying | either |
|---|---|---|---|---|---|
| fixed 3 | 2 | 100% | 0 | 0 | 0 |
| fixed 3 | 10 | 100% | 0 | 0 | 0 |
| poisson 1 | 2 | 31% | 20% | 3.4% | 24% |
| poisson 1 | 3 | 11% | 6.9% | 1.8% | 8.7% |
| poisson 1 | 5 | 1.4% | 0.90% | 1.1% | 2.0% |
| poisson 1 | 10 | 0.01% | 0.01% | 0.33% | 0.34% |
| poisson 3 | 2 | 17% | 14% | 11% | 24% |
| poisson 3 | 3 | 3.2% | 2.5% | 4.3% | 6.9% |
| poisson 3 | 5 | 0.13% | 0.10% | 1.0% | 1.1% |
| poisson 3 | 10 | 0 | 0 | 0.08% | 0.08% |
| poisson 10 | 2 | 9.0% | 8.0% | 17% | 25% |
| poisson 10 | 5 | 0.01% | 0.01% | 1.0% | 1.0% |
| poisson 30 | 2 | 5.3% | 5.0% | 20% | 25% |
| poisson 30 | 3 | 0.32% | 0.30% | 7.2% | 7.5% |
| poisson 30 | 5 | 0.00% | 0.00% | 1.0% | 1.0% |
| poisson 30 | 10 | 0 | 0 | 0.03% | 0.03% |
| poisson 100 | 2 | 2.8% | 2.7% | 23% | 25% |
| poisson 100 | 5 | 0 | 0 | 1.1% | 1.1% |
| over 10 cv0.3 | 2 | 6.9% | 6.3% | 19% | 25% |
| over 10 cv0.3 | 5 | 0.00% | 0.00% | 1.3% | 1.3% |
| over 100 cv0.3 | 2 | 1.0% | 1.0% | 25% | 26% |
| over 100 cv0.3 | 3 | 0.01% | 0.01% | 8.6% | 8.6% |
| over 100 cv0.3 | 5 | 0 | 0 | 1.6% | 1.6% |
| over 100 cv0.3 | 10 | 0 | 0 | 0.15% | 0.15% |

Four things fall out, and only the first is the reported one.

**The coincidence is real and it does fall fast.** An exact baseline at n = 2 is
a coincidence 31% of the time at a count of ~1, 17% at ~3, 5.3% at ~30 and 1.0%
at ~100-and-wandering; by n = 5 it is under 1.4% everywhere and by n = 10 it is
gone. Nearly every such baseline then fires: at poisson 30, 5.0 of the 5.3
points. So the junior's report is exactly what the rule does.

**But the total does not depend on the count's size.** 24%, 24%, 25%, 25%, 25%,
25%, 26% at n = 2. What size moves is only the *split*: the exact branch carries
20 of 24 points at a count of ~1 and 2.7 of 25 at ~100, and the varying branch
takes up precisely the slack. The exact branch is not the leak; it is the part
of the leak that is visible at small counts.

**The varying branch is no better at n = 2, and it is worse for big counts.**
`|c − median| > 2 × (max − min)` sounds conservative, but the range of two draws
is a poor estimate of spread and is often near zero by chance. In control-chart
terms the expected range of n samples is `d₂(n)·σ` with d₂ = 1.13 at n = 2 and
2.33 at n = 5, so a fixed multiple of the observed range is a bar of ~2.3σ at
n = 2 and ~4.7σ at n = 5. **The rule is most permissive exactly where the
evidence is thinnest** — the opposite of what `E(n)` is doing one line later.

**n is what governs it, and the decay is steep**: ~25% → ~7.5% → ~3% → ~1% →
~0.2% → ~0.05% for n = 2, 3, 4, 5, 7, 10. §3's zero-in-256 and this 25% are not
in conflict: they are the two ends of "does this context's counts vary at all".

## 4. What that does to a whole run

A run reports every behavior at once, so the reader's false-positive rate is the
per-comparison rate compounded. 20 independent varying counts at n = 2 make a
noisy third run all but certain: `1 − (1 − 0.25)²⁰ = 99.7%`. The junior's r3 was
not bad luck; it was the expected outcome.

Over a context's first 12 runs (20 counts, nothing changing, baseline capped at
10 as siftr caps it):

| generator | guard | loud runs | signals per loud run | reminder-runs per signal |
|---|---|---|---|---|
| fixed 3 | shipped | 0 | 0 | 0 |
| poisson 3 | shipped | 27% | 2.74 | 1.93 |
| poisson 3 | exact ±2 below n=3 | 27% | 2.33 | 0.94 |
| poisson 3 | exact 1√med | 26% | 2.22 | 0.59 |
| poisson 3 | median-pooled 3σ | 12% | 1.28 | 0.25 |
| poisson 30 | shipped | 26% | 2.90 | 2.09 |
| poisson 30 | exact ±2 below n=3 | 26% | 2.86 | 1.70 |
| poisson 30 | median-pooled 3σ | 6.5% | 1.33 | 0.16 |
| over 10 cv0.3 | shipped | 29% | 2.75 | 1.96 |
| over 10 cv0.3 | exact ±2 below n=3 | 29% | 2.67 | 1.44 |
| over 10 cv0.3 | median-pooled 3σ | 13% | 1.28 | 0.16 |

The reminder column is the second half of the junior's complaint, measured: a
noise signal stays open for **1.9–2.1 further runs** on average, because
re-judging it asks whether later runs still leave its original baseline, and a
count that wanders keeps doing so. Guarding the exact branch alone halves that
(1.93 → 0.94 at poisson 3) and leaves the *rate* of loud runs untouched.

## 5. The candidates, priced

Each candidate is the shipped rule plus a floor under its tolerance, so it can
only ever fire where the rule already does. False positives per comparison:

| generator | n | shipped | exact ±2 below n=3 | exact ±2 always | exact 1√med | exact 2√med | pooled 2σ | pooled 3σ |
|---|---|---|---|---|---|---|---|---|
| fixed 3 | any | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| poisson 1 | 2 | 24% | 10% | 10% | 10% | 4.8% | 3.8% | 0.83% |
| poisson 3 | 2 | 24% | 19% | 19% | 18% | 13% | 7.4% | 2.4% |
| poisson 10 | 2 | 25% | 23% | 23% | 20% | 18% | 6.7% | 1.7% |
| poisson 30 | 2 | 25% | 25% | 25% | 22% | 21% | 6.4% | 1.2% |
| poisson 100 | 2 | 25% | 25% | 25% | 24% | 23% | 6.4% | 1.2% |
| over 10 cv0.3 | 2 | 25% | 24% | 24% | 22% | 20% | 6.8% | 2.5% |
| over 100 cv0.3 | 2 | 26% | 26% | 26% | 26% | 26% | 6.6% | 2.1% |
| poisson 3 | 3 | 6.9% | 6.9% | 5.7% | 5.6% | 4.7% | 3.2% | 1.3% |
| poisson 30 | 3 | 7.5% | 7.5% | 7.4% | 7.3% | 7.2% | 2.9% | 0.66% |
| poisson 30 | 5 | 1.0% | 1.0% | 1.0% | 1.0% | 1.0% | 0.65% | 0.20% |

**Every exact-branch guard is a rounding error above a count of ~10.** At
poisson 30, n = 2: 25% → 25% (±2), 22% (1√med), 21% (2√med). They only look
useful at poisson 1, where the exact branch happens to be most of the leak.

And they are not free. True positives, on a sustained change landing at run 3
(n = 2) or run 4 (n = 3):

| generator | change | lands at | shipped | exact ±2 below n=3 | exact 1√med | median-pooled 3σ |
|---|---|---|---|---|---|---|
| fixed 3 | ×1.25 (3 → 4) | run 3 | 100% | **0** | **0** | 100% |
| fixed 3 | ×1.25 (3 → 4) | run 4 | 100% | 100% | **0** | 100% |
| fixed 3 | ×2 | run 3 or 4 | 100% | 100% | 100% | 100% |
| fixed 28 | any | run 3 or 4 | 100% | 100% | 100% | 100% |
| poisson 3 | ×2 | run 3 | 44% | 41% | 41% | 22% |
| poisson 3 | ×3.33 | run 3 | 75% | 74% | 74% | 61% |
| poisson 30 | ×1.25 | run 3 | 39% | 39% | 37% | 11% |
| poisson 30 | ×2 | run 3 | 91% | 91% | 91% | 86% |
| poisson 30 | ×3.33 | run 3 | 100% | 100% | 100% | 100% |

The zeroes are the cost that matters, and they are **permanent, not delayed**: a
small change on a deterministic count that goes unreported at run 3 is in the
baseline by run 4 — `[3, 3, 4]` with a current 4 is inside `[min, max]` — so it
is never reported at all. §5 of `signals.md` names this as baseline
contamination; here it is the mechanism by which a guard's cost compounds.

So the trade the reported candidates offer is: forgive the smallest real changes
on the counts that never lie, to remove a fifth of the noise on the counts that
always do. That is the wrong way round for a tool whose first principle is
precision, and it is why **nothing in the rule changed.**

## 6. What the numbers do support: pool the spread across behaviors

The reason `n = 2` is hopeless per behavior is that two numbers cannot estimate a
spread. But a run is not two numbers — it is hundreds of behaviors, and **how
much this context's counts wander is estimable from all of them at once, at any
n**. Measure the variance-to-mean ratio φ (0 where counts hold still, ~1 for
independent arrivals, higher where the rate wanders) over the context's
behaviors, and floor the tolerance at `k·√(φ·median)`:

- on a test suite φ is **exactly** 0, the floor is **exactly** 0, and every
  verdict is the shipped rule's — including the 3 → 4 the other guards lose;
- on a varying context it is a k-sigma bar that scales itself to each count.

At k = 3 it takes n = 2 from ~25% to **1.2–2.5%** and n = 3 from ~7.5% to
**0.6–1.5%**, uniformly across every count size, for 5 points of recall on a ×2
change (91% → 86% at poisson 30) and none at all on a ×3.33. In precision terms,
for one real ×2 change among 20 poisson-30 counts at n = 2: **16% → 79%.**

Estimate φ from the **median** behavior, not the sum. Both read the same on a
homogeneous context (§4's life table: 6.3% vs 6.5% loud runs), but they part
company on the case a test suite actually meets — one behavior that genuinely
moved inside the baseline window. On 20 counts of 3, the smallest move a still-
quiet behavior needs to stay audible:

| behaviors that moved, of 20 | summed φ | median φ |
|---|---|---|
| 0 | +1 | +1 |
| 1 | +3 | +1 |
| 5 | +6 | +1 |
| 9 | +7 | +1 |
| 10 | +7 | +7 |
| 11 | +7 | +10 |

Summing lets one real regression in the baseline deafen the other nineteen.
Taking the median holds the floor at zero until **half** the context moves,
which is the right reading of "this context's counts are deterministic".

**Not shipped, and what it needs first.** k = 3 is calibrated on Poisson and
lognormal-rate synthetic counts; real logs are stranger than either
(`log-contexts.md`: 67% of templates seen exactly once). The mechanism also
couples a per-behavior rule to the rest of the run, which raises questions this
measurement did not ask: whether measures of different kinds (a request's query
count, a template's line count, a stderr count) share one dispersion, and what
a capped or refused run contributes. Both are answerable on a real log corpus
and neither is answerable here. Until then its effect on every corpus siftr has
is provably nil, so shipping it would buy nothing that could be checked.

## 7. The ranking inversion, priced separately

The second consequence: an exact baseline takes `E(n)·1.0` while a varying one
takes `E(n)·(1 − width/distance)`, so a coincidence at n = 2 outranks a genuinely
evidenced signal at the same n. Since `confidence` left human output (measured
AUC 0.43, `confidence.md`), rank order *is* what the reader sees.

Measured on runs where a real change and at least one noise signal both fired,
the share where the real change ranks first — as shipped, and with an exact
baseline discounted to `E(n − 1)` on the grounds that n identical runs are n − 1
confirmations that the count holds still:

| generator | change | n | contested runs | shipped | exact discounted |
|---|---|---|---|---|---|
| poisson 3 | ×2 | 2 | 22,225 | 3.7% | 5.3% |
| poisson 3 | ×3.33 | 2 | 37,221 | 4.3% | 15% |
| poisson 30 | ×2 | 2 | 44,940 | 18% | 35% |
| poisson 30 | ×3.33 | 2 | 49,769 | 29% | **64%** |
| over 10 cv0.3 | ×3.33 | 2 | 41,467 | 16% | 38% |
| poisson 3 | ×3.33 | 3 | 21,972 | 27% | 27% |
| poisson 30 | ×3.33 | 3 | 38,497 | 77% | 78% |
| poisson 30 | ×3.33 | 5 | 8,983 | 92% | 92% |

The inversion is real and the discount is a large fix for it — **at n = 2 only**.
At n ≥ 3 the two columns are the same, because by then an exact baseline is a
0.3% event and almost never contests anything; and on a deterministic context
every FREQUENCY signal is exact, so a uniform discount changes no order at all.

So it buys a rank improvement in exactly one place: the n = 2 comparison of a
context whose counts vary — where §3 of this doc says three signals in four are
noise regardless of how they are ordered. Against that it would change every
stored and printed confidence, `signals.md` §2's formula and §6's vectors 14,
15 and 30, and every fixture's expected numbers. **Not shipped**: correct
ordering of a list that should not exist is not worth a stored-format change.
If §6's floor is ever implemented, re-measure this on top of it — the firing fix
removes most of the contested cases that make it look valuable.

## 8. What stands, and what to do instead

- `signals.md` §2's thresholds and confidence formulas are **unchanged**. §3's
  backtest stands; this adds the regime it did not cover.
- FREQUENCY is trustworthy on counts that hold still — which, measured, is what
  a test suite's counts do — and at n ≥ 5 on counts that don't (≤ 1.6%).
- **Between n = 2 and n = 4 on a context whose counts vary, FREQUENCY is noise
  at 25%, 7.5% and 3% per behavior per comparison.** Nothing in the rule
  distinguishes the two regimes, and the branch a signal came from does not
  either.
- Before ingesting anything log-shaped, §6 is the change to make, and it needs a
  real corpus to calibrate k. `ack --wrong` feedback on FREQUENCY signals from
  varying contexts would label that data instead of assuming its shape, which is
  what this measurement had to do.

**Pre-registered prediction.** On any context whose counts vary — measurable as
a non-zero median φ over its behaviors — at least 1 comparison in 5 at n = 2, and
1 in 15 at n = 3, will carry a FREQUENCY signal with no change behind it. If a
real log corpus comes in materially quieter than that, the generators here are
too noisy and §6's k should be re-fitted downward before it ships.
