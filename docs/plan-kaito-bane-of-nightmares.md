# Kaito, Bane of Nightmares — Implementation Plan

**Oracle Text:**
> Ninjutsu {1}{U}{B}
> During your turn, as long as Kaito has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof.
> [+1]: You get an emblem with "Ninjas you control get +1/+1."
> [0]: Surveil 2. Then draw a card for each opponent who lost life this turn.
> [−2]: Tap target creature. Put two stun counters on it.

## Current Status

- Mana cost / color identity: correct (2UB, color override present)
- Loyalty: 4, correct
- Ninjutsu keyword: parsed (type exists in `keywords.rs`, cost extracted as `{1}{U}{B}`)
- [-2] Tap + stun counters: fully parsed (Tap + PutCounter sub_ability with stun/2) — should work at runtime
- Static (hexproof only): partially parsed — AddKeyword Hexproof is present but condition and creature animation are missing
- Surveil 2: parsed and functional

## Gaps

### 1. Ninjutsu runtime (high complexity)

The keyword is parsed but has zero game logic in `crates/engine/src/game/`. Ninjutsu (CR 702.49) requires:

- Activation timing: during the declare blockers step, after blockers are declared
- Cost: pay mana + return an unblocked attacker you control to its owner's hand
- Effect: put this card from your hand onto the battlefield tapped and attacking
- The attacking creature enters without going through the declare attackers step — it's simply "attacking" (no attack trigger, no "becomes blocked" check)
- It bypasses normal casting — it's an activated ability from hand, not a spell

Key engine areas: `combat.rs` (unblocked attacker detection), `keywords.rs` (cost structure), `casting.rs` or a new handler for alternative-zone activated abilities.

Ninjutsu is shared by ~30+ cards across Magic's history, so this is a high-value building block.

### 2. Emblem support (high complexity — cross-cutting)

The engine has no emblem infrastructure. Emblems (CR 114) require:

- A new game object type or zone concept (emblems live in the command zone)
- Emblems have no characteristics except abilities — they can't be destroyed, exiled, or interacted with
- Emblems persist for the rest of the game
- Kaito's emblem grants a continuous effect: "Ninjas you control get +1/+1"
- This continuous effect needs to go through the layer system (layer 7c for P/T modification)
- The effect filters by subtype (Ninja) and controller (emblem's controller)

This is not just one card's problem — emblems appear on dozens of planeswalkers. Building this unlocks a large class of cards.

### 3. Planeswalker-to-creature conditional animation (medium complexity)

"During your turn, as long as Kaito has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof."

This is a conditional static ability that:
- Adds creature type (with subtype Ninja)
- Sets base P/T to 3/4
- Adds hexproof
- Only active during controller's turn AND when Kaito has ≥1 loyalty counter

The existing `Animate` effect handles turning non-creatures into creatures, but this needs:
- A compound condition: `IsYourTurn` AND `HasCounters(Loyalty, ≥1)`
- The static ability parser to emit the full animation (currently only emits AddKeyword Hexproof)

Similar pattern: Gideon planeswalkers (e.g., Gideon Jura "he's a 6/6 Human Soldier creature"), though Gideon does it via activated ability, not static. The static conditional version is more like Restless lands ("as long as ~ is a creature...") but with a turn condition added.

### 4. Variable draw count — opponents who lost life this turn (medium complexity)

"Draw a card for each opponent who lost life this turn" currently parses as a flat `Draw { count: 1 }` with incorrect `duration: UntilEndOfTurn`.

This needs:
- A `QuantityRef` variant (e.g., `OpponentsWhoLostLifeThisTurn`) that queries game state
- The `Draw` effect to accept a dynamic count (`QuantityExpr` rather than fixed `i32`)
- The engine already tracks `life_lost_this_turn` on each player (found in `player.rs` and `life.rs`)
- At resolution time, count how many opponents of the controller lost life this turn, draw that many

This pattern ("for each opponent who...") appears on multiple cards and is a useful building block.

### 5. Compound static conditions (low-medium complexity)

The static ability needs two simultaneous conditions:
- "During your turn" — a turn-based condition
- "As long as Kaito has one or more loyalty counters on him" — a self-referential counter check

The condition system may need a `And` combinator or compound condition variant. Check existing condition types in `ability.rs` to see what's already supported.
