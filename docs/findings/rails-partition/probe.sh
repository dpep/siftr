#!/usr/bin/env bash
# What siftr makes of a Rails log in each of the four formats a Rails app can emit.
# Reads the committed rails_demo fixture, writes to a scratch dir, touches no real data dir.
#
#   probe.sh [SCRATCH]
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd)
scratch=${1:-${TMPDIR:-/tmp}/siftr-rails-partition}
siftr=$root/target/release/siftr
fixture=$root/fixtures/rails_demo/baseline/test.log

[ -x "$siftr" ] || cargo build --release --manifest-path "$root/Cargo.toml"
rm -rf "$scratch"
mkdir -p "$scratch"
export SIFTR_HOME=$scratch/home

kinds() { # run id -> "behaviors N {kind: behaviors} | events {kind: events}"
  "$siftr" summary "$1" -j -n 5000 | python3 -c '
import collections, json, sys
d = json.load(sys.stdin)
b, e = collections.Counter(), collections.Counter()
for row in d["behaviors"]:
    b[row["behavior"]["kind"]] += 1
    e[row["behavior"]["kind"]] += row["stats"]["count"]
print(d["behaviors_total"], "behaviors", dict(b), "| events", dict(e))'
}

cp "$fixture" "$scratch/a_plain.log"
for v in tag:b_tagged info:c_info taginfo:d_tagged_info; do
  python3 "$root/docs/findings/rails-partition/variants.py" "${v%%:*}" "$fixture" "$scratch/${v##*:}.log"
done

n=0
for v in a_plain b_tagged c_info d_tagged_info; do
  n=$((n + 1))
  mkdir -p "$scratch/dir_$v"
  cp "$scratch/$v.log" "$scratch/dir_$v/test.log"
  # The directory names the file test.log, the only stream that reaches the Rails interpreter.
  "$siftr" "$scratch/dir_$v" --context "$v" --no-report
  printf '%-16s %5s lines  ' "$v" "$(wc -l <"$scratch/$v.log")"
  kinds "r$n"
done

# The same untagged bytes read as a file, which reaches the interpreters as stdout.
"$siftr" "$scratch/a_plain.log" --context stdout_plain --no-report
printf '%-16s %5s lines  ' "a_plain (stdin)" "$(wc -l <"$scratch/a_plain.log")"
kinds "r$((n + 1))"
