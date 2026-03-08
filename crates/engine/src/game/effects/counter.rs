use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// Counter target spells on the stack.
/// Removes them from the stack and moves to graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // Remove from stack
            let stack_idx = state.stack.iter().position(|e| e.id == *obj_id);
            if let Some(idx) = stack_idx {
                state.stack.remove(idx);
                // Move to graveyard
                zones::move_to_zone(state, *obj_id, Zone::Graveyard, events);
                events.push(GameEvent::SpellCountered {
                    object_id: *obj_id,
                    countered_by: ability.source_id,
                });
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
    use crate::types::game_state::{StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn counter_removes_from_stack_and_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(1), "Spell".to_string(), Zone::Stack);
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell { card_id: CardId(1) },
        });

        let ability = ResolvedAbility {
            api_type: "Counter".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.stack.is_empty());
        assert!(state.players[1].graveyard.contains(&obj_id));
        assert!(events.iter().any(|e| matches!(e, GameEvent::SpellCountered { .. })));
    }
}
