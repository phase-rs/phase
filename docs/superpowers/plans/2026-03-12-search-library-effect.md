# SearchLibrary Effect Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `Effect::SearchLibrary` so the engine can handle tutor cards ("search your library for X, put it into Y, then shuffle"), unlocking ~1,129 cards.

**Architecture:** New `Effect::SearchLibrary` variant + `WaitingFor::SearchChoice` follow the exact same pattern as the existing `RevealHand → RevealChoice → pending_continuation` flow. The resolver filters the controller's library by `TargetFilter`, presents legal choices via `WaitingFor::SearchChoice`, and the engine match arm runs the `pending_continuation` chain (ChangeZone to destination + Shuffle). A new `FilterProp::HasSupertype` is needed so the parser can express "basic land card."

**Tech Stack:** Rust engine only — `crates/engine/src/` (types, effects, game, parser). No frontend changes in this plan.

---

## File Structure

| Action | File | Responsibility |
|--------|------|----------------|
| Modify | `crates/engine/src/types/ability.rs` | Add `Effect::SearchLibrary`, `EffectKind::Search`, `FilterProp::HasSupertype` |
| Modify | `crates/engine/src/types/game_state.rs` | Add `WaitingFor::SearchChoice` variant |
| Create | `crates/engine/src/game/effects/search_library.rs` | Resolver: filter library → set WaitingFor::SearchChoice |
| Modify | `crates/engine/src/game/effects/mod.rs` | Wire dispatch + add to continuation-pause list |
| Modify | `crates/engine/src/game/engine.rs` | Handle `(SearchChoice, SelectCards)` → shuffle + continuation |
| Modify | `crates/engine/src/game/filter.rs` | Add `HasSupertype` match arm |
| Modify | `crates/engine/src/parser/oracle_effect.rs` | Replace search stub + shuffle stub with typed effects |

---

## Chunk 1: Types and Filter Infrastructure

### Task 1: Add `FilterProp::HasSupertype` to filter infrastructure

The existing `TargetFilter::Typed` uses `FilterProp` for property checks, but there's no way to filter by supertype (Basic, Legendary, Snow). "Search for a basic land card" is the #1 tutor pattern (~283 cards), so we need this.

**Files:**
- Modify: `crates/engine/src/types/ability.rs` (FilterProp enum, ~line 290)
- Modify: `crates/engine/src/game/filter.rs` (matches_filter_prop, ~line 143)

- [ ] **Step 1: Write failing test for HasSupertype filter**

In `crates/engine/src/game/filter.rs`, add to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn has_supertype_basic_matches_basic_land() {
    let mut state = setup();
    let id = add_creature(&mut state, PlayerId(0), "Plains");
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .supertypes
        .push(crate::types::card_type::Supertype::Basic);
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Land];

    let filter = TargetFilter::Typed {
        card_type: Some(TypeFilter::Land),
        subtype: None,
        controller: None,
        properties: vec![FilterProp::HasSupertype {
            value: "Basic".to_string(),
        }],
    };
    assert!(matches_target_filter(&state, id, &filter, id));
}

