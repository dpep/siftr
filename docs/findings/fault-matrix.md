# Fault matrix: what siftr still detects, and at what rank

Measured 2026-09-19 against 6715345. The fixture replays prove siftr's output did not
**change**; nothing proved it still **detects**. A `rules.rs` edit that halved recall
would pass the whole gate clean and reach a user before it reached a test.

`script/fault-matrix` closes that. It injects faults of known kind and known magnitude
into `dogfood/rails_demo`, runs the real loop, and records for each what siftr said:
detected or not, at which group rank, in which tier, with what confidence. Cells sit on
both sides of every threshold, because a cell expected to **miss** is as load-bearing as
one expected to hit — a rule that widens shows up there and nowhere else.

```
script/fault-matrix --reps 5            # 25 cells x 5 trials, 191s on a quiet machine
script/fault-matrix                     # 3 trials each, the default
script/fault-matrix --cells Z --reps 30 # just the false-positive rate
```

Exit 1 when a cell flips. Per-trial records land in `$TMPDIR/siftr-fault-matrix/results.ndjson`,
one JSON object per run, carrying the run id every number came from.

## 1. How a cell works

A **seed** is 4 baseline runs recorded into one `SIFTR_HOME`. Every trial clones that
store, runs the demo once with the fault switched on, and reads `siftr run -j`. Cells
sharing a seed therefore share one baseline, which is what makes their rows comparable.
Faults reach the app through `script/faults.rb`, required into rspec via `SPEC_OPTS`
(`--require` accumulates where `--format` would replace the user's formatters —
`capture.md`), so the demo's own source is never edited and the normalized command, and
so the context, is identical for baseline and fault runs.

A cell counts as **detected** only when siftr raises a signal about the fault that was
injected. A run that produced some other signal is not a hit; on a control cell it is a
false positive.

## 2. The matrix

25 cells x 5 trials, 191s, quiet machine (run wall time 877–2760ms, median 1010ms).
`measured` is baseline median → current, in the signal's own unit.

| cell | fault | magnitude | expect | hits | rank | tier | conf | measured |
|---|---|---|---|---|---|---|---|---|
| Z1 | none | — | miss | 0/5 | — | — | — | — |
| Z2 | none, 200ms example present | — | miss | 0/5 | — | — | — | — |
| A1 | latency, fast example | +30ms | miss | 0/5 | — | — | — | — |
| A2 | latency, fast example | +80ms | miss | 0/5 | — | — | — | — |
| A3 | latency, fast example | +110ms | hit | 5/5 | 1 of 1 | 2 | 0.44–0.45 | 1→111–116 |
| A4 | latency, fast example | +150ms | hit | 5/5 | 1 of 1 | 2 | 0.50–0.51 | 1→154–157 |
| A5 | latency, fast example | +300ms | hit | 5/5 | 1 of 1 | 2 | 0.63 | 1→302–306 |
| B1 | latency, 200ms example | +300ms | miss | 0/5 | — | — | — | — |
| B2 | latency, 200ms example | +500ms | miss | 0/5 | — | — | — | — |
| B3 | latency, 200ms example | +700ms | hit | 5/5 | 1 of 1 | 2 | 0.44 | 206→901–907 |
| B4 | latency, 200ms example | +1500ms | hit | 5/5 | 1 of 1 | 2 | 0.59 | 206→1700–1710 |
| C1 | frequency, exact baseline | +1 query | hit | 5/5 | 1 of 1 | 2 | 0.83 | 3→4 |
| C2 | frequency, exact baseline | +7 queries | hit | 5/5 | 1 of 1 | 2 | 0.83 | 3→10 |
| D1 | frequency, varying baseline | 5 queries | miss | 0/5 | — | — | — | — |
| D2 | frequency, varying baseline | 7 queries | miss | 0/5 | — | — | — | — |
| D3 | frequency, varying baseline | 10 queries | hit | 5/5 | 1 of 1 | 2 | 0.50 | 5→10 |
| E1 | error, green baseline | 1 example | hit | 5/5 | 1 of 1 | 1 | 0.83 | — |
| E2 | error, same class in every baseline run | 1 example | miss | 0/5 | — | — | — | — |
| F1 | new stderr line | 2 lines | hit | 5/5 | 1 of 1 | 2 | 0.83 | — |
| F2 | new stderr line, in 2 of 4 baseline runs | 2 lines | miss | 0/5 | — | — | — | — |
| G1 | incomplete run | 1 spec file | hit | 5/5 | 1 of 7 | 1 | 0.83 | — |
| H1 | examples added | +3 examples | hit | 5/5 | 1 of 4 | 4 | 0.83 | — |
| H2 | examples removed | -3 examples | hit | 5/5 | 1 of 4 | 4 | 0.83 | 1→0 |
| I1 | error, hand-written description | 1 example | hit | 5/5 | 1 of 3 | 1 | 0.83 | — |
| I2 | error, generated description | 1 example | miss | 0/5 | — | — | — | — |

