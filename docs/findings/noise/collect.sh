#!/usr/bin/env bash
# Interleaves scenarios so machine-load drift over time hits baseline and
# regression runs alike (a block of baselines then a block of toggles would
# confound load with the regression).
#   collect.sh <out_dir> [rounds]
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
OUT=$1 ROUNDS=${2:-25}

for r in $(seq "$ROUNDS"); do
  "$HERE/run.sh" "$OUT" demo baseline 1
  if [ $((r % 5)) -eq 0 ]; then
    "$HERE/run.sh" "$OUT" demo n_plus_one 1 SIFTR_DEMO_N_PLUS_ONE=1
    "$HERE/run.sh" "$OUT" demo slow 1 SIFTR_DEMO_SLOW=1
    "$HERE/run.sh" "$OUT" demo warn 1 SIFTR_DEMO_WARN=1
    "$HERE/run.sh" "$OUT" demo fail 1 SIFTR_DEMO_FAIL=1
  fi
  if [ $((r % 2)) -eq 1 ]; then "$HERE/run.sh" "$OUT" iriq baseline 1; fi
done
