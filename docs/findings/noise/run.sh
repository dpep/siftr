#!/usr/bin/env bash
# Records repeated rspec runs with the siftr listener for noise measurement.
#   run.sh <out_dir> <suite: demo|iriq> <scenario> <n> [VAR=value ...]
# Each run lands in <out_dir>/<suite>/<scenario>/<timestamp>/ with rspec.ndjson
# (log_offset rebased to the slice), test.log slice (demo), stdout, stderr,
# exit code, wall time and the 1-minute load average before the run.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
LISTENER=$ROOT/docs/findings/siftr_rspec_listener.rb
OUT=$1 SUITE=$2 SCENARIO=$3 N=$4; shift 4

case "$SUITE" in
  demo) APP=$ROOT/dogfood/rails_demo; LOG=$APP/log/test.log ;;
  demorand) APP=$ROOT/dogfood/rails_demo; LOG=$APP/log/test.log; EXTRA_SPEC_OPTS="--order rand" ;;
  iriq) APP=$HOME/code/lib/ruby/iriq; LOG= ;;
  *) echo "unknown suite $SUITE" >&2; exit 2 ;;
esac

for _ in $(seq "$N"); do
  mkdir -p "$OUT/$SUITE/$SCENARIO"
  dir=$OUT/$SUITE/$SCENARIO/$(printf '%03d' "$(ls "$OUT/$SUITE/$SCENARIO" | wc -l)")
  mkdir -p "$dir"
  start=0
  if [ -n "$LOG" ]; then touch "$LOG"; start=$(wc -c < "$LOG" | tr -d ' '); fi
  sysctl -n vm.loadavg | awk '{print $2}' > "$dir/load.txt"
  t0=$(ruby -e 'print Process.clock_gettime(Process::CLOCK_MONOTONIC)')
  code=0
  (cd "$APP" && env "$@" SIFTR_RSPEC_EVENTS="$dir/rspec.ndjson" ${LOG:+SIFTR_RSPEC_LOG="$LOG"} \
    SPEC_OPTS="${EXTRA_SPEC_OPTS:-} --require $LISTENER" bundle exec rspec > "$dir/stdout.txt" 2> "$dir/stderr.txt") || code=$?
  ruby -e "print Process.clock_gettime(Process::CLOCK_MONOTONIC) - $t0" > "$dir/wall.txt"
  echo "$code" > "$dir/exit_code.txt"
  if [ -n "$LOG" ]; then
    tail -c +$((start + 1)) "$LOG" > "$dir/test.log"
    ruby -rjson -i -ne 'h = JSON.parse($_); h["log_offset"] -= '"$start"' if h["log_offset"]; puts JSON.generate(h)' "$dir/rspec.ndjson"
  fi
  echo "$SUITE/$SCENARIO exit=$code load=$(cat "$dir/load.txt") wall=$(cat "$dir/wall.txt")"
done
