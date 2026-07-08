use rand::seq::SliceRandom;

use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

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
    let (filter, count_expr, from_top, destination, enter_tapped) = match &ability.effect {
        Effect::Seek {
            filter,
            count,
            from_top,
            destination,
            enter_tapped,
        } => (
            filter.clone(),
            count.clone(),
            *from_top,
            *destination,
            *enter_tapped,
        ),
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
    let library_scope = player
        .library
        .iter()
        .take(from_top.unwrap_or(player.library.len()));
    let mut matching: Vec<_> = library_scope
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

    // CR 614.6: route every sought card through the zone-change pipeline
    // (`zone_pipeline::move_objects_simultaneously`) rather than a raw move for
    // non-battlefield destinations. The raw move never proposed a per-card
    // ZoneChange, so a `Moved` redirect ("if a card would be put into a
    // graveyard/hand from anywhere, ... instead") never fired; the battlefield
    // path already used the pipeline for ETB effects, so both destinations now
    // share one entry. Attribution stays `ability.source_id` so battlefield
    // entries record `entered_via_ability_source` and exile-link tracking keys
    // off the seek's source, matching the pre-batch single-move behavior.
    let track_exiled =
        crate::game::exile_links::should_track_exiled_by_source(state, ability.source_id, ability);
    let reqs: Vec<ZoneMoveRequest> = matching[..pick_count]
        .iter()
        .map(|&card_id| {
            let mut req = ZoneMoveRequest::effect(card_id, destination, ability.source_id);
            req.mods.enter_tapped = enter_tapped;
            if track_exiled {
                req = req.track_exiled_by_source();
            }
            req
        })
        .collect();

    // CR 616.1: a `Moved` redirect (or, for a battlefield entry, an as-enters
    // choice) can surface a player choice mid-batch. `move_objects_simultaneously`
    // parks `state.waiting_for` and stashes the undelivered tail in
    // `state.pending_batch_deliveries`; bail before emitting `EffectResolved` so
    // the surfaced prompt is not clobbered and no later pick overwrites the
    // parked replacement. The resume path
    // (`zone_pipeline::drain_pending_batch_deliveries`) finishes the batch.
    if matches!(
        zone_pipeline::move_objects_simultaneously(state, reqs, events),
        BatchMoveResult::NeedsChoice
    ) {
        return Ok(());
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
    use crate::types::ability::{
        CardPredicateChoice, ChoiceValue, FilterProp, QuantityExpr, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_seek_ability(filter: TargetFilter, count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Seek {
                filter,
                count: QuantityExpr::Fixed {
                    value: count as i32,
                },
                from_top: None,
                destination: Zone::Hand,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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
                from_top: None,
                destination: Zone::Battlefield,
                enter_tapped: crate::types::zones::EtbTapState::from_legacy_bool(tapped),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_seek_from_top(filter: TargetFilter, from_top: usize) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Seek {
                filter,
                count: QuantityExpr::Fixed { value: 1 },
                from_top: Some(from_top),
                destination: Zone::Hand,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
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

    fn add_library_artifact(
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
            .push(CoreType::Artifact);
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
    fn seek_from_top_limits_candidate_pool_before_filtering() {
        let mut state = GameState::new_two_player(42);
        add_library_land(&mut state, 1, PlayerId(0), "Forest");
        add_library_creature(&mut state, 2, PlayerId(0), "Bear");
        let artifact = add_library_artifact(&mut state, 3, PlayerId(0), "Key");

        let ability = make_seek_from_top(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            2,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let player = &state.players[0];
        assert!(!player.hand.contains(&artifact));
        assert!(player.library.contains(&artifact));
    }

    #[test]
    fn seek_from_top_can_find_matching_card_inside_limit() {
        let mut state = GameState::new_two_player(42);
        add_library_land(&mut state, 1, PlayerId(0), "Forest");
        let artifact = add_library_artifact(&mut state, 2, PlayerId(0), "Key");
        add_library_creature(&mut state, 3, PlayerId(0), "Bear");

        let ability = make_seek_from_top(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
            2,
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&artifact));
    }

    #[test]
    fn seek_of_chosen_kind_matches_land_choice() {
        let mut state = GameState::new_two_player(42);
        let land = add_library_land(&mut state, 1, PlayerId(0), "Forest");
        let creature = add_library_creature(&mut state, 2, PlayerId(0), "Bear");
        state.last_named_choice = Some(ChoiceValue::CardPredicate(CardPredicateChoice::Land));

        let filter = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::MatchesLastChosenCardPredicate]),
        );
        let ability = make_seek_ability(filter, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let player = &state.players[0];
        assert!(player.hand.contains(&land));
        assert!(!player.hand.contains(&creature));
    }

    #[test]
    fn seek_of_chosen_kind_matches_nonland_choice() {
        let mut state = GameState::new_two_player(42);
        let land = add_library_land(&mut state, 1, PlayerId(0), "Forest");
        let creature = add_library_creature(&mut state, 2, PlayerId(0), "Bear");
        state.last_named_choice = Some(ChoiceValue::CardPredicate(CardPredicateChoice::Nonland));

        let filter = TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::MatchesLastChosenCardPredicate]),
        );
        let ability = make_seek_ability(filter, 1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let player = &state.players[0];
        assert!(!player.hand.contains(&land));
        assert!(player.hand.contains(&creature));
    }

    /// D2 discriminating test (CR 616.1): a multi-card seek whose per-card moves
    /// surface a replacement-ordering choice must PARK the prompt and stash the
    /// undelivered tail — never push `EffectResolved` over the parked prompt nor
    /// overwrite `pending_replacement` with a later pick. Two simultaneously-
    /// applicable graveyard→exile redirects make each seeked-to-graveyard card
    /// prompt for CR 616.1 ordering, so the first card pauses the batch.
    ///
    /// On the old per-card loop (raw `move_to_zone`, ignored result) the loop
    /// ran to completion and emitted `EffectResolved` over the parked state.
    #[test]
    fn seek_parks_on_per_card_replacement_choice_and_stashes_tail() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Two graveyard→exile Moved redirects → CR 616.1 ordering prompt per card.
        for (desc, card) in [
            ("Rest in Peace redirect", CardId(1000)),
            ("Leyline of the Void redirect", CardId(1001)),
        ] {
            let source = create_object(
                &mut state,
                card,
                PlayerId(0),
                "Redirect Source".to_string(),
                Zone::Battlefield,
            );
            let redirect = ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        destination: Zone::Exile,
                        origin: None,
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
                ))
                .description(desc.to_string());
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions = vec![redirect].into();
        }

        // Two creatures in the controller's library to seek to the graveyard.
        add_library_creature(&mut state, 1, PlayerId(0), "Bear 1");
        add_library_creature(&mut state, 2, PlayerId(0), "Bear 2");

        let ability = ResolvedAbility::new(
            Effect::Seek {
                filter: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 2 },
                from_top: None,
                destination: Zone::Graveyard,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The first card's CR 616.1 prompt must be parked, the tail stashed, and
        // EffectResolved NOT emitted over the parked prompt.
        assert!(
            matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::ReplacementChoice { .. }
            ),
            "per-card ordering prompt must be parked"
        );
        let stash = state
            .pending_batch_deliveries
            .as_ref()
            .expect("the undelivered tail must be stashed for the resume path");
        // Fix-4: the re-stash must carry the batch-uniform request context so
        // the drain rebuilds equivalent requests — seek attributes every move to
        // `ability.source_id` (CR 400.7), not to each moved object itself.
        assert_eq!(
            stash.source_id,
            Some(ObjectId(100)),
            "stashed tail must preserve the seek's ability-source attribution"
        );
        assert!(
            !events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Seek,
                    ..
                }
            )),
            "EffectResolved must not be pushed over a parked replacement prompt"
        );
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
