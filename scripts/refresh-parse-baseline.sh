#!/usr/bin/env bash
#
# refresh-parse-baseline.sh — regenerate the committed parse-regression baseline.
#
# Writes data/parse-baseline.json: a per-card { ast_hash, supported, gap_count }
# snapshot of the full card DB, produced by `set-check --snapshot` over
# client/public/card-data.json. This file is git-COMMITTED (see the
# !data/parse-baseline.json allowlist entry in .gitignore) so that parse drift
# is a shared, reviewable, over-time signal rather than an ad-hoc /tmp file:
# every PR diffs against it via ./scripts/snapshot-regression.sh, and intended
# parse changes refresh it in the same PR (exactly like a committed snapshot).
#
# Update protocol: this is a committed, generated baseline refreshed by
# contributors, not by CI auto-commit.
#   * Run this whenever you intentionally change the parsed output of any card
#     (a parser/engine change that moves an ast_hash). Commit the updated
#     data/parse-baseline.json in the SAME PR as the change that caused it, so
#     the diff is reviewable: reviewers see exactly which cards' parses moved.
#   * A PR that moves an ast_hash without refreshing this baseline will show the
#     drift via ./scripts/snapshot-regression.sh — that is the gate that keeps
#     the shared baseline honest. (A CI freshness check that re-runs this script
#     and fails on an uncommitted diff is a reasonable maintainer add-on; see
#     docs/AI-CONTRIBUTOR.md §6.)
#   * Run ./scripts/gen-card-data.sh first if client/public/card-data.json is
#     stale or absent — the baseline reflects whatever card data is on disk.
#
# Usage:
#   ./scripts/refresh-parse-baseline.sh           # full DB -> data/parse-baseline.json
#   ./scripts/refresh-parse-baseline.sh OUT.json  # write to a different path
#
# Equivalent cargo alias: `cargo parse-baseline`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$REPO_ROOT/data/parse-baseline.json}"
CARD_DATA="$REPO_ROOT/client/public/card-data.json"

if [[ ! -f "$CARD_DATA" ]]; then
  echo "No card-data.json found at $CARD_DATA" >&2
  echo "Generate it first: ./scripts/gen-card-data.sh" >&2
  exit 2
fi

# Prefer the in-repo build (matches the current engine that produced the
# card-data being snapshotted) over a possibly-stale `set-check` on PATH; the
# binary deserializes the parsed AST, so it must match the engine version.
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

# Full-DB snapshot (no --set/--deck filter): every card face's parse-only
# ast_hash plus its support/gap state.
run_set_check "$REPO_ROOT/client/public" --snapshot "$OUT"

echo "Refreshed parse baseline -> $OUT" >&2
echo "Commit it in the same PR as the parser change that moved these hashes." >&2
