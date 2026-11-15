#!/usr/bin/env bash
# Fixture seams for the coverage-regression check's numeric boundary (issue #10549).
#
# Native Windows jq terminates every line with CRLF, so a count read from it carries a CR into
# arithmetic and `[[ "$count" -gt "$base_count" ]]` reports "invalid arithmetic operator" instead of
# deciding. These rows run the REAL gate against fixture JSON with (a) a normal jq and (b) a jq stub
# that appends CR exactly as the Windows build does, and require the same verdict from both.
#
# Usage: scripts/lib/coverage_crlf_tests.sh   (exit 0 = all rows pass, 1 = a row failed)
# No network, no repo mutation: everything happens in a temp directory.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
GATE="$REPO_ROOT/scripts/coverage-regression-check.sh"

PASS=0
FAIL=0
TMP="$(mktemp -d "${TMPDIR:-/tmp}/cov-crlf-tests.XXXXXX")" || {
    printf 'FAIL could not create a temp dir\n' >&2
    exit 1
}
trap 'rm -rf "$TMP"' EXIT

ok() { PASS=$((PASS + 1)); printf 'ok   %s\n' "$1"; }
bad() { FAIL=$((FAIL + 1)); printf 'FAIL %s\n' "$1"; }

# The real jq, resolved BEFORE any PATH override.
REAL_JQ="$(type -P jq 2>/dev/null)" || {
    printf 'FAIL jq not found on PATH\n' >&2
    exit 1
}

[ -x "$GATE" ] || { printf 'FAIL gate missing or not executable: %s\n' "$GATE" >&2; exit 1; }

# --- fixtures ---------------------------------------------------------------------------------
# Minimal but valid: the fields the gate reads before the diagnostics block, plus two diagnostic
# categories so both the `-gt` and the `-lt` arms have a subject.
cat > "$TMP/base.json" <<'EOF'
{"total_cards":10,"supported_cards":5,"cards":[],"diagnostics":{"swallowed_clause":68,"ignored_remainder":1}}
EOF
cat > "$TMP/worse.json" <<'EOF'
{"total_cards":10,"supported_cards":5,"cards":[],"diagnostics":{"swallowed_clause":70,"ignored_remainder":1}}
EOF
cat > "$TMP/better.json" <<'EOF'
{"total_cards":10,"supported_cards":5,"cards":[],"diagnostics":{"swallowed_clause":66,"ignored_remainder":1}}
EOF

# --- stubs ------------------------------------------------------------------------------------
mkdir -p "$TMP/crlf" "$TMP/text"
# What native Windows jq does: every printed line ends CRLF.
cat > "$TMP/crlf/jq" <<EOF
#!/usr/bin/env bash
# pipefail belongs to the STUB, not the suite: without it the pipe reports awk's status and every
# jq -e branch looks true under this stub — a false model rather than a CRLF regression.
set -o pipefail
"$REAL_JQ" "\$@" | awk '{printf "%s\\r\\n", \$0}'
EOF
# A jq that answers with prose where a count is required: the gate must refuse, not guess.
cat > "$TMP/text/jq" <<'EOF'
#!/usr/bin/env bash
printf 'n/a\r\n'
EOF
mkdir -p "$TMP/mid_total" "$TMP/mid_curr_total" "$TMP/mid_diag"
cat > "$TMP/mid_total/jq" <<EOF
#!/usr/bin/env bash
case "\$*" in
    *'total_cards // 0'*) printf 'n/a\\r\\n' ;;
    *) exec "$REAL_JQ" "\$@" ;;
esac
EOF
cat > "$TMP/mid_curr_total/jq" <<EOF
#!/usr/bin/env bash
case "\$*" in
    *'total_cards // 0'*worse.json*) printf 'n/a\\r\\n' ;;
    *) exec "$REAL_JQ" "\$@" ;;
