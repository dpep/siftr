#!/usr/bin/env bash
# Replay a public Ruby/RSpec repo's own history under `siftr run`, one run per
# commit, so siftr sees real successive runs of a real project instead of faults
# we injected ourselves.
#
#   replay.sh REPO COMMITS OUTDIR [SIFTR]
#
# REPO     a clone of the project, checked out anywhere; this script moves its HEAD
# COMMITS  one short sha per line, oldest first (blank lines and # comments ignored)
# OUTDIR   per-commit rspec output, siftr's report, and the replay's own log
# SIFTR    path to the siftr binary (default: siftr on PATH)
#
# Env:
#   BUNDLE_PATH   gem install dir, shared across commits so only drift re-installs
#   PIN_GEMS      lines appended to the Gemfile after each checkout, to hold a
#                 dependency at the era's version (see docs/findings/oss-corpus.md)
set -uo pipefail

repo=${1:?repo}
commits=${2:?commit list}
outdir=${3:?outdir}
siftr=${4:-siftr}

export SIFTR_HOME="$outdir/siftr-home"
mkdir -p "$SIFTR_HOME"
log="$outdir/replay.log"

while read -r sha _; do
  case "$sha" in ''|'#'*) continue ;; esac

  git -C "$repo" checkout -f --detach "$sha" >/dev/null 2>&1 || {
    echo "$sha checkout-failed" | tee -a "$log"; continue
  }
  [ -n "${PIN_GEMS:-}" ] && printf '\n%s\n' "$PIN_GEMS" >>"$repo/Gemfile"

  if ! (cd "$repo" && bundle install --quiet) >>"$outdir/$sha.bundle" 2>&1; then
    echo "$sha bundle-failed" | tee -a "$log"; continue
  fi

  # siftr run exits with the child's code; a red suite is data, not a stop.
  (cd "$repo" && "$siftr" run -- bundle exec rspec) >"$outdir/$sha.out" 2>&1
  code=$?
  (cd "$repo" && "$siftr" changes -j) >"$outdir/$sha.changes.json" 2>&1

  echo "$sha rspec=$code $(grep -cE '^' "$outdir/$sha.out")lines" | tee -a "$log"
done <"$commits"
