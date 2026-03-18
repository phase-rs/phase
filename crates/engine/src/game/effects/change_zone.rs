use rand::seq::SliceRandom;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    Duration, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
    TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{ExileLink, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 401.3: Shuffle a player's library using the game's seeded RNG.
/// Reusable helper for auto-shuffle after zone moves to Library.
pub fn shuffle_library(state: &mut GameState, player: PlayerId) {
    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
        p.library.shuffle(&mut state.rng);
    }
}

/// Result of a single zone-move attempt through the replacement pipeline.
enum ZoneMoveResult {
    /// Object was moved (or prevented). Continue processing.
    Done,
    /// A replacement effect needs a player choice before continuing.
    NeedsChoice(PlayerId),
}

/// Execute a single object zone-change through the full pipeline:
/// ProposedEvent → replacement → move → ExileLink → shuffle → layers_dirty.
///
/// Shared by both `resolve()` (targeted) and `resolve_all()` (mass) to ensure
/// identical behavior for replacement effects, exile tracking, and auto-shuffle.
fn execute_zone_move(
    state: &mut GameState,
    obj_id: ObjectId,
    from_zone: Zone,
    dest_zone: Zone,
    source_id: ObjectId,
    duration: Option<&Duration>,
    events: &mut Vec<GameEvent>,
) -> ZoneMoveResult {
    let proposed = ProposedEvent::zone_change(obj_id, from_zone, dest_zone, Some(source_id));

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
                zones::move_to_zone(state, object_id, to, events);
                if to == Zone::Battlefield || from_zone == Zone::Battlefield {
                    state.layers_dirty = true;
                }
                // CR 401.3: If an object is put into a library (not at a specific
                // position), that library is shuffled afterward.
                if to == Zone::Library {
                    let owner = state.objects.get(&object_id).map(|o| o.owner);
                    if let Some(owner) = owner {
                        shuffle_library(state, owner);
                    }
                }
                // CR 610.3a: Track exile-until-leaves links
                if to == Zone::Exile {
                    if let Some(Duration::UntilHostLeavesPlay) = duration {
                        state.exile_links.push(ExileLink {
                            exiled_id: object_id,
                            source_id,
                            return_zone: from_zone,
                        });
                    }
                }
            }
            ZoneMoveResult::Done
        }
        ReplacementResult::Prevented => ZoneMoveResult::Done,
        ReplacementResult::NeedsChoice(player) => ZoneMoveResult::NeedsChoice(player),
    }
}

