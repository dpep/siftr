#!/usr/bin/env bash
# Runs a real test suite under `siftr run` N times into one fresh context and
# dumps `changes -j` after each — the false-positive floor as a developer
# actually meets it, through the capture path rather than a replay.
#
#   suite.sh <siftr> <suite_dir> <out_dir> <runs> [command...]
#
# Nothing about the suite changes between runs, so every signal counted is a
# false positive. Feed <out_dir> to tally.rb.
set -uo pipefail

SIFTR=$1 SUITE=$2 OUT=$3 N=$4; shift 4
CMD=("$@"); [ ${#CMD[@]} -eq 0 ] && CMD=(bundle exec rspec)

rm -rf "$OUT"; mkdir -p "$OUT/home"
export SIFTR_HOME=$OUT/home

for i in $(seq "$N"); do
  n=$(printf '%03d' "$i")
  sysctl -n vm.loadavg | awk '{print $2}' > "$OUT/$n.load"
  (cd "$SUITE" && "$SIFTR" run -q --no-report -- "${CMD[@]}") > "$OUT/$n.stdout" 2> "$OUT/$n.stderr"
  echo "$?" > "$OUT/$n.exit"
  (cd "$SUITE" && "$SIFTR" changes -j) > "$OUT/$n.json" 2>/dev/null
  echo "run $i exit=$(cat "$OUT/$n.exit") load=$(cat "$OUT/$n.load")"
done
(cd "$SUITE" && "$SIFTR" summary -j -n 100000) > "$OUT/summary.json" 2>/dev/null
