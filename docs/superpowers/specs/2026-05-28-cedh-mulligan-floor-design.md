# cEDH AI Mulligan Floor — Design

**Date:** 2026-05-28
**Status:** Approved (approach + scope), pending spec review
**Scope:** `crates/phase-ai` mulligan policies + one public engine helper

## Problem

The cEDH mulligan policy (`crates/phase-ai/src/policies/mulligan/cedh_keepables.rs`)
is intentionally aggressive: it `ForceMulligan`s any hand that lacks 2–4 lands
plus a fast-mana / tutor / interaction staple (or a complete in-hand combo).
Under the London + free-first mulligan rules that Commander/cEDH games use, a
run of force-mulligans can drive the AI's kept hand all the way down toward the
engine's hard floor of **1 card** (`MAX_MULLIGANS`, `crates/engine/src/game/mulligan.rs`).

A real cEDH player stops mulliganing well before that — a 3-card hand almost
never wins. We want the cEDH AI to refuse any mulligan that would leave it with
**fewer than 4 cards**.

## Goal / Non-goals

**Goal:** When the cEDH AI is deciding whether to mulligan, and taking one more
mulligan would result in a kept hand of fewer than 4 cards, the AI keeps its
current hand instead — overriding every other policy's verdict.

**Non-goals:**
- No change for non-cEDH decks (floor is gated on `bracket_tier == Cedh`).
- No "use Serum Powder at the floor" optimization (Serum Powder never reduces
  hand size, so it can't violate the floor; smarter Powder use is out of scope).
- No change to the engine's legal mulligan limit (`MAX_MULLIGANS`). The floor is
  an AI *preference* that stops well above the engine's hard cap.

## Card-count math (CR 103.5)

At a mulligan decision the player holds a fresh 7-card hand and has
`mulligan_count` mulligans already taken. On a keep they bottom `bottom_count`
cards, where:

- `bottom_count = mulligan_count` in normal formats, or
- `bottom_count = mulligan_count - 1` when the game grants a free first
  mulligan (Commander / cEDH, or any ≥3-seat game).

So **kept hand size = `7 - bottom_count`**. "Never below 4 cards" means: force a
keep whenever taking one more mulligan would make the *next* kept hand size < 4.

Worked thresholds:

| Format        | Force-keep when | At that count, keep = | One more mulligan = |
|---------------|-----------------|-----------------------|---------------------|
| free-first    | `mulligan_count ≥ 4` | 4 cards          | 3 cards (blocked)   |
| normal        | `mulligan_count ≥ 3` | 4 cards          | 3 cards (blocked)   |

The implementation computes this from the resulting-hand-size math rather than a
hardcoded count, so it stays correct under both rule sets.

## Architectural gap

`MulliganScore` (`crates/phase-ai/src/policies/mulligan/mod.rs`) has only:
- `ForceMulligan { reason }` — hard veto toward mulligan, and
- `Score { delta, reason }` — additive (positive = prefer keep).

The registry gives **any** `ForceMulligan` absolute priority:
`keep = if forced { false } else { total > 0.0 }`. A policy therefore cannot
override another policy's (or its own) force-mulligan to lock in a keep — even an
arbitrarily large positive `delta` loses. Enforcing a floor requires a hard
"force keep" that outranks `ForceMulligan`.

## Design

### 1. New `MulliganScore::ForceKeep` variant

Add a third variant:

```rust
pub enum MulliganScore {
    /// Hard veto toward keeping — outranks ForceMulligan. A policy emits this
    /// when the hand must not be mulliganed regardless of other verdicts
    /// (e.g. a card-count floor).
    ForceKeep { reason: PolicyReason },
    ForceMulligan { reason: PolicyReason },
    Score { delta: f64, reason: PolicyReason },
}
```

### 2. Registry precedence: `ForceKeep > ForceMulligan > sum(delta)`

In `MulliganRegistry::evaluate_hand`, track both forces:

```rust
let mut forced_keep = false;
let mut forced_mulligan = false;
let mut total = 0.0;
for policy in &self.policies {
    match &score {
        MulliganScore::ForceKeep { .. } => forced_keep = true,
        MulliganScore::ForceMulligan { .. } => forced_mulligan = true,
        MulliganScore::Score { delta, .. } => total += *delta,
    }
    trace.push((policy.id(), score));
}
let keep = if forced_keep {
    true
} else if forced_mulligan {
    false
} else {
    total > 0.0
};
```

Update the module doc comment to describe the three-way precedence.

### 3. Public engine helper (CR 103.5 single authority)

`bottom_count_for` is currently private in `crates/engine/src/game/mulligan.rs`.
Expose the resulting-hand-size rule as the engine's single authority so phase-ai
does not duplicate the free-first math:

```rust
/// CR 103.5: Number of cards a player keeps after deciding to keep with
/// `mulligan_count` mulligans taken (free-first discount applied when the
/// game grants one). Starting hand size minus the bottoms owed.
pub fn kept_hand_size_after(mulligan_count: u8, free_first: bool) -> usize {
    STARTING_HAND_SIZE.saturating_sub(bottom_count_for(mulligan_count, free_first) as usize)
}
```

(`saturating_sub` guards the underflow at extreme `mulligan_count` values where
bottoms owed exceed the starting hand size.)

(`bottom_count_for` stays private; only the hand-size result is exported.)

### 4. cEDH policy emits `ForceKeep` at the floor

In `CedhKeepablesMulligan::evaluate`, before the existing land-count /
acceleration `ForceMulligan` branches, add the floor guard. The policy is
already gated on `bracket_tier == Cedh`, so the floor is cEDH-only by
construction.

```rust
const CEDH_MULLIGAN_FLOOR: usize = 4;

// (after the bracket_tier gate)
let free_first = match &state.waiting_for {
    WaitingFor::MulliganDecision { free_first_mulligan, .. } => *free_first_mulligan,
    // Evaluated outside the mulligan step (tests / projection) — no floor.
    _ => false,
};
if engine::game::mulligan::kept_hand_size_after(mulligans_taken + 1, free_first)
    < CEDH_MULLIGAN_FLOOR
{
    return MulliganScore::ForceKeep {
        reason: PolicyReason::new("cedh_keepables_card_floor")
            .with_fact("mulligans_taken", mulligans_taken as i64),
    };
}
```

The trait already receives `mulligans_taken` and `state`; `free_first_mulligan`
is read from the authoritative `WaitingFor::MulliganDecision` field rather than
re-deriving format/seat rules.

### 5. Bridge — no change needed

`search.rs` already maps `decision.keep == true` to `MulliganChoice::Keep` and
only consults Serum Powder when `!decision.keep`. With `ForceKeep` producing
`keep == true`, the AI keeps and never burns a Powder at the floor. No edit to
the bridge.

## Data flow

```
search.rs::choose_action
  └─ WaitingFor::MulliganDecision { pending, free_first_mulligan }
       └─ MulliganRegistry::evaluate_hand(hand, state, …, mulligans_taken)
            └─ CedhKeepablesMulligan::evaluate
                 ├─ bracket_tier == Cedh?            (gate)
                 ├─ kept_hand_size_after(n+1, ff) < 4 → ForceKeep   ← new floor
                 ├─ land-count / acceleration checks → ForceMulligan
                 └─ baseline / combo                 → Score
            └─ aggregate: ForceKeep > ForceMulligan > sum(delta)
       └─ keep → MulliganChoice::Keep
```

## Testing

Engine (`crates/engine/src/game/mulligan.rs` tests):
- `kept_hand_size_after`: normal (count 0→7, 3→4, 4→3) and free-first
  (count 0→7, 1→7, 4→4, 5→3).

Registry (`policies/mulligan/mod.rs` tests):
- `ForceKeep` overrides a co-occurring `ForceMulligan` → `keep == true`.
- Precedence ordering with mixed `ForceKeep` + `Score` deltas.

cEDH policy (`cedh_keepables.rs` tests):
- free-first: a hand that would otherwise `ForceMulligan` (e.g. < 2 lands)
  returns `ForceKeep` once `mulligans_taken == 4`.
- free-first: same hand at `mulligans_taken == 3` still `ForceMulligan`s
  (floor not yet reached — keeping there = 5 cards).
- non-free: floor engages at `mulligans_taken == 3`.
- non-cEDH deck at high `mulligans_taken` is unaffected (zero-delta Score).

## Risk / blast radius

Low. One new enum variant (one exhaustive match site to update — the registry
aggregation; all other sites are constructors or wildcard test matches), one new
public engine helper, one guard branch in the cEDH policy. No transport, no
frontend, no change to the engine's mulligan resolution or legal-action limits.
