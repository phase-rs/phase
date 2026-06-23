#!/usr/bin/env bash
#
# Print the engine-source content hash for a given commit.
#
# This is the single authority for the parse-diff baseline key. The same hash
# is computed on BOTH sides of the coverage-parse-diff flow:
#
#   - publish (main-push CI): hash HEAD, upload coverage-data-<hash>.json to R2.
#   - fetch (PR CI): hash the PR's base.sha, fetch coverage-data-<hash>.json.
#
# Because the head coverage is built from the merge commit (base.sha + PR), a
# baseline keyed by base.sha's hash makes `head - baseline` cancel to exactly
# the PR's parse changes (see .github/workflows/ci.yml "Parse-detail diff").
#
# The fingerprint covers exactly the inputs that determine parse output and
# mirrors the cardgen-cache key (ci.yml): the engine source tree, the engine
# crate manifest, and the lockfile (dep pins like nom affect parsing).
#
# Implemented with `git ls-tree -r` rather than `hashFiles`/working-tree hashing
# so it is computable for ANY commit straight from history — no checkout — which
# is what lets the PR side hash base.sha without disturbing its checked-out tree.
#
# Usage: scripts/engine-source-hash.sh <commit-ish>
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: engine-source-hash.sh <commit-ish>" >&2
  exit 2
fi

sha="$1"

# `-r` recurses into the src tree so every file's blob hash participates; the
# blob hashes change iff content changes. Hash the listing to a stable digest.
# Truncated to 16 hex chars to match the card_data_hash convention (ci.yml).
git ls-tree -r "$sha" -- \
  crates/engine/src \
  crates/engine/Cargo.toml \
  Cargo.lock \
  | sha256sum \
  | cut -c1-16
