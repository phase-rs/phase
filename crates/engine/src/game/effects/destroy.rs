use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
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
                        ProposedEvent::Destroy { object_id, source, .. } => {
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
                                    if let ProposedEvent::ZoneChange { object_id: oid, to, .. } = zone_event {
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
                                    state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
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
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn destroy_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Bear".to_string(), Zone::Battlefield);
        let ability = ResolvedAbility {
            api_type: "Destroy".to_string(),
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
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "God".to_string(), Zone::Battlefield);
        state.objects.get_mut(&obj_id).unwrap().keywords.push(crate::types::keywords::Keyword::Indestructible);

        let ability = ResolvedAbility {
            api_type: "Destroy".to_string(),
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
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Bear".to_string(), Zone::Battlefield);
        let ability = ResolvedAbility {
            api_type: "Destroy".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::CreatureDestroyed { object_id } if *object_id == obj_id)));
    }
}
