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

**Two antecedent classes, one seam.** This is why a single hard-coded scope cannot
be right: the discriminator is *not* the pronoun — both classes say both "they"
and "that player" — it is whether the player was introduced by a per-player
trigger iteration or by a declared target.

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

Both classes then fall out of the existing rungs with no new logic:

- Citadel of Pain — the trigger sets `relative_player_scope = ScopedPlayer`
  → `third_person` → "they control" → `ScopedPlayer`.
- Curious Herd / Jovial Evil / Pact of the Serpent — the chain sets
  `declared_player_scope = TargetPlayer` → `declared_that_player` / `declared_they`
  → `TargetPlayer`.

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
    antecedents: &'a PlayerAntecedents,
}
```

The antecedents are captured where the live context exists — the effect-chain
chunk loop that already sets `where_x_expression` on the clause IR
(`oracle_effect/mod.rs:48518, 48553, 49620, 49692, 50321, 50459`) — carried on
`ClauseIr` beside `where_x_expression`
(`oracle_ir/effect_chain.rs:950`), and handed to the rewriter by
`assembly.rs:3636`. `ClauseIr` is `pub(crate)` parser IR and is not serialized,
so there is no wire or fixture surface.

## Verification that discriminates

Per-class, because a single fixture cannot distinguish the two antecedents — the
exact failure that made the previous PR's test inadequate:

1. **Citadel of Pain** (CR 109.4 + CR 115.1) — runtime, two players with unequal
   untapped land counts; on the opponent's end step the damage must equal *that
   player's* count, not the controller's. Reverting the production change must
   flip it.
2. **Curious Herd** (CR 608.2c) — runtime, P0/P1 with unequal artifact counts;
   the token count must read the *declared target's* artifacts. This is the case
   the previous approach got wrong, and it must fail under a hard-coded
   `ScopedPlayer` as well as under today's `You`.
3. **Negative/reach guard** — a where-X count with a *direct* "you control"
   phrase must stay `ControllerRef::You`, proving the threading did not make every
   count target-relative.
4. **Coverage honesty** — Psychic Allergy must remain an honest
   `where_x_binding` gap; the fix must not flip an unrepresentable count to
   falsely-supported.

The parse-diff receipt is the blast-radius check: any card outside this class
whose where-X count changes controller is unintended, and the additive argument
above says there should be none.
