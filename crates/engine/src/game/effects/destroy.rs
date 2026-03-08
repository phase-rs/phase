use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
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
            if obj.keywords.iter().any(|k| k.eq_ignore_ascii_case("indestructible")) {
                continue;
            }

            zones::move_to_zone(state, *obj_id, Zone::Graveyard, events);
            events.push(GameEvent::CreatureDestroyed { object_id: *obj_id });
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
        state.objects.get_mut(&obj_id).unwrap().keywords.push("Indestructible".to_string());

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
