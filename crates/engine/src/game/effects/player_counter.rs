use crate::game::quantity;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::{PlayerCounterKind, PlayerId};

/// CR 122.1: Give player counters of a named type.
/// Poison counters dispatch to the dedicated field (CR 104.3d SBA).
/// All other counter types use the generic player_counters map.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (counter_kind, count, target) = match &ability.effect {
        Effect::GivePlayerCounter {
            counter_kind,
            count,
            target,
        } => (counter_kind, count, target),
        _ => {
            return Err(EffectError::MissingParam(
                "expected GivePlayerCounter".into(),
            ))
        }
    };

    // CR 122.1: Resolve the quantity to a concrete count.
    let raw = quantity::resolve_quantity_with_targets(state, count, ability);
    let amount = raw.max(0) as u32;
    if amount == 0 {
        return Ok(());
    }

    // Resolve target player(s)
    let players = match target {
        TargetFilter::Controller | TargetFilter::SelfRef => vec![ability.controller],
        _ => {
            // Targeted: resolve from ability.targets
            let targeted: Vec<_> = ability
                .targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Player(pid) => Some(*pid),
                    _ => None,
                })
                .collect();
            if targeted.is_empty() {
                // No valid targets — do nothing (fizzle already handled by stack.rs)
                return Ok(());
            } else {
                targeted
            }
        }
    };

    for player_id in &players {
        let player = &mut state.players[player_id.0 as usize];
        player.add_player_counters(counter_kind, amount);

        // CR 122.1: Emit event for counter change.
        events.push(GameEvent::PlayerCounterChanged {
            player: *player_id,
            counter_kind: *counter_kind,
            delta: amount as i32,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GivePlayerCounter,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 122.1: Remove every counter of every kind from the resolving
/// target player(s). Covers "target opponent loses all counters" (Suncleanser)
/// and "each opponent loses all counters" (Final Act). Clears both the
/// dedicated `poison_counters` field (CR 104.3d routing, mirrored here) and
/// every entry in the generic `player_counters` map. One
/// `PlayerCounterChanged` event is emitted per cleared kind so animations and
/// logs see an atomic, itemized record of the removal.
pub fn resolve_lose_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::LoseAllPlayerCounters { target } => target,
        _ => {
            return Err(EffectError::MissingParam(
                "expected LoseAllPlayerCounters".into(),
            ))
        }
    };

    // Resolve target player(s). The `player_scope` iteration layer rebinds
    // `ability.controller` per matching player before this resolver runs, so
    // `TargetFilter::Controller`/`SelfRef` already addresses the iterating
    // player when the caller used "each opponent".
    let players: Vec<PlayerId> = match target {
        TargetFilter::Controller | TargetFilter::SelfRef => vec![ability.controller],
        _ => ability
            .targets
            .iter()
            .filter_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                _ => None,
            })
            .collect(),
    };

    for player_id in players {
        clear_all_player_counters(state, player_id, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::LoseAllPlayerCounters,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 122.1: Zero out every counter kind on a single player. Poison counters
/// live in their own field (CR 104.3d state-based action routing); every other
/// kind is tracked in the `player_counters` map. Both paths drain to zero and
/// emit a per-kind `PlayerCounterChanged { delta: -count }` event so replay
/// and UI can itemize what was removed.
fn clear_all_player_counters(
    state: &mut GameState,
    player_id: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let player = &mut state.players[player_id.0 as usize];

    if player.poison_counters > 0 {
        let delta = -(player.poison_counters as i32);
        player.poison_counters = 0;
        events.push(GameEvent::PlayerCounterChanged {
            player: player_id,
            counter_kind: PlayerCounterKind::Poison,
            delta,
        });
    }

    // Drain the generic map — collect kinds first to release the borrow before
    // mutating/emitting events.
    let drained: Vec<(PlayerCounterKind, u32)> = player
        .player_counters
        .drain()
        .filter(|(_, count)| *count > 0)
        .collect();
    for (counter_kind, count) in drained {
        events.push(GameEvent::PlayerCounterChanged {
            player: player_id,
            counter_kind,
            delta: -(count as i32),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, TargetFilter};
    use crate::types::identifiers::ObjectId;
    use crate::types::player::{PlayerCounterKind, PlayerId};

    fn make_ability(
        counter_kind: PlayerCounterKind,
        count: QuantityExpr,
        target: TargetFilter,
        controller: PlayerId,
    ) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::GivePlayerCounter {
                counter_kind,
                count,
                target,
            },
            controller,
            source_id: ObjectId(1),
            targets: vec![],
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            description: None,
            player_scope: None,
            chosen_x: None,
            cost_paid_object_mana_value: None,
            ability_index: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
        }
    }

    #[test]
    fn poison_counter_uses_dedicated_field() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(
            PlayerCounterKind::Poison,
            QuantityExpr::Fixed { value: 1 },
            TargetFilter::Controller,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].poison_counters, 1);
        // Should NOT be in the generic map
        assert_eq!(
            state.players[0]
                .player_counters
                .get(&PlayerCounterKind::Poison),
            None
        );
    }

    #[test]
    fn experience_counter_uses_generic_map() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(
            PlayerCounterKind::Experience,
            QuantityExpr::Fixed { value: 2 },
            TargetFilter::Controller,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].player_counter(&PlayerCounterKind::Experience),
            2
        );
    }

    #[test]
    fn counter_accumulates() {
        let mut state = GameState::default();
        let mut events = Vec::new();

        let ability = make_ability(
            PlayerCounterKind::Rad,
            QuantityExpr::Fixed { value: 3 },
            TargetFilter::Controller,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].player_counter(&PlayerCounterKind::Rad), 6);
    }

    #[test]
    fn targeted_player_counter() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let mut ability = make_ability(
            PlayerCounterKind::Poison,
            QuantityExpr::Fixed { value: 1 },
            TargetFilter::Any,
            PlayerId(0),
        );
        ability.targets = vec![TargetRef::Player(PlayerId(1))];

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].poison_counters, 0);
        assert_eq!(state.players[1].poison_counters, 1);
    }

    #[test]
    fn emits_counter_changed_event() {
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_ability(
            PlayerCounterKind::Ticket,
            QuantityExpr::Fixed { value: 1 },
            TargetFilter::Controller,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerCounterChanged {
                counter_kind,
                delta: 1,
                ..
            } if *counter_kind == PlayerCounterKind::Ticket
        )));
    }

    fn make_lose_all(target: TargetFilter, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility {
            effect: Effect::LoseAllPlayerCounters { target },
            controller,
            source_id: ObjectId(1),
            targets: vec![],
            kind: AbilityKind::Spell,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            description: None,
            player_scope: None,
            chosen_x: None,
            cost_paid_object_mana_value: None,
            ability_index: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
        }
    }

    #[test]
    fn lose_all_clears_poison_and_generic_counters() {
        // CR 122.1: Every counter kind — poison (dedicated field)
        // and generic (experience/rad/ticket) — must be zeroed in one pass.
        let mut state = GameState::default();
        let mut events = Vec::new();
        state.players[0].poison_counters = 3;
        state.players[0]
            .player_counters
            .insert(PlayerCounterKind::Experience, 4);
        state.players[0]
            .player_counters
            .insert(PlayerCounterKind::Rad, 2);

        let ability = make_lose_all(TargetFilter::Controller, PlayerId(0));
        resolve_lose_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].poison_counters, 0);
        assert!(state.players[0].player_counters.is_empty());
    }

    #[test]
    fn lose_all_emits_per_kind_events() {
        // CR 122.1: Each cleared kind produces a distinct PlayerCounterChanged
        // event so the animation layer can itemize the removal.
        let mut state = GameState::default();
        let mut events = Vec::new();
        state.players[1].poison_counters = 5;
        state.players[1]
            .player_counters
            .insert(PlayerCounterKind::Ticket, 1);

        let mut ability = make_lose_all(TargetFilter::Any, PlayerId(0));
        ability.targets = vec![TargetRef::Player(PlayerId(1))];
        resolve_lose_all(&mut state, &ability, &mut events).unwrap();

        let poison_event = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::PlayerCounterChanged {
                    player: PlayerId(1),
                    counter_kind: PlayerCounterKind::Poison,
                    delta: -5,
                }
            )
        });
        let ticket_event = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::PlayerCounterChanged {
                    player: PlayerId(1),
                    counter_kind: PlayerCounterKind::Ticket,
                    delta: -1,
                }
            )
        });
        assert!(poison_event, "expected poison -5 event");
        assert!(ticket_event, "expected ticket -1 event");
    }

    #[test]
    fn lose_all_is_noop_when_no_counters() {
        // CR 122.1: Absent counters produce no PlayerCounterChanged events.
        let mut state = GameState::default();
        let mut events = Vec::new();
        let ability = make_lose_all(TargetFilter::Controller, PlayerId(0));
        resolve_lose_all(&mut state, &ability, &mut events).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::PlayerCounterChanged { .. })));
    }
}
