# A whole-system log is not a context

Measured 2026-09-19 against 0.1.7 + the suite-size demotion, on a 30-minute macOS
unified-log capture (`log show --style syslog`), 583k lines, split into three disjoint
chronological windows of 194,372 lines each and ingested as three runs of one context.
The capture stayed in a scratch directory; nothing below is log content, only counts and
shapes. Sources are labelled by volume rank rather than named.

This answers a question `dogfood-system-logs.md` left open: siftr produces thousands of
signals on log input, and it was not clear whether the normalizer, the rules, or the
source was at fault.

## 1. The normalizer is not the problem

The head is dense and collapses well:

| | share of lines |
|---|---|
| top 10 templates | 29% |
| top 100 templates | 70% |
| top 1000 templates | 88% |

12,901 templates from 177,435 lines in a separate 8-minute capture — about 14 lines per
template. Template extraction is doing its job.

## 2. Two thirds of templates are one-offs carrying a twentieth of the volume

| | |
|---|---|
| templates seen exactly once | 8,581 = **67%** of templates |
| share of lines they carry | **4.8%** |
| templates seen at most twice | 76% |

`examples/families.rs` on the same corpus: 12,902 distinct templates, 1,392 of them one
literal word apart from another, of which 1,121 (32,817 lines) fall in the classifier's
`Unknown` bucket. Consistent with §2 of `grouping.md`, which measured that merging on
exactly this evidence is what destroys behaviors — this is not an argument for merging.

## 3. The discriminator is recurrence, not rate

Across two disjoint windows of the same source, and against RSpec runs as the
counter-example population:

| corpus | singleton templates | recur in the next run |
|---|---|---|
| unified log, window 1 → 2 | 10,432 | **10%** |
| unified log, *recurring* templates (count > 1) | 4,282 | 75% |
| rspec `rails_demo` baseline → baseline_2 | 35 | **100%** |
| rspec `a10_clean` → `a10_fail` | 14 | **86%** |

A log template seen once essentially never returns; one seen twice usually does. An RSpec
singleton returns almost always, because a `test.example` occurs once per run *by
construction* while recurring perfectly across runs.

**This does not revive the rejected recommendation.** `dogfood-system-logs.md` rec 3
proposed refusing a source by its first-run *singleton rate*, and `grouping.md` §6.2
rejected it because RSpec scores 72–89% singleton — *above* this corpus. Confirmed again
here: on rate, logs (67%) score **lower** than rspec (74–93%). Rate is not a
discriminator. Recurrence across runs is a different quantity and separates by 7.5×.

## 4. What the tail costs

One-offs occupy ~71% of the 20,000-behavior cap, and each presents as NEW on the next
run:

```
r3 vs 2 baseline runs (r1 r2): 10362 signals, too many to be findings,
                               so no changes were recorded
```

So on three clean windows of a real log, siftr reports **nothing at all**. An earlier
two-window split hit the behavior cap outright and came back INCOMPLETE.

## 5. Suppressing one-offs is necessary and not sufficient

Reconstructed from the run summaries, since a refused run stores no signals:

| | |
|---|---|
| NEW candidates (absent from both baselines) | 9,659 |
| …one-off in the current run | 7,724 (**80%**) |
| …recurring in the current run | 1,935 |
| DISAPPEARED candidates present in every baseline | 311 |
| **excluding never-recurring one-offs leaves** | **~2,092 — still refused** |

Removing 80% of the flood does not rescue the run. The residual ~1,900 are genuinely new
*recurring* shapes: a whole-system log emits roughly two thousand of them per ten-minute
window. **The flood refusal is correct for this source**, which upholds `grouping.md`
rec 2 rather than contradicting it.

## 6. Per-source, almost all of it becomes serviceable

Same three windows, partitioned by the process the syslog header names:

| src | lines | templates w1 | w2 | w3 | NEW in w3 | of those one-off | signals est |
|---|---|---|---|---|---|---|---|
| S1 | 78,789 | 318 | 320 | 612 | 536 | 337 | 540 |
| S2 | 34,119 | 188 | 239 | 185 | 158 | 155 | 201 |
| S3 | 32,401 | 3 | 3 | 4 | 1 | 1 | **1** |
| S4 | 25,662 | 214 | 238 | 278 | 60 | 48 | 60 |
| S5 | 17,771 | 1,586 | 1,560 | 1,281 | 1,090 | 1,082 | **1,102** |
| S6 | 15,458 | 413 | 382 | 487 | 199 | 166 | 200 |
| S7 | 13,502 | 1 | 46 | 1 | 0 | 0 | 0 |
| S8 | 10,421 | 155 | 161 | 132 | 1 | 1 | 20 |

