#!/usr/bin/env bash
# Fixture seams for scripts/check-pr-body.sh (issue #10461).
#
# The gate's rules are the repository PR-body audit's rules (`scripts/pr_review.py`), so
# each row here states a body and the verdict the gate derives from that audit — except
# two groups the audit has no verdict for: the rows for the gate's ONE documented
# addition (every template section required and unique, which the audit tolerates), and
# the instrument/self-test rows, which pin the gate's own exit codes. The positive
# fixture is BUILT FROM .github/PULL_REQUEST_TEMPLATE.md, so a section added to the
# template is required and exercised the same day — no second list to drift.
#
# Usage: scripts/lib/pr_body_tests.sh   (exit 0 = all rows pass, 1 = a row failed)
# No cargo, no network, no repo mutation: everything happens in a temp directory.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
GATE="$REPO_ROOT/scripts/check-pr-body.sh"
TEMPLATE="$REPO_ROOT/.github/PULL_REQUEST_TEMPLATE.md"

PASS=0
FAIL=0
TMP="$(mktemp -d "${TMPDIR:-/tmp}/pr-body-tests.XXXXXX")" || {
    printf 'FAIL could not create a temp dir\n' >&2
    exit 1
}
trap 'rm -rf "$TMP"' EXIT

HEAD_A="4f12def7a841cd107eadcdc86f5824dda0c136bf"
HEAD_B="bf584d6bd39f254549be49ed9ff2b845e748e32c"
BASE="9b6e15ac7d6e2f0e8a4f0d1c6a7b3e5f2d8c9a10"
LAST_OUT=""

ok() {
    PASS=$((PASS + 1))
    printf 'ok   %s\n' "$1"
}

bad() {
    FAIL=$((FAIL + 1))
    printf 'FAIL %s\n' "$1"
}

# expect <row name> <expected exit> <body file> [head]
expect() {
    local name="$1" want="$2" body="$3" head="${4:-}"
    local rc
    if [ -n "$head" ]; then
        LAST_OUT="$("$GATE" "$body" "$head" 2>&1)"
    else
        LAST_OUT="$("$GATE" "$body" 2>&1)"
    fi
    rc=$?
    if [ "$rc" -eq "$want" ]; then
        ok "$name (exit $rc)"
    else
        bad "$name: expected exit $want, got $rc"
        printf '%s\n' "$LAST_OUT" | sed 's/^/     /'
    fi
}

expect_message() {
    local name="$1" needle="$2"
    if printf '%s' "$LAST_OUT" | grep -qF "$needle"; then
        ok "$name message names: $needle"
    else
        bad "$name: message does not name '$needle'"
        printf '%s\n' "$LAST_OUT" | sed 's/^/     /'
    fi
}

# A correctly filled body: the real template, boxes checked, both head-bound lines
# naming $1, and a real base.
mkbody() {
    local head="$1" out="$2"
    sed \
        -e 's/^- \[ \]/- [x]/' \
        -e "s|^Gate A PASS head=<40-hex-sha> base=<40-hex-sha>$|Gate A PASS head=$head base=$BASE|" \
        -e "s|^Final review-impl PASS head=<40-hex-sha>$|Final review-impl PASS head=$head|" \
        "$TEMPLATE" > "$out"
}

# ---------------------------------------------------------------- instrument
[ -f "$TEMPLATE" ] || { printf 'FAIL template missing: %s\n' "$TEMPLATE" >&2; exit 1; }
[ -x "$GATE" ] || { printf 'FAIL gate missing or not executable: %s\n' "$GATE" >&2; exit 1; }

TEMPLATE_SECTIONS="$(grep -cE '^## ' "$TEMPLATE")"
if [ "$TEMPLATE_SECTIONS" -ge 10 ]; then
    ok "reach: the template carries $TEMPLATE_SECTIONS sections"
else
    bad "reach: template carries only $TEMPLATE_SECTIONS sections"
fi

mkbody "$HEAD_A" "$TMP/good.md"

# ---------------------------------------------------------------- row 1: --fill
# What `gh pr create --fill` produces: commit subjects and bodies, no sections.
cat > "$TMP/fill.md" <<'EOF'
perf(engine): retire a shortcut repetition's write-only journal payloads

Issue #10475: a taken loop's journal grew with every repetition because each
cycle journals whole stack entries, trigger/LKI context sets and object
snapshots, and nothing dropped them until the turn boundary.

Refs #10475
EOF
expect "--fill body is refused" 1 "$TMP/fill.md" "$HEAD_A"
expect_message "--fill body" "missing template section: ## Summary"
expect_message "--fill body" "missing template section: ## Gate A"

# ---------------------------------------------------------------- row 2: correct
expect "template body with matching head passes" 0 "$TMP/good.md" "$HEAD_A"
expect_message "template body" "PR body PASS head=$HEAD_A"

