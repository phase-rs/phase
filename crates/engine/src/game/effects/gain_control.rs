use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// GainControl: change target permanent's controller to the ability's controller.
pub fn resolve(
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
            obj.controller = ability.controller;
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
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_gain_control_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility {
            api_type: "GainControl".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(target)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn gain_control_changes_controller() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        assert_eq!(state.objects[&target_id].controller, PlayerId(1));

        let ability = make_gain_control_ability(target_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&target_id].controller, PlayerId(0));
    }

    #[test]
    fn gain_control_nonexistent_target_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_gain_control_ability(ObjectId(999));
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_err());
    }
}
