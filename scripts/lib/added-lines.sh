# shellcheck shell=bash
# Bash binding of scripts/lib/added-lines.py for the diff-based gates
# (check-parser-combinators.sh, check-engine-authorities.sh,
# check-test-card-data-load.sh, check-legacy-quantity-callsites.sh).
#
# Sourced, not executed. A gate computes its genuinely-added line set once, over
# its whole view (a move pairs lines across files), then reads each file's
# records from it.

# Set once the helper's own suite has passed in this gate process, so a gate
# that runs the suite early (the parser gate, ahead of its diff-only exits)
# does not run it again in added_lines_load. Reset here so an inherited
# variable cannot skip the suite.
ADDED_LINES_SUITE_PASSED=""
ADDED_LINES_CACHED=""

# The helper's own suite, ahead of every scan that relies on it (the same shape
# as the parser gate's D0 and the authority gate's B0): a regression in the
# helper does not fail a gate, it silently exempts or re-flags lines. It costs
# ~3s. Exits 1 when the suite is red.
added_lines_check_suite() {
    if [ -n "$ADDED_LINES_SUITE_PASSED" ]; then
        return 0
    fi
    if ! python3 "$(dirname "${BASH_SOURCE[0]}")/added_lines_tests.py" >/dev/null 2>&1; then
        echo "ERROR: the added-line helper's own test suite is RED." >&2
        echo "       This gate cannot tell moved lines from added ones until it passes:" >&2
        echo "           python3 scripts/lib/added_lines_tests.py" >&2
        exit 1
    fi
    ADDED_LINES_SUITE_PASSED=1
}

# $1 = base ref. A base that names no commit would leave the diff, and so the
# verdict, empty. Exits 2 (no verdict) instead.
added_lines_require_base() {
    local base_oid
    if ! base_oid=$(git rev-parse --verify --quiet --end-of-options "$1" 2>/dev/null) \
        || ! git cat-file -e "${base_oid}^{commit}" >/dev/null 2>&1; then
        echo "ERROR: base ref '$1' does not resolve to a commit; no verdict." >&2
        exit 2
    fi
}

# Args: [--cached] <base> -- <pathspec>...   Sets ADDED_LINES to the helper's
# `<path>\t<line>\t<content>` records, after the helper's suite has passed,
# and latches whether those lines index the index (`--cached`) or the working
# tree. Exits 2 when the helper cannot read the diff.
added_lines_load() {
    added_lines_check_suite
    ADDED_LINES_CACHED=""
    if [ "${1:-}" = "--cached" ]; then ADDED_LINES_CACHED=1; fi
    ADDED_LINES="$(python3 "$(dirname "${BASH_SOURCE[0]}")/added-lines.py" "$@")" || exit 2
}

# $1 = path. Prints that file's records from ADDED_LINES as `<line>\t<content>`.
added_lines_for() {
    awk -v p="$1"$'\t' 'index($0, p) == 1 { print substr($0, length(p) + 1) }' <<< "$ADDED_LINES"
}

# $1 = `<line>\t<content>` records, $2 = ERE. Prints the records whose `+<content>`
# matches $2; the gates' patterns anchor on the diff's `+`. Returns 2, after an
# error, when the match itself fails.
added_lines_grep() {
    local nums
    # Only grep's no-match status is expected; preserve failures in other stages.
    nums=$(
        set -o pipefail
        printf '%s\n' "$1" | cut -f2- | sed 's/^/+/' |
            { grep -nE -- "$2" || [ "$?" -eq 1 ]; } | cut -d: -f1
    ) || { echo "ERROR: added-line match failed for ERE '$2'; no verdict." >&2; return 2; }
    [ -n "$nums" ] || return 0
    awk 'NR == FNR { want[$1]; next } FNR in want' <(printf '%s\n' "$nums") <(printf '%s\n' "$1")
}

# $1 = path, $2 = line number. That line of the post-image the records index.
added_lines_text_at() {
    if [ -n "$ADDED_LINES_CACHED" ]; then
        git show ":$1" | sed -n "$2p"
    else
        sed -n "$2p" "$1"
    fi
}
