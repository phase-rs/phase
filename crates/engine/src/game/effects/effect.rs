use crate::game::filter;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// Effect effect: creates a temporary game effect (emblem-like).
/// Reads typed GenericEffect { static_abilities, duration } fields.
/// Applies referenced static abilities directly to targeted objects.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = &ability.effect
    {
        // Apply each static ability definition to targets
        for static_def in static_abilities {
            apply_static_effect(state, ability, static_def.clone(), target.as_ref());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn apply_static_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    static_def: StaticDefinition,
    target_filter: Option<&TargetFilter>,
) {
    if !ability.targets.is_empty() {
        for target in &ability.targets {
            if let TargetRef::Object(obj_id) = target {
                let mut bound_static_def = static_def.clone();
                bound_static_def.affected = Some(TargetFilter::SelfRef);
                apply_static_to_object(state, *obj_id, bound_static_def);
            }
        }
        return;
    }

    match target_filter.or(static_def.affected.as_ref()) {
        Some(TargetFilter::SelfRef) => apply_static_to_object(state, ability.source_id, static_def),
        Some(TargetFilter::Player | TargetFilter::Controller | TargetFilter::None) | None => {}
        Some(filter) => {
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
                apply_static_to_object(state, obj_id, static_def.clone());
            }
        }
    }
}

fn apply_static_to_object(state: &mut GameState, obj_id: ObjectId, static_def: StaticDefinition) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        if !obj.granted_static_definitions.contains(&static_def) {
            obj.granted_static_definitions.push(static_def);
            state.layers_dirty = true;
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
    fn generic_effect_applies_to_source_for_self_ref() {
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
                static_abilities: vec![static_def.clone()],
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

        assert!(state.objects[&source]
            .granted_static_definitions
            .contains(&static_def));
    }

    #[test]
    fn generic_effect_applies_to_matching_battlefield_filter() {
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
                static_abilities: vec![static_def.clone()],
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

        assert!(state.objects[&your_creature]
            .granted_static_definitions
            .contains(&static_def));
        assert!(!state.objects[&opp_creature]
            .granted_static_definitions
            .contains(&static_def));
    }

    #[test]
    fn generic_effect_binds_targeted_object_statics_to_self_ref() {
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

        assert_eq!(state.objects[&target_creature].granted_static_definitions.len(), 1);
        assert_eq!(
            state.objects[&target_creature].granted_static_definitions[0].affected,
            Some(TargetFilter::SelfRef)
        );
        assert!(state.objects[&other_creature]
            .granted_static_definitions
            .is_empty());
    }
}
