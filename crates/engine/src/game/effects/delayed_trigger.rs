use crate::types::ability::{
    DelayedTriggerCondition, Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::TrackedSetId;
use crate::types::zones::Zone;

/// CR 603.7: Create a delayed triggered ability during resolution.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (mut condition, effect_def, uses_tracked_set) = match &ability.effect {
        Effect::CreateDelayedTrigger {
            condition,
            effect,
            uses_tracked_set,
        } => (
            condition.clone(),
            effect.as_ref().clone(),
            *uses_tracked_set,
        ),
        _ => {
            return Err(EffectError::MissingParam(
                "CreateDelayedTrigger".to_string(),
            ))
        }
    };

    bind_contextual_filter_to_condition(&mut condition, &ability.targets);

    // CR 505.1 + CR 603.7a: "your next <phase>" binds the trigger to the
    // ability's controller. The parser emits a placeholder `PlayerId(0)` in
    // `AtNextPhaseForPlayer.player` because compile-time AST has no access to
    // runtime player ids; rewrite here to the actual controller at resolve
    // time. Mirrors the `bind_contextual_filter_to_condition` pattern above.
    if let DelayedTriggerCondition::AtNextPhaseForPlayer { player, .. } = &mut condition {
        *player = ability.controller;
    }

    // Build the delayed trigger's resolved ability from the definition
    let mut delayed_effect = *effect_def.effect.clone();

    // CR 603.7: Bind the most recent tracked set to the effect's target filter,
    // resolving sentinel TrackedSetId(0) or TargetFilter::Any, and upgrading
    // ChangeZone → ChangeZoneAll for delayed triggers (which have empty explicit targets).
    if uses_tracked_set {
        if let Some((&real_id, _)) = state
            .tracked_object_sets
            .iter()
            .filter(|(_, objects)| !objects.is_empty())
            .max_by_key(|(id, _)| id.0)
        {
            bind_tracked_set_to_effect(&mut delayed_effect, real_id);
        }
    }

    // CR 603.7c + CR 701.36a: If the delayed inner effect references the
    // "token created this way" anaphor via `TargetFilter::LastCreated`,
    // snapshot the currently-tracked token IDs into the delayed ability's
    // targets NOW. The delayed trigger may fire arbitrarily later, by which
    // time `last_created_token_ids` will have been overwritten by other
    // token-creating effects (CR 603.7c: a delayed trigger refers to a
    // particular object even if later events change it).
    let snapshot_targets = if effect_references_last_created(&delayed_effect)
        && !state.last_created_token_ids.is_empty()
    {
        state
            .last_created_token_ids
            .iter()
            .map(|&id| TargetRef::Object(id))
            .collect()
    } else {
        vec![]
    };

    let delayed_ability = ResolvedAbility::new(
        delayed_effect,
        snapshot_targets,
        ability.source_id,
        ability.controller,
    );

    // CR 603.7c: Most delayed triggers fire once and are removed.
    // WheneverEvent triggers fire each time and persist until end-of-turn cleanup.
    let one_shot = !matches!(
        condition,
        crate::types::ability::DelayedTriggerCondition::WheneverEvent { .. }
    );
    state.delayed_triggers.push(DelayedTrigger {
        condition,
        ability: delayed_ability,
        controller: ability.controller,
        source_id: ability.source_id,
        one_shot,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateDelayedTrigger,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.36a + CR 603.7c: Walk an effect (and any nested sub-ability
/// definitions) looking for `TargetFilter::LastCreated` in a target position.
/// Used by `resolve` to decide whether to snapshot `last_created_token_ids`
/// into the delayed ability's `targets` at creation time.
fn effect_references_last_created(effect: &Effect) -> bool {
    matches!(effect.target_filter(), Some(TargetFilter::LastCreated))
}

fn bind_contextual_filter_to_condition(
    condition: &mut DelayedTriggerCondition,
    parent_targets: &[TargetRef],
) {
    if let DelayedTriggerCondition::WhenDiesOrExiled { filter } = condition {
        if matches!(filter, TargetFilter::ParentTarget) {
            // Positive ParentTarget → resolve to SpecificObject for the animated target.
            // normalize_contextual_filter only handles Not(ParentTarget); bare ParentTarget
            // needs direct rewriting here so the delayed trigger can match at check time.
            let object_ids: Vec<_> = parent_targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            *filter = match object_ids.as_slice() {
                [] => TargetFilter::Any,
                [id] => TargetFilter::SpecificObject { id: *id },
                _ => TargetFilter::Or {
                    filters: object_ids
                        .into_iter()
                        .map(|id| TargetFilter::SpecificObject { id })
                        .collect(),
                },
            };
        } else {
            *filter = crate::game::filter::normalize_contextual_filter(filter, parent_targets);
        }
    }
}

/// Bind a tracked set to an effect's target filter, resolve origin zone,
/// and upgrade ChangeZone → ChangeZoneAll if needed.
///
/// Three responsibilities:
/// 1. Resolve TrackedSetId(0) sentinel → TrackedSetId(real_id)
/// 2. Bind TargetFilter::Any → TrackedSet(real_id) for implicit pronouns
/// 3. Set origin zone to Exile (tracked sets are always from exile)
fn bind_tracked_set_to_effect(effect: &mut Effect, real_id: TrackedSetId) {
    match effect {
        Effect::ChangeZoneAll { origin, target, .. } => {
            // Resolve target filter
            match target {
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                }
                | TargetFilter::Any => {
                    *target = TargetFilter::TrackedSet { id: real_id };
                }
                _ => {}
            }
            // CR 400.7: Tracked objects are in exile; set origin for zone scan
            if origin.is_none() {
                *origin = Some(Zone::Exile);
            }
        }
        // Upgrade ChangeZone → ChangeZoneAll: ChangeZone uses ability.targets (empty for
        // delayed triggers), so it would move nothing. ChangeZoneAll scans by filter.
        Effect::ChangeZone { destination, .. } => {
            *effect = Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: *destination,
                target: TargetFilter::TrackedSet { id: real_id },
            };
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect, QuantityExpr,
    };
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    #[test]
    fn creates_delayed_trigger_on_state() {
        let mut state = GameState::new_two_player(42);
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert_eq!(state.delayed_triggers.len(), 1);
        assert!(state.delayed_triggers[0].one_shot);
        assert_eq!(state.delayed_triggers[0].controller, PlayerId(0));
        assert_eq!(state.delayed_triggers[0].source_id, ObjectId(5));
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        );
    }

    #[test]
    fn uses_tracked_set_binds_to_change_zone_all() {
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        // Register a tracked set
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10), ObjectId(11)]);
        state.next_tracked_set_id = 2;

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert_eq!(state.delayed_triggers.len(), 1);

        // The delayed trigger's effect should reference the tracked set
        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZoneAll { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(1)
                    }
                );
            }
            other => panic!("Expected ChangeZoneAll, got {:?}", other),
        }
    }

    #[test]
    fn uses_tracked_set_resolves_sentinel() {
        use crate::types::identifiers::TrackedSetId;

        let mut state = GameState::new_two_player(42);
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10)]);
        state.next_tracked_set_id = 2;

        // Parser emits ChangeZone with TrackedSetId(0) sentinel
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Should be upgraded to ChangeZoneAll with resolved TrackedSetId and Exile origin
        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
            } => {
                assert_eq!(*origin, Some(Zone::Exile));
                assert_eq!(*destination, Zone::Battlefield);
                assert_eq!(
                    *target,
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(1)
                    }
                );
            }
            other => panic!("Expected ChangeZoneAll, got {:?}", other),
        }
    }

    /// CR 505.1 + CR 603.7a: `AtNextPhaseForPlayer` player field is emitted
    /// by the parser as a `PlayerId(0)` placeholder (compile-time AST has no
    /// access to runtime player ids). `resolve()` rewrites it to
    /// `ability.controller` so the delayed trigger fires on the correct
    /// player's turn. Used by Mana Sculpt.
    #[test]
    fn at_next_phase_for_player_rebinds_placeholder_to_controller() {
        let mut state = GameState::new_two_player(42);
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        // Cast by PlayerId(1), with the placeholder PlayerId(0) in the
        // condition. Resolver must rewrite to PlayerId(1).
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player: PlayerId(1),
            },
            "placeholder player must be rewritten to ability.controller"
        );
    }
}
