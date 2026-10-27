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
       "$REPO_ROOT/scripts/lib/detect_cross_product_alts_tests.py" \
       "$REPO_ROOT/scripts/lib/added-lines.sh" \
       "$REPO_ROOT/scripts/lib/added-lines.py" \
       "$REPO_ROOT/scripts/lib/added_lines_tests.py" "$FIXTURE/scripts/lib/"
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
    git -C "$FIXTURE" config commit.gpgsign false
    git -C "$FIXTURE" config tag.gpgsign false
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
OUT="$(git -C "$FIXTURE" status --porcelain)"
if [ -z "$OUT" ]; then ok 'CI fixture starts from a clean checkout'; else fail 'CI fixture must be clean'; fi
if git -C "$FIXTURE" merge-base --is-ancestor "$BASE" HEAD; then
    ok 'CI base ref is an ancestor of fixture HEAD'
else
    fail 'CI fixture base must be an ancestor'
fi
run_gate
expect_rc 'baseline ceiling passes' 0
expect_out 'real worktree snapshot is named' 'Gate H PASS (residue: 1 legacy obligations, ceiling 1; files scanned 2; snapshot worktree)'
printf 'CI clean-checkout gate: exit %s\n%s\n' "$RC" "$OUT"
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

# Explicit closure return types must not hide a helper-result discard.
new_fixture
printf "fn added() { let (filter, _) = ctx.with_player_antecedents(|s| -> (TargetFilter, &'a str) { parse_target_with_ctx(input, s) },); }\n" >> "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'annotated closure discard fails' 1
expect_out 'annotated closure growth names file and line' 'crates/engine/src/parser/probe.rs:2: binding parse_target_with_ctx'
expect_out 'annotated closure growth exceeds ceiling' 'actual 2, ceiling 1'
printf 'SMOKE annotated closure discard: exit %s\n%s\n' "$RC" "$OUT"
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'removing annotated closure discard passes' 0
expect_out 'annotated closure removal restores ceiling' 'residue: 1 legacy obligations, ceiling 1'
printf 'SMOKE removed annotated closure discard: exit %s\n%s\n' "$RC" "$OUT"

# Compound receivers must not hide a helper-result discard.
new_fixture
printf 'fn added() { let (filter, _) = contexts[0].with_player_antecedents(|s| parse_target_with_ctx(input, s)); }\n' >> "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'indexed receiver discard fails' 1
expect_out 'indexed receiver growth names file and line' 'crates/engine/src/parser/probe.rs:2: binding parse_target_with_ctx'
expect_out 'indexed receiver growth exceeds ceiling' 'actual 2, ceiling 1'
printf 'SMOKE indexed receiver discard: exit %s\n%s\n' "$RC" "$OUT"
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'removing indexed receiver discard passes' 0
expect_out 'indexed receiver removal restores ceiling' 'residue: 1 legacy obligations, ceiling 1'
printf 'SMOKE removed indexed receiver discard: exit %s\n%s\n' "$RC" "$OUT"

# Compound receiver projections discard the helper's remainder too.
new_fixture
printf 'fn added() { let f = contexts[0].with_player_antecedents(|s| parse_target_with_ctx(input, s)).0; }\n' >> "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'indexed receiver projection fails' 1
expect_out 'indexed receiver projection names file and line' 'crates/engine/src/parser/probe.rs:2: projection parse_target_with_ctx'
expect_out 'indexed receiver projection exceeds ceiling' 'actual 2, ceiling 1'
printf 'SMOKE indexed receiver projection: exit %s\n%s\n' "$RC" "$OUT"
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'removing indexed receiver projection passes' 0
expect_out 'indexed receiver projection removal restores ceiling' 'residue: 1 legacy obligations, ceiling 1'
printf 'SMOKE removed indexed receiver projection: exit %s\n%s\n' "$RC" "$OUT"

# Generic wrapper and receiver calls must retain helper-result discard origins.
new_fixture
printf 'fn added() { let (f, _) = Self::get_context::<Vec<(A, B)>>().with_player_scope::<fn() -> T>(scope, |s| parse_target_with_ctx(input, s)); }\n' >> "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'turbofish wrapper discard fails' 1
expect_out 'turbofish wrapper growth names file and line' 'crates/engine/src/parser/probe.rs:2: binding parse_target_with_ctx'
expect_out 'turbofish wrapper growth exceeds ceiling' 'actual 2, ceiling 1'
printf 'SMOKE turbofish wrapper discard: exit %s\n%s\n' "$RC" "$OUT"
printf 'fn f() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'removing turbofish wrapper discard passes' 0
expect_out 'turbofish wrapper removal restores ceiling' 'residue: 1 legacy obligations, ceiling 1'
printf 'SMOKE removed turbofish wrapper discard: exit %s\n%s\n' "$RC" "$OUT"

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
printf 'PRE-COMMIT staged source and ledger gate: exit %s\n%s\n' "$RC" "$OUT"
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

