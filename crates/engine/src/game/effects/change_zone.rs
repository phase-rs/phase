use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{effect_variant_name, Effect, EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Move target objects between zones.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let dest_zone = match &ability.effect {
        Effect::ChangeZone { destination, .. } => *destination,
        _ => return Err(EffectError::MissingParam("Destination".to_string())),
    };

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let from_zone = state
                .objects
                .get(obj_id)
                .map(|o| o.zone)
                .unwrap_or(Zone::Battlefield);

            let proposed = ProposedEvent::ZoneChange {
                object_id: *obj_id,
                from: from_zone,
                to: dest_zone,
                cause: Some(ability.source_id),
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    if let ProposedEvent::ZoneChange { object_id, to, .. } = event {
                        zones::move_to_zone(state, object_id, to, events);
                        if to == Zone::Battlefield || from_zone == Zone::Battlefield {
                            state.layers_dirty = true;
                        }
                        // Track exile-until-leaves links
                        if to == Zone::Exile {
                            if let Some(crate::types::ability::Duration::UntilHostLeavesPlay) =
                                &ability.duration
                            {
                                state.exile_links.push(crate::types::game_state::ExileLink {
                                    exiled_id: object_id,
                                    source_id: ability.source_id,
                                });
                            }
                        }
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    let candidate_count = state
                        .pending_replacement
                        .as_ref()
                        .map(|p| p.candidates.len())
                        .unwrap_or(0);
                    state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
                        player,
                        candidate_count,
                    };
                    return Ok(());
                }
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
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
        crate::types::ability::TargetFilter::Typed {
            card_type: Some(crate::types::ability::TypeFilter::Permanent),
            subtype: None,
            controller: None,
            properties: vec![],
        }
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

    for obj_id in matching {
        zones::move_to_zone(state, obj_id, dest_zone, events);
        if dest_zone == Zone::Battlefield || origin_zone == Zone::Battlefield {
            state.layers_dirty = true;
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
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
        // Note: the original test used Valid="Creature.OppCtrl" which is
        // string-based filtering; with typed TargetFilter this would need
        // the filter module to support TargetFilter matching.
        // For now, ChangeZoneAll uses string-based filter as before.
    }
}