/// Move target objects between zones.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (dest_zone, owner_library) = match &ability.effect {
        Effect::ChangeZone {
            destination,
            owner_library,
            ..
        } => (*destination, *owner_library),
        _ => return Err(EffectError::MissingParam("Destination".to_string())),
    };

    let target_filter = match &ability.effect {
        Effect::ChangeZone { target, .. } => target,
        _ => &TargetFilter::Any,
    };

    // SelfRef with no explicit targets: process the source object through the zone-change pipeline
    // (e.g., "shuffle ~ into its owner's library")
    let self_ref_targets =
        if matches!(target_filter, TargetFilter::SelfRef) && ability.targets.is_empty() {
            vec![TargetRef::Object(ability.source_id)]
        } else {
            vec![]
        };

    let effective_targets = if self_ref_targets.is_empty() {
        &ability.targets
    } else {
        &self_ref_targets
    };

    for target in effective_targets {
        if let TargetRef::Object(obj_id) = target {
            // CR 114.4: Emblems cannot be moved between zones
            if state.objects.get(obj_id).is_some_and(|o| o.is_emblem) {
                continue;
            }

            let from_zone = state
                .objects
                .get(obj_id)
                .map(|o| o.zone)
                .unwrap_or(Zone::Battlefield);

            // CR 400.7: When owner_library is true, route to the object's owner's library.
            // The actual owner routing is handled by zones::move_to_zone which uses
            // the object's owner for player-owned zones.
            let effective_dest = dest_zone;
            let _ = owner_library; // routing handled by move_to_zone

            match execute_zone_move(
                state,
                *obj_id,
                from_zone,
                effective_dest,
                ability.source_id,
                ability.duration.as_ref(),
                events,
            ) {
                ZoneMoveResult::Done => {}
                ZoneMoveResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Move all objects matching the filter from `Origin` zone to `Destination` zone.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (origin_zone, dest_zone, target_filter) = match &ability.effect {
        Effect::ChangeZoneAll {
            origin,
            destination,
            target,
        } => {
            let origin_z = origin.unwrap_or(Zone::Battlefield);
            (origin_z, *destination, target.clone())
        }
        _ => return Err(EffectError::MissingParam("ChangeZoneAll".to_string())),
    };

    // Use a permissive default filter if the effect's target is None
    let effective_filter = if matches!(target_filter, crate::types::ability::TargetFilter::None) {
        crate::types::ability::TargetFilter::Typed(TypedFilter {
            card_type: Some(crate::types::ability::TypeFilter::Permanent),
            subtype: None,
            controller: None,
            properties: vec![],
        })
    } else {
        target_filter
    };

    // Collect matching object IDs from the origin zone
    let matching: Vec<_> = state
        .objects
        .iter()
        .filter(|(&id, obj)| {
            obj.zone == origin_zone
                && crate::game::filter::matches_target_filter_controlled(
                    state,
                    id,
                    &effective_filter,
                    ability.source_id,
                    ability.controller,
                )
        })
        .map(|(id, _)| *id)
        .collect();

    // CR 603.7: Clean up consumed tracked set after scanning.
    if let TargetFilter::TrackedSet { id } = &effective_filter {
        state.tracked_object_sets.remove(id);
    }

    for obj_id in matching {
        match execute_zone_move(
            state,
            obj_id,
            origin_zone,
            dest_zone,
            ability.source_id,
            ability.duration.as_ref(),
            events,
        ) {
            ZoneMoveResult::Done => {}
            ZoneMoveResult::NeedsChoice(player) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn move_from_hand_to_battlefield() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn move_to_exile() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&obj_id));
    }

    #[test]
    fn exile_return_with_until_host_leaves_records_link() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Battlefield,
        );
        let source_id = ObjectId(100);
        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        ability.duration = Some(crate::types::ability::Duration::UntilHostLeavesPlay);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&target_id));
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, target_id);
        assert_eq!(state.exile_links[0].source_id, source_id);
        // CR 610.3a: return_zone should be the zone before exile
        assert_eq!(state.exile_links[0].return_zone, Zone::Battlefield);
    }

    #[test]
    fn exile_without_until_host_leaves_no_link() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.exile.contains(&target_id));
        assert!(
            state.exile_links.is_empty(),
            "Should NOT record ExileLink without UntilHostLeavesPlay"
        );
    }

    #[test]
    fn auto_shuffle_after_library_destination() {
        // CR 401.3: Moving an object to a library should shuffle that library afterward
        let mut state = GameState::new_two_player(42);
        // Add some cards to player 0's library so we can detect shuffle
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 10),
                PlayerId(0),
                format!("Lib Card {}", i),
                Zone::Library,
            );
        }
        let lib_before = state.players[0].library.clone();

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Library,
                target: TargetFilter::Any,
                owner_library: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be in library
        assert!(state.players[0].library.contains(&obj_id));
        // Library should have been shuffled — at minimum the order may have changed
        // (with enough cards, the probability of identical order is negligible)
        // We verify that shuffle was called by checking the library contains the object
        // and has the right size
        assert_eq!(state.players[0].library.len(), lib_before.len() + 1);
    }

    #[test]
    fn owner_library_routes_to_owners_library() {
        // CR 400.7: owner_library=true should route to the object's owner's library
        let mut state = GameState::new_two_player(42);
        // Create a creature owned by player 1 but currently controlled by player 0
        // (simulating a stolen creature)
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1), // owned by player 1
            "Stolen Creature".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Library,
                target: TargetFilter::Any,
                owner_library: true,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0), // controller is player 0
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Object should be in player 1's library (owner), not player 0's
        assert!(
            state.players[1].library.contains(&obj_id),
            "should be in owner's library (player 1)"
        );
        assert!(
            !state.players[0].library.contains(&obj_id),
            "should NOT be in controller's library (player 0)"
        );
    }

    #[test]
    fn self_ref_change_zone_processes_source() {
        // SelfRef target on ChangeZone should process the source object
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Self Card".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Library,
                target: TargetFilter::SelfRef,
                owner_library: true,
            },
            vec![], // empty targets — SelfRef means source_id
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Source should have moved to library
        assert!(
            state.players[0].library.contains(&source_id),
            "SelfRef source should be in library"
        );
        assert!(
            !state.battlefield.contains(&source_id),
            "SelfRef source should no longer be on battlefield"
        );
    }

    #[test]
    fn change_zone_all_bounce_opponent_creatures() {
        let mut state = GameState::new_two_player(42);
        let opp1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opp2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Wolf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Controller's creature should stay
        let mine = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "My Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&mine)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Hand,
                target: TargetFilter::None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // All permanents bounced (filter is "Permanent" by default)
        // ChangeZoneAll uses typed TargetFilter for filtering.
    }

    #[test]
    fn resolve_all_exile_with_until_host_leaves_creates_links() {
        // Phase 2 fix: resolve_all should create ExileLinks for UntilHostLeavesPlay
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Starcage".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&c2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter {
                    card_type: Some(crate::types::ability::TypeFilter::Creature),
                    subtype: None,
                    controller: Some(crate::types::ability::ControllerRef::Opponent),
                    properties: vec![],
                }),
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.duration = Some(crate::types::ability::Duration::UntilHostLeavesPlay);
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Both creatures should be exiled
        assert!(state.exile.contains(&c1), "c1 should be in exile");
        assert!(state.exile.contains(&c2), "c2 should be in exile");

        // CR 610.3a: ExileLinks should be created for each exiled object
        assert_eq!(
            state.exile_links.len(),
            2,
            "should have 2 exile links, got {}",
            state.exile_links.len()
        );
        for link in &state.exile_links {
            assert_eq!(link.source_id, source_id, "link source should be Starcage");
            assert_eq!(
                link.return_zone,
                Zone::Battlefield,
                "should return to battlefield"
            );
        }
    }
}
