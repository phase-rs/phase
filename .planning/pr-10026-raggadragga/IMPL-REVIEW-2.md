# IMPL-REVIEW-2 — PR phase-rs/phase#10026 (Raggadragga, Goreguts Boss)

Verdict: FINDINGS (1 LOW)

Scope: `git diff upstream/main...HEAD` at `ecad10b74` on `rebase-implement-pr-10026-fix`
(6 commits: RED tests `60d8da8fd`, plan, plan-review, impl `8ca42621e`,
IMPL-REVIEW-1, fix `ecad10b74`).
Mode: isolated reviewer, no code edits. Round-1 review
(`IMPL-REVIEW-1.md`, 1 MED) re-verified element-by-element below against the
current tree; every claim re-checked by direct reads. Test evidence observed
this session (see bottom).

## Round-1 F1 closure — CLOSED (all five elements verified)

1. Plural accepted at the shared seam. `parse_mana_ability`
   (`crates/phase-parser/src/parser/oracle_nom/filter.rs:199-205`) is now an
   `alt` over `tag("a mana ability")` and `tag("mana abilities")`, both
   lowering to `FilterProp::HasManaAbility`. Both production call sites route
   through it: the folder with-arm
   (`crates/phase-parser/src/parser/oracle_target.rs:4494-4498`) and the
   search loop
   (`crates/phase-parser/src/parser/oracle_effect/search.rs:2626-2630`). No
   other `tag("a mana ability")` producer remains in parser production code;
   the static trailing-strip needle
   (`oracle_static/shared.rs:5559`) is intentionally untouched (different
   whole-subject `TextPair` shape, green with its own test), and a fixture
   census shows no static-form plural card exists — `with mana abilities`
   hits exactly one row across `integration_cards/*.jsonl` (Power Sink's
   effect clause).
2. False comment corrected. `filter.rs:193-198` now states both number forms
   are printed Oracle text with the "has ≥1 mana ability" predicate, and the
   disjoint-heads order note. Repo grep for `rulings prose` over
   `crates/phase-parser/src` returns zero matches. (One stale instance of the
   same sentence survives in `PLAN.md` — raised as the LOW finding below;
   the code half of F1 is fully closed.)
