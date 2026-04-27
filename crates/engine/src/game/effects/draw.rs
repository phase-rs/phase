use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::static_abilities::prohibition_scope_matches_player;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

pub(crate) fn allowed_draw_count(
    state: &GameState,
    player_id: crate::types::player::PlayerId,
    count: u32,
) -> u32 {
    let Some(player) = state.players.iter().find(|p| p.id == player_id) else {
        return 0;
    };

    let mut allowed = count;
    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the phased-out /
    // command-zone / condition gate.
    for (source_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        let source_id = source_obj.id;

        {
            match def.mode {
                StaticMode::CantDraw { ref who }
                    if prohibition_scope_matches_player(who, player_id, source_id, state) =>
                {
                    return 0;
                }
                StaticMode::PerTurnDrawLimit { ref who, max }
                    if prohibition_scope_matches_player(who, player_id, source_id, state) =>
                {
                    let remaining = max.saturating_sub(player.cards_drawn_this_turn);
                    allowed = allowed.min(remaining);
                }
                _ => {}
            }
        }
    }

    allowed
}

/// CR 121.1: Draw a card — put the top card of library into hand.
///
/// CR 601.2c + CR 115.1: When the parsed `Effect::Draw { target }` is a
/// player-target filter (e.g. `TargetFilter::Player` from "Target player draws
/// a card"), the drawing player is whichever `TargetRef::Player` was chosen
/// during spell announcement. `ResolvedAbility::target_player()` extracts
/// that choice and falls back to `ability.controller` when the target is a
/// context-ref (Controller, SelfRef, etc.) — preserving the historical
/// "controller draws" behavior for plain "draw a card" / "you draw" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_cards, drawing_player) = match &ability.effect {
        // CR 107.1b: Resolve with full ability context so `QuantityRef::Variable { "X" }`
        // finds the caster-chosen X on the ability.
        // CR 601.2c: For `target: TargetFilter::Player`, the drawing player was
        // chosen during spell announcement and is in `ability.targets` —
        // `target_player()` reads it back, falling back to controller for
        // context-ref filters that don't surface a target slot.
        // CR 608.2d: `up_to` is honored at runtime via the player choice
        // mechanism in `engine_resolution_choices` — by the time we reach
        // here the engine has already resolved the chosen count into the
        // ProposedEvent::Draw count. So the resolver here just reads the
        // configured count expression as the upper bound.
        Effect::Draw {
            count,
            target,
            up_to: _,
        } => (
            resolve_quantity_with_targets(state, count, ability) as u32,
            // CR 121.1 + CR 615.5 + CR 609.7: context-ref target filters
            // (PostReplacementSourceController, ParentTargetController, etc.)
            // resolve via state slots — falling straight to `ability.controller`
            // would draw cards for the wrong player on prevention follow-ups
            // like Swans of Bryn Argoll.
            super::resolve_player_for_context_ref(state, ability, target),
        ),
        _ => (1, ability.controller),
    };

    let proposed = ProposedEvent::Draw {
        player_id: drawing_player,
        count: num_cards,
        applied: HashSet::new(),
    };

    // CR 614.1a: Route draw through replacement pipeline (e.g. Dredge, Abundance).
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_draw_after_replacement(state, event, events);
        }
        ReplacementResult::Prevented => {
            // Draw was prevented, skip
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 121.1: Apply a post-replacement `ProposedEvent::Draw` to the game state.
///
/// Extracted from `resolve`'s Execute arm so the same logic can be invoked by
/// `handle_replacement_choice` when a player accepts a draw-replacement choice.
/// Caller is responsible for emitting `EffectResolved`.
pub fn apply_draw_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) {
    let ProposedEvent::Draw {
        player_id, count, ..
    } = event
    else {
        debug_assert!(
            false,
            "apply_draw_after_replacement called with non-Draw ProposedEvent"
        );
        return;
    };

    let allowed_count = allowed_draw_count(state, player_id, count);
    let Some(player) = state.players.iter().find(|p| p.id == player_id) else {
        return;
    };

    let cards_to_draw: Vec<_> = player
        .library
        .iter()
        .take(allowed_count as usize)
        .copied()
        .collect();

    // CR 704.5b: If library has fewer cards than requested, mark the player.
    // CR 121.4: Partial draws are legal — draw what's available.
    if allowed_count > 0 && cards_to_draw.len() < allowed_count as usize {
        if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
            player.drew_from_empty_library = true;
        }
    }

    // CR 609.3: Record the actually-drawn count so chained sub-abilities like
    // "draw cards equal to N, then discard that many" can resolve their
    // dynamic count via `EventContextAmount`'s `last_effect_count` fallback.
    // Mirrors the convention used by `change_zone.rs`, `sacrifice.rs`, etc.
    let drawn_count = cards_to_draw.len() as i32;
    state.last_effect_count = Some(drawn_count);

    for obj_id in cards_to_draw {
        zones::move_to_zone(state, obj_id, Zone::Hand, events);
        // CR 121.1 + CR 504.1: Increment per-step + per-turn counters BEFORE
        // emitting the event so the ordinal embedded in `CardDrawn` reflects
        // this draw (1-indexed). Triggers/replacements that gate on "first
        // draw of the draw step" read this ordinal.
        let nth_in_step = if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id)
        {
            player.cards_drawn_this_turn = player.cards_drawn_this_turn.saturating_add(1);
            player.cards_drawn_this_step = player.cards_drawn_this_step.saturating_add(1);
            player.cards_drawn_this_step
        } else {
            1
        };
        events.push(GameEvent::CardDrawn {
            player_id,
            object_id: obj_id,
            nth_in_step,
        });
        record_first_draw_and_enqueue_miracle(state, player_id, obj_id);
    }
}

