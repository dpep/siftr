# Watching a developer work: 25 signals, none of them useful

Measured 2026-09-19 against 0.1.7 plus `--no-report`, on a fresh Rails 8.1 + rspec-rails
app built from scratch by a separate agent playing a junior developer. Design, scoring
rules and predictions were **written down before the run** and are reproduced below
unchanged; nothing here was decided after seeing results.

The question every earlier measurement dodged: siftr can find a regression in a captured
corpus, but does its output land usefully on someone in the middle of doing the work?

## Setup

A junior built two tickets in sequence — a posts index, then comments rendered under each
post — writing specs throughout and keeping a timestamped worklog. They ran the suite
through `bin/test`, which nests siftr *inside* the wrapper:

```sh
exec siftr run --no-report -- bundle exec rspec "$@"
```

`--no-report` (added for this) keeps siftr silent, so the developer saw ordinary rspec
output and nothing else. The store lived outside the working tree. Sequential tickets
were deliberate: ticket 1 establishes a baseline suite over several runs so ticket 2's
changes have something to compare against.

**siftr must nest inside the wrapper, not outside it.** `siftr run -- bin/test` reads
only stdout, stderr and rusage — see `source-detection.md`. Verified before the run:
every run recorded `rails_log rspec rusage stderr stdout`.

## What happened

| runs | signals | groups |
|---|---|---|
| r1–r4 | 0 | — |
| **r5** (ticket 1 done) | **13** | **13** |
| r6 | 0 | — |
| **r7** (ticket 2 done) | **12** | **12** |
| r8 | 0 | — |

**25 signals. None worth a developer's attention.** Roughly 80% were NEW behaviors from
specs the junior had just written — correct, deliberate work reported as change. The
rank-1 headline in *both* reporting runs was `TRANSACTION ROLLBACK TRANSACTION`: SQLite
transaction bookkeeping from rspec's transactional fixtures.

Note the shape. The silent runs are the ones that re-ran an unchanged suite; the loud
ones are the ones where work happened. **siftr is quiet when nothing is going on and
loud exactly when someone is working.**

## Why nothing grouped: changes in development are diffuse

Every one of the 25 signals had `attribution: null`, so every signal keyed on
`Key::Behavior(id)` — unique per behavior — giving one group per signal and making every
signal a headline.

The first four explanations were all wrong, and each died against a control (the
`rails_demo` fixtures, which group correctly): a missing `log_ino` (absent from the
working fixture too — siftr rewrites that key suffix, so stored captures are
post-rebase), zero-width example spans, a live-vs-`ingest` divergence (replaying the
junior's *own* captures through `ingest` reproduced the failure exactly), and broken
offset rebasing (offsets were contiguous and well-formed in both).

Mapping the run's SQL lines onto the example ranges rebuilt from the listener events
settles it:

```
ROLLBACK TRANSACTION   7 occurrences -> 7 distinct examples   (0 outside any example)
Post Load              3 occurrences -> 3 distinct examples
Post Create            6 occurrences -> 2 distinct examples
```

**Zero lines fell outside a scope**, so scoping works perfectly. `ROLLBACK` went 1 → 7 by
adding exactly one occurrence to each of seven examples. No example owns that change, so
there is nothing to attribute it to, and siftr correctly declines to name one.

Compare the control, where the same machinery works: the N+1's extra queries all land in
a single request spec, all three related signals carry `attribution.scope` pointing at
that one example, and they collapse into **one** change with three supporting members.

So the rule is:

> **Attribution requires a change concentrated in one example. Development makes changes
> diffuse.** Add five specs and every shared behavior moves a little in every example.

Grouping is downstream of attribution, so the mechanism that makes siftr readable
disengages precisely when a developer is working. This is a mismatch between the model
and the workflow, not a defect: every individual judgement siftr made here was correct.

## Predictions against results

| | Predicted beforehand | Outcome |
|---|---|---|
| P1 | NEW floods, >50% of signals | **Confirmed**, ~80% |
| P2 | The N+1 is caught | **Void** — no N+1 was written |
| P3 | Rank-1 isn't the useful change | **Confirmed**, both reporting runs |
| P4 | Findings arrive shattered | **Confirmed**; cause was diffuseness, not scope |
| P5 | Timing mismatch | Untested — no real defect occurred |
| P6 | Confident false alarms | **Confirmed** — all 25 |

## Two flaws in the experiment, for whoever runs the next one

1. **Subagents inherit the repository's `CLAUDE.md`.** A junior spawned from this repo
   receives siftr's design document as project instructions — including the gate section
   that names N+1 as what the dogfood loop detects. Any junior launched this way is
   pre-briefed about the thing being measured.
2. **A capable model may never make the mistake.** The junior added `.includes(:comments)`
   while writing the view, logging it as "a classic N+1". Contamination aside, N+1 is one
   of the most familiar patterns in Rails; designing an experiment that waits for a
   competent model to stumble into one is a weak premise. **Seed the regression instead.**

## Recommendations

1. **A behavior that moves uniformly across every example is bookkeeping, not a finding.**
   **Needs evidence**, but it is where this measurement points: `ROLLBACK` and `BEGIN`
   changed in 7 of 7 examples and headlined both runs. A rule keyed on that ratio would
   have removed both headlines and cost nothing else here. Pre-registered check before it
   ships: the `rails_demo` N+1 must stay 1 group with its current headline, and the 17
   fixture scenarios must not change.

2. **Do not treat NEW behaviors from newly-added spec files as findings.** They were the
   bulk of the noise and are, by construction, the developer's own deliberate work. This
   is the identity-based collapse `grouping.md` §6.3 already argued for, now with a second
   corpus behind it.

3. **Don't tune grouping to fire on diffuse changes.** The refusal to attribute is
   correct; forcing a group would file unrelated behaviors under one headline, which
   `grouping.md` measured at a 15–20% false-merge rate.

## Reproducing

The scaffold, worklog, store and alignment script live in a scratch directory, not the
repo. The fixture half — the control that killed four hypotheses — reproduces from the
repository alone: replay `fixtures/rails_demo/{baseline,baseline_2,baseline,n_plus_one}`
into a temp `--home` with `ingest --dir` and compare `explain s1` against a live run.
