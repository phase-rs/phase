# RevealHand Effect Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement RevealHand as a first-class effect mechanic, enabling ~207 cards (Thoughtseize, Duress, Intimidation Tactics, etc.) that reveal an opponent's hand and choose a card from it.

**Architecture:** New `Effect::RevealHand` variant + `WaitingFor::RevealChoice` state. The effect handler marks opponent's hand cards as revealed in `GameState.revealed_cards`, then sets `RevealChoice` for the caster to select a card. The sub-ability chain (already parsed correctly for exile/discard) runs via `pending_continuation`. The `filter_state_for_player` function respects `revealed_cards` — cards in this set are NOT hidden, so the frontend sees real card data instead of "Hidden Card". The frontend already renders card faces vs card backs based on `face_down` on the object — no frontend card rendering changes needed, only adding the `RevealChoice` case to `CardChoiceModal`.

**Tech Stack:** Rust engine (types, effect handler, engine match arm, parser), TypeScript frontend (types, modal)

---

## File Map

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `crates/engine/src/types/ability.rs` | Add `Effect::RevealHand` variant, `EffectKind::RevealHand`, display name, `From` impl |
| Modify | `crates/engine/src/types/game_state.rs` | Add `revealed_cards: HashSet<ObjectId>` to `GameState`, add `WaitingFor::RevealChoice` variant |
| Create | `crates/engine/src/game/effects/reveal_hand.rs` | Effect handler: mark cards revealed, emit event, set `WaitingFor::RevealChoice` |
| Modify | `crates/engine/src/game/effects/mod.rs` | Register module, add dispatch arm, add to `is_known_effect`, add `RevealChoice` to continuation check |
| Modify | `crates/engine/src/game/engine.rs` | Add `RevealChoice` + `SelectCards` match arm — chosen card flows to `pending_continuation` |
| Modify | `crates/engine/src/parser/oracle_effect.rs` | Fix `"reveal "` handler to distinguish library-reveal from hand-reveal |
| Modify | `crates/server-core/src/filter.rs` | Skip hiding for cards in `revealed_cards` |
| Modify | `crates/server-core/src/session.rs` | Add `RevealChoice` to player extraction match |
| Modify | `crates/phase-ai/src/legal_actions.rs` | Add `RevealChoice` legal action generation |
| Modify | `crates/phase-ai/src/search.rs` | Add `RevealChoice` AI heuristic |
| Modify | `client/src/adapter/types.ts` | Add `RevealChoice` to `WaitingFor` union |
| Modify | `client/src/components/modal/CardChoiceModal.tsx` | Add `RevealChoice` case with `RevealModal` component |
| Modify | `client/src/components/hand/OpponentHand.tsx` | Show card faces when object data is present (already works via `face_down` — just pass `showCards` based on non-hidden objects) |

---

## Chunk 1: Engine Types & Effect Handler

### Task 1: Add `Effect::RevealHand` variant and type plumbing

**Files:**
- Modify: `crates/engine/src/types/ability.rs:689-700` (before `Unimplemented`)
- Modify: `crates/engine/src/types/ability.rs:790-791` (display name)
- Modify: `crates/engine/src/types/ability.rs:844-845` (EffectKind — repurpose existing `Reveal` placeholder)
- Modify: `crates/engine/src/types/ability.rs:896-897` (From impl)

- [ ] **Step 1: Add `Effect::RevealHand` variant**

In `crates/engine/src/types/ability.rs`, insert before `Unimplemented`:

```rust
RevealHand {
    #[serde(default = "default_target_filter_any")]
    target: TargetFilter,
},
```

- [ ] **Step 2: Add display name**

In the `impl Effect` display name match (around line 790), add:

```rust
Effect::RevealHand { .. } => "RevealHand",
```

- [ ] **Step 3: Wire `EffectKind::Reveal` to `RevealHand`**

The `EffectKind::Reveal` variant already exists (line 849) as a placeholder. Keep it — it's now the real kind for `RevealHand`.