# The pre-PR authoring case: no head argument yet, lines still required.
expect "template body passes without a head argument" 0 "$TMP/good.md"
expect_message "no-head run" "PR body PASS ("

# ---------------------------------------------------------------- row 3: wrong head
expect "a body bound to another head is refused" 1 "$TMP/good.md" "$HEAD_B"
expect_message "wrong head" "Gate A names head $HEAD_A, but the submitted head is $HEAD_B"

# ---------------------------------------------------------------- row 4: boxes
# The audit's box rule: each required label exactly once as `- [x] <label>` inside
# `## Verification`, never as `- [ ] <label>`.
python3 - "$TMP/good.md" > "$TMP/unchecked.md" <<'PY'
import sys

sys.stdout.write(open(sys.argv[1], encoding="utf-8").read().replace("- [x]", "- [ ]", 1))
PY
expect "one unchecked box is refused" 1 "$TMP/unchecked.md" "$HEAD_A"
expect_message "unchecked box" "unchecked verification box: Required checks ran clean"

python3 - "$TMP/good.md" > "$TMP/upper-box.md" <<'PY'
import sys

sys.stdout.write(open(sys.argv[1], encoding="utf-8").read().replace("- [x]", "- [X]"))
PY
expect "an uppercase checked box is refused (the audit reads lowercase only)" 1 "$TMP/upper-box.md" "$HEAD_A"
expect_message "uppercase box" "missing checked verification box"

python3 - "$TMP/good.md" > "$TMP/dup-box.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
label = "- [x] Both anchors cite existing analogous code at the same seam."
sys.stdout.write(body.replace(label, label + "\n" + label))
PY
expect "a duplicated checked box is refused" 1 "$TMP/dup-box.md" "$HEAD_A"
expect_message "duplicated box" "duplicated checked verification box"

# A stray unchecked item that is NOT one of the audit's required labels is the author's
# business: the audit passes it, so the gate must too.
python3 - "$TMP/good.md" > "$TMP/stray-box.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
marker = "- [x] Required checks ran clean"
head, _, rest = body.partition(marker)
sys.stdout.write(head + marker + rest.replace("\n", "\n- [ ] follow-up: none, this is a custom item\n", 1))
PY
expect "a custom unchecked item inside ## Verification is not a violation" 0 "$TMP/stray-box.md" "$HEAD_A"

# ---------------------------------------------------------------- row 5: sections
python3 - "$TMP/good.md" > "$TMP/no-section.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
sys.stdout.write(body.replace("## Claimed parse impact\n", ""))
PY
expect "a body missing one template section is refused" 1 "$TMP/no-section.md" "$HEAD_A"
expect_message "missing section" "missing template section: ## Claimed parse impact"

python3 - "$TMP/good.md" > "$TMP/dup-section.md" <<'PY'
import sys

sys.stdout.write(open(sys.argv[1], encoding="utf-8").read() + "\n## Summary\n\nagain\n")
PY
expect "a duplicate template section is refused" 1 "$TMP/dup-section.md" "$HEAD_A"
expect_message "duplicate section" "duplicate template section: ## Summary"

# ---------------------------------------------------------------- row 6: head-bound lines
python3 - "$TMP/good.md" > "$TMP/no-final.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
sys.stdout.write("\n".join(l for l in body.splitlines() if not l.startswith("Final review-impl PASS head=")))
PY
expect "a body without the Final review-impl line is refused" 1 "$TMP/no-final.md" "$HEAD_A"
expect_message "missing final line" "missing head-bound line in ## final review-impl"

# The audit reads each line from ITS OWN section: a real line parked elsewhere, with the
# template's placeholder left in the section, reads as missing there.
python3 - "$TMP/good.md" > "$TMP/misplaced.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
gate = f"Gate A PASS head={body.split('Gate A PASS head=')[1].split()[0]} base={body.split('base=')[1].split()[0]}"
lines = []
for line in body.splitlines():
    if line.startswith("Gate A PASS head="):
        lines.append("Gate A PASS head=<40-hex-sha> base=<40-hex-sha>")
    else:
        lines.append(line)
sys.stdout.write("\n".join(lines) + "\n\n" + gate + "\n")
PY
expect "head-bound lines outside their section are refused" 1 "$TMP/misplaced.md" "$HEAD_A"

# A duplicated head-bound line naming the submitted head is a second claim: the audit
# refuses it, so the gate must too.
python3 - "$TMP/good.md" > "$TMP/dup-line.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
lines = []
for line in body.splitlines():
    lines.append(line)
    if line.startswith("Gate A PASS head="):
        lines.append(line)
sys.stdout.write("\n".join(lines) + "\n")
PY
expect "a duplicated Gate A line is refused" 1 "$TMP/dup-line.md" "$HEAD_A"
expect_message "duplicated line" "duplicated head-bound line in ## gate a"

