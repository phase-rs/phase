use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
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
            zones::move_to_zone(state, *obj_id, Zone::Graveyard, events);
            events.push(GameEvent::PermanentSacrificed {
                object_id: *obj_id,
                player_id,
            });
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
    fn sacrifice_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Creature".to_string(), Zone::Battlefield);
        let ability = ResolvedAbility {
            api_type: "Sacrifice".to_string(),
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
    fn sacrifice_emits_permanent_sacrificed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Creature".to_string(), Zone::Battlefield);
        let ability = ResolvedAbility {
            api_type: "Sacrifice".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == obj_id && *player_id == PlayerId(0))));
    }
}