**Seven of the top eight stay under the 1,000-signal refusal**, against 10,362 for the
whole log. The exception, S5, is the pathological one — 1,586 templates from 17,771 lines
— and 1,082 of its 1,090 NEW are one-offs, so §5's rule drops it to about 20.

**The two mechanisms are complementary, and neither works alone**: one-off suppression
alone leaves the whole log at 2,092 and still refused; per-source partitioning alone
leaves S5 over the line. Together, all eight are serviceable.

## 7. Scope boundary

Source attribution covers **67% of lines** (96% of templates; the strict `proc[pid]` form
and the looser one agree on the source in 12,928 of 12,928 cases, so the partition is not
measuring something else). The other 33% — 561 templates, 65,084 lines — carries no
`host proc[pid]` header.

Those are **not** continuation lines of multi-line entries: zero of them begin with
whitespace. They are a denser population than the corpus average (30% one-offs against
67% overall) with 447 distinct leading tokens, the top 10 carrying 60% of their lines. So
they are well-behaved templates that this partition simply cannot address, not a
normalizer defect. How to identify a source for them is open.

## 7b. Replicated on a second corpus: width is the variable

Everything above rests on one capture, so it was repeated on a structurally different
log — `/var/log/install.log`, 146,467 lines, split into three disjoint windows of 48,822
and ingested the same way. No content from it is quoted.

| | unified log | install.log |
|---|---|---|
| templates in window 1 | 11,051 | **455** |
| singletons | 67% of templates, 4.8% of lines | 34%, **0.3%** |
| singleton recurrence, w1 → w2 | **10%** | **23%** |
| recurring recurrence, w1 → w2 | 75% | 82% |
| distinct sources | 291 | **28** |
| lines attributable to a source | 67% | **96%** |
| outcome of window 3 | 10,362 signals, **refused** | **443 changes, reported** |

The qualitative result replicates: one-offs recur far less often than templates seen more
than once (23% against 82%), and the tail carries a fraction of a percent of the volume.

The magnitudes do not, and that is the useful part. **This corpus does not flood.** It
reports normally with no partitioning, and it has 28 sources where the unified log has
291. So the variable is not "log versus test suite" but **how many sources a context
mixes together** — a log that is already narrow behaves exactly as §6 predicts a
per-source context would. That is a natural experiment in favour of recommendation 1
rather than a counter-example to it.

Two honest qualifications. 443 changes is under the refusal threshold but is not a set of
findings anyone reads; 209 of its 280 NEW candidates are one-offs, so recommendation 2
would cut it substantially, which is further evidence the two belong together. And this
corpus is friendlier in every dimension at once — fewer sources, denser templates, 96%
attributable — so it bounds the claim rather than proving the general case.

## 8. Recommendations

1. **A log context should be per-source, not per-file.** **Needs evidence** for the
   plumbing, but the measurement is unambiguous: 10,362 signals becomes ≤540 for seven of
   the top eight sources. Pre-registered check: replaying the three windows partitioned by
   source must leave each of the top eight under the refusal threshold, and the
   `rails_demo` and `rspec_hunt` scenarios must not change.

2. **A behavior that has never recurred should not consume a cap slot or raise NEW.**
   **Needs evidence.** It removes 80% of signal volume and ~71% of cap pressure, and the
   recurrence separation (10% against 86–100%) shows it does not touch RSpec. It is not
   sufficient alone — see §5 — so it should ship with (1), not instead of it. Pre-registered
   check: `rails_demo` and `rspec_hunt` scenarios byte-identical, since every RSpec
   singleton recurs; the log corpus' S5 must drop from ~1,102 to under 100.

3. **Do not merge one-word-apart templates**, still. 1,121 of 1,392 families are
   `Unknown` to the classifier, and `grouping.md` measured a 15–20% false-merge rate for
   evidence of exactly this kind.

## 9. Reproducing

The corpus is a local system log and is deliberately not committed. Any equivalent capture
reproduces the method: split one capture into three disjoint chronological windows, ingest
each as a run of one context with `ingest --file`, then compare the per-run
`summary -j -n <large>` documents — a behavior's count is at `.behaviors[].stats.count`
and `behaviors` is limited by `-n`, so raise it rather than reading `behaviors | length`
as a total. The RSpec counter-examples in §3 reproduce from the repository alone.
