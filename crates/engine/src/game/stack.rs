use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::ability_utils::{flatten_targets_in_chain, validate_targets_in_chain};
use super::effects;
use super::targeting;
use super::zones;

/// CR 405.1: Add an object to the stack.
pub fn push_to_stack(state: &mut GameState, entry: StackEntry, events: &mut Vec<GameEvent>) {
    events.push(GameEvent::StackPushed {
        object_id: entry.id,
    });
    state.stack.push(entry);
}

/// CR 608.2: Resolve the top object on the stack.
pub fn resolve_top(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let entry = match state.stack.pop() {
        Some(e) => e,
        None => return,
    };

    // CR 603.4: Intervening-if condition rechecked at resolution time.
    if let StackEntryKind::TriggeredAbility {
        condition: Some(ref condition),
        source_id,
        ..
    } = entry.kind
    {
        if !super::triggers::check_trigger_condition(
            state,
            condition,
            entry.controller,
            Some(source_id),
        ) {
            events.push(GameEvent::StackResolved {
                object_id: entry.id,
            });
            return;
        }
    }

    // CR 603.7c: Set trigger event context for event-context target resolution.
    // TriggeringSpellController, TriggeringSource, etc. read this during resolution.
    if let StackEntryKind::TriggeredAbility {
        trigger_event: Some(ref te),
        ..
    } = entry.kind
    {
        state.current_trigger_event = Some(te.clone());
    }

    // Extract the resolved ability from the stack entry
    let (ability, is_spell, cast_as_adventure) = match &entry.kind {
        StackEntryKind::Spell {
            ability,
            cast_as_adventure,
            ..
        } => (ability.clone(), true, *cast_as_adventure),
        StackEntryKind::ActivatedAbility { ability, .. } => (ability.clone(), false, false),
        StackEntryKind::TriggeredAbility { ability, .. } => (ability.clone(), false, false),
    };

    // Capture targets for Aura attachment after resolution
    let spell_targets = ability.targets.clone();

    let original_targets = flatten_targets_in_chain(&ability);
    if !original_targets.is_empty() {
        let validated = validate_targets_in_chain(state, &ability);
        let legal_targets = flatten_targets_in_chain(&validated);
        if targeting::check_fizzle(&original_targets, &legal_targets) {
            // CR 608.2b: Fizzle — all targets illegal, spell is countered on resolution.
            if is_spell {
                zones::move_to_zone(state, entry.id, Zone::Graveyard, events);
            }
            events.push(GameEvent::StackResolved {
                object_id: entry.id,
            });
            return;
        }
        execute_effect(state, &validated, events);
    } else {
        execute_effect(state, &ability, events);
    }

    // CR 608.3: Determine destination zone for spells.
    if is_spell {
        let dest = if cast_as_adventure {
            // CR 715.4: Adventure spell resolves → exile with casting permission.
            Zone::Exile
        } else if is_permanent_type(state, entry.id) {
            // CR 608.3: Permanent spells enter the battlefield.
            Zone::Battlefield
        } else {
            // CR 608.2: Non-permanent spells are put into owner's graveyard.
            Zone::Graveyard
        };
        zones::move_to_zone(state, entry.id, dest, events);

        // CR 715.4: When an Adventure spell resolves to exile, restore the creature face
        // and grant AdventureCreature permission so it can be cast from exile.
        if cast_as_adventure {
            if let Some(obj) = state.objects.get_mut(&entry.id) {
                // Restore creature face characteristics (swap back from Adventure face)
                if let Some(creature_face) = obj.back_face.take() {
                    let adventure_snapshot = super::printed_cards::snapshot_object_face(obj);
                    super::printed_cards::apply_back_face_to_object(obj, creature_face);
                    obj.back_face = Some(adventure_snapshot);
                }
                obj.casting_permissions
                    .push(crate::types::ability::CastingPermission::AdventureCreature);
            }
        }

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

    // CR 603.7c: Clear trigger event context after resolution completes.
    state.current_trigger_event = None;

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
    use crate::types::ability::{
        Effect, QuantityExpr, ResolvedAbility, TargetRef, TypedFilter,
    };
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
                crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
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
                cast_as_adventure: false,
            },
        });

        aura_id
    }

    #[test]
    fn trigger_event_context_becomes_target_controller() {
        // Set up: triggered ability with BecomesTarget event in trigger_event.
        // Verify: at resolution, current_trigger_event is set so
        // TriggeringSpellController can resolve to the controller of the source.
        let mut state = setup();

        // Create a "spell" object controlled by player 1 that is the source in BecomesTarget
        let spell_id = create_object(
            &mut state,
            CardId(80),
            PlayerId(1),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );

        let trigger_event = GameEvent::BecomesTarget {
            object_id: ObjectId(999), // target doesn't matter for this test
            source_id: spell_id,
        };

        // Build a triggered ability that would want to resolve TriggeringSpellController
        let resolved = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "EventContextTest".to_string(),
                description: None,
            },
            vec![],
            ObjectId(50),
            PlayerId(0),
        );

        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;

        state.stack.push(StackEntry {
            id: entry_id,
            source_id: ObjectId(50),
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(50),
                ability: resolved,
                condition: None,
                trigger_event: Some(trigger_event.clone()),
            },
        });

        // Before resolution, current_trigger_event should be None
        assert!(state.current_trigger_event.is_none());

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // After resolution, current_trigger_event should be cleared
        assert!(state.current_trigger_event.is_none());

        // Verify the event was set during resolution by checking the resolve happened
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::StackResolved { .. })));

        // Verify event-context resolution works with the trigger event
        // by manually setting and checking the resolution function
        state.current_trigger_event = Some(trigger_event);
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellController,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(1))));

        // TriggeringSpellOwner should return the owner
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellOwner,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(1))));

        // TriggeringSource should return the source object
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSource,
            ObjectId(50),
        );
        assert_eq!(result, Some(TargetRef::Object(spell_id)));

        // Clean up
        state.current_trigger_event = None;
    }

    #[test]
    fn trigger_event_context_no_event_returns_none() {
        let state = setup();
        // With no current_trigger_event, resolution should return None
        let result = crate::game::targeting::resolve_event_context_target(
            &state,
            &crate::types::ability::TargetFilter::TriggeringSpellController,
            ObjectId(1),
        );
        assert!(result.is_none());
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
                cast_as_adventure: false,
            },
        });

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        // Should be on battlefield, not attached to anything
        assert!(state.battlefield.contains(&ench_id));
        assert_eq!(state.objects.get(&ench_id).unwrap().attached_to, None);
    }

    #[test]
    fn multi_target_chain_resolves_remaining_legal_target() {
        let mut state = setup();

        let first_target = create_object(
            &mut state,
            CardId(70),
            PlayerId(1),
            "First Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&first_target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        let second_target = create_object(
            &mut state,
            CardId(71),
            PlayerId(1),
            "Second Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&second_target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
        }

        let spell_id = create_object(
            &mut state,
            CardId(72),
            PlayerId(0),
            "Twin Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![TargetRef::Object(first_target)],
            spell_id,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: crate::types::ability::TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![TargetRef::Object(second_target)],
            spell_id,
            PlayerId(0),
        ));

        state.stack.push(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(72),
                ability,
                cast_as_adventure: false,
            },
        });

        state.battlefield.retain(|&id| id != first_target);
        state.objects.get_mut(&first_target).unwrap().zone = Zone::Graveyard;
        state.players[1].graveyard.push(first_target);

        let mut events = Vec::new();
        resolve_top(&mut state, &mut events);

        assert!(state.players[0].graveyard.contains(&spell_id));
        assert_eq!(state.objects[&second_target].damage_marked, 2);
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::DamageDealt {
                    target: TargetRef::Object(target),
                    amount: 2,
                    ..
                } if *target == second_target
            )),
            "expected the remaining legal target to be damaged"
        );
    }
}
