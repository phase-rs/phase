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
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_cards = match &ability.effect {
        // CR 107.1b: Resolve with full ability context so `QuantityRef::Variable { "X" }`
        // finds the caster-chosen X on the ability.
        Effect::Draw { count } => resolve_quantity_with_targets(state, count, ability) as u32,
        _ => 1,
    };

    let proposed = ProposedEvent::Draw {
        player_id: ability.controller,
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

    for obj_id in cards_to_draw {
        zones::move_to_zone(state, obj_id, Zone::Hand, events);
        events.push(GameEvent::CardDrawn {
            player_id,
            object_id: obj_id,
        });
        if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
            player.cards_drawn_this_turn = player.cards_drawn_this_turn.saturating_add(1);
        }
    }
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
}
