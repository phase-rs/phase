#!/usr/bin/env bash
# Diff-based gate: new parser code must not introduce string-matching dispatch
# patterns. Forces nom combinators on first write per the CLAUDE.md mandate,
# rather than leaving refactor-to-combinators for review.
#
# Existing non-combinator code in the parser is frozen in amber — this check
# only flags *newly added* offending lines in the diff.
#
# Three forbidden pattern families:
#   (A) String-method dispatch: .strip_prefix / .contains("...") / .split_once
#       / .find("...") / etc. Use nom combinators (tag, alt, take_until) instead.
#   (B) Match-arm dispatch on string literals: `match expr { "foo" => ..., }`.
#       Discriminant is parser text; arms are literals. Use alt((tag(...))).
#   (C) Chained `if let Ok((rest, _)) = tag("…")(input)` blocks (≥2 in one
#       file). Sequential tag tries should compose into a single alt(()).
#
# Exempt: lines (or the line immediately above) with
#     // allow-noncombinator: <reason>
# Legitimate uses are rare (TextPair dual-string helpers, punctuation stripping
# on already-tokenized input, dynamic-string prefixes with runtime tag bodies,
# string assertions in tests).
#
# Usage:
#   scripts/check-parser-combinators.sh [base-ref]
#
# Default base-ref is the merge-base with origin/main. In CI, pass the PR
# target branch's SHA explicitly.

set -euo pipefail

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"
SCOPE='crates/engine/src/parser'

# (A) String-method dispatch. The "..." suffix on `.contains` / `.starts_with`
# / `.ends_with` / `.find` / `.trim_*_matches` matches only string-literal
# arguments — `.contains(&item)` (Vec/slice op) and `.trim_end_matches('.')`
# (char arg, structural cleanup) are legitimate. strip_prefix / strip_suffix /
# split_once / rsplit_once almost always operate on string literals; flag
# unconditionally.
FORBIDDEN_METHODS='\.strip_prefix\(|\.strip_suffix\(|\.split_once\(|\.rsplit_once\(|\.contains\("|\.starts_with\("|\.ends_with\("|\.find\("|\.trim_end_matches\("|\.trim_start_matches\("'

# (B) Match-arm string-literal pattern. Lines that look like `"literal" => ...`
# at the start of an indented block. In Rust, string-literal patterns are
# valid only when matching a `&str`, which in parser code means matching on
# parser text — exactly the dispatch the mandate prohibits. Inline `#[cfg(test)]`
# fixtures inside parser modules are within scope; if a test legitimately
# match-maps strings (rare), use `// allow-noncombinator: test fixture`.
FORBIDDEN_MATCH_ARM='^\+[[:space:]]*"[^"]+"[[:space:]]*=>'

# (C) `if let Ok((…)) = tag("literal")(…)`. One use is fine (extracting a
# single optional prefix). Two or more in one file is the chained anti-pattern
# — should collapse into `alt((tag(...), tag(...)))`. Counted per file.
IFLET_TAG_PATTERN='^\+[[:space:]]*if[[:space:]]+let[[:space:]]+Ok.*=[[:space:]]*tag(_no_case)?(::<[^>]*>)?\("[^"]+"\)'

FAIL=0
report_methods=""
report_match_arm=""
report_iflet_tag=""

# Filter a per-file candidate list against the allow-noncombinator escape
# hatch. Reads candidate lines (each prefixed by '+') on stdin, prints the
# unfiltered text to stdout. Args: $1 = file path.
filter_allow_noncombinator() {
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
            if echo "$prev" | grep -q 'allow-noncombinator'; then
                continue
            fi
        fi
        # Same-line annotation also exempts.
        if echo "$text" | grep -q 'allow-noncombinator'; then
            continue
        fi
        added="${added}${text}
"
    done <<< "$candidates"
    printf '%s' "${added%$'\n'}"
}

files=$(git diff --name-only "$BASE" -- "$SCOPE" ':(exclude)**/*.md' 2>/dev/null || true)
if [ -z "$files" ]; then
    exit 0
fi