Every detection headed its group, and every one but G1, H1, H2 was the only group in the
report. Ranking is not where this model is weak.

The six cells expected to miss for a **threshold** reason (A1, A2, B1, B2, D1, D2) and
the three expected to miss for a **rule** reason (E2, F2, I2) all did, 0/5 each. The
boundary cells either side are 30ms apart (A2/A3), 200ms apart (B2/B3) and 3 queries
apart (D2/D3), so a rule that moved by more than that would show.

## 3. What the misses say about where the thresholds are

**LATENCY's two arms behave as `need = max(100ms, 3 x median)` says, and the arm that
binds is the one the rule picks.** The demo's `Post summarizes the body` sits at a 1ms
median, so its need is the 100ms floor: +80ms is silent and +110ms fires 5/5. Give the
same example a 200ms baseline and the ratio arm takes over: +500ms is silent (need 618ms)
and +700ms fires 5/5. Both boundaries land within one cell of the rule's own arithmetic.

**FREQUENCY's exact arm really does fire on a single query.** C1 moves
`GET UsersController#show 2xx` from 3 queries to 4 and lands at rank 1, tier 2,
confidence 0.83 — the same rank and confidence as C2's N+1-sized 3→10. Magnitude feeds
ranking, not confidence, exactly as §2 says.

**FREQUENCY's varying arm is wide.** With a baseline of [4, 5, 6] the rule needs the
current value outside [4, 6] **and** more than 2 x width = 4 from the median. 7 queries —
a real, reproducible +2 — is silent. That is by design, but it means a regression that
lands on a measure someone else has already made noisy is invisible until it is large.
`signals.md` §5's baseline-contamination risk has this shape: one N+1 run in the window
turns an exact baseline into a varying one, and D2 is what the next run then looks like.

## 4. Three results that disagree with `signals.md` §3

**§3's zero false positives do not reproduce.** §3 reports "0 signals in 256 clean
comparisons" and puts the 95% upper bound at about 1.6% per comparison. A dedicated
control sweep — the same two clean cells, 30 trials each, 60 clean comparisons at n=4,
144s, run wall 993–6691ms with a median of 1611ms — produced **2 LATENCY signals**:

| trial | behavior | baseline median | current | confidence |
|---|---|---|---|---|
| Z2 rep 6 | `./spec/models/post_spec.rb # Post requires a title` | 17.6ms | 303ms | 0.62 |
| Z2 rep 21 | same | 17.6ms | 248ms | 0.58 |

Both are a single pause on one fast example: 285ms and 230ms, past the 100ms floor and
17x and 14x the median. §1 measured those pauses at "up to about 70ms". Pooling every
clean control trial run while building this harness — 86 comparisons across four sessions
at different machine loads — gives 3 such signals, about 3%. All three are LATENCY on an
example; no other rule produced one.

The disagreement is not necessarily about the thresholds. §3's backtest replayed
**captured** runs of a suite whose baseline it also drew from that capture; these are
fresh runs on a shared machine with four other agents live. If that is the difference,
then §3's number describes a quieter world than the one `siftr run` ships into, and
§3's own pre-registered prediction — "over the next 50 clean local runs of each suite, at
any load, at most 2 comparisons carry an example LATENCY signal" — is at its limit after
60.

