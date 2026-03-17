use crate::game::static_abilities::{check_static_ability, StaticCheckContext};
use crate::game::zones;
use crate::types::ability::{
    Duration, Effect, EffectError, EffectKind, ResolvedAbility, StaticDefinition, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind};
use crate::types::identifiers::ObjectId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

/// Counter target spells or abilities on the stack.
/// Spells are removed from the stack and moved to graveyard.
/// Abilities are simply removed from the stack (they aren't cards).
/// Respects CantBeCountered static ability.
///
/// If the effect carries a `source_static`, it is applied to the counter's source
/// (e.g., Tidebinder) with `affected: SpecificObject(source_permanent_id)` after
/// successfully countering a permanent's ability. This implements "that permanent
/// loses all abilities for as long as ~" patterns.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let source_static = match &ability.effect {
        Effect::Counter { source_static, .. } => source_static.clone(),
        _ => None,
    };

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            // Check if the target has CantBeCountered
            let ctx = StaticCheckContext {
                source_id: Some(*obj_id),
                target_id: Some(*obj_id),
                ..Default::default()
            };
            if check_static_ability(state, "CantBeCountered", &ctx) {
                continue;
            }

            let has_cant_be_countered = state
                .objects
                .get(obj_id)
                .map(|obj| {
                    obj.static_definitions
                        .iter()
                        .any(|sd| sd.mode == StaticMode::Other("CantBeCountered".into()))
                })
                .unwrap_or(false);
            if has_cant_be_countered {
                continue;
            }

            // Remove from stack
            let stack_idx = state.stack.iter().position(|e| e.id == *obj_id);
            if let Some(idx) = stack_idx {
                let is_spell = matches!(state.stack[idx].kind, StackEntryKind::Spell { .. });
                let source_permanent_id = state.stack[idx].source_id;
                state.stack.remove(idx);

                if is_spell {
                    // Spells are cards — move to graveyard
                    zones::move_to_zone(state, *obj_id, Zone::Graveyard, events);
                } else {
                    // Ability was countered — apply source_static if present
                    apply_source_static(
                        state,
                        ability.source_id,
                        source_permanent_id,
                        &source_static,
                    );
                }

                events.push(GameEvent::SpellCountered {
                    object_id: *obj_id,
                    countered_by: ability.source_id,
                });
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Register a transient continuous effect for a counter's source_static.
///
/// The effect targets the countered ability's source permanent and persists
/// as long as the counter source (e.g., Tidebinder) remains on the battlefield.
fn apply_source_static(
    state: &mut GameState,
    counter_source_id: ObjectId,
    source_permanent_id: ObjectId,
    source_static: &Option<StaticDefinition>,
) {
    let static_def = match source_static {
        Some(def) => def,
        None => return,
    };

    // Only apply if the source permanent is still on the battlefield
    if !state.battlefield.contains(&source_permanent_id) {
        return;
    }

    let controller = state
        .objects
        .get(&counter_source_id)
        .map(|o| o.controller)
        .unwrap_or_default();

    state.add_transient_continuous_effect(
        counter_source_id,
        controller,
        Duration::UntilHostLeavesPlay,
        TargetFilter::SpecificObject { id: source_permanent_id },
        static_def.modifications.clone(),
        static_def.condition.clone(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::game_state::{StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_dummy_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Dummy".to_string(),
                description: None,
            },
            vec![],
            source_id,
            controller,
        )
    }

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
                ability: make_dummy_ability(obj_id, PlayerId(1)),
                cast_as_adventure: false,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
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
            .push(StaticDefinition::new(StaticMode::Other(
                "CantBeCountered".to_string(),
            )));
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: make_dummy_ability(obj_id, PlayerId(1)),
                cast_as_adventure: false,
            },
        });

        let ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Spell should still be on the stack (not countered)
        assert_eq!(state.stack.len(), 1);
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::SpellCountered { .. })));
    }

    #[test]
    fn counter_ability_applies_source_static_to_counter_source() {
        use crate::types::ability::{ContinuousModification, Duration, StaticDefinition};

        let mut state = GameState::new_two_player(42);

        // Source permanent on the battlefield (e.g., a creature whose ability was activated)
        let source_permanent = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Source Creature".to_string(),
            Zone::Battlefield,
        );

        // Tidebinder on the battlefield (the counter source)
        let tidebinder = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Tidebinder".to_string(),
            Zone::Battlefield,
        );

        // Triggered ability on the stack (from the source creature)
        let ability_on_stack = ObjectId(999);
        state.stack.push(StackEntry {
            id: ability_on_stack,
            source_id: source_permanent,
            controller: PlayerId(1),
            kind: StackEntryKind::TriggeredAbility {
                source_id: source_permanent,
                ability: make_dummy_ability(source_permanent, PlayerId(1)),
                condition: None,
                trigger_event: None,
            },
        });

        let source_static = StaticDefinition::continuous()
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::StackAbility,
                source_static: Some(source_static),
            },
            vec![TargetRef::Object(ability_on_stack)],
            tidebinder,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // Ability should be removed from stack
        assert!(state.stack.is_empty(), "ability should be countered");

        // Should register a transient continuous effect targeting the source permanent
        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "Should have one transient continuous effect"
        );
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.source_id, tidebinder, "source should be Tidebinder");
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificObject { id: source_permanent },
            "should target the source permanent"
        );
        assert_eq!(
            tce.duration,
            Duration::UntilHostLeavesPlay,
            "should persist while Tidebinder is on battlefield"
        );
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::RemoveAllAbilities],
            "should remove all abilities"
        );
    }

    #[test]
    fn counter_spell_does_not_apply_source_static() {
        use crate::types::ability::{ContinuousModification, StaticDefinition};

        let mut state = GameState::new_two_player(42);

        let tidebinder = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Tidebinder".to_string(),
            Zone::Battlefield,
        );

        // A spell on the stack (not an ability)
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: make_dummy_ability(spell_id, PlayerId(1)),
                cast_as_adventure: false,
            },
        });

        let source_static = StaticDefinition::continuous()
            .modifications(vec![ContinuousModification::RemoveAllAbilities]);

        let counter_ability = ResolvedAbility::new(
            Effect::Counter {
                target: TargetFilter::Any,
                source_static: Some(source_static),
            },
            vec![TargetRef::Object(spell_id)],
            tidebinder,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &counter_ability, &mut events).unwrap();

        // Spell countered, but source_static should NOT be applied (it's a spell, not an ability)
        assert!(
            state.transient_continuous_effects.is_empty(),
            "source_static should not apply when countering a spell"
        );
    }
}
