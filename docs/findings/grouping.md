# Grouping outside RSpec: should lockstep counts make one change?

siftr promises that "related signals group under one headline", but grouping keys on
the enclosing RSpec example, so on a log source every signal is its own group —
`dogfood-system-logs.md` §2.4 saw one device-attach event surface as four separate
headlines, and a five-minute unified-log window as 8,952 groups of one. The proposal
was to group signals whose **per-run count vectors are identical**. CLAUDE.md warns
that evidence-based merging is where behaviors go to die, so §2.4's recommendation 5
made it conditional on a measured false-merge rate. This measures it.

Measured 2026-09-17 against `main` at 401c5ca (0.1.5), macOS 25.4. No Rust was written
and no behavior changed. Three corpora:

| corpus | what | runs | lines |
|---|---|---|---|
| fixtures | `fixtures/rails_demo` and `fixtures/rspec_hunt` replayed with `ingest --dir` | 17 scenarios, 3–4 baseline runs each | — |
| `syslog` | `/var/log/system.log.{7…0}.gz`, one daily rotation per run | 8 | 15,836 |
| `wifi` | `/var/log/wifi.log.{9…0}.bz2`, one daily rotation per run | 10 | 94,256 |
| `unified` | `log show --style syslog`, 8 disjoint 3-minute windows | 8 | 448,895 |

The log captures stayed in a scratch directory and are not quoted here; every number
below is a count or a shape.

## 1. The rules compared

- **exact** — identical vector of the signal's own measure over the baseline runs and
  the current run. The rule as proposed.
- **pair** — identical (baseline median, current) only: the looser "just the latest"
  variant, which is what §2.4's `165.5 → 251` actually records.
- **tolerant** — elementwise equal within `max(1, 2%)`, since the real four-daemon case
  was 481 / 481 / 480 / 480, not four equal numbers.
- **freq-only** — exact, but only FREQUENCY and LATENCY may merge; a presence vector
  (`0,0,0 → 1`) carries no magnitude to agree on.
- **floor5** — exact, but only vectors whose largest value is ≥ 5 may merge.

## 2. False-merge rate where the right answer is known

Truth is siftr's own grouping on the fixture scenarios — example scope, stderr prefix
and spec-file collapse — which `signals.md` §3 and §7 verified case by case. 18
replays, the 17 that produce signals carrying 48 signals in 29 groups (the
clean-against-clean replay produces none). Every signal pair is either same-group or
different-group under truth; a **false merge** is a pair the candidate rule puts
together that truth keeps apart.

| rule | merged pairs | correct | false | same file/message | unrelated | **false-merge share** |
|---|---|---|---|---|---|---|
| **exact** | 143 | 122 | 21 | 15 | 6 | **15%** (21/143) |
| pair | 151 | 121 | 30 | 15 | 15 | **20%** (30/151) |
| tolerant | 144 | 122 | 22 | 15 | 7 | **15%** (22/144) |
| freq-only | 1 | 1 | 0 | 0 | 0 | 0% |
| floor5 | 1 | 1 | 0 | 0 | 0 | 0% |

Of the 21 false merges under **exact**, 15 are examples of one spec file — merges an
identity rule would make correctly — and 6 join behaviors with nothing in common.

**The recall is an illusion.** Of the 122 correct pairs, **120 are one scenario**:
`a20_warn1 ×3 → a4_warn3`, where 16 examples of a deleted `b_spec.rb` all read
`1,1,1 → 0`. siftr already groups those, by the spec-file collapse (`signals.md` §7),
not by their numbers. Of the remaining two, one is the two-call-site deprecation
warning that the stderr-prefix rule already groups, and one is the N+1's request and
example query counts. **The rule reproduces nothing that identity rules don't already
deliver.**

Group-level, the same result from the other side: exact reproduces 21 of 29 truth
groups, but 19 of those are groups of one, which any rule reproduces by doing nothing.
Of the 3 truth groups with more than one member it reproduces 2 — and misses the
flagship: **the N+1, siftr's one fully-verified correct grouping, splits from 1 group
into 3**, because `queries 3,3,3 → 10`, `count 1,1,1 → 9` and `count 1,1,1 → 0` are
three different vectors describing one cause.

Where it merges, it merges the wrong things together. In `hunt_grown` (a suite that
grew from 4 to 10 examples in the run that also regressed) truth is 8 groups; exact
collapses 7 of them into 1, because six new examples and a new deprecation warning all
read `0,0,0 → 1`. The merged group's headline is the lowest tier in it — the
deprecation warning — so "six new examples of `b_spec.rb`" is filed as supporting
evidence under a warning it has nothing to do with. That is the silent wrong answer
CLAUDE.md's warning is about. Under **pair** the ERROR on `a_spec.rb # a a1` joins the
same group, putting a test failure under a warning's headline.

