use crate::game::effects::life::{apply_life_totals_assignment, LifeAssignmentOutcome};
use crate::game::static_abilities::{player_has_cant_gain_life, player_has_cant_lose_life};
use crate::game::targeting::resolve_effect_player_ref;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingEffectResolved};
use crate::types::player::PlayerId;

/// CR 701.12a: Two players exchange life totals (Soul Conduit, Axis of
/// Mortality, Magus of the Mirror, Mirror Universe).
///
/// Both players' life totals swap simultaneously. Per CR 701.12c each player
/// gains or loses the amount of life necessary to equal the other player's
/// previous life total (a gain/loss, not a raw set), and per CR 701.12a it is
/// all-or-nothing: if EITHER player's required life change is forbidden (CR
/// 119.7 can't-gain when rising, CR 119.8 can't-lose when falling), no part of
/// the exchange occurs.
///
/// Player resolution per slot:
/// - A context-ref filter (`Controller` for "you") resolves to the ability's
///   controller and consumes no declared target.
/// - Any other filter (`Player` for "target player", an opponent filter for
///   "target opponent") consumes one `TargetRef::Player` from `ability.targets`
///   in declaration order — mirroring the dual-target slot surfacing in
///   `ability_utils`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExchangeLifeTotals { player_a, player_b } = &ability.effect else {
        // Dispatcher in effects/mod.rs only routes ExchangeLifeTotals here.
        return Ok(());
    };

    let resolved_kind = EffectKind::from(&ability.effect);
    let emit_noop = |events: &mut Vec<GameEvent>| {
        events.push(GameEvent::EffectResolved {
            kind: resolved_kind,
            source_id: ability.source_id,
        });
    };

    // Resolve each player slot in declaration order. Non-context-ref filters
    // consume the next declared `TargetRef::Player`; context-refs (Controller)
    // resolve from the ability directly and consume no target.
    let mut declared_players = ability.targets.iter().filter_map(|t| match t {
        TargetRef::Player(pid) => Some(*pid),
        TargetRef::Object(_) => None,
    });
    let mut resolve_slot = |filter: &_| -> Option<PlayerId> {
        if crate::types::ability::TargetFilter::is_context_ref(filter) {
            resolve_effect_player_ref(state, ability, filter)
        } else {
            declared_players.next()
        }
    };

    let Some(pid_a) = resolve_slot(player_a) else {
        // CR 701.12a: can't complete — no part occurs.
        emit_noop(events);
        return Ok(());
    };
    let Some(pid_b) = resolve_slot(player_b) else {
        emit_noop(events);
        return Ok(());
    };

    // CR 701.12b analog: a player exchanging life totals with themselves does
    // nothing.
    if pid_a == pid_b {
        emit_noop(events);
        return Ok(());
    }

    // CR 701.12a: capture BOTH previous values before any mutation.
    let life_a = state
        .players
        .iter()
        .find(|p| p.id == pid_a)
        .ok_or(EffectError::PlayerNotFound)?
        .life;
    let life_b = state
        .players
        .iter()
        .find(|p| p.id == pid_b)
        .ok_or(EffectError::PlayerNotFound)?
        .life;

    // CR 119.7 / CR 119.8 + CR 701.12a: All-or-nothing pre-check. Player A moves
    // toward life_b, player B toward life_a. If EITHER change is forbidden
    // (can't-gain when rising, can't-lose when falling), no part occurs.
    let blocked = |state: &GameState, pid: PlayerId, from: i32, to: i32| -> bool {
        match to.cmp(&from) {
            std::cmp::Ordering::Greater => player_has_cant_gain_life(state, pid),
            std::cmp::Ordering::Less => player_has_cant_lose_life(state, pid),
            std::cmp::Ordering::Equal => false,
        }
    };
    if blocked(state, pid_a, life_a, life_b) || blocked(state, pid_b, life_b, life_a) {
        emit_noop(events);
        return Ok(());
    }

    // CR 701.12c: each player's life total becomes the other's previous total (a
    // gain/loss, not a set), applied simultaneously from the pre-swap snapshot.
    // Delegated to the shared N-slot permutation helper — the 2-player exchange is
    // its special case.
    match apply_life_totals_assignment(
        state,
        &[(pid_a, life_b), (pid_b, life_a)],
        ability.controller,
        Some(PendingEffectResolved::new(resolved_kind, ability.source_id)),
        events,
    )? {
        // CR 616.1: a competing replacement required a player choice; the helper
        // installed the WaitingFor and the resume path completes resolution.
        LifeAssignmentOutcome::Deferred => Ok(()),
        LifeAssignmentOutcome::Applied => {
            events.push(GameEvent::EffectResolved {
                kind: resolved_kind,
                source_id: ability.source_id,
            });
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn exchange_ability(targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ExchangeLifeTotals {
                player_a: TargetFilter::Player,
                player_b: TargetFilter::Player,
            },
            targets,
            crate::types::identifiers::ObjectId(100),
            PlayerId(0),
        )
    }

    /// CR 701.12c + CR 701.12a: a straight swap. P0=20, P1=5 → P0=5, P1=20.
    /// Fails (stays 20/5) if the resolver is a no-op.
    #[test]
    fn exchange_life_totals_swaps() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 5;

        let ability = exchange_ability(vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 5);
        assert_eq!(state.players[1].life, 20);
    }

    /// CR 701.12a + CR 119.7: All-or-nothing. P1 can't gain life and the swap
    /// would raise P1 (5 → 20), so NO part of the exchange occurs — both totals
    /// stay unchanged.
    #[test]
    fn exchange_life_totals_blocked_when_cant_gain_does_nothing() {
        let mut state = GameState::new_two_player(99);
        state.players[0].life = 20;
        state.players[1].life = 5;

        // Player 1 can't gain life.
        let lock = create_object(
            &mut state,
            CardId(901),
            PlayerId(1),
            "Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantGainLife).affected(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
            );

        let ability = exchange_ability(vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.12a: neither side changes.
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 5);
    }

    /// CR 701.12b analog: a player exchanging with themselves does nothing.
    #[test]
    fn exchange_life_totals_same_player_is_noop() {
        let mut state = GameState::new_two_player(7);
        state.players[0].life = 13;

        let ability = exchange_ability(vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(0)),
        ]);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 13);
    }
}
