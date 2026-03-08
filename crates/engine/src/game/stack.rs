use std::collections::HashMap;

use crate::types::ability::ResolvedAbility;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::effects::{self, EffectHandler};
use super::targeting;
use super::zones;

pub fn push_to_stack(
    state: &mut GameState,
    entry: StackEntry,
    events: &mut Vec<GameEvent>,
) {
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    state.stack.push(entry);
}

pub fn resolve_top(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    registry: &HashMap<String, EffectHandler>,
) {
    let entry = match state.stack.pop() {
        Some(e) => e,
        None => return,
    };

    // Extract the resolved ability from the stack entry
    let (ability, is_spell) = match &entry.kind {
        StackEntryKind::Spell { ability, .. } => (ability.clone(), true),
        StackEntryKind::ActivatedAbility { ability, .. } => (ability.clone(), false),
        StackEntryKind::TriggeredAbility { ability, .. } => (ability.clone(), false),
    };

    // Run fizzle check if the ability has targets
    if !ability.targets.is_empty() {
        if let Some(valid_tgts) = ability.params.get("ValidTgts") {
            let legal = targeting::validate_targets(
                state,
                &ability.targets,
                valid_tgts,
                ability.controller,
                ability.source_id,
            );
            if targeting::check_fizzle(&ability.targets, &legal) {
                // Fizzle: all targets illegal -- move card to graveyard without executing
                if is_spell {
                    zones::move_to_zone(state, entry.id, Zone::Graveyard, events);
                }
                events.push(GameEvent::StackResolved {
                    object_id: entry.id,
                });
                return;
            }
            // Update ability with only still-legal targets
            let mut ability = ability;
            ability.targets = legal;
            execute_effect(registry, state, &ability, events);
        } else {
            execute_effect(registry, state, &ability, events);
        }
    } else {
        execute_effect(registry, state, &ability, events);
    }

    // Determine destination zone for spells
    if is_spell {
        let dest = if is_permanent_type(state, entry.id) {
            Zone::Battlefield
        } else {
            Zone::Graveyard
        };
        zones::move_to_zone(state, entry.id, dest, events);
    }
    // Activated abilities: source stays where it is, no zone movement

    events.push(GameEvent::StackResolved {
        object_id: entry.id,
    });
}

fn execute_effect(
    registry: &HashMap<String, EffectHandler>,
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    if ability.api_type.is_empty() {
        return; // No-op ability (used in tests with empty api_type)
    }
    // Use resolve_ability_chain to support SubAbility/Execute chaining
    let _ = effects::resolve_ability_chain(state, ability, events, registry, 0);
}

pub fn stack_is_empty(state: &GameState) -> bool {
    state.stack.is_empty()
}

fn is_permanent_type(state: &GameState, object_id: ObjectId) -> bool {
    use crate::types::card_type::CoreType;

    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    obj.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            CoreType::Creature
                | CoreType::Artifact
                | CoreType::Enchantment
                | CoreType::Planeswalker
                | CoreType::Land
        )
    })
}
