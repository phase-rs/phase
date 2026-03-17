use crate::game::filter;
use crate::types::ability::{
    Duration, Effect, EffectError, EffectKind, ResolvedAbility, StaticDefinition, TargetFilter,
    TargetRef,
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
    // Targeted effects: register one transient effect per target object
    if !ability.targets.is_empty() {
        for target in &ability.targets {
            if let TargetRef::Object(obj_id) = target {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: *obj_id },
                    static_def.modifications.clone(),
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
                static_def.modifications.clone(),
                static_def.condition.clone(),
            );
        }
        Some(TargetFilter::Player | TargetFilter::Controller | TargetFilter::None) | None => {}
        Some(filter) => {
            // Broadcast filter: find matching objects at resolution time and bind each
            let matching: Vec<ObjectId> = state
                .battlefield
                .iter()
                .filter(|obj_id| {
                    filter::matches_target_filter_controlled(
                        state,
                        **obj_id,
                        filter,
                        ability.source_id,
                        ability.controller,
                    )
                })
                .copied()
                .collect();
            for obj_id in matching {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: obj_id },
                    static_def.modifications.clone(),
                    static_def.condition.clone(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, Duration, StaticDefinition, TypedFilter,
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
}
