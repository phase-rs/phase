use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;

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
        let proposed = ProposedEvent::Damage {
            source_id: ability.source_id,
            target: target.clone(),
            amount: num_dmg,
            is_combat: false,
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::Damage { target: ref t, amount, .. } = event {
                    match t {
                        TargetRef::Object(obj_id) => {
                            let obj = state
                                .objects
                                .get_mut(obj_id)
                                .ok_or(EffectError::ObjectNotFound(*obj_id))?;
                            obj.damage_marked += amount;
                        }
                        TargetRef::Player(player_id) => {
                            let player = state
                                .players
                                .iter_mut()
                                .find(|p| p.id == *player_id)
                                .ok_or(EffectError::PlayerNotFound)?;
                            player.life -= amount as i32;
                        }
                    }
                    events.push(GameEvent::DamageDealt {
                        source_id: ability.source_id,
                        target: t.clone(),
                        amount,
                    });
                }
            }
            ReplacementResult::Prevented => {
                // Damage was prevented, skip
            }
            ReplacementResult::NeedsChoice(player) => {
                let candidate_count = state
                    .pending_replacement
                    .as_ref()
                    .map(|p| p.candidates.len())
                    .unwrap_or(0);
                state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
                    player,
                    candidate_count,
                };
                return Ok(());
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Deal damage to all permanents (and optionally players) matching the `Valid` filter.
/// Reads `NumDmg` and `Valid` params.
pub fn resolve_all(
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

    let filter = ability
        .params
        .get("Valid")
        .map(|s| s.as_str())
        .unwrap_or("Creature");

    // Collect matching object IDs
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| super::matches_filter(obj, filter, ability.controller))
                .unwrap_or(false)
        })
        .copied()
        .collect();

    for obj_id in matching {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.damage_marked += num_dmg;
        }
        events.push(GameEvent::DamageDealt {
            source_id: ability.source_id,
            target: TargetRef::Object(obj_id),
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
    use crate::types::card_type::CoreType;
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

    #[test]
    fn damage_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(&mut state, CardId(1), PlayerId(0), "Bear".to_string(), Zone::Battlefield);
        state.objects.get_mut(&bear1).unwrap().card_types.core_types.push(CoreType::Creature);

        let bear2 = create_object(&mut state, CardId(2), PlayerId(1), "Opp Bear".to_string(), Zone::Battlefield);
        state.objects.get_mut(&bear2).unwrap().card_types.core_types.push(CoreType::Creature);

        let ability = ResolvedAbility {
            api_type: "DamageAll".to_string(),
            params: HashMap::from([
                ("NumDmg".to_string(), "2".to_string()),
                ("Valid".to_string(), "Creature".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&bear1].damage_marked, 2);
        assert_eq!(state.objects[&bear2].damage_marked, 2);
    }
}
