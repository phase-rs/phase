use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Deal damage to each target.
/// Reads `NumDmg` param for the amount.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_dmg: u32 = ability
        .params
        .get("NumDmg")
        .ok_or_else(|| EffectError::MissingParam("NumDmg".to_string()))?
        .parse()
        .map_err(|_| EffectError::InvalidParam("NumDmg must be a number".to_string()))?;

    for target in &ability.targets {
        match target {
            TargetRef::Object(obj_id) => {
                let obj = state
                    .objects
                    .get_mut(obj_id)
                    .ok_or(EffectError::ObjectNotFound(*obj_id))?;
                obj.damage_marked += num_dmg;
            }
            TargetRef::Player(player_id) => {
                let player = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == *player_id)
                    .ok_or(EffectError::PlayerNotFound)?;
                player.life -= num_dmg as i32;
            }
        }
        events.push(GameEvent::DamageDealt {
            source_id: ability.source_id,
            target: target.clone(),
            amount: num_dmg,
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
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_ability(num_dmg: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility {
            api_type: "DealDamage".to_string(),
            params: HashMap::from([("NumDmg".to_string(), num_dmg.to_string())]),
            targets,
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn deal_damage_to_creature() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(&mut state, CardId(1), PlayerId(1), "Bear".to_string(), Zone::Battlefield);
        let ability = make_ability(3, vec![TargetRef::Object(obj_id)]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj_id].damage_marked, 3);
    }

    #[test]
    fn deal_damage_to_player() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 15);
    }

    #[test]
    fn deal_damage_emits_events() {
        let mut state = GameState::new_two_player(42);
        let ability = make_ability(2, vec![TargetRef::Player(PlayerId(0))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::DamageDealt { amount: 2, .. })));
        assert!(events.iter().any(|e| matches!(e, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn missing_num_dmg_returns_error() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            api_type: "DealDamage".to_string(),
            params: HashMap::new(),
            targets: vec![TargetRef::Player(PlayerId(0))],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(matches!(result, Err(EffectError::MissingParam(_))));
    }
}
