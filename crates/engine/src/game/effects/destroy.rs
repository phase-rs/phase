use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetRef, TypedFilter,
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

            // CR 114.4: Emblems cannot be destroyed
            if obj.is_emblem {
                continue;
            }

            // Skip if not on battlefield
            if obj.zone != Zone::Battlefield {
                continue;
            }

            // Check for indestructible
            if obj.has_keyword(&crate::types::keywords::Keyword::Indestructible) {
                continue;
            }

            let cant_regenerate = matches!(
                &ability.effect,
                Effect::Destroy {
                    cant_regenerate: true,
                    ..
                }
            );
            let proposed = ProposedEvent::Destroy {
                object_id: *obj_id,
                source: Some(ability.source_id),
                cant_regenerate,
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
                            let zone_proposed = ProposedEvent::zone_change(
                                object_id,
                                Zone::Battlefield,
                                Zone::Graveyard,
                                source,
                            );
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
                                    state.waiting_for =
                                        crate::game::replacement::replacement_choice_waiting_for(
                                            player, state,
                                        );
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

/// Destroy all permanents matching the filter.
/// CR 701.15: Routes each destruction through the replacement pipeline
/// so regeneration shields and other replacements can intercept.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, cant_regenerate) = match &ability.effect {
        Effect::DestroyAll {
            target,
            cant_regenerate,
        } => (target.clone(), *cant_regenerate),
        _ => (crate::types::ability::TargetFilter::Any, false),
    };

    // Use a creature filter as default if the effect's target is None
    let effective_filter = if matches!(target_filter, crate::types::ability::TargetFilter::None) {
        crate::types::ability::TargetFilter::Typed(TypedFilter {
            card_type: Some(crate::types::ability::TypeFilter::Creature),
            subtype: None,
            controller: None,
            properties: vec![],
        })
    } else {
        target_filter
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
                && crate::game::filter::matches_target_filter_controlled(
                    state,
                    **id,
                    &effective_filter,
                    ability.source_id,
                    ability.controller,
                )
        })
        .copied()
        .collect();

    for obj_id in matching {
        let proposed = ProposedEvent::Destroy {
            object_id: obj_id,
            source: Some(ability.source_id),
            cant_regenerate,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => match event {
                ProposedEvent::Destroy {
                    object_id, source, ..
                } => {
                    let zone_proposed = ProposedEvent::zone_change(
                        object_id,
                        Zone::Battlefield,
                        Zone::Graveyard,
                        source,
                    );
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
                            state.waiting_for =
                                replacement::replacement_choice_waiting_for(player, state);
                            return Ok(());
                        }
                    }
                    events.push(GameEvent::CreatureDestroyed { object_id });
                }
                ProposedEvent::ZoneChange { object_id, to, .. } => {
                    zones::move_to_zone(state, object_id, to, events);
                    state.layers_dirty = true;
                }
                _ => {}
            },
            ReplacementResult::Prevented => {} // Regenerated or other replacement
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
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
    fn destroy_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
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

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
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
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
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

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&bear1));
        assert!(!state.battlefield.contains(&bear2));
        // Land survives
        assert_eq!(state.battlefield.len(), 1);
    }

    #[test]
    fn destroy_prevented_by_regen_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Add regeneration shield
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature survived
        assert!(
            state.battlefield.contains(&bear_id),
            "Creature with regen shield should survive Destroy"
        );
        // No CreatureDestroyed event
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CreatureDestroyed { .. })));
        // Regenerated event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Regenerated { .. })));
    }

    #[test]
    fn destroy_all_prevented_by_regen_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Protected creature
        let protected_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&protected_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&protected_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        // Unprotected creature
        let unprotected_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Unshielded".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&unprotected_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Protected creature survives
        assert!(
            state.battlefield.contains(&protected_id),
            "Creature with regen shield should survive DestroyAll"
        );
        // Unprotected creature destroyed
        assert!(
            !state.battlefield.contains(&unprotected_id),
            "Unshielded creature should be destroyed by DestroyAll"
        );
    }

    #[test]
    fn destroy_all_cant_regenerate_bypasses_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.battlefield.contains(&bear_id),
            "cant_regenerate should bypass regen shield in DestroyAll"
        );
    }
}
