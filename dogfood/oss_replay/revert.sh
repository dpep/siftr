#!/usr/bin/env bash
# Ask the question the chronological replay cannot: a real regression, not one we wrote.
#
#   revert.sh REPO FIX PATHS… -- OUTDIR SIFTR
#
# Baselines N green runs at FIX, then restores PATHS from FIX's parent — the
# maintainer's fix undone, their regression test left in place — and runs once
# more. The bug and the test are both the project's; only the reverting is ours.
set -uo pipefail

repo=${1:?repo}; fix=${2:?fix commit}; shift 2
paths=()
while [ "${1:-}" != "--" ]; do paths+=("$1"); shift; done
shift
outdir=${1:?outdir}; siftr=${2:-siftr}
runs=${BASELINE_RUNS:-4}

export SIFTR_HOME="$outdir/siftr-home"
mkdir -p "$SIFTR_HOME"

checkout() {
  git -C "$repo" checkout -f --detach "$fix" >/dev/null 2>&1 || exit 1
  [ -n "${PIN_GEMS:-}" ] && printf '\n%s\n' "$PIN_GEMS" >>"$repo/Gemfile"
  (cd "$repo" && bundle install --quiet) >/dev/null 2>&1
}

checkout
for i in $(seq "$runs"); do
  (cd "$repo" && "$siftr" run -- bundle exec rspec) >"$outdir/baseline$i.out" 2>&1
done

checkout
git -C "$repo" checkout "$fix~1" -- "${paths[@]}" || exit 1
(cd "$repo" && "$siftr" run -- bundle exec rspec) >"$outdir/reverted.out" 2>&1
(cd "$repo" && "$siftr" changes -j) >"$outdir/reverted.changes.json" 2>&1
tail -30 "$outdir/reverted.out"
