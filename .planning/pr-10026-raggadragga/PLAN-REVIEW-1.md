# PLAN-REVIEW-1 — Raggadragga (#10026) plan review (independent, review-engine-plan)

Verdict: FINDINGS — 2 text-only findings, each with replacement text below.
Per the skill's review loop, a round whose gaps are all wording fixes with
replacements is closed by applying them, with no further review. No behavior
or machinery findings. No blocking gaps.

Scope reviewed: `.planning/pr-10026-raggadragga/PLAN.md` (commit 9d0c7cbbf)
plus `class-fixtures.json`. All 12 required checks applied; every load-bearing
code citation re-read in the tree (not grep-only). No code edited.

## What verified clean (evidence, not trust)

- Root cause: `parse_type_phrase_folding_unbound` suffix scan has arms for
  mana-value (:4406), base-P/T (:4420), power (:4427), superlative (:4457),
  counters (:4477), "with no abilities" (:4482–4494, read verbatim), keywords
  (:4496) — and no "with a mana ability" arm. Confirmed.
- Failure chain: `parse_single_subject` "a "/"an " arm (:15851, exact) calls
  `parse_type_phrase_folding_with_ctx`; Attacks arm (:17178, exact) requires
  rest to start with `attacks`/`attack` (guarded against `attacker`/
  `attacking`), so rest ` with a mana ability attacks, ...` falls through to
  `TriggerMode::Unknown`. Production proof: `r.jsonl` Raggadragga row carries
  `{"Unknown":"Whenever a creature you control with a mana ability attacks"}`.
- Runtime chain needs no edits: `attack_declaration_attacker_matches`
  (`trigger_matchers.rs:2715`, consults `valid_card` when present) →
  `valid_card_matches` → `FilterProp::HasManaAbility => obj.abilities.iter()
  .any(is_mana_ability)` (`filter.rs:11692`, exact). Confirmed by read.
- Clause-3 not-a-defect: fixture row shows
  `ManaSpentToCast{TriggeringSpell,Total} GE 7` with full
  untap + UntilEndOfTurn +7/+7/trample lowering. Confirmed.
- Oracle text in plan matches fixture verbatim (all 3 clauses + all 3 rulings).
- Class census reproduced: `with a mana ability` in `oracle_text` over
  `integration_cards/*.jsonl` hits only Raggadragga; Chandra's Regulator and
  Spiders-Man hits are rulings prose (verified by reading both rows).
- Nom compliance: exact combinators specified
  (`value`/`tag`/`alt`/`preceded`/`space1`), mirroring `parse_no_abilities`
  (`filter.rs:184`, read). `alt` and `nom_filter` imports already exist in
  `oracle_target.rs` (:1, :38). No `contains`/`starts_with` dispatch. The
  two bare predicates have disjoint heads (`a ...` vs `no ...`).
- Precedence audit holds: mana-value arms all require a `mana value` token
  (`:8431+`, read — including the `with a/an` parity arm, still keyed on
  `mana value`); power needs P/T comparators; superlative needs a
  superlative head; counter needs `counter`. None can claim `with a mana
  ability`. Full-suite row + mutation check backstop this.
- Mirror tests exist: `no_abilities_suffix_plural/singular`
  (`oracle_target.rs:16073-16098`, read) for row 4;
  `test_parse_no_abilities*` trio (`filter.rs:993-1012`, read) for row 3.
- Stale-fixture claim holds: only `r.jsonl` itself mentions the
  `raggadragga, goreguts boss` key; RED tests parse Oracle text at runtime
  (`add_creature_from_oracle`); nothing pins the stale `Unknown` shape.
- Check 8 skills: `/oracle-parser`, `/add-trigger`, `/card-test` applied;
  `/add-engine-variant` correctly dismissed (no new variant — `HasManaAbility`
  exists with full eval coverage); inapplicable skills explicitly excluded.
