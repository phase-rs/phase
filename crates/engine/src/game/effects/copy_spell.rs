use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CopySpell: duplicate the top spell on the stack.
/// Creates a new StackEntry with the same ability but a new ID and pushes it onto the stack.
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

    // Allocate a new object ID for the copy
    let copy_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // Create the copy with a new ID but same kind
    let copy_entry = crate::types::game_state::StackEntry {
        id: copy_id,
        source_id: top_entry.source_id,
        controller: ability.controller,
        kind: top_entry.kind.clone(),
    };

    state.stack.push(copy_entry);

    events.push(GameEvent::StackPushed { object_id: copy_id });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, QuantityExpr, TargetFilter};
    use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn test_copy_spell_duplicates_stack_entry() {
        let mut state = GameState::new_two_player(42);

        // Put a spell on the stack
        let original_ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );

        state.stack.push(StackEntry {
            id: ObjectId(10),
            source_id: ObjectId(10),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: original_ability.clone(),
                casting_variant: CastingVariant::Normal,
            },
        });

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
        // But same kind
        match (&state.stack[0].kind, &state.stack[1].kind) {
            (
                StackEntryKind::Spell {
                    card_id: c1,
                    ability: a1,
                    ..
                },
                StackEntryKind::Spell {
                    card_id: c2,
                    ability: a2,
                    ..
                },
            ) => {
                assert_eq!(c1, c2);
                assert_eq!(
                    crate::types::ability::effect_variant_name(&a1.effect),
                    crate::types::ability::effect_variant_name(&a2.effect)
                );
            }
            _ => panic!("Expected both entries to be Spells"),
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
}
