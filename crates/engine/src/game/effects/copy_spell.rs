use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
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
    // Find the top spell on the stack (not the copy_spell effect itself)
    let top_entry = state
        .stack
        .last()
        .cloned()
        .ok_or_else(|| EffectError::MissingParam("No spell on stack to copy".to_string()))?;

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

    // Create the copy with a new ID but same kind
    let copy_entry = crate::types::game_state::StackEntry {
        id: copy_id,
        source_id: top_entry.source_id,
        controller: ability.controller,
        kind: top_entry.kind.clone(),
    };

    state.stack.push_back(copy_entry);
    events.push(GameEvent::StackPushed { object_id: copy_id });

    // CR 707.10c: If the copy has targets, allow the controller to choose new ones.
    let copy_targets = top_entry
        .ability()
        .map(|a| a.targets.clone())
        .unwrap_or_default();

    if !copy_targets.is_empty() {
        // Build target slots — each slot shows current target. Legal alternatives
        // are not computed here (the engine handler validates at selection time).
        let target_slots: Vec<CopyTargetSlot> = copy_targets
            .iter()
            .map(|t| CopyTargetSlot {
                current: t.clone(),
                legal_alternatives: Vec::new(),
            })
            .collect();

        state.waiting_for = WaitingFor::CopyRetarget {
            player: ability.controller,
            copy_id,
            target_slots,
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
}
