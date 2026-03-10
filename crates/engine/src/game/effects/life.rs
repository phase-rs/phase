use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;

/// Gain life for the controller.
/// Reads `LifeAmount` param.
pub fn resolve_gain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount: i32 = ability
        .params
        .get("LifeAmount")
        .ok_or_else(|| EffectError::MissingParam("LifeAmount".to_string()))?
        .parse()
        .map_err(|_| EffectError::InvalidParam("LifeAmount must be a number".to_string()))?;

    let proposed = ProposedEvent::LifeGain {
        player_id: ability.controller,
        amount: amount as u32,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::LifeGain {
                player_id,
                amount: gain_amount,
                ..
            } = event
            {
                let player = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;
                player.life += gain_amount as i32;

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: gain_amount as i32,
                });
            }
        }
        ReplacementResult::Prevented => {}
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

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Lose life for the target player (or controller if no target).
/// Reads `LifeAmount` param.
pub fn resolve_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    use crate::types::ability::TargetRef;

    let amount: i32 = ability
        .params
        .get("LifeAmount")
        .ok_or_else(|| EffectError::MissingParam("LifeAmount".to_string()))?
        .parse()
        .map_err(|_| EffectError::InvalidParam("LifeAmount must be a number".to_string()))?;

    // Determine target player: use first player target or fall back to controller
    let target_player_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Player(pid) = t {
                Some(*pid)
            } else {
                None
            }
        })
        .unwrap_or(ability.controller);

    let proposed = ProposedEvent::LifeLoss {
        player_id: target_player_id,
        amount: amount as u32,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::LifeLoss {
                player_id,
                amount: loss_amount,
                ..
            } = event
            {
                let player = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;
                player.life -= loss_amount as i32;

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: -(loss_amount as i32),
                });
            }
        }
        ReplacementResult::Prevented => {}
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

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn gain_life_increases_controller_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "GainLife".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "GainLife".to_string(),
            params: HashMap::from([("LifeAmount".to_string(), "5".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 25);
    }

    #[test]
    fn lose_life_decreases_target_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "LoseLife".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "LoseLife".to_string(),
            params: HashMap::from([("LifeAmount".to_string(), "3".to_string())]),
            targets: vec![TargetRef::Player(PlayerId(1))],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn gain_life_emits_positive_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "GainLife".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "GainLife".to_string(),
            params: HashMap::from([("LifeAmount".to_string(), "4".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == 4)));
    }

    #[test]
    fn lose_life_emits_negative_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "LoseLife".to_string(),
                params: std::collections::HashMap::new(),
            },
            api_type: "LoseLife".to_string(),
            params: HashMap::from([("LifeAmount".to_string(), "2".to_string())]),
            targets: vec![TargetRef::Player(PlayerId(0))],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == -2)));
    }
}
