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
    let (filter, count) = match &ability.effect {
        Effect::SearchLibrary { filter, count, .. } => (filter.clone(), *count as usize),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TypeFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

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
        basic: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
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
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not set SearchChoice for empty library"
        );
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

        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not set SearchChoice when no cards match"
        );
    }

    #[test]
    fn search_only_searches_controllers_library() {
        let mut state = GameState::new_two_player(42);
        let _opponent_creature = add_library_creature(&mut state, 1, PlayerId(1), "Opponent Bear");
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
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not search opponent's library"
        );
    }
}
