use rand::Rng;

use crate::game::quantity::resolve_quantity;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::resolve_ability_chain;

/// CR 705: Flip a coin and optionally execute win/lose effects.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (win_effect, lose_effect) = match &ability.effect {
        Effect::FlipCoin {
            win_effect,
            lose_effect,
        } => (win_effect.as_deref(), lose_effect.as_deref()),
        _ => return Err(EffectError::MissingParam("FlipCoin".to_string())),
    };

    // CR 705.1: Flip a coin using the game's seeded RNG.
    let won = state.rng.random_bool(0.5);

    events.push(GameEvent::CoinFlipped {
        player_id: ability.controller,
        won,
    });

    // CR 705.2: Execute the appropriate branch.
    let branch = if won { win_effect } else { lose_effect };
    if let Some(def) = branch {
        let sub = ResolvedAbility::new(
            *def.effect.clone(),
            ability.targets.clone(),
            ability.source_id,
            ability.controller,
        );
        resolve_ability_chain(state, &sub, events, 0)?;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FlipCoin,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 705: Flip N coins. For each flip that comes up heads (won), execute
/// `win_effect`; for each that comes up tails (lost), execute `lose_effect`.
/// Generalization of `resolve` for "flip N coins" patterns where the Oracle
/// text binds the heads count to a downstream effect (e.g., Ral Zarek's -7:
/// target opponent skips one turn per heads).
pub fn resolve_flip_coins(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count_expr, win_effect, lose_effect) = match &ability.effect {
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
        } => (count, win_effect.as_deref(), lose_effect.as_deref()),
        _ => return Err(EffectError::MissingParam("FlipCoins".to_string())),
    };

    // CR 107.1: resolve `count` in the ability's context; clamp at zero.
    let n =
        resolve_quantity(state, count_expr, ability.controller, ability.source_id).max(0) as u32;

    // CR 705.1: Flip each coin with the game's seeded RNG, routing each
    // outcome through the appropriate branch exactly as the single-flip
    // resolver does — so downstream `win_effect`/`lose_effect` see the same
    // stacking/target semantics whether they ran once or N times.
    for _ in 0..n {
        let won = state.rng.random_bool(0.5);
        events.push(GameEvent::CoinFlipped {
            player_id: ability.controller,
            won,
        });
        let branch = if won { win_effect } else { lose_effect };
        if let Some(def) = branch {
            let sub = ResolvedAbility::new(
                *def.effect.clone(),
                ability.targets.clone(),
                ability.source_id,
                ability.controller,
            );
            resolve_ability_chain(state, &sub, events, 0)?;
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FlipCoins,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 705: Flip coins until you lose a flip, then execute effect.
pub fn resolve_until_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let win_effect = match &ability.effect {
        Effect::FlipCoinUntilLose { win_effect } => win_effect.as_ref(),
        _ => return Err(EffectError::MissingParam("FlipCoinUntilLose".to_string())),
    };

    // CR 705: Flip coins until a flip is lost. Count the wins.
    // Safety cap prevents infinite loops with pathological RNG seeds.
    const MAX_FLIPS: u32 = 1000;
    let mut win_count = 0u32;
    for _ in 0..MAX_FLIPS {
        let won = state.rng.random_bool(0.5);
        events.push(GameEvent::CoinFlipped {
            player_id: ability.controller,
            won,
        });
        if !won {
            break;
        }
        win_count += 1;
    }

    // Execute the win effect once for each win (via repeat_for-like iteration).
    if win_count > 0 {
        for _ in 0..win_count {
            let sub = ResolvedAbility::new(
                *win_effect.effect.clone(),
                ability.targets.clone(),
                ability.source_id,
                ability.controller,
            );
            resolve_ability_chain(state, &sub, events, 0)?;
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::FlipCoinUntilLose,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, QuantityExpr};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn flip_coin_emits_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CoinFlipped { .. })));
    }

    #[test]
    fn flip_coin_with_branches_resolves_one() {
        let mut state = GameState::new_two_player(42);

        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: crate::types::ability::GainLifePlayer::Controller,
            },
        ));
        let lose_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
        ));

        let ability = ResolvedAbility::new(
            Effect::FlipCoin {
                win_effect: Some(win_effect),
                lose_effect: Some(lose_effect),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let initial_life = state.players[0].life;
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Exactly one branch should have fired — life changed
        let new_life = state.players[0].life;
        assert_ne!(new_life, initial_life, "One branch should have fired");
        // Either gained 5 (won) or lost 3 (lost)
        assert!(
            new_life == initial_life + 5 || new_life == initial_life - 3,
            "Expected +5 or -3, got {}",
            new_life - initial_life
        );
    }

    #[test]
    fn flip_coin_until_lose_emits_multiple_events() {
        let mut state = GameState::new_two_player(42);
        // Add cards to library to draw from
        for i in 0..10 {
            crate::game::zones::create_object(
                &mut state,
                crate::types::identifiers::CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                crate::types::zones::Zone::Library,
            );
        }

        let ability = ResolvedAbility::new(
            Effect::FlipCoinUntilLose {
                win_effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: crate::types::ability::TargetFilter::Controller,
                    },
                )),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve_until_lose(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Must have at least one CoinFlipped event (the losing flip)
        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert!(flip_count >= 1);

        // The last CoinFlipped should be a loss
        let last_flip = events
            .iter()
            .rev()
            .find(|e| matches!(e, GameEvent::CoinFlipped { .. }));
        assert!(matches!(
            last_flip,
            Some(GameEvent::CoinFlipped { won: false, .. })
        ));
    }

    #[test]
    fn flip_coins_emits_n_coin_flip_events() {
        // CR 705.1: FlipCoins with count=5 emits exactly 5 CoinFlipped events.
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 5 },
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();

        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert_eq!(flip_count, 5);
    }

    #[test]
    fn flip_coins_zero_count_is_noop() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 0 },
                win_effect: None,
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();
        let flip_count = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
            .count();
        assert_eq!(flip_count, 0);
    }

    #[test]
    fn flip_coins_runs_win_effect_per_heads() {
        // CR 705.2: `win_effect` fires once per heads. With a deterministic
        // seed and 4 coins, the exact heads count is stable; assert that the
        // win_effect ran exactly that many times.
        let mut state = GameState::new_two_player(42);
        let initial_life = state.players[0].life;

        let win_effect = Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: crate::types::ability::GainLifePlayer::Controller,
            },
        ));

        let ability = ResolvedAbility::new(
            Effect::FlipCoins {
                count: QuantityExpr::Fixed { value: 4 },
                win_effect: Some(win_effect),
                lose_effect: None,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_flip_coins(&mut state, &ability, &mut events).unwrap();

        let heads = events
            .iter()
            .filter(|e| matches!(e, GameEvent::CoinFlipped { won: true, .. }))
            .count() as i32;
        assert_eq!(state.players[0].life - initial_life, heads);
    }
}
