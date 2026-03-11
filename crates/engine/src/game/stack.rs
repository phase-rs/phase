use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::effects;
use super::targeting;
use super::zones;

pub fn push_to_stack(state: &mut GameState, entry: StackEntry, events: &mut Vec<GameEvent>) {
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    state.stack.push(entry);
}

pub fn resolve_top(state: &mut GameState, events: &mut Vec<GameEvent>) {
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
        let valid_tgts = extract_target_filter_string(&ability.effect);
        let legal = targeting::validate_targets(
            state,
            &ability.targets,
            &valid_tgts,
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
        execute_effect(state, &ability, events);
    } else {
        execute_effect(state, &ability, events);
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
    state: &mut GameState,
    ability: &crate::types::ability::ResolvedAbility,
    events: &mut Vec<GameEvent>,
) {
    // Skip unimplemented effects (logged elsewhere as warnings)
    if matches!(
        ability.effect,
        crate::types::ability::Effect::Unimplemented { .. }
    ) {
        return;
    }
    // Use resolve_ability_chain to support SubAbility/Execute chaining
    let _ = effects::resolve_ability_chain(state, ability, events, 0);
}

pub fn stack_is_empty(state: &GameState) -> bool {
    state.stack.is_empty()
}

/// Extract a string-based target filter from a typed Effect for fizzle validation.
/// Bridges the typed TargetFilter to the string-based targeting system.
fn extract_target_filter_string(effect: &crate::types::ability::Effect) -> String {
    use crate::types::ability::{Effect, TargetFilter};
    let target = match effect {
        Effect::DealDamage { target, .. }
        | Effect::Pump { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Counter { target, .. }
        | Effect::Tap { target, .. }
        | Effect::Untap { target, .. }
        | Effect::Sacrifice { target, .. }
        | Effect::GainControl { target, .. }
        | Effect::Attach { target, .. }
        | Effect::Fight { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::CopySpell { target, .. } => target,
        _ => return "Any".to_string(),
    };
    match target {
        TargetFilter::Any => "Any".to_string(),
        TargetFilter::Player => "Player".to_string(),
        TargetFilter::Controller => "Player.You".to_string(),
        TargetFilter::Typed {
            card_type,
            controller,
            ..
        } => {
            let type_str = match card_type {
                Some(crate::types::ability::TypeFilter::Creature) => "Creature",
                Some(crate::types::ability::TypeFilter::Land) => "Land",
                Some(crate::types::ability::TypeFilter::Artifact) => "Artifact",
                Some(crate::types::ability::TypeFilter::Enchantment) => "Enchantment",
                _ => "Any",
            };
            let ctrl_str = match controller {
                Some(crate::types::ability::ControllerRef::You) => ".YouCtrl",
                Some(crate::types::ability::ControllerRef::Opponent) => ".OppCtrl",
                None => "",
            };
            format!("{}{}", type_str, ctrl_str)
        }
        _ => "Any".to_string(),
    }
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
