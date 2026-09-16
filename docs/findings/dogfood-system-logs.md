# Dogfood: macOS system logs

Pointing siftr 0.1.3 (installed from crates.io, not a dev build) at telemetry it
was never designed against, to find what the model is missing. Measured
2026-09-16 on macOS 25.4. Three sources, ingested repeatedly so baselines form:

| context | source | runs | shape |
|---|---|---|---|
| `syslog` | `/var/log/system.log.{6…0}.gz`, one daily rotation per run | 7 | BSD syslog, `Mmm DD HH:MM:SS <host> proc[pid]: msg` |
| `wifi` | `/var/log/wifi.log.{9…0}.bz2`, one daily rotation per run | 10 | ctime, `Www Mmm DD HH:MM:SS.mmm [subsys]/pid …` |
| `unified` | `log show --style syslog`, eight disjoint 5-minute windows | 8 | ISO-8601, `YYYY-MM-DD HH:MM:SS.uuuuuu±ZZZZ <host> proc[pid]: msg` |

Daily rotations are the honest analogue of repeated runs: the same job, the same
machine, one day apart. Every ingest used `--context`, a scratch `--home` and a
scratch project dir. No private log content appears below; templates are siftr's
own output, and every reproduction case is synthetic.

## 1. What it got right

**The template is the right unit on BSD syslog.** 3796 lines of one day's
`system.log` became 53 behaviors. Timestamp, host and pid are all masked, and
the surviving text is the log statement:

```
<timestamp> <host> <proc>[<int>]: Entered:_<Redacted>Disconnected, mux-device:<int>
```

**Repeated identical input produces no signals.** Four ingests of the same file:
run 1 records, run 2 reports "only ERROR can fire until there are 2 baseline
runs", runs 3 and 4 report `0 changes`. `--quiet-unless-changed` printed nothing
at all on the unchanged run and a full report on a changed one. The cron/CI
contract holds.

**A real change was found with correct evidence.** On `syslog` day 3 a
device-attach path's count moved 165.5 → 251 against a baseline of 162–169, and
on day 4 to 745. `siftr explain` prints the rule, the per-run baseline counts and
the exemplar lines with line numbers — provenance end to end, as principle 2
requires.

**Throughput is a non-issue.** 86k lines ingested in 1.84s wall; a 170k-line
window in about 1s.

## 2. What it got wrong

### 2.1 The weekday name is not masked, so every behavior splits seven ways

`wifi.log` uses ctime stamps. siftr masks the time but keeps the weekday, so one
log statement becomes seven behaviors — `Tue Sep <int> <timestamp> …`,
`Wed Sep <int> <timestamp> …`, and so on. Each weekday's first appearance fires
NEW.

Synthetic reproduction, no private data:

```
$ printf 'Thu Sep 10 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready\nFri Sep 11 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready\nSat Sep 12 00:33:52.607 [airport]/637 @[1.6] (foo.m:6110) widget ready\n' > ts.log
$ siftr ingest ts.log --context ts && siftr summary
    COUNT  BEHAVIOR
        1  Fri Sep <int> <timestamp> [airport]/<int> @[<float>] (foo.m:<int>) widget ready
        1  Thu Sep <int> <timestamp> [airport]/<int> @[<float>] (foo.m:<int>) widget ready
        1  Sat Sep <int> <timestamp> [airport]/<int> @[<float>] (foo.m:<int>) widget ready
```

Three behaviors where there is one. The same file with the weekday stripped
(`sed -E 's/^(Mon|Tue|Wed|Thu|Fri|Sat|Sun) //'`), re-ingested as its own
10-run series:

| day | lines | signals, as shipped | signals, weekday stripped |
|---|---|---|---|
| 1 | 11376 | 0 | 0 |
| 2 | 10022 | 0 | 0 |
| 3 | 13515 | 58 | 32 |
| 4 | 8730 | 51 | 29 |
| 5 | 8148 | 39 | 4 |
| 6 | 10181 | 42 | 3 |
| 7 | 10599 | 40 | 3 |
| 8 | 8879 | 20 | 0 |
| 9 | 8569 | 1 | 1 |
| 10 | 8104 | 3 | 0 |
| **total** | | **254** | **72** |

**182 of 254 signals (72%) were artifacts of an unmasked weekday.** The effect is
concentrated where it hurts most: over days 7–10, once a normal baseline would
have settled, the shipped build emits 64 signals and the stripped corpus emits 4.
Days 8 and 10 go to exactly zero — "nothing changed" is the truthful answer for a
quiet day, and siftr said 20 and 3.

The decay pattern is itself the confirmation: signals fall off after seven runs,
which is when the seventh distinct weekday finally enters the baseline.

The masker already handles `Mmm DD HH:MM:SS` correctly (the `syslog` context
proves it). The gap is the ctime variant: an optional leading weekday name, and
fractional seconds.

