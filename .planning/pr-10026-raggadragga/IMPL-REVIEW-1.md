# IMPL-REVIEW-1 — PR phase-rs/phase#10026 (Raggadragga, Goreguts Boss)

Verdict: FINDINGS (1 MED)

Scope: `git diff upstream/main...HEAD` at `8ca42621e` on `rebase-implement-pr-10026-fix`
(4 commits: RED tests `60d8da8fd`, plan, plan-review, impl `8ca42621e`).
Mode: isolated reviewer, no code edits. Prior results 1 (implementation) and 2 (proof)
read fully; every claim below re-verified against the current tree.

## Findings

**[MED]** New "with a mana ability" arm is singular-only; the plural "with mana abilities" is printed Oracle text (Power Sink), flows through the same changed seam, and is still silently dropped — while the new code comment claims the plural exists only in rulings prose. Evidence: `crates/phase-parser/src/parser/oracle_nom/filter.rs:193-198` (`tag("a mana ability")` plus "plural `mana abilities` appears only in rulings prose"); `crates/phase-parser/src/parser/oracle_target.rs:4493-4497` (alt over the two singular-headed predicates); Power Sink, whose Oracle text is "Counter target spell unless its controller pays {X}. If that player doesn't, they tap all lands with mana abilities they control and lose all unspent mana." (`crates/engine/tests/fixtures/integration_cards/p.jsonl`, key `power sink`), parses to a `SetTapState` target of `Typed(Land, controller null, properties [])` — the qualifier is silently dropped with no `Unimplemented` marker; call chain `TapAll` (`oracle_effect/mod.rs:24237-24242`) → `parse_target_with_ctx` (`oracle_target.rs:898`) → folder fallback (`oracle_target.rs:2489`) → `parse_type_phrase_folding_unbound` (`oracle_target.rs:3464`) → with-arm (`oracle_target.rs:4482`): every `parse_target` arm before the fallback (lines 1047–2489, all special forms — anaphors, pronouns, possessives, zone refs) was checked and none matches a bare "lands …" phrase, so the plural reaches the new arm and fails both alt legs. CR 605.1 (`docs/MagicCompRules.txt:2687`: "Some activated abilities and some triggered abilities are mana abilities …") governs both number forms — the predicate is "has ≥1 mana ability" regardless of subject number. Why it matters: the parser lens requires plural-variant verification for every new parser arm, and the plan's fixture census grepped only the singular string, so the shared "spelling authority" covers one of two printed number-forms at its own seam while its comment hides the gap. Suggested fix: accept the plural head in the shared combinator/arm, add a fold-level test plus a production-path test pinning `HasManaAbility` on Power Sink's tap target (assert the property, not just scope+Tap as the current test does), and correct the false sentence in `filter.rs` and the matching census claim in `.planning/pr-10026-raggadragga/PLAN.md`. Scoping note for the fixer: after the plural fix, verify whether trailing "they control" attaches in the folder's linear scan; if controller scoping still dangles, record that as a separate folder-order gap — this finding closes on the qualifier (`HasManaAbility` attached, no silent drop, comment corrected).

## Pre-existing

None. (Power Sink's dropped qualifier is the witness for F1, not a separate entry.)

## CR verification (grep-checked against `docs/MagicCompRules.txt` in this review)

- CR 605.1 (:2687, "Some activated abilities and some triggered abilities are mana abilities …") — describes `parse_mana_ability` and both call-site arms. OK.
- CR 113.1 + CR 113.3 (:740, :759) — retained pre-existing cite on the extended arm for the "no abilities" leg; 113.3 (four ability categories) is the apt half, 113.1 the pre-existing convention (also in `oracle_static/shared.rs:5547`). Not flagged.
- CR 508.1 (:2267, declare attackers) + CR 603.2 (:2567, trigger matching) — describe the attack-trigger subject filter in tests. OK.
- CR 603.4 (:2598, intervening-if) — describes the clause-3 condition pin. OK.
- CR 601.2h (:2478, "The player pays the total cost …") — describes the mana-spent payment record read. OK.
- CR 613.4c (:2996, layer 7c modify P/T) — describes the anthem pump. OK.

## Lenses applied, no further findings

Correct seam (shared type-phrase "with X" authority, no trigger-local strip); idiomatic nom (`value`/`tag`/`alt`/`preceded`/`space1`, disjoint-head order comment); no card-specific hacks ("Raggadragga" in production code only in comments); no new enum variant (`HasManaAbility` reused, eval pre-existing); `search.rs` re-delegation behavior-preserving (existing test pins it; `space1` only widens whitespace); static trailing-strip intentionally untouched (different `TextPair` whole-subject shape, green with its own test — accepted, not duplication); tests all production-entry with paired reach-guards and `assert_no_unimplemented` on trigger executes (mode asserted before props; revert-fail already demonstrated by the proof step); real printed cards with verbatim Oracle text throughout (Raggadragga, Llanowar Elves, Pelakka Wurm, Grizzly Bears); stale `r.jsonl` Unknown row confirmed unpinned (only the fixture itself references the key) and honest-stale (under-claims, needs only the next oracle-gen); no serialized-surface change; `shard_4.rs` registration trivial. fmt/clippy/coverage-script checks skipped per skill (CI-enforced).