#[test]
fn has_supertype_basic_rejects_nonbasic_land() {
    let mut state = setup();
    let id = add_creature(&mut state, PlayerId(0), "Stomping Ground");
    state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Land];
    // No supertypes — nonbasic

    let filter = TargetFilter::Typed {
        card_type: Some(TypeFilter::Land),
        subtype: None,
        controller: None,
        properties: vec![FilterProp::HasSupertype {
            value: "Basic".to_string(),
        }],
    };
    assert!(!matches_target_filter(&state, id, &filter, id));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine -- has_supertype`
Expected: Compilation error — `FilterProp::HasSupertype` doesn't exist yet.

- [ ] **Step 3: Add `FilterProp::HasSupertype` variant**

In `crates/engine/src/types/ability.rs`, add to the `FilterProp` enum (after `Multicolored`):

```rust
/// Matches objects with a specific supertype (Basic, Legendary, Snow).
HasSupertype {
    value: String,
},
```

- [ ] **Step 4: Add match arm in `filter.rs`**

In `crates/engine/src/game/filter.rs`, add to `matches_filter_prop` (before the `FilterProp::Other` arm):

```rust
FilterProp::HasSupertype { value } => {
    let st: Result<crate::types::card_type::Supertype, _> = value.parse();
    match st {
        Ok(supertype) => obj.card_types.supertypes.contains(&supertype),
        Err(_) => true, // Unknown supertype — permissive
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine -- has_supertype`
Expected: Both tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/types/ability.rs crates/engine/src/game/filter.rs
git commit -m "feat: add FilterProp::HasSupertype for basic/legendary/snow filtering"
```

---

### Task 2: Add `Effect::SearchLibrary` and `EffectKind::Search`

**Files:**
- Modify: `crates/engine/src/types/ability.rs`

- [ ] **Step 1: Add `Effect::SearchLibrary` variant**

In `crates/engine/src/types/ability.rs`, add to the `Effect` enum (after `Shuffle`):

```rust
/// Search a player's library for card(s) matching a filter.
/// The destination is handled by the sub_ability chain (ChangeZone + Shuffle).
SearchLibrary {
    /// What cards can be found.
    filter: TargetFilter,
    /// How many cards to find (usually 1).
    #[serde(default = "default_one")]
    count: u32,
    /// Whether to reveal the found card(s) to all players.
    #[serde(default)]
    reveal: bool,
},
```

Note: Verify that `default_one` already exists as a serde default helper. If not, add it near the other default helpers:

```rust
fn default_one() -> u32 { 1 }
```

- [ ] **Step 2: Add display/kind match arms**

In the `Effect` impl block's display name match (the function that maps `Effect` → `&str`), add:

```rust
Effect::SearchLibrary { .. } => "SearchLibrary",
```

Add `SearchLibrary` to the `EffectKind` enum:

```rust
SearchLibrary,
```

And in the `From<&Effect> for EffectKind` impl:

```rust
Effect::SearchLibrary { .. } => EffectKind::SearchLibrary,
```

- [ ] **Step 3: Add `"SearchLibrary"` to the `is_supported_effect` list in `effects/mod.rs`**

In `crates/engine/src/game/effects/mod.rs`, add `"SearchLibrary"` to the `matches!` list in `is_supported_effect()`.

- [ ] **Step 4: Run clippy + tests to verify compilation**

Run: `cargo clippy -p engine --all-targets -- -D warnings && cargo test -p engine -- effect_kind`
Expected: PASS (no logic tests yet, just verifying the type additions compile cleanly).

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/types/ability.rs crates/engine/src/game/effects/mod.rs
git commit -m "feat: add Effect::SearchLibrary and EffectKind::SearchLibrary types"
```

---

### Task 3: Add `WaitingFor::SearchChoice`

**Files:**
- Modify: `crates/engine/src/types/game_state.rs`

- [ ] **Step 1: Add `WaitingFor::SearchChoice` variant**

In `crates/engine/src/types/game_state.rs`, add to the `WaitingFor` enum (after `RevealChoice`):

```rust
/// Player is choosing card(s) from a filtered library search.
SearchChoice {
    player: PlayerId,
    /// Object IDs of legal choices (pre-filtered from library).
    cards: Vec<ObjectId>,
    /// How many cards to select.
    count: usize,
},
```

- [ ] **Step 2: Add the `active_player()` mapping**

In the `active_player()` method on `GameState` (or wherever `WaitingFor` variants are matched to determine the active player), add:

```rust
WaitingFor::SearchChoice { player, .. } => Some(*player),
```

- [ ] **Step 3: Run compilation check**

Run: `cargo test -p engine --no-run`
Expected: Compilation succeeds. There may be non-exhaustive match warnings that we'll fix in later tasks.

- [ ] **Step 4: Commit**

```bash
git add crates/engine/src/types/game_state.rs
git commit -m "feat: add WaitingFor::SearchChoice for library search player choice"
```

---

## Chunk 2: Effect Resolver and Engine Handler

### Task 4: Create `search_library.rs` effect resolver

This is the core resolver. It filters the controller's library using `matches_target_filter`, collects legal choices, and sets `WaitingFor::SearchChoice`. If no cards match (or library is empty), it skips the choice and runs the continuation directly (MTG rule: you can "fail to find" with a non-public search).

**Files:**
- Create: `crates/engine/src/game/effects/search_library.rs`

- [ ] **Step 1: Write failing test for basic search resolver behavior**

Create `crates/engine/src/game/effects/search_library.rs` with tests first:

```rust
use crate::game::filter::matches_target_filter_controlled;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// Search controller's library for card(s) matching a filter.
///
/// Collects all library objects matching the filter, then sets
/// `WaitingFor::SearchChoice` for the player to pick. If no cards
/// match, resolves immediately (MTG "fail to find" rule).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TypeFilter;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use crate::types::card_type::CoreType;

    fn make_search_ability(filter: TargetFilter, count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count,
                reveal: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn add_library_creature(
        state: &mut GameState,
        card_id: u32,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(state, CardId(card_id), owner, name.to_string(), Zone::Library);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    fn add_library_land(
        state: &mut GameState,
        card_id: u32,
        owner: PlayerId,
        name: &str,
        basic: bool,
    ) -> ObjectId {
        let id = create_object(state, CardId(card_id), owner, name.to_string(), Zone::Library);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        if basic {
            obj.card_types
                .supertypes
                .push(crate::types::card_type::Supertype::Basic);
        }
        id
    }

    #[test]
    fn search_finds_matching_cards_sets_search_choice() {
        let mut state = GameState::new_two_player(42);
        let bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");
        let _land = add_library_land(&mut state, 2, PlayerId(0), "Forest", true);

        let ability = make_search_ability(
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            },
            1,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(cards.contains(&bear), "Should contain the creature");
                assert_eq!(cards.len(), 1, "Should NOT contain the land");
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn search_with_any_filter_shows_all_library_cards() {
        let mut state = GameState::new_two_player(42);
        let card1 = add_library_creature(&mut state, 1, PlayerId(0), "Bear");
        let card2 = add_library_land(&mut state, 2, PlayerId(0), "Forest", true);

        let ability = make_search_ability(TargetFilter::Any, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&card1));
                assert!(cards.contains(&card2));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    #[test]
    fn search_empty_library_resolves_immediately() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_search_ability(TargetFilter::Any, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should NOT set SearchChoice — fail to find
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::SearchLibrary,
                ..
            }
        )));
    }

    #[test]
    fn search_no_matches_resolves_immediately() {
        let mut state = GameState::new_two_player(42);
        // Only lands in library, searching for creatures
        add_library_land(&mut state, 1, PlayerId(0), "Forest", true);
        add_library_land(&mut state, 2, PlayerId(0), "Plains", true);

        let ability = make_search_ability(
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            },
            1,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn search_only_searches_controllers_library() {
        let mut state = GameState::new_two_player(42);
        let _opponent_creature =
            add_library_creature(&mut state, 1, PlayerId(1), "Opponent Bear");
        // Controller has no creatures
        add_library_land(&mut state, 2, PlayerId(0), "Forest", true);

        let ability = make_search_ability(
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            },
            1,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should fail to find — opponent's library is not searched
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine -- search_library`
Expected: FAIL — `resolve()` is `todo!()`.

- [ ] **Step 3: Implement the resolver**

Replace the `todo!()` body in `resolve()`:

```rust
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (filter, count) = match &ability.effect {
        Effect::SearchLibrary {
            filter, count, ..
        } => (filter.clone(), *count as usize),
        _ => (TargetFilter::Any, 1),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // Collect library objects that match the filter
    let matching: Vec<_> = player
        .library
        .iter()
        .filter(|&&obj_id| {
            matches_target_filter_controlled(
                state,
                obj_id,
                &filter,
                ability.source_id,
                ability.controller,
            )
        })
        .copied()
        .collect();

    if matching.is_empty() {
        // MTG "fail to find" — resolve immediately
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchLibrary,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let pick_count = count.min(matching.len());

    state.waiting_for = WaitingFor::SearchChoice {
        player: ability.controller,
        cards: matching,
        count: pick_count,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SearchLibrary,
        source_id: ability.source_id,
    });

    Ok(())
}
```

- [ ] **Step 4: Wire the resolver into `effects/mod.rs`**

In `crates/engine/src/game/effects/mod.rs`:

1. Add `pub mod search_library;` to the module declarations.
2. In `resolve_effect()`, add the dispatch arm:

```rust
Effect::SearchLibrary { .. } => search_library::resolve(state, ability, events),
```

3. In `resolve_ability_chain()`, add `WaitingFor::SearchChoice { .. }` to the continuation-pause match (around line 166):

```rust
WaitingFor::ScryChoice { .. }
    | WaitingFor::DigChoice { .. }
    | WaitingFor::SurveilChoice { .. }
    | WaitingFor::RevealChoice { .. }
    | WaitingFor::SearchChoice { .. }
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine -- search_library`
Expected: All 5 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/game/effects/search_library.rs crates/engine/src/game/effects/mod.rs
git commit -m "feat: add search_library effect resolver with library filtering"
```

---

### Task 5: Handle `(SearchChoice, SelectCards)` in `engine.rs`

This is the engine match arm that runs when the player makes their selection. It validates the choice, removes selected cards from the library, shuffles the library, and runs the `pending_continuation` with the chosen cards as targets.

**Files:**
- Modify: `crates/engine/src/game/engine.rs`

- [ ] **Step 1: Write integration test for search → select → continuation flow**

Add to the engine's test module (or a new `#[cfg(test)]` block at the bottom of `engine.rs` tests):

```rust
#[test]
fn search_library_select_card_runs_continuation() {
    use crate::types::ability::*;
    use crate::types::zones::Zone;

    let mut state = GameState::new_two_player(42);
    // Put a creature in P0's library
    let bear_id = crate::game::zones::create_object(
        &mut state,
        crate::types::identifiers::CardId(1),
        crate::types::player::PlayerId(0),
        "Bear".to_string(),
        Zone::Library,
    );
    state
        .objects
        .get_mut(&bear_id)
        .unwrap()
        .card_types
        .core_types
        .push(crate::types::card_type::CoreType::Creature);

    // Also add some other cards so we can verify shuffle
    for i in 2..=5 {
        crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(i),
            crate::types::player::PlayerId(0),
            format!("Filler {}", i),
            Zone::Library,
        );
    }
    let lib_size_before = state.players[0].library.len();

    // Simulate the engine being in SearchChoice state with a ChangeZone continuation
    state.waiting_for = WaitingFor::SearchChoice {
        player: crate::types::player::PlayerId(0),
        cards: vec![bear_id],
        count: 1,
    };
    // Continuation: put chosen card into hand, then shuffle
    let shuffle_sub = ResolvedAbility {
        effect: Effect::Shuffle {
            target: TargetFilter::Controller,
        },
        targets: vec![],
        source_id: crate::types::identifiers::ObjectId(100),
        controller: crate::types::player::PlayerId(0),
        sub_ability: None,
        duration: None,
    };
    state.pending_continuation = Some(Box::new(ResolvedAbility {
        effect: Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            target: TargetFilter::Any,
        },
        targets: vec![], // Will be filled by engine with chosen card
        source_id: crate::types::identifiers::ObjectId(100),
        controller: crate::types::player::PlayerId(0),
        sub_ability: Some(Box::new(shuffle_sub)),
        duration: None,
    }));

    let result = apply(
        &mut state,
        &GameAction::SelectCards {
            cards: vec![bear_id],
        },
    );
    assert!(result.is_ok());

    // Bear should now be in hand
    assert!(
        state.players[0].hand.contains(&bear_id),
        "Selected card should be in hand"
    );
    // Bear should NOT be in library
    assert!(
        !state.players[0].library.contains(&bear_id),
        "Selected card should be removed from library"
    );
    // Library should be one card shorter (the selected card moved to hand)
    assert_eq!(state.players[0].library.len(), lib_size_before - 1);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine -- search_library_select`
Expected: FAIL — no match arm for `(SearchChoice, SelectCards)`.

- [ ] **Step 3: Add the engine match arm**

In `crates/engine/src/game/engine.rs`, in the main `apply()` match block, add after the `RevealChoice` handler (around line 480):

```rust
// SearchChoice: player selects card(s) from filtered library search.
// Selected cards become targets for the pending_continuation (ChangeZone + Shuffle).
// The shuffle happens via the continuation chain, not here.
(
    WaitingFor::SearchChoice {
        player,
        cards: legal_cards,
        count,
    },
    GameAction::SelectCards { cards: chosen },
) => {
    let p = *player;
    let legal = legal_cards.clone();
    let expected_count = *count;

    // Validate selection count
    if chosen.len() != expected_count {
        return Err(EngineError::InvalidAction(format!(
            "Must select exactly {} card(s), got {}",
            expected_count,
            chosen.len()
        )));
    }

    // Validate all chosen cards are in legal set
    for &card_id in chosen {
        if !legal.contains(&card_id) {
            return Err(EngineError::InvalidAction(
                "Selected card not in search results".to_string(),
            ));
        }
    }

    // Run the pending continuation with chosen cards as targets
    if let Some(mut cont) = state.pending_continuation.take() {
        if chosen.len() == 1 {
            cont.targets = vec![crate::types::ability::TargetRef::Object(chosen[0])];
        } else {
            cont.targets = chosen
                .iter()
                .map(|&id| crate::types::ability::TargetRef::Object(id))
                .collect();
        }
        let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
        state.waiting_for.clone()
    } else {
        WaitingFor::Priority { player: p }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine -- search_library`
Expected: All tests PASS (both resolver tests and integration test).

- [ ] **Step 5: Run full engine test suite**

Run: `cargo test -p engine`
Expected: All existing tests still pass — no regressions.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/game/engine.rs
git commit -m "feat: handle SearchChoice player selection in engine apply()"
```

---

## Chunk 3: Parser and Shuffle Fix

### Task 6: Fix standalone shuffle parser (quick win — ~13 cards)

The parser currently emits `Unimplemented("shuffle")` for "Shuffle your library" text. The engine already has `Effect::Shuffle` and the resolver. This is pure parser wiring.

**Files:**
- Modify: `crates/engine/src/parser/oracle_effect.rs`

- [ ] **Step 1: Update the existing test expectation**

In `crates/engine/src/parser/oracle_effect.rs`, find the test `effect_shuffle_library` (~line 2438) and update:

```rust
#[test]
fn effect_shuffle_library() {
    let e = parse_effect("Shuffle your library");
    assert!(matches!(
        e,
        Effect::Shuffle {
            target: TargetFilter::Controller,
        }
    ));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p engine -- effect_shuffle_library`
Expected: FAIL — parser still emits `Unimplemented`.

- [ ] **Step 3: Fix the parser**

In `crates/engine/src/parser/oracle_effect.rs`, replace the shuffle block (~line 303-309):

```rust
// --- Shuffle ---
if lower.starts_with("shuffle") && lower.contains("library") {
    if lower == "shuffle your library" || lower == "shuffle your library." {
        return Effect::Shuffle {
            target: TargetFilter::Controller,
        };
    }
    if lower == "shuffle their library" || lower == "shuffle their library." {
        return Effect::Shuffle {
            target: TargetFilter::Player,
        };
    }
    // Compound shuffle patterns ("shuffle X into library") — not yet handled
    return Effect::Unimplemented {
        name: "shuffle".to_string(),
        description: Some(text.to_string()),
    };
}
```

- [ ] **Step 4: Add test for "shuffle their library"**

```rust
#[test]
fn effect_shuffle_their_library() {
    let e = parse_effect("Shuffle their library");
    assert!(matches!(
        e,
        Effect::Shuffle {
            target: TargetFilter::Player,
        }
    ));
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p engine -- effect_shuffle`
Expected: Both shuffle tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/parser/oracle_effect.rs
git commit -m "feat: parse 'shuffle your/their library' into Effect::Shuffle"
```

---

### Task 7: Parse search/tutor Oracle text into `Effect::SearchLibrary`

Replace the existing stub at line 258 that maps all search text to a generic `ChangeZone`. Parse the search filter ("basic land card", "creature card", etc.) and emit `Effect::SearchLibrary` with a proper `TargetFilter` + `sub_ability` chain for destination + shuffle.

**Files:**
- Modify: `crates/engine/src/parser/oracle_effect.rs`

- [ ] **Step 1: Write tests for the most common tutor patterns**

Add to the parser tests:

```rust
#[test]
fn parse_search_basic_land_to_hand() {
    // "Search your library for a basic land card, reveal it, put it into your hand, then shuffle"
    let e = parse_effect("Search your library for a basic land card, reveal it, put it into your hand, then shuffle your library");
    match e {
        Effect::SearchLibrary { filter, count, reveal } => {
            assert_eq!(count, 1);
            assert!(reveal);
            // Filter should match basic + land
            assert!(matches!(
                filter,
                TargetFilter::Typed {
                    card_type: Some(TypeFilter::Land),
                    ..
                }
            ));
        }
        other => panic!("Expected SearchLibrary, got {:?}", other),
    }
}

#[test]
fn parse_search_creature_to_hand() {
    let e = parse_effect("Search your library for a creature card, reveal it, put it into your hand, then shuffle your library");
    match e {
        Effect::SearchLibrary { filter, count, reveal } => {
            assert_eq!(count, 1);
            assert!(reveal);
            assert!(matches!(
                filter,
                TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }
            ));
        }
        other => panic!("Expected SearchLibrary, got {:?}", other),
    }
}

#[test]
fn parse_search_any_card_to_hand() {
    let e = parse_effect("Search your library for a card, put it into your hand, then shuffle your library");
    match e {
        Effect::SearchLibrary { filter, count, .. } => {
            assert_eq!(count, 1);
            assert!(matches!(filter, TargetFilter::Any));
        }
        other => panic!("Expected SearchLibrary, got {:?}", other),
    }
}

#[test]
fn parse_search_land_to_battlefield() {
    // Rampant Growth pattern
    let e = parse_effect("Search your library for a basic land card, put it onto the battlefield tapped, then shuffle your library");
    assert!(matches!(e, Effect::SearchLibrary { .. }));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p engine -- parse_search`
Expected: FAIL — parser emits `ChangeZone` instead of `SearchLibrary`.

- [ ] **Step 3: Implement the search parser**

Replace the search library block in `oracle_effect.rs` (~line 258-265) with:

```rust
// --- Search library ---
if lower.starts_with("search your library") || lower.starts_with("search their library") {
    return parse_search_library(text, lower.as_str());
}
```

Then add this helper function (place it near the other helper functions in the file):

```rust
/// Parse "search your library for X" Oracle text into Effect::SearchLibrary.
///
/// Extracts the card filter from the "for a/an <type> card" clause,
/// detects reveal, and identifies the destination from "put it into/onto" text.
/// The destination and shuffle are encoded as sub_ability chain on the
/// AbilityDefinition level, but at the Effect level we only store the filter.
fn parse_search_library(text: &str, lower: &str) -> Effect {
    // Extract what we're searching for: "for a <type> card" or "for a card"
    let filter = if let Some(for_idx) = lower.find("for a ") {
        let after_for = &lower[for_idx + 6..]; // skip "for a "
        parse_search_filter(after_for)
    } else if let Some(for_idx) = lower.find("for an ") {
        let after_for = &lower[for_idx + 7..]; // skip "for an "
        parse_search_filter(after_for)
    } else {
        TargetFilter::Any
    };

    let reveal = lower.contains("reveal");
    let count = if lower.contains("up to two") {
        2
    } else if lower.contains("up to three") {
        3
    } else {
        1
    };

    Effect::SearchLibrary {
        filter,
        count,
        reveal,
    }
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
fn parse_search_filter(text: &str) -> TargetFilter {
    // Find the end of the type description (before comma, period, or "and put")
    let type_end = text
        .find(',')
        .or_else(|| text.find('.'))
        .or_else(|| text.find(" and put"))
        .or_else(|| text.find(" and shuffle"))
        .unwrap_or(text.len());
    let type_text = text[..type_end].trim();

    // Strip trailing "card" or "cards"
    let type_text = type_text
        .strip_suffix(" cards")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim();

    // Check for "a card" / "card" alone (Demonic Tutor pattern)
    if type_text == "card" || type_text.is_empty() {
        return TargetFilter::Any;
    }

    // Check for "basic land" pattern
    let is_basic = type_text.contains("basic");
    let clean = type_text.replace("basic ", "");

    // Map type name to TypeFilter
    let card_type = match clean.trim() {
        "land" => Some(TypeFilter::Land),
        "creature" => Some(TypeFilter::Creature),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "instant or sorcery" => {
            // Return an Or filter for dual-type searches
            let mut properties = vec![];
            if is_basic {
                properties.push(FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                });
            }
            return TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Instant),
                        subtype: None,
                        controller: None,
                        properties: properties.clone(),
                    },
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Sorcery),
                        subtype: None,
                        controller: None,
                        properties,
                    },
                ],
            };
        }
        other => {
            // Could be a subtype search: "forest card", "plains card", "equipment card"
            // Check against known land subtypes and artifact subtypes
            let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
            if land_subtypes.contains(&other) {
                let mut properties = vec![];
                if is_basic {
                    properties.push(FilterProp::HasSupertype {
                        value: "Basic".to_string(),
                    });
                }
                return TargetFilter::Typed {
                    card_type: Some(TypeFilter::Land),
                    subtype: Some(capitalize(other)),
                    controller: None,
                    properties,
                };
            }
            if other == "equipment" {
                return TargetFilter::Typed {
                    card_type: Some(TypeFilter::Artifact),
                    subtype: Some("Equipment".to_string()),
                    controller: None,
                    properties: vec![],
                };
            }
            if other == "aura" {
                return TargetFilter::Typed {
                    card_type: Some(TypeFilter::Enchantment),
                    subtype: Some("Aura".to_string()),
                    controller: None,
                    properties: vec![],
                };
            }
            // Fallback: treat as Any with a description note
            return TargetFilter::Any;
        }
    };

    let mut properties = vec![];
    if is_basic {
        properties.push(FilterProp::HasSupertype {
            value: "Basic".to_string(),
        });
    }

    TargetFilter::Typed {
        card_type,
        subtype: None,
        controller: None,
        properties,
    }
}

/// Capitalize the first letter of a string (for subtype names).
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}
```

Note: Check whether a `capitalize` helper already exists in the parser. If so, reuse it. If a similar helper exists with a different name, use that instead.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p engine -- parse_search`
Expected: All 4 search parser tests PASS.

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p engine`
Expected: All tests PASS. The existing search stub test (if any) may need updating — check for any test that asserts `ChangeZone` for search text.

- [ ] **Step 6: Commit**

```bash
git add crates/engine/src/parser/oracle_effect.rs
git commit -m "feat: parse 'search your library for X' into Effect::SearchLibrary"
```

---

### Task 8: Wire sub_ability chain for search destination + shuffle in the parser

The parser emits `Effect::SearchLibrary` with the filter, but the ability definition also needs `sub_ability` to chain the destination (ChangeZone → Hand/Battlefield) and shuffle. This happens at the `AbilityDefinition` level, not the `Effect` level.

Check how the parser currently builds `AbilityDefinition` from parsed effects — specifically where `sub_ability` is constructed. The search Oracle text like "search your library for a card, put it into your hand, then shuffle" needs to produce:

```
AbilityDefinition {
    effect: SearchLibrary { filter, count, reveal },
    sub_ability: Some(AbilityDefinition {
        effect: ChangeZone { origin: Library, destination: Hand },
        sub_ability: Some(AbilityDefinition {
            effect: Shuffle { target: Controller },
            sub_ability: None,
        }),
    }),
}
```

**Files:**
- Modify: `crates/engine/src/parser/oracle_effect.rs` or the parent parser module that builds `AbilityDefinition`

- [ ] **Step 1: Investigate how AbilityDefinition.sub_ability is currently built**

Find where the parser constructs `AbilityDefinition` from parsed `Effect` values. Search for `sub_ability:` assignments in the parser module. The search effect parser may need to return a richer structure than just `Effect`, or the parent parser needs to detect `SearchLibrary` and attach the appropriate sub_ability chain.

- [ ] **Step 2: Write integration test for end-to-end ability parsing**

This test should parse a full Oracle text line and verify the `AbilityDefinition` has the correct sub_ability chain. The exact test location depends on where `AbilityDefinition` parsing tests live (likely in the parser module's test block).

```rust
#[test]
fn search_ability_has_shuffle_sub_ability() {
    // Parse a full ability definition for a tutor card
    // Verify: SearchLibrary → ChangeZone(Hand) → Shuffle chain
    let text = "Search your library for a creature card, reveal it, put it into your hand, then shuffle your library";
    // ... parse into AbilityDefinition ...
    // assert sub_ability chain is SearchLibrary -> ChangeZone -> Shuffle
}
```

- [ ] **Step 3: Implement sub_ability chaining for search effects**

The implementation depends on the parser's architecture (discovered in step 1). The key invariant: every `SearchLibrary` effect MUST have a `Shuffle` at the end of its sub_ability chain. The destination (`Hand`, `Battlefield`, etc.) is parsed from the "put it into/onto" clause.

Destination detection (add to `parse_search_library` or its caller):
- "put it into your hand" / "put them into your hand" → `Zone::Hand`
- "put it onto the battlefield" / "put them onto the battlefield" → `Zone::Battlefield`
- "put it onto the battlefield tapped" → `Zone::Battlefield` (tapped flag on ChangeZone or handled by replacement)
- "put it on top of your library" → `Zone::Library` (top position — may need special handling)
- Default (no explicit destination, e.g. some tutor shorthand) → `Zone::Hand`

- [ ] **Step 4: Run tests**

Run: `cargo test -p engine`
Expected: All tests PASS including the new integration test.

- [ ] **Step 5: Commit**

```bash
git add crates/engine/src/parser/
git commit -m "feat: wire sub_ability chain (ChangeZone + Shuffle) for search effects"
```

---

## Chunk 4: Validation

### Task 9: Run full suite + clippy

- [ ] **Step 1: Run clippy strict**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: No warnings.

- [ ] **Step 2: Run fmt check**

Run: `cargo fmt --all -- --check`
Expected: No formatting issues.

- [ ] **Step 3: Run full test suite**

Run: `cargo test --all`
Expected: All tests PASS.

- [ ] **Step 4: Spot-check card coverage improvement**

Run: `cargo coverage`

Look at the coverage report — the number of cards with `Unimplemented("shuffle")` should drop to near zero, and a substantial number of formerly-unimplemented search cards should now have `SearchLibrary` effects.

- [ ] **Step 5: Commit any final fixes**

If clippy or tests found issues, fix and commit.
