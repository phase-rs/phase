use crate::game::game_object::CounterType;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Map counter type string to CounterType enum.
fn parse_counter_type(s: &str) -> CounterType {
    match s {
        "P1P1" => CounterType::Plus1Plus1,
        "M1M1" => CounterType::Minus1Minus1,
        "LOYALTY" => CounterType::Loyalty,
        other => CounterType::Generic(other.to_string()),
    }
}

/// Add counters to target objects.
/// Reads `CounterType` and `CounterNum` params.
pub fn resolve_add(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let counter_type_str = ability
        .params
        .get("CounterType")
        .cloned()
        .unwrap_or_else(|| "P1P1".to_string());
    let counter_num: u32 = ability
        .params
        .get("CounterNum")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    let ct = parse_counter_type(&counter_type_str);

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get_mut(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
            let entry = obj.counters.entry(ct.clone()).or_insert(0);
            *entry += counter_num;

            events.push(GameEvent::CounterAdded {
                object_id: *obj_id,
                counter_type: counter_type_str.clone(),
                count: counter_num,
            });
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Remove counters from target objects, clamping at 0.
/// Reads `CounterType` and `CounterNum` params.
pub fn resolve_remove(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let counter_type_str = ability
        .params
        .get("CounterType")
        .cloned()
        .unwrap_or_else(|| "P1P1".to_string());
    let counter_num: u32 = ability
        .params
        .get("CounterNum")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    let ct = parse_counter_type(&counter_type_str);

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get_mut(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
            let entry = obj.counters.entry(ct.clone()).or_insert(0);
            *entry = entry.saturating_sub(counter_num);

            events.push(GameEvent::CounterRemoved {
                object_id: *obj_id,
                counter_type: counter_type_str.clone(),
                count: counter_num,
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
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_counter_ability(api_type: &str, ct: &str, num: u32, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility {
            api_type: api_type.to_string(),
            params: HashMap::from([
                ("CounterType".to_string(), ct.to_string()),
                ("CounterNum".to_string(), num.to_string()),
            ]),
            targets: vec![TargetRef::Object(target)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn add_counter_increments() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Creature".to_string(), Zone::Battlefield);
        let mut events = Vec::new();

        resolve_add(&mut state, &make_counter_ability("AddCounter", "P1P1", 2, obj_id), &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].counters[&CounterType::Plus1Plus1], 2);
    }

    #[test]
    fn remove_counter_decrements_clamped() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Creature".to_string(), Zone::Battlefield);
        state.objects.get_mut(&obj_id).unwrap().counters.insert(CounterType::Plus1Plus1, 1);
        let mut events = Vec::new();

        resolve_remove(&mut state, &make_counter_ability("RemoveCounter", "P1P1", 3, obj_id), &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].counters[&CounterType::Plus1Plus1], 0);
    }

    #[test]
    fn add_generic_counter() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Artifact".to_string(), Zone::Battlefield);
        let mut events = Vec::new();

        resolve_add(&mut state, &make_counter_ability("AddCounter", "charge", 3, obj_id), &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].counters[&CounterType::Generic("charge".to_string())], 3);
    }

    #[test]
    fn add_counter_emits_counter_added_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Creature".to_string(), Zone::Battlefield);
        let mut events = Vec::new();

        resolve_add(&mut state, &make_counter_ability("AddCounter", "P1P1", 1, obj_id), &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::CounterAdded { counter_type, count: 1, .. } if counter_type == "P1P1")));
    }
}
