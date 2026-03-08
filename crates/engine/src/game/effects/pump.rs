use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Temporarily pump target creatures' power and toughness.
/// Reads `NumAtt` and `NumDef` params (default "0").
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_att: i32 = ability
        .params
        .get("NumAtt")
        .map(|v| v.parse().unwrap_or(0))
        .unwrap_or(0);
    let num_def: i32 = ability
        .params
        .get("NumDef")
        .map(|v| v.parse().unwrap_or(0))
        .unwrap_or(0);

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get_mut(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
            if let Some(ref mut power) = obj.power {
                *power += num_att;
            }
            if let Some(ref mut toughness) = obj.toughness {
                *toughness += num_def;
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
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    #[test]
    fn pump_increases_power_and_toughness() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Bear".to_string(), Zone::Battlefield);
        state.objects.get_mut(&obj_id).unwrap().power = Some(2);
        state.objects.get_mut(&obj_id).unwrap().toughness = Some(2);

        let ability = ResolvedAbility {
            api_type: "Pump".to_string(),
            params: HashMap::from([
                ("NumAtt".to_string(), "3".to_string()),
                ("NumDef".to_string(), "3".to_string()),
            ]),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].power, Some(5));
        assert_eq!(state.objects[&obj_id].toughness, Some(5));
    }

    #[test]
    fn pump_with_negative_values() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Bear".to_string(), Zone::Battlefield);
        state.objects.get_mut(&obj_id).unwrap().power = Some(3);
        state.objects.get_mut(&obj_id).unwrap().toughness = Some(3);

        let ability = ResolvedAbility {
            api_type: "Pump".to_string(),
            params: HashMap::from([
                ("NumAtt".to_string(), "-2".to_string()),
                ("NumDef".to_string(), "-2".to_string()),
            ]),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].power, Some(1));
        assert_eq!(state.objects[&obj_id].toughness, Some(1));
    }
}
