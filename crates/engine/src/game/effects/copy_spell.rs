use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{CopyTargetSlot, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// CR 707.10: Copy a spell — create a copy on the stack with the same characteristics and choices.
/// CR 707.10a: The copy becomes a token.
/// CR 707.10c: Controller may choose new targets for the copy.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 707.10: Find the spell to copy.
    // For SelfRef targets (e.g. Casualty "copy this spell"), look up by source_id so
    // that intermediate triggered abilities pushed between the original spell and this
    // copy trigger do not cause state.stack.last() to return the wrong entry.
    // For explicit object targets (e.g. Twincast), use the chosen target id.
    // Fallback: take the top of the stack (legacy / untargeted copy effects).
    let top_entry = if let Some(TargetRef::Object(target_id)) = ability.targets.first() {
        state
            .stack
            .iter()
            .find(|e| e.id == *target_id)
            .cloned()
            .ok_or_else(|| {
                EffectError::MissingParam("Target spell not found on stack".to_string())
            })?
    } else if matches!(
        ability.effect,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            ..
        }
    ) {
        // CR 702.176a (Casualty): copy the spell this ability belongs to, identified by source_id.
        state
            .stack
            .iter()
            .find(|e| e.id == ability.source_id)
            .cloned()
            .ok_or_else(|| {
                EffectError::MissingParam("Source spell not found on stack".to_string())
            })?
    } else {
        state
            .stack
            .last()
            .cloned()
            .ok_or_else(|| EffectError::MissingParam("No spell on stack to copy".to_string()))?
    };

    // CR 707.10 + CR 101.2: A spell with "this spell can't be copied" is
    // uncopyable — the copy attempt fails with no effect. Check the target
    // spell's static definitions via the single-authority helper used by
    // counter.rs for the analogous CantBeCountered case.
    let has_cant_be_copied = state
        .objects
        .get(&top_entry.id)
        .map(|obj| {
            super::super::functioning_abilities::active_static_definitions(state, obj)
                .any(|sd| sd.mode == StaticMode::CantBeCopied)
        })
        .unwrap_or(false);
    if has_cant_be_copied {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Allocate a new object ID for the copy
    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // CR 707.10a: The copy becomes a token. Create a GameObject with copiable
    // characteristics from the original spell so zone transitions work correctly.
    let source_obj = state
        .objects
        .get(&top_entry.id)
        .ok_or(EffectError::ObjectNotFound(top_entry.id))?;
    let mut copy_obj = source_obj.clone();
    copy_obj.id = copy_id;
    copy_obj.controller = ability.controller;
    copy_obj.zone = Zone::Stack;
    copy_obj.is_token = true;
    state.objects.insert(copy_id, copy_obj);

    // Build the copy's kind, updating internal source_id references to copy_id.
    // CR 707.10: The copy has the same characteristics as the original, but its
    // identity is distinct. Reset additional_cost_paid and kickers_paid so any
    // "if its [additional] cost was paid" conditions (e.g. Offspring ETB triggers)
    // do not fire for the copy — the copy was placed on the stack, not cast.
    let copy_kind = {
        use crate::types::game_state::StackEntryKind;
        let mut kind = top_entry.kind.clone();
        if let StackEntryKind::Spell {
            ability: Some(ref mut a),
            ..
        } = kind
        {
            a.source_id = copy_id;
            a.context.additional_cost_paid = false;
            a.context.kickers_paid.clear();
        }
        kind
    };

    // CR 707.10: The copy's source_id is its own id (not the original's).
    let copy_entry = crate::types::game_state::StackEntry {
        id: copy_id,
        source_id: copy_id,
        controller: ability.controller,
        kind: copy_kind,
    };

    state.stack.push_back(copy_entry);
    events.push(GameEvent::StackPushed { object_id: copy_id });

    // CR 707.10c: If the copy has targets, allow the controller to choose new ones.
    let copy_targets = top_entry
        .ability()
        .map(|a| a.targets.clone())
        .unwrap_or_default();

    if !copy_targets.is_empty() {
        // Compute legal alternatives for each slot so the UI can present valid
        // choices. If build_target_slots fails (no legal targets exist for the
        // copy), fall back to empty alternatives — the copy still goes on the
        // stack and will fizzle at resolution per CR 608.2b if all targets remain
        // illegal.
        // Use the copy's ability (with copy_id as source_id) so protection and
        // hexproof checks reflect the copy's identity, not the original's.
        let selection_slots = top_entry
            .ability()
            .map(|a| {
                let mut copy_ability = a.clone();
                copy_ability.source_id = copy_id;
                copy_ability
            })
            .and_then(|a| super::super::ability_utils::build_target_slots(state, &a).ok())
            .unwrap_or_default();

        let target_slots: Vec<CopyTargetSlot> = copy_targets
            .iter()
            .enumerate()
            .map(|(i, t)| CopyTargetSlot {
                current: t.clone(),
                legal_alternatives: selection_slots
                    .get(i)
                    .map(|s| s.legal_targets.clone())
                    .unwrap_or_default(),
            })
            .collect();

        state.waiting_for = WaitingFor::CopyRetarget {
            player: ability.controller,
            copy_id,
            target_slots,
            current_slot: 0,
        };
        // EffectResolved deferred until after retarget choice completes.
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{Effect, QuantityExpr, TargetFilter, TargetRef};
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    /// Helper: push a spell onto the stack with a matching GameObject.
    fn push_spell(
        state: &mut GameState,
        obj_id: ObjectId,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        ability: ResolvedAbility,
        variant: CastingVariant,
    ) {
        let obj = GameObject::new(obj_id, card_id, owner, name.to_string(), Zone::Stack);
        state.objects.insert(obj_id, obj);
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: owner,
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(ability),
                casting_variant: variant,
                actual_mana_spent: 0,
            },
        });
    }

    #[test]
    fn test_copy_spell_duplicates_stack_entry() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability.clone(),
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // Stack should have 2 entries now
        assert_eq!(state.stack.len(), 2);
        // Copy should have a different ID
        assert_ne!(state.stack[0].id, state.stack[1].id);

        // CR 707.10a: The copy's GameObject should be a token
        let copy_id = state.stack[1].id;
        let copy_obj = state.objects.get(&copy_id).expect("copy object exists");
        assert!(copy_obj.is_token);
        assert_eq!(copy_obj.zone, Zone::Stack);

        // Same spell kind
        match (&state.stack[0].kind, &state.stack[1].kind) {
            (
                StackEntryKind::Spell {
                    card_id: c1,
                    ability: Some(a1),
                    ..
                },
                StackEntryKind::Spell {
                    card_id: c2,
                    ability: Some(a2),
                    ..
                },
            ) => {
                assert_eq!(c1, c2);
                assert_eq!(
                    crate::types::ability::effect_variant_name(&a1.effect),
                    crate::types::ability::effect_variant_name(&a2.effect)
                );
            }
            _ => panic!("Expected both entries to be Spells with abilities"),
        }
    }

    #[test]
    fn test_copy_spell_empty_stack_returns_error() {
        let mut state = GameState::new_two_player(42);
        assert!(state.stack.is_empty());

        let ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn test_copy_spell_with_targets_enters_retarget() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(ObjectId(50))],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Lightning Bolt",
            original_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // CR 707.10c: Copy has targets → should enter CopyRetarget.
        assert!(matches!(state.waiting_for, WaitingFor::CopyRetarget { .. }));
        // Copy should still be on the stack
        assert_eq!(state.stack.len(), 2);
    }

    #[test]
    fn test_copy_spell_without_targets_skips_retarget() {
        let mut state = GameState::new_two_player(42);

        let original_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );

        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Divination",
            original_ability,
            CastingVariant::Normal,
        );

        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &copy_ability, &mut events).unwrap();

        // No targets → should NOT enter CopyRetarget, should emit EffectResolved
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::CopyRetarget { .. }
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    /// Helper: push a triggered ability onto the stack (no targets).
    fn push_trigger(
        state: &mut GameState,
        obj_id: ObjectId,
        card_id: CardId,
        owner: PlayerId,
        ability: ResolvedAbility,
    ) {
        let obj = crate::game::game_object::GameObject::new(
            obj_id,
            card_id,
            owner,
            "Trigger Token".to_string(),
            Zone::Stack,
        );
        state.objects.insert(obj_id, obj);
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: owner,
            kind: StackEntryKind::TriggeredAbility {
                source_id: obj_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: String::new(),
            },
        });
    }

    /// CR 702.176a (Casualty): When another trigger sits between the original
    /// spell and the Casualty copy trigger, SelfRef lookup must find the spell
    /// by source_id rather than using stack.last().
    #[test]
    fn test_copy_spell_selfref_finds_spell_past_intermediate_trigger() {
        let mut state = GameState::new_two_player(42);

        // Push original targeted spell (Anguished Unmaking-style)
        let original_ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: crate::types::zones::Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(ObjectId(99))],
            ObjectId(10),
            PlayerId(0),
        );
        push_spell(
            &mut state,
            ObjectId(10),
            CardId(1),
            PlayerId(0),
            "Anguished Unmaking",
            original_ability.clone(),
            CastingVariant::Normal,
        );

        // Push an intermediate triggered ability (e.g. Monastery Mentor token trigger)
        let mentor_ability = ResolvedAbility::new(
            Effect::Token {
                name: "Monk".to_string(),
                power: crate::types::ability::PtValue::Fixed(1),
                toughness: crate::types::ability::PtValue::Fixed(1),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(11),
            PlayerId(0),
        );
        push_trigger(
            &mut state,
            ObjectId(11),
            CardId(2),
            PlayerId(0),
            mentor_ability,
        );

        // Simulate resolve_top popping the Casualty copy trigger (top of stack).
        // The Casualty ability has source_id = 10 (Anguished Unmaking) and SelfRef target.
        let casualty_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
            },
            vec![],
            ObjectId(10), // source_id = original spell
            PlayerId(0),
        );
        let mut events = Vec::new();

        // Stack is now: [Anguished Unmaking (10), Mentor trigger (11)]
        // copy_spell::resolve should find ObjectId(10) via source_id, not stack.last() (=11)
        resolve(&mut state, &casualty_ability, &mut events).unwrap();

        // Should have entered CopyRetarget (original had targets) with the copy of the spell
        assert!(
            matches!(state.waiting_for, WaitingFor::CopyRetarget { .. }),
            "Expected CopyRetarget but got {:?}",
            state.waiting_for
        );
        // Stack: original + mentor trigger + copy = 3 entries
        assert_eq!(state.stack.len(), 3);
        // The copy should be a copy of Anguished Unmaking (ChangeZone), not the Mentor trigger
        let copy_entry = state.stack.back().unwrap();
        assert!(
            copy_entry
                .ability()
                .is_some_and(|a| matches!(a.effect, Effect::ChangeZone { .. })),
            "Copy should replicate ChangeZone (Anguished Unmaking), not the trigger"
        );
    }
}
