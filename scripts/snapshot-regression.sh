#!/usr/bin/env bash
#
# snapshot-regression.sh — Oracle parser regression check.
#
# This is the ONE canonical parse-regression path. It is a thin wrapper around
# the `set-check` binary (crates/engine/src/bin/set_check.rs), which computes a
# stable per-card `ast_hash` over each card face's parse-relevant fields and
# diffs current hashes against a baseline. Cards that were `supported` in the
# baseline whose AST moved are surfaced as *regression suspects* — far more
# precise than the old raw 100-line `jq` diff of card-data.json this script
# used to run (a single field reorder produced hundreds of meaningless lines;
# the ast_hash is reorder-stable and parse-only).
#
# Two workflows:
#
#   1. Committed-baseline (default, distributed/CI):
#        # 1. (optional) regenerate card data after your parser change
#        ./scripts/gen-card-data.sh
#        # 2. diff the current parse against the shared, git-committed baseline
#        ./scripts/snapshot-regression.sh
#      Diffs client/public/card-data.json against data/parse-baseline.json
#      (the committed baseline). Exit 0 = no AST moved; exit 1 = something
#      moved (intended or regression — read the "Regression suspects" block).
#      Intended changes are landed by refreshing the baseline in the same PR
#      (a committed, generated file contributors refresh and reviewers inspect
#      in the PR diff):
#        ./scripts/refresh-parse-baseline.sh   (or: cargo parse-baseline)
#
#   2. Ad-hoc before/after (local, transient):
#        # before your change
#        ./scripts/snapshot-regression.sh --snapshot /tmp/before.json
#        # ... edit parser, regenerate card data ...
#        ./scripts/snapshot-regression.sh /tmp/before.json
#      Positional arg = baseline to diff against (back-compatible with the old
#      `./scripts/snapshot-regression.sh [before.json]` interface).
#
# Usage:
#   ./scripts/snapshot-regression.sh [BASELINE]        # diff (default baseline: data/parse-baseline.json)
#   ./scripts/snapshot-regression.sh --snapshot FILE   # write a baseline snapshot
#   ./scripts/snapshot-regression.sh --set CODE        # scope the diff to one set
#   ./scripts/snapshot-regression.sh --deck PATH       # scope the diff to a deck list
#
# Any extra flags after the baseline are forwarded to set-check unchanged
# (e.g. --set, --deck, --quiet).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_BASELINE="$REPO_ROOT/data/parse-baseline.json"
CARD_DATA="$REPO_ROOT/client/public/card-data.json"

# Resolve set-check. The binary must be built from the SAME engine that produced
# the card-data.json being diffed — it deserializes the parsed AST, so an older
# binary fails to load card-data that contains a newer Effect/AST variant. We
# therefore prefer the in-repo build (cargo, which rebuilds against the current
# engine) over a possibly-stale `set-check` on PATH. If cargo is unavailable we
# fall back to the PATH binary (fine when it tracks the same revision as main).
run_set_check() {
  if command -v cargo >/dev/null 2>&1; then
    ( cd "$REPO_ROOT" && cargo run --quiet --profile tool --bin set-check -- "$@" )
  elif command -v set-check >/dev/null 2>&1; then
    set-check "$@"
  else
    echo "set-check unavailable: install it on PATH or run from a repo with cargo." >&2
    return 127
  fi
}

# --snapshot mode: write a baseline of the current parse and exit. Pass the
# data root explicitly so set-check reads client/public/card-data.json.
if [[ "${1:-}" == "--snapshot" ]]; then
  shift
  SNAP_OUT="${1:?--snapshot requires an output FILE}"
  shift || true
  run_set_check "$REPO_ROOT/client/public" --snapshot "$SNAP_OUT" "$@"
  exit $?
fi

# --diff mode (default). First positional arg, if present and not a flag, is the
# baseline; otherwise use the committed baseline.
BASELINE="$DEFAULT_BASELINE"
if [[ $# -gt 0 && "$1" != --* ]]; then
  BASELINE="$1"
  shift
fi

if [[ ! -f "$BASELINE" ]]; then
  echo "No baseline found at $BASELINE" >&2
  if [[ "$BASELINE" == "$DEFAULT_BASELINE" ]]; then
    echo "Generate the committed baseline first: ./scripts/refresh-parse-baseline.sh" >&2
  else
    echo "Create one first: ./scripts/snapshot-regression.sh --snapshot $BASELINE" >&2
  fi
  exit 2
fi

if [[ ! -f "$CARD_DATA" ]]; then
  echo "No current card-data.json found at $CARD_DATA" >&2
  echo "Generate it first: ./scripts/gen-card-data.sh" >&2
  exit 2
fi

# Delegate to set-check's per-card AST diff. It exits non-zero iff any card's
# ast_hash moved (added/removed/changed), printing the changed cards and — for
# the subset that were supported in the baseline — the "Regression suspects"
# block. Forward the data root so it reads client/public/card-data.json.
run_set_check "$REPO_ROOT/client/public" --diff "$BASELINE" "$@"
