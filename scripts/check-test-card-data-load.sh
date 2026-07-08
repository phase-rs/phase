#!/usr/bin/env bash
# Diff-based gate: test code must not load the full ~90 MB card-data export
# (client/public/card-data.json) directly.
#
# Under `cargo nextest` every test runs in its own process, so a per-test
# `CardDatabase::from_export(client/public/card-data.json)` reparses the whole
# ~90 MB / 35k-card export — tens of seconds PER TEST in a debug build. Worse,
# the export is gitignored and absent in the CI `Rust tests` job, so those tests
# silently self-skip there: they are invisible in CI yet bloat every local and
# Tilt `test-engine` run.
#
# Use the fixture-backed loaders in tests/integration/support.rs instead:
#   shared_card_db()          -> parsed CardDatabase from the committed fixture
#                                (tests/fixtures/integration_cards.json),
#                                parses in milliseconds and runs in CI.
#   shared_card_export_json() -> raw JSON of the full export, ONLY for the few
#                                drift-guard tests that must scan every card.
# Need a card the fixture lacks? Add it with `python3 scripts/gen-test-fixture.py`.
#
# Existing offenders are frozen in amber — this check flags only *newly added*
# offending lines in the diff (same mechanism as check-parser-combinators.sh and
# check-engine-authorities.sh), so it can land before the back-catalogue is
# migrated.
#
# Exempt: a flagged line (or the line immediately above it) carrying
#     // allow-full-card-db: <reason>
# Allowed files (legitimately reference the full-export path): see ALLOWED_FILES.
#
# Usage:
#   scripts/check-test-card-data-load.sh [base-ref]
#
# Default base-ref is the merge-base with origin/main. In CI, pass the PR target
# branch's SHA explicitly.

set -euo pipefail

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"

# Test code lives both inline (`#[cfg(test)]` in crates/engine/src) and in
# crates/engine/tests. src/bin/* are CLI tools that load the real export by
# design and are allow-listed below rather than excluded by scope.
SCOPE='crates/engine'

# Pre-commit hook mode: only check staged changes (mirrors the sibling gates)
# so another agent's unstaged work isn't flagged.
DIFF_MODE=""
if [ -n "${GIT_INDEX_FILE:-}" ] || [ "$BASE" = "$(git rev-parse HEAD 2>/dev/null)" ]; then
    DIFF_MODE="--cached"
fi

# The full-export path, banned as a LOAD target in test code. Provenance
# references in doc comments ("verified against client/public/card-data.json")
# are fine and are filtered out by the comment check below.
FORBIDDEN='client/public/card-data\.json'
# The canonical loader (support.rs) names the path on purpose; the CLI bins load
# the real export by design.
ALLOWED_FILES='crates/engine/tests/integration/support\.rs|crates/engine/src/bin/'

FAIL=0
report=""

# Drop comment-only lines: a load reference is code, a provenance note is a
# comment. Matches leading `//`, `///`, `*` (block-comment body) and `/*`.
strip_comment_lines() {
    grep -Ev '^[[:space:]]*(//|\*|/\*)' || true
}

# Emit "<new-file-lineno>\t<content>" for each ADDED line in the diff for $file.
# The line number is parsed from the hunk header (@@ -a,b +c,d @@): with
# --unified=0 only '+' lines advance the new-file counter, so the running line
# number is exact. This replaces the old text-match-back-into-file approach,
# which mis-attributed duplicate lines (always matched the first occurrence).
added_lines_with_numbers() {
    local file="$1"
    git diff $DIFF_MODE --unified=0 "$BASE" -- "$file" | awk '
        /^@@/        { match($0, /\+[0-9]+/); ln = substr($0, RSTART + 1, RLENGTH - 1) + 0; next }
        /^\+\+\+/    { next }                       # skip the +++ b/file header
        /^\+/        { print ln "\t" substr($0, 2); ln++; next }
    '
}

files=$(git diff $DIFF_MODE --name-only "$BASE" -- "$SCOPE" ':(exclude)**/*.md' 2>/dev/null || true)
if [ -z "$files" ]; then
    exit 0
fi

while IFS= read -r file; do
    [ -f "$file" ] || continue
    if echo "$file" | grep -qE "$ALLOWED_FILES"; then
        continue
    fi

    while IFS=$'\t' read -r ln content; do
        [ -n "$ln" ] || continue
        # Provenance notes in comments are fine — only code lines load the path.
        printf '%s\n' "$content" | strip_comment_lines | grep -qE "$FORBIDDEN" || continue
        # Exempt: annotation on the same line, or on the line immediately above.
        # `case` globs (not grep args) so a content line starting with '-' is safe.
        case "$content" in *allow-full-card-db*) continue ;; esac
        if [ "$ln" -gt 1 ]; then
            prev=$(sed -n "$((ln - 1))p" "$file")
            case "$prev" in *allow-full-card-db*) continue ;; esac
        fi
        report="${report}
  ${file}:${ln}:${content}"
        FAIL=1
    done <<< "$(added_lines_with_numbers "$file")"
done <<< "$files"

if [ "$FAIL" -eq 1 ]; then
    cat >&2 <<EOF
ERROR: New test code loads the full card-data export (client/public/card-data.json).

Under nextest (process-per-test) this reparses ~90 MB per test — tens of seconds
each in debug — and the export is gitignored, so the test silently self-skips in
CI while bloating every local and Tilt test run.

Use the fixture-backed loaders in crates/engine/tests/integration/support.rs:
    CardDatabase::from_export(".../client/public/card-data.json")
                                  ->  support::shared_card_db()          (fixture)
    (a test that must scan EVERY card)
                                  ->  support::shared_card_export_json()  (raw JSON)
Add any card the fixture lacks with: python3 scripts/gen-test-fixture.py

Forbidden in added lines (diff vs ${BASE}):
${report}

If a test genuinely must load the full export (e.g. an all-cards drift guard
that cannot use shared_card_export_json), annotate the line with:

    // allow-full-card-db: <one-line reason>

EOF
    exit 1
fi

exit 0