# Precise-capture `use<'a>` bounds are valid Rust, not import items. Exercise
# source classification and module discovery through both real snapshot paths.
new_fixture
cat > "$FIXTURE/crates/engine/src/parser/capture.rs" <<'EOF'
fn keep<'a>(i: &'a str) -> impl Fn() -> &'a str + use<'a> { move || i }
EOF
printf 'fn consumed(i: &str) { let (v, rest) = parse_target(i); consume(rest); }\n' >> "$FIXTURE/crates/engine/src/parser/probe.rs"
run_gate
expect_rc 'precise capture and consumed remainder pass worktree gate' 0
expect_out 'precise capture worktree preserves witness' 'residue: 1 legacy obligations, ceiling 1'
printf 'PRECISE-CAPTURE worktree gate: exit %s\n%s\n' "$RC" "$OUT"
git -C "$FIXTURE" add crates/engine/src/parser/probe.rs crates/engine/src/parser/capture.rs
run_cached
expect_rc 'precise capture and consumed remainder pass index gate' 0
expect_out 'precise capture index preserves witness' 'snapshot index)'
printf 'PRECISE-CAPTURE staged-index gate: exit %s\n%s\n' "$RC" "$OUT"
add_discard
git -C "$FIXTURE" add crates/engine/src/parser/probe.rs
run_cached
expect_rc 'precise capture cannot hide an indexed discard' 1
expect_out 'precise capture indexed growth names source' 'probe.rs:3: binding parse_target_with_ctx'

# Refutable let-else, or-patterns and rustfmt trailing commas must grow the real
# H census in both snapshots; consumed/unrelated replacements remove the growth.
for form in let-else or-pattern trailing-comma-drop qualified-drop any-default selfref-default wrapped-pattern whole-binding whole-match; do
    new_fixture
    case "$form" in
        whole-binding)
            discarded='fn probe(input: &str) { let ref mut _r: (TargetFilter, &str) = parse_target(input); }'
            consumed='fn probe(input: &str) { let ref mut r: (TargetFilter, &str) = parse_target(input); consume(r); }'
            actual=2
            ;;
        whole-match)
            discarded='fn probe(input: &str) { match parse_target(input) { _unused | _unused if keep() => {}, } }'
            consumed='fn probe(input: &str) { match parse_target(input) { result | result if keep() => { consume(result); }, } }'
            actual=3
            ;;
        let-else)
            discarded='fn probe(input: &str) -> Option<TargetFilter> { let (TargetFilter::Typed(typed), _) = parse_target(input) else { return None; }; Some(TargetFilter::Typed(typed)) }'
            consumed='fn probe(input: &str) -> Option<TargetFilter> { let (TargetFilter::Typed(typed), rest) = parse_target(input) else { return None; }; consume(rest); Some(TargetFilter::Typed(typed)) }'
            actual=2
            ;;
        or-pattern)
            discarded='fn probe(input: &str) { match parse_target(input) { (TargetFilter::Any, _) | (TargetFilter::SelfRef, _) => None, (filter, rest) => rest.is_empty().then_some(filter), } }'
            consumed='fn probe(input: &str) { match parse_target(input) { (TargetFilter::Any, rest) | (TargetFilter::SelfRef, rest) => consume(rest), (filter, rest) => rest.is_empty().then_some(filter), } }'
            actual=3
            ;;
        trailing-comma-drop|qualified-drop)
            drop_name=drop
            if [ "$form" = qualified-drop ]; then drop_name=std::mem::drop; fi
            discarded="fn probe(input: &str) {
    $drop_name(
        parse_target(
            input,
        ),
    );
}"
            consumed="fn probe(input: &str) {
    $drop_name(
        consume(parse_target(input)),
    );
}"
            actual=2
            ;;
        any-default|selfref-default)
            variant=Any
            if [ "$form" = selfref-default ]; then variant=SelfRef; fi
            discarded="fn probe() {
    consume(Some(
        producer().unwrap_or(
            TargetFilter::$variant,
        ),
    ));
}"
            consumed="fn probe() {
    consume(Some(
        producer().unwrap_or(
            Other::$variant,
        ),
    ));
}"
            actual=2
            ;;
        wrapped-pattern)
            discarded='fn probe(input: &str) {
    match Some(
        parse_target(
            input,
        ),
    ) {
        Some((filter, _),) => use_it(filter),
        None => (),
    }
}'
            consumed='fn probe(input: &str) {
    match Some(
        parse_target(
            input,
        ),
    ) {
        Some((filter, rest),) => consume(rest),
        None => (),
    }
}'
            actual=2
            ;;
    esac
    printf '%s\n' "$discarded" >> "$FIXTURE/crates/engine/src/parser/probe.rs"
    run_gate
    expect_rc "$form introduced discard fails worktree H" 1
    expect_out "$form worktree growth is counted" "actual $actual, ceiling 1"
    printf 'SMOKE %s worktree added: exit %s\n%s\n' "$form" "$RC" "$OUT"
    printf 'fn f() { let (v, _) = parse_target(input); }\n%s\n' "$consumed" > "$FIXTURE/crates/engine/src/parser/probe.rs"
    run_gate
    expect_rc "$form removing discard passes worktree H" 0
    expect_out "$form consumed worktree returns to baseline" 'residue: 1 legacy obligations, ceiling 1'
    printf 'SMOKE %s worktree removed: exit %s\n%s\n' "$form" "$RC" "$OUT"
    printf 'fn f() { let (v, _) = parse_target(input); }\n%s\n' "$discarded" > "$FIXTURE/crates/engine/src/parser/probe.rs"
    git -C "$FIXTURE" add crates/engine/src/parser/probe.rs
    printf 'fn f() { let (v, _) = parse_target(input); }\n%s\n' "$consumed" > "$FIXTURE/crates/engine/src/parser/probe.rs"
    run_cached
    expect_rc "$form indexed discard fails despite consumed worktree" 1
    expect_out "$form index growth is counted" "actual $actual, ceiling 1"
    printf 'SMOKE %s staged-index added: exit %s\n%s\n' "$form" "$RC" "$OUT"
    git -C "$FIXTURE" add crates/engine/src/parser/probe.rs
    run_cached
    expect_rc "$form removing indexed discard passes H" 0
    expect_out "$form consumed index returns to baseline" 'residue: 1 legacy obligations, ceiling 1'
    expect_out "$form consumed snapshot is index" 'snapshot index)'
    printf 'SMOKE %s staged-index removed: exit %s\n%s\n' "$form" "$RC" "$OUT"
