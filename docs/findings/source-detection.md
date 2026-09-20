# A wrapped test command is invisible, and nothing can tell siftr otherwise

Measured 2026-09-19 against 0.1.7, on a fresh Rails 8.1 + rspec-rails app. Found while
building a harness, not while looking for it.

## What happens

Most teams put their test entry point behind a script — `bin/test`, `bin/rspec`,
`make test`. Pointed at one, siftr reads nothing but the command's own output:

```
$ siftr sources -- bin/test
  rspec      on   does not apply  … (the command isn't an rspec run)
  rails_log  on   does not apply  … (the command isn't a Ruby test run)
```

Same directory, same app, naming the underlying command instead:

```
$ siftr sources -- bundle exec rspec
  rspec      on   applies         … (the command runs rspec)
  rails_log  on   applies         … (log/ is there and the Gemfile names rails)
```

On a Rails project the difference between full telemetry and **no SQL at all** is whether
the developer typed the wrapper their team told them to use.

## Why

`sources::for_command` gates every source on `suite(argv)`. When that returns `None` the
`Some(suite)` branch is never entered, so neither `Rspec` nor `RailsLog` is constructed;
only `Rusage` is appended afterwards. What `suite` accepts:

| command | verdict |
|---|---|
| `rspec`, `bundle exec rspec` | `Rspec` — listener **and** Rails log |
| `rake spec`, `rails spec`, `…spec:foo` | `Rspec` |
| `rake test`, `rails test`, `…test:foo` | `Other` — Rails log, **no listener**, so no per-example scope |
| anything else, including every wrapper script | `None` — stdout, stderr, rusage only |

**The restriction is deliberate and its reasoning is sound**, as the code says: *"Only
commands known to run a test suite: any other command could be attributed a concurrent
run's log lines."* siftr cannot know what a script execs, and guessing risks stealing
another process's log lines. Teaching it to recognise `bin/test` is not the fix.

## Why configuration doesn't rescue it

`Enabled` is **subtractive only**. `sources::enabled()` builds it from
`config.source_enabled(...)` defaulting to true, and `for_command` consults those fields
*inside* the branch it never reaches. So `.siftr.toml` can switch a source off and can
never switch one on. A user who knows exactly what their wrapper runs has no way to say so.

The escapes that exist are undiscoverable: invoke the underlying command directly
(abandoning the team's convention), or nest siftr inside the wrapper
(`bin/test` running `siftr run -- bundle exec rspec`), which requires already knowing the
problem exists. `siftr sources` reports it honestly, but only to someone suspicious enough
to ask.

## Cost

Silent and total on the projects it hits. A team with `bin/test` gets runs that record,
baseline and report — with the SQL and request telemetry simply absent. Nothing warns
them; `changes` just has less to say. This is the failure mode siftr's own first
principles are most opposed to: not a wrong answer, a confident quiet one.

## Recommendation

1. **Let `.siftr.toml` assert the suite**, e.g. `suite = "rspec"`. A user declaring what
   their own wrapper runs is an assertion, not an inference, so it does not weaken the
   concurrency reasoning behind the whitelist — it satisfies that reasoning from the one
   source that actually knows. **Needs evidence only for the spelling, not the idea.**
   Pre-registered check: with the assertion, `bin/test` must record
   `rails_log rspec rusage stderr stdout`, matching what `bundle exec rspec` records
   today; a command the config does not claim must still read only stdout, stderr, rusage.

2. **Say something when a Ruby project's run reads nothing but its own output.** One line
   pointing at `siftr sources` turns a silent absence into a visible one. Cheaper than
   (1) and worth doing either way.
