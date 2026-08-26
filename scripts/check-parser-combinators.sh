#!/usr/bin/env bash
# Diff-based gate: new parser code must not introduce string-matching dispatch
# patterns. Forces nom combinators on first write per the CLAUDE.md mandate,
# rather than leaving refactor-to-combinators for review.
#
# Existing non-combinator code in the parser is frozen in amber — this check
# only flags *newly added* offending lines in the diff.
#
# Six forbidden pattern families:
#   (A) String-method dispatch: .strip_prefix / .contains("...") / .split_once
#       / .find("...") / .rfind("...") / .split("...") / .splitn / etc.
#       Use nom combinators (tag, alt, take_until) instead.
#   (B) Match-arm dispatch on string literals: `match expr { "foo" => ..., }`.
#       Discriminant is parser text; arms are literals. Use alt((tag(...))).
#   (C) Chained `if let Ok((rest, _)) = tag("…")(input)` blocks (≥2 in one
#       file). Sequential tag tries should compose into a single alt(()).
#   (D) Un-factored cross-product alt: a flat `alt` whose ≥4 `tag` arms share a
#       long common prefix AND suffix (e.g. "in addition to {its,their,...} other
#       [creature ]types"). Factor each varying axis into its own alt()/opt()
#       inside a sequence; see PATTERNS.md section 8b. Multi-line structural
#       check delegated to scripts/lib/detect-cross-product-alts.py.
#   (E) Verbatim-sentence equality: `lower == "twenty-five plus chars..."`.
#       Matching a whole Oracle sentence handles exactly one card — decompose
#       into typed building blocks (grammar prefix/suffix combinators).
#   (F) Hand-constructed `Effect::Unimplemented { .. }` literals. The single
#       authority is `Effect::unimplemented(name, fragment)` — it documents
#       the name-is-a-category-key contract the coverage report depends on.
#       Match-arm destructuring (`Effect::Unimplemented { name, .. } =>`) is
#       not flagged.
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

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CROSS_PRODUCT_DETECTOR="$SCRIPT_DIR/lib/detect-cross-product-alts.py"

BASE="${1:-$(git merge-base origin/main HEAD 2>/dev/null || echo HEAD~1)}"
BASE_SHA="$(git rev-parse "$BASE")"
HEAD_SHA="$(git rev-parse HEAD)"
SCOPE='crates/engine/src/parser'

# When invoked as a pre-commit hook (GIT_INDEX_FILE is set, or no explicit base
# was provided and BASE == HEAD), only check staged changes to avoid flagging
# another agent's unstaged work in the working tree.
DIFF_MODE=""
if [ -n "${GIT_INDEX_FILE:-}" ] || [ "$BASE" = "$(git rev-parse HEAD 2>/dev/null)" ]; then
    DIFF_MODE="--cached"
fi

# ---------------------------------------------------------------------------
# (G) ROUTER/GRANT ARCHITECTURE GATE  — Plan 02 step 5 item 13 / step 7.
#
# WHOLE-FILE, not diff-based: this is an architecture invariant, so it is
# checked on every invocation regardless of what the diff touches.
#
# THE BOUNDARY. There are two keyword-parsing surfaces and they are NOT
# interchangeable:
#
#   parse_router_keyword_line()      STRICT, whole line. All-consuming: returns a
#   parse_router_keyword_list()      STRICT, keyword list (comma parts + MTGJSON).
#   parse_router_keyword_fragment()  STRICT, one keyword phrase.
#                                    These return a typed keyword ONLY when the text
#                                    parses completely (keyword + permitted P/R/M
#                                    tail). They are the ONLY surfaces that may
#                                    license a router to CONSUME a line.
#
#   parse_granted_keyword_fragment() PERMISSIVE. By design they take the leading
#   extract_granted_keyword_list()   keyword and DISCARD the remainder — correct
#                                    for an EMBEDDED grant ("...gains vanishing 3
#                                    if ..." inside a static/token/vote payload),
#                                    and invalid at a whole-line router boundary,
#                                    where the discarded remainder is dropped
#                                    SEMANTICS with no Unimplemented raised.
#
# A router that advances a line on a permissive parse is a SILENT SWALLOW: no
# keyword recorded, no diagnostic, and the card renders as fully supported.
#
# STATUS: MIGRATION COMPLETE (task #123). Plan 02 step 5 wired the strict router into
# priorities 9 and 13; task #123 migrated the remaining 16 permissive calls (priorities
# 0, 1b, 8f, the flashback/suspend/specialize/buyback/escalate/commander-ninjutsu/d20
# intercepts, and the two routing classifiers) onto the strict surfaces. The permissive
# symbols are now ABSENT from oracle.rs entirely — not merely unused, but not imported.
#
# This gate is therefore no longer a ratchet with an allowlist. It is the plain
# invariant Plan 02 step 7 asks for: NO permissive keyword-parser symbol may appear in
# a router context, at any count, ever. A reintroduction fails the build.
#
# SPAN EXTRACTION: from the `fn NAME(` signature at column 0 to the next `}` at
# column 0. Brace COUNTING would be wrong here — oracle.rs is saturated with mana
# symbols ("{T}", "{2}{B}") inside string literals, and a naive counter reads
# those as scope. rustfmt guarantees a top-level fn closes with `}` at column 0,
# so the anchor is exact. Full-line comments are excluded so that prose NAMING a
# permissive symbol is not miscounted as a call.
# ---------------------------------------------------------------------------
ORACLE_RS='crates/engine/src/parser/oracle.rs'
PERMISSIVE_SYMS='parse_granted_keyword_fragment|extract_granted_keyword_list'
# Pre-rename spellings. These must not come back under any name, in any context.
LEGACY_SYMS='parse_keyword_from_oracle|extract_keyword_line'

