#!/usr/bin/env bash
# Tests for scripts/check-prelowered-ratchet.sh.
#
# The regression these exist for: the gate was written with `declare -A`, which
# bash 3.2 -- macOS's `/bin/bash`, and often the first `bash` on PATH -- rejects
# outright, so the pre-commit hook died on its first line and every contributor
# on a stock Mac committed with `--no-verify`, disabling every OTHER hook step
# too. The port to parallel indexed arrays then introduced a second, quieter
# fault: `declare -A` made a repeated ledger path last-write-wins, while a
# linear lookup makes it first-match, so a stale looser row could shadow a
# tightened one and pass a count the old gate failed.
#
# So these drive the SCRIPT as a whole -- ledger in, exit code and message out
# -- not its lookup helpers in isolation. A test that only asserted "the lookup
# finds a path" could not have caught either fault: the first killed the script
# before any lookup ran, and the second turned on which of two equally findable
# rows was returned.
#
# Each case runs against a throwaway repo in a mktemp dir: `scripts/` holding a
# copy of the real gate plus a fixture ledger, and `crates/engine/src/parser/`
# holding .rs files with a chosen number of `PreLowered` occurrences. The gate
# resolves both its ledger and its repo root from its own location, so a copy in
# a fixture tree measures the fixture and nothing else.
#
# Run under the OLDEST bash on the box (`/bin/bash` when it exists, which on
# macOS is the 3.2 that started all this), because "works under bash 3.2" is
# half of what is being asserted -- running only under a PATH bash 5 would let
# the original bug back in unnoticed.
#
# Venue: the Tilt `prelowered-ratchet` resource (label 'lint'). NOT GitHub CI --
# enrolling a script gate there needs a `.github/workflows/**` edit, which is a
# hard stop for agent changes, the same limit pnpm-preflight and lobby-servers
# run under. CI does run the gate itself (ci.yml), just not these tests.
#
# Run:  bash scripts/lib/prelowered_ratchet_tests.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/../check-prelowered-ratchet.sh"

if [ ! -f "$GATE" ]; then
  echo "prelowered_ratchet_tests: gate not found at $GATE" >&2
  exit 1
fi

# The gate measures with ripgrep; without it every count reads 0 and the
# over-ceiling cases would pass for the wrong reason.
if ! command -v rg >/dev/null 2>&1; then
  echo "prelowered_ratchet_tests: ripgrep (rg) is required" >&2
  exit 1
fi

# `/bin/bash` is bash 3.2 on macOS and the shell the pre-commit hook's
# `#!/usr/bin/env bash` most often resolves to there.
BASH_BIN="/bin/bash"
[ -x "$BASH_BIN" ] || BASH_BIN="$(command -v bash)"
printf 'driving %s under %s (%s)\n\n' \
  "$(basename "$GATE")" "$BASH_BIN" "$("$BASH_BIN" -c 'echo "$BASH_VERSION"')"

PASS=0
FAIL=0
fail() { printf '  FAIL: %s\n' "$1" >&2; FAIL=$((FAIL + 1)); }
ok()   { printf '  ok: %s\n' "$1";     PASS=$((PASS + 1)); }

FIXTURE=""
OUT=""
RC=0

# Every fixture is torn down on exit, including on a failing assertion, so a red
# run doesn't leave a dozen trees in $TMPDIR. Only ever holds `mktemp -d` paths.
FIXTURES=()
cleanup() {
  local d
  for d in ${FIXTURES+"${FIXTURES[@]}"}; do
    [ -n "$d" ] && [ -d "$d" ] && rm -rf "$d"
  done
}
trap cleanup EXIT

# A repo root with only what the gate reads: its own copy in scripts/, and the
# parser tree it greps.
new_fixture() {
  FIXTURE="$(mktemp -d)"
  FIXTURES+=("$FIXTURE")
  mkdir -p "$FIXTURE/scripts" "$FIXTURE/crates/engine/src/parser"
  cp "$GATE" "$FIXTURE/scripts/check-prelowered-ratchet.sh"
}

# Ledger body on stdin, so each case reads as the file it is testing.
ledger() { cat > "$FIXTURE/scripts/prelowered-ratchet.txt"; }

# parser_file <path-under-parser/> <occurrences>
parser_file() {
  local rel="$1" n="$2" i
  mkdir -p "$(dirname "$FIXTURE/crates/engine/src/parser/$rel")"
  : > "$FIXTURE/crates/engine/src/parser/$rel"
  for (( i = 0; i < n; i++ )); do
    echo "    OracleNodeIr::PreLowered(x)," >> "$FIXTURE/crates/engine/src/parser/$rel"
  done
}

run_gate() {
  OUT="$("$BASH_BIN" "$FIXTURE/scripts/check-prelowered-ratchet.sh" 2>&1)"
  RC=$?
}

expect_rc() {
  local desc="$1" want="$2"
  if [ "$RC" -eq "$want" ]; then
    ok "$desc"
  else
    fail "$desc (exit $RC, wanted $want)
$(printf '%s\n' "$OUT" | sed 's/^/      | /')"
  fi
}

