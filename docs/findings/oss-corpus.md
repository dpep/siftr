# A public repo's own history as a corpus

Every corpus we had was one we wrote: we injected a fault, siftr found it, we
concluded siftr finds faults. This replays a public Ruby/RSpec project's real
commits instead — regressions found by strangers, fix commits labelled by
maintainers who never heard of us.

Corpus: **faraday** (`lostisland/faraday`, MIT), 44 first-parent commits of
`main`, `ad8fe1e` (2025-07-04) … `d37e1e0` (2026-09-05). Harness:
`dogfood/oss_replay/replay.sh` (chronological replay) and `revert.sh` (undo one
real fix, keep its test). Clone the repo outside this tree; the scripts take its
path.

## The environment was cheap, which was the surprise

Getting an OSS suite green at an arbitrary old commit is supposed to be the
whole cost of this exercise. It wasn't, because two constraints did all the work:

- **A pure-Ruby library, not a Rails app.** No database, no native gems that
  matter, one `bundle install` shared by all 44 commits. Warm rspec wall time
  median **1.3s** (n=20, range 0.78–2.9s); 44 commits replayed in ~35 minutes,
  nearly all of it `bundle install` re-checks rather than the suite.
- **A window inside one Ruby's era.** Ruby 3.4 changed `Hash#inspect`
  (`{:status=>400}` → `{status: 400}`) and `NoMethodError` quoting, so a 2024
  commit fails on Ruby 3.4.9 for reasons that have nothing to do with the
  project. Staying inside 2025-07…2026-09 avoids it entirely.

One pin was needed: `json ~> 2.7`. The `json` 3.0 gem changed `JSON.parse`'s
arity, which faraday itself only adapted to at `b25b1b2` (2026-08-12); without
the pin every earlier commit fails on a dependency's break rather than its own
code. That is the general shape of the tax — **pin the era's dependency, bound
the window to one runtime** — and it is a line of config, not a project.

All 44 commits ran green (`exit 0`), 598→646 examples.

## What siftr said: 138 signals over 44 runs

| | count | share |
|---|---|---|
| signals total | 138 | |
| …artifact of one normalizer gap (below) | 84 | 61% |
| …genuine | 54 | 39% |
| runs carrying **only** the artifact | 31 of 44 | 70% |

Of the 54 genuine signals, **53 named a spec file the commit actually changed.**
The one that didn't was a LATENCY signal at `bc27144`, a commit that edits only
`.github/workflows/*.yml` — nothing that can change runtime. It fired at
confidence 0.55, siftr's own low end, which is the system being honest about a
guess. One latency false positive in 44 runs is a good rate for a suite this
short.

On the ten commits that fix something:

- **Seven hits.** Each named the exact spec file of the fix. The sharpest was
  `0e008c5` (rename `UnprocessableEntity` → `UnprocessableContent`), reported as
  a DISAPPEARED plus two NEW in `raise_error_spec.rb` — the rename read as a
  rename. Three more (`a6d3a3a`, `3f1280c`, `36764bf`) are security fixes merged
  from private forks, whose entire commit message is "Merge commit from fork";
  siftr's report named `connection_spec.rb`, `request_spec.rb` and
  `nested_spec.rb` respectively, which is strictly more than the commit message
  says.
