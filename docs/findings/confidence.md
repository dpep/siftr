# Confidence: what the number beside a signal actually knows

`siftr changes` prints `conf 0.80` beside almost every signal. Principle 4 says
severity and confidence "come from effect size, baseline spread and the number
of baseline runs — never a constant". This measures whether the printed number
does any of that work, on 2026-09-17, over the three corpora the repository
already carries. It answers the open question left as item 6 of
`dogfood-system-logs.md` ("give confidence an effect-size term when baseline
spread is zero — **needs evidence**").

The answer: for **92% of signals the number is exactly `(n+1)/(n+2)`**, a
bijection of the baseline run count. It ranks true findings **below** chance
(AUC 0.43), and it breaks no ties. An effect-size term cannot fix this, because
effect size is undefined for the kinds that dominate real corpora.

Reproduce: the rule functions in `noise/analyze.rb` mirror
`src/signal/rules.rs`, so the backtest corpus can be re-scored offline —
`load` it with `ARGV.replace(["none"])` to suppress its report dispatch, then
re-define `presence`/`frequency`/`error`/`latency` with identical logic that
also returns the baseline values each used, so candidate formulas can be
derived from the same recorded data. Fixtures were replayed with
`siftr ingest --context … -j --dir fixtures/<corpus>/<state>` into a scratch
`SIFTR_HOME`. The harness was three throwaway scripts; nothing in `src/` or
`tests/` was changed, and no rule was re-tuned.

## 1. What computes confidence, per kind

From `src/signal/rules.rs`. `E(n) = (n+1)/(n+2)` is the rule of succession over
`n` baseline runs: 0.75, 0.80, 0.83, 0.86 and 0.92 for n = 2, 3, 4, 5 and 10.

| kind | formula | reads effect size? | reads baseline spread? |
|---|---|---|---|
| NEW | `E(n)` | no | no |
| DISAPPEARED | `E(n)` | no | no |
| INCOMPLETE | `E(n)`, n ≥ 1 | no | no |
| ERROR | `1 − (j+1)/(n+2)`, j = baseline failures | no | no |
| FREQUENCY, exact baseline | `E(n)` | no | no (it is zero) |
| FREQUENCY, varying baseline | `E(n)·(1 − width/distance)` | yes | yes |
| LATENCY | `E(n)·e/(1+e)`, e = Δ/need, need = max(100ms, 3·median) | yes | no (scales with baseline *level*) |

So `(n+1)/(n+2)` is not the whole story on paper — two of seven rules carry an
effect-size term. Two things collapse that in practice:

- **ERROR with no baseline failure is E(n) identically.** `1 − (0+1)/(n+2) =
  (n+1)/(n+2)`. The `j` term only engages when a baseline run already failed
  with a *different* exception; a same-exception failure is suppressed as known
  flaky instead. So ERROR is E(n) in the ordinary case.
- **Only LATENCY normalizes by anything the baseline measured**, and it
  normalizes by the *threshold*, not by spread. FREQUENCY's varying branch is
  the single rule that reads run-to-run spread.

Confidence's only functional role in the codebase is the tie-break in
`signal::precedence` — `tier` first, then confidence descending. No threshold
anywhere reads it; nothing is suppressed or promoted by it.

## 2. On the backtest corpus, 92% of signals are E(n) exactly

`docs/findings/noise/data/runs.json`: 90 runs across demo, demorand and iriq,
re-scored at n ∈ {2, 3, 5, 10} — 350 comparisons, **264 signals**. The 256
clean comparisons produced 0 signals, reproducing §3 of `signals.md`.

| kind | signals | distinct confidences | determined by baseline n alone? |
|---|---|---|---|
| NEW | 104 | 0.75, 0.80, 0.86, 0.92 | **yes** |
| FREQUENCY | 94 | 0.75, 0.80, 0.86, 0.92 | **yes** |
| DISAPPEARED | 26 | 0.75, 0.80, 0.86, 0.92 | **yes** |
| ERROR | 20 | 0.75, 0.80, 0.86, 0.92 | **yes** |
| LATENCY | 20 | 0.56, 0.57, 0.60, 0.61, 0.64, 0.65, 0.69, 0.70 | no |

**244 of 264 signals (92%) carry exactly E(n).** For each of those four kinds,
fixing the baseline count fixes the confidence to a single value. Only LATENCY
varies within a baseline size, and it varies by **0.01** (0.56 vs 0.57 at n = 2).

Why the two effect-size terms never engage:

- **The varying-FREQUENCY branch fired 0 times in 94 FREQUENCY signals.** All 94
  had an exact baseline. That follows from §1 of `signals.md`: counts are
  exactly deterministic, so `min == max` and the rule takes the exact branch.
- **Every count baseline had spread exactly zero** — 120 of 120 count signals
  with a multi-run baseline. The denominator an effect-size-over-spread term
  would need is 0 for all of them.
- **NEW and ERROR have no baseline magnitude at all** (124 of 264 signals here).
  Presence and failure are binary; there is nothing to size.

## 3. Confidence ranks true findings below chance

Labels: a signal is a **true finding** if the seeded regression explains it
(the N+1's request query count, its `Comment Load` count and disappearance, and
the affected example's query total; the `slow` example's latency; the `warn`
deprecation lines; the `fail` example's error). Signals from the **cold-DB**
runs are incidental — `signals.md` itself calls them "environment, not code"
and tiers them 5. That gives 184 true and 80 incidental signals.

| baseline n | true findings | incidental | AUC (current) |
|---|---|---|---|
| 2 | 48 (0.56–0.75) | 20 (0.75) | 0.448 |
| 3 | 48 (0.60–0.80) | 20 (0.80) | 0.448 |
| 5 | 48 (0.64–0.86) | 20 (0.86) | 0.448 |
| 10 | 40 (0.69–0.92) | 20 (0.92) | 0.438 |
| **pooled** | **184** | **80** | **0.429** |

**AUC 0.43 is below chance.** Only **42%** (78/184) of true findings outrank the
median incidental signal. The reason is mechanical: at a fixed n, every count
rule returns the same number, so a real N+1 and a cold DB's schema-load chatter
both print `conf 0.86` and are indistinguishable. The count rules tie; the
tie-break is then decided by whatever else `precedence` compares.

The inversion is worst exactly where the effect-size term exists:

**AUC(seeded LATENCY vs cold-DB) = 0.000.** All 20 seeded latency signals score
0.56–0.70; all 80 cold-DB signals score 0.75–0.92. Because `e/(1+e) < 1`
always, the one kind that measures effect size is capped *below* every kind
that ignores it. The genuine 300ms regression scores lower than a schema load.

Ranking is not broken by this, because `tier` carries it — LATENCY is tier 2,
cold-DB setup is tier 5. Confidence contributes nothing: across all 350
comparisons there were **54 same-tier multi-member sets, and confidence
differed in 0 of them.** It has never broken a tie on this corpus.

## 4. On the committed fixtures, the number is 0.80 almost always

`fixtures/rails_demo` (three clean captures, so n = 3 and E(3) = 0.80) and
`fixtures/rspec_hunt`, replayed through the built binary.

| corpus | signals | at 0.80 | other |
|---|---|---|---|
| rails_demo, 8 scenarios | 13 | 12 | LATENCY 0.61 |
| rspec_hunt, 8 sequences | 33 | 32 | INCOMPLETE 0.83 (n = 4) |

**44 of 46 fixture signals print `conf 0.80`.** The same 0.80 covers a failing
test, a deprecation warning, an N+1's four signals, a spec file that failed to
load, 16 examples lost with a deleted file, and six examples gained. The single
0.83 differs only because that sequence had four baseline runs rather than
three.

This is what the reported 0.39–0.91 range looks like up close. That range came
from `dogfood-system-logs.md`, where **37,491 of 38,244 signals were NEW** — a
corpus of almost nothing but the one kind whose confidence is E(n) by
construction. The spread there is the spread of E(n) over varying baseline
sizes plus a thin tail of varying-FREQUENCY, not evidence being weighed.

## 5. The candidate, recomputed offline

Add an effect-size term, current versus baseline, normalized by baseline
spread, from the same recorded data. Both variants keep `E(n)` as a factor:

- **relative**: `E(n)·r/(1+r)`, r = |c − median| / max(median, 1)
- **spread**: `E(n)·z/(1+z)`, z = |c − median| / max(1.4826·MAD, 1) — the floor
  of 1 is not a detail, it is the whole problem: without it the denominator is
  zero for every count behavior measured here.

Pooled, both beat the current formula: AUC **0.429 → 0.536** (relative) and
**0.739** (spread). That looks like a win, and it is not one. Comparing like
with like — the same kind on both sides — shows where the gain comes from:

| kind | true | incidental | AUC current | AUC relative | AUC spread |
|---|---|---|---|---|---|
| FREQUENCY | 78 | 16 | 0.471 | 0.605 | 1.000 |
| NEW | 40 | 64 | 0.500 | 0.375 | **0.500** |

The pooled gain is a **per-kind reordering**: it pushes LATENCY up (to 0.71–0.91)
and NEW, DISAPPEARED and ERROR down (to 0.38–0.46). That ordering is precisely
what `tier` already encodes, so the candidate buys a second, weaker copy of
`tier` and calls it confidence.

Within a kind it does real work in exactly one place — FREQUENCY, where the
cold-DB counts move less than the N+1's do. And it cannot touch the kind that
matters most: **for NEW it is undefined**, there being no baseline value to
subtract. I gave NEW a flat `0.5·E(n)`, which is honest about the fact that
there is nothing to measure, and its within-kind AUC stays at 0.500. NEW is 40%
of this corpus and **98% of the system-log corpus**, so a term that cannot score
NEW cannot fix the complaint that motivated it.

**What a change would cost.** Every threshold in `signals.md` was tuned against
the current formula, and the confidence values are not decoration in the tests:
§6's 30 test vectors pin exact numbers, asserted by `rules.rs`'s own
`latency_vectors`/`frequency_vectors`/`presence_vectors`/`error_vectors` (29
vectors) and by `tests/signals_fixtures.rs`. Moving the formula rewrites all of
them, invalidates §3's zero-false-positive backtest, and re-opens the LATENCY
sweep that sits one step from 4–7 false positives on every axis. That is a
large bill for a term that, measured here, reproduces `tier`.

## 6. What this means

Confidence is a **baseline-adequacy statistic, not a discriminator**. E(n)
answers a real and useful question — "how many runs back this claim?" — and it
answers it correctly. It does not answer "how much should you care", and the
0.00–1.00 scale, the two significant figures and the name all imply that it
does. That implication is the defect, not the arithmetic.

The thresholds do the discriminating. They are why 256 clean comparisons
produced 0 signals. `tier` does the ranking. Confidence, as shipped, is a
restatement of the baseline run count in a costume that suggests it weighed
evidence — the same class of problem as the confident-wrong-answer bugs fixed
in 0.1.5, and the reason principle 4 exists.

## 7. Recommendation

1. **Document confidence as what it is: baseline adequacy, E(n).** **Ship it.**
   It is measured, it is true for 92% of signals, and it costs nothing. Say in
   `signals.md` §2 that for NEW, DISAPPEARED, INCOMPLETE, exact FREQUENCY and
   unflaky ERROR the value is exactly `(n+1)/(n+2)` and carries no effect size,
   and that its only functional role is a tie-break it has never broken.

2. **Stop printing a per-signal `conf` in the default human output; print the
   baseline it rests on instead** — `3 baseline runs` rather than `conf 0.80`.
   **Ship it.** For 92% of signals this is an exact relabelling with zero
   information loss: `conf` is a bijection of n, so `n = 3` says strictly more
   and implies strictly less. Keep the number in `-j` and in `explain`, where
   the formula is already spelled out beside it. This removes the false
   implication without touching a single threshold.

3. **Do not add an effect-size term.** **Ship it** as a decision, closing item 6
   of `dogfood-system-logs.md`. It is undefined for NEW and ERROR (98% of the
   log corpus), has a zero denominator for 100% of the count baselines measured,
   and its pooled gain is a per-kind reordering `tier` already encodes. The
   zero-spread case that motivated it is real, but this is not its fix — a brace
   fragment scoring 0.75 is a *behavior* problem (it should not be a behavior),
   not a confidence problem.

4. **Re-scale LATENCY onto the same footing as the count rules, or drop the
   printed number (2) and let the question lapse.** **Needs evidence.**
   `e/(1+e)` caps LATENCY at 0.70 while a cold DB reaches 0.92, giving
   AUC 0.000 against incidental signals. The measurement that decides it: a
   calibration set of known-true and known-false latency changes, large enough
   to fit the map from `e` onto a probability — §5 of `signals.md` already
   records that one toggle cannot calibrate `e/(1+e)`. Until that exists,
   re-scaling would swap one unjustified constant for another.

5. **Measure confidence on a corpus whose counts genuinely vary.** **Needs
   evidence.** The varying-FREQUENCY branch — the only rule that reads
   run-to-run spread — fired 0 times in 350 comparisons, so the one
   spread-aware formula siftr ships is untested on RSpec data. §5 of
   `signals.md` already flags that another app's lazy queries would make
   per-example counts vary. The measurement: re-score a larger Rails suite's
   baseline history and report how many FREQUENCY signals take the varying
   branch, and what confidences it produces.

Not recommended: a severity score alongside confidence, and any per-signal
weighting derived from it. Both would need the calibration set item 4 asks for
before they could mean anything, and `tier` already carries the ordering.

## Reproducing

Everything here came from committed data — `docs/findings/noise/data/runs.json`,
`fixtures/rails_demo` and `fixtures/rspec_hunt`. No new corpus was captured,
nothing outside the repository was read, and each fixture replay used a scratch
`SIFTR_HOME` under a temporary directory that was deleted afterwards. The three
analysis scripts were scratch-only and are not committed; the `Reproduce` note
above records the one non-obvious step needed to rebuild them, which is re-defining
`analyze.rb`'s rule functions so each signal carries the baseline values that
produced it.
