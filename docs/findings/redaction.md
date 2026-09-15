# Redaction: cost, coverage, and what to do with old stores

siftr 0.1.0 stored credentials from a command's output verbatim, in behavior templates, kept lines and raw captures.
Redaction now runs once per line, before anything is stored (`src/normalize/secrets.rs`). Measured 2026-09-15 on an
M-series Mac, release builds, each timing the median of 9–10 interleaved runs with a fresh data dir. Every input
was synthetic or built from committed fixtures, and was deleted after measuring.

## Inputs

- **synthetic**: 100k lines of Rails-style `test.log` (ANSI-coloured SQL, requests, params, backtraces) with 1% of
  lines planted with a random credential in one of 14 shapes: GitHub, AWS after an ANSI escape, Stripe, JWT, empty-user
  `redis://` password, `RAILS_MASTER_KEY=`, Slack, a SQL bind, a cookie, JSON inside an RSpec event, a PEM block,
  plus planted emails and public IPs. Clean lines include near misses: `password_digest`, `[FILTERED]`, `sort_key=`,
  `token_type: Bearer`, `Api::V1::AccountsController`.
- **fixtures**: the committed `fixtures/rails_demo/*/test.log` and `stdout.txt`, repeated to 100k lines.

## Coverage

| | caught | lines changed that held nothing |
|---|---|---|
| synthetic, planted credentials (`secrets`) | 868/868 | 0 of 98,986 |
| synthetic, planted emails and public IPs (`pii`) | 78/78, 68/68 | 0 |
| every committed fixture scenario, capture vs source | none planted | 0 of 2,983 |

The rule table (`tests/redact_secrets.rs`) carries launder's cases (e935da7…e403693) plus the shapes siftr's logs add.

## Cost

| | before | after | |
|---|---|---|---|
| `siftr ingest`, synthetic 100k lines | 77.9 ms | 87.9 ms | +13% |
| `siftr ingest`, fixtures 100k lines | 70.2 ms | 81.4 ms | +16% |
| scanner alone, synthetic / fixtures | | 104 / 100 ns per line | 0 allocations on a clean line |

The first working version cost +28%. `sample` on both binaries showed where: the scanner's per-byte loop (the rules
had been inlined into it) and captures written a line at a time through an 8 KB buffer, where 256 KB chunks used to
pass through whole. Captures now buffer 256 KB, the rules are out of line, and the loop stops only at `: = ,` and at
the marker byte every token prefix carries (`_` of `ghp_`, `J` of `eyJ`). After that, the scanner is what remains of the
difference: most bytes cost one table lookup.

## Old stores: delete captures, don't rewrite them

Migration 10 redacts the database in place and deletes earlier runs' raw captures. On a store of 10 high-cardinality
runs recorded by the previous build (20,001 behaviors, 206,870 kept lines, 33 MB database, 77 MB of captures):

| | |
|---|---|
| migration, redacting the database and deleting captures | 0.23 s |
| kept lines with a credential, before → after | 540 → 0 |
| rewriting captures instead (redact line by line, rename) | 293 MB/s |

Rewriting reads every captured byte while the migration holds the store, and another siftr waits only 2 s for it:
at the default 20 kept runs of a 1M-line suite (about 2.4 GB) that is about 8 s. Deleting costs one unlink per
file, and loses only `explain`'s whole-message read-back for old runs, whose kept lines stay. Database redaction scales
with kept lines, about 1 µs each: a store at the per-run caps (20 runs × 20,000 behaviors × 8 lines) would take
about 3.5 s, once.

## Limits

- Recognized shapes only. A credential under a key siftr doesn't know, with no known prefix, is stored.
- `SIFTR_REDACT=off` keeps raw captures but not raw kept lines: they come from the masked view templates use, so
  the database never holds a recognized credential whatever the setting.
- A line longer than 1 MiB is captured clipped, as analyzed: its unscanned tail could hold a credential.
- SQLite can keep freed pages with old text until `siftr gc` vacuums; the migration doesn't vacuum.