## 3. What it would gain on log-shaped input

| corpus | run | signals | groups now | exact | pair | tolerant | by (kind, source) | biggest exact cluster | processes it spans |
|---|---|---|---|---|---|---|---|---|---|
| `syslog` | all | 15 | 15 | 7 | 7 | 6 | — | 4 | 2 |
| `wifi` | all | 41 | 41 | 9 | 9 | 7 | — | 14 | 2 |
| `unified` | r7 | 851 | 851 | 41 | 37 | 26 | 46 | **569** | 17 |
| `unified` | r8 | 918 | 918 | 46 | 45 | 32 | 43 | **600** | 25 |

The compression looks spectacular and means nothing. On `unified` r8, 918 signals
become 46 clusters, but **one cluster holds 600 of them** — every behavior that
appeared once in this run and never before (`0,…,0 → 1`), spanning 25 distinct
processes. r7's two largest are 569 NEW at `0,…,0 → 1` and 178 DISAPPEARED. Across
both runs, **1,527 of 1,769 log signals (86%) have a degenerate presence vector**, so
the thing being grouped is "appeared once", which is not a cause.

For comparison, grouping the same signals by (kind, source process) — identity, no
numbers — produces 43–46 groups, the same order of magnitude, and every group is a
statement a reader can check: "N new message shapes, all from one daemon."

**The flood is already bounded upstream.** 0.1.5's `MAX_SIGNALS` cap meant four of the
eight `unified` runs recorded **no** signals at all rather than 2,481–4,907 of them.
The 8,952-groups-of-one case from §2.4 can no longer happen; what remains is a run of
851–918, and grouping is not what makes that readable.

## 4. Coincidence: how often unrelated behaviors move alike

Behaviors present in **every** run of a context fire no signal and imply no shared
cause, so any identical count vector among them is pure coincidence.

| corpus | runs | behaviors in all runs | sharing a vector with another | clusters |
|---|---|---|---|---|
| `syslog` | 8 | 22 | 18 (82%) | 3 |
| `wifi` | 10 | 33 | 20 (61%) | 7 |
| `unified` | 8 | 322 | 250 (78%) | 43 |

Neither of the two suggested defences works. **More runs barely help**: on `unified`,
requiring agreement across 2 runs collides at 88%, across all 8 at 78%. **Magnitude
helps only far past where signals live**:

| max count ≥ | 1 | 2 | 3 | 5 | 10 | 25 | 100 |
|---|---|---|---|---|---|---|---|
| `unified` collision rate | 78% | 74% | 74% | 64% | 47% | 40% | 29% |

Even at counts ≥ 100 — where almost nothing is left (21 of 322 behaviors) — three in
ten still coincide.

**And the rule misses the event it was designed for.** Injecting a synthetic
four-behavior lockstep event into `unified` r8's real signal set, 200 trials per cell
(four behaviors of one process, all pushed to the same count):

| jitter | ×1.5 | ×2 | ×3 |
|---|---|---|---|
| none (all four exactly equal) | 20% | 25% | 24% |
| ±1 | 0% | 2% | 2% |

With the four given *identical* current counts, exact vector equality still groups them
only a fifth of the time, because their baseline counts differ. With ±1 of
jitter — which is what §2.4's 481 / 481 / 480 / 480 is — it essentially never fires.
The motivating example fails the rule proposed to catch it. Consistently, exact
equality separates 12,777 (r7) and 18,882 (r8) signal pairs that *are* within 2% of
each other elementwise.

Loosening to tolerant equality trades that away directly: it cuts `unified` r8 to 32
clusters and raises the fixture false-merge share to 15% with 7 unrelated pairs, and
`pair` — the variant that would have caught §2.4's four daemons — is the worst of the
three at 20%.

## 5. What this means

The count vector is the wrong evidence. It is simultaneously too strict to catch
lockstep (counts of independent processes differ by one) and too loose to be safe
(most behaviors that appear once share a vector with hundreds of others). Every case
where it looked right is a case where a shared *identity* — one spec file, one message,
one process — was the actual evidence, and siftr already uses identity for exactly this
in the spec-file collapse.

Precision over recall (principle 3) settles it: a wrong group is worse than no group,
because it files a real finding under an unrelated headline and the reader never learns
it was there.

## 6. Recommendation

1. **Don't ship count-vector grouping**, in any of the three forms. **Measured
   false-merge rate 15% of merges (21 of 143 pairs) on the corpus where the right
   answer is known; 20% for the `pair` variant that §2.4's numbers imply.** It splits
   the one grouping siftr is verified to get right (the N+1: 1 group → 3), it files six
   new examples under a deprecation warning's headline, it recovers a true lockstep
   event in 0–2% of trials once the counts differ by ±1, and on the log input it was
   meant for, its "gain" is a bucket of 600 unrelated NEW signals. This closes
   `dogfood-system-logs.md` recommendation 5.

