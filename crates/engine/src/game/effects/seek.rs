use rand::seq::SliceRandom;

use crate::game::effects::change_zone;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// Seek — MTG Arena Alchemy digital-only mechanic. No CR rule number applies.
/// Randomly pick card(s) from library matching filter, put to destination.
/// No reveal, no shuffle, no player choice. Analogous to a hidden-information
/// search (CR 701.23b) but randomized.
/// Seek is not a draw — no CardDrawn event, no draw-trigger interaction.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (filter, count_expr, destination, enter_tapped) = match &ability.effect {
        Effect::Seek {
            filter,
            count,
            destination,
            enter_tapped,
        } => (filter.clone(), count.clone(), *destination, *enter_tapped),
        _ => return Err(EffectError::InvalidParam("Expected Seek".to_string())),
    };

    let count = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as usize;

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // Collect library objects that match the filter.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = FilterContext::from_ability(ability);
    let mut matching: Vec<_> = player
        .library
        .iter()
        .filter(|&&obj_id| matches_target_filter(state, obj_id, &filter, &ctx))
        .copied()
        .collect();

    if matching.is_empty() || count == 0 {
        // "Fail to find" — resolve immediately
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Seek,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Randomly select from matching cards
    matching.shuffle(&mut state.rng);
    let pick_count = count.min(matching.len());

    for &card_id in &matching[..pick_count] {
        match destination {
            Zone::Battlefield => {
                // Route through replacement pipeline for ETB effects
                change_zone::execute_zone_move(
                    state,
                    card_id,
                    Zone::Library,
                    Zone::Battlefield,
                    ability.source_id,
                    None,
                    false,
                    enter_tapped,
                    None,
                    &[],
                    events,
                );
            }
            _ => {
                zones::move_to_zone(state, card_id, destination, events);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Seek,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_seek_ability(filter: TargetFilter, count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Seek {
                filter,
                count: QuantityExpr::Fixed {
                    value: count as i32,
                },
                destination: Zone::Hand,
                enter_tapped: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_seek_to_battlefield(filter: TargetFilter, tapped: bool) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Seek {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                destination: Zone::Battlefield,
                enter_tapped: tapped,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn add_library_creature(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
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
        card_id: u64,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    #[test]
    fn seek_finds_matching_card_moves_to_hand() {
        let mut state = GameState::new_two_player(42);
        let bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");
        let _land = add_library_land(&mut state, 2, PlayerId(0), "Forest");

        let ability = make_seek_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Card should be in hand, not library
        let player = &state.players[0];
        assert!(
            player.hand.contains(&bear),
            "Sought creature should be in hand"
        );
        assert!(
            !player.library.contains(&bear),
            "Sought creature should not be in library"
        );
    }

    #[test]
    fn seek_no_matches_resolves_cleanly() {
        let mut state = GameState::new_two_player(42);
        // Only lands in library, seeking creatures
        add_library_land(&mut state, 1, PlayerId(0), "Forest");

        let ability = make_seek_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Seek,
                ..
            }
        )));
    }

    #[test]
    fn seek_empty_library_resolves_cleanly() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_seek_ability(TargetFilter::Any, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Seek,
                ..
            }
        )));
    }

    #[test]
    fn seek_only_searches_controllers_library() {
        let mut state = GameState::new_two_player(42);
        let _opponent_creature = add_library_creature(&mut state, 1, PlayerId(1), "Opponent Bear");
        add_library_land(&mut state, 2, PlayerId(0), "Forest");

        let ability = make_seek_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should fail to find — opponent's library is not searched
        let player = &state.players[0];
        assert!(
            player.hand.is_empty(),
            "Should not find opponent's creature"
        );
    }

    #[test]
    fn seek_count_two_moves_two_cards() {
        let mut state = GameState::new_two_player(42);
        let bear1 = add_library_creature(&mut state, 1, PlayerId(0), "Bear 1");
        let bear2 = add_library_creature(&mut state, 2, PlayerId(0), "Bear 2");
        let _land = add_library_land(&mut state, 3, PlayerId(0), "Forest");

        let ability = make_seek_ability(TargetFilter::Typed(TypedFilter::creature()), 2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let player = &state.players[0];
        assert!(player.hand.contains(&bear1) && player.hand.contains(&bear2));
        assert_eq!(player.hand.len(), 2);
    }

    #[test]
    fn seek_to_battlefield_moves_card() {
        let mut state = GameState::new_two_player(42);
        let bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");

        let ability = make_seek_to_battlefield(TargetFilter::Typed(TypedFilter::creature()), false);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&bear).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(!obj.tapped);
    }

    #[test]
    fn seek_to_battlefield_tapped() {
        let mut state = GameState::new_two_player(42);
        let bear = add_library_creature(&mut state, 1, PlayerId(0), "Bear");

        let ability = make_seek_to_battlefield(TargetFilter::Typed(TypedFilter::creature()), true);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&bear).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert!(obj.tapped);
    }
}
