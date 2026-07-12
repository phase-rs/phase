#!/usr/bin/env bash
# Diff-based gate: new engine code must go through the single-authority
# helpers instead of poking at raw state the authority encapsulates.
#
# Existing offenders are frozen in amber — this check only flags *newly
# added* offending lines in the diff (same mechanism as
# check-parser-combinators.sh).
#
# Pattern families:
#   (A) Raw keyword queries: `.keywords.contains(` / `.keywords.iter(`.
#       Battlefield objects' post-layer `obj.keywords` happens to be correct,
#       but the same query against a hand/graveyard/exile object silently
#       misses off-zone grants. The authorities in game/keywords.rs make the
#       choice explicit:
#           keywords::has_keyword / has_keyword_kind            (object-scoped)
#           keywords::object_has_effective_keyword_kind          (state-scoped,
#               consults off_zone_characteristics — required for anything that
#               can run on a non-battlefield object)
#
#   (B) Raw zone mutation: the movers in game/zones.rs, direct writes to the
#       zone containers, and direct `GameObject::zone` assignment. Any of these
#       skips replacement consultation, `ZoneChanged`, triggers, and draw
#       bookkeeping. Gameplay zone changes go through zone_pipeline.
#       Unlike (A), this section is FULL-TREE, not diff-only: it is a ratchet
#       against a frozen baseline, so a pre-existing site cannot be quietly
#       duplicated into a new one. See scripts/zone_authority_census.py.
#
# Exempt: lines (or the line immediately above) with
#     // allow-raw-authority: <reason>      (A)
#     // allow-raw-zone: <reason>           (B)
# Allowed files (the authorities themselves and the layer/copy machinery that
# must read raw keyword state): see ALLOWED_KEYWORD_FILES below.
#
# Usage:
#   scripts/check-engine-authorities.sh [base-ref]
#
# Default base-ref is the merge-base with origin/main. In CI, pass the PR
# target branch's SHA explicitly.

set -euo pipefail

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"
SCOPE='crates/engine/src'

# Pre-commit hook mode: only check staged changes (mirrors
# check-parser-combinators.sh) so another agent's unstaged work isn't flagged.
DIFF_MODE=""
if [ -n "${GIT_INDEX_FILE:-}" ] || [ "$BASE" = "$(git rev-parse HEAD 2>/dev/null)" ]; then
    DIFF_MODE="--cached"
fi

# (A) Raw keyword queries.
FORBIDDEN_KEYWORD_QUERY='\.keywords\.(contains|iter)\('
# Files allowed to touch raw keyword state: the authority module, the layer
# engine that computes `obj.keywords`, and copy machinery that snapshots
# characteristics wholesale.
ALLOWED_KEYWORD_FILES='game/keywords\.rs|game/layers\.rs|game/game_object\.rs|effects/become_copy\.rs|effects/token_copy\.rs'

FAIL=0
report_keyword=""

filter_allow_annotation() {
    local file="$1"
    local candidates="$2"
    local added=""
    while IFS= read -r diff_line; do
        [ -z "$diff_line" ] && continue
        local text="${diff_line#*+}"
        local ln
        ln=$(grep -nFx "$text" "$file" 2>/dev/null | head -1 | cut -d: -f1)
        if [ -n "$ln" ] && [ "$ln" -gt 1 ]; then
            local prev
            prev=$(sed -n "$((ln-1))p" "$file")
            if echo "$prev" | grep -q 'allow-raw-authority'; then
                continue
            fi
        fi
        if echo "$text" | grep -q 'allow-raw-authority'; then
            continue
        fi
        added="${added}${text}
"
    done <<< "$candidates"
    printf '%s' "${added%$'\n'}"
}

# NB: no early exit on an empty diff — section (B) is full-tree and must run
# even when this change touched no engine source.
files=$(git diff $DIFF_MODE --name-only "$BASE" -- "$SCOPE" ':(exclude)**/*.md' 2>/dev/null || true)

while IFS= read -r file; do
    [ -f "$file" ] || continue
    if echo "$file" | grep -qE "$ALLOWED_KEYWORD_FILES"; then
        continue
    fi

    diff_added=$(git diff $DIFF_MODE --unified=0 "$BASE" -- "$file" | grep -E '^\+[^+]' || true)
    if [ -z "$diff_added" ]; then
        continue
    fi

    keyword_hits=$(echo "$diff_added" | grep -Ev 'allow-raw-authority' | grep -E "$FORBIDDEN_KEYWORD_QUERY" || true)
    keyword_clean=$(filter_allow_annotation "$file" "$keyword_hits")
    if [ -n "$keyword_clean" ]; then
        report_keyword="${report_keyword}
  ${file}:"
        while IFS= read -r line; do
            report_keyword="${report_keyword}
    ${line}"
        done <<< "$keyword_clean"
        FAIL=1
    fi
done <<< "$files"

if [ "$FAIL" -eq 1 ]; then
    cat >&2 <<EOF
ERROR: New engine code bypasses a single-authority helper.

(A) Raw keyword query — use the authorities in game/keywords.rs:
    obj.keywords.contains(&kw)        ->  keywords::has_keyword(obj, &kw)
    obj.keywords.iter().any(...)      ->  keywords::has_keyword_kind(obj, kind)
    (anything that can run on a non-battlefield object)
                                      ->  keywords::object_has_effective_keyword_kind(state, id, kind)
Raw queries silently miss off-zone keyword grants for hand/graveyard/exile
objects — the state-scoped authority is the only variant that consults
off_zone_characteristics.

Forbidden in added lines (diff vs ${BASE}):
${report_keyword}

If a use is genuinely structural (layer engine internals, characteristic
snapshots), annotate the line with:

    // allow-raw-authority: <one-line reason>

EOF
fi

# (B) Raw zone mutation — full-tree ratchet against the frozen baseline.
if ! python3 "$(dirname "$0")/zone_authority_census.py" --check; then
    FAIL=1
fi

exit "$FAIL"
