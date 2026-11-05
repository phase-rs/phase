# Where-X player antecedent provenance (#6508 and its class)

**As of `154167c12`** (`upstream/main`, 2026-11-04). Measured, not traced — every
claim below carries the file:line it was read from or the probe that produced it.

Supersedes the approach in closed PR
[#8896](https://github.com/phase-rs/phase/pull/8896), whose review correctly
rejected it. That PR hard-coded `ControllerRef::ScopedPlayer` for every where-X
count; this document records why that is wrong for three of the five cards in the
class and what the correct seam is.

## The defect

A `", where X is …"` clause whose count carries a **controller anaphor**
("they control", "that player controls") is lowered with **no player antecedent in
scope**, so the count silently binds to the caster.

Measured by parsing each card's verbatim Scryfall Oracle text through
`parser::oracle::parse_oracle_text` and serializing the whole `ParsedAbilities`:

| Card | Antecedent | Correct | Lowered to |
|---|---|---|---|
| Citadel of Pain | `each player's end step` trigger | `ScopedPlayer` | `You` ❌ |
| Psychic Allergy | `each opponent's upkeep` trigger | `ScopedPlayer` | *(honest `where_x_binding` gap — chosen-color filter)* |
| Curious Herd | `Choose target opponent.` | `TargetPlayer` | `You` ❌ |
| Jovial Evil | `target opponent` | `TargetPlayer` | `You` ❌ |
| Pact of the Serpent | `Target player` | `TargetPlayer` | `You` ❌ (both the draw and the life-loss slot) |

Jovial Evil is the clearest witness: the damage `target` lowers to
`controller: Opponent` while the amount's `ObjectCount` filter lowers to
`controller: You`, in one effect. The spell counts the caster's white creatures
and damages the opponent.

**Three antecedent classes, not two.** The table's "Antecedent" column was the
starting hypothesis; measurement split the declared-target half in two. The
discriminator is *not* the pronoun — every class prints both "they" and "that
player" — so no hard-coded scope is right for the set:

| Class | Antecedent introduced by | Witness | Status |
|---|---|---|---|
| A | a per-player trigger iteration | Citadel of Pain | **FIXED**, runtime test + revert proof |
| B1 | a declared target in a PRIOR clause | Curious Herd | deferred (see below) |
| B2 | a target the clause's OWN subject declares | Jovial Evil; Pact of the Serpent's chained sibling | deferred (see below) |

## Why B1/B2 are deferred — the ctx-free combinator layer

Threading antecedents into `oracle_util::parse_count_slot` (the count-slot leaf)
*does* fix Curious Herd — measured, final AST `controller: DeclaredPlayer`. It was
reverted anyway, because it makes two paths disagree on one card where they must
agree, and the repo already pins that agreement:

- `oracle_effect::tests::shared_where_x_binds_draws_and_loses_life_siblings`
  (issue #1587) requires both siblings of "target player draws X cards and loses X
  life, where X is …" to share one binding. Threading the count slot fixes the
  draw and cannot fix the sibling, so the test goes red. Before the change both
  siblings read `You` — wrong, but consistently wrong, which is what it pinned.

The sibling's phrase, "the number of creatures they control **of the chosen
type**", is DECLINED by the antecedent-aware CDA parser and falls through to
`oracle_nom::quantity`, which is **ctx-free by signature**
(`fn(input: &str) -> OracleResult<'_, QuantityRef>`; 21 context-free
`parse_type_phrase_folding(` call sites). Measured call paths, from an instrument
on the legacy-seed fallback itself (158 firings):

- `parse_the_number_of` → `parse_number_of_inner` → `parse_qualified_controlled_type`
  → `parse_type_phrase_folding` (no ctx) → seed → `You`
- the same shape via `parse_sacrificed_this_turn_filter` and
  `parse_for_each_clause_ref_with_they_controller`

**The anaphor is destroyed in transit:** by the time that layer returns a
`QuantityRef`, the phrase has already become `ControllerRef::You`, so nothing
downstream can rebind it. Covering B1/B2 therefore requires either antecedents
threaded through a deliberately pure combinator layer, or that layer taught to
return an *unbound* anaphor for a later binding pass — a design task whose blast
radius includes the sacrifice and for-each quantity families, not a leaf fix.

A dead end worth recording so it is not retried: `ParseContext::clause_declared_group`
looks like the missing B2 rung and is not. It has only two writers (a damage
continuation chain, a `TargetOnly` exile root), so it is `None` for the Pact class,
and at the capture site the clause IR's antecedents were already correct
(`declared_they: Some(DeclaredPlayer { .. })`) — the capture was never the defect.

## Root cause

`lower.rs::parse_where_x_quantity_with_owner` calls the **no-context** quantity
parser:

- `crates/phase-parser/src/parser/oracle_effect/lower.rs:12160` —
  `None => parse_cda_quantity(where_x_expression)`

The context-bearing sibling `parse_cda_quantity_with_context` *does* bind a
controller anaphor from the context, and that behavior is already pinned by
`oracle_quantity.rs::cda_quantity_uses_relative_player_scope_for_they_control`.
The where-X path simply never supplies a context.

With no antecedent, the binding falls to a fallback that is **explicitly named and
documented as interim legacy**:

- `crates/phase-parser/src/parser/oracle_target.rs:6784` —
  `interim_you_anaphor_seed`: *"A present-tense 'that player controls' / 'they
  control' … with no antecedent in scope reads as `You`, the legacy default …"*

## Why the fix is strictly additive

`parse_controller_suffix` tries the real binder first and the legacy seed **only**
when the binder finds no antecedent:

- `crates/phase-parser/src/parser/oracle_target.rs:6714`

```rust
let ctrl = bind_controller_phrase(&phrase, &ctx.player_antecedents())
    .or_else(|| legacy_reader_anaphor_seed(&phrase))?;
```

Therefore supplying an antecedent changes **only** the cards that have one;
every call site left un-threaded keeps today's byte-identical behavior via the
seed. No coverage regression is reachable from the threading itself, and the
`interim_` seed is displaced exactly as its name anticipates.

## The building block to reuse — do not add a new field or enum

All of the machinery already exists on main. The fix is reuse, so neither the
`add-engine-variant` gate nor a new `ParseContext` field is in scope.

- `oracle_ir/context.rs:67` — `PlayerAntecedents { declared_that_player,
  declared_they, third_person, object_controller }`, a **rung hierarchy**:
  a chain-declared player is the nearer antecedent of "that player" / "they"
  than `third_person`.
- `oracle_ir/context.rs:121` — `bind_controller_phrase`, "the single binder for a
  parsed controller phrase", which **fails closed** to `None`.
- `oracle_ir/context.rs:1011` — `ParseContext::player_antecedents()`, which
  already derives those rungs from `declared_player_scope`,
  `prior_player_declaration` and `relative_player_scope`.

Class A falls out of the existing rungs with no new logic, and is what shipped:

- Citadel of Pain — the trigger sets `relative_player_scope = ScopedPlayer`
  → `third_person` → "they control" → `ScopedPlayer`.

B1/B2 reach the rungs correctly too — the measured clause-IR antecedents are
right (`declared_they: Some(DeclaredPlayer { .. })`). They are blocked downstream
of the rungs, in the ctx-free combinator layer described above, not by missing
antecedents.

A thread-local is **not** an option here: the codebase deliberately removed the
thread-locals this would reintroduce (`relative_player_scope` "replaces
thread-local `oracle_target_scope`"; `diagnostics` "replaces thread-local
`oracle_warnings` accumulator").

## Shape of the change

The where-X rewriter family threads `where_x_expression: Option<&str>` through 19
signatures in `lower.rs` (166 mentions, ~21 of which consume it as a string),
rooted at `apply_where_x_ability_expression` (`lower.rs:13739`) and reaching the
single quantity-slot authority `bind_where_x_quantity` (`lower.rs:12502`).

Rather than add a parallel parameter to all 19, **change the parameter's type** to
a typed binding that carries both axes — the house "parameterize, don't
proliferate" move, and it keeps every pass-through call site unchanged:

```rust
pub(crate) struct WhereXBinding<'a> {
    expression: Option<&'a str>,
    /// `None` = no antecedent was in scope at this lowering site, which is how
    /// every site behaved before this binding existed.
    antecedents: Option<&'a PlayerAntecedents>,
}
```

The antecedents are optional, not mandatory, so a site with no `ParseContext` in
reach (`try_parse_reveal_until`, `parse_counter_ast`) says so in the type instead
of fabricating an empty set that reads like a real measurement.

One method, `WhereXBinding::resolve_quantity`, is the single door from where-X
text to a quantity, so the expression and the antecedents it is read under cannot
drift apart at a call site. Every leaf goes through it.

The antecedents are captured where the live context exists — every
`ClauseIrBuilder` site in the effect-chain chunk loop that already sets
`where_x_expression` — carried on `ClauseIr` beside it, and handed to the
rewriter by one accessor, `ClauseIr::where_x_binding`, so a call site cannot
supply the expression and forget the antecedents.

Two sites are NOT a plain capture, and both are load-bearing:

- The DigAlt seam inherits its where-X text from the *previous* clause
  (`prev_clause.and_then(|c| c.where_x_expression.clone())`), so it inherits the
  antecedents from that same clause. Reading the current chunk's context there
  would bind an inherited count's anaphor to a player the inherited clause never
  named.
- `mana.rs::apply_where_x_count_expression` reaches the same bare leaf and its
  enclosing `try_parse_add_mana_effect_with_context` already has a live context,
  so the where-X *mana* count (Carpet of Flowers and its class) is threaded too.
  This is beyond the five cards in the table above and is the reason the fix is a
  class fix rather than a card fix.

`ClauseIr` is `pub(crate)` parser IR, but it **does** derive `Serialize` (it
appears in parser traces), and `PlayerAntecedents` does not. The new field is
`#[serde(skip)]`, so the trace schema is unchanged and no fixture moves.

## Verification that discriminates

The tests live in `crates/engine/tests/integration/controller_anaphor_binds_through_context.rs`,
which already exists for this family ("an anaphoric controller phrase keeps
binding to the player its ability introduced") and already pins the
**direct-amount** form of both antecedent classes. The where-X form was the hole;
the shipped change fills the per-player half of it:

| | per-player antecedent | declared-target antecedent |
|---|---|---|
| direct amount | Acidic Soil, Price of Progress (pre-existing) | Anathemancer (pre-existing) |
| `, where X is …` | **Citadel of Pain** (shipped) | B1/B2 — deferred, see above |

Rows actually shipped:

1. **Citadel of Pain** (CR 109.4 + CR 115.1) — runtime, two players with unequal
   untapped land counts; on the opponent's end step the damage must equal *that
   player's* count, not the controller's. Reverting the production change must
   flip it.
   Verified: disabling the fix at its single choke point
   (`WhereXBinding::antecedents_or_empty`) makes this row FAIL.
2. **Coverage honesty** — Psychic Allergy remains an honest `where_x_binding`
   gap; the fix must not flip an unrepresentable count (its chosen-colour filter)
   to falsely-supported. Asserted over the whole parse, because both of that
   card's triggers are `Phase::Upkeep` and selecting one by phase is ambiguous.

Deferred with the classes they cover — written and measured, then removed with the
production change they depend on: a Curious Herd runtime row (P0/P1 unequal
artifact counts; the token count must read the declared target's) and its paired
`you control` reach-guard proving the threading does not make every count
target-relative.

The parse-diff receipt is the blast-radius check: any card outside this class
whose where-X count changes controller is unintended, and the additive argument
above says there should be none.