### 2.2 The host is only masked after a BSD timestamp, not after ISO-8601

Synthetic reproduction, two hosts in three timestamp formats:

```
Sep 10 00:33:52 alpha-host foo[637]: widget ready
Sep 10 00:33:52 beta-host foo[638]: widget ready
Sep 10 00:33:52.607 alpha-host foo[637]: widget ready
Sep 10 00:33:52.607 beta-host foo[638]: widget ready
2026-09-10 00:33:52.607000-0700 alpha-host foo[637]: widget ready
2026-09-10 00:33:52.607000-0700 beta-host foo[638]: widget ready
```

```
    COUNT  BEHAVIOR
        4  <timestamp> <host> foo[<int>]: widget ready
        1  <timestamp> beta-host foo[<int>]: widget ready
        1  <timestamp> alpha-host foo[<int>]: widget ready
```

The four BSD lines collapse to one behavior across both hosts. The two ISO-8601
lines stay split per host. `log show --style syslog` — the obvious way to reach
macOS telemetry — emits ISO-8601, so every stored template from it carries the
hostname.

Two consequences. On any aggregated or multi-host log, every behavior splits per
host. And the hostname lands in the behavior template in the database, where it
is not covered by `SIFTR_REDACT=pii` (emails, public IPs, home directories). On
this machine the hostname is derived from the account name, so it is the kind of
thing `pii` exists to keep out.

### 2.3 Nothing bounds a run's signal count

One `unified` window produced **8,952 changes**. Across the 8-run context siftr
recorded **38,244 signals**, of which 9,366 were still open at the end. `changes`
does cap what it *prints* (top 3, then "… 8,949 more"), but the store, the
still-open list and the headline number are uncapped, and the headline number is
what tells the user whether to look. "8,952 changes" is not a finding; it is a
statement that the comparison failed.

Principle 3 says a signal the developer ignores is a cost. Here the cost is the
entire output.

### 2.4 Grouping never fires outside RSpec

`changes r25 -j`: 8,952 signals, 8,952 groups, maximum group size 1.

The clearest case is small enough to check by hand. On `syslog`, one device-attach
event makes three daemons log, at counts 481 / 481 / 480 / 480 — four behaviors
moving in lockstep, all reported as `165.5 → 251`. They surfaced as four separate
signals, each its own group headline. Grouping keys on the enclosing RSpec
example; a log source has no scope, so every signal is its own group.

### 2.5 Confidence loses its discriminating power when baseline spread is zero

A behavior whose entire template is `)}` — a closing brace, a continuation line
from a multi-line structured dump — produced a FREQUENCY signal at confidence
0.75:

```
s276  FREQUENCY  conf 0.75  in r20, group 7 headline
behavior  )}
change    count 1 → 6
rule      identical in all 2 baseline runs, so any change counts; confidence (n+1)/(n+2) = 0.75
```

When the baseline has no spread, the formula drops its effect-size and spread
terms and becomes a function of the baseline run count alone. A brace fragment
then scores like a real regression. Across all 38,244 signals confidence ran
0.39–0.91 with a mean of 0.82 — it separates almost nothing. Principle 4 asks
confidence to come from effect size, baseline spread *and* run count; with zero
spread only the third survives, and the number keeps the authority of the other
two.

### 2.6 Only three of six signal kinds can fire on an ingested log

Across 38,244 signals: 37,491 NEW, 490 DISAPPEARED, 263 FREQUENCY. Zero LATENCY,
zero ERROR, zero INCOMPLETE.

LATENCY is scoped to an RSpec example and, for suite duration, is "never
standalone" (`signals.md`), so an ingest source cannot produce one by
construction — even though 706 of one window's 9,768 behaviors did carry a
duration. ERROR derives from test outcome and exit code, so a log line whose text
announces an error is just another `log` behavior. `wifi.log` carries lines
reading `… Error …`; none produced an ERROR signal.

### 2.7 Minor: ingested file lines are labelled `stdout`

`siftr explain` cites evidence from `siftr ingest FILE` as `stdout:83`. The stream
was a file. Cosmetic, but provenance is the product.

### 2.8 Minor: "resolved" means only "did not recur"

24,459 of 37,491 NEW signals were recorded `resolved`. On a source where most
templates never repeat, resolution is automatic and says nothing about whether
anyone looked. The word oversells what happened.

## 3. A hypothesis I tested and killed

The `)}` signal suggested that multi-line records — structured dumps, brace
blocks — were fragmenting into junk behaviors, and that record framing would be
the high-value fix. It isn't.

A 5-minute window holds 77,338 lines, of which 11,717 (15%) are continuation
lines. Re-ingesting all 8 windows with continuation lines dropped
(`grep '^2026-09-16 '`):

| | behaviors, window 1 | signals, 8 runs |
|---|---|---|
| as captured | 9,768 | 37,975 |
| continuation lines dropped | 9,067 | 36,706 |

