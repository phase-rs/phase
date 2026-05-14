use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, FilterProp, ResolvedAbility};
use crate::types::ability::{TargetFilter, TypeFilter};
use crate::types::card::CardFace;
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, OutsideGameCardUse, OutsideGameChoiceEntry, WaitingFor};
use crate::types::identifiers::CardId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::SearchOutsideGame {
        filter,
        count,
        reveal,
        destination,
    } = &ability.effect
    else {
        return Ok(());
    };

    let (inner_count, up_to) = count.peel_up_to();
    let count = resolve_quantity_with_targets(state, inner_count, ability).max(0) as usize;
    let Some(pool) = state
        .deck_pools
        .iter()
        .find(|pool| pool.player == ability.controller)
    else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchOutsideGame,
            source_id: ability.source_id,
        });
        return Ok(());
    };

    let choices: Vec<_> = pool
        .current_sideboard
        .iter()
        .enumerate()
        .filter_map(|(sideboard_index, entry)| {
            let available_count =
                available_sideboard_count(state, ability.controller, sideboard_index, entry.count);
            (available_count > 0 && sideboard_entry_matches_filter(&entry.card, filter)).then(
                || {
                    let mut entry = entry.clone();
                    entry.count = available_count;
                    OutsideGameChoiceEntry {
                        sideboard_index,
                        entry,
                    }
                },
            )
        })
        .collect();

    if choices.is_empty() || count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchOutsideGame,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let available_total = choices
        .iter()
        .map(|choice| choice.entry.count as usize)
        .sum();
    state.waiting_for = WaitingFor::OutsideGameChoice {
        player: ability.controller,
        count: count.min(available_total),
        choices,
        reveal: *reveal,
        up_to,
        destination: *destination,
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SearchOutsideGame,
        source_id: ability.source_id,
    });
    Ok(())
}

pub(crate) fn put_sideboard_entry_into_game(
    state: &mut GameState,
    player: PlayerId,
    sideboard_index: usize,
    destination: Zone,
) -> Result<crate::types::identifiers::ObjectId, EffectError> {
    let card_face = {
        let entry = state
            .deck_pools
            .iter()
            .find(|pool| pool.player == player)
            .ok_or(EffectError::PlayerNotFound)?;
        let Some(entry) = entry.current_sideboard.get(sideboard_index) else {
            return Err(EffectError::InvalidParam(
                "sideboard index out of range".to_string(),
            ));
        };
        if available_sideboard_count(state, player, sideboard_index, entry.count) == 0 {
            return Err(EffectError::InvalidParam(
                "sideboard card already brought into game".to_string(),
            ));
        }
        entry.card.clone()
    };

    if let Some(used) = state
        .outside_game_cards_brought_in
        .iter_mut()
        .find(|used| used.player == player && used.sideboard_index == sideboard_index)
    {
        used.count += 1;
    } else {
        state
            .outside_game_cards_brought_in
            .push(OutsideGameCardUse {
                player,
                sideboard_index,
                count: 1,
            });
    }

    let card_id = CardId(state.next_object_id);
    let obj_id = zones::create_object(state, card_id, player, card_face.name.clone(), destination);
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        apply_card_face_to_object(obj, &card_face);
    }
    Ok(obj_id)
}

fn available_sideboard_count(
    state: &GameState,
    player: PlayerId,
    sideboard_index: usize,
    sideboard_count: u32,
) -> u32 {
    let used = state
        .outside_game_cards_brought_in
        .iter()
        .find(|used| used.player == player && used.sideboard_index == sideboard_index)
        .map_or(0, |used| used.count);
    sideboard_count.saturating_sub(used)
}

fn sideboard_entry_matches_filter(card: &CardFace, filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::None => false,
        TargetFilter::Typed(typed) => {
            typed.controller.is_none()
                && typed
                    .type_filters
                    .iter()
                    .all(|type_filter| card_matches_type_filter(card, type_filter))
                && typed.properties.iter().all(|property| match property {
                    FilterProp::HasSupertype { value } => card.card_type.supertypes.contains(value),
                    _ => false,
                })
        }
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|inner| sideboard_entry_matches_filter(card, inner)),
        TargetFilter::And { filters } => filters
            .iter()
            .all(|inner| sideboard_entry_matches_filter(card, inner)),
        TargetFilter::Not { filter } => !sideboard_entry_matches_filter(card, filter),
        _ => false,
    }
}

