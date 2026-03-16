use crate::game::combat::has_summoning_sickness;
use crate::game::coverage::unimplemented_mechanics;
use crate::game::devotion::count_devotion;
use crate::game::mana_sources::display_land_mana_colors;
use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::types::ability::StaticCondition;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;

/// Compute display-only derived fields on game state before serialization.
///
/// This must be called by any consumer (WASM, Tauri, server) before
/// serializing the state to the frontend. It sets:
/// - `GameObject::unimplemented_mechanics`
/// - `GameObject::has_summoning_sickness`
/// - `GameObject::devotion` (for Theros gods pattern)
/// - `Player::can_look_at_top_of_library`
pub fn derive_display_state(state: &mut GameState) {
    let turn = state.turn_number;

    for obj in state.objects.values_mut() {
        obj.unimplemented_mechanics = unimplemented_mechanics(obj);
        obj.has_summoning_sickness = has_summoning_sickness(obj, turn);
        obj.available_mana_colors.clear();
    }

    // Compute per-card devotion for cards with DevotionGE conditions
    // (Theros gods pattern — derive colors from the card's own base_color)
    let devotion_cards: Vec<_> = state
        .objects
        .iter()
        .filter_map(|(&id, obj)| {
            let has_devotion_static = obj
                .static_definitions
                .iter()
                .any(|def| matches!(&def.condition, Some(StaticCondition::DevotionGE { .. })));
            if has_devotion_static && !obj.base_color.is_empty() {
                let devotion = count_devotion(state, obj.controller, &obj.base_color);
                Some((id, devotion))
            } else {
                None
            }
        })
        .collect();
    for (id, devotion) in devotion_cards {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.devotion = Some(devotion);
        }
    }

    // Compute dynamic land frame colors from currently available mana options.
    let mana_color_cards: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = state.objects.get(&id)?;
            if !obj.card_types.core_types.contains(&CoreType::Land) {
                return None;
            }
            let colors = display_land_mana_colors(state, id, obj.controller);
            Some((id, colors))
        })
        .collect();
    for (id, colors) in mana_color_cards {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.available_mana_colors = colors;
        }
    }

    // Compute per-player derived fields
    let peek_flags: Vec<bool> = state
        .players
        .iter()
        .map(|p| {
            let ctx = StaticCheckContext {
                player_id: Some(p.id),
                ..Default::default()
            };
            check_static_ability(state, "MayLookAtTopOfLibrary", &ctx)
        })
        .collect();
    for (i, flag) in peek_flags.into_iter().enumerate() {
        state.players[i].can_look_at_top_of_library = flag;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn derive_sets_summoning_sickness_for_new_creature() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(1);

        derive_display_state(&mut state);

        assert!(state.objects[&id].has_summoning_sickness);
    }

    #[test]
    fn derive_clears_summoning_sickness_for_old_creature() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 3;
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        state.objects.get_mut(&id).unwrap().entered_battlefield_turn = Some(1);

        derive_display_state(&mut state);

        assert!(!state.objects[&id].has_summoning_sickness);
    }

    #[test]
    fn derive_sets_unimplemented_flag() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test".to_string(),
            Zone::Battlefield,
        );

        derive_display_state(&mut state);

        // Should have set the flag (false for a card with no mechanics)
        let obj = &state.objects[&id];
        assert!(obj.unimplemented_mechanics.is_empty());
    }

    #[test]
    fn derive_sets_can_look_at_top_default_false() {
        let mut state = GameState::new_two_player(42);

        derive_display_state(&mut state);

        assert!(!state.players[0].can_look_at_top_of_library);
        assert!(!state.players[1].can_look_at_top_of_library);
    }
}
