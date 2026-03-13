use crate::game::filter;
use crate::types::ability::{
    Effect, EffectError, EffectKind, PtValue, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Temporarily pump target creatures' power and toughness.
/// Reads power/toughness from `Effect::Pump`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_att, num_def) = match &ability.effect {
        Effect::Pump {
            power, toughness, ..
        } => (resolve_pt_value(power), resolve_pt_value(toughness)),
        _ => (0, 0),
    };

    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let obj = state
                .objects
                .get_mut(obj_id)
                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
            if let Some(ref mut power) = obj.power {
                *power += num_att;
            }
            if let Some(ref mut toughness) = obj.toughness {
                *toughness += num_def;
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Pump all creatures matching the typed TargetFilter on the battlefield.
/// Reads power/toughness/filter from `Effect::PumpAll`.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (num_att, num_def, target_filter) = match &ability.effect {
        Effect::PumpAll {
            power,
            toughness,
            target,
        } => (
            resolve_pt_value(power),
            resolve_pt_value(toughness),
            target.clone(),
        ),
        _ => (0, 0, TargetFilter::None),
    };

    // Collect matching object IDs first to avoid borrow conflicts
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            filter::matches_target_filter_controlled(
                state,
                **id,
                &target_filter,
                ability.source_id,
                ability.controller,
            )
        })
        .copied()
        .collect();

    for obj_id in matching {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            if let Some(ref mut power) = obj.power {
                *power += num_att;
            }
            if let Some(ref mut toughness) = obj.toughness {
                *toughness += num_def;
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn resolve_pt_value(value: &PtValue) -> i32 {
    match value {
        PtValue::Fixed(n) => *n,
        PtValue::Variable(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{PtValue, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn pump_increases_power_and_toughness() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().power = Some(2);
        state.objects.get_mut(&obj_id).unwrap().toughness = Some(2);

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].power, Some(5));
        assert_eq!(state.objects[&obj_id].toughness, Some(5));
    }

    #[test]
    fn pump_with_negative_values() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().power = Some(3);
        state.objects.get_mut(&obj_id).unwrap().toughness = Some(3);

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(-2),
                toughness: PtValue::Fixed(-2),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].power, Some(1));
        assert_eq!(state.objects[&obj_id].toughness, Some(1));
    }

    #[test]
    fn pump_all_your_creatures() {
        let mut state = GameState::new_two_player(42);
        // Controller's creatures
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear1).unwrap().power = Some(2);
        state.objects.get_mut(&bear1).unwrap().toughness = Some(2);
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear 2".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&bear2).unwrap().power = Some(1);
        state.objects.get_mut(&bear2).unwrap().toughness = Some(1);
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Opponent's creature (should NOT be pumped)
        let opp = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&opp).unwrap().power = Some(3);
        state.objects.get_mut(&opp).unwrap().toughness = Some(3);
        state
            .objects
            .get_mut(&opp)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::PumpAll {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .into(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].power, Some(3));
        assert_eq!(state.objects[&bear1].toughness, Some(3));
        assert_eq!(state.objects[&bear2].power, Some(2));
        assert_eq!(state.objects[&bear2].toughness, Some(2));
        // Opponent unchanged
        assert_eq!(state.objects[&opp].power, Some(3));
        assert_eq!(state.objects[&opp].toughness, Some(3));
    }
}
