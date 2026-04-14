use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use crate::game::elimination::eliminate_player;
use crate::game::players;
use crate::game::static_abilities::player_has_cant_win;

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

/// CR 104.2b + CR 104.3e: Resolve "win the game" — all opponents of the controller lose.
///
/// A player wins by eliminating all opponents. Per CR 104.2b, an effect may
/// state that a player wins the game. Per CR 104.3e, all other players lose.
pub fn resolve_win(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 104.2b: CantWinTheGame blocks effect-based wins. If the controller is
    // under a CantWinTheGame static, the win effect resolves but has no
    // effect — no opponents are eliminated. CR 104.2a's last-player-standing
    // case is enforced separately in elimination::check_game_over and
    // correctly overrides CantWinTheGame (per the CR text, 104.2a "overrides
    // all effects that would preclude that player from winning the game").
    if player_has_cant_win(state, ability.controller) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::WinTheGame,
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 104.2b + CR 104.3e: A player wins the game — all opponents lose.
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
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, Effect, ResolvedAbility, StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    /// Helper: add a permanent with `CantWinTheGame` static affecting players
    /// matching the given `ControllerRef` (from the permanent's perspective).
    /// Mirrors `sba::tests::add_cant_lose_permanent`.
    fn add_cant_win_permanent(
        state: &mut GameState,
        owner: PlayerId,
        affected_controller: ControllerRef,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(100),
            owner,
            "Platinum Angel".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantWinTheGame).affected(TargetFilter::Typed(
                TypedFilter::default().controller(affected_controller),
            )),
        );
        id
    }

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

    /// CR 104.2b: The controller of the win effect is under `CantWinTheGame`,
    /// so the effect resolves but no opponents are eliminated.
    #[test]
    fn win_effect_blocked_when_controller_has_cant_win() {
        let mut state = GameState::new_two_player(42);
        // Platinum Angel owned by PlayerId(0) with CantWinTheGame affecting You.
        add_cant_win_permanent(&mut state, PlayerId(0), ControllerRef::You);

        let ability = ResolvedAbility::new(Effect::WinTheGame, vec![], ObjectId(1), PlayerId(0));
        let mut events = Vec::new();

        resolve_win(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].is_eliminated);
        assert!(!state.players[1].is_eliminated);
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::WinTheGame,
                ..
            }
        )));
    }

    /// CR 104.2b: `CantWinTheGame` only protects the player it's affecting. A
    /// permanent on PlayerId(1)'s side does not block PlayerId(0) from winning.
    #[test]
    fn win_effect_not_blocked_when_cant_win_affects_other_player() {
        let mut state = GameState::new_two_player(42);
        // Permanent owned by PlayerId(1), affects its controller (PlayerId(1)).
        add_cant_win_permanent(&mut state, PlayerId(1), ControllerRef::You);

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

    /// CR 104.2b: Platinum Angel's full clause — the permanent's controller
    /// can't lose, and opponents can't win. This test covers the "opponents
    /// can't win" half via the `Opponent` filter.
    #[test]
    fn win_effect_blocked_mirrors_platinum_angel_clause() {
        let mut state = GameState::new_two_player(42);
        // Platinum Angel owned by PlayerId(0) with two statics:
        //  - CantLoseTheGame affecting You (PlayerId(0))
        //  - CantWinTheGame affecting Opponent (PlayerId(1))
        let angel = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Platinum Angel".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&angel).unwrap();
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::CantLoseTheGame).affected(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
            ));
        obj.static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantWinTheGame).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
            );

        // PlayerId(1) tries to win — blocked by the opponent-scoped CantWin.
        let ability = ResolvedAbility::new(Effect::WinTheGame, vec![], ObjectId(1), PlayerId(1));
        let mut events = Vec::new();

        resolve_win(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].is_eliminated);
        assert!(!state.players[1].is_eliminated);
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::WinTheGame,
                ..
            }
        )));
    }
}
