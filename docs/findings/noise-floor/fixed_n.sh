#!/usr/bin/env bash
# The floor at a fixed baseline size. Replaying a corpus straight through spends
# most of its comparisons at n=10, because siftr keeps the last ten runs — but
# the risky regime is a young context, where the baseline range is narrow and
# E(n) is lowest. This rebuilds a fresh store per target run holding exactly its
# n immediate predecessors.
#
#   fixed_n.sh <siftr> <corpus_dir> <out_dir> <n> [context]
#
# <out_dir>/<nnn>.json is the comparison for run nnn against exactly n baseline
# runs. Feed <out_dir> to tally.rb.
set -uo pipefail

SIFTR=$1 CORPUS=$2 OUT=$3 N=$4 CTX=${5:-fixedn}
rm -rf "$OUT"; mkdir -p "$OUT"

mapfile -t RUNS < <(find "$CORPUS" -mindepth 1 -maxdepth 1 -type d | sort)
for ((i = N; i < ${#RUNS[@]}; i++)); do
  home=$OUT/home
  rm -rf "$home"; mkdir -p "$home"
  export SIFTR_HOME=$home
  for ((j = i - N; j < i; j++)); do
    "$SIFTR" ingest --context "$CTX" --dir "${RUNS[$j]}" --no-report
  done
  "$SIFTR" ingest --context "$CTX" --dir "${RUNS[$i]}" --no-report
  "$SIFTR" changes --context "$CTX" -j > "$OUT/$(printf '%03d' "$((i + 1))").json"
done
rm -rf "$OUT/home"
echo "compared $((${#RUNS[@]} - N)) runs at n=$N"
