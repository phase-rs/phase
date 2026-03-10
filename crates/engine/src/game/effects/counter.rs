use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// Counter target spells on the stack.
/// Removes them from the stack and moves to graveyard.
/// Respects CantBeCountered static ability.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // Check if the spell has CantBeCountered
            let ctx = StaticCheckContext {
                source_id: Some(*obj_id),
                target_id: Some(*obj_id),
                ..Default::default()
            };
            if check_static_ability(state, "CantBeCountered", &ctx) {
                // Spell cannot be countered -- skip it
                continue;
            }

            // Also check if the object itself has a CantBeCountered static definition
            let has_cant_be_countered = state
                .objects
                .get(obj_id)
                .map(|obj| {
                    obj.static_definitions
                        .iter()
                        .any(|sd| sd.mode_str() == "CantBeCountered")
                })
                .unwrap_or(false);
            if has_cant_be_countered {
                continue;
            }

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
        api_type: ability.api_type().to_string(),
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
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: ResolvedAbility {
                    effect: crate::types::ability::Effect::Other {
                        api_type: String::new(),
                        params: std::collections::HashMap::new(),
                    },
                    params: HashMap::new(),
                    targets: vec![],
                    source_id: obj_id,
                    controller: PlayerId(1),
                    sub_ability: None,
                    svars: HashMap::new(),
                },
            },
        });

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Counter".to_string(),
                params: std::collections::HashMap::new(),
            },
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
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::SpellCountered { .. })));
    }

    #[test]
    fn cant_be_countered_spell_stays_on_stack() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Uncounterable".to_string(),
            Zone::Stack,
        );
        // Add CantBeCountered static definition to the spell
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition {
                mode: StaticMode::Other("CantBeCountered".to_string()),
                params: HashMap::new(),
            });
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: ResolvedAbility {
                    effect: crate::types::ability::Effect::Other {
                        api_type: String::new(),
                        params: std::collections::HashMap::new(),
                    },
                    params: HashMap::new(),
                    targets: vec![],
                    source_id: obj_id,
                    controller: PlayerId(1),
                    sub_ability: None,
                    svars: HashMap::new(),
                },
            },
        });

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Counter".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::new(),
            targets: vec![TargetRef::Object(obj_id)],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Spell should still be on the stack (not countered)
        assert_eq!(state.stack.len(), 1);
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::SpellCountered { .. })));
    }
}