done

# Production names containing `tests` remain scanned in both snapshots; exact
# test filenames and a test-only outlined child remain negative controls.
for name in contests.rs latest_tests_helper.rs; do
    new_fixture
    printf '#[cfg(test)]\nmod support;\n' > "$FIXTURE/crates/engine/src/parser/mod.rs"
    for excluded in tests.rs foo_tests.rs support.rs; do
        printf 'fn hidden() { let (v, _) = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/$excluded"
    done
    consumed='fn f() { let (v, rest) = parse_target(input); consume(rest); }'
    discarded='fn f() { let (v, _) = parse_target(input); }'
    printf '%s\n' "$consumed" > "$FIXTURE/crates/engine/src/parser/$name"
    run_gate
    expect_rc "$name consumed result and test-file controls pass worktree" 0
    printf '%s\n' "$discarded" > "$FIXTURE/crates/engine/src/parser/$name"
    run_gate
    expect_rc "$name production discard fails worktree" 1
    expect_out "$name unledgered production growth is named" "$name: actual 1, ceiling 0 (no ledger entry)"
    printf 'SMOKE %s worktree added: exit %s\n%s\n' "$name" "$RC" "$OUT"
    git -C "$FIXTURE" add crates/engine/src/parser
    printf '%s\n' "$consumed" > "$FIXTURE/crates/engine/src/parser/$name"
    run_cached
    expect_rc "$name indexed discard fails despite consumed worktree" 1
    expect_out "$name indexed production growth is named" "$name: actual 1, ceiling 0 (no ledger entry)"
    printf 'SMOKE %s staged-index added: exit %s\n%s\n' "$name" "$RC" "$OUT"
    run_gate
    expect_rc "$name removing discard passes worktree" 0
    git -C "$FIXTURE" add "crates/engine/src/parser/$name"
    run_cached
    expect_rc "$name removing indexed discard passes" 0
    expect_out "$name consumed index returns to baseline" 'residue: 1 legacy obligations, ceiling 1'
    expect_out "$name consumed snapshot is index" 'snapshot index)'
    printf 'SMOKE %s staged-index removed: exit %s\n%s\n' "$name" "$RC" "$OUT"
done

# A child outlined under an exact test filename (tests/support.rs) is test-only
# in both snapshots; a production #[path] reference to it brings it back.
for owner in tests unit_tests; do
    new_fixture
    printf '#[cfg(test)]\nmod %s;\n' "$owner" > "$FIXTURE/crates/engine/src/parser/mod.rs"
    printf 'mod support;\n' > "$FIXTURE/crates/engine/src/parser/$owner.rs"
    mkdir -p "$FIXTURE/crates/engine/src/parser/$owner"
    printf 'fn hidden() { let _parsed = parse_target(input); }\n' > "$FIXTURE/crates/engine/src/parser/$owner/support.rs"
    run_gate
    expect_rc "$owner child stays test-only in worktree" 0
    expect_out "$owner child is not counted in worktree" 'residue: 1 legacy obligations, ceiling 1'
    git -C "$FIXTURE" add crates/engine/src/parser
    run_cached
    expect_rc "$owner child stays test-only in index" 0
    expect_out "$owner child is not counted in index" 'residue: 1 legacy obligations, ceiling 1'
    printf '#[cfg(test)]\nmod %s;\n#[path = "%s/support.rs"]\nmod shared;\n' "$owner" "$owner" > "$FIXTURE/crates/engine/src/parser/mod.rs"
    run_gate
    expect_rc "$owner child reached by production path counts" 1
    expect_out "$owner production path names the child" "$owner/support.rs: actual 1, ceiling 0 (no ledger entry)"
done

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
