#!/usr/bin/env bash
# Replays a corpus of captured runs in order into one fresh context and dumps
# `changes -j` after each, which is exactly what a developer would have seen
# after that run.
#
#   replay.sh <siftr> <corpus_dir> <out_dir> [context]
#
# <corpus_dir> holds one subdirectory per run (traffic.sh's output, or any
# `siftr DIR` scenario). <out_dir>/<nnn>.json is the comparison for that run;
# feed the directory to tally.rb.
set -uo pipefail

SIFTR=$1 CORPUS=$2 OUT=$3 CTX=${4:-floor}
HOME_DIR=$OUT/home
rm -rf "$OUT"; mkdir -p "$OUT" "$HOME_DIR"
export SIFTR_HOME=$HOME_DIR

i=0
for d in "$CORPUS"/*/; do
  [ -d "$d" ] || continue
  i=$((i + 1))
  n=$(printf '%03d' "$i")
  "$SIFTR" "$d" --context "$CTX" --no-report || echo "replay failed: $d" >&2
  "$SIFTR" changes --context "$CTX" -j > "$OUT/$n.json"
done
"$SIFTR" summary -j -n 100000 > "$OUT/summary.json" 2>/dev/null
echo "replayed $i runs into $CTX"
