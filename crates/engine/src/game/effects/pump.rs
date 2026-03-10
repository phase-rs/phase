use crate::game::filter::object_matches_filter_controlled;
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

/// Pump all creatures matching the `Valid` filter on the battlefield.
/// Reads `NumAtt`, `NumDef`, and `Valid` params.
pub fn resolve_all(
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
    let filter = ability
        .params
        .get("Valid")
        .map(|s| s.as_str())
        .unwrap_or("Creature");

    // Collect matching object IDs first to avoid borrow conflicts
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            object_matches_filter_controlled(
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
        if let Some(obj) = state.objects.get_mut(&obj_id) {
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
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    #[test]
    fn pump_increases_power_and_toughness() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().power = Some(2);
        state.objects.get_mut(&obj_id).unwrap().toughness = Some(2);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Pump".to_string(),
                params: std::collections::HashMap::new(),
            },
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
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().power = Some(3);
        state.objects.get_mut(&obj_id).unwrap().toughness = Some(3);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Pump".to_string(),
                params: std::collections::HashMap::new(),
            },
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

    #[test]
    fn pump_all_your_creatures() {
        let mut state = GameState::new_two_player(42);
        // Controller's creatures
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear1).unwrap().power = Some(2);
        state.objects.get_mut(&bear1).unwrap().toughness = Some(2);
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
            PlayerId(0),
            "Bear 2".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear2).unwrap().power = Some(1);
        state.objects.get_mut(&bear2).unwrap().toughness = Some(1);
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Opponent's creature (should NOT be pumped)
        let opp = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&opp).unwrap().power = Some(3);
        state.objects.get_mut(&opp).unwrap().toughness = Some(3);
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "PumpAll".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "PumpAll".to_string(),
            params: HashMap::from([
                ("NumAtt".to_string(), "1".to_string()),
                ("NumDef".to_string(), "1".to_string()),
                ("Valid".to_string(), "Creature.YouCtrl".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].power, Some(3));
        assert_eq!(state.objects[&bear1].toughness, Some(3));
        assert_eq!(state.objects[&bear2].power, Some(2));
        assert_eq!(state.objects[&bear2].toughness, Some(2));
        // Opponent unchanged
        assert_eq!(state.objects[&opp].power, Some(3));
        assert_eq!(state.objects[&opp].toughness, Some(3));
    }
}
