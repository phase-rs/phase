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
#                                (tests/fixtures/integration_cards.json.gz),
#                                parses in milliseconds and runs in CI.
#   shared_card_export_json() -> raw JSON of the full export, ONLY for the few
#                                drift-guard tests that must scan every card.
# Need a card the fixture lacks? Add it with `python3 scripts/gen-test-fixture.py`.
#
# Existing offenders are frozen in amber — this check flags only *newly added*
# offending lines in the diff (same mechanism as check-parser-combinators.sh and
# check-engine-authorities.sh), so it can land before the back-catalogue is
# migrated. A line moved within the gate's view (its scope minus SCOPE_EXCLUDES)
# is not added; the added-line set comes from scripts/lib/added-lines.py.
#
# Exempt: a flagged line (or the line immediately above it) carrying
#     // allow-full-card-db: <reason>
# Allowed files (legitimately reference the full-export path): see SCOPE_EXCLUDES.
#
# Usage:
#   scripts/check-test-card-data-load.sh [base-ref]
#
# Default base-ref is the merge-base with origin/main. In CI, pass the PR target
# branch's SHA explicitly.
#
# Exit status:
#   0  no newly added load found.
#   1  a newly added load found.
#   1  the added-line helper's own test suite (scripts/lib/added_lines_tests.py)
#      failed; no verdict.
#   2  the added-line helper (scripts/lib/added-lines.py) could not read the
#      diff; no verdict.

set -euo pipefail
# shellcheck source=scripts/lib/added-lines.sh
. "$(dirname "$0")/lib/added-lines.sh"

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"

# Test code lives both inline (`#[cfg(test)]` in crates/engine/src) and in
# crates/engine/tests.
SCOPE=(crates/engine)
# The canonical loader (support.rs) names the path on purpose; the CLI bins load
# the real export by design. These files are outside the gate's view, so a line
# moved out of one into a checked file is checked.
SCOPE_EXCLUDES=(
    ':(exclude)**/*.md'
    ':(exclude)crates/engine/tests/integration/support.rs'
    ':(exclude)crates/engine/src/bin'
)

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

FAIL=0
report=""

# Drop comment-only lines: a load reference is code, a provenance note is a
# comment. Matches leading `//`, `///`, `*` (block-comment body) and `/*`.
strip_comment_lines() {
    grep -Ev '^[[:space:]]*(//|\*|/\*)' || true
}

files=$(git diff $DIFF_MODE --name-only "$BASE" -- "${SCOPE[@]}" "${SCOPE_EXCLUDES[@]}" 2>/dev/null || true)
if [ -z "$files" ]; then
    exit 0
fi
added_lines_load $DIFF_MODE "$BASE" -- "${SCOPE[@]}" "${SCOPE_EXCLUDES[@]}"

while IFS= read -r file; do
    [ -f "$file" ] || continue

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
    done <<< "$(added_lines_for "$file")"
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