3. Power Sink pins HasManaAbility. New production-path test
   `power_sink_plural_mana_abilities_keeps_qualifier`
   (`crates/phase-parser/src/parser/oracle_effect/tests.rs:11063-11106`)
   parses Power Sink's verbatim Oracle text (byte-identical to the `power
   sink` fixture row modulo Rust line-continuation whitespace) through
   `parse_card`, finds the `SetTapState{All,Tap}` def (panic reach-guard if
   the shape is absent), and asserts the tap target stays `Land` AND carries
   `HasManaAbility`. Discriminating: pre-fix the target parses to
   `Typed(Land, null, [])`, failing the property assert. Observed green.
4. No silent drop. The qualifier is consumed into `properties` at the shared
   seam instead of falling through unclaimed; the full `phase-parser` suite
   stays green (13207 passed, 0 failed), so no adjacent arm's language
   changed.
5. Trailing-controller question resolved — attaches, no folder-order gap. The
   with-arm (`oracle_target.rs:4490-4504`) runs BEFORE the late
   ownership/controller pass (`:4533-4544`), so after consuming "with mana
   abilities" the remaining " they control" reaches `parse_controller_suffix`,
   which admits present-tense anaphoric "they control" (`:7228-7231`) and
   always binds (antecedent binder, else the interim You seed `:7281-7286`)
   — full consumption, rest empty. This corroborates the fix commit's probe
   claim by two independent routes: the pass-ordering trace plus pre-existing
   pins of the identical controller-phrase class (the "target commander they
   control" test `:15075-15084` and the seed test `:19812-19828`). Which
   `ControllerRef` binds (You vs TargetPlayer) depends on ambient antecedents
   — controller-seam-owned pre-existing behavior, out of scope per the R1
   scoping note. The production test's silence on controller is correct
   scoping, not a gap.

## Findings

**[LOW]** PLAN.md's Nom Compliance section still asserts the refuted "plural appears only in rulings prose" claim, contradicting the file's own F1 CORRECTION note. Evidence: `.planning/pr-10026-raggadragga/PLAN.md:212-214` ("Singular only: the qualifier position is singular in every attested Oracle printing …; plural `mana abilities` appears only in rulings prose") vs `:85-90` (CORRECTION: plural is printed Oracle text — Power Sink). Why it matters: the same file states opposite facts about the accepted language, and the HEAD commit message claims the PLAN.md correction complete while this second instance of the false sentence (the census at Step 0.5 was corrected; this rationale paragraph was missed) still stands. Suggested fix: append a supersession pointer to the Nom Compliance sentence (or reword it to both number forms) matching the Step 0.5 CORRECTION.

## Pre-existing

None. (PLAN-REVIEW-1 P1/P2 — cycling/exhaust qualifiers — were listed there
and per the skill are not repeated.)

## CR verification (grep-checked against `docs/MagicCompRules.txt` in this review; the file is present in this tree)

- CR 605.1 (:2687, "Some activated abilities and some triggered abilities are mana abilities …") — describes `parse_mana_ability` and both call-site arms, both number forms. OK. Corroborated by Power Sink's 2010-03-01 ruling ("Only lands that actually have mana abilities will get tapped").
- CR 113.1 + CR 113.3 (:740, :759) — retained pre-existing cite on the extended arm for the "no abilities" leg. Not flagged.
- CR 508.1 (:2267) + CR 603.2 (:2567) — attack-trigger subject filter in tests. OK.
- CR 603.4 (:2598, intervening-if) — clause-3 condition pin + six-mana silence test. OK.
- CR 601.2h (:2478, "The player pays the total cost …") — mana-spent payment record read. OK.
- CR 613.4c (:2996, layer 7c P/T modify) — anthem pump. OK.

## Lenses applied, no further findings

Correct seam (plural handled in the shared bare-predicate combinator, not a
second arm); idiomatic nom (`alt`/`value`/`tag`/`preceded`/`space1`, accurate
disjoint-heads comments); no card-specific hacks ("Raggadragga"/"Power Sink"
in production code only in comments); no new enum variant
(`HasManaAbility` reused, eval pre-existing); `search.rs` re-delegation now
reduces duplication (single spelling authority; plural widening there is
harmless — no printed search-clause card uses it); static trailing-strip
untouched, justified by the single-hit plural census; new tests all
production-entry with reach-guards (Power Sink `parse_card` + panic-on-absent
shape; six-mana `GameRunner` cast with spell-resolved + anthem-P/T guards
mirroring the cheap-spell negative; fold plural test fails on revert since
neither pre-fix leg matches); negative assertions paired (`with flying`
guard asserts the keyword prop AND empty rest; keyword non-shadow holds);
pre-existing Power Sink table entry (`tests.rs:11009-11024`) unaffected (uses
`..` matcher, asserts continuation-condition only — no duplication with the
new qualifier pin); verbatim Oracle text throughout (Power Sink joins
byte-exact via line continuation); stale `p.jsonl` row honest-stale
(`confirmed_misparse: dropped_qualifier` under-claims until the next
oracle-gen, same accepted posture as `r.jsonl`); `class-fixtures.json` is an
honest record of the singular census run, now annotated by the PLAN
CORRECTION — not raised; no serialized-surface change; `shard_4.rs`
registration trivial. fmt/clippy/coverage-script checks skipped per skill
(CI-enforced).

## Test evidence observed this session

- `cargo test -p phase-parser --lib` → 13207 passed, 0 failed, 3 ignored (49.75s), EXIT=0.
- `cargo test -p phase-engine --test integration_4 raggadragga` → 5 passed (incl. new six-mana boundary), EXIT=0.
- Focused: `mana_ability` filter → 29 passed; `power_sink` → 1 passed; `raggadragga` lib filter → 2 passed.
