use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    effect_variant_name, Effect, EffectError, ResolvedAbility, TargetRef, TargetSpec,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Destroy target creatures/permanents on the battlefield.
/// Skips objects with the "indestructible" keyword.
/// Moves destroyed objects to their owner's graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;

            // Skip if not on battlefield
            if obj.zone != Zone::Battlefield {
                continue;
            }

            // Check for indestructible
            if obj.has_keyword(&crate::types::keywords::Keyword::Indestructible) {
                continue;
            }

            let proposed = ProposedEvent::Destroy {
                object_id: *obj_id,
                source: Some(ability.source_id),
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Destroy {
                            object_id, source, ..
                        } => {
                            // Destruction resolved -- now create a ZoneChange proposal
                            // so Moved replacements can intercept the actual zone transfer
                            let zone_proposed = ProposedEvent::ZoneChange {
                                object_id,
                                from: Zone::Battlefield,
                                to: Zone::Graveyard,
                                cause: source,
                                applied: HashSet::new(),
                            };
                            match replacement::replace_event(state, zone_proposed, events) {
                                ReplacementResult::Execute(zone_event) => {
                                    if let ProposedEvent::ZoneChange {
                                        object_id: oid, to, ..
                                    } = zone_event
                                    {
                                        zones::move_to_zone(state, oid, to, events);
                                        state.layers_dirty = true;
                                    }
                                }
                                ReplacementResult::Prevented => {}
                                ReplacementResult::NeedsChoice(player) => {
                                    let candidate_count = state
                                        .pending_replacement
                                        .as_ref()
                                        .map(|p| p.candidates.len())
                                        .unwrap_or(0);
                                    state.waiting_for =
                                        crate::types::game_state::WaitingFor::ReplacementChoice {
                                            player,
                                            candidate_count,
                                        };
                                    return Ok(());
                                }
                            }
                            events.push(GameEvent::CreatureDestroyed { object_id });
                        }
                        ProposedEvent::ZoneChange { object_id, to, .. } => {
                            // Destroy replacement redirected directly to a zone change
                            zones::move_to_zone(state, object_id, to, events);
                            state.layers_dirty = true;
                        }
                        _ => {}
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

/// Destroy all permanents matching the filter.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let filter = match &ability.effect {
        Effect::DestroyAll {
            target: TargetSpec::All { filter },
        } if !filter.is_empty() => filter.as_str(),
        Effect::DestroyAll { .. } => "Creature",
        _ => ability
            .params
            .get("Valid")
            .map(|s| s.as_str())
            .unwrap_or("Creature"),
    };

    // Collect matching object IDs that are on the battlefield and not indestructible
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            let is_indestructible = state
                .objects
                .get(id)
                .map(|obj| obj.has_keyword(&crate::types::keywords::Keyword::Indestructible))
                .unwrap_or(false);
            !is_indestructible
                && crate::game::filter::object_matches_filter_controlled(
                    state,
                    **id,
                    filter,
                    ability.source_id,
                    ability.controller,
                )
        })
        .copied()
        .collect();

    for obj_id in matching {
        zones::move_to_zone(state, obj_id, Zone::Graveyard, events);
        state.layers_dirty = true;
        events.push(GameEvent::CreatureDestroyed { object_id: obj_id });
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
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn destroy_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Destroy".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn destroy_skips_indestructible() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "God".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Indestructible);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Destroy".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
    }

    #[test]
    fn destroy_emits_creature_destroyed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Destroy".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CreatureDestroyed { object_id } if *object_id == obj_id)
        ));
    }

    #[test]
    fn destroy_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Non-creature should survive
        let _land = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "DestroyAll".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::from([("Valid".to_string(), "Creature".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&bear1));
        assert!(!state.battlefield.contains(&bear2));
        // Land survives
        assert_eq!(state.battlefield.len(), 1);
    }
}
