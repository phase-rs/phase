use std::collections::HashSet;

use crate::game::{quantity, replacement};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterAddition, PendingEffectResolved};
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};

pub fn add_player_counter_with_replacement(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    counter_kind: PlayerCounterKind,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    if count == 0 {
        return true;
    }

    // CR 122.1 + CR 614.17: Player-counter additions pass through the
    // replacement pipeline so "players can't get counters" effects can prevent
    // the event before any player state is mutated.
    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Player {
            actor,
            player_id,
            counter_kind,
        },
        count,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        replacement::ReplacementResult::Execute(event) => {
            if let ProposedEvent::AddCounter {
                placement:
                    CounterPlacement::Player {
                        player_id,
                        counter_kind,
                        ..
                    },
                count,
                ..
            } = event
            {
                apply_player_counter_addition(state, player_id, counter_kind, count, events);
            }
            true
        }
        replacement::ReplacementResult::Prevented => true,
        replacement::ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            false
        }
    }
}

pub fn apply_player_counter_addition(
    state: &mut GameState,
    player_id: PlayerId,
    counter_kind: PlayerCounterKind,
    amount: u32,
    events: &mut Vec<GameEvent>,
) {
    if amount == 0 {
        return;
    }
    let player = &mut state.players[player_id.0 as usize];
    player.add_player_counters(&counter_kind, amount);

    // CR 122.1: Emit event for counter change.
    events.push(GameEvent::PlayerCounterChanged {
        player: player_id,
        counter_kind,
        delta: amount as i32,
    });
}

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

    // CR 115.1: Context-ref filters (Controller, TriggeringPlayer,
    // ParentTargetController, …) must NOT consult `ability.targets` — chain
    // target propagation would otherwise leak the parent's Player target into
    // a sub-ability with `target: Controller`. Mirror Draw / Mill / Discard.
    let players = if target.is_context_ref() {
        vec![super::resolve_player_for_context_ref(
            state, ability, target,
        )]
    } else {
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
        }
        targeted
    };

    let additions: Vec<_> = players
        .iter()
        .map(|player_id| PendingCounterAddition::Player {
            actor: ability.controller,
            player_id: *player_id,
            counter_kind: *counter_kind,
            count: amount,
        })
        .collect();
    let completion = PendingEffectResolved::new(EffectKind::GivePlayerCounter, ability.source_id);
    for (index, addition) in additions.iter().cloned().enumerate() {
        let PendingCounterAddition::Player {
            actor,
            player_id,
            counter_kind,
            count,
        } = addition
        else {
            continue;
        };
        if !add_player_counter_with_replacement(
            state,
            actor,
            player_id,
            counter_kind,
            count,
            events,
        ) {
            super::counters::stash_pending_counter_additions(
                state,
                additions[index + 1..].to_vec(),
                completion,
            );
            return Ok(());
        }
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

    // CR 115.1 + CR 122.1: The `player_scope` iteration layer rebinds
    // `ability.controller` per matching player before this resolver runs, so
    // context-ref filters (Controller / SelfRef / TriggeringPlayer / …) must
    // resolve via `resolve_player_for_context_ref` — never via
    // `ability.targets`, which would inherit a parent's chosen Player target
    // through chain propagation.
    let players: Vec<PlayerId> = if target.is_context_ref() {
        vec![super::resolve_player_for_context_ref(
            state, ability, target,
        )]
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
    use crate::game::game_object::GameObject;
    use crate::types::ability::{AbilityKind, QuantityExpr, SpellContext, TargetFilter};
    use crate::types::ability::{
        QuantityModification, ReplacementDefinition, ReplacementPlayerScope,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::{PlayerCounterKind, PlayerId};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

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
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            source_incarnation: None,
            source_card_id: None,
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
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            effect_context_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            dig_found_nothing_for_parent_target: false,
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
    fn player_counter_addition_is_prevented_by_global_replacement() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let solemnity_id = ObjectId(99);
        let mut solemnity = GameObject::new(
            solemnity_id,
            CardId(99),
            PlayerId(0),
            "Solemnity".to_string(),
            Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        replacement.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        solemnity.replacement_definitions = vec![replacement].into();
        state.objects.insert(solemnity_id, solemnity);
        state.battlefield.push_back(solemnity_id);

        let ability = make_ability(
            PlayerCounterKind::Poison,
            QuantityExpr::Fixed { value: 1 },
            TargetFilter::Controller,
            PlayerId(1),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].poison_counters, 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::PlayerCounterChanged { .. })),
            "prevented player-counter additions must not emit counter-change events"
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
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            source_incarnation: None,
            source_card_id: None,
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
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            effect_context_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            cant_be_copied: false,
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: crate::types::ability::SubAbilityLink::ContinuationStep,
            modal: None,
            mode_abilities: vec![],
            dig_found_nothing_for_parent_target: false,
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
