use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, GainLifePlayer, LifeAmount, ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

/// Gain life for the controller (or targeted creature's controller when player = TargetedController).
pub fn resolve_gain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, player_kind) = match &ability.effect {
        Effect::GainLife { amount, player } => (amount, player),
        _ => return Err(EffectError::MissingParam("GainLife amount".to_string())),
    };

    // Resolve the target object (if any) for TargetedController / TargetPower.
    let target_obj = ability.targets.iter().find_map(|t| {
        if let TargetRef::Object(id) = t {
            state.objects.get(id)
        } else {
            None
        }
    });

    let player_id: PlayerId = match player_kind {
        GainLifePlayer::TargetedController => target_obj
            .map(|o| o.controller)
            .unwrap_or(ability.controller),
        GainLifePlayer::Controller => ability.controller,
    };

    let final_amount = match amount {
        LifeAmount::Fixed(n) => *n,
        LifeAmount::TargetPower => target_obj.and_then(|o| o.power).unwrap_or(0),
    };

    if final_amount <= 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let proposed = ProposedEvent::LifeGain {
        player_id,
        amount: final_amount as u32,
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
                player.life_gained_this_turn += gain_amount;
                state.layers_dirty = true;

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: gain_amount as i32,
                });
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// Apply life loss from damage, running through the replacement pipeline.
/// Returns the actual amount of life lost (may differ due to replacements like doubling).
pub fn apply_damage_life_loss(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> u32 {
    if amount == 0 {
        return 0;
    }
    let proposed = ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied: HashSet::new(),
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::LifeLoss {
                player_id: pid,
                amount: loss_amount,
                ..
            } = event
            {
                if let Some(player) = state.players.iter_mut().find(|p| p.id == pid) {
                    player.life -= loss_amount as i32;
                    player.life_lost_this_turn += loss_amount;
                }
                state.layers_dirty = true;
                events.push(GameEvent::LifeChanged {
                    player_id: pid,
                    amount: -(loss_amount as i32),
                });
                loss_amount
            } else {
                0
            }
        }
        ReplacementResult::Prevented => 0,
        ReplacementResult::NeedsChoice(_) => {
            // Multiple replacement choices for life loss from damage; apply unmodified for now
            if let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) {
                player.life -= amount as i32;
                player.life_lost_this_turn += amount;
            }
            state.layers_dirty = true;
            events.push(GameEvent::LifeChanged {
                player_id,
                amount: -(amount as i32),
            });
            amount
        }
    }
}

/// Lose life for the target player (or controller if no target).
pub fn resolve_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount: i32 = match &ability.effect {
        Effect::LoseLife { amount } => *amount,
        _ => return Err(EffectError::MissingParam("LoseLife amount".to_string())),
    };

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
                player.life_lost_this_turn += loss_amount;
                state.layers_dirty = true;

                events.push(GameEvent::LifeChanged {
                    player_id,
                    amount: -(loss_amount as i32),
                });
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
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
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn gain_life_increases_controller_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: LifeAmount::Fixed(5),
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 25);
    }

    #[test]
    fn lose_life_decreases_target_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseLife { amount: 3 },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 17);
    }

    #[test]
    fn gain_life_emits_positive_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: LifeAmount::Fixed(4),
                player: GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == 4)));
    }

    #[test]
    fn lose_life_emits_negative_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseLife { amount: 2 },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == -2)));
    }
}
