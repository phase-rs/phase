use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Tap target permanents.
pub fn resolve_tap(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get_mut(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
            obj.tapped = true;
            events.push(GameEvent::PermanentTapped { object_id: *obj_id });
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Untap target permanents.
pub fn resolve_untap(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get_mut(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
            obj.tapped = false;
            events.push(GameEvent::PermanentUntapped { object_id: *obj_id });
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

    fn make_ability(api_type: &str, target: ObjectId) -> ResolvedAbility {
        ResolvedAbility {
            api_type: api_type.to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(target)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn tap_sets_tapped_true() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Land".to_string(), Zone::Battlefield);
        let mut events = Vec::new();

        resolve_tap(&mut state, &make_ability("Tap", obj_id), &mut events).unwrap();

        assert!(state.objects[&obj_id].tapped);
        assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    #[test]
    fn untap_sets_tapped_false() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(0), "Land".to_string(), Zone::Battlefield);
        state.objects.get_mut(&obj_id).unwrap().tapped = true;
        let mut events = Vec::new();

        resolve_untap(&mut state, &make_ability("Untap", obj_id), &mut events).unwrap();

        assert!(!state.objects[&obj_id].tapped);
        assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentUntapped { .. })));
    }
}