fn card_matches_type_filter(card: &CardFace, filter: &TypeFilter) -> bool {
    match filter {
        TypeFilter::Creature => card.card_type.core_types.contains(&CoreType::Creature),
        TypeFilter::Land => card.card_type.core_types.contains(&CoreType::Land),
        TypeFilter::Artifact => card.card_type.core_types.contains(&CoreType::Artifact),
        TypeFilter::Enchantment => card.card_type.core_types.contains(&CoreType::Enchantment),
        TypeFilter::Instant => card.card_type.core_types.contains(&CoreType::Instant),
        TypeFilter::Sorcery => card.card_type.core_types.contains(&CoreType::Sorcery),
        TypeFilter::Planeswalker => card.card_type.core_types.contains(&CoreType::Planeswalker),
        TypeFilter::Battle => card.card_type.core_types.contains(&CoreType::Battle),
        TypeFilter::Permanent => card
            .card_type
            .core_types
            .iter()
            .any(|card_type| card_type.is_permanent_type()),
        TypeFilter::Card | TypeFilter::Any => true,
        TypeFilter::Non(inner) => !card_matches_type_filter(card, inner),
        TypeFilter::Subtype(subtype) => card.card_type.subtypes.contains(subtype),
        TypeFilter::AnyOf(filters) => filters
            .iter()
            .any(|inner| card_matches_type_filter(card, inner)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::game::effects;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, TypedFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CardType;
    use crate::types::game_state::PlayerDeckPool;

    fn face(name: &str, core_type: CoreType) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![core_type],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn entry(name: &str, core_type: CoreType, count: u32) -> DeckEntry {
        DeckEntry {
            card: face(name, core_type),
            count,
        }
    }

    fn state_with_sideboard(sideboard: Vec<DeckEntry>) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.deck_pools = vec![PlayerDeckPool {
            player: PlayerId(0),
            current_sideboard: Arc::new(sideboard),
            ..Default::default()
        }];
        state
    }

    fn wish_chain(source_id: crate::types::identifiers::ObjectId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::SearchOutsideGame {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Sorcery)),
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }),
                reveal: true,
                destination: Zone::Hand,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            source_id,
            PlayerId(0),
        )));
        ability
    }

    #[test]
    fn choosing_sideboard_sorcery_preserves_match_sideboard_and_exiles_source() {
        let mut state = state_with_sideboard(vec![
            entry("Pyroclasm", CoreType::Sorcery, 2),
            entry("Lightning Bolt", CoreType::Instant, 1),
        ]);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Burning Wish".to_string(),
            Zone::Stack,
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &wish_chain(source), &mut events, 0).unwrap();
        match &state.waiting_for {
            WaitingFor::OutsideGameChoice {
                choices,
                count,
                reveal,
                up_to,
                ..
            } => {
                assert_eq!(*count, 1);
                assert!(*reveal);
                assert!(*up_to);
                assert_eq!(choices.len(), 1);
                assert_eq!(choices[0].sideboard_index, 0);
            }
            other => panic!("expected OutsideGameChoice, got {other:?}"),
        }

        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices: vec![0],
            },
        )
        .unwrap();

        let hand_names: Vec<_> = state.players[0]
            .hand
            .iter()
            .filter_map(|id| state.objects.get(id).map(|obj| obj.name.as_str()))
            .collect();
        assert_eq!(hand_names, vec!["Pyroclasm"]);
        assert_eq!(state.deck_pools[0].current_sideboard[0].count, 2);
        assert_eq!(state.outside_game_cards_brought_in.len(), 1);
        assert_eq!(state.outside_game_cards_brought_in[0].player, PlayerId(0));
        assert_eq!(state.outside_game_cards_brought_in[0].sideboard_index, 0);
        assert_eq!(state.outside_game_cards_brought_in[0].count, 1);
        assert!(state.players[0].hand.iter().all(|id| *id != source));
        assert!(state.exile.contains(&source));

        let second_source = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Burning Wish".to_string(),
            Zone::Stack,
        );
        let mut second_events = Vec::new();
        effects::resolve_ability_chain(
            &mut state,
            &wish_chain(second_source),
            &mut second_events,
            0,
        )
        .unwrap();
        match &state.waiting_for {
            WaitingFor::OutsideGameChoice { choices, .. } => {
                assert_eq!(choices.len(), 1);
                assert_eq!(choices[0].entry.count, 1);
            }
            other => panic!("expected OutsideGameChoice, got {other:?}"),
        }
    }

    #[test]
    fn no_matching_sideboard_card_still_runs_continuation() {
        let mut state = state_with_sideboard(vec![entry("Lightning Bolt", CoreType::Instant, 1)]);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Burning Wish".to_string(),
            Zone::Stack,
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &wish_chain(source), &mut events, 0).unwrap();

        assert!(!matches!(
            state.waiting_for,
            WaitingFor::OutsideGameChoice { .. }
        ));
        assert!(state.exile.contains(&source));
    }

    #[test]
    fn single_copy_brought_into_game_is_not_offered_again() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 1)]);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Burning Wish".to_string(),
            Zone::Stack,
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &wish_chain(source), &mut events, 0).unwrap();
        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices: vec![0],
            },
        )
        .unwrap();

        let second_source = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Burning Wish".to_string(),
            Zone::Stack,
        );
        let mut second_events = Vec::new();
        effects::resolve_ability_chain(
            &mut state,
            &wish_chain(second_source),
            &mut second_events,
            0,
        )
        .unwrap();

        assert!(!matches!(
            state.waiting_for,
            WaitingFor::OutsideGameChoice { .. }
        ));
        assert_eq!(state.deck_pools[0].current_sideboard[0].count, 1);
        assert_eq!(state.outside_game_cards_brought_in[0].count, 1);
    }

    #[test]
    fn illegal_sideboard_selection_is_rejected() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 1)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            choices: vec![OutsideGameChoiceEntry {
                sideboard_index: 0,
                entry: state.deck_pools[0].current_sideboard[0].clone(),
            }],
            count: 1,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let result = crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices: vec![1],
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn duplicate_sideboard_selection_up_to_available_count_is_accepted() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 2)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            choices: vec![OutsideGameChoiceEntry {
                sideboard_index: 0,
                entry: state.deck_pools[0].current_sideboard[0].clone(),
            }],
            count: 2,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices: vec![0, 0],
            },
        )
        .unwrap();

        let hand_names: Vec<_> = state.players[0]
            .hand
            .iter()
            .filter_map(|id| state.objects.get(id).map(|obj| obj.name.as_str()))
            .collect();
        assert_eq!(hand_names, vec!["Pyroclasm", "Pyroclasm"]);
        assert_eq!(state.outside_game_cards_brought_in[0].count, 2);
        assert_eq!(state.deck_pools[0].current_sideboard[0].count, 2);
    }

    #[test]
    fn duplicate_sideboard_selection_exceeding_available_count_is_rejected() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 2)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            choices: vec![OutsideGameChoiceEntry {
                sideboard_index: 0,
                entry: state.deck_pools[0].current_sideboard[0].clone(),
            }],
            count: 3,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let result = crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices: vec![0, 0, 0],
            },
        );

        assert!(result.is_err());
        assert!(state.players[0].hand.is_empty());
        assert!(state.outside_game_cards_brought_in.is_empty());
    }

    #[test]
    fn ai_generates_outside_game_choice_action() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 1)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            choices: vec![OutsideGameChoiceEntry {
                sideboard_index: 0,
                entry: state.deck_pools[0].current_sideboard[0].clone(),
            }],
            count: 1,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let actions = crate::ai_support::legal_actions(&state);

        assert!(actions.iter().any(|action| matches!(
            action,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices
            } if sideboard_indices == &vec![0]
        )));
    }

    #[test]
    fn ai_generates_duplicate_outside_game_indices_for_available_copies() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 2)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            choices: vec![OutsideGameChoiceEntry {
                sideboard_index: 0,
                entry: state.deck_pools[0].current_sideboard[0].clone(),
            }],
            count: 2,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let actions = crate::ai_support::legal_actions(&state);

        assert!(actions.iter().any(|action| matches!(
            action,
            GameAction::ChooseOutsideGameCards {
                sideboard_indices
            } if sideboard_indices == &vec![0, 0]
        )));
    }

    #[test]
    fn visibility_redacts_opponent_outside_game_choices() {
        let mut state = state_with_sideboard(vec![
            entry("Pyroclasm", CoreType::Sorcery, 2),
            entry("Grapeshot", CoreType::Sorcery, 1),
        ]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            choices: vec![
                OutsideGameChoiceEntry {
                    sideboard_index: 0,
                    entry: state.deck_pools[0].current_sideboard[0].clone(),
                },
                OutsideGameChoiceEntry {
                    sideboard_index: 1,
                    entry: state.deck_pools[0].current_sideboard[1].clone(),
                },
            ],
            count: 1,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };
        state
            .outside_game_cards_brought_in
            .push(OutsideGameCardUse {
                player: PlayerId(0),
                sideboard_index: 0,
                count: 1,
            });

        let self_view = crate::game::filter_state_for_viewer(&state, PlayerId(0));
        let opponent_view = crate::game::filter_state_for_viewer(&state, PlayerId(1));

        assert_eq!(self_view.outside_game_cards_brought_in.len(), 1);
        assert!(opponent_view.outside_game_cards_brought_in.is_empty());
        match self_view.waiting_for {
            WaitingFor::OutsideGameChoice { choices, count, .. } => {
                assert_eq!(count, 1);
                assert_eq!(choices[0].entry.card.name, "Pyroclasm");
                assert_eq!(choices[0].entry.count, 2);
                assert_eq!(choices.len(), 2);
            }
            other => panic!("expected OutsideGameChoice, got {other:?}"),
        }
        match opponent_view.waiting_for {
            WaitingFor::OutsideGameChoice { choices, count, .. } => {
                assert_eq!(count, 0);
                assert!(choices.is_empty());
            }
            other => panic!("expected OutsideGameChoice, got {other:?}"),
        }
    }
}