expect_out() {
  local desc="$1" needle="$2"
  case "$OUT" in
    *"$needle"*) ok "$desc" ;;
    *) fail "$desc (no '$needle' in output)
$(printf '%s\n' "$OUT" | sed 's/^/      | /')" ;;
  esac
}

# --- the review finding: a repeated ledger path -----------------------------
# `declare -A` resolved these last-write-wins; the indexed-array port resolved
# them first-match. The two disagree in OPPOSITE directions depending on row
# order, which is why neither is a contract worth keeping -- both cases below
# are the same ambiguous ledger, and both must be rejected.

# Old gate: ceiling 1, count 2 -> FAIL. First-match port: ceiling 3 -> PASS.
# That gap is the ratchet being bypassed by a stale row, so: rejected.
new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 3
crates/engine/src/parser/oracle.rs 1
EOF
parser_file oracle.rs 2
run_gate
expect_rc  "duplicate ledger path (loose row first) is rejected" 1
expect_out "duplicate ledger path names the path" "duplicate ledger entry for 'crates/engine/src/parser/oracle.rs'"

# The mirror image: old gate PASSES (ceiling 3), first-match port FAILS
# (ceiling 1). Same file, same counts, only the row order flipped -- proof the
# answer was order-dependent rather than merely first-vs-last.
new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 1
crates/engine/src/parser/oracle.rs 3
EOF
parser_file oracle.rs 2
run_gate
expect_rc "duplicate ledger path (tight row first) is rejected too" 1

# Identical rows are unambiguous in effect but are still a ledger authoring
# error, and exempting them would reintroduce a value-dependent special case in
# the one place the rule needs to be structural.
new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 3
crates/engine/src/parser/oracle.rs 3
EOF
parser_file oracle.rs 2
run_gate
expect_rc "duplicate ledger path with equal ceilings is rejected" 1

# A duplicate must be caught even when nothing measures it, so the ledger is
# repaired when it is edited rather than when a file happens to trip it.
new_fixture
ledger <<'EOF'
crates/engine/src/parser/gone.rs 2
crates/engine/src/parser/gone.rs 1
EOF
run_gate
expect_rc "duplicate ledger path is rejected with no matching file" 1

# --- the ratchet contract itself (plan 6.1) ---------------------------------
# These are the behaviours the bash 3.2 port had to carry over unchanged; they
# are here so the next edit to the lookup cannot quietly drop one.

new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 2
EOF
parser_file oracle.rs 2
run_gate
expect_rc  "count equal to ceiling passes" 0
expect_out "equal count reports the gate name" "Gate P PASS"

new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 2
EOF
parser_file oracle.rs 3
run_gate
expect_rc  "count above ceiling fails" 1
expect_out "over-ceiling names the count and the ceiling" "has 3 'PreLowered' occurrence(s), ceiling is 2"

new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 5
EOF
parser_file oracle.rs 2
run_gate
expect_rc  "count below ceiling passes" 0
expect_out "slack prints the exact tightening edit" "crates/engine/src/parser/oracle.rs 5 -> 2"

# The whole reason the gate greps: a producer smuggled into a file nobody
# listed must fail, not go unmeasured.
new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 1
EOF
parser_file oracle.rs 1
parser_file oracle_new.rs 1
run_gate
expect_rc  "unledgered file carrying PreLowered fails" 1
expect_out "unledgered file is named" "crates/engine/src/parser/oracle_new.rs has 1 'PreLowered' occurrence(s) but no ledger entry"

new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 3
EOF
run_gate
expect_rc  "ledger entry whose file is gone passes as slack" 0
expect_out "retired entry is flagged for deletion" "(entry can be deleted)"

# Generated snapshots are output, not producers; counting them would move the
# ratchet for reasons unrelated to the burn-down.
new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs 1
EOF
parser_file oracle.rs 1
parser_file oracle_ir/snapshots/big.rs 9
run_gate
expect_rc "PreLowered under snapshots/ is not counted" 0

# --- ledger hygiene ---------------------------------------------------------

new_fixture
ledger <<'EOF'
# a comment mentioning PreLowered

crates/engine/src/parser/oracle.rs 1   # trailing prose is ignored
EOF
parser_file oracle.rs 1
run_gate
expect_rc "comments and blank lines are skipped" 0

new_fixture
ledger <<'EOF'
crates/engine/src/parser/oracle.rs lots
EOF
parser_file oracle.rs 1
run_gate
expect_rc  "non-numeric ceiling is rejected" 1
expect_out "malformed ceiling is quoted back" "malformed ledger line"

new_fixture
rm -f "$FIXTURE/scripts/prelowered-ratchet.txt"
run_gate
expect_rc  "missing ledger is rejected" 1
expect_out "missing ledger says where it looked" "ledger not found at"

# An empty ledger with an empty parser tree exercises both arrays at length 0.
# bash 3.2 errors on `"${arr[@]}"` under `set -u` when the array is empty, so
# this is the case that catches an unguarded expansion being added later.
new_fixture
ledger < /dev/null
run_gate
expect_rc "empty ledger and empty tree do not trip set -u" 0

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
