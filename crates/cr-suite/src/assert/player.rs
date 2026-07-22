//! Game-over / hand assertions (CR 104 / CR 402).

use engine::game::scenario::GameRunner;
use engine::types::game_state::WaitingFor;
use engine::types::player::PlayerId;

use super::{AssertionFailure, HandCompare};

pub fn assert_game_over(
    runner: &GameRunner,
    winner: Option<PlayerId>,
) -> Result<(), AssertionFailure> {
    match &runner.state().waiting_for {
        WaitingFor::GameOver {
            winner: actual_winner,
        } => {
            if let Some(expected) = winner {
                if *actual_winner != Some(expected) {
                    return Err(AssertionFailure {
                        kind: "game_over".into(),
                        detail: format!(
                            "expected winner {expected:?}, got {actual_winner:?}"
                        ),
                    });
                }
            }
            Ok(())
        }
        other => Err(AssertionFailure {
            kind: "game_over".into(),
            detail: format!("expected GameOver, got {other:?}"),
        }),
    }
}

pub fn assert_game_not_over(runner: &GameRunner) -> Result<(), AssertionFailure> {
    if matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }) {
        return Err(AssertionFailure {
            kind: "game_not_over".into(),
            detail: "game unexpectedly ended".into(),
        });
    }
    Ok(())
}

pub fn assert_hand_count(
    runner: &GameRunner,
    player: PlayerId,
    expected: usize,
    compare: HandCompare,
) -> Result<(), AssertionFailure> {
    let actual = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .ok_or_else(|| AssertionFailure {
            kind: "hand_count".into(),
            detail: format!("player {player:?} not found"),
        })?;

    let ok = match compare {
        HandCompare::AtLeast => actual >= expected,
        HandCompare::Equals => actual == expected,
    };
    if !ok {
        return Err(AssertionFailure {
            kind: "hand_count".into(),
            detail: format!(
                "player {player:?}: hand count {actual} does not satisfy {compare:?} {expected}"
            ),
        });
    }
    Ok(())
}