python3 - "$TMP/good.md" > "$TMP/no-base.md" <<'PY'
import sys

body = open(sys.argv[1], encoding="utf-8").read()
sys.stdout.write(body.replace(" base=" + "9b6e15ac7d6e2f0e8a4f0d1c6a7b3e5f2d8c9a10", ""))
PY
expect "a Gate A line without base is refused" 1 "$TMP/no-base.md" "$HEAD_A"

python3 - "$TMP/good.md" > "$TMP/uppercase.md" <<'PY'
import re
import sys

body = open(sys.argv[1], encoding="utf-8").read()
sys.stdout.write(re.sub(r"(PASS head=)([0-9a-f]{40})", lambda m: m.group(1) + m.group(2).upper(), body))
PY
expect "an uppercase SHA is refused (the audit parses lowercase only)" 1 "$TMP/uppercase.md" "$HEAD_A"

# Body validation does not use the audit's POSIX event-log locks. Exercise the
# real gate with that module unavailable, as on a native Windows Python.
mkdir "$TMP/no-locks"
printf 'import sys\nsys.modules["fcntl"] = None\n' > "$TMP/no-locks/sitecustomize.py"
PYTHONPATH="$TMP/no-locks${PYTHONPATH:+:$PYTHONPATH}" \
    expect "body validation works without POSIX event locks" 0 "$TMP/good.md" "$HEAD_A"
expect_message "no-lock body validation" "PR body PASS head=$HEAD_A"

# ---------------------------------------------------------------- row 7: instrument failures
expect "a missing body file is an instrument failure" 2 "$TMP/absent.md" "$HEAD_A"
"$GATE" "$TMP/good.md" "" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 2 ]; then ok "an explicitly empty head is an instrument failure"; else bad "empty head: expected exit 2, got $rc"; fi
"$GATE" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 2 ]; then ok "no arguments exits 2"; else bad "no arguments: expected exit 2, got $rc"; fi

NO_PY="$(mktemp -d "${TMPDIR:-/tmp}/pr-body-nopy.XXXXXX")"
for tool in bash grep mktemp tr cat dirname sed awk; do
    p="$(command -v "$tool" 2>/dev/null)" && ln -sf "$p" "$NO_PY/$tool"
done
nopy_out="$(PATH="$NO_PY" /bin/bash "$GATE" "$TMP/good.md" "$HEAD_A" 2>&1)"
rc=$?
if [ "$rc" -eq 2 ] && printf '%s' "$nopy_out" | grep -qF "could not run the body check"; then
    ok "a missing python3 is an instrument failure"
else
    bad "missing python3: expected exit 2 from the runner, got $rc"
    printf '%s\n' "$nopy_out" | sed 's/^/     /'
fi
rm -rf "$NO_PY"

# ---------------------------------------------------------------- row 9: missing audit module
# The gate's rules come from `scripts/pr_review.py`; without it there is no verdict to
# give, so the run is an instrument failure rather than a body violation.
NO_AUDIT="$(mktemp -d "${TMPDIR:-/tmp}/pr-body-noaudit.XXXXXX")"
mkdir -p "$NO_AUDIT/scripts/lib" "$NO_AUDIT/.github"
cp "$GATE" "$NO_AUDIT/scripts/check-pr-body.sh"
cp "$TEMPLATE" "$NO_AUDIT/.github/PULL_REQUEST_TEMPLATE.md"
"$NO_AUDIT/scripts/check-pr-body.sh" "$TMP/good.md" "$HEAD_A" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 2 ]; then ok "a missing audit module is an instrument failure"; else bad "missing audit: expected exit 2, got $rc"; fi
# A present audit that fails to import takes the PYTHON-level handler, not the bash
# pre-flight: a regression that reports an audit fault as a body violation must fail here.
printf 'raise ImportError("unimportable audit fixture")\n' > "$NO_AUDIT/scripts/pr_review.py"
"$NO_AUDIT/scripts/check-pr-body.sh" "$TMP/good.md" "$HEAD_A" >/dev/null 2>&1
rc=$?
if [ "$rc" -eq 2 ]; then ok "an unimportable audit module is an instrument failure"; else bad "unimportable audit: expected exit 2, got $rc"; fi
rm -rf "$NO_AUDIT"

# ---------------------------------------------------------------- row 8: self-test entry
# `--self-test` re-runs this suite; the flag the entry point sets stops the recursion,
# so the inner run skips this row and the outer run is the one that proves the entry.
if [ "${PR_BODY_SELFTEST:-0}" = "1" ]; then
    ok "reach: inner --self-test run (recursion row skipped)"
else
    "$GATE" --self-test >/dev/null 2>&1
    rc=$?
    if [ "$rc" -eq 0 ]; then
        ok "--self-test runs this suite and passes"
    else
        bad "--self-test exited $rc"
    fi
fi

printf '\n%s passed, %s failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