- **Three misses**, all the same kind: `674fc15`, `b25b1b2` and `9458f04` change
  one line of `lib/` and ship no test. `9458f04` ("Preserve caller-owned JSON
  parser options", `parser_options` → `parser_options&.dup`) is a real bug a
  real user hit, and nothing in the run differs. Not a rule to fix — CI couldn't
  see it either.
- `a01039c` is docs-only and siftr said nothing. Correct.

**The honest discount on the seven hits: every one is a NEW test example.** On a
green suite siftr is reporting "these tests are new", which `git diff --stat
spec/` also reports. Correct attribution, low marginal information. The
chronological arm cannot measure the thing we care about, because a project's
own `main` is green at every commit — nobody merges red.

## So revert a real fix and keep its test

`revert.sh` baselines four green runs at a fix commit, then restores only the
`lib/` file from its parent. The bug is the project's, the regression test is the
project's; ours is the reverting.

**`36764bf`** (params-encoder security fix) → 29 failures. 37 signals, top three
all ERROR, 35 of 37 naming `nested_spec.rb` and `connection_spec.rb` — the specs
of the reverted file. This is siftr working as designed.

**`edd8cc5`** (`Fix Faraday::Response#to_hash when request is not finished`) → 2
failures, and siftr reported **no error at all**. Seven signals; the top three
headlines were a SimpleCov chatter line and two artifacts. The two real failures
appeared as NEW + DISAPPEARED pairs below the fold.

## The bug: a `test.example`'s identity moves when its outcome does

`src/interpret/rspec.rs:146` builds the behavior id from RSpec's
`full_description` (via `unnumber`, which only strips secret-placeholder
numbering). For a one-liner example — `it { is_expected.to eq(x) }` — RSpec
*generates* that description from the matcher that ran. So:

- **It changes when the example fails.** The passing behavior is
  `… is expected to eq 1`; the failing one is `… ` with no generated tail. Two
  ids, so `rules::error` — which compares one id's failures against its own
  baseline — can never fire. A regression reads as NEW + DISAPPEARED, and the
  word "failed" never appears.
- **It changes every run when the matcher's argument has an address.**
  `is_expected.to eq(subject)` yields `… is expected to eq #<Object:0x0000…>`.
  A fresh id per run, forever.

Both come from the same place: the `test.example` template is taken verbatim,
while the generic path's `Normalizer` would have masked the address (it renders
`<hex>` correctly — the gap is only that this path doesn't use it).

The address half is the 84 signals above: faraday has exactly two such examples
at HEAD (`Faraday::RackBuilder` and `Faraday::Response`, the only two of its ten
`eq #<…>` descriptions whose class doesn't define a custom `inspect`), so
**every run from the third onward opened with two NEW signals at up to 0.92
confidence**. Confidence *rises* with baseline size, so the more evidence siftr
accumulates that this is noise, the more sure it becomes that it is a change.

### Repro — two examples, one file, no faraday

```ruby
# spec/pair_spec.rb
BROKEN = ENV.key?('BREAK')
RSpec.describe 'outcome stability' do
  it('named example') { expect(1).to eq(BROKEN ? 2 : 1) }
  it { expect(1).to eq(BROKEN ? 2 : 1) }
end
```

Four green runs under `siftr run -- bundle exec rspec`, then one with `BREAK=1`.
Both examples fail identically; siftr reports:

```
s1  ERROR        ./spec/pair_spec.rb # outcome stability named example
                 failed with RSpec::Expectations::ExpectationNotMetError; passed in 4 of 4 baseline runs
s2  NEW          ./spec/pair_spec.rb # outcome stability is expected to eq 2
s3  DISAPPEARED  ./spec/pair_spec.rb # outcome stability is expected to eq 1
```

The address half needs no `BREAK` at all — `it { is_expected.to eq(subject) }`
alone yields a NEW signal on every run of an unchanged, passing suite.

## Two smaller things

- **`--format documentation` is counted twice.** faraday's `.rspec` sets it, so
  each example's name reaches siftr both on stdout and from the listener: 1301
  behaviors for 646 examples at HEAD. The stdout copies land as kind `log`, which no
  rule attributes to a spec file — and the SimpleCov line that took the top
  headline in the `edd8cc5` revert came from that side.
- **`summary -j` omits `kind`.** The human table prints `log` / `test.example`
  per behavior; the JSON has `kind: null` for all of them, so a consumer can't
  do what the human can. Not investigated further.

## Is it worth extending?

Yes — but for the revert arm, not the chronological one.

The chronological replay bought one thing, and it was worth the afternoon: a
**noise floor measured against real code we didn't write**. 31 of 44 runs
carrying nothing but a normalizer artifact is a number no synthetic corpus could
have produced, because we would have built the fixture without the artifact in
it. It also bought the `bc27144` latency false positive. Having spent that, a
second chronological pass over another repo would mostly re-measure the same
thing; do it once more only after the rspec identity bug is fixed, to confirm the
floor drops to roughly zero.

The revert arm is where the leverage is, and it is cheap now that the harness
exists: any fix commit that ships a regression test yields a real bug with a real
test for the price of one `git checkout <fix>~1 -- <path>`. Faraday alone has
several more. That is the arm that answers "would siftr have caught it", and it
is the one that found the bug above.

What this does **not** establish: anything about SQL, HTTP or resource behaviors.
A pure-Ruby library exercises only the rspec interpreter. A Rails app remains the
thing we actually want, and remains the expensive thing to stand up.