while IFS= read -r file; do
    [ -f "$file" ] || continue

    # Pull all added lines once (without line-number prefix) for reuse.
    diff_added=$(git diff --unified=0 "$BASE" -- "$file" | grep -E '^\+[^+]' || true)
    if [ -z "$diff_added" ]; then
        continue
    fi

    # (A) String-method dispatch.
    methods_hits=$(echo "$diff_added" | grep -Ev 'allow-noncombinator' | grep -E "$FORBIDDEN_METHODS" || true)
    methods_clean=$(filter_allow_noncombinator "$file" "$methods_hits")
    if [ -n "$methods_clean" ]; then
        report_methods="${report_methods}
  ${file}:"
        while IFS= read -r line; do
            report_methods="${report_methods}
    ${line}"
        done <<< "$methods_clean"
        FAIL=1
    fi

    # (B) Match-arm string-literal patterns.
    match_arm_hits=$(echo "$diff_added" | grep -Ev 'allow-noncombinator' | grep -E "$FORBIDDEN_MATCH_ARM" || true)
    match_arm_clean=$(filter_allow_noncombinator "$file" "$match_arm_hits")
    if [ -n "$match_arm_clean" ]; then
        report_match_arm="${report_match_arm}
  ${file}:"
        while IFS= read -r line; do
            report_match_arm="${report_match_arm}
    ${line}"
        done <<< "$match_arm_clean"
        FAIL=1
    fi

    # (C) Chained if-let-tag. Count occurrences in this file's added lines;
    # 2+ is the anti-pattern. Single occurrences are fine (and common).
    iflet_hits=$(echo "$diff_added" | grep -Ev 'allow-noncombinator' | grep -E "$IFLET_TAG_PATTERN" || true)
    iflet_clean=$(filter_allow_noncombinator "$file" "$iflet_hits")
    iflet_count=0
    if [ -n "$iflet_clean" ]; then
        iflet_count=$(printf '%s\n' "$iflet_clean" | grep -c '.' || true)
    fi
    if [ "$iflet_count" -ge 2 ]; then
        report_iflet_tag="${report_iflet_tag}
  ${file}: (${iflet_count} chained tag if-lets)"
        while IFS= read -r line; do
            report_iflet_tag="${report_iflet_tag}
    ${line}"
        done <<< "$iflet_clean"
        FAIL=1
    fi
done <<< "$files"

if [ "$FAIL" -eq 1 ]; then
    cat >&2 <<EOF
ERROR: New parser code violates the nom-combinator mandate.

The parser mandate (CLAUDE.md) requires nom combinators for ALL parsing
dispatch. Copy-paste-ready patterns for every common shape are in:

    crates/engine/src/parser/oracle_nom/PATTERNS.md

EOF

    if [ -n "$report_methods" ]; then
        cat >&2 <<EOF
(A) String-method dispatch — use combinators instead:
    .strip_prefix / .trim_start_matches  -> Pattern 1 (optional fixed prefix)
    .strip_suffix / .trim_end_matches    -> Pattern 2 or 3 (suffix / trailing)
    .contains / .starts_with / .ends_with -> Pattern 7 (integrate into parse)
    .split_once / .rsplit_once           -> Pattern 6 (delimiter split)
    .find("...")                         -> Pattern 5 (word-boundary scan)

Forbidden in added lines (diff vs ${BASE}):
${report_methods}

EOF
    fi

    if [ -n "$report_match_arm" ]; then
        cat >&2 <<EOF
(B) Match-arm dispatch on string literals — use alt((tag(...), tag(...))):
    match subject_tp.lower.trim() {                ->  alt((
        "creatures" => Some(TypedFilter::creature()),  tag("creatures").map(|_| TypedFilter::creature()),
        "permanents" => Some(TypedFilter::permanent()),tag("permanents").map(|_| TypedFilter::permanent()),
        ...                                             ...
    }                                                 )).parse(input)

Forbidden in added lines (diff vs ${BASE}):
${report_match_arm}

EOF
    fi

    if [ -n "$report_iflet_tag" ]; then
        cat >&2 <<EOF
(C) Chained if-let-tag blocks — collapse into a single alt(()):
    if let Ok((rest, _)) = tag("foo")(input) { ... }   ->  alt((
    if let Ok((rest, _)) = tag("bar")(input) { ... }       tag("foo"),
                                                            tag("bar"),
                                                          )).parse(input)?

Two or more sequential tag tries in one file are the chained anti-pattern.
A single if-let-tag for an optional prefix is fine.

Forbidden in added files (diff vs ${BASE}):
${report_iflet_tag}

EOF
    fi

    cat >&2 <<EOF
If a use is genuinely structural (not parsing dispatch) — e.g. TextPair
dual-string stripping, punctuation cleanup on pre-tokenized chunks, or
word-boundary scanning — annotate the line with:

    // allow-noncombinator: <one-line reason>

See PATTERNS.md section 9 for the criteria on legitimate escape-hatch use.

EOF
    exit 1
fi

exit 0
