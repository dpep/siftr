# Attention: what the feedback ledger knows about which signals get ignored

Principle 3 says precision over recall — "a signal the developer ignores is a
cost" — and the repository has no measurement of how often that happens.
`siftr history --signals` prints lines ending `resolved in r6 without
investigation`, which reads like siftr already knows. This is what backs that
phrase, what it can and cannot support, and what the number comes out as on
2026-09-19 over every siftr store on this machine.

Two answers, and the second is the one that matters:

1. **The phrase is backed by recorded fact, not inferred from the resolution.**
   The `feedback` table holds a row per siftr command run against a signal, and
   `Outcome::investigated` reads those rows. It is independent evidence.
2. **There is no real-use data to aggregate.** The store at the default home
   holds one run, one line, zero signals, zero feedback. Every other store on
   this machine is a scripted agent or test harness. The scorecard's numbers
   below are the shape of the corpus that produced them, not a precision
   measurement of siftr.

## 1. What is recorded, and by what

`src/store/feedback.rs`. Five kinds, each written by a named command at the
moment it runs:

| kind | written by | means |
| --- | --- | --- |
| `surfaced` | `run`, `changes` and their JSON, via `cmd::record_shown` | siftr printed this signal to someone |
| `investigated` | `explain` | somebody walked the signal to its evidence |
| `evidence_requested` | `evidence` | somebody printed a behavior's raw lines |
| `acked` | `ack` | being acted on |
| `dismissed` | `dismiss` | judged not worth acting on |

`Outcome::investigated` (`src/bin/siftr/cmd/history.rs`) is true when any
`investigated`, `evidence_requested` or `acked` row exists for the signal's
behavior between the signal's own run and the run it resolved in. Nothing is
derived from how fast it resolved; a signal that resolves in the very next run
still reads `after investigation` if `explain` ran on it first. So the phrase is
not a restatement of the resolution.

Three limits on what the row can carry, all of them worth knowing before quoting
a rate:

- **It records siftr being asked, not attention being paid.** A developer who
  reads the change in the run summary and goes and fixes the code, never running
  another siftr command, is recorded exactly like one who ignored it. So the
  examined rate is a *floor* on attention. It can only ever say "at least this
  many were looked at", never "the rest were ignored".
- **Attribution is by behavior, not by signal.** `Store::feedback_on` keys on
  `behavior_id` and a time window; the `signal_id` column is written but not
  read. Two signals of different kinds on one behavior in one run — a FREQUENCY
  and a LATENCY on the same query — both read as examined if either was
  explained. The comment on `Feedback::signal` says this is deliberate: which
  signal a behavior-level command served "is left to the reader to derive".
- **Retention takes the evidence with the runs.** When the baseline runs a
  verdict reads have been pruned, `outcomes` returns `Outcome::pruned` with an
  empty feedback list, so investigation reads false when it is really unknown.
  The scorecard therefore counts those signals as `unjudged` and leaves them out
  of every rate rather than filing "we can't tell" under the flattering answer.

## 2. The scorecard

`siftr history --scorecard` totals, per signal kind, exactly the rows
`history --signals` prints over the same window (`-n`, `--context`). A rate is
rounded where it is built, to the significant figures its denominator backs: one
figure below ten signals, two below a hundred, three above. 3 of 4 is `0.8`, not
`0.75` — the fourth signal would move it by a quarter. Two rates over different
denominators therefore need not sum to 1; the counts printed beside them are the
answer, and a rate that hid its own coarseness would be the worse trade.

`resolved`, `recurred` and `open` are the *latest* verdict, the same one
`history --signals` prints, so they partition `judged` and a change fixed and
broken again counts once, under `recurred`. `resolved unexamined` is therefore a
subset of `resolved`, not of everything that ever resolved.

## 3. What it says here, and why that is not a precision number

Every store on this machine, 2026-09-19: 143 store files found, 129 project
directories still present, 96 stores holding at least one signal.

| kind | raised | examined | resolved | resolved unexamined |
| --- | --- | --- | --- | --- |
| new | 466 | 1 | 14 | 14 |
| frequency | 210 | 14 | 52 | 50 |
| disappeared | 185 | 0 | 23 | 23 |
| incomplete | 30 | 11 | 14 | 12 |
| error | 21 | 0 | 5 | 5 |
| latency | 17 | 1 | 3 | 3 |
| **all** | **929** | **27** | **111** | **107** |

781 of the 929 are still open, none are unjudged, and across all 143 stores the
feedback table holds 1163 `surfaced` rows against 26 `investigated`, 5
`evidence_requested`, 0 `acked` and 0 `dismissed`.

**None of this is evidence about siftr's precision**, and the shape of the table
says why. These stores were written by agents and test harnesses replaying
fixtures: a scripted session ingests a few captures, runs `explain` if the script
said to, and ends — which is why four signals in five are open (nothing came
after them) and why `dismissed` is empty (no script has ever dismissed
anything). The `disappeared` and `error` rows reading 0 examined is not a finding
about those kinds; it is a finding about which commands the dogfood scripts call.

The one store at the default home — `~/.local/share/siftr` — holds a single
1-line `ingest` run with no signals and no feedback. There is no human-use
corpus on this machine at all.

## 4. What would make the number mean something

- **Daily use of one real context.** The scorecard needs runs a person actually
  waited for and signals a person actually read. A month of `siftr run --
  bundle exec rspec` on one project would do it; nothing else will.
- **Separate `surfaced` by interface.** The column is already stored
  (`human`/`json`). An agent reading `-j` and a person reading a terminal are
  different readers with different costs, and the ledger can already tell them
  apart — the scorecard does not yet split on it, deliberately, because with 0
  human sessions the split would print two columns of noise.
- **`dismiss` has never been used.** It is the only row that records a signal
  judged *wrong*, which is the one fact a precision number actually needs. Until
  it is used, the scorecard measures whether siftr was asked for more, not
  whether what it said was right.
