#!/usr/bin/env bash
# Collects N batches of identical requests against the dogfood app's development
# stack, one directory per batch, for the false-positive floor measurement.
#
#   traffic.sh <out_dir> <batches> [requests_per_path_per_batch] [gap_seconds]
#
# Each batch lands in <out_dir>/<nnn>/test.log — the bytes appended to
# log/development.log while that batch was in flight. Replay one with
# `siftr ingest --dir`. See traffic.rb for what is and is not real here.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
APP=$ROOT/dogfood/rails_demo
OUT=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")

mkdir -p "$OUT"
export RAILS_ENV=development
cd "$APP" || exit 2
bin/rails db:prepare >/dev/null 2>&1 || { echo "db:prepare failed" >&2; exit 2; }
: > log/development.log
exec bin/rails runner "$ROOT/docs/findings/noise-floor/traffic.rb" "$OUT" "$2" "${3:-8}" "${4:-0}"
