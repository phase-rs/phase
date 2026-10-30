#!/usr/bin/env bash
# Real diff-gate fixture seams for the move-aware added-line set
# (scripts/lib/added-lines.py), under macOS bash 3.2. Each case commits a change
# in a throwaway repo and runs a copy of the real gate against its base commit:
# a verbatim move passes, a planted line in the same diff is still listed (the
# reach guard), and a line moved out of a file the gate excludes is checked.
# No cargo and no lane index mutations. Tilt runs this suite.
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# A caller may be a pre-commit hook. Never let its git environment redirect
# fixture writes to the lane's index, worktree, refs, object database or config,
# or turn the gates' `:(exclude)**/...` globs into literal pathspecs.
unset GIT_INDEX_FILE GIT_DIR GIT_WORK_TREE GIT_PREFIX GIT_COMMON_DIR
unset GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES
unset GIT_CONFIG_PARAMETERS GIT_CONFIG_COUNT
unset GIT_LITERAL_PATHSPECS GIT_GLOB_PATHSPECS GIT_NOGLOB_PATHSPECS GIT_ICASE_PATHSPECS
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
BASH_BIN=/bin/bash
PASS=0
FAIL=0
FIXTURE=""
BASE=""
GATE=""
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
# The lines a gate lists under its report header, without the help text around
# them: every non-blank line between the header and the next blank line.
listed() {
    printf '%s\n' "$OUT" | awk '
        /^(Forbidden in added lines|New parser diff lines call legacy)/ { on = 1; next }
        on && /^$/ { if (seen) { on = 0; seen = 0 }; next }
        on { seen = 1; print }
    '
}
expect_listed() {
    local got
    got="$(listed)"
    if [ "$got" = "$2" ]; then ok "$1"; else fail "$1 (listed:
$got
expected:
$2)"; fi
}
# $1 = gate script name. A repo holding that gate and the added-line helper.
new_fixture() {
    GATE="$1"
    FIXTURE="$(mktemp -d "${TMPDIR:-/tmp}/diff-gates.XXXXXX")"
    FIXTURES+=("$FIXTURE")
    mkdir -p "$FIXTURE/scripts/lib"
    cp "$REPO_ROOT/scripts/$GATE" "$FIXTURE/scripts/"
    cp "$REPO_ROOT/scripts/lib/added-lines.sh" \
       "$REPO_ROOT/scripts/lib/added-lines.py" \
       "$REPO_ROOT/scripts/lib/added_lines_tests.py" "$FIXTURE/scripts/lib/"
    git -C "$FIXTURE" init -q
    git -C "$FIXTURE" config user.email fixture@example.invalid
    git -C "$FIXTURE" config user.name 'diff-gate fixture'
}
# $1 = path, remaining args = its lines.
put() {
    local path="$FIXTURE/$1"
    shift
    mkdir -p "$(dirname "$path")"
    printf '%s\n' "$@" > "$path"
}
move() { mkdir -p "$(dirname "$FIXTURE/$2")" && git -C "$FIXTURE" mv "$1" "$2"; }
commit() {
    git -C "$FIXTURE" add -A
    git -C "$FIXTURE" commit -q --no-verify -m "$1"
}
commit_base() { commit baseline; BASE="$(git -C "$FIXTURE" rev-parse HEAD)"; }
run_gate() {
    OUT="$(cd "$FIXTURE" && "$BASH_BIN" "scripts/$GATE" "$BASE" 2>&1)"
    RC=$?
}
# shellcheck disable=SC2016 # $BASH_VERSION expands in the child shell.
printf 'driving the real diff gates under %s (%s)\n' "$BASH_BIN" "$("$BASH_BIN" -c 'echo "$BASH_VERSION"')"

# --- check-parser-combinators.sh: family (A) -------------------------------
# Gates G and H run first on every invocation: G needs its three router
# contexts in oracle.rs, H its detector, seam suite and an (empty) ledger, and
# the detector's import reads game/mod.rs.
parser_fixture() {
    new_fixture check-parser-combinators.sh
    cp "$REPO_ROOT/scripts/zone_authority_census.py" "$FIXTURE/scripts/"
    cp "$REPO_ROOT/scripts/lib/detect-residue-discards.py" \
       "$REPO_ROOT/scripts/lib/detect_residue_discards_tests.py" \
       "$REPO_ROOT/scripts/lib/detect-cross-product-alts.py" \
       "$REPO_ROOT/scripts/lib/detect_cross_product_alts_tests.py" "$FIXTURE/scripts/lib/"
    mkdir -p "$FIXTURE/crates/engine/src/game"
    cp "$REPO_ROOT/crates/engine/src/game/mod.rs" "$FIXTURE/crates/engine/src/game/"
    put crates/engine/src/parser/oracle.rs \
        'fn parse_oracle_ir() {' '}' \
        'fn is_semicolon_keyword_line() {' '}' \
        'fn is_spell_resolution_instruction_line() {' '}'
    put scripts/residue-ratchet.txt '# fixture ledger: no residue'
}
P_FROZEN='fn frozen(t: &str) -> Option<&str> { t.strip_prefix("when ") }'
P_PLANTED='fn planted(t: &str) -> Option<(&str, &str)> { t.split_once(", ") }'

