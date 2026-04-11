use crate::game::filter::matches_target_filter_controlled;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.23a + CR 401.2: Search a library — look through it, find card(s) matching criteria, then shuffle.
/// CR 401.2: Libraries are normally face-down; searching is an exception that lets a player look through cards.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (filter, count, reveal, has_target_player) = match &ability.effect {
        Effect::SearchLibrary {
            filter,
            count,
            reveal,
            target_player,
        } => (
            filter.clone(),
            *count as usize,
            *reveal,
            target_player.is_some(),
        ),
        _ => (TargetFilter::Any, 1, false, false),
    };

    // CR 701.23a: When target_player is set, search that player's library.
    // The target player is resolved from ability.targets (player targets).
    let search_player_id = if has_target_player {
        ability
            .targets
            .iter()
            .find_map(|t| {
                if let TargetRef::Player(pid) = t {
                    Some(*pid)
                } else {
                    None
                }
            })
            .unwrap_or(ability.controller)
    } else {
        ability.controller
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == search_player_id)
        .ok_or(EffectError::PlayerNotFound)?;
    events.push(GameEvent::PlayerPerformedAction {
        player_id: ability.controller,
        action: PlayerActionKind::SearchedLibrary,
    });
    state
        .players_who_searched_library_this_turn
        .insert(ability.controller);

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
        // CR 701.23b: A player searching a hidden zone isn't required to find
        // cards even if they're present ("fail to find"). Resolve immediately.
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
        reveal,
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
    use crate::types::ability::TypedFilter;
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
                target_player: None,
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

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::SearchedLibrary,
            } if *player_id == PlayerId(0)
        )));

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                reveal,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(!reveal);
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
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::SearchedLibrary,
            } if *player_id == PlayerId(0)
        )));
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

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
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

        let ability = make_search_ability(TargetFilter::Typed(TypedFilter::creature()), 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should fail to find — opponent's library is not searched
        assert!(
            !matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Should not search opponent's library"
        );
    }

    #[test]
    fn search_with_reveal_sets_reveal_flag() {
        let mut state = GameState::new_two_player(42);
        add_library_creature(&mut state, 1, PlayerId(0), "Bear");

        let ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: 1,
                reveal: true,
                target_player: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { reveal, .. } => {
                assert!(*reveal);
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }
}