# NOT a router-context symbol: `parse_crew_keyword`. Plan 02 step 5 item 11 groups it
# with the remainder-discarding helpers, but that describes the PRE-step-5 code. As it
# stands it is strict: its cadence tail is `all_consuming(tag("activate only once each
# turn"))`, so "Crew 2 if you control an artifact" returns None rather than eating the
# suffix, and its call site advances only inside `if let Some`.

arch_fail=0

router_span() {
    # $1 = fn name. Emits that fn's body with full-line comments stripped.
    awk -v fn="$1" '
        $0 ~ "^([a-z(),: ]*)?fn " fn "\\(" { inside = 1 }
        inside && /^\}$/                   { exit }
        inside && $0 !~ /^[[:space:]]*\/\// { print }
    ' "$ORACLE_RS"
}

for ctx in parse_oracle_ir is_semicolon_keyword_line is_spell_resolution_instruction_line; do
    span="$(router_span "$ctx")"
    if [ -z "$span" ]; then
        echo "✗ (G) router context '$ctx' not found in $ORACLE_RS — the gate is blind; fix the span anchor." >&2
        arch_fail=1
        continue
    fi
    actual="$(printf '%s\n' "$span" | grep -cE "$PERMISSIVE_SYMS" || true)"
    if [ "$actual" -ne 0 ]; then
        echo "✗ (G) $ctx: $actual permissive keyword-parser call(s), expected 0." >&2
        echo "      Routers must consume a line only via the STRICT surfaces" >&2
        echo "      (parse_router_keyword_line / _list / _fragment). The permissive" >&2
        echo "      surface discards the remainder and silently swallows semantics —" >&2
        echo "      no keyword, no diagnostic, and the card renders as fully supported." >&2
        echo "      This migration is COMPLETE (task #123); do not reintroduce it." >&2
        arch_fail=1
    fi
done

legacy_hits="$(grep -nE "$LEGACY_SYMS" "$ORACLE_RS" | grep -vE '^[0-9]+:[[:space:]]*//' || true)"
if [ -n "$legacy_hits" ]; then
    echo "✗ (G) legacy permissive symbol reintroduced in $ORACLE_RS:" >&2
    printf '%s\n' "$legacy_hits" >&2
    echo "      parse_keyword_from_oracle/extract_keyword_line were RENAMED to the" >&2
    echo "      grant-context names to make this boundary nameable. Do not resurrect them." >&2
    arch_fail=1
fi

if [ "$arch_fail" -ne 0 ]; then
    echo "" >&2
    echo "Gate G FAIL (router/grant architecture)." >&2
    exit 1
fi
printf 'Gate G PASS (router/grant architecture: strict router vs permissive grant boundary intact)\n'

# ---------------------------------------------------------------------------
# DO NOT "harden" families (A), (B), (E), (F) by filtering them through the
# census lexer (scripts/zone_authority_census.py `strip_noncode`). It looks like
# the obvious single-authority move. It would BLIND this gate. (#76)
#
# Those families match ON the string literal itself:
#
#     .contains("        "lit" =>        == "long sentence"        name: "..."
#
# `strip_noncode` returns a code stream with literals REMOVED, so the very quote
# each pattern keys on is gone: every one of them would silently stop matching
# real violations, and the gate would pass string-matching parser code forever
# while reporting green. (Measured during #76: routing these through stripped
# code misread 194 REAL match arms as non-matches.)
#
# What they are exposed to is the mirror-image, and it is BENIGN: a forbidden
# pattern written inside a COMMENT or a literal is a FALSE HIT. That is loud (it
# blocks a commit, the author sees exactly why) and it already has an escape
# hatch (`// allow-noncombinator:`). #76 measured 10 such lines in the whole
# parser scope, all of them doc comments describing the forbidden pattern.
# A false MISS from this class is structurally impossible here: these greps read
# raw text, so literal-awareness could only ever REMOVE matches, never add them.
#
# If the false hits ever become a real cost, the fix is NOT strip_noncode: it is
# a POSITION mask (which character offsets are code vs comment vs literal), and
# these greps would filter candidates by match offset. Two APIs, one grammar.
#
# Family (D) is the opposite case and DOES delegate: it needs paren counting on
# CODE (see scripts/lib/detect-cross-product-alts.py), where a `)` inside
# `take_until(")")` is data. It reads structure from the code stream and arms
# from the raw text.
# ---------------------------------------------------------------------------