parser_fixture
put crates/engine/src/parser/clause.rs 'use super::*;' "$P_FROZEN" 'fn other() {}'
commit_base
move crates/engine/src/parser/clause.rs crates/engine/src/parser/clause/mod.rs
commit rename
run_gate
expect_rc 'parser: whole-file rename of a frozen line passes' 0
expect_out 'parser: the rename was scanned' 'Gate A: scanned 1 file(s)'
expect_out 'parser: the rename reaches Gate A PASS' 'Gate A PASS head='
printf '%s\n' "$P_PLANTED" >> "$FIXTURE/crates/engine/src/parser/clause/mod.rs"
commit planted
run_gate
expect_rc 'parser: rename plus a planted line fails' 1
expect_listed 'parser: only the planted line is listed' "  crates/engine/src/parser/clause/mod.rs:
    $P_PLANTED"

parser_fixture
put crates/engine/src/parser/clause_tests.rs 'fn t() {}' "$P_FROZEN"
put crates/engine/src/parser/clause.rs 'fn other() {}'
commit_base
put crates/engine/src/parser/clause_tests.rs 'fn t() {}'
put crates/engine/src/parser/clause.rs 'fn other() {}' "$P_FROZEN"
commit 'move out of a test file'
run_gate
expect_rc 'parser: a line moved out of an excluded *_tests.rs file fails' 1
expect_listed 'parser: the line moved out of the excluded file is listed' "  crates/engine/src/parser/clause.rs:
    $P_FROZEN"

# The AST-layer grammar module is in the gate's scope: a line moved into it from
# the parser is a move, a line planted in it is listed, and other types/ files
# stay outside the gate's view.
parser_fixture
put crates/engine/src/parser/clause.rs 'use super::*;' "$P_FROZEN" 'fn other() {}'
commit_base
put crates/engine/src/parser/clause.rs 'use super::*;' 'fn other() {}'
put crates/engine/src/types/text_grammar.rs "$P_FROZEN"
commit 'move into the grammar module'
run_gate
expect_rc 'parser: a line moved into types/text_grammar.rs passes' 0
expect_out 'parser: types/text_grammar.rs was scanned' 'Gate A: scanned 2 file(s)'
printf '%s\n' "$P_PLANTED" >> "$FIXTURE/crates/engine/src/types/text_grammar.rs"
commit planted
run_gate
expect_rc 'parser: a line planted in types/text_grammar.rs fails' 1
expect_listed 'parser: only the line planted in the grammar module is listed' "  crates/engine/src/types/text_grammar.rs:
    $P_PLANTED"
put crates/engine/src/types/other.rs "$P_PLANTED"
commit 'plant outside the grammar module'
run_gate
expect_listed 'parser: a line planted in another types/ file is not listed' "  crates/engine/src/types/text_grammar.rs:
    $P_PLANTED"

# --- check-engine-authorities.sh: family (A) -------------------------------
# Sections (B0)-(D) are full-tree censuses of the real engine. Here they are
# stubs that pass, so section (A), the diff-based one, decides the exit.
authority_fixture() {
    local census
    new_fixture check-engine-authorities.sh
    for census in zone_authority_census_tests.py zone_authority_census.py \
                  draw_replacement_census.py post_replacement_continuation_census.py; do
        put "scripts/$census" '# fixture stub: this suite drives section (A) only'
    done
}
A_FROZEN='fn flies(o: &GameObject) -> bool { o.keywords.contains(&Keyword::Flying) }'
A_PLANTED='fn any_kw(o: &GameObject) -> bool { o.keywords.iter().any(|k| k.is_evasion()) }'

authority_fixture
put crates/engine/src/game/combat.rs 'use super::*;' "$A_FROZEN" 'fn other() {}'
commit_base
move crates/engine/src/game/combat.rs crates/engine/src/game/combat/mod.rs
commit rename
run_gate
expect_rc 'authority: whole-file rename of a frozen line passes' 0
printf '%s\n' "$A_PLANTED" >> "$FIXTURE/crates/engine/src/game/combat/mod.rs"
commit planted
run_gate
expect_rc 'authority: rename plus a planted line fails' 1
expect_listed 'authority: only the planted line is listed' "  crates/engine/src/game/combat/mod.rs:
    $A_PLANTED"

