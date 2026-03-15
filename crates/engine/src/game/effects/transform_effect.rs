use crate::game::transform::transform_permanent;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match &ability.effect {
        Effect::Transform { .. } => {}
        _ => {
            return Err(EffectError::InvalidParam(
                "expected Transform effect".to_string(),
            ))
        }
    }

    let object_id = match ability.targets.as_slice() {
        [TargetRef::Object(object_id)] => *object_id,
        [] => ability.source_id,
        _ => {
            return Err(EffectError::InvalidParam(
                "transform expects exactly one object target".to_string(),
            ))
        }
    };

    transform_permanent(state, object_id, events)
        .map_err(|err| EffectError::InvalidParam(err.to_string()))?;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Transform,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind, TargetFilter};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_dfc(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Front Face".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string()],
        };
        obj.keywords = vec![Keyword::Vigilance];
        obj.base_keywords = vec![Keyword::Vigilance];
        obj.abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Transform {
                target: TargetFilter::SelfRef,
            },
        )];
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
        obj.back_face = Some(crate::game::game_object::BackFaceData {
            name: "Back Face".to_string(),
            power: Some(4),
            toughness: Some(4),
            card_types: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Werewolf".to_string()],
            },
            keywords: vec![Keyword::Trample],
            abilities: vec![],
            color: vec![ManaColor::Green, ManaColor::Red],
        });
        id
    }

    #[test]
    fn transform_effect_uses_source_when_no_explicit_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::SelfRef,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let object = &state.objects[&source_id];
        assert!(object.transformed);
        assert_eq!(object.name, "Back Face");
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Transform,
                source_id: emitted_source,
            } if *emitted_source == source_id
        )));
    }

    #[test]
    fn transform_effect_uses_explicit_object_target() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target_id = setup_dfc(&mut state);
        let ability = ResolvedAbility::new(
            Effect::Transform {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target_id)],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&target_id].transformed);
        assert!(!state.objects[&source_id].transformed);
    }
}