In the `From<&Effect> for EffectKind` impl (around line 896), add before `Unimplemented`:

```rust
Effect::RevealHand { .. } => EffectKind::Reveal,
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p engine 2>&1 | head -20`
Expected: Success (no errors — `RevealHand` is defined but not yet dispatched)

- [ ] **Step 5: Commit**

```
feat: add Effect::RevealHand variant with EffectKind plumbing
```

---

### Task 2: Add `revealed_cards` to `GameState` and `WaitingFor::RevealChoice`

**Files:**
- Modify: `crates/engine/src/types/game_state.rs:69-70` (WaitingFor enum — add RevealChoice)
- Modify: `crates/engine/src/types/game_state.rs:267-278` (GameState struct — add revealed_cards)
- Modify: `crates/engine/src/types/game_state.rs:345-348` (GameState::new — initialize revealed_cards)

- [ ] **Step 1: Add `WaitingFor::RevealChoice`**

In the `WaitingFor` enum (after `SurveilChoice`), add:

```rust
RevealChoice {
    player: PlayerId,
    cards: Vec<ObjectId>,
    /// Type filter for which cards can be chosen (e.g. artifact or creature only).
    #[serde(default = "super::ability::default_target_filter_any")]
    filter: TargetFilter,
},
```

- [ ] **Step 2: Add `revealed_cards` field to `GameState`**

After `triggers_fired_this_game` (line ~271), add:

```rust
/// Cards currently revealed to all players (e.g. during a RevealHand effect).
/// `filter_state_for_player` skips hiding these cards.
#[serde(default)]
pub revealed_cards: HashSet<ObjectId>,
```

- [ ] **Step 3: Initialize in `GameState::new`**

In the constructor (before `pending_continuation: None`), add:

```rust
revealed_cards: HashSet::new(),
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p engine 2>&1 | head -20`
Expected: May have warnings about unused fields, but no errors.

- [ ] **Step 5: Commit**

```
feat: add WaitingFor::RevealChoice and GameState.revealed_cards
```

---

### Task 3: Create `reveal_hand.rs` effect handler

**Files:**
- Create: `crates/engine/src/game/effects/reveal_hand.rs`
- Test: inline `#[cfg(test)]` module

