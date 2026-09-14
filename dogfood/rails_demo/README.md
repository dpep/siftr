# rails_demo

The smallest real Rails 8.1 app (sqlite, rspec-rails) for dogfooding siftr.
Each `SIFTR_DEMO_*` env var switches on one genuine regression in app code.

## Setup

Ruby 3.4.9 (`.ruby-version`).

```
cd dogfood/rails_demo
bundle install          # or `bundle install --local` offline if the gems are installed
bundle exec rspec       # 10 examples, 0 failures, 1 pending
```

No db step needed: rspec-rails loads `db/schema.rb` into `db/test.sqlite3` on
the first run. SQL is logged to `log/test.log` (Rails' test default: debug
level, ANSI-colored).

## Regressions

| Env var | Code change | What changes in the telemetry |
|---|---|---|
| `SIFTR_DEMO_N_PLUS_ONE=1` | `UsersController#show` drops `includes(:comments)` | show request: `Completed … (3 queries …)` → `(10 queries …)`, 8 `Comment Load` lines in `log/test.log` |
| `SIFTR_DEMO_SLOW=1` | `Post#summary` sleeps 0.3s | example `Post summarizes the body` ~1–10ms → ~310ms |
| `SIFTR_DEMO_WARN=1` | `User#display_name` emits a deprecation | 2 `DEPRECATION WARNING: User#display_name is deprecated…` lines on **stderr** (not the log) |
| `SIFTR_DEMO_FAIL=1` | `User` drops its email validation | `User requires an email` fails, exit code 1 |

## Fixtures

`bin/capture_fixtures` re-runs every scenario and rewrites
`fixtures/rails_demo/<scenario>/`. See `docs/findings/capture.md`.
