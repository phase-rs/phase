use crate::game::effects::change_zone::{self, ZoneMoveResult};
use crate::game::printed_cards::apply_card_face_to_object;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::TargetFilter;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, OutsideGameCardUse, OutsideGameChoiceEntry, OutsideGameChoiceSource, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
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
        source_pool,
    } = &ability.effect
    else {
        return Ok(());
    };

    let (inner_count, up_to) = count.peel_up_to();
    let count = resolve_quantity_with_targets(state, inner_count, ability).max(0) as usize;

    // CR 400.11a: Sideboard half of the candidate pool. Absent when no deck
    // pool is registered for the controller (casual play / no sideboard).
    let mut choices: Vec<OutsideGameChoiceEntry> = state
        .deck_pools
        .iter()
        .find(|pool| pool.player == ability.controller)
        .map(|pool| {
            pool.current_sideboard
                .iter()
                .enumerate()
                .filter_map(|(sideboard_index, entry)| {
                    let available_count = available_sideboard_count(
                        state,
                        ability.controller,
                        sideboard_index,
                        entry.count,
                    );
                    (available_count > 0
                        && crate::game::filter::matches_target_filter_against_face(
                            &entry.card,
                            filter,
                        ))
                    .then(|| OutsideGameChoiceEntry {
                        source: OutsideGameChoiceSource::Sideboard {
                            sideboard_index,
                            card: entry.card.clone(),
                        },
                        count: available_count,
                        name: entry.card.name.clone(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // CR 406.3 + CR 400.11: Karn/Coax-class — also append face-up exile cards
    // the controller owns and that match the filter. The exile zone is a normal
    // in-game zone, so we route through the standard filter pipeline.
    if source_pool.includes_face_up_exile() {
        let exile_candidates = collect_face_up_exile_candidates(state, ability, filter);
        choices.extend(exile_candidates);
    }

    if choices.is_empty() || count == 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::SearchOutsideGame,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let available_total = choices.iter().map(|choice| choice.count as usize).sum();
    state.waiting_for = WaitingFor::OutsideGameChoice {
        player: ability.controller,
        source_id: ability.source_id,
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

/// CR 406.3 + CR 400.11: Collect face-up exile cards owned by the activating
/// ability's controller that match `filter`. Each candidate is a unique
/// in-game object, so the per-entry `count` is fixed at 1.
fn collect_face_up_exile_candidates(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<OutsideGameChoiceEntry> {
    let controller = ability.controller;
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    // CR 406.3: A card's default face state in exile is face-up; the
    // `face_down: true` marker means the card is exiled face-down. Use the
    // canonical `targeting::zone_object_ids` helper for zone iteration.
    crate::game::targeting::zone_object_ids(state, Zone::Exile)
        .into_iter()
        .filter_map(|object_id| {
            let object = state.objects.get(&object_id)?;
            if object.owner != controller {
                return None;
            }
            if object.face_down {
                return None;
            }
            if !crate::game::filter::matches_target_filter(state, object_id, filter, &ctx) {
                return None;
            }
            Some(OutsideGameChoiceEntry {
                source: OutsideGameChoiceSource::FaceUpExile { object_id },
                count: 1,
                name: object.name.clone(),
            })
        })
        .collect()
}

/// CR 406.3 + CR 400.11 + CR 614.1: Move a face-up exile object into
/// `destination`. The object retains its identity (no new object created);
/// because exile is an in-game zone, the move routes through the normal
/// `ChangeZone` replacement pipeline.
pub(crate) fn put_face_up_exile_into(
    state: &mut GameState,
    object_id: ObjectId,
    destination: Zone,
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<ZoneMoveResult, EffectError> {
    if !state.exile.contains(&object_id) {
        return Err(EffectError::InvalidParam(
            "face-up exile object not in exile zone".to_string(),
        ));
    }
    let Some(object) = state.objects.get(&object_id) else {
        return Err(EffectError::InvalidParam(
            "face-up exile object missing".to_string(),
        ));
    };
    if object.face_down {
        return Err(EffectError::InvalidParam(
            "exile object is face-down".to_string(),
        ));
    }
    let ctx = change_zone::ChangeZoneIterationCtx {
        source_id,
        controller,
        origin: Some(Zone::Exile),
        destination,
        enter_transformed: false,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enters_under_player: None,
        enters_attacking: false,
        enter_with_counters: Vec::new(),
        conditional_enter_with_counters: vec![],
        duration: None,
        track_exiled_by_source: false,
        // Search-from-outside brings cards in face up.
        face_down_profile: None,
        library_placement: None,
        // CR 614.12: search-from-outside carries no moved-object type gate.
        enters_modified_if: None,
        enter_attached_to: None,
    };
    Ok(change_zone::process_one_zone_move(
        state, &ctx, object_id, events,
    ))
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

// CR 205: card-type/face filtering for sideboard entries now delegates to the
// shared `crate::game::filter::matches_target_filter_against_face` building block
// (used here and by `Effect::CreateTokenCopyFromPool`), so face-vs-filter logic
// lives in exactly one place.

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::deck_loading::DeckEntry;
    use crate::game::effects;
    use crate::game::zones::create_object;
    use crate::types::ability::{OutsideGameSourcePool, QuantityExpr, TypeFilter, TypedFilter};
    use crate::types::actions::{GameAction, OutsideGameSelection};
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
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

    /// CR 400.11a: Build a sideboard-source `OutsideGameChoiceEntry` from a
    /// `DeckEntry`. Used by tests to seed the offered-choices list.
    fn sideboard_choice(sideboard_index: usize, entry: &DeckEntry) -> OutsideGameChoiceEntry {
        OutsideGameChoiceEntry {
            source: OutsideGameChoiceSource::Sideboard {
                sideboard_index,
                card: entry.card.clone(),
            },
            count: entry.count,
            name: entry.card.name.clone(),
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
                source_pool: OutsideGameSourcePool::Sideboard,
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
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                assert!(matches!(
                    &choices[0].source,
                    OutsideGameChoiceSource::Sideboard {
                        sideboard_index: 0,
                        ..
                    }
                ));
            }
            other => panic!("expected OutsideGameChoice, got {other:?}"),
        }

        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 0 }],
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
                assert_eq!(choices[0].count, 1);
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
                selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 0 }],
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
            source_id: ObjectId(0),
            choices: vec![sideboard_choice(
                0,
                &state.deck_pools[0].current_sideboard[0].clone(),
            )],
            count: 1,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let result = crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 1 }],
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn duplicate_sideboard_selection_up_to_available_count_is_accepted() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 2)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            source_id: ObjectId(0),
            choices: vec![sideboard_choice(
                0,
                &state.deck_pools[0].current_sideboard[0].clone(),
            )],
            count: 2,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                selections: vec![
                    OutsideGameSelection::Sideboard { sideboard_index: 0 },
                    OutsideGameSelection::Sideboard { sideboard_index: 0 },
                ],
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
            source_id: ObjectId(0),
            choices: vec![sideboard_choice(
                0,
                &state.deck_pools[0].current_sideboard[0].clone(),
            )],
            count: 3,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let result = crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                selections: vec![
                    OutsideGameSelection::Sideboard { sideboard_index: 0 },
                    OutsideGameSelection::Sideboard { sideboard_index: 0 },
                    OutsideGameSelection::Sideboard { sideboard_index: 0 },
                ],
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
            source_id: ObjectId(0),
            choices: vec![sideboard_choice(
                0,
                &state.deck_pools[0].current_sideboard[0].clone(),
            )],
            count: 1,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let actions = crate::ai_support::legal_actions(&state);

        assert!(actions.iter().any(|action| matches!(
            action,
            GameAction::ChooseOutsideGameCards {
                selections
            } if selections.as_slice() == [OutsideGameSelection::Sideboard { sideboard_index: 0 }]
        )));
    }

    #[test]
    fn ai_generates_duplicate_outside_game_indices_for_available_copies() {
        let mut state = state_with_sideboard(vec![entry("Pyroclasm", CoreType::Sorcery, 2)]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            source_id: ObjectId(0),
            choices: vec![sideboard_choice(
                0,
                &state.deck_pools[0].current_sideboard[0].clone(),
            )],
            count: 2,
            reveal: true,
            up_to: false,
            destination: Zone::Hand,
        };

        let actions = crate::ai_support::legal_actions(&state);

        let expected = vec![
            OutsideGameSelection::Sideboard { sideboard_index: 0 },
            OutsideGameSelection::Sideboard { sideboard_index: 0 },
        ];
        assert!(actions.iter().any(|action| matches!(
            action,
            GameAction::ChooseOutsideGameCards { selections } if selections == &expected
        )));
    }

    /// CR 406.3 + CR 400.11: Karn-class disjunction (-2 ability) — a face-up
    /// artifact in the controller's exile zone must appear as a candidate
    /// alongside any sideboard cards, and choosing it must move that specific
    /// in-game object into the controller's hand.
    // CR 400.7: Conceptually a new object on zone change; the engine preserves
    // ObjectId as an implementation-level optimization, consistent with all
    // other inter-zone moves.
    #[test]
    fn karn_minus_two_pulls_face_up_exile_artifact_to_hand() {
        let mut state = state_with_sideboard(vec![entry("Pithing Needle", CoreType::Artifact, 0)]);
        // Seed a face-up artifact in the controller's exile zone.
        let exiled = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Pithing Needle".to_string(),
            Zone::Exile,
        );
        if let Some(obj) = state.objects.get_mut(&exiled) {
            obj.card_types = CardType {
                core_types: vec![CoreType::Artifact],
                ..Default::default()
            };
            obj.face_down = false;
        }

        // Karn-class source on the battlefield; activate -2.
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Karn, the Great Creator".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::SearchOutsideGame {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }),
                reveal: true,
                destination: Zone::Hand,
                source_pool: OutsideGameSourcePool::SideboardAndFaceUpExile,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        match &state.waiting_for {
            WaitingFor::OutsideGameChoice { choices, .. } => {
                // The face-up exile object must appear in the choice list.
                let has_exile_choice = choices.iter().any(|choice| {
                    matches!(
                        &choice.source,
                        OutsideGameChoiceSource::FaceUpExile { object_id } if *object_id == exiled
                    )
                });
                assert!(
                    has_exile_choice,
                    "Karn-class must offer face-up exile artifact, got {:?}",
                    choices
                );
            }
            other => panic!("expected OutsideGameChoice, got {other:?}"),
        }

        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                selections: vec![OutsideGameSelection::FaceUpExile { object_id: exiled }],
            },
        )
        .unwrap();

        // CR 406.3: The object retains its identity and is now in Hand.
        assert!(
            state.players[0].hand.iter().any(|id| *id == exiled),
            "exile object must be in hand after Karn-class resolution"
        );
        assert!(
            !state.exile.contains(&exiled),
            "exile object must have left the exile zone"
        );
        let obj_zone = state.objects.get(&exiled).map(|obj| obj.zone);
        assert_eq!(
            obj_zone,
            Some(Zone::Hand),
            "moved object's zone must equal Hand"
        );
    }

    /// CR 406.3 + CR 400.11: When the Karn-class disjunction's other branch
    /// (sideboard) is exercised, the sideboard pipeline still resolves —
    /// proving the source pool is additive, not replacing.
    #[test]
    fn karn_minus_two_pulls_sideboard_artifact_to_hand() {
        let mut state = state_with_sideboard(vec![entry("Pithing Needle", CoreType::Artifact, 1)]);
        let source = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Karn, the Great Creator".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::SearchOutsideGame {
                filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 1 }),
                reveal: true,
                destination: Zone::Hand,
                source_pool: OutsideGameSourcePool::SideboardAndFaceUpExile,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        crate::game::apply_as_current(
            &mut state,
            GameAction::ChooseOutsideGameCards {
                selections: vec![OutsideGameSelection::Sideboard { sideboard_index: 0 }],
            },
        )
        .unwrap();

        let hand_names: Vec<_> = state.players[0]
            .hand
            .iter()
            .filter_map(|id| state.objects.get(id).map(|obj| obj.name.as_str()))
            .collect();
        assert_eq!(hand_names, vec!["Pithing Needle"]);
        assert_eq!(state.outside_game_cards_brought_in[0].count, 1);
    }

    #[test]
    fn visibility_redacts_opponent_outside_game_choices() {
        let mut state = state_with_sideboard(vec![
            entry("Pyroclasm", CoreType::Sorcery, 2),
            entry("Grapeshot", CoreType::Sorcery, 1),
        ]);
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            source_id: ObjectId(0),
            choices: vec![
                sideboard_choice(0, &state.deck_pools[0].current_sideboard[0].clone()),
                sideboard_choice(1, &state.deck_pools[0].current_sideboard[1].clone()),
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
                assert_eq!(choices[0].name, "Pyroclasm");
                assert_eq!(choices[0].count, 2);
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
