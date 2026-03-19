use crate::types::ability::{
    Effect, EffectError, EffectKind, ReplacementDefinition, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 701.15: "Regenerate [permanent]" creates a one-shot replacement shield.
/// The next time that permanent would be destroyed this turn, instead remove
/// all damage marked on it, tap it, and remove it from combat.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // If no explicit targets, regenerate the source itself.
    // "Regenerate this creature" parses to Any with no targeting keyword,
    // so when targets are empty we default to self.
    let use_self = match &ability.effect {
        Effect::Regenerate { target } => {
            matches!(target, TargetFilter::None | TargetFilter::SelfRef)
                || (matches!(target, TargetFilter::Any) && ability.targets.is_empty())
        }
        _ => false,
    };

    let targets: Vec<_> = if use_self {
        vec![ability.source_id]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    };

    for obj_id in targets {
        let on_battlefield = state
            .objects
            .get(&obj_id)
            .is_some_and(|o| o.zone == Zone::Battlefield);

        if !on_battlefield {
            continue;
        }

        // CR 701.15: Create a regeneration shield as a replacement definition.
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();

        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.replacement_definitions.push(shield);
        }
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
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn regenerate_creates_shield_on_source() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(obj.replacement_definitions[0].is_regeneration_shield);
        assert!(!obj.replacement_definitions[0].is_consumed);
        assert_eq!(
            obj.replacement_definitions[0].event,
            ReplacementEvent::Destroy
        );
    }

    #[test]
    fn regenerate_creates_shield_on_target() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(obj.replacement_definitions[0].is_regeneration_shield);
    }

    #[test]
    fn regenerate_skips_off_battlefield() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Hand,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert!(obj.replacement_definitions.is_empty());
    }

    #[test]
    fn regenerate_self_with_any_and_empty_targets() {
        // "Regenerate this creature" parses to Any with no targeting keyword
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Skeleton".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::Any,
            },
            vec![], // no explicit targets
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(
            obj.replacement_definitions.len(),
            1,
            "Should create shield on source when targets empty"
        );
    }

    #[test]
    fn regenerate_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Regenerate {
                target: TargetFilter::SelfRef,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Regenerate,
                ..
            }
        )));
    }
}