# (A) String-method dispatch. The "..." suffix on `.contains` / `.starts_with`
# / `.ends_with` / `.find` / `.rfind` / `.split` / `.trim_*_matches` matches
# only string-literal arguments — `.contains(&item)` (Vec/slice op),
# `.trim_end_matches('.')` (char arg, structural cleanup), and the documented
# `.find(' ')` word-boundary-scan idiom are legitimate. strip_prefix /
# strip_suffix / split_once / rsplit_once / splitn almost always operate on
# string literals; flag unconditionally.
FORBIDDEN_METHODS='\.strip_prefix\(|\.strip_suffix\(|\.split_once\(|\.rsplit_once\(|\.splitn\(|\.contains\("|\.starts_with\("|\.ends_with\("|\.find\("|\.rfind\("|\.split\("|\.trim_end_matches\("|\.trim_start_matches\("'

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

# (E) Verbatim-sentence equality. `expr == "long literal"` (or reversed) with a
# 25+-char literal is a whole-Oracle-sentence match — the single most
# prohibited pattern. Short literals (`== "x"` for a counter symbol, type word)
# are legitimate leaf comparisons and stay unflagged.
FORBIDDEN_VERBATIM_EQ='(==|!=)[[:space:]]*"[^"]{25,}"|"[^"]{25,}"[[:space:]]*(==|!=)'

# (F) Hand-constructed Unimplemented literal. Construction is either a
# single-line `Effect::Unimplemented { name: ...` (colon after `name`) or a
# multi-line opener ending in `{`. Destructuring patterns (`{ name, .. } =>`,
# `{ .. }`) match neither alternative.
FORBIDDEN_UNIMPL_LITERAL='Effect::Unimplemented[[:space:]]*\{[[:space:]]*$|Effect::Unimplemented[[:space:]]*\{[[:space:]]*name:'

FAIL=0
report_methods=""
report_match_arm=""
report_iflet_tag=""
report_crossprod=""
report_verbatim_eq=""
report_unimpl=""

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

# Outlined test files (`mod tests;` / `#[path] mod ..._tests;` siblings) are
# #[cfg(test)]-gated by their module declaration and contain only test fixtures
# and assertions (e.g. `assert!(s.contains("..."))`) — never production parser
# dispatch, which would be dead code under cfg(test). They lose the inline
# `#[cfg(test)]` marker a line-based scan keys on, so exclude them by name; their
# parent module file is still fully scanned, including any inline test fixtures.
files=$(git diff $DIFF_MODE --name-only "$BASE" -- "$SCOPE" \
    ':(exclude)**/*.md' \
    ':(exclude)**/tests.rs' \
    ':(exclude)**/*_tests.rs' 2>/dev/null || true)
if [ -z "$files" ]; then
    printf 'Gate A PASS head=%s base=%s\n' "$HEAD_SHA" "$BASE_SHA"
    exit 0
fi

# (D0) Family (D)'s own seam suite, ahead of the scan it protects — the same
# shape as section (B0) of check-engine-authorities.sh, and for the same reason.
# The cross-product detector finds an `alt` block's end by COUNTING PARENS, which
# is a lex; a regression there does not fail this gate, it silently mis-scopes it
# in both directions (a phantom block from an `alt((` in a comment BLOCKS a good
# commit; a stray `))` in a comment truncates a real block so a cross product
# SHIPS). Neither shows up as a failure here, so the suite that pins the lexing
# runs first. It costs ~5ms and only when parser files actually changed.
if command -v python3 >/dev/null 2>&1 && [ -f "$SCRIPT_DIR/lib/detect_cross_product_alts_tests.py" ]; then
    if ! python3 "$SCRIPT_DIR/lib/detect_cross_product_alts_tests.py" >/dev/null 2>&1; then
        echo "ERROR: the cross-product detector's own test suite is RED." >&2
        echo "       Family (D) cannot be trusted until it passes:" >&2
        echo "           python3 scripts/lib/detect_cross_product_alts_tests.py" >&2
        exit 1
    fi
