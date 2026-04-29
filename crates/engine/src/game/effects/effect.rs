use crate::game::filter;
use crate::game::quantity::{quantity_expr_uses_recipient, resolve_quantity_with_targets};
use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, ResolvedAbility,
    StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// Effect handler: creates transient continuous effects from a GenericEffect.
///
/// Resolved GenericEffect definitions are registered as state-level transient
/// continuous effects with explicit durations, rather than being pushed onto
/// individual game objects. This ensures proper layer evaluation and cleanup.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
    } = &ability.effect
    {
        // CR 611.2b: Default UntilEndOfTurn applies to non-"becomes" GenericEffects
        // (pump spells, etc.). "Becomes" effects inject Duration::Permanent at parse time.
        let dur = ability
            .duration
            .clone()
            .or(duration.clone())
            .unwrap_or(Duration::UntilEndOfTurn);

        for static_def in static_abilities {
            register_transient_effect(state, ability, static_def, target.as_ref(), &dur);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn register_transient_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    static_def: &StaticDefinition,
    target_filter: Option<&TargetFilter>,
    duration: &Duration,
) {
    let modifications = snapshot_transient_modifications(state, ability, &static_def.modifications);

    // Targeted effects: register one transient effect per target object
    if !ability.targets.is_empty() {
        for target in &ability.targets {
            if let TargetRef::Object(obj_id) = target {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: *obj_id },
                    modifications.clone(),
                    static_def.condition.clone(),
                );
            }
        }
        return;
    }

    // Non-targeted: resolve the affected filter
    match target_filter.or(static_def.affected.as_ref()) {
        Some(TargetFilter::SelfRef) => {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificObject {
                    id: ability.source_id,
                },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        // CR 113.10 + CR 702.16j: Player-scoped affected filter — register the
        // transient effect bound to the ability's controller (a player) via
        // SpecificPlayer. Queried by player_has_protection_from_everything
        // and friends in static_abilities.rs.
        Some(TargetFilter::Controller) => {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificPlayer {
                    id: ability.controller,
                },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        // Pass-through: the caller already pinned a specific player.
        Some(TargetFilter::SpecificPlayer { id }) => {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificPlayer { id: *id },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        Some(TargetFilter::Player | TargetFilter::None) | None => {}
        Some(filter) => {
            let filter = crate::game::effects::resolved_object_filter(ability, filter);
            // Broadcast filter: find matching objects at resolution time and bind each.
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            let ctx = filter::FilterContext::from_ability(ability);
            let matching: Vec<ObjectId> = state
                .battlefield
                .iter()
                .filter(|obj_id| filter::matches_target_filter(state, **obj_id, &filter, &ctx))
                .copied()
                .collect();
            for obj_id in matching {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: obj_id },
                    modifications.clone(),
                    static_def.condition.clone(),
                );
            }
        }
    }
}

fn snapshot_transient_modifications(
    state: &GameState,
    ability: &ResolvedAbility,
    modifications: &[ContinuousModification],
) -> Vec<ContinuousModification> {
    modifications
        .iter()
        .map(|modification| match modification {
            ContinuousModification::AddDynamicPower { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::AddPower {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            ContinuousModification::AddDynamicToughness { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::AddToughness {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            ContinuousModification::SetPowerDynamic { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::SetPower {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            ContinuousModification::SetToughnessDynamic { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::SetToughness {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            _ => modification.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, Duration, QuantityExpr, QuantityRef,
        StaticDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn generic_effect_registers_transient_effect_for_self_ref() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.source_id, source);
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: source });
        assert_eq!(tce.duration, Duration::UntilEndOfTurn);
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]
        );
    }

    #[test]
    fn generic_effect_registers_transient_effect_for_matching_filter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let your_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ally".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&your_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opp_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Enemy".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should create transient effect for your_creature only
        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject { id: your_creature }
        );
    }

    #[test]
    fn generic_effect_binds_targeted_object_to_specific_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let other_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature()))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![TargetRef::Object(target_creature)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject {
                id: target_creature
            }
        );
    }

    #[test]
    fn generic_effect_snapshots_dynamic_pt_modifications_at_resolution() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chorus of Might".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let ally_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Ally A".to_string(),
            Zone::Battlefield,
        );
        let ally_b = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Ally B".to_string(),
            Zone::Battlefield,
        );
        for id in [target_creature, ally_a, ally_b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let creature_count = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![
                ContinuousModification::AddDynamicPower {
                    value: creature_count.clone(),
                },
                ContinuousModification::AddDynamicToughness {
                    value: creature_count,
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Trample,
                },
            ]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![TargetRef::Object(target_creature)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let late_ally = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Late Ally".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&late_ally)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let modifications = &state.transient_continuous_effects[0].modifications;
        assert!(
            modifications.contains(&ContinuousModification::AddPower { value: 3 }),
            "dynamic power should snapshot to the creature count at resolution, got {modifications:?}"
        );
        assert!(
            modifications.contains(&ContinuousModification::AddToughness { value: 3 }),
            "dynamic toughness should snapshot to the creature count at resolution, got {modifications:?}"
        );
        assert!(
            !modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddDynamicPower { .. }
                    | ContinuousModification::AddDynamicToughness { .. }
            )),
            "transient P/T pump must not remain live after resolution: {modifications:?}"
        );
    }

    /// CR 702.16j end-to-end: parse Teferi's-Protection-style clause, feed
    /// the parsed effect into `resolve`, and verify the single-authority
    /// query reports the controller as protected. This exercises the full
    /// pipeline from Oracle text to runtime enforcement hook.
    #[test]
    fn parse_and_resolve_you_gain_protection_from_everything_grants_player_protection() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection".to_string(),
            Zone::Battlefield,
        );

        let parsed = parse_effect_chain("you gain protection from everything", AbilityKind::Spell);
        let ability = ResolvedAbility::new((*parsed.effect).clone(), vec![], source, PlayerId(0))
            .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            crate::game::static_abilities::player_has_protection_from_everything(
                &state,
                PlayerId(0)
            ),
            "controller must be protected after resolution"
        );
        assert!(
            !crate::game::static_abilities::player_has_protection_from_everything(
                &state,
                PlayerId(1)
            ),
            "opponent must NOT gain protection — scoping is per-controller"
        );
    }

    /// CR 113.10 + CR 702.16j: When a GenericEffect carries `affected:
    /// Controller`, `register_transient_effect` must bind the transient to
    /// `SpecificPlayer { id: ability.controller }`. This is the runtime hook
    /// for Teferi's-Protection-style player-scoped keyword grants.
    #[test]
    fn generic_effect_controller_affected_binds_to_specific_player() {
        use crate::types::ability::TargetFilter;
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection".to_string(),
            Zone::Battlefield,
        );

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Controller)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            "Controller-scoped keyword grant must bind to SpecificPlayer for the ability's controller"
        );
        // End-to-end: the registered effect is observable via the single-
        // authority query used by targeting/damage/attack enforcement.
        assert!(
            crate::game::static_abilities::player_has_protection_from_everything(
                &state,
                PlayerId(0)
            )
        );
    }
}
