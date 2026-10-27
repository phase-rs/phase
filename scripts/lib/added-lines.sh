# shellcheck shell=bash
# Bash binding of scripts/lib/added-lines.py for the diff-based gates
# (check-parser-combinators.sh, check-engine-authorities.sh,
# check-test-card-data-load.sh, check-legacy-quantity-callsites.sh).
#
# Sourced, not executed. A gate computes its genuinely-added line set once, over
# its whole view (a move pairs lines across files), then reads each file's
# records from it.

# Args: [--cached] <base> -- <pathspec>...   Sets ADDED_LINES to the helper's
# `<path>\t<line>\t<content>` records.
#
# The helper's own suite runs here, ahead of every scan that relies on it (the
# same shape as the parser gate's D0 and the authority gate's B0): a regression
# in the helper does not fail a gate, it silently exempts or re-flags lines. It
# costs ~3s, and only when the gate has in-scope changes.
added_lines_load() {
    local lib
    lib="$(dirname "${BASH_SOURCE[0]}")"
    if ! python3 "$lib/added_lines_tests.py" >/dev/null 2>&1; then
        echo "ERROR: the added-line helper's own test suite is RED." >&2
        echo "       This gate cannot tell moved lines from added ones until it passes:" >&2
        echo "           python3 scripts/lib/added_lines_tests.py" >&2
        exit 1
    fi
    ADDED_LINES="$(python3 "$lib/added-lines.py" "$@")" || exit 2
}

# $1 = path. Prints that file's records from ADDED_LINES as `<line>\t<content>`.
added_lines_for() {
    awk -v p="$1"$'\t' 'index($0, p) == 1 { print substr($0, length(p) + 1) }' <<< "$ADDED_LINES"
}
