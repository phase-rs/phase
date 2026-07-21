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
#       Unlike (A), this section is FULL-TREE, not diff-only: every classified
#       site requires a nonempty `allow-raw-zone:` annotation. See
#       scripts/zone_authority_census.py.
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

# (B0) The census scanner's own seam suite. Gates (B) and (C) both stand on
# `strip_noncode` in zone_authority_census.py; a lexer regression there would
# not fail those gates — it would silently mis-scope them (a swallowed hit
# reads as migration progress). So the suite that pins the lexer runs here,
# ahead of the gates it protects.
if ! python3 "$(dirname "$0")/zone_authority_census_tests.py"; then
    FAIL=1
fi

# (B) Raw zone mutation — full-tree hard gate requiring explicit annotations.
if ! python3 "$(dirname "$0")/zone_authority_census.py" --check; then
    FAIL=1
fi

# (C) Draw replacement-definition producers — exact-match freeze.
# Plan 03 gives every `ReplacementEvent::Draw` definition an explicit CR 121.2
# scope (instruction-count vs individual-draw) assigned at construction. A
# producer the rewrite misses would silently take a default scope, so the set of
# producers is frozen here. The corpus half of this census needs the generated
# card-data and therefore runs in the card-data CI job, not this one.
#
# SCOPE: the whole cargo workspace — every `crates/*/src`, 13 crates today,
# globbed rather than enumerated so crate #14 is in scope the day it lands. This
# was 3 hand-named crates until the raw-string branch in `strip_noncode` (#5704)
# lifted the scanner ceiling a workspace-wide scan used to hit (`CensusError:
# crates/draft-wasm/src/suggest.rs:437: brace tracking desynced`).
#
# Widening surfaced ZERO new producers: `ReplacementEvent::Draw` occurs in exactly
# two crates (engine, mtgish-import). The other ten are not producer-free merely by
# assumption — phase-ai builds `ReplacementDefinition`s with the same constructor
# and struct-literal idioms this census matches, just never with `Draw`. So the
# frozen population is unchanged at 7 rows, and now covers the surface it always
# claimed to.
#
# Outside the workspace, and NOT scanned: `client/src-tauri` and
# `lobby-worker/broker-wasm` (Cargo `exclude`; 4 `.rs` files, zero mentions of
# `ReplacementEvent`). `client/src-tauri` also grows a gitignored `target/` on any
# local Tauri build, which an rglob would descend into — making the scanned
# population depend on whether a developer had run one.
if ! python3 "$(dirname "$0")/draw_replacement_census.py" --producers --check; then
    FAIL=1
fi

# (D) Post-replacement continuation producer/consumer surface — full-tree
# ratchet against the frozen baseline. Plan 03's CR 121.2 draw state machine
# reaches this surface through delayed replacement work, and CR 616.1g requires
# that work to run only after the replacement's modified event is handled. A
# `Template` or `Resolved` continuation can hold arbitrary ability work, so this
# freezes every syntactic construction, installation, stash, and dispatch site
# rather than pretending source inspection can prove a site cannot reach Draw.
if ! python3 "$(dirname "$0")/post_replacement_continuation_census.py" --check; then
    FAIL=1
fi

exit "$FAIL"
