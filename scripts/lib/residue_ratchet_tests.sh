#!/usr/bin/env bash
# Real Gate G/H/A fixture seams, under macOS bash 3.2. No cargo or lane index
# mutations. Tilt runs this suite; CI runs the gate and its inline Python suite.
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# A caller may be a pre-commit hook. Never let its git environment redirect
# fixture writes to the lane's index, worktree, refs, or object database.
unset GIT_INDEX_FILE GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR
unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
BASH_BIN=/bin/bash
PASS=0
FAIL=0
FIXTURE=""
OUT=""
RC=0
FIXTURES=()
cleanup() {
    local dir
    for dir in "${FIXTURES[@]}"; do rm -rf "$dir"; done
}
trap cleanup EXIT
ok() { printf '  ok: %s\n' "$1"; PASS=$((PASS + 1)); }
fail() { printf '  FAIL: %s\n%s\n' "$1" "$OUT" >&2; FAIL=$((FAIL + 1)); }
expect_rc() { if [ "$RC" -eq "$2" ]; then ok "$1"; else fail "$1 (exit $RC, expected $2)"; fi; }
expect_out() { if [[ "$OUT" == *"$2"* ]]; then ok "$1"; else fail "$1 (missing $2)"; fi; }
new_fixture() {
    FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/residue-ratchet.XXXXXX")"
    FIXTURES+=("$FIXTURE")
    mkdir -p "$FIXTURE/scripts/lib" "$FIXTURE/crates/engine/src/parser" "$FIXTURE/crates/engine/src/game"
    cp "$REPO_ROOT/scripts/check-parser-combinators.sh" "$FIXTURE/scripts/"
    cp "$REPO_ROOT/scripts/zone_authority_census.py" "$FIXTURE/scripts/"
    cp "$REPO_ROOT/scripts/lib/detect-residue-discards.py" \
       "$REPO_ROOT/scripts/lib/detect_residue_discards_tests.py" \
       "$REPO_ROOT/scripts/lib/detect-cross-product-alts.py" \
       "$REPO_ROOT/scripts/lib/detect_cross_product_alts_tests.py" "$FIXTURE/scripts/lib/"
    cp "$REPO_ROOT/crates/engine/src/game/mod.rs" "$FIXTURE/crates/engine/src/game/"
    cat > "$FIXTURE/crates/engine/src/parser/oracle.rs" <<'EOF'
fn parse_oracle_ir() {
}
fn is_semicolon_keyword_line() {
}
fn is_spell_resolution_instruction_line() {
}
EOF
    printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
    printf '# fixture reviewed ceiling\ncrates/engine/src/parser/probe.rs 1\n' > "$FIXTURE/scripts/residue-ratchet.txt"
    git -C "$FIXTURE" init -q
    git -C "$FIXTURE" config user.email fixture@example.invalid
    git -C "$FIXTURE" config user.name 'Gate H fixture'
    git -C "$FIXTURE" add .
    git -C "$FIXTURE" -c core.hooksPath=/dev/null commit -qm baseline
    BASE="$(git -C "$FIXTURE" rev-parse HEAD)"
    # A real non-parser base/head window selects worktree H and A's skip/pass.
    printf 'fixture window\n' > "$FIXTURE/.window"
    git -C "$FIXTURE" add .window
    git -C "$FIXTURE" -c core.hooksPath=/dev/null commit -qm window
}
run_gate() {
    OUT="$(cd "$FIXTURE" && "$BASH_BIN" scripts/check-parser-combinators.sh "$BASE" 2>&1)"
    RC=$?
}
run_cached() {
    OUT="$(cd "$FIXTURE" && GIT_INDEX_FILE="$FIXTURE/.git/index" "$BASH_BIN" scripts/check-parser-combinators.sh HEAD 2>&1)"
    RC=$?
}
ledger() { cat > "$FIXTURE/scripts/residue-ratchet.txt"; }
add_discard() {
    printf 'fn added() { let (v, _) = parse_target_with_ctx(input, ctx); }\n' >> "$FIXTURE/crates/engine/src/parser/probe.rs"
}
printf 'driving real Gate H under %s (%s)\n' "$BASH_BIN" "$("$BASH_BIN" -c 'echo "$BASH_VERSION"')"