- Checks 4/5/10/11/12: parser-only placement, no new types, no new identity
  binding (`TriggeringSource` pre-existing), adjacent-sibling negatives
  present (rows 4+6), Sizing section present and consistent (1 unit).
- Check 9 matrix: rows 1–7 each name seam, production entry, test,
  revert-fail, and negative/hostile; rows 1–2 are committed RED with paired
  positive reach-guards (clause-3 shape test; vanilla attacker + Raggadragga
  self non-match via the (4,4) anthem assertion). Row 7 keeps coverage
  honest (`Unknown` 1→0, no new `Effect::unimplemented`).
- Check 9a: the anthem preservation value
  `Typed(Creature, You, [HasManaAbility])` re-derived from verbatim clause 1
  + CR 605.1 (live ability set) matches the fixture's `static_abilities`;
  committed tests assert runtime behavior, not fixture parity. No
  defective-reference risk.

## Finding F1 [text]: wrong precedent call-site count

Location: PLAN.md §"Extension vs Creation": "precedent:
`parse_no_abilities` with three call sites".

Evidence: repo-wide search for `parse_no_abilities` finds exactly TWO
production call sites — `oracle_target.rs:4489` (direct call) and
`search.rs:2632` (as a `preceded` function item) — plus the definition,
its unit tests, and one comment mention (`layers.rs:21412`, not a call).
The static trailing-strip (`shared.rs:5559-5560`) uses inline tag needles,
not the combinator. (This correction strengthens the plan: the fix mirrors
both real call sites, so coverage is complete, not 2-of-3.)

Replacement — old:

> Pure extension of the established "bare predicate in `oracle_nom/filter.rs` +
> call-site grammar" pattern (precedent: `parse_no_abilities` with three call
> sites).

Replacement — new:

> Pure extension of the established "bare predicate in `oracle_nom/filter.rs` +
> call-site grammar" pattern (precedent: `parse_no_abilities` with two
> production call sites — `oracle_target.rs:4489` and `search.rs:2632`; the
> static trailing-strip uses inline tag needles, not the combinator).

## Finding F2 [text]: sibling-qualifier justification overstates working precedent

Locations: PLAN.md §0.5 ("Sibling ability-qualifier phrases proving the
general pattern: ... `with a cycling ability` → Spellpyre Phoenix; `with an
exhaust ability` → Sala, Deck Boss") and §Pattern Coverage (class framed as
the qualifier family sharing the seam).

Evidence (all read directly):

- Spellpyre Phoenix fixture row: the `with a cycling ability` target lowers
  to `Typed` filters carrying ONLY `InZone(Graveyard)` + `Owned(You)` — the
  qualifier is dropped, with no `Unimplemented` marker in the row. No
  `cycling` handling exists in `oracle_target.rs` (zero hits).
- Sala, Deck Boss fixture row: `Each creature you control with an exhaust
  ability has haste` lowers to `"static_abilities":[]` — the clause is
  entirely unsupported.
- No `HasCycling`/`HasExhaust` (or equivalent) `FilterProp` exists
  (`events.rs` `Has*` variants enumerated: `HasManaAbility`,
  `HasNoAbilities`, `HasAdventure`, `HasXInManaCost`,
  `HasXInActivationCost`, `HasHasteOrControlledSinceTurnBegan`,
  `HasSingleTarget`, ...), so neither qualifier CAN route through the seam
  today. Only `with no abilities` is a working precedent through the
  identical seam (same arm, same combinator pattern, same two call sites).