A 7% cut in behaviors and a **3% cut in signals, uniform across all eight runs**.
Framing multi-line records would not have made this source usable. (On first read
I had the runs misaligned and thought the result was worse rather than
negligible; aligned, it is uniformly and unimportantly better.)

The real driver is that the unified log is not a recurring-behavior corpus at
all. **68–70% of its templates occur exactly once in the run that produced them**
(6,641 of 9,768; 9,730 of 13,804). That is genuine message diversity, not a
masking gap: collapsing every digit run and every hex token ≥ 6 chars merged only
5% of one window's behaviors. A dozen standard macOS daemons emit 300–970
*distinct* message shapes each per five minutes.

Singleton rate is the number that separates a source siftr can serve from one it
cannot:

| source | behaviors | seen exactly once |
|---|---|---|
| `wifi`, one day | 46 | 20% |
| `syslog`, one day | 53 | 36% |
| `unified`, 5 minutes | 9,768–13,804 | 68–70% |

## 4. What this means

siftr's model assumes behaviors recur; that is what makes a baseline mean
anything. BSD syslog satisfies it and siftr works there, modulo two normalizer
bugs. The macOS unified log violates it outright, and no feature fixes that —
the useful response is to detect the violation and say so.

Neither a new semantic kind nor a `syslog` interpreter is justified by this data.
The value is in the normalizer and in one precondition check.

## 5. Recommendation

1. **Mask the weekday name and fractional seconds in the ctime timestamp rule.**
   **Ship it.** Removes 72% of signals on a real system log (254 → 72) and 94% in
   steady state (64 → 4), and `wifi.log`-style ctime stamps are not exotic — it is
   the BSD default. The rule already exists for `Mmm DD HH:MM:SS`; this widens it.
   Regression test: the three-line synthetic file in §2.1 must yield one behavior.

2. **Recognize the host field after an ISO-8601 timestamp, not only after a BSD
   one.** **Ship it.** Small, and it keeps the hostname out of stored templates,
   which `SIFTR_REDACT=pii` currently cannot reach. Regression test: the six-line
   synthetic file in §2.2 must yield one behavior.

3. **Refuse to baseline a source that will not baseline.** **Ship it.** siftr
   already computes everything needed: on the first run of a context, if the share
   of behaviors seen exactly once exceeds a threshold, say so and stop, instead of
   recording 9,768 behaviors and emitting 9,000 NEW signals on the next run. On
   this data the separation is wide and clean — 20% and 36% for the sources that
   work, 68–70% for the one that cannot — so a threshold near 50% is not a close
   call. This is the finding that would most change what a user does: it converts
   the worst outcome from 8,952 useless signals into one honest sentence.
   Pre-registered check: `syslog` and `wifi` must pass at that threshold and
   `unified` must fail, on this corpus.

4. **Cap a run's reported signal count and say what was dropped.** **Ship it**,
   as the backstop for sources that slip past (3). A run reporting thousands of
   changes should report that the comparison failed. Cheap, bounds the worst case
   for every source.

5. **Group signals whose per-run count vectors are identical across the whole
   baseline.** **Needs evidence.** The four-daemon device-attach case in §2.4 is
   exactly right, but CLAUDE.md already warns that evidence-based merging is where
   behaviors go to die, and lockstep counts can be coincidence at low counts. The
   measurement that decides it: over the existing RSpec backtest corpus, how often
   does exact count-vector equality group signals that the current
   example-scope rule already groups correctly, and how often does it merge two
   unrelated behaviors? Ship only if the false-merge rate is near zero.

6. **Give confidence an effect-size term when baseline spread is zero.**
   **Needs evidence.** The degenerate case is real, but the replacement has to be
   backtested against `signals.md`'s existing sweeps before the numbers move —
   changing the formula changes every threshold that was tuned against it.

7. **An error-marker outcome for ingested log lines.** **Needs evidence.** ERROR
   cannot fire on a log file today, which will surprise anyone who points siftr at
   one. But a rule keying on the word "error" is exactly the kind of thing that
   fires constantly on clean input. Decide it by measuring: over these three
   corpora, how many lines per run would such a rule mark, and what share of them
   a developer would act on. If a quiet day still marks hundreds, drop it.

Not recommended: multi-line record framing (§3), a `syslog` interpreter, and any
new semantic kind. None is what stands between siftr and this data.

## Reproducing

Ingests used a scratch `--home` and `--context` per source; nothing touched the
default data dir. Peak scratch usage was 1.7 GB — 234 MB of captured log copies
and about 1.4 GB across seven siftr stores (the `unified` store alone reached
198 MB for 8 runs, since captures are retained for the last 20 runs). All of it
was deleted afterwards. The captured system logs were never committed; the two
reproduction cases in §2.1 and §2.2 are synthetic and are the only inputs needed
to see both normalizer bugs.
