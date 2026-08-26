#!/usr/bin/env bash
# Select the main-side parent of a synthetic PR merge commit.
# Usage: parse-diff-base.sh <pr-head-sha> [merge-commit]

set -euo pipefail

PR_HEAD_SHA="${1:?usage: parse-diff-base.sh <pr-head-sha> [merge-commit]}"
MERGE_COMMIT="${2:-HEAD}"

if ! P1="$(git rev-parse "$MERGE_COMMIT^1" 2>/dev/null)" \
    || ! P2="$(git rev-parse "$MERGE_COMMIT^2" 2>/dev/null)"; then
    echo "parse-diff baseline pending: synthetic merge parents unavailable" >&2
    exit 1
fi

if [ "$P1" = "$PR_HEAD_SHA" ] && [ "$P2" != "$PR_HEAD_SHA" ]; then
    printf '%s\n' "$P2"
elif [ "$P2" = "$PR_HEAD_SHA" ] && [ "$P1" != "$PR_HEAD_SHA" ]; then
    printf '%s\n' "$P1"
else
    echo "parse-diff baseline pending: PR head is not exactly one merge parent" >&2
    exit 1
fi
