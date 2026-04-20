#!/usr/bin/env bash
# Diff-based gate: new parser code must not introduce string-matching methods
# used for parsing dispatch. Forces nom combinators on first write per the
# CLAUDE.md mandate, rather than leaving refactor-to-combinators for review.
#
# Existing non-combinator code in the parser is frozen in amber — this check
# only flags *newly added* offending lines in the diff.
#
# Exempt: lines with "// allow-noncombinator: <reason>" annotation. Legitimate
# uses are rare (TextPair dual-string helpers, punctuation stripping on already-
# tokenized input, dynamic-string prefixes with runtime-computed tag bodies).
#
# Usage:
#   scripts/check-parser-combinators.sh [base-ref]
#
# Default base-ref is the merge-base with origin/main (the branch divergence
# point). In CI, pass the PR target branch explicitly.

set -euo pipefail

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"
SCOPE='crates/engine/src/parser'
# String-matching-for-parsing patterns. The "..." suffix on `.contains` /
# `.starts_with` / `.ends_with` / `.find` / `.trim_*_matches` matches only
# string-literal arguments — `.contains(&item)` (Vec/slice op) and
# `.trim_end_matches('.')` (char arg, structural punctuation cleanup) are
# legitimate and not flagged. strip_prefix/strip_suffix/split_once almost
# always operate on string literals; flag unconditionally.
FORBIDDEN='\.strip_prefix\(|\.strip_suffix\(|\.split_once\(|\.rsplit_once\(|\.contains\("|\.starts_with\("|\.ends_with\("|\.find\("|\.trim_end_matches\("|\.trim_start_matches\("'

files=$(git diff --name-only "$BASE" -- "$SCOPE" 2>/dev/null || true)
if [ -z "$files" ]; then
    exit 0
fi

FAIL=0
report=""

while IFS= read -r file; do
    [ -f "$file" ] || continue
    added=$(git diff --unified=0 "$BASE" -- "$file" \
        | grep -E '^\+[^+]' \
        | grep -Ev 'allow-noncombinator' \
        | grep -E "$FORBIDDEN" \
        || true)
    if [ -n "$added" ]; then
        report="${report}
  ${file}:"
        while IFS= read -r line; do
            report="${report}
    ${line}"
        done <<< "$added"
        FAIL=1
    fi
done <<< "$files"

if [ "$FAIL" -eq 1 ]; then
    cat >&2 <<EOF
ERROR: New parser code uses forbidden string-matching methods.

The parser mandate (CLAUDE.md) requires nom combinators for ALL parsing
dispatch. Copy-paste-ready patterns for every common shape are in:

    crates/engine/src/parser/oracle_nom/PATTERNS.md

Likely matches for the patterns below:
  .strip_prefix / .trim_start_matches -> Pattern 1 (optional fixed prefix)
  .strip_suffix / .trim_end_matches   -> Pattern 2 or 3 (optional suffix /
                                         trailing clause after token sequence)
  .contains / .starts_with / .ends_with -> Pattern 7 (integrate into parse)
  .split_once / .rsplit_once          -> Pattern 6 (delimiter split)
  .find("...")                        -> Pattern 5 (word-boundary scan)

Forbidden in added lines (diff vs ${BASE}):
${report}

If a use is genuinely structural (not parsing dispatch) — e.g. TextPair
dual-string stripping, punctuation cleanup on pre-tokenized chunks, or
word-boundary scanning — annotate the line with:

    // allow-noncombinator: <one-line reason>

See PATTERNS.md section 9 for the criteria on legitimate escape-hatch use.

EOF
    exit 1
fi

exit 0