fi

while IFS= read -r file; do
    [ -f "$file" ] || continue

    # Pull all added lines once (without line-number prefix) for reuse.
    diff_added=$(git diff $DIFF_MODE --unified=0 "$BASE" -- "$file" | grep -E '^\+[^+]' || true)
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

    # (E) Verbatim-sentence equality comparisons.
    verbatim_hits=$(echo "$diff_added" | grep -Ev 'allow-noncombinator' | grep -E "$FORBIDDEN_VERBATIM_EQ" || true)
    verbatim_clean=$(filter_allow_noncombinator "$file" "$verbatim_hits")
    if [ -n "$verbatim_clean" ]; then
        report_verbatim_eq="${report_verbatim_eq}
  ${file}:"
        while IFS= read -r line; do
            report_verbatim_eq="${report_verbatim_eq}
    ${line}"
        done <<< "$verbatim_clean"
        FAIL=1
    fi

    # (F) Hand-constructed Effect::Unimplemented literals.
    unimpl_hits=$(echo "$diff_added" | grep -Ev 'allow-noncombinator' | grep -E "$FORBIDDEN_UNIMPL_LITERAL" || true)
    unimpl_clean=$(filter_allow_noncombinator "$file" "$unimpl_hits")
    if [ -n "$unimpl_clean" ]; then
        report_unimpl="${report_unimpl}
  ${file}:"
        while IFS= read -r line; do
            report_unimpl="${report_unimpl}
    ${line}"
        done <<< "$unimpl_clean"
        FAIL=1
    fi

    # (D) Un-factored cross-product alt. Multi-line structural check: feed the
    # unified=0 diff for this file to the Python detector, which maps added
    # lines onto post-image `alt` blocks and flags those with >=4 tag arms
    # sharing a long common prefix AND suffix. Skipped (not failed) if python3
    # is unavailable, so the gate degrades gracefully outside CI.
    if command -v python3 >/dev/null 2>&1 && [ -f "$CROSS_PRODUCT_DETECTOR" ]; then
        crossprod_hits=$(git diff $DIFF_MODE --unified=0 "$BASE" -- "$file" \
            | python3 "$CROSS_PRODUCT_DETECTOR" "$file" 2>/dev/null || true)
        if [ -n "$crossprod_hits" ]; then
            report_crossprod="${report_crossprod}
${crossprod_hits}"
            FAIL=1
        fi
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
    .split_once / .rsplit_once / .splitn -> Pattern 6 (delimiter split)
    .split("...")                        -> Pattern 6 (delimiter split)
    .find("...") / .rfind("...")         -> Pattern 5 (word-boundary scan)

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

    if [ -n "$report_verbatim_eq" ]; then
        cat >&2 <<EOF
(E) Verbatim-sentence equality — the single most prohibited pattern:
    lower == "whole oracle sentence here"   ->  decompose into typed building
                                                blocks: grammar prefix/suffix
                                                combinators + typed enum variants
A whole-sentence match handles exactly one card. Identify the grammatical
structure and parse each axis with combinators so the pattern covers every
card in the class.

Forbidden in added lines (diff vs ${BASE}):
${report_verbatim_eq}

EOF
    fi

    if [ -n "$report_unimpl" ]; then
        cat >&2 <<EOF
(F) Hand-constructed Effect::Unimplemented literal — use the constructor:
    Effect::Unimplemented {                ->  Effect::unimplemented(
        name: "...".into(),                        "pattern_class_key",
        description: Some(text.into()),            unparsed_fragment,
    }                                          )
The \`name\` must be a stable snake_case pattern-class key (the coverage
report groups parse gaps by it) — never the raw Oracle text fragment.

Forbidden in added lines (diff vs ${BASE}):
${report_unimpl}

EOF
    fi

    if [ -n "$report_crossprod" ]; then
        cat >&2 <<EOF
(D) Un-factored cross-product alt — factor each varying axis (PATTERNS.md §8b):
    alt((                                      ->  recognize((
        tag("in addition to its other types"),     tag("in addition to "),
        tag("in addition to their other types"),   alt((tag("its"), tag("their"), ...)),
        tag("in addition to his other types"),      tag(" other "),
        ... (8 arms = 4 pronouns x 2 scopes)        opt(tag("creature ")),
    ))                                              tag("types"),
                                                ))
The arm count should be the SUM of per-axis choices, never the PRODUCT.

Flagged blocks (diff vs ${BASE}):
${report_crossprod}

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

printf 'Gate A PASS head=%s base=%s\n' "$HEAD_SHA" "$BASE_SHA"
exit 0
