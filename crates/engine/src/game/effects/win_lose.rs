use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use crate::game::elimination::eliminate_player;
use crate::game::players;

/// CR 104.3a: Resolve "lose the game" — the affected player loses.
///
/// Target resolution:
/// - If the ability has player targets, those players lose.
/// - Otherwise, the ability's controller loses (self-inflicted, e.g. "you lose the game").
pub fn resolve_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let players_to_eliminate: Vec<_> = if ability.targets.is_empty() {
        // No target: controller loses (e.g., "you lose the game")
        vec![ability.controller]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                _ => None,
            })
            .collect()
    };

    for pid in players_to_eliminate {
        // CR 104.3a: A player who loses the game leaves the game.
        eliminate_player(state, pid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::LoseTheGame,
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 104.3a: Resolve "win the game" — all opponents of the controller lose.
///
/// A player wins by eliminating all opponents. Per CR 104.3a, if a player
/// wins the game, all other players lose the game.
pub fn resolve_win(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 104.3a: A player wins the game if all of their opponents have lost.
    let opponents: Vec<_> = players::opponents(state, ability.controller)
        .into_iter()
        .filter(|&pid| players::is_alive(state, pid))
        .collect();

    for pid in opponents {
        eliminate_player(state, pid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::WinTheGame,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn lose_the_game_eliminates_controller_when_untargeted() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::LoseTheGame, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
    }

    #[test]
    fn lose_the_game_eliminates_targeted_player() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseTheGame,
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn win_the_game_eliminates_all_opponents() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(Effect::WinTheGame, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve_win(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].is_eliminated);
        assert!(!state.players[0].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }
}