# D-3: an excluded authority file is outside the gate's view, so its deletions
# exempt nothing. As a post-filter allowlist the file would be in the view, and
# this move would pass.
authority_fixture
put crates/engine/src/game/keywords.rs 'use super::*;' "$A_FROZEN"
put crates/engine/src/game/combat.rs 'use super::*;'
commit_base
put crates/engine/src/game/keywords.rs 'use super::*;'
put crates/engine/src/game/combat.rs 'use super::*;' "$A_FROZEN"
commit 'move out of the authority'
run_gate
expect_rc 'authority: a line moved out of excluded game/keywords.rs fails' 1
expect_listed 'authority: the line moved out of keywords.rs is listed' "  crates/engine/src/game/combat.rs:
    $A_FROZEN"

# --- check-test-card-data-load.sh -------------------------------------------
C_FROZEN='    let db = CardDatabase::from_export(Path::new("../../client/public/card-data.json"));'
C_PLANTED='    let raw = std::fs::read_to_string("../../client/public/card-data.json");'

new_fixture check-test-card-data-load.sh
put crates/engine/tests/integration/combat.rs 'fn load() {' "$C_FROZEN" '}'
commit_base
move crates/engine/tests/integration/combat.rs crates/engine/tests/integration/combat_damage.rs
commit rename
run_gate
expect_rc 'card-data: whole-file rename of a frozen load passes' 0
put crates/engine/tests/integration/combat_damage.rs 'fn load() {' "$C_FROZEN" '}' 'fn load_raw() {' "$C_PLANTED" '}'
commit planted
run_gate
expect_rc 'card-data: rename plus a planted load fails' 1
expect_listed 'card-data: only the planted load is listed, at its line' "  crates/engine/tests/integration/combat_damage.rs:5:$C_PLANTED"

new_fixture check-test-card-data-load.sh
put crates/engine/tests/integration/support.rs 'fn load() {' "$C_FROZEN" '}'
put crates/engine/tests/integration/combat.rs 'fn other() {}'
commit_base
put crates/engine/tests/integration/support.rs 'fn load() {' '}'
put crates/engine/tests/integration/combat.rs 'fn other() {}' "$C_FROZEN"
commit 'move out of support.rs'
run_gate
expect_rc 'card-data: a load moved out of excluded support.rs fails' 1
expect_listed 'card-data: the load moved out of support.rs is listed' "  crates/engine/tests/integration/combat.rs:2:$C_FROZEN"

# --- check-legacy-quantity-callsites.sh ---------------------------------------
# Its embedded seam check reads five hardcoded seam files and requires each
# named function to exist, so the fixture holds a stub of each.
legacy_fixture() {
    new_fixture check-legacy-quantity-callsites.sh
    put crates/engine/src/parser/oracle_effect/imperative.rs 'fn try_parse_roll_die_with_modifier() {}'
    put crates/engine/src/parser/oracle_effect/lower.rs 'fn parse_dynamic_counter_suffix_body() {}'
    put crates/engine/src/parser/oracle_effect/mod.rs \
        'fn try_parse_gain_energy() {}' 'fn parse_dynamic_energy_unless_cost() {}' 'fn parse_where_x_is() {}'
    put crates/engine/src/parser/oracle_effect/search.rs 'fn parse_highest_mana_value_library_suffix() {}'
    put crates/engine/src/parser/oracle_replacement.rs 'fn parse_enters_with_where_x_suffix() {}'
}
L_FROZEN='fn amount(t: &str) -> Option<QuantityRef> { parse_quantity_ref(t) }'
L_PLANTED='fn each(t: &str) -> Option<QuantityExpr> { parse_for_each_clause(t) }'

legacy_fixture
put crates/engine/src/parser/oracle_cost.rs 'use super::*;' "$L_FROZEN"
commit_base
move crates/engine/src/parser/oracle_cost.rs crates/engine/src/parser/oracle_cost/mod.rs
commit rename
run_gate
expect_rc 'legacy-quantity: whole-file rename of a frozen call passes' 0
printf '%s\n' "$L_PLANTED" >> "$FIXTURE/crates/engine/src/parser/oracle_cost/mod.rs"
commit planted
run_gate
expect_rc 'legacy-quantity: rename plus a planted call fails' 1
expect_listed 'legacy-quantity: only the planted call is listed' "  crates/engine/src/parser/oracle_cost/mod.rs:
    +$L_PLANTED"

printf '\n%d passed, %d failed\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ]