esac
EOF
cat > "$TMP/mid_diag/jq" <<EOF
#!/usr/bin/env bash
case "\$*" in
    *.diagnostics\[*) printf 'n/a\\r\\n' ;;
    *) exec "$REAL_JQ" "\$@" ;;
esac
EOF
chmod +x "$TMP/crlf/jq" "$TMP/text/jq" "$TMP/mid_total/jq" "$TMP/mid_curr_total/jq" "$TMP/mid_diag/jq"

# --- helpers ----------------------------------------------------------------------------------
# run <jq-dir|-> <baseline> <current> : stdout+stderr in $OUT, status in $RC
run() {
    local jqdir="$1" baseline="$2" current="$3"
    if [ "$jqdir" = "-" ]; then
        OUT="$("$GATE" "$baseline" "$current" 2>&1)"
    else
        OUT="$(PATH="$jqdir:$PATH" "$GATE" "$baseline" "$current" 2>&1)"
    fi
    RC=$?
}

expect_same() {
    local label="$1" plain_out="$2" plain_rc="$3"
    if [ "$RC" -eq "$plain_rc" ] && [ "$OUT" = "$plain_out" ]; then
        ok "$label: the CRLF jq produces the same verdict as the plain jq (exit $RC)"
    else
        bad "$label: CRLF jq diverges from the plain jq (exit $RC vs $plain_rc)"
        diff <(printf '%s\n' "$plain_out") <(printf '%s\n' "$OUT") | head -12 | sed 's/^/     /'
    fi
}

# --- row 1: the control, on the platform's own jq ----------------------------------------------
run - "$TMP/base.json" "$TMP/worse.json"
if [ "$RC" -eq 0 ] && printf '%s' "$OUT" | grep -q "DIAGNOSTIC NOTE: swallowed_clause increased from 68 to 70"; then
    ok "control: a plain jq reports the increase (exit 0)"
else
    bad "control: expected the increase note and exit 0, got exit $RC"
    printf '%s\n' "$OUT" | sed 's/^/     /'
fi
PLAIN_INCREASE_OUT="$OUT"
PLAIN_INCREASE_RC="$RC"

run - "$TMP/base.json" "$TMP/better.json"
if [ "$RC" -eq 0 ] && printf '%s' "$OUT" | grep -q "DIAGNOSTIC IMPROVEMENT: swallowed_clause decreased from 68 to 66"; then
    ok "control: a plain jq reports the decrease (exit 0)"
else
    bad "control: expected the improvement note and exit 0, got exit $RC"
    printf '%s\n' "$OUT" | sed 's/^/     /'
fi
PLAIN_DECREASE_OUT="$OUT"
PLAIN_DECREASE_RC="$RC"

# --- row 2: the same fixtures under a CRLF jq ---------------------------------------------------
# This is the regression: at the unfixed base the `-gt` comparison raises "invalid arithmetic
# operator" and the run does not produce this verdict at all.
run "$TMP/crlf" "$TMP/base.json" "$TMP/worse.json"
expect_same "increase under CRLF" "$PLAIN_INCREASE_OUT" "$PLAIN_INCREASE_RC"
if printf '%s' "$OUT" | grep -qi "invalid arithmetic operator"; then
    bad "increase under CRLF: the run still reports an arithmetic error"
else
    ok "increase under CRLF: no arithmetic error"
fi

# The `-lt` arm the issue names separately.
run "$TMP/crlf" "$TMP/base.json" "$TMP/better.json"
expect_same "decrease under CRLF" "$PLAIN_DECREASE_OUT" "$PLAIN_DECREASE_RC"

# --- row 3: a non-numeric count is refused, never silently compared -----------------------------
run "$TMP/text" "$TMP/base.json" "$TMP/worse.json"
if [ "$RC" -ne 0 ] && printf '%s' "$OUT" | grep -q "non-numeric value where a count was required"; then
    ok "a non-numeric jq value fails loudly (exit $RC)"
