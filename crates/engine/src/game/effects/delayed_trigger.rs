use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::zones::Zone;

/// CR 603.7: Create a delayed triggered ability during resolution.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (condition, effect_def, uses_tracked_set) = match &ability.effect {
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

    // Build the delayed trigger's resolved ability from the definition
    let mut delayed_effect = effect_def.effect.clone();

    // If using tracked set, bind the most recent tracked set to the effect's filter
    // CR 603.7: "those cards" references the specific objects from the originating effect
    if uses_tracked_set {
        // Find the tracked set with the highest ID (most recently created)
        if let Some((&set_id, _)) = state
            .tracked_object_sets
            .iter()
            .filter(|(_, objects)| !objects.is_empty())
            .max_by_key(|(id, _)| id.0)
        {
            delayed_effect = Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSet(set_id),
            };
        }
    }

    let delayed_ability = ResolvedAbility::new(
        delayed_effect,
        vec![],
        ability.source_id,
        ability.controller,
    );

    // CR 603.7c: Most delayed triggers fire once and are removed
    state.delayed_triggers.push(DelayedTrigger {
        condition,
        ability: delayed_ability,
        controller: ability.controller,
        source_id: ability.source_id,
        one_shot: true,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateDelayedTrigger,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, DelayedTriggerCondition, Effect};
    use crate::types::identifiers::ObjectId;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;

    #[test]
    fn creates_delayed_trigger_on_state() {
        let mut state = GameState::new_two_player(42);
        let effect_def = AbilityDefinition::new(AbilityKind::Spell, Effect::Draw { count: 1 });
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
                assert_eq!(*target, TargetFilter::TrackedSet(TrackedSetId(1)));
            }
            other => panic!("Expected ChangeZoneAll, got {:?}", other),
        }
    }
}