new_fixture
run_gate
expect_rc 'baseline ceiling passes' 0
expect_out 'real worktree snapshot is named' 'Gate H PASS (residue: 1 legacy obligations, ceiling 1; files scanned 2; snapshot worktree)'
OUT="$(cd "$FIXTURE" && "$BASH_BIN" scripts/check-parser-combinators.sh "$(git rev-parse HEAD)" 2>&1)"; RC=$?
expect_rc 'clean same-head window retains cannot-answer exit' 3
expect_out 'H runs before A cannot-answer exit' 'Gate H PASS'
expect_out 'A cannot-answer wording survives' 'Gate A CANNOT ANSWER: nothing was scanned.'
add_discard
run_gate
expect_rc 'introduced production discard fails' 1
expect_out 'growth names file and line' 'crates/engine/src/parser/probe.rs:2: binding parse_target_with_ctx'
expect_out 'growth names actual and ceiling' 'actual 2, ceiling 1'
printf 'SMOKE introduced discard: exit %s\n%s\n' "$RC" "$OUT"
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'removing introduced discard passes again' 0
printf 'SMOKE removed discard: exit %s\n%s\n' "$RC" "$OUT"
printf 'fn nom() { let (rest, _) = parse_target_demonstrative_subject(input)?; }\n' >> "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'nom second-component unit discard passes' 0

# Per-file ceilings and strict ledger parsing, including disappeared files.
ledger <<'EOF'
crates/engine/src/parser/probe.rs 3
EOF
run_gate
expect_rc 'below ceiling passes' 0
expect_out 'tightening suggestion is exact' 'crates/engine/src/parser/probe.rs 3 -> 1'
ledger <<'EOF'
crates/engine/src/parser/probe.rs 1
crates/engine/src/parser/retired.rs 2
EOF
run_gate
expect_rc 'deleted file is zero not a failure' 0
expect_out 'deleted row suggests removal' 'crates/engine/src/parser/retired.rs 2 -> 0 (entry can be deleted)'
ledger <<'EOF'
# comments and blanks

crates\engine\src\parser\probe.rs 1
EOF
run_gate
expect_rc 'comments blanks and normalized separators pass' 0
for rows in '3 1' '1 3' '1 1'; do
    set -- $rows
    printf 'crates/engine/src/parser/missing.rs %s\ncrates/engine/src/parser/missing.rs %s\n' "$1" "$2" | ledger
    run_gate
    expect_rc "duplicate missing-file rows $rows fail" 1
    expect_out 'duplicate path is actionable' "duplicate ledger entry for 'crates/engine/src/parser/missing.rs'"
done
for row in \
    'crates/engine/src/parser/probe.rs many' \
    'crates/engine/src/parser/probe.rs -1' \
    'crates/engine/src/parser/probe.rs 1 extra' \
    '/crates/engine/src/parser/probe.rs 1' \
    'crates/engine/src/parser/../probe.rs 1' \
    'crates/engine/src/game/probe.rs 1'; do
    printf '%s\n' "$row" | ledger
    run_gate
    expect_rc "malformed/out-of-scope row rejected: $row" 1
done
ledger < /dev/null
run_gate
expect_rc 'nonzero file without ledger row fails' 1
expect_out 'unledgered file is named' 'probe.rs: actual 1, ceiling 0 (no ledger entry)'
printf 'fn f() { let (v, rest) = parse_target(input); consume(rest); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'zero obligations with an empty ledger passes' 0
expect_out 'zero-hit scan reports files' 'residue: 0 legacy obligations, ceiling 0; files scanned 2'
OUT="$(python3 -B "$FIXTURE/scripts/lib/detect-residue-discards.py" --list --root "$FIXTURE" 2>&1)"; RC=$?
expect_rc 'zero-hit list is a real successful scan' 0
expect_out 'zero-hit list reports snapshot and scan count' 'TOTAL 0 obligations; files scanned 2; snapshot worktree'
rm -rf "$FIXTURE/crates/engine/src/parser"
OUT="$(python3 -B "$FIXTURE/scripts/lib/detect-residue-discards.py" --check 2>&1)"; RC=$?
expect_rc 'detector rejects missing parser root' 1
expect_out 'missing-root error is explicit' 'missing parser root'
mkdir -p "$FIXTURE/crates/engine/src/parser"
OUT="$(python3 -B "$FIXTURE/scripts/lib/detect-residue-discards.py" --check 2>&1)"; RC=$?
expect_rc 'detector rejects empty parser root' 1
expect_out 'empty-root error is explicit' 'empty parser scope'