This does not narrow the fix: the `with a mana ability` arm itself remains
class-shaped (every type-phrase consumer — trigger subjects, targets, union
folding — plus search), with Raggadragga as the validating consumer of a
reusable building block. Check 1's gate is satisfied by the `with no
abilities` precedent + the structural all-consumers argument. But the plan
must not present cycling/exhaust as proof the seam handles the family, and
must acknowledge them as out-of-scope pre-existing gaps rather than
supporting evidence.

Replacement — old (§0.5):

> - Sibling ability-qualifier phrases proving the general pattern:
>   `with no abilities` → Jasmine Boreal of the Seven, Muraganda Petroglyphs,
>   Ruxa, Patient Professor; `with a cycling ability` → Spellpyre Phoenix;
>   `with an exhaust ability` → Sala, Deck Boss.
>
> The fix is still class-shaped, not card-shaped: it completes the shared
> type-phrase "with X" qualifier authority (which already owns power, mana value,
> counters, keywords, and "no abilities") so EVERY current and future
> "with a mana ability" subject — trigger subjects, targets, search filters —
> parses through one grammar. The skill's "if the answer is 1, stop" gate is
> satisfied by the sibling qualifier class sharing the identical seam.

Replacement — new (§0.5):

> - Working sibling precedent through the identical seam: `with no abilities`
>   → Jasmine Boreal of the Seven, Muraganda Petroglyphs, Ruxa, Patient
>   Professor (same arm, same bare-predicate combinator pattern, same two
>   call sites).
> - Future class members, NOT working precedent (out of scope, pre-existing
>   gaps): `with a cycling ability` (Spellpyre Phoenix — qualifier currently
>   dropped from the target) and `with an exhaust ability` (Sala, Deck Boss —
>   anthem clause currently unsupported); neither has a `FilterProp`, so each
>   needs its own variant + eval before it can use this seam.
>
> The fix is still class-shaped, not card-shaped: it completes the shared
> type-phrase "with X" qualifier authority (which already owns power, mana value,
> counters, keywords, and "no abilities") so EVERY current and future
> "with a mana ability" subject — trigger subjects, targets, search filters —
> parses through one grammar. The skill's "if the answer is 1, stop" gate is
> satisfied by the `with no abilities` precedent sharing the identical seam
> plus the all-consumers structural argument.

Also apply the same narrowing to §Pattern Coverage's first sentence if it
names cycling/exhaust as covered (it names only 605.1/113.1 — no change
needed there beyond keeping F1's count fix in mind; reviewer checked).

## Pre-existing (untagged, non-blocking, found while checking the plan's claims)

- P1: `with a cycling ability` target qualifier silently dropped
  (Spellpyre Phoenix fixture: no prop, no `Unimplemented` marker).
- P2: `with an exhaust ability` static clause unsupported
  (Sala fixture: `"static_abilities":[]`).
- Both need their own `FilterProp` + eval + parser wiring; explicitly out of
  this plan's scope. Listed once here per the skill; not repeated later.

## Residual assumptions / non-blocking notes

- N1 (check 7): CR numbers (605.1/113.1/113.3/508.1/603.2/603.4) NOT
  grep-verified — `docs/MagicCompRules.txt` is gitignored and absent
  (confirmed: only `docs/MagicCompRules.edition` exists). The plan is honest
  about this and instructs the implementer to re-grep after fetching the
  pinned edition. Citations match pervasive adjacent usage (CR 605.1 cited
  50+ times in `crates/phase-parser/src/parser`, verified by search;
  `filter.rs:11690` and the adjacent arm's `CR 113.1 + CR 113.3` comments are
  the direct precedents), so residual risk is renumbering-only. Implementer
  MUST run the re-grep before commit; no plan change needed.
- N2: `search.rs` re-delegation accepts a trivially wider whitespace language
  (`space1` vs the current single-space literal tag) — harmonization with the
  adjacent no-abilities arm, negligible; the plan's "same accepted language"
  holds modulo whitespace normalization. No change needed.
- N3 (check 7a): the mutation-check scratch test uses synthetic shapes
  (`creature with a mana ability`, `artifacts with a mana ability`) as a
  throwaway generality probe, deleted after — acceptable; no committed
  synthetic card. No change needed.
- N4: line citations spot-checked exact (`:15851`, `:17178`, `:4482-4494`,
  `:184`, `:993-1012`, `:2623`, `:2715`, `:11692`, `:5547-5560`) — no drift
  at review time.