/// CR 702.94a + CR 603.11: Shared first-draw hook — record the drawn
/// `ObjectId` as `player`'s first-of-turn if absent, and if the drawn card has
/// `Keyword::Miracle(cost)`, enqueue a `MiracleOffer` for the priority-entry
/// flush to surface as `WaitingFor::MiracleReveal`. Subsequent draws do NOT
/// overwrite the first-draw entry and do NOT enqueue more offers (the static
/// ability only functions for the first-drawn card per CR 702.94a).
pub(crate) fn record_first_draw_and_enqueue_miracle(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    object_id: crate::types::identifiers::ObjectId,
) {
    // Only the FIRST draw of the turn per player establishes the miracle
    // eligibility condition. `or_insert_with` returns a `&mut V` indicating
    // whether the entry was freshly set; compare against `object_id` to know.
    let is_first = !state.first_card_drawn_this_turn.contains_key(&player);
    state
        .first_card_drawn_this_turn
        .entry(player)
        .or_insert(object_id);
    if !is_first {
        return;
    }
    // CR 702.94a: Static ability functions from hand — check the drawn object's
    // keywords. Reads the concrete `Keyword::Miracle(ManaCost)` payload; if
    // layers remove miracle before draw resolution the offer simply never
    // queues. `obj.keywords` is the printed+granted keyword set visible on the
    // object while it's in the owner's hand.
    let Some(obj) = state.objects.get(&object_id) else {
        return;
    };
    if obj.owner != player {
        return;
    }
    let miracle_cost = obj.keywords.iter().find_map(|k| match k {
        crate::types::keywords::Keyword::Miracle(cost) => Some(cost.clone()),
        _ => None,
    });
    let Some(cost) = miracle_cost else {
        return;
    };
    state
        .pending_miracle_offers
        .push(crate::types::game_state::MiracleOffer {
            player,
            object_id,
            cost,
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, StaticDefinition};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::statics::ProhibitionScope;

    fn make_ability(num_cards: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
                target: crate::types::ability::TargetFilter::Controller,
                up_to: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn draw_moves_top_card_to_hand() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&card_id));
        assert!(!state.players[0].library.contains(&card_id));
    }

    #[test]
    fn draw_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(2);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&c1));
        assert!(state.players[0].hand.contains(&c2));
    }

    #[test]
    fn draw_emits_card_drawn_and_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CardDrawn { .. })));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Draw,
                ..
            }
        )));
    }

    #[test]
    fn draw_from_empty_library_sets_flag() {
        let mut state = GameState::new_two_player(42);
        // Library is empty — drawing should set the flag
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.players[0].drew_from_empty_library,
            "Drawing from empty library should set flag"
        );
    }

    #[test]
    fn partial_draw_sets_flag() {
        let mut state = GameState::new_two_player(42);
        // Library has 1 card, but we draw 3 — partial draw, flag should be set
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(3);
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have drawn the 1 card available
        assert_eq!(state.players[0].hand.len(), 1);
        // But flag should be set because library couldn't fulfill the full draw
        assert!(
            state.players[0].drew_from_empty_library,
            "Partial draw should set flag"
        );
    }

    #[test]
    fn normal_draw_does_not_set_flag() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.players[0].drew_from_empty_library,
            "Normal draw should not set flag"
        );
    }

    #[test]
    fn cant_draw_blocks_all_draws_for_affected_player() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Omen Machine".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantDraw {
                who: ProhibitionScope::AllPlayers,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert!(state.players[0].hand.is_empty());
        assert_eq!(state.players[0].library.len(), 1);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::CardDrawn { .. })));
    }

    #[test]
    fn cant_draw_opponents_only_does_not_block_controller() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Narset".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantDraw {
                who: ProhibitionScope::Opponents,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn per_turn_draw_limit_allows_partial_multi_card_draw() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Spirit of the Labyrinth".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::AllPlayers,
                max: 1,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].cards_drawn_this_turn, 1);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::CardDrawn { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn per_turn_draw_limit_ignores_unaffected_player() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Narset".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::PerTurnDrawLimit {
                who: ProhibitionScope::Opponents,
                max: 1,
            }));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].cards_drawn_this_turn, 1);
    }

    /// CR 702.94a + CR 603.11: First card drawn per turn is recorded so the
    /// miracle reveal prompt can gate eligibility. Subsequent draws do NOT
    /// overwrite the recorded ObjectId.
    #[test]
    fn first_card_drawn_this_turn_records_only_the_first() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let _second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );

        // Pre-condition: no first-draw recorded yet.
        assert!(!state.first_card_drawn_this_turn.contains_key(&PlayerId(0)));

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        // Post-condition: only the first drawn object is recorded.
        assert_eq!(
            state.first_card_drawn_this_turn.get(&PlayerId(0)),
            Some(&first),
            "first_card_drawn_this_turn should record the first drawn ObjectId and not overwrite",
        );
    }

    /// CR 702.94a: A second resolve() call in the same turn does NOT update
    /// the recorded first-drawn ObjectId — the entry is set on the very first
    /// draw of the turn and stable until the turn reset clears it.
    #[test]
    fn first_card_drawn_this_turn_stable_across_draw_calls() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let _second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();
        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert_eq!(
            state.first_card_drawn_this_turn.get(&PlayerId(0)),
            Some(&first),
            "second draw this turn must not overwrite the first-draw entry",
        );
    }

    /// CR 702.94a + CR 603.11: A card with Miracle drawn as the first card of
    /// the turn queues a `MiracleOffer` with the keyword's mana cost. A second
    /// draw of another miracle card in the same resolution does NOT queue a
    /// second offer (CR 702.94a only honors the first-drawn card).
    #[test]
    fn miracle_first_draw_queues_offer() {
        use crate::types::mana::{ManaCost, ManaCostShard};
        let mut state = GameState::new_two_player(42);
        // Put two miracle-tagged cards on the library top.
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "MiracleOne".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "MiracleTwo".to_string(),
            Zone::Library,
        );
        // Attach Keyword::Miracle({W}) to each.
        for obj_id in [first, second] {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.keywords
                .push(crate::types::keywords::Keyword::Miracle(ManaCost::Cost {
                    shards: vec![ManaCostShard::White],
                    generic: 0,
                }));
            obj.base_keywords = obj.keywords.clone();
        }

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        // Only the first drawn card queues a miracle offer.
        assert_eq!(
            state.pending_miracle_offers.len(),
            1,
            "only the first drawn card should queue a miracle offer"
        );
        let offer = &state.pending_miracle_offers[0];
        assert_eq!(offer.player, PlayerId(0));
        assert_eq!(offer.object_id, first);
    }

    /// CR 702.94a: A card without Miracle as the first-drawn card does NOT
    /// queue an offer, even if later drawn cards have Miracle.
    #[test]
    fn miracle_non_first_draw_does_not_queue_offer() {
        use crate::types::mana::{ManaCost, ManaCostShard};
        let mut state = GameState::new_two_player(42);
        // First card: no miracle. Second card: miracle.
        let _first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mundane".to_string(),
            Zone::Library,
        );
        let miracle_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "MiracleCard".to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&miracle_card).unwrap();
        obj.keywords
            .push(crate::types::keywords::Keyword::Miracle(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            }));
        obj.base_keywords = obj.keywords.clone();

        let mut events = Vec::new();
        resolve(&mut state, &make_ability(2), &mut events).unwrap();

        assert!(
            state.pending_miracle_offers.is_empty(),
            "non-first-drawn miracle card must not queue an offer"
        );
    }
}
