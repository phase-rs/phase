use std::str::FromStr;

use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::{BendingType, GameEvent};
use crate::types::game_state::GameState;

/// Animate effect: turn a non-creature permanent into a creature.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (power, toughness, types_list, kw_list, is_earthbend) = match &ability.effect {
        Effect::Animate {
            power,
            toughness,
            types,
            keywords,
            is_earthbend,
            ..
        } => (
            power.unwrap_or(0),
            toughness.unwrap_or(0),
            types.as_slice(),
            keywords.as_slice(),
            *is_earthbend,
        ),
        _ => (0, 0, [].as_slice(), [].as_slice(), false),
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

        // Add types from typed Effect field
        for t in types_list {
            let t = t.trim();
            if let Ok(core) = CoreType::from_str(t) {
                if !obj.card_types.core_types.contains(&core) {
                    obj.card_types.core_types.push(core);
                }
            } else if !obj.card_types.subtypes.contains(&t.to_string()) {
                obj.card_types.subtypes.push(t.to_string());
            }
        }

        // Grant keywords (e.g., Haste for Earthbending)
        for kw in kw_list {
            if !obj.keywords.contains(kw) {
                obj.keywords.push(kw.clone());
            }
        }

        state.layers_dirty = true;
    }

    // Emit earthbend event for bending trigger system (mirrors grant_permission.rs Airbend pattern)
    if is_earthbend {
        events.push(GameEvent::Earthbend {
            source_id: ability.source_id,
            controller: ability.controller,
        });
        if let Some(p) = state
            .players
            .iter_mut()
            .find(|p| p.id == ability.controller)
        {
            p.bending_types_this_turn.insert(BendingType::Earth);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn resolve_animate_targets(ability: &ResolvedAbility) -> Vec<crate::types::identifiers::ObjectId> {
    if let Effect::Animate { target, .. } = &ability.effect {
        if matches!(target, TargetFilter::None) {
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

        let ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(7),
                toughness: Some(7),
                types: vec!["Creature".to_string(), "Beast".to_string()],
                keywords: vec![],
                target: TargetFilter::None,
                is_earthbend: false,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.power, Some(7));
        assert_eq!(obj.toughness, Some(7));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert!(obj.card_types.subtypes.contains(&"Beast".to_string()));
    }
}