**The §3 recall table's bins are too coarse to predict a specific example.** It gives
median ≥100ms, +300ms → 0.02 and +1000ms → 0.70. B1 (+300ms on a 200ms example) misses
5/5, agreeing; but B3 (+700ms on the same example) hits 5/5, where the table's next row
suggests 0.70 at +1000ms. Both are the rule behaving exactly as written — need is
3 x median, so 618ms for this example and several seconds for iriq's slowest — which
means the table is reporting the **distribution of medians in the bin**, not a property
of a regression. Read `need = max(100ms, 3 x median)` for the example in hand instead.

**Recall at the floor is sharper than "~0.4".** §3 records ~0.4 for a synthetic +100ms on
a sub-100ms example. A3 injects +110ms and measures 111–116ms of delta: 5/5 in the quiet
run and 5/5 again in a loaded one. The 0.4 is a synthetic magnitude landing exactly on
`delta <= need`, which is a knife edge, not a region; 10% past the floor is already
saturated.

## 5. A class of failure siftr cannot report, now recorded as a cell

I1 and I2 break two examples of one file in the same run, differing only in where their
description comes from:

```ruby
RSpec.describe "outcome stability" do
  broken = ENV.key?("SIFTR_FAULT_BREAK")
  it("named example") { expect(1).to eq(broken ? 2 : 1) }
  it { expect(1).to eq(broken ? 2 : 1) }
end
```

The named one is ERROR at tier 1, rank 1, 5/5. The one-liner is never ERROR, 0/5. RSpec
generates a one-liner's `full_description` from the matcher that ran, and the
`test.example` behavior id is built from that description, so the failing example is a
**different behavior** from its passing self and `rules::error` has no baseline to judge
it against. What the reader gets instead, from I2's own record:

```
new          ./spec/models/zz_outcome_spec.rb # outcome stability is expected to eq 2   tier 4
disappeared  ./spec/models/zz_outcome_spec.rb # outcome stability is expected to eq 1   tier 4
```

A failing test, reported as two tier-4 changes with the word "failed" nowhere in it. The
cell is recorded as an expected miss rather than left out, so a fix has something to flip.
Every example in `dogfood/` and `fixtures/` is hand-described, which is why no existing
test sees this.

## 6. Two smaller observations

**An incomplete run is right but noisy.** G1 puts INCOMPLETE at rank 1, tier 1, correctly
naming the file that failed to load — inside a report of **7 groups**, the other six being
tier-4 NEW on the individual lines of the load error (`RuntimeError:`, `Failure/Error:
raise "injected load error"`, the backtrace line, the changed test summary). The headline
is right and the top-3 cut hides most of it, but the run reports 7 changes where there
was one.

**The §7 diffuse-count demotion holds under injection.** H1 adds 3 examples and H2 removes
3; in both, `TRANSACTION BEGIN deferred TRANSACTION` and `TRANSACTION ROLLBACK TRANSACTION`
move 10 ↔ 13 in lockstep with the example count and are demoted to tier 5, while the
example NEW/DISAPPEARED keep tier 4 and the headline. That is the behavior
`dogfood-junior-loop.md` asked for, now measured rather than replayed.

## 7. What this harness is and is not

It is a recall and rank instrument for one suite on one machine. The numbers in §2 are
reproducible to the cell; the numbers in §4 are a rate with 60–86 trials behind them, so
two significant figures at most and a wide interval — 2 of 60 is consistent with anything
from 0.4% to 11%.

It cannot see: anything outside the RSpec + Rails-log source set (`source-detection.md`),
LATENCY on the request population except by accident (one appeared during a loaded run —
`GET UsersController#index 2xx`, 23ms → 137ms, confidence 0.44, on a trial whose injected
fault was a 30ms sleep in an unrelated model method), baselines longer than n=4, and any
suite whose shape differs from the demo's 10 examples.

What it is for: run it before and after a `rules.rs` change and diff the two tables. A
threshold that moved shows up as a boundary cell flipping; a rule that lost its guard
shows up in the control sweep; a kind that stopped firing shows up as a whole row going
to 0/5.