- [ ] **Step 1: Write the test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use crate::types::ability::TargetRef;

    fn make_reveal_ability(controller: PlayerId, target_player: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Player(target_player)],
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn reveal_hand_sets_reveal_choice_with_opponent_hand() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(&mut state, CardId(1), PlayerId(1), "Bolt".to_string(), Zone::Hand);
        let card2 = create_object(&mut state, CardId(2), PlayerId(1), "Bear".to_string(), Zone::Hand);

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::RevealChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&card1));
                assert!(cards.contains(&card2));
            }
            other => panic!("Expected RevealChoice, got {:?}", other),
        }
    }

    #[test]
    fn reveal_hand_marks_cards_as_revealed() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(&mut state, CardId(1), PlayerId(1), "Bolt".to_string(), Zone::Hand);

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.revealed_cards.contains(&card1));
    }

    #[test]
    fn reveal_hand_emits_cards_revealed_event() {
        let mut state = GameState::new_two_player(42);
        create_object(&mut state, CardId(1), PlayerId(1), "Bolt".to_string(), Zone::Hand);

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::CardsRevealed { .. })));
    }

    #[test]
    fn reveal_empty_hand_does_nothing() {
        let mut state = GameState::new_two_player(42);
        // Player 1 has no cards in hand

        let ability = make_reveal_ability(PlayerId(0), PlayerId(1));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should not set RevealChoice — no cards to choose from
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
```

- [ ] **Step 2: Write the implementation**

```rust
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// RevealHand: reveal target player's hand, then let the caster choose a card.
///
/// Marks all cards in the target player's hand as revealed in `GameState.revealed_cards`
/// (so `filter_state_for_player` doesn't hide them), emits `CardsRevealed`, and sets
/// `WaitingFor::RevealChoice` for the caster to select a card matching the filter.
/// The sub-ability chain (exile, discard, etc.) runs via `pending_continuation`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let filter = match &ability.effect {
        Effect::RevealHand { target } => target.clone(),
        _ => TargetFilter::Any,
    };

    // Find the target player from resolved targets
    let target_player = ability
        .targets
        .iter()
        .find_map(|t| match t {
            TargetRef::Player(pid) => Some(*pid),
            _ => None,
        })
        .ok_or(EffectError::NoValidTargets)?;

    let hand: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .map(|p| p.hand.clone())
        .unwrap_or_default();

    if hand.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Reveal,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Mark all hand cards as revealed
    for &card_id in &hand {
        state.revealed_cards.insert(card_id);
    }

    // Emit event with card names
    let card_names: Vec<String> = hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect();
    events.push(GameEvent::CardsRevealed {
        player: target_player,
        card_names,
    });

    state.waiting_for = WaitingFor::RevealChoice {
        player: ability.controller,
        cards: hand,
        filter,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Reveal,
        source_id: ability.source_id,
    });

    Ok(())
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p engine -- reveal_hand --nocapture`
Expected: All 4 tests pass.

- [ ] **Step 4: Commit**

```
feat: add reveal_hand effect handler with tests
```

---

### Task 4: Register the handler and wire dispatch

**Files:**
- Modify: `crates/engine/src/game/effects/mod.rs:31` (add module declaration)
- Modify: `crates/engine/src/game/effects/mod.rs:41-86` (add dispatch arm)
- Modify: `crates/engine/src/game/effects/mod.rs:91-133` (add to `is_known_effect`)
- Modify: `crates/engine/src/game/effects/mod.rs:161-165` (add `RevealChoice` to continuation check)

- [ ] **Step 1: Add module declaration**

After `pub mod pump;` (line 27), add:

```rust
pub mod reveal_hand;
```

- [ ] **Step 2: Add dispatch arm**

In `resolve_effect` match, after `Effect::Shuffle { .. }` arm (line 80), add:

```rust
Effect::RevealHand { .. } => reveal_hand::resolve(state, ability, events),
```

- [ ] **Step 3: Add to `is_known_effect`**

In the `is_known_effect` matches (line ~133), add `"RevealHand"` to the list.

- [ ] **Step 4: Add `RevealChoice` to continuation check**

In `resolve_ability_chain` (line ~161-165), add `WaitingFor::RevealChoice { .. }` to the pattern that saves sub-abilities as continuations:

```rust
if matches!(
    state.waiting_for,
    WaitingFor::ScryChoice { .. }
        | WaitingFor::DigChoice { .. }
        | WaitingFor::SurveilChoice { .. }
        | WaitingFor::RevealChoice { .. }
) {
```

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p engine 2>&1 | tail -5`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```
feat: register RevealHand in effect dispatch and continuation check
```

---

## Chunk 2: Engine Resolution & State Filtering

### Task 5: Add `RevealChoice` resolution in `engine.rs`

**Files:**
- Modify: `crates/engine/src/game/engine.rs:415` (after `DigChoice` arm)

- [ ] **Step 1: Add `RevealChoice` + `SelectCards` match arm**

After the `DigChoice` arm (ends ~line 415), add:

```rust
// RevealChoice: player selects a card from revealed hand.
// Chosen card becomes the target for the pending_continuation sub-ability
// (e.g. ChangeZone to exile, DiscardCard, etc.). Unchosen cards stay in hand.
// Clear revealed_cards so opponent's hand goes hidden again.
(
    WaitingFor::RevealChoice {
        player,
        cards: all_cards,
        ..
    },
    GameAction::SelectCards { cards: chosen },
) => {
    let p = *player;
    let all = all_cards.clone();
    if chosen.len() != 1 {
        return Err(EngineError::InvalidAction(format!(
            "Must select exactly 1 card, got {}",
            chosen.len()
        )));
    }
    let chosen_id = chosen[0];
    if !all.contains(&chosen_id) {
        return Err(EngineError::InvalidAction(
            "Selected card not in revealed hand".to_string(),
        ));
    }

    // Clear revealed status
    for &card_id in &all {
        state.revealed_cards.remove(&card_id);
    }

    // Run the pending continuation with the chosen card as its target
    if let Some(mut cont) = state.pending_continuation.take() {
        cont.targets = vec![crate::types::ability::TargetRef::Object(chosen_id)];
        let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
        state.waiting_for.clone()
    } else {
        WaitingFor::Priority { player: p }
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p engine 2>&1 | tail -5`
Expected: All tests pass.

- [ ] **Step 3: Commit**

```
feat: add RevealChoice resolution in engine — chosen card flows to continuation
```

---

### Task 6: Update `filter_state_for_player` to respect `revealed_cards`

**Files:**
- Modify: `crates/server-core/src/filter.rs:13-19` (skip hiding for revealed cards)
- Test: inline test in same file

- [ ] **Step 1: Write the test**

Add to the existing `tests` module in `crates/server-core/src/filter.rs`:

```rust
#[test]
fn revealed_cards_remain_visible_in_opponent_hand() {
    let mut state = setup_state();
    let opp_hand = &state.players[1].hand;
    let revealed_id = opp_hand[0];

    // Mark the card as revealed
    state.revealed_cards.insert(revealed_id);

    let filtered = filter_state_for_player(&state, PlayerId(0));

    let obj = filtered.objects.get(&revealed_id).unwrap();
    assert_ne!(obj.name, "Hidden Card", "Revealed card should not be hidden");
    assert!(!obj.face_down, "Revealed card should not be face_down");
}
```

- [ ] **Step 2: Modify `filter_state_for_player`**

Change the opponent hand hiding loop (lines 13-19) to skip revealed cards:

```rust
// Hide hand card details for ALL opponents (not just one)
let opponents = players::opponents(state, viewer);
let opp_hand_ids: Vec<ObjectId> = opponents
    .iter()
    .flat_map(|&opp| filtered.players[opp.0 as usize].hand.iter().copied())
    .collect();
for obj_id in opp_hand_ids {
    if !state.revealed_cards.contains(&obj_id) {
        hide_card(&mut filtered, obj_id);
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p server-core 2>&1 | tail -5`
Expected: All tests pass including new test.

- [ ] **Step 4: Commit**

```
feat: filter_state_for_player skips hiding revealed cards
```

---

### Task 7: Update server-core session and AI for `RevealChoice`

**Files:**
- Modify: `crates/server-core/src/session.rs:33-35` (add RevealChoice to player extraction)
- Modify: `crates/phase-ai/src/legal_actions.rs:60-68` (add RevealChoice legal actions)
- Modify: `crates/phase-ai/src/search.rs:107-118` (add RevealChoice AI heuristic)

- [ ] **Step 1: Add to session player extraction**

In the `WaitingFor` match in `crates/server-core/src/session.rs` (around line 33-35), add:

```rust
| WaitingFor::RevealChoice { player, .. }
```

- [ ] **Step 2: Add legal action generation**

In `crates/phase-ai/src/legal_actions.rs`, after the `DigChoice` arm, add:

```rust
WaitingFor::RevealChoice { cards, .. } => {
    // Each card in the revealed hand is a valid choice
    cards
        .iter()
        .map(|&card| GameAction::SelectCards { cards: vec![card] })
        .collect()
}
```

- [ ] **Step 3: Add AI heuristic**

In `crates/phase-ai/src/search.rs`, after the `DigChoice` handling, add:

```rust
if let WaitingFor::RevealChoice { cards, .. } = &state.waiting_for {
    // Pick the highest-value card from opponent's hand to exile/discard
    let mut scored: Vec<_> = cards
        .iter()
        .map(|&id| (id, crate::eval::evaluate_card(state, id)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    if let Some((best, _)) = scored.first() {
        return Some(GameAction::SelectCards { cards: vec![*best] });
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --all 2>&1 | tail -5`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```
feat: add RevealChoice support to server session and AI
```

---

## Chunk 3: Parser Fix

### Task 8: Fix oracle parser to distinguish reveal-hand from reveal-library

**Files:**
- Modify: `crates/engine/src/parser/oracle_effect.rs:311-323` (reveal handler)
- Test: add tests in existing test module

- [ ] **Step 1: Write the tests**

Add to the test module in `crates/engine/src/parser/oracle_effect.rs`:

```rust
#[test]
fn effect_reveal_hand_parses_as_reveal_hand() {
    let e = parse_effect("reveal their hand");
    assert!(matches!(e, Effect::RevealHand { .. }));
}

#[test]
fn effect_reveal_top_cards_still_parses_as_dig() {
    let e = parse_effect("Reveal the top 3 cards of your library");
    assert!(matches!(e, Effect::Dig { count: 3, .. }));
}

#[test]
fn effect_reveal_your_hand_parses_as_reveal_hand() {
    let e = parse_effect("reveal your hand");
    assert!(matches!(e, Effect::RevealHand { .. }));
}
```

- [ ] **Step 2: Update the reveal handler**

Replace the `"reveal "` block (lines 311-323) with:

```rust
// --- Reveal ---
if lower.starts_with("reveal ") {
    // "reveal their/your hand" → RevealHand
    if lower.contains("hand") {
        return Effect::RevealHand {
            target: TargetFilter::Any,
        };
    }
    // "reveal the top N cards of your library" → Dig
    let count = if lower.contains("the top ") {
        let after_top = &lower[lower.find("the top ").unwrap() + 8..];
        parse_number(after_top).map(|(n, _)| n).unwrap_or(1)
    } else {
        1
    };
    return Effect::Dig {
        count,
        destination: None,
    };
}
```

- [ ] **Step 3: Run parser tests**

Run: `cargo test -p engine -- effect_reveal --nocapture`
Expected: All reveal tests pass (both new and existing).

- [ ] **Step 4: Commit**

```
feat: parser distinguishes reveal-hand from reveal-library
```

---

### Task 9: Fix the "choose" sentence parsing for reveal-hand cards

**Files:**
- Modify: `crates/engine/src/parser/oracle_effect.rs:100-160` (in `parse_imperative_effect`)

The oracle text "You choose an artifact or creature card from it" currently falls through to `Unimplemented("choose")`. For cards like Intimidation Tactics, this sentence is absorbed into the `RevealHand` effect — the filter (artifact or creature) should be set on the `RevealHand` effect, and the "choose" sentence should be a no-op pass-through since the `RevealChoice` UI already handles selection.

- [ ] **Step 1: Write a test**

```rust
#[test]
fn effect_chain_reveal_choose_exile() {
    let def = parse_effect_chain(
        "reveal their hand. You choose an artifact or creature card from it. Exile that card",
        AbilityKind::Spell,
    );
    // First effect should be RevealHand
    assert!(matches!(def.effect, Effect::RevealHand { .. }));
    // Should have sub_ability chain ending in ChangeZone to exile
    let sub = def.sub_ability.as_ref().expect("should have sub_ability");
    // The "choose" may be unimplemented or absorbed — the final sub should be exile
    fn find_exile(ability: &AbilityDefinition) -> bool {
        if matches!(ability.effect, Effect::ChangeZone { destination: Zone::Exile, .. }) {
            return true;
        }
        if let Some(ref sub) = ability.sub_ability {
            return find_exile(sub);
        }
        false
    }
    assert!(find_exile(sub), "Should have ChangeZone to Exile in sub-ability chain");
}
```

- [ ] **Step 2: Add "choose" handler that extracts type filter**

In `parse_imperative_effect`, add before the final `Unimplemented` fallback area, after existing effect handlers:

```rust
// --- Choose card from revealed hand (absorbed into RevealHand filter) ---
if lower.starts_with("choose ") && lower.contains("card from it") {
    // Extract type filter: "choose an artifact or creature card from it"
    let filter = parse_choose_filter(&lower);
    return Effect::RevealHand { target: filter };
}
```

Add helper function:

```rust
fn parse_choose_filter(lower: &str) -> TargetFilter {
    // Extract type info between "choose" and "card from it"
    let after_choose = lower.strip_prefix("choose ").unwrap_or(lower);
    let before_card = after_choose.split("card").next().unwrap_or("");
    let cleaned = before_card
        .trim()
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim_start_matches("a ");

    // Parse "artifact or creature", "nonland", etc.
    let parts: Vec<&str> = cleaned.split(" or ").collect();
    if parts.len() > 1 {
        let filters: Vec<TargetFilter> = parts
            .iter()
            .filter_map(|p| type_str_to_filter(p.trim()))
            .collect();
        if filters.len() > 1 {
            return TargetFilter::Or { filters };
        }
        if let Some(f) = filters.into_iter().next() {
            return f;
        }
    }
    if let Some(f) = type_str_to_filter(cleaned) {
        return f;
    }
    TargetFilter::Any
}

fn type_str_to_filter(s: &str) -> Option<TargetFilter> {
    let card_type = match s {
        "artifact" => Some(TypeFilter::Artifact),
        "creature" => Some(TypeFilter::Creature),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "land" => Some(TypeFilter::Land),
        _ if s.starts_with("non") => {
            return Some(TargetFilter::Typed {
                card_type: None,
                subtype: None,
                controller: None,
                properties: vec![FilterProp::NonType { value: s[3..].to_string() }],
            });
        }
        _ => None,
    };
    card_type.map(|ct| TargetFilter::Typed {
        card_type: Some(ct),
        subtype: None,
        controller: None,
        properties: vec![],
    })
}
```

- [ ] **Step 3: Run parser tests**

Run: `cargo test -p engine -- effect_chain_reveal --nocapture`
Expected: The chain test passes.

- [ ] **Step 4: Run all tests**

Run: `cargo test --all 2>&1 | tail -5`
Expected: All pass.

- [ ] **Step 5: Commit**

```
feat: parser handles "choose X card from it" as RevealHand filter
```

---

## Chunk 4: Frontend

### Task 10: Add `RevealChoice` to TypeScript types

**Files:**
- Modify: `client/src/adapter/types.ts:273` (add to WaitingFor union)

- [ ] **Step 1: Add type**

After the `SurveilChoice` line in the `WaitingFor` union, add:

```typescript
| { type: "RevealChoice"; data: { player: PlayerId; cards: ObjectId[]; filter: TargetFilter } }
```

Note: `TargetFilter` is already defined in `types.ts`. If not, use `unknown` for the filter field — the frontend doesn't need to evaluate the filter; the engine enforces it.

- [ ] **Step 2: Commit**

```
feat: add RevealChoice to frontend WaitingFor type
```

---

### Task 11: Add `RevealModal` to `CardChoiceModal`

**Files:**
- Modify: `client/src/components/modal/CardChoiceModal.tsx`

- [ ] **Step 1: Add type extraction and switch case**

Add type extraction:

```typescript
type RevealChoice = Extract<WaitingFor, { type: "RevealChoice" }>;
```

Add to the `switch` statement in `CardChoiceModal`:

```typescript
case "RevealChoice":
  if (waitingFor.data.player !== playerId) return null;
  return <RevealModal data={waitingFor.data} />;
```

- [ ] **Step 2: Implement `RevealModal`**

This follows the same pattern as `DigModal` — select exactly 1 card — but with different copy:

```tsx
function RevealModal({ data }: { data: RevealChoice["data"] }) {
  const dispatch = useGameDispatch();
  const objects = useGameStore((s) => s.gameState?.objects);
  const inspectObject = useUiStore((s) => s.inspectObject);
  const [selected, setSelected] = useState<ObjectId | null>(null);

  const handleConfirm = useCallback(() => {
    if (selected !== null) {
      dispatch({
        type: "SelectCards",
        data: { cards: [selected] },
      });
    }
  }, [dispatch, selected]);

  if (!objects) return null;

  return (
    <ChoiceOverlay
      title="Opponent's Hand"
      subtitle="Choose a card to exile"
    >
      <div className="mb-6 flex w-full max-w-5xl items-center justify-center gap-3 sm:mb-10">
        {data.cards.map((id, index) => {
          const obj = objects[id];
          if (!obj) return null;
          const isSelected = selected === id;
          return (
            <motion.button
              key={id}
              className={`relative rounded-lg transition ${
                isSelected
                  ? "z-10 ring-2 ring-emerald-400/80"
                  : "hover:shadow-[0_0_16px_rgba(200,200,255,0.3)]"
              }`}
              initial={{ opacity: 0, y: 60, scale: 0.85 }}
              animate={{ opacity: isSelected ? 1 : 0.7, y: 0, scale: 1 }}
              transition={{ delay: 0.1 + index * 0.08, duration: 0.35 }}
              whileHover={{ scale: 1.05, y: -6 }}
              onClick={() => setSelected(isSelected ? null : id)}
              onMouseEnter={() => inspectObject(id)}
              onMouseLeave={() => inspectObject(null)}
            >
              <CardImage cardName={obj.name} size="normal" />
            </motion.button>
          );
        })}
      </div>
      <button
        className={`rounded-lg px-8 py-3 text-lg font-bold transition ${
          selected !== null
            ? "bg-emerald-600 text-white shadow-lg hover:bg-emerald-500"
            : "cursor-not-allowed bg-gray-700 text-gray-500"
        }`}
        onClick={handleConfirm}
        disabled={selected === null}
      >
        Confirm
      </button>
    </ChoiceOverlay>
  );
}
```

- [ ] **Step 3: Run frontend type check**

Run: `cd client && pnpm run type-check 2>&1 | tail -10`
Expected: No type errors.

- [ ] **Step 4: Commit**

```
feat: add RevealModal to CardChoiceModal for reveal-hand mechanic
```

---

### Task 12: Update `OpponentHand` to show revealed card faces

**Files:**
- Modify: `client/src/components/hand/OpponentHand.tsx:44`

The `OpponentHand` component already has the `showCards` prop and the rendering logic at line 44:
```typescript
const obj = showCards && objects ? objects[id] : null;
```

With the `filter_state_for_player` change, revealed cards will have their real data (not "Hidden Card") in the state. The component already checks `obj` to decide face vs back. We just need to also check if the object exists and is NOT face_down:

- [ ] **Step 1: Update card rendering logic**

Change line 44 from:

```typescript
const obj = showCards && objects ? objects[id] : null;
```

to:

```typescript
const obj = objects ? objects[id] : null;
const showFace = showCards || (obj && !obj.face_down);
```

Then update line 61 to use `showFace` instead of `obj`:

```typescript
{showFace && obj ? (
```

- [ ] **Step 2: Run type check**

Run: `cd client && pnpm run type-check 2>&1 | tail -10`
Expected: No type errors.

- [ ] **Step 3: Commit**

```
feat: OpponentHand shows card faces for revealed (non-hidden) cards
```

---

## Chunk 5: Integration Verification

### Task 13: Verify end-to-end with `cargo test --all` and `pnpm run type-check`

- [ ] **Step 1: Run full Rust test suite**

Run: `cargo test --all 2>&1 | tail -10`
Expected: All tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -10`
Expected: No warnings.

- [ ] **Step 3: Run frontend checks**

Run: `cd client && pnpm run type-check && pnpm lint 2>&1 | tail -10`
Expected: No errors.

- [ ] **Step 4: Run coverage report**

Run: `cargo coverage 2>&1 | grep -i "reveal\|intimidation"`
Expected: Intimidation Tactics and other reveal-hand cards should no longer show as unsupported.

- [ ] **Step 5: Final commit if any cleanup needed**

```
chore: cleanup after RevealHand integration verification
```