2. **Keep treating a flood as a failed comparison, upstream.** **Ship it** — the cap is
   already in 0.1.5 and, on this corpus, it is what turns four unusable runs into an
   honest refusal. That is the right layer for a flood, rather than trying to make
   thousands of signals presentable by grouping them. Nothing in this measurement
   changes those thresholds.

   **Not** by singleton rate, though: `dogfood-system-logs.md` recommendation 3 proposed
   refusing a source whose first-run singleton rate is high, and that was measured and
   **rejected** — real RSpec runs score 72–89% singleton (the demo suite 72–77%), *above*
   the 68% unified-log corpus the threshold was meant to reject, because a `test.example`
   occurs once per run by construction while recurring perfectly across runs. The rate is
   not even monotone in what it predicts: 100% singleton produced 0 signals forever, 96%
   produced 11, 68% produced 8,564. Recommendation 3 is closed, not pending.

3. **Group NEW/DISAPPEARED behaviors that share an identity, as the spec-file collapse
   already does for DISAPPEARED.** **Shipped**, for the RSpec half. The evidence pointed
   here: 15 of the 21 false merges above are pairs a same-file rule would have made
   correctly, and on log input a (kind, source) rule yields groups of the same
   cardinality as the vector rule while every group stays explainable. Pre-registered
   check before it ships: `hunt_grown` must go from 8 changes to 3 (the ERROR, the
   warning, and "6 new examples of `./spec/b_spec.rb`"), the N+1 must stay at 1 group
   with its current headline, and the other 15 fixture scenarios must not change at
   all. For a log source the identity is the source field, which the generic
   interpreter does not expose yet — that parse is the prerequisite, not the grouping.

   **Result (2026-09-19).** `hunt_grown` went 8 changes → **3**, exactly the three named,
   at 8 signals either way: the rule regroups, it never drops. The N+1 is unmoved — 1
   group, same headline, same supporting members, same attribution — and so is the
   traffic corpus (`b1…b3 → b4` 1 group, `t1…t3 → t4` 1 signal). The collapse keys on
   `(kind, spec file)`, so examples renamed within a file stay two changes rather than
   one, and no comparison of counts enters the decision.

   **The "15 scenarios unchanged" half of the prediction was too strong, and the way it
   broke is the rule working.** Replaying all 24 scenarios against three baselines (50
   replays) leaves every `fixtures/rails_demo` scenario byte-identical, and changes 10
   `rspec_hunt` replays beyond `hunt_grown` — every one of them a replay whose baseline
   ran a *smaller* suite (4 → 10, 4 → 20, 10 → 20), where the added file's examples
   collapse exactly as designed (17 changes → 2 at 4 → 20). Not one replay whose
   baseline ran the same examples changed. The pre-registration had assumed each
   scenario replayed against a size-matched baseline; under that pairing it holds
   literally.

   **`hunt_grown` is reproducible from the repository**, which §7 previously left only to
   the deleted scratch directory: replay `fixtures/rspec_hunt/a4_clean` three times, then
   `fixtures/rspec_hunt/a10_fail_warn`, into one context by naming each directory
   (`siftr <dir> --context hunt`). That is the suite growing 4 → 10 examples in the run
   that also regressed. Re-measured 2026-09-19 on `main` after the suite-size demotion
   shipped: **8 changes in 8 groups**, headed by `ERROR ./spec/a_spec.rb # a a1`, so the
   target above stands unchanged. That is the *before* number — with the collapse now
   shipped the same replay gives **3**, as the Result above records. The control that
   confirms the identification is `a10_fail` in place of `a10_fail_warn` — identical but
   without the deprecation warning — which gives 7.

4. **If numeric agreement is ever revisited, it must be a tie-breaker inside a shared
   identity, never the key.** **Needs evidence.** The measured collision rate only
   falls to 29% at counts ≥ 100, which no threshold on its own can rescue. What would
   change the answer: a corpus where behaviors carry a shared request, trace or session
   id, so that "moved together" can be checked against something other than arithmetic.

## 7. Reproducing

Scripts live in the measurement scratch directory, not the repo: `collect_fixtures.py`
(replays each scenario into a temp `SIFTR_HOME`, then dumps `changes -j` beside the
store's per-run aggregates), `analyze_fixtures.py` (the five rules, pair-level truth
comparison and adjudication), `analyze_logs.py` and `analyze_clusters.py` (per-run
grouping, cluster shape, collision rates) and `inject_event.py` (the synthetic lockstep
event). Each log corpus was ingested oldest-rotation-first into its own scratch
`--home` and `--context`; nothing touched the default data dir, no capture was
committed, and all of it was deleted afterwards. The fixture half reproduces from the
repository alone.
