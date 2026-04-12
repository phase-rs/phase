use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
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
    // CR 107.3a + CR 601.2b: Resolve the count expression against the ability so
    // `Variable("X")` picks up the caster's announced X. Fixed counts are unaffected.
    let (filter, count, reveal, has_target_player) = match &ability.effect {
        Effect::SearchLibrary {
            filter,
            count,
            reveal,
            target_player,
        } => (
            filter.clone(),
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
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

    // CR 107.3a + CR 601.2b: Evaluate the filter with the resolving ability
    // in scope so dynamic thresholds (e.g. `CmcLE { value: Variable("X") }`
    // for Nature's Rhythm) resolve against the caster's announced X.
    let filter_ctx = FilterContext::from_ability(ability);
    let matching: Vec<_> = player
        .library
        .iter()
        .filter(|&&obj_id| matches_target_filter(state, obj_id, &filter, &filter_ctx))
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
    use crate::types::ability::{QuantityExpr, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_search_ability(filter: TargetFilter, count: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: count },
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
                count: QuantityExpr::Fixed { value: 1 },
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

    fn add_library_creature_with_cmc(
        state: &mut GameState,
        card_id: u64,
        owner: PlayerId,
        name: &str,
        cmc: u32,
    ) -> ObjectId {
        use crate::types::mana::ManaCost;
        let id = create_object(
            state,
            CardId(card_id),
            owner,
            name.to_string(),
            Zone::Library,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.mana_cost = ManaCost::generic(cmc);
        id
    }

    /// CR 107.3a + CR 601.2b: Nature's Rhythm — search for a creature card with mana
    /// value X or less. With X=4, only CMC-≤-4 creatures should be selectable,
    /// regardless of what's in the library.
    #[test]
    fn natures_rhythm_x_mana_value_restricts_search_targets() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef};
        let mut state = GameState::new_two_player(42);
        let cmc2 = add_library_creature_with_cmc(&mut state, 1, PlayerId(0), "Small", 2);
        let cmc4 = add_library_creature_with_cmc(&mut state, 2, PlayerId(0), "Mid", 4);
        add_library_creature_with_cmc(&mut state, 3, PlayerId(0), "Large", 5);
        add_library_creature_with_cmc(&mut state, 4, PlayerId(0), "Behemoth", 8);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(4);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 2, "Expected only CMC-2 and CMC-4 creatures");
                assert!(cards.contains(&cmc2));
                assert!(cards.contains(&cmc4));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    /// CR 107.3b: X=0 restricts to CMC-0 creatures only.
    #[test]
    fn natures_rhythm_x_zero_restricts_to_cmc_zero_creatures() {
        use crate::types::ability::{FilterProp, QuantityExpr, QuantityRef};
        let mut state = GameState::new_two_player(42);
        let zero_cmc = add_library_creature_with_cmc(&mut state, 1, PlayerId(0), "Zero", 0);
        add_library_creature_with_cmc(&mut state, 2, PlayerId(0), "NonZero", 2);

        let filter =
            TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::CmcLE {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            }]));
        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(0);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => {
                assert_eq!(cards.len(), 1);
                assert!(cards.contains(&zero_cmc));
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }

    /// CR 107.3a: `SearchLibrary.count = Variable("X")` with `chosen_x = 3` →
    /// `pick_count == 3`.
    #[test]
    fn search_library_with_x_count_picks_x_cards() {
        use crate::types::ability::{QuantityExpr, QuantityRef};
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            add_library_creature(&mut state, 1 + i as u64, PlayerId(0), &format!("C{i}"));
        }

        let mut ability = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                reveal: false,
                target_player: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice { count, .. } => {
                assert_eq!(*count, 3);
            }
            other => panic!("Expected SearchChoice, got {:?}", other),
        }
    }
}
