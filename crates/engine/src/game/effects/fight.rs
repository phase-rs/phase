use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Fight: two creatures deal damage equal to their power to each other simultaneously.
/// Source creature fights the target creature.
/// Reads `Defined` param for source selection (default: Self = source_id).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Source creature is the ability's source
    let source_id = ability.source_id;

    // Target creature from ability.targets
    let target_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .ok_or_else(|| EffectError::MissingParam("Fight target".to_string()))?;

    // Get source power
    let source_power = state
        .objects
        .get(&source_id)
        .ok_or(EffectError::ObjectNotFound(source_id))?
        .power
        .unwrap_or(0);

    // Get target power
    let target_power = state
        .objects
        .get(&target_id)
        .ok_or(EffectError::ObjectNotFound(target_id))?
        .power
        .unwrap_or(0);

    // Source deals damage to target (power of source -> target's damage)
    if source_power > 0 {
        let target_obj = state
            .objects
            .get_mut(&target_id)
            .ok_or(EffectError::ObjectNotFound(target_id))?;
        target_obj.damage_marked += source_power as u32;

        events.push(GameEvent::DamageDealt {
            source_id,
            target: TargetRef::Object(target_id),
            amount: source_power as u32,
        });
    }

    // Target deals damage to source (power of target -> source's damage)
    if target_power > 0 {
        let source_obj = state
            .objects
            .get_mut(&source_id)
            .ok_or(EffectError::ObjectNotFound(source_id))?;
        source_obj.damage_marked += target_power as u32;

        events.push(GameEvent::DamageDealt {
            source_id: target_id,
            target: TargetRef::Object(source_id),
            amount: target_power as u32,
        });
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    #[test]
    fn test_fight_mutual_damage() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Fight".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "Fight".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(wolf)],
            source_id: bear,
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Bear (3/3) deals 3 damage to Wolf -> Wolf has 3 damage
        assert_eq!(state.objects[&wolf].damage_marked, 3);
        // Wolf (2/2) deals 2 damage to Bear -> Bear has 2 damage
        assert_eq!(state.objects[&bear].damage_marked, 2);
    }

    #[test]
    fn test_fight_emits_damage_events() {
        let mut state = GameState::new_two_player(42);
        let bear = make_creature(&mut state, PlayerId(0), "Bear", 3, 3);
        let wolf = make_creature(&mut state, PlayerId(1), "Wolf", 2, 2);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Fight".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "Fight".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(wolf)],
            source_id: bear,
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should have 2 DamageDealt events + 1 EffectResolved
        let damage_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::DamageDealt { .. }))
            .collect();
        assert_eq!(damage_events.len(), 2);
    }

    #[test]
    fn test_fight_zero_power_no_damage() {
        let mut state = GameState::new_two_player(42);
        let wall = make_creature(&mut state, PlayerId(0), "Wall", 0, 5);
        let bear = make_creature(&mut state, PlayerId(1), "Bear", 2, 2);

        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Fight".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "Fight".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Object(bear)],
            source_id: wall,
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Wall has 0 power, deals no damage to Bear
        assert_eq!(state.objects[&bear].damage_marked, 0);
        // Bear has 2 power, deals 2 damage to Wall
        assert_eq!(state.objects[&wall].damage_marked, 2);
    }
}
