use std::str::FromStr;

use crate::types::ability::{
    effect_variant_name, Effect, EffectError, ResolvedAbility, TargetRef, TargetSpec,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Animate effect: turn a non-creature permanent into a creature.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (power, toughness, types_list) = match &ability.effect {
        Effect::Animate {
            power,
            toughness,
            types,
            ..
        } => (
            power.unwrap_or(0),
            toughness.unwrap_or(0),
            Some(types.as_slice()),
        ),
        _ => {
            let p = ability
                .params
                .get("Power")
                .map(|v| v.parse().unwrap_or(0))
                .unwrap_or(0);
            let t = ability
                .params
                .get("Toughness")
                .map(|v| v.parse().unwrap_or(0))
                .unwrap_or(0);
            (p, t, None)
        }
    };

    let targets = resolve_animate_targets(ability);

    for obj_id in targets {
        let obj = state
            .objects
            .get_mut(&obj_id)
            .ok_or(EffectError::ObjectNotFound(obj_id))?;

        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);

        // Add types from typed Effect field, or fall back to params
        if let Some(types) = types_list {
            for t in types {
                let t = t.trim();
                if let Ok(core) = CoreType::from_str(t) {
                    if !obj.card_types.core_types.contains(&core) {
                        obj.card_types.core_types.push(core);
                    }
                } else if !obj.card_types.subtypes.contains(&t.to_string()) {
                    obj.card_types.subtypes.push(t.to_string());
                }
            }
        } else if let Some(types_str) = ability.params.get("Types") {
            for t in types_str.split(',') {
                let t = t.trim();
                if let Ok(core) = CoreType::from_str(t) {
                    if !obj.card_types.core_types.contains(&core) {
                        obj.card_types.core_types.push(core);
                    }
                } else if !obj.card_types.subtypes.contains(&t.to_string()) {
                    obj.card_types.subtypes.push(t.to_string());
                }
            }
        }

        state.layers_dirty = true;
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

fn resolve_animate_targets(ability: &ResolvedAbility) -> Vec<crate::types::identifiers::ObjectId> {
    if let Effect::Animate { target, .. } = &ability.effect {
        if matches!(target, TargetSpec::None) {
            return vec![ability.source_id];
        }
    } else if let Some(defined) = ability.params.get("Defined") {
        if defined == "Self" {
            return vec![ability.source_id];
        }
    }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    #[test]
    fn animate_sets_power_toughness_and_creature_type() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Animate".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::from([
                ("Power".to_string(), "7".to_string()),
                ("Toughness".to_string(), "7".to_string()),
                ("Types".to_string(), "Creature,Beast".to_string()),
                ("Defined".to_string(), "Self".to_string()),
            ]),
            targets: vec![],
            source_id: obj_id,
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.power, Some(7));
        assert_eq!(obj.toughness, Some(7));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert!(obj.card_types.subtypes.contains(&"Beast".to_string()));
    }
}
