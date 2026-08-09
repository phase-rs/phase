use std::collections::HashSet;

use crate::game::{quantity, replacement};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterAddition, PendingEffectResolved};
use crate::types::player::{PlayerCounterKind, PlayerId};
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};
use crate::types::resolved_commands::ResolvedPlayerEdit;

/// The replacement-aware outcome of attempting to add player counters.
///
/// Distinguished from a plain `bool` because callers fall into two families
/// that need `Prevented` handled differently:
/// - Effect resolution (`resolve` below, `deal_damage`'s infect/toxic poison,
///   `proliferate`, the pending-counter-addition drain in `counters.rs`)
///   treats `Applied` and `Prevented` identically — the pending item is fully
///   resolved either way, whether or not any counters actually landed.
/// - Cost payment (`costs.rs`'s `GetPlayerCounters` ability-cost arm) must
///   treat `Prevented` as a FAILED payment: a "players can't get counters"
///   replacement (Solemnity) silently zeroing out a Ward's player-counter
///   cost must not be mistaken for having actually paid it, or Ward's whole
///   deterrent is bypassed for free (CR 702.21a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCounterAdditionOutcome {
    /// The counters were added (possibly replacement-adjusted in count).
    Applied,
    /// A replacement effect prevented the counter addition outright.
    Prevented,
    /// Replacement ordering or an optional replacement needs this player's
    /// choice; `state.waiting_for` has already been set.
    NeedsChoice,
}

pub fn add_player_counter_with_replacement(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    counter_kind: PlayerCounterKind,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> PlayerCounterAdditionOutcome {
    if count == 0 {
        return PlayerCounterAdditionOutcome::Applied;
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
            PlayerCounterAdditionOutcome::Applied
        }
        replacement::ReplacementResult::Prevented => PlayerCounterAdditionOutcome::Prevented,
        replacement::ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            PlayerCounterAdditionOutcome::NeedsChoice
        }
    }
}

/// The replacement-aware result of previewing a player-counter addition.
///
/// Mirrors `counters::CounterAdditionPreview` (the object-counter sibling),
/// parameterized for players instead of an object incarnation — players have
/// no incarnation-staleness concept, so there is no `None` "target no longer
/// matches" case here.
///
/// This is intentionally an engine-internal decision fact rather than wire
/// state: callers use it while evaluating a currently-bound action, so it
/// must not be serialized or retained across turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCounterAdditionPreview {
    /// The proposed count reaches the player unchanged.
    Applied { count: u32 },
    /// A replacement effect prevents the counter addition.
    Prevented,
    /// Replacement ordering or an optional replacement needs this player's choice.
    ChoiceRequired { player: PlayerId },
    /// Replacement effects change the proposed counter count (e.g. a doubler).
    Transformed { count: u32 },
    /// A replacement rewrites the counter event into a different event class.
    ///
    /// The preview cannot claim that the requested counter was added, so
    /// consumers must handle this explicitly rather than treating it as an
    /// absent preview.
    Unsupported,
}

/// Preview a player-counter addition through the real replacement pipeline,
/// without mutating live game state.
///
/// Runs on an isolated clone of `state`, so a tactical caller cannot add
/// pending choices, events, or counters to the live game. Used by
/// `phase-ai`'s Ward-lethality check (`can_pay_ward_cost`) to project the
/// REPLACEMENT-ADJUSTED poison total a payment would actually give — a
/// doubler or +N effect can make a printed count understate the real result,
/// and trusting the printed count alone can let the AI accept a payment that
/// is actually lethal (CR 104.3d).
///
/// CR 122.1 + CR 614.1: Counter placement is subject to applicable
/// replacement effects before the event happens.
pub fn preview_player_counter_addition(
    state: &GameState,
    actor: PlayerId,
    player_id: PlayerId,
    counter_kind: PlayerCounterKind,
    count: u32,
) -> PlayerCounterAdditionPreview {
    if count == 0 {
        return PlayerCounterAdditionPreview::Applied { count };
    }

    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Player {
            actor,
            player_id,
            counter_kind,
        },
        count,
        applied: HashSet::new(),
    };
    let mut preview_state = state.clone();
    let mut events = Vec::new();

    match replacement::replace_event(&mut preview_state, proposed, &mut events) {
        replacement::ReplacementResult::Execute(ProposedEvent::AddCounter {
            count: resulting_count,
            ..
        }) if resulting_count == count => PlayerCounterAdditionPreview::Applied {
            count: resulting_count,
        },
        replacement::ReplacementResult::Execute(ProposedEvent::AddCounter {
            count: resulting_count,
            ..
        }) => PlayerCounterAdditionPreview::Transformed {
            count: resulting_count,
        },
        // A replacement may redirect the event into a different event class.
        // The counter-placement fact is explicitly unsupported rather than
        // absent, so conservative callers cannot mistake it for "no counters".
        replacement::ReplacementResult::Execute(_) => PlayerCounterAdditionPreview::Unsupported,
        replacement::ReplacementResult::Prevented => PlayerCounterAdditionPreview::Prevented,
        replacement::ReplacementResult::NeedsChoice(player) => {
            PlayerCounterAdditionPreview::ChoiceRequired { player }
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
    state
        .resolve_and_apply_player_edit(
            player_id,
            ResolvedPlayerEdit::Counter {
                kind: counter_kind,
                delta: amount as i32,
            },
        )
        .expect("post-replacement player counter gain must target a live player");

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
        if add_player_counter_with_replacement(state, actor, player_id, counter_kind, count, events)
            == PlayerCounterAdditionOutcome::NeedsChoice
        {
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
        subject: None,
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
        subject: None,
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
    let poison = state
        .players
        .iter()
        .find(|player| player.id == player_id)
        .expect("counter target must be a live player")
        .poison_counters;
    if poison > 0 {
        let delta = -(poison as i32);
        state
            .resolve_and_apply_player_edit(
                player_id,
                ResolvedPlayerEdit::Counter {
                    kind: PlayerCounterKind::Poison,
                    delta,
                },
            )
            .expect("live player counter removal must apply");
        events.push(GameEvent::PlayerCounterChanged {
            player: player_id,
            counter_kind: PlayerCounterKind::Poison,
            delta,
        });
    }

    // Drain the generic map — collect kinds first to release the borrow before
    // mutating/emitting events.
    let drained: Vec<(PlayerCounterKind, u32)> = state
        .players
        .iter()
        .find(|player| player.id == player_id)
        .expect("counter target must be a live player")
        .player_counters
        .iter()
        .map(|(kind, count)| (*kind, *count))
        .filter(|(_, count)| *count > 0)
        .collect();
    for (counter_kind, count) in drained {
        state
            .resolve_and_apply_player_edit(
                player_id,
                ResolvedPlayerEdit::Counter {
                    kind: counter_kind,
                    delta: -(count as i32),
                },
            )
            .expect("live player counter removal must apply");
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
            trigger_source: None,
            trigger_definition_ref: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
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
            selected_mode_labels: Vec::new(),
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
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
            sibling_condition: crate::types::ability::SiblingCondition::Dependent,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
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
            trigger_source: None,
            trigger_definition_ref: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
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
            selected_mode_labels: Vec::new(),
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
            cost_paid_object_ids: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
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
            sibling_condition: crate::types::ability::SiblingCondition::Dependent,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
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