else
    bad "a non-numeric jq value must fail loudly, got exit $RC"
    printf '%s\n' "$OUT" | sed 's/^/     /'
fi

# --- row 4: a refusal on a LATER numeric read is still named on stderr -------------------------
# These three sites used to carry a leftover `2>/dev/null` from their bare-jq days, which discarded
# `jq_num`'s own message and left a bare exit 2 with empty stderr — the failure this row pins.
for target in mid_total mid_curr_total mid_diag; do
    run "$TMP/$target" "$TMP/base.json" "$TMP/worse.json"
    if [ "$RC" -ne 0 ] && printf '%s' "$OUT" | grep -q "non-numeric value where a count was required"; then
        ok "a mid-run refusal at $target is named on stderr (exit $RC)"
    else
        bad "a mid-run refusal at $target must be named, got exit $RC"
        printf '%s\n' "$OUT" | sed 's/^/     /'
    fi
done

# --- row 5: a sign-only value is refused, not silently "clean" ---------------------------------
# `-` passes a naive `[!0-9-]` guard and then fails inside `[[ ]]`'s condition context with no
# verdict and exit 0 — the false clean this boundary exists to prevent.
mkdir -p "$TMP/signonly"
cat > "$TMP/signonly/jq" <<EOF
#!/usr/bin/env bash
case "\$*" in
    *.diagnostics\[*) printf -- '-\\r\\n' ;;
    *) exec "$REAL_JQ" "\$@" ;;
esac
EOF
chmod +x "$TMP/signonly/jq"
run "$TMP/signonly" "$TMP/base.json" "$TMP/worse.json"
if [ "$RC" -ne 0 ] && printf '%s' "$OUT" | grep -q "non-numeric value where a count was required"; then
    ok "a sign-only value is refused (exit $RC)"
else
    bad "a sign-only value must be refused, got exit $RC"
    printf '%s\n' "$OUT" | sed 's/^/     /'
fi

# --- row 6: the loop's own count is validated, not just CR-stripped ----------------------------
# The two diagnostics loops derive `count` from raw `jq -r … | to_entries[]`. A non-numeric value
# there used to die later on `set -u` as an unnamed `n: unbound variable`; it now refuses by name.
mkdir -p "$TMP/loop_count"
cat > "$TMP/loop_count/jq" <<EOF
#!/usr/bin/env bash
set -o pipefail
case "\$*" in
    *to_entries*) printf 'swallowed_clause=n/a\\r\\n' ;;
    *) exec "$REAL_JQ" "\$@" ;;
esac
EOF
chmod +x "$TMP/loop_count/jq"
run "$TMP/loop_count" "$TMP/base.json" "$TMP/worse.json"
if [ "$RC" -ne 0 ] && printf '%s' "$OUT" | grep -q "non-numeric diagnostic count"; then
    ok "a non-numeric diagnostic count is refused by name (exit $RC)"
else
    bad "a non-numeric diagnostic count must be refused by name, got exit $RC"
    printf '%s\n' "$OUT" | sed 's/^/     /'
fi

# --- row 7: an embedded CR is not a line ending and must not manufacture a count ---------------
mkdir -p "$TMP/embedded_cr"
cat > "$TMP/embedded_cr/jq" <<EOF
#!/usr/bin/env bash
case "\$*" in
    *.diagnostics\[*) printf '1\\r2\\r\\n' ;;
    *) exec "$REAL_JQ" "\$@" ;;
esac
EOF
chmod +x "$TMP/embedded_cr/jq"
run "$TMP/embedded_cr" "$TMP/base.json" "$TMP/worse.json"
if [ "$RC" -ne 0 ] && printf '%s' "$OUT" | grep -q "non-numeric value where a count was required"; then
    ok "an embedded CR count is refused (exit $RC)"
else
    bad "an embedded CR count must be refused, got exit $RC"
    printf '%s\n' "$OUT" | sed 's/^/     /'
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
