use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{effect_variant_name, EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Sacrifice target permanents controlled by the ability's controller.
/// Moves them to their owner's graveyard.
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

            // Only sacrifice things on the battlefield
            if obj.zone != Zone::Battlefield {
                continue;
            }

            let player_id = obj.controller;

            let proposed = ProposedEvent::Sacrifice {
                object_id: *obj_id,
                player_id,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    match event {
                        ProposedEvent::Sacrifice {
                            object_id,
                            player_id: pid,
                            ..
                        } => {
                            zones::move_to_zone(state, object_id, Zone::Graveyard, events);
                            state.layers_dirty = true;
                            events.push(GameEvent::PermanentSacrificed {
                                object_id,
                                player_id: pid,
                            });
                        }
                        ProposedEvent::ZoneChange { object_id, to, .. } => {
                            // Replacement redirected (e.g., exile instead of graveyard)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_sacrifice_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn sacrifice_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = make_sacrifice_ability(obj_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn sacrifice_emits_permanent_sacrificed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = make_sacrifice_ability(obj_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == obj_id && *player_id == PlayerId(0))));
    }
}
