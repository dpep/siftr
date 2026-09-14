# siftr

Wrap a command, turn its output into **behaviors**, compare them with recent runs of the same command, and surface
**behavioral changes** with the raw lines that back them.

```
siftr run -- bundle exec rspec     # passes output through, exits with rspec's code, prints changes to stderr
siftr changes                      # what changed in the latest run
siftr explain s3                   # a signal's baseline vs current numbers, and its evidence
siftr evidence 3f9a0c12de          # raw lines kept for a behavior
```

## Commands

| Command | Does |
|---|---|
| `run -- CMD…` | Runs `CMD`, tees its stdout/stderr, records and analyzes the run. `-q` hides the passthrough. |
| `ingest [FILE]` | Records a file or stdin as a run's stdout. `--context NAME` groups comparable inputs. |
| `changes [RUN]` | Signals for a run (default: latest in this project). |
| `summary [RUN]` | Top behaviors by `--by count` or `--by time`. |
| `evidence <BEHAVIOR>` | Exemplar lines for a behavior (`--run`, `-n`). |
| `explain <SIGNAL>` | Signal, behavior, current and baseline numbers, exemplars. |
| `history` | Runs in this project. |

Every command takes `-j` for JSON on stdout. Human output ends with the next command to run.

Exit codes: `run` exits with the command's code (125 if siftr fails before starting it, 126/127 if it cannot be
executed or found). Queries exit 0 with results, 1 when nothing is found, 2 on error.

## Data

`--home DIR` > `SIFTR_HOME` > `$XDG_DATA_HOME/siftr` > `~/.local/share/siftr`. Holds `siftr.db` (SQLite) and each
run's raw capture under `runs/<run>/`.

## Development

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Design principles, the domain model and crate layout are in [CLAUDE.md](CLAUDE.md).
