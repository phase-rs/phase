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

# Args: [--cached] <base> -- <pathspec>...   Sets ADDED_LINES to the helper's
# `<path>\t<line>\t<content>` records, after the helper's suite has passed.
# Exits 2 when the helper cannot read the diff.
added_lines_load() {
    added_lines_check_suite
    ADDED_LINES="$(python3 "$(dirname "${BASH_SOURCE[0]}")/added-lines.py" "$@")" || exit 2
}

# $1 = path. Prints that file's records from ADDED_LINES as `<line>\t<content>`.
added_lines_for() {
    awk -v p="$1"$'\t' 'index($0, p) == 1 { print substr($0, length(p) + 1) }' <<< "$ADDED_LINES"
}