# Index owns both source and ledger: unstaged edits cannot fix staged faults or
# make an unrelated staged snapshot fail. Worktree/index tooling is not swapped.
new_fixture
add_discard
git -C "$FIXTURE" add crates/engine/src/parser/probe.rs
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_cached
expect_rc 'unstaged fix cannot hide indexed violation' 1
expect_out 'indexed violation retains original line' 'probe.rs:2: binding parse_target_with_ctx'
git -C "$FIXTURE" add crates/engine/src/parser/probe.rs
add_discard
run_cached
expect_rc 'unstaged violation cannot poison clean index' 0
expect_out 'index snapshot named' 'snapshot index)'
rm "$FIXTURE/scripts/residue-ratchet.txt"
run_cached
expect_rc 'unstaged ledger deletion cannot poison clean index' 0
printf 'crates/engine/src/parser/probe.rs 1\n' > "$FIXTURE/scripts/residue-ratchet.txt"
rm "$FIXTURE/crates/engine/src/parser/probe.rs"
run_cached
expect_rc 'unstaged source deletion cannot poison clean index' 0
printf 'let (v, _) = parse_target(input);\n' > "$FIXTURE/crates/engine/src/parser/unstaged.rs"
run_cached
expect_rc 'unstaged new source is not part of indexed snapshot' 0
rm "$FIXTURE/crates/engine/src/parser/unstaged.rs"
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
ledger <<'EOF'
crates/engine/src/parser/probe.rs 0
EOF
git -C "$FIXTURE" add scripts/residue-ratchet.txt
ledger <<'EOF'
crates/engine/src/parser/probe.rs 1
EOF
run_cached
expect_rc 'unstaged ledger increase cannot loosen indexed ceiling' 1
git -C "$FIXTURE" add scripts/residue-ratchet.txt
printf 'let (v, _) = parse_target(input);\n' > "$FIXTURE/crates/engine/src/parser/new.rs"
git -C "$FIXTURE" add crates/engine/src/parser/new.rs
run_cached
expect_rc 'indexed new source without row fails' 1
printf 'crates/engine/src/parser/new.rs 1\n' >> "$FIXTURE/scripts/residue-ratchet.txt"
git -C "$FIXTURE" add scripts/residue-ratchet.txt
run_cached
expect_rc 'indexed source and indexed ledger addition pass' 0
rm "$FIXTURE/crates/engine/src/parser/new.rs"
git -C "$FIXTURE" add -u crates/engine/src/parser/new.rs
run_cached
expect_rc 'indexed source deletion passes as zero' 0
expect_out 'indexed deletion tightens row' 'crates/engine/src/parser/new.rs 1 -> 0'
rm "$FIXTURE/scripts/residue-ratchet.txt"
git -C "$FIXTURE" add -u scripts/residue-ratchet.txt
printf 'crates/engine/src/parser/probe.rs 1\n' > "$FIXTURE/scripts/residue-ratchet.txt"
run_cached
expect_rc 'unstaged ledger cannot replace indexed deletion' 1
expect_out 'missing indexed ledger is identified' 'scripts/residue-ratchet.txt'

# Availability failures are loud, including the inline seam suite.
for dependency in lib/detect-residue-discards.py residue-ratchet.txt lib/detect_residue_discards_tests.py; do
    new_fixture
    rm "$FIXTURE/scripts/$dependency"
    run_gate
    expect_rc "missing dependency fails: $dependency" 1
    expect_out 'missing dependency reports Gate H' 'Gate H FAIL: required residue gate file missing'
done
new_fixture
printf 'raise RuntimeError("deliberately broken seam suite")\n' > "$FIXTURE/scripts/lib/detect_residue_discards_tests.py"
run_gate
expect_rc 'failing detector seam suite blocks gate' 1
expect_out 'seam-suite failure reported' 'residue detector self-tests failed'
new_fixture
mkdir "$FIXTURE/no-python"
for binary in git dirname awk grep; do
    ln -s "$(command -v "$binary")" "$FIXTURE/no-python/$binary"
done
OUT="$(cd "$FIXTURE" && PATH="$FIXTURE/no-python" "$BASH_BIN" scripts/check-parser-combinators.sh "$BASE" 2>&1)"; RC=$?
expect_rc 'missing python fails closed' 1
expect_out 'missing python message actionable' 'Gate H FAIL: python3 is required'

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
