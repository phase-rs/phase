use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::resolve_player_for_context_ref;
use crate::game::elimination::eliminate_player;
use crate::game::players;
use crate::game::static_abilities::player_has_cant_win;

/// CR 104.3e: Resolve "lose the game" — the affected player loses.
///
/// Player resolution (in priority order):
/// 1. If `Effect::LoseTheGame.target` names a context-ref subject
///    (`TriggeringPlayer`, `Controller`, `SelfRef`), resolve it directly —
///    these never produce stack-time target slots (they bind to the
///    current trigger event or to the ability's controller).
/// 2. Otherwise, if the ability has player targets in `ability.targets`,
///    those players lose.
/// 3. Otherwise, the ability's controller loses (the default
///    "you lose the game" reading).
///
/// The context-ref path is required for Ezio Auditore da Firenze's
/// "that player loses the game" reflexive sub-ability: the parser lowers
/// "that player" to `TargetFilter::TriggeringPlayer`, which is a context
/// ref (CR 603.7c) — `extract_target_filter_from_effect` filters context
/// refs out of stack-time target slots, so `ability.targets` is empty even
/// though the effect names a specific subject.
pub fn resolve_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::LoseTheGame { target } => target.as_ref(),
        _ => return Err(EffectError::MissingParam("expected LoseTheGame".into())),
    };

    let players_to_eliminate: Vec<_> = match target_filter {
        // Directed single-player subject ("that player loses", "you lose",
        // "target player loses"). The canonical context-ref resolver in
        // `effects::resolve_player_for_context_ref` is a superset of the
        // prior bespoke helper: it handles Controller, SelfRef-equivalent,
        // TriggeringPlayer (via event-context lookup), and the
        // `TargetFilter::Player` case by reading `ability.targets`.
        Some(filter) => vec![resolve_player_for_context_ref(state, ability, filter)],
        None if ability.targets.is_empty() => {
            // No directed subject and no targets: controller loses
            // (e.g., "you lose the game").
            vec![ability.controller]
        }
        None => {
            // CR 115.1: Multi-player loss path (e.g., "target player and
            // target opponent lose the game"). Iterate every player target
            // captured at announcement.
            ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Player(pid) => Some(*pid),
                    _ => None,
                })
                .collect()
        }
    };

    for pid in players_to_eliminate {
        // CR 104.3e + CR 810.8a: An effect-stated loss is still a loss, so a
        // "can't lose the game" continuous effect precludes it (CR 810.8a's
        // Platinum Angel example: "Neither that player nor their teammate can
        // lose the game" — the same clause Angel's Grace and Gideon of the
        // Trials' emblem grant). Mirrors the `player_has_cant_win` gate on the
        // sibling `resolve_win` path (CR 104.2b). The concede (CR 104.3a) and
        // SBA (CR 104.3b-d) paths do not route through here: concede must
        // never be blocked, and the SBA loop pre-filters via the same
        // `player_has_cant_lose` predicate.
        if crate::game::sba::player_has_cant_lose(state, pid) {
            continue;
        }
        // CR 104.3e: A player who loses the game leaves the game.
        eliminate_player(state, pid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::LoseTheGame,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 104.2b + CR 104.3e: Resolve "win the game" — the winner's opponents lose.
///
/// Winner resolution (in priority order):
/// 1. If `Effect::WinTheGame.target` names a context-ref subject, that
///    player wins (CR 104.2b). Mirrors the `LoseTheGame` directed-subject
///    path used by "that player wins the game" wordings.
/// 2. Otherwise, if `ability.targets` contains a player target, that
///    player wins.
/// 3. Otherwise, the ability's controller wins (the standard "you win the
///    game" reading).
///
/// After resolving the winner, all opponents of the winner are eliminated
/// (CR 104.3e), unless the winner is under `CantWinTheGame` (CR 104.2b).
pub fn resolve_win(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target_filter = match &ability.effect {
        Effect::WinTheGame { target } => target.as_ref(),
        _ => return Err(EffectError::MissingParam("expected WinTheGame".into())),
    };

    // Directed single-winner subject. The canonical context-ref resolver in
    // `effects::resolve_player_for_context_ref` handles Controller, SelfRef,
    // TriggeringPlayer, and `TargetFilter::Player` (consulting
    // `ability.targets`) in one call; absence of a filter falls back to the
    // controller (standard "you win the game" reading).
    let winner = match target_filter {
        Some(filter) => resolve_player_for_context_ref(state, ability, filter),
        None => ability.controller,
    };

    // CR 104.2b: CantWinTheGame blocks effect-based wins. If the winner is
    // under a CantWinTheGame static, the win effect resolves but has no
    // effect — no opponents are eliminated. CR 104.2a's last-player-standing
    // case is enforced separately in elimination::check_game_over and
    // correctly overrides CantWinTheGame (per the CR text, 104.2a "overrides
    // all effects that would preclude that player from winning the game").
    if player_has_cant_win(state, winner) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::WinTheGame,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 104.2b + CR 104.3e: A player wins the game — all opponents lose.
    let opponents: Vec<_> = players::opponents(state, winner)
        .into_iter()
        .filter(|&pid| players::is_alive(state, pid))
        .collect();

    for pid in opponents {
        eliminate_player(state, pid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::WinTheGame,
        source_id: ability.source_id,
        subject: None,
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
        let ability = ResolvedAbility::new(
            Effect::LoseTheGame { target: None },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
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
            Effect::LoseTheGame { target: None },
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

    /// CR 104.3e + CR 603.7c + CR 109.4: Ezio Auditore da Firenze regression
    /// (issue #1962, TEST-ONLY hardening) — `Effect::LoseTheGame` with
    /// `target: Some(TargetFilter::TriggeringPlayer)` must eliminate the
    /// damaged player (extracted from `state.current_trigger_event`), NOT
    /// the ability's controller. This locks the load-bearing
    /// "right player loses" invariant exercised by `resolve_lose`'s
    /// context-ref branch — the bug the issue reported was Ezio's
    /// controller eliminating *themselves* because the directed-subject
    /// target was dropped at the parser layer and `resolve_lose` fell
    /// through to the `ability.controller` default. With the fix, the
    /// parser supplies `TriggeringPlayer`, and the resolver reads
    /// `current_trigger_event`'s damage target to land elimination on
    /// the correct player.
    #[test]
    fn lose_the_game_with_triggering_player_target_eliminates_damaged_player_not_controller() {
        use crate::types::events::GameEvent;
        use crate::types::identifiers::ObjectId;

        let mut state = GameState::new_two_player(42);

        // Seed the trigger event as `process_triggers` would: a combat
        // damage event targeting P1 (the damaged player), with source
        // controlled by P0 (the ability controller).
        state.current_trigger_event = Some(GameEvent::DamageDealt {
            source_id: ObjectId(99),
            target: TargetRef::Player(PlayerId(1)),
            amount: 4,
            is_combat: true,
            excess: 0,
        });

        // Ability controlled by P0, targeting TriggeringPlayer (P1 — the
        // damaged player). `ability.targets` is empty because context-ref
        // filters never produce stack-time target slots (see resolve_lose
        // docs and `extract_target_filter_from_effect`).
        let ability = ResolvedAbility::new(
            Effect::LoseTheGame {
                target: Some(TargetFilter::TriggeringPlayer),
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.players[1].is_eliminated,
            "P1 (damaged player extracted from current_trigger_event) must be eliminated"
        );
        assert!(
            !state.players[0].is_eliminated,
            "P0 (ability controller) must NOT be eliminated — \
             TriggeringPlayer target must not fall through to the controller default \
             (issue #1962: Ezio's controller used to lose the game themselves)"
        );
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
        let ability = ResolvedAbility::new(
            Effect::WinTheGame { target: None },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
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

        let ability = ResolvedAbility::new(
            Effect::WinTheGame { target: None },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
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

        let ability = ResolvedAbility::new(
            Effect::WinTheGame { target: None },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
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

    /// CR 104.3e + CR 810.8a: An effect-stated loss ("you lose the game",
    /// Pact triggers, Door to Nothingness) is precluded by a `CantLoseTheGame`
    /// static on the affected player — the "can't lose" half of the Platinum
    /// Angel / Angel's Grace / Gideon-emblem clause. RED if `resolve_lose`
    /// eliminates unconditionally. The untargeted elimination test above is
    /// the live-path sibling proving the loss path executes without the
    /// static (reach guard).
    #[test]
    fn lose_effect_blocked_by_cant_lose_static() {
        let mut state = GameState::new_two_player(42);
        let angel = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Platinum Angel".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&angel)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantLoseTheGame).affected(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
            ));

        // PlayerId(0)'s own "you lose the game" effect resolves but the loss
        // is precluded (CR 810.8a's Platinum Angel example).
        let ability = ResolvedAbility::new(
            Effect::LoseTheGame { target: None },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].is_eliminated);
        assert!(!state.players[1].is_eliminated);
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::LoseTheGame,
                ..
            }
        )));
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
        let ability = ResolvedAbility::new(
            Effect::WinTheGame { target: None },
            vec![],
            ObjectId(1),
            PlayerId(1),
        );
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
