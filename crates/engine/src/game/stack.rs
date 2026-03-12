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

    // Intervening-if: recheck condition at resolution time for triggered abilities
    if let StackEntryKind::TriggeredAbility {
        condition: Some(ref condition),
        ..
    } = entry.kind
    {
        if !super::triggers::check_trigger_condition(state, condition, entry.controller) {
            events.push(GameEvent::StackResolved {
                object_id: entry.id,
            });
            return;
        }
    }

    // Extract the resolved ability from the stack entry
    let (ability, is_spell) = match &entry.kind {
        StackEntryKind::Spell { ability, .. } => (ability.clone(), true),
        StackEntryKind::ActivatedAbility { ability, .. } => (ability.clone(), false),
        StackEntryKind::TriggeredAbility { ability, .. } => (ability.clone(), false),
    };

    // Capture targets for Aura attachment after resolution
    let spell_targets = ability.targets.clone();

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

        // Aura attachment: if the permanent is an Aura with targets, attach to the first target
        if dest == Zone::Battlefield {
            let is_aura = state
                .objects
                .get(&entry.id)
                .map(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
                .unwrap_or(false);
            if is_aura {
                if let Some(crate::types::ability::TargetRef::Object(target_id)) =
                    spell_targets.first()
                {
                    // Verify target is still on the battlefield
                    if state.battlefield.contains(target_id) {
                        effects::attach::attach_to(state, entry.id, *target_id);
                    }
                    // If target is gone, SBA check_unattached_auras will handle cleanup
                }
            }
        }
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
        Effect::GenericEffect {
            target: Some(target),
            ..
        } => target,
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
                Some(crate::types::ability::TypeFilter::Card) => "Card",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetRef};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    fn create_aura_on_stack(state: &mut GameState, target_id: ObjectId) -> ObjectId {
        let aura_id = create_object(
            state,
            CardId(100),
            PlayerId(0),
            "Pacifism".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&aura_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords.push(Keyword::Enchant(
                crate::types::ability::TargetFilter::Typed {
                    card_type: Some(crate::types::ability::TypeFilter::Creature),
                    subtype: None,
                    controller: None,
                    properties: vec![],
                },
            ));
        }

        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Aura".to_string(),
                description: None,
            },
            vec![TargetRef::Object(target_id)],
            aura_id,
            PlayerId(0),
        );

        state.stack.push(StackEntry {
            id: aura_id,
            source_id: aura_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: resolved,
            },
        });

        aura_id
    }

    #[test]
    fn aura_resolving_attaches_to_target() {
        let mut state = setup();

        // Create a creature on the battlefield
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Create an Aura spell targeting the creature
        let aura_id = create_aura_on_stack(&mut state, creature);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Aura should be on the battlefield
        assert!(state.battlefield.contains(&aura_id));
        // Aura should be attached to the creature
        assert_eq!(
            state.objects.get(&aura_id).unwrap().attached_to,
            Some(creature)
        );
        // Creature should list the Aura in its attachments
        assert!(state
            .objects
            .get(&creature)
            .unwrap()
            .attachments
            .contains(&aura_id));
    }

    #[test]
    fn aura_fizzles_when_target_left_battlefield() {
        let mut state = setup();

        // Create a creature, then remove it from battlefield before resolution
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let aura_id = create_aura_on_stack(&mut state, creature);

        // Remove creature from battlefield before resolution
        state.battlefield.retain(|&id| id != creature);
        if let Some(obj) = state.objects.get_mut(&creature) {
            obj.zone = Zone::Graveyard;
        }
        state.players[1].graveyard.push(creature);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Aura should fizzle to graveyard (not to battlefield)
        assert!(!state.battlefield.contains(&aura_id));
        assert!(state.players[0].graveyard.contains(&aura_id));
    }

    #[test]
    fn non_aura_permanent_resolving_no_attachment() {
        let mut state = setup();

        // Create a non-Aura enchantment on the stack
        let ench_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Intangible Virtue".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&ench_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Enchantment);

        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "GlobalEnchantment".to_string(),
                description: None,
            },
            vec![],
            ench_id,
            PlayerId(0),
        );

        state.stack.push(StackEntry {
            id: ench_id,
            source_id: ench_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(60),
                ability: resolved,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Should be on battlefield, not attached to anything
        assert!(state.battlefield.contains(&ench_id));
        assert_eq!(state.objects.get(&ench_id).unwrap().attached_to, None);
    }
}
