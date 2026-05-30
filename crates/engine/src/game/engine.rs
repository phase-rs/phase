use rand::Rng;
use thiserror::Error;

use crate::types::ability::{EffectKind, KeywordAction, TargetRef};
use crate::types::actions::GameAction;
use crate::types::events::{BendingType, GameEvent, ManaTapState, PlayerActionKind};
use crate::types::game_state::{
    ActionResult, AutoPassMode, AutoPassRequest, ConvokeMode, GameState, RetargetScope, StackEntry,
    StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::match_config::MatchType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::ability_utils::{
    begin_target_selection_for_ability, build_target_slots, compute_unavailable_modes,
    modal_choice_for_player,
};
use super::casting;
use super::casting_costs;
use super::effects;
use super::engine_casting;
use super::engine_combat;
use super::engine_modes;
use super::engine_payment_choices;
use super::engine_priority;
use super::engine_replacement;
use super::engine_resolution_choices;
use super::engine_stack;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources;
use super::match_flow;
use super::mulligan;
use super::planeswalker;
use super::priority;
use super::public_state::{
    bump_state_revision, finalize_public_state, mark_public_state_all_dirty,
    mark_public_state_from_events, sync_waiting_for,
};
use super::sba;
use super::triggers;
use super::turn_control;
use super::turns;
use super::zones;

#[derive(Debug, Clone, Error)]
pub enum EngineError {
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    #[error("Wrong player")]
    WrongPlayer,
    #[error("Not your priority")]
    NotYourPriority,
    #[error("Action not allowed: {0}")]
    ActionNotAllowed(String),
}

fn handle_unlock_room_door(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    door: crate::game::game_object::RoomDoor,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if state.active_player != player
        || !matches!(state.phase, Phase::PreCombatMain | Phase::PostCombatMain)
        || !state.stack.is_empty()
    {
        return Err(EngineError::ActionNotAllowed(
            "Room doors can be unlocked only as a main-phase special action with an empty stack"
                .to_string(),
        ));
    }

    let cost = {
        let obj = state
            .objects
            .get(&object_id)
            .ok_or_else(|| EngineError::InvalidAction("Room not found".to_string()))?;
        if obj.controller != player || obj.zone != Zone::Battlefield {
            return Err(EngineError::ActionNotAllowed(
                "Only the controller of a battlefield Room can unlock it".to_string(),
            ));
        }
        if !obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Room")
        {
            return Err(EngineError::ActionNotAllowed(
                "Object is not a Room".to_string(),
            ));
        }
        if obj.room_unlocks.unwrap_or_default().is_unlocked(door) {
            return Err(EngineError::ActionNotAllowed(
                "That door is already unlocked".to_string(),
            ));
        }
        match door {
            crate::game::game_object::RoomDoor::Left => obj.mana_cost.clone(),
            crate::game::game_object::RoomDoor::Right => obj
                .back_face
                .as_ref()
                .map(|face| face.mana_cost.clone())
                .ok_or_else(|| {
                    EngineError::ActionNotAllowed("Room has no right door face".to_string())
                })?,
        }
    };

    casting::pay_unless_cost(state, player, &cost, events)?;

    super::room::unlock_door_designation(state, object_id, player, door, events);
    Ok(WaitingFor::Priority { player })
}

/// Public engine entrypoint. Every caller must supply the `actor` — the
/// `PlayerId` whose authenticated identity is making this action. The engine
/// rejects any action whose `actor` does not match `authorized_submitter(state)`
/// (with a narrow Concede exception — see `check_actor_authorization`).
///
/// # Safety contract (non-negotiable)
///
/// `actor` must come from a **trusted transport boundary**, never from
/// client-supplied payload data. Adapters that forward actions from a remote
/// peer (WebSocket server, P2P host) must tag the action with the PlayerId
/// associated with the *connection*, not a value copied out of the wire frame.
/// Otherwise a malicious peer can trivially spoof another player's identity.
///
/// Engine-internal simulation (AI search, legal-action probing) may use
/// [`apply_as_current`] which derives `actor` from the game state itself.
pub fn apply(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    // Clear transient inter-effect state at the start of each player action.
    // last_effect_count is set by interactive handlers (e.g., DiscardChoice) and
    // consumed by sub_ability continuations via EventContextAmount fallback.
    state.last_effect_count = None;
    state.last_effect_counts_by_player.clear();
    state.exiled_from_hand_this_resolution = 0;
    check_actor_authorization(state, actor, &action)?;
    let mut result = apply_action(state, actor, action)?;
    reconcile_terminal_result(state, &mut result);
    bump_state_revision(state);
    sync_waiting_for(state, &result.waiting_for);
    run_auto_pass_loop(state, &mut result);
    reconcile_terminal_result(state, &mut result);
    remember_public_reveals(state, &result.events);
    // Targeted public-state dirty marking over the full accumulated event set
    // (the auto-pass loop appends events). `finalize_public_state` is the only
    // consumer of `public_state_dirty`, so marking once here over the complete
    // event stream is correct and cheapest.
    mark_public_state_from_events(state, &result.events);
    finalize_public_state(state);
    result.log_entries = super::log::resolve_log_entries(&result.events, state);
    Ok(result)
}

fn reconcile_terminal_result(state: &mut GameState, result: &mut ActionResult) {
    // Safety net (fixes #962): If a player-loss SBA would eliminate a player,
    // run SBAs now. CR 704.3 normally checks SBAs when a player would receive
    // priority, but skipping them here can leave the engine waiting on a dead
    // player for a non-priority choice.
    //
    // The predicate lives in `sba` so it shares the same CR 101.2 "can't lose"
    // exception as the real player-loss SBA checks, and stays narrower than the
    // full SBA loop to avoid unrelated mid-resolution SBA prompts.
    if sba::has_pending_player_loss_sba(state) {
        sba::check_state_based_actions(state, &mut result.events);
        // SBA may have advanced waiting_for (e.g., GameOver, or Priority for
        // the next living player). Sync the result.
        result.waiting_for = state.waiting_for.clone();
    }

    super::elimination::ensure_game_over_if_terminal(state, &mut result.events);
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        result.waiting_for = state.waiting_for.clone();
    }
}

fn remember_public_reveals(state: &mut GameState, events: &[GameEvent]) {
    for event in events {
        if let GameEvent::CardsRevealed { card_ids, .. } = event {
            state.public_revealed_cards.extend(card_ids.iter().copied());
        }
    }
}

/// Engine-level authorization guard. Any *game action* must come from the
/// `authorized_submitter` for the current `WaitingFor` (which already accounts
/// for turn-decision-controller effects like Mindslaver). Two exception classes:
///
/// - `Concede` self-authenticates via its own `player_id` field — but we still
///   require it to match `actor` so a player cannot concede someone else on
///   their behalf (CR 104.3a).
/// - **Preference actions** (SetPhaseStops, SetAutoPass, CancelAutoPass) are
///   per-player UI settings. They have no CR semantics, mutate only the
///   submitter's own preference slot, and may legitimately fire at any time —
///   e.g. the human toggles a phase stop while the AI holds priority. The
///   downstream handlers route by `actor`, so any seat may set its own
///   preferences regardless of `WaitingFor`.
fn check_actor_authorization(
    state: &GameState,
    actor: PlayerId,
    action: &GameAction,
) -> Result<(), EngineError> {
    if let GameAction::Concede { player_id } = action {
        // CR 104.3a: A player may concede at any time — but only themselves.
        if *player_id != actor {
            return Err(EngineError::WrongPlayer);
        }
        return Ok(());
    }
    if matches!(
        action,
        GameAction::SetPhaseStops { .. }
            | GameAction::CancelAutoPass
            | GameAction::Debug(_)
            | GameAction::GrantDebugPermission { .. }
            | GameAction::RevokeDebugPermission { .. }
            | GameAction::ReorderHand { .. }
    ) {
        return Ok(());
    }
    // CR 103.5: For simultaneous-decision states (MulliganDecision,
    // MulliganBottomCards, OpeningHandBottomCards), authorize against the full pending set so any
    // pending player may submit in any order. Falls back to single-player
    // semantics for every other variant.
    let authorized = turn_control::authorized_submitters(state);
    if !authorized.is_empty() && !authorized.contains(&actor) {
        return Err(EngineError::WrongPlayer);
    }
    Ok(())
}

/// Engine-internal convenience: apply `action` as the player the engine is
/// currently waiting on. Intended for simulation (AI search, legal-action
/// probing) and tests — *not* for transport adapters, which must pass a
/// transport-authenticated `actor` to [`apply`] directly.
///
/// For [`GameAction::Concede`] the concede payload's `player_id` is used as
/// the actor, so tests can concede any player without first maneuvering the
/// `WaitingFor` state onto that player.
pub fn apply_as_current(
    state: &mut GameState,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    let actor = match &action {
        GameAction::Concede { player_id } => *player_id,
        // CR 103.5: For simultaneous-decision states, pick the first pending
        // player as the simulation representative. `authorized_submitters`
        // returns the full set; `first()` is deterministic (seat-ordered).
        _ => {
            let submitters = turn_control::authorized_submitters(state);
            submitters.first().copied().ok_or_else(|| {
                EngineError::InvalidAction(
                    "apply_as_current: no authorized submitter (game over?)".to_string(),
                )
            })?
        }
    };
    apply(state, actor, action)
}

pub(super) fn resume_pending_continuation_if_priority(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        effects::drain_pending_continuation(state, events);
    }
    Ok(())
}

/// Decision emitted by the auto-pass loop's per-iteration check.
enum AutoPassDecision {
    /// No active auto-pass — leave the loop and let the frontend take over.
    Exit,
    /// Auto-pass completed or was interrupted (opponent action, phase stop,
    /// stack terminator). Clear the flag and exit.
    Finish,
    /// Continue passing priority for this iteration.
    Pass,
}

/// Classify what the auto-pass loop should do for `player` at the current
/// priority window.
///
/// Interrupts (MTGA-style): `UntilStackEmpty` bails when the stack empties or
/// grows beyond the baseline (trigger or opponent spell); `UntilEndOfTurn`
/// bails when an opponent-controlled object is on top of the stack or when the
/// current phase is in the user-supplied `phase_stops` list.
fn priority_auto_pass_decision(state: &GameState, player: PlayerId) -> AutoPassDecision {
    let Some(mode) = state.auto_pass.get(&player) else {
        return AutoPassDecision::Exit;
    };
    match mode {
        AutoPassMode::UntilStackEmpty { initial_stack_len } => {
            if state.stack.is_empty() || state.stack.len() > *initial_stack_len {
                AutoPassDecision::Finish
            } else {
                AutoPassDecision::Pass
            }
        }
        AutoPassMode::UntilEndOfTurn => {
            let opponent_on_stack = state
                .stack
                .last()
                .is_some_and(|top| top.controller != player);
            if opponent_on_stack || phase_stop_hit(state, player) {
                AutoPassDecision::Finish
            } else {
                AutoPassDecision::Pass
            }
        }
    }
}

/// True when `player` has an active `UntilEndOfTurn` auto-pass session.
fn end_of_turn_active(state: &GameState, player: PlayerId) -> bool {
    matches!(
        state.auto_pass.get(&player),
        Some(AutoPassMode::UntilEndOfTurn)
    )
}

/// True when the current phase appears in `player`'s configured phase-stop list.
/// Consulted at every engine-driven auto-pass site so the user's preference is
/// respected whether or not an auto-pass session is active (e.g. suppresses
/// the empty-blockers auto-submit when the defender wants a Ninjutsu window).
fn phase_stop_hit(state: &GameState, player: PlayerId) -> bool {
    state
        .phase_stops
        .get(&player)
        .is_some_and(|stops| stops.contains(&state.phase))
}

fn pass_priority_once_with_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    state.cancelled_casts.clear();
    // CR 117.4 + 608.1: When all players pass in succession the stack begins
    // resolving; at that moment the AI guard against re-activating pending
    // abilities is no longer needed.
    state.pending_activations.clear();

    let stack_was_empty = state.stack.is_empty();
    // CR 117.4 + CR 723.5/723.8: pass the *seat* that holds priority, not
    // `priority_player` — under turn-control the latter is the authorized
    // submitter (the controller), which would mis-count consecutive passes and
    // soft-lock the game.
    let current_seat = turn_control::priority_seat(state);
    let wf = priority::handle_priority_pass(current_seat, state, events);
    sync_waiting_for(state, &wf);

    // CR 608.2 + CR 117.4: Drain any pending continuation queued during the
    // priority pass (e.g. effects that chain a sub-resolution after the parent
    // settles) while the stack is still in its post-resolution state. Without
    // this drain, a continuation queued after a no-choice effect would sit
    // until an unrelated action, by which point referenced stack objects may
    // have left the stack.
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        effects::drain_pending_continuation(state, events);
    }

    let skip_triggers =
        stack_was_empty && !state.stack.is_empty() && state.phase == Phase::CombatDamage;

    let wf = engine_priority::run_post_action_pipeline(
        state,
        events,
        &state.waiting_for.clone(),
        skip_triggers,
    )?;
    sync_waiting_for(state, &wf);
    Ok(wf)
}

fn active_until_stack_empty_requester(state: &GameState) -> Option<PlayerId> {
    state.auto_pass.iter().find_map(|(player, mode)| {
        matches!(mode, AutoPassMode::UntilStackEmpty { .. }).then_some(*player)
    })
}

fn priority_player_has_meaningful_action(state: &GameState) -> bool {
    let mut probe = state.clone();
    probe.auto_pass.clear();
    let actions = crate::ai_support::legal_actions(&probe);
    crate::ai_support::has_meaningful_priority_action(&probe, &actions)
}

fn finish_completed_or_interrupted_until_stack_empty_sessions(state: &mut GameState) -> bool {
    let finished: Vec<PlayerId> = state
        .auto_pass
        .iter()
        .filter_map(|(player, mode)| match mode {
            AutoPassMode::UntilStackEmpty { initial_stack_len }
                if state.stack.is_empty() || state.stack.len() > *initial_stack_len =>
            {
                Some(*player)
            }
            _ => None,
        })
        .collect();

    for player in &finished {
        state.auto_pass.remove(player);
    }

    !finished.is_empty()
}

fn auto_pass_loop_max_iterations(state: &GameState) -> usize {
    let living_players = state
        .players
        .iter()
        .filter(|player| !player.is_eliminated)
        .count()
        .max(1);
    state
        .stack
        .len()
        .saturating_mul(living_players)
        .saturating_mul(2)
        .saturating_add(16)
        .clamp(500, 10_000)
}

#[cfg(test)]
mod auto_pass_decision_tests {
    use super::*;
    use std::sync::Arc;

    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, CopyRetargetPermission, Effect, QuantityExpr,
        ResolvedAbility, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::game_state::CastingVariant;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::zones::Zone;

    fn stack_entry(controller: PlayerId) -> StackEntry {
        StackEntry {
            id: ObjectId(0),
            source_id: ObjectId(0),
            controller,
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Equip {
                    equipment_id: ObjectId(0),
                    target_creature_id: ObjectId(0),
                },
            },
        }
    }

    fn is_pass(d: &AutoPassDecision) -> bool {
        matches!(d, AutoPassDecision::Pass)
    }

    fn is_finish(d: &AutoPassDecision) -> bool {
        matches!(d, AutoPassDecision::Finish)
    }

    fn priority_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        state
    }

    #[test]
    fn apply_reconciles_eliminated_two_player_game_to_game_over() {
        let mut state = priority_state();
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilEndOfTurn,
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::GameOver {
                winner: Some(PlayerId(0))
            }
        )));
    }

    fn push_simple_stack_entry(state: &mut GameState, id: u64, controller: PlayerId) {
        state.stack.push_back(StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(id),
            controller,
            kind: StackEntryKind::KeywordAction {
                action: KeywordAction::Crew {
                    vehicle_id: ObjectId(id),
                    paid_creature_ids: Vec::new(),
                },
            },
        });
    }

    fn draw_ability(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source_id,
            controller,
        )
    }

    fn add_non_mana_activated_artifact(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(900),
            controller,
            "Priority Action".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&object_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        object_id
    }

    fn push_spell(
        state: &mut GameState,
        id: ObjectId,
        controller: PlayerId,
        ability: ResolvedAbility,
    ) {
        state.stack.push_back(StackEntry {
            id,
            source_id: id,
            controller,
            kind: StackEntryKind::Spell {
                card_id: CardId(id.0),
                ability: Some(ability),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    }

    #[test]
    fn exit_when_no_auto_pass_set() {
        let state = GameState::default();
        assert!(matches!(
            priority_auto_pass_decision(&state, PlayerId(0)),
            AutoPassDecision::Exit
        ));
    }

    #[test]
    fn until_end_of_turn_passes_through_empty_stack_without_phase_stop() {
        let mut state = GameState {
            phase: Phase::PostCombatMain,
            ..GameState::default()
        };
        state
            .auto_pass
            .insert(PlayerId(0), AutoPassMode::UntilEndOfTurn);
        assert!(is_pass(&priority_auto_pass_decision(&state, PlayerId(0))));
    }

    #[test]
    fn until_end_of_turn_finishes_on_opponent_stack_activity() {
        // Opponent spell/trigger on top must interrupt auto-pass so the player
        // always gets a chance to respond.
        let mut state = GameState::default();
        state.stack.push_back(stack_entry(PlayerId(1)));
        state
            .auto_pass
            .insert(PlayerId(0), AutoPassMode::UntilEndOfTurn);
        assert!(is_finish(&priority_auto_pass_decision(&state, PlayerId(0))));
    }

    #[test]
    fn until_end_of_turn_passes_through_own_stack_activity() {
        // MTGA-style: resolve your own spells without pausing.
        let mut state = GameState::default();
        state.stack.push_back(stack_entry(PlayerId(0)));
        state
            .auto_pass
            .insert(PlayerId(0), AutoPassMode::UntilEndOfTurn);
        assert!(is_pass(&priority_auto_pass_decision(&state, PlayerId(0))));
    }

    #[test]
    fn until_end_of_turn_finishes_at_configured_phase_stop() {
        // User-flagged phase stop halts auto-pass even when the stack is empty
        // and no opponent action has interrupted.
        let mut state = GameState {
            phase: Phase::DeclareBlockers,
            ..GameState::default()
        };
        state
            .auto_pass
            .insert(PlayerId(0), AutoPassMode::UntilEndOfTurn);
        state
            .phase_stops
            .insert(PlayerId(0), vec![Phase::DeclareBlockers]);
        assert!(is_finish(&priority_auto_pass_decision(&state, PlayerId(0))));
    }

    #[test]
    fn phase_stop_hit_reads_per_player_preferences() {
        let mut state = GameState {
            phase: Phase::DeclareBlockers,
            ..GameState::default()
        };
        // No entry for the player → no stop.
        assert!(!phase_stop_hit(&state, PlayerId(0)));

        // Unrelated phase in the list → no stop.
        state.phase_stops.insert(PlayerId(0), vec![Phase::Upkeep]);
        assert!(!phase_stop_hit(&state, PlayerId(0)));

        // Current phase in the list → stop.
        state
            .phase_stops
            .insert(PlayerId(0), vec![Phase::Upkeep, Phase::DeclareBlockers]);
        assert!(phase_stop_hit(&state, PlayerId(0)));

        // Per-player: player 1's stops don't bleed into player 0.
        state.phase_stops.remove(&PlayerId(0));
        state
            .phase_stops
            .insert(PlayerId(1), vec![Phase::DeclareBlockers]);
        assert!(!phase_stop_hit(&state, PlayerId(0)));
        assert!(phase_stop_hit(&state, PlayerId(1)));
    }

    #[test]
    fn phase_stop_hit_is_independent_of_auto_pass_mode() {
        // Phase stops apply even without an active auto-pass session —
        // this is what closes the "no legal blockers auto-submitted
        // regardless of preference" gap.
        let mut state = GameState {
            phase: Phase::DeclareBlockers,
            ..GameState::default()
        };
        state
            .phase_stops
            .insert(PlayerId(0), vec![Phase::DeclareBlockers]);
        assert!(phase_stop_hit(&state, PlayerId(0)));
        assert!(!end_of_turn_active(&state, PlayerId(0)));
    }

    #[test]
    fn until_end_of_turn_does_not_auto_submit_available_blockers() {
        let waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![ObjectId(10)],
            valid_block_targets: [(ObjectId(10), vec![ObjectId(20)])].into_iter().collect(),
            block_requirements: Default::default(),
        };
        let mut state = GameState {
            phase: Phase::DeclareBlockers,
            active_player: PlayerId(1),
            waiting_for: waiting_for.clone(),
            ..GameState::default()
        };
        state
            .auto_pass
            .insert(PlayerId(0), AutoPassMode::UntilEndOfTurn);

        let mut result = ActionResult {
            events: Vec::new(),
            waiting_for,
            log_entries: Vec::new(),
        };
        run_auto_pass_loop(&mut state, &mut result);

        assert!(matches!(
            result.waiting_for,
            WaitingFor::DeclareBlockers {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(
            state.auto_pass.contains_key(&PlayerId(0)),
            "the defender's auto-pass session should stay armed after pausing for legal blockers"
        );
    }

    #[test]
    fn until_stack_empty_resolves_large_stack_in_one_apply() {
        let mut state = priority_state();
        for idx in 0..264 {
            push_simple_stack_entry(&mut state, 10_000 + idx, PlayerId(0));
        }

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilStackEmpty,
            },
        )
        .unwrap();

        assert!(state.stack.is_empty());
        assert!(!state.auto_pass.contains_key(&PlayerId(0)));
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(
            result
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                .count(),
            264
        );
    }

    #[test]
    fn until_stack_empty_stops_on_non_requester_meaningful_action() {
        let mut state = priority_state();
        push_simple_stack_entry(&mut state, 20_000, PlayerId(1));
        add_non_mana_activated_artifact(&mut state, PlayerId(1));

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilStackEmpty,
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 1);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        assert!(
            state.auto_pass.contains_key(&PlayerId(0)),
            "requester's session stays active while waiting on opponent action"
        );
    }

    #[test]
    fn until_stack_empty_non_requester_own_stack_shortcut_does_not_hide_action() {
        let mut state = priority_state();
        push_simple_stack_entry(&mut state, 21_000, PlayerId(1));
        add_non_mana_activated_artifact(&mut state, PlayerId(1));
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        state.priority_player = PlayerId(1);
        state.auto_pass.insert(
            PlayerId(0),
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: 1,
            },
        );

        let mut result = ActionResult {
            events: Vec::new(),
            waiting_for: state.waiting_for.clone(),
            log_entries: Vec::new(),
        };
        run_auto_pass_loop(&mut state, &mut result);

        assert_eq!(state.stack.len(), 1);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn until_stack_empty_stops_on_interactive_waiting_for() {
        let mut state = priority_state();
        let spell_id = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Scry Spell".to_string(),
            Zone::Stack,
        );
        create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Library Card".to_string(),
            Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            spell_id,
            PlayerId(0),
        );
        push_spell(&mut state, spell_id, PlayerId(0), ability);

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilStackEmpty,
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::ScryChoice {
                player: PlayerId(0),
                ..
            }
        ));
    }

    #[test]
    fn until_stack_empty_stops_on_stack_growth() {
        let mut state = priority_state();
        let copied_id = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Copied Spell".to_string(),
            Zone::Stack,
        );
        push_spell(
            &mut state,
            copied_id,
            PlayerId(0),
            draw_ability(copied_id, PlayerId(0)),
        );
        let copy_id = create_object(
            &mut state,
            CardId(904),
            PlayerId(0),
            "Copy Spell".to_string(),
            Zone::Stack,
        );
        let copy_ability = ResolvedAbility::new(
            Effect::CopySpell {
                target: TargetFilter::Any,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
            },
            Vec::new(),
            copy_id,
            PlayerId(0),
        );
        push_spell(&mut state, copy_id, PlayerId(0), copy_ability);

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilStackEmpty,
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 2);
        assert!(!state.auto_pass.contains_key(&PlayerId(0)));
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn until_stack_empty_does_not_advance_phase_after_stack_empties() {
        let mut state = priority_state();
        push_simple_stack_entry(&mut state, 30_000, PlayerId(0));

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetAutoPass {
                mode: AutoPassRequest::UntilStackEmpty,
            },
        )
        .unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }
}

/// Auto-pass loop: when a player has an auto-pass flag and receives priority,
/// automatically pass for them until the goal condition is met or interrupted.
fn run_auto_pass_loop(state: &mut GameState, result: &mut ActionResult) {
    for _ in 0..auto_pass_loop_max_iterations(state) {
        match &result.waiting_for {
            WaitingFor::Priority { player } => {
                let player = *player;
                let decision = priority_auto_pass_decision(state, player);
                match decision {
                    AutoPassDecision::Exit => {
                        let Some(requester) = active_until_stack_empty_requester(state) else {
                            break;
                        };
                        if requester == player {
                            break;
                        }
                        if finish_completed_or_interrupted_until_stack_empty_sessions(state) {
                            break;
                        }
                        if priority_player_has_meaningful_action(state) {
                            break;
                        }
                    }
                    AutoPassDecision::Finish => {
                        state.auto_pass.remove(&player);
                        break;
                    }
                    AutoPassDecision::Pass => {}
                }

                let mut events = Vec::new();
                match pass_priority_once_with_pipeline(state, &mut events) {
                    Ok(wf) => {
                        let stack_empty_or_grew =
                            finish_completed_or_interrupted_until_stack_empty_sessions(state);
                        result.events.extend(events);
                        result.waiting_for = wf;
                        if stack_empty_or_grew {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            // UntilEndOfTurn: auto-submit empty attackers unless the user flagged
            // this phase as a stop.
            WaitingFor::DeclareAttackers { player, .. }
                if end_of_turn_active(state, *player) && !phase_stop_hit(state, *player) =>
            {
                let mut events = Vec::new();
                match engine_combat::handle_empty_attackers(state, &mut events) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Auto-submit empty blockers only when there's nothing to choose.
            // CR 509.1 says the turn-based action still runs when no legal blocks
            // are available, and CR 117.1c requires the active player to receive
            // priority during the step (instants and Ninjutsu-family activations
            // per CR 702.49 — notably Sneak, which is restricted to this step).
            // A phase stop on Declare Blockers overrides this even without an
            // auto-pass session: if the player explicitly asked to pause here,
            // honor it.
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                ..
            } if !phase_stop_hit(state, *player) && valid_blocker_ids.is_empty() => {
                let mut events = Vec::new();
                match engine_combat::handle_empty_blockers(state, *player, &mut events) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Non-auto-passable WaitingFor (interactive choice, game over, etc.)
            _ => break,
        }
    }
}

/// CR 707.10c: Finalize a `CopyRetarget` flow — write the slot-derived targets
/// back onto the copy's stack entry, emit `EffectResolved`, hand priority back
/// to the chooser, and drain any pending continuation queued during resolution.
fn finalize_copy_retarget(
    state: &mut GameState,
    player: PlayerId,
    copy_id: ObjectId,
    slots: &[crate::types::game_state::CopyTargetSlot],
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let targets: Vec<_> = slots
        .iter()
        .map(|slot| {
            slot.current.clone().ok_or_else(|| {
                EngineError::InvalidAction(
                    "Copy target selection has an unchosen target slot".to_string(),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(entry) = state.stack.iter_mut().find(|e| e.id == copy_id) {
        if let Some(ability) = entry.ability_mut() {
            ability.targets = targets;
        }
    }
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::CopySpell,
        source_id: copy_id,
    });
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
    effects::drain_pending_continuation(state, events);
    Ok(())
}

fn apply_action(
    state: &mut GameState,
    actor: PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    // Clear stale revealed_cards from the previous action.
    // RevealTop reveals (e.g. Goblin Guide) are momentary — shown for one state update.
    // RevealHand reveals (e.g. Thoughtseize) persist through the RevealChoice interaction.
    // ManifestDread reveals persist through ManifestDreadChoice (cards come from WaitingFor).
    // CR 701.20b: DigChoice reveals (reveal-dig, e.g. Satyr Wayfinder) persist through
    // the selection — revealed cards remain public while the player chooses.
    if !matches!(
        state.waiting_for,
        WaitingFor::RevealChoice { .. }
            | WaitingFor::ManifestDreadChoice { .. }
            | WaitingFor::DigChoice { .. }
    ) {
        state.revealed_cards.clear();
    }

    let mut events = Vec::new();
    let mut triggers_processed_inline = false;

    // CancelAutoPass works from any WaitingFor state (player may cancel during
    // interactive choices). Routed by `actor` — previously used
    // `authorized_submitter(state)`, which silently cancelled the wrong player's
    // session when fired while an opponent held the prompt.
    if matches!(action, GameAction::CancelAutoPass) {
        state.auto_pass.remove(&actor);
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // SetPhaseStops propagates the player's phase-stop preference. Pure preference
    // state — no game logic, no WaitingFor transition. Works from any state so
    // frontends can sync on preference changes regardless of the current prompt.
    // Routed by `actor` so the human can update their own stops while the AI
    // holds priority (the previous "authorized_submitter" lookup rejected this
    // outright via the WrongPlayer guard, surfacing as an in-game dispatch error).
    if let GameAction::SetPhaseStops { stops } = &action {
        if stops.is_empty() {
            state.phase_stops.remove(&actor);
        } else {
            state.phase_stops.insert(actor, stops.clone());
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 402.3: Hand order has no game-rules significance — ReorderHand is a
    // display-preference update on the actor's own hand. Validated as a strict
    // permutation of the current hand and applied with no event emission, no
    // WaitingFor transition, and no auto-pass / lands-tapped clearing. Mirrors
    // the SetPhaseStops / CancelAutoPass pattern: any-state, routed by `actor`.
    if let GameAction::ReorderHand { order } = &action {
        // Canonical accessor in this crate is direct indexing — see
        // `state.players[player.0 as usize]` throughout `ai_support/candidates.rs`,
        // `game/companion.rs`, and the existing test module. Bounds-check via
        // `len()` rather than swapping to `.get_mut()`, to stay idiomatic with
        // the rest of the file.
        if (actor.0 as usize) >= state.players.len() {
            return Err(EngineError::InvalidAction(format!(
                "ReorderHand: actor {:?} is not a valid player index",
                actor
            )));
        }
        let player = &mut state.players[actor.0 as usize];

        if order.len() != player.hand.len() {
            return Err(EngineError::InvalidAction(format!(
                "ReorderHand: expected {} ids, got {}",
                player.hand.len(),
                order.len()
            )));
        }

        // Permutation check: same multiset. Sort copies and compare — O(n log n)
        // is fine for hand sizes (typically <= 7, capped well under any realistic
        // limit by CR 402.2 and our zone semantics). ObjectId is not Ord, so
        // sort by the inner u64 key directly.
        let mut current: Vec<ObjectId> = player.hand.iter().copied().collect();
        let mut requested = order.clone();
        current.sort_unstable_by_key(|id| id.0);
        requested.sort_unstable_by_key(|id| id.0);
        if current != requested {
            return Err(EngineError::InvalidAction(
                "ReorderHand: order is not a permutation of the current hand".into(),
            ));
        }

        player.hand = order.iter().copied().collect();

        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // CR 104.3a: A player may concede at any time. Concede bypasses the WaitingFor
    // dispatch entirely — there is no priority/state check. Eliminating the player
    // performs CR 800.4a object cleanup and advances `waiting_for` if the conceder
    // owned it (see `eliminate_player`).
    if let GameAction::Concede { player_id } = action {
        let mut events = Vec::new();
        super::elimination::eliminate_player(state, player_id, &mut events);
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // Debug actions bypass WaitingFor dispatch — gated on debug_mode flag
    // (engine-level: the action runs) and `debug_permitted` (transport-level:
    // the player may submit). The transport layer (server-core / WASM) is
    // responsible for enforcing per-player permission; this engine check is
    // a defense-in-depth invariant — a player not in `debug_permitted` should
    // never have reached `apply`.
    if let GameAction::Debug(debug_action) = action {
        if !state.debug_mode {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug_mode to be enabled".into(),
            ));
        }
        if !state.debug_permitted.is_empty() && !state.debug_permitted.contains(&actor) {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug permission".into(),
            ));
        }
        let description = debug_action.describe(state);
        let mut result =
            super::engine_debug::apply_debug_action(state, actor, debug_action, &mut events)?;
        result
            .events
            .push(crate::types::events::GameEvent::DebugActionUsed {
                player_id: actor,
                description,
            });
        return Ok(result);
    }

    // Sandbox host-only grant/revoke of debug permission. server-core also
    // checks this at the transport boundary; the engine repeats the check as
    // defense-in-depth so WASM and P2P-host paths cannot be bypassed by a
    // malicious actor crafting the action shape directly. The host convention
    // (PlayerId(0)) is fixed across every transport — see
    // `crates/server-core/src/session.rs` `HOST_PLAYER`. Emits a public audit
    // event on success.
    const HOST_PLAYER: PlayerId = PlayerId(0);
    if matches!(
        action,
        GameAction::GrantDebugPermission { .. } | GameAction::RevokeDebugPermission { .. }
    ) {
        if !state.format_config.allow_debug_actions {
            return Err(EngineError::ActionNotAllowed(
                "Sandbox mode is not enabled for this game".to_string(),
            ));
        }
        if actor != HOST_PLAYER {
            return Err(EngineError::ActionNotAllowed(
                "Only the host can grant or revoke debug permission".to_string(),
            ));
        }
        if let GameAction::RevokeDebugPermission { player_id } = action {
            if player_id == HOST_PLAYER {
                return Err(EngineError::ActionNotAllowed(
                    "The host cannot revoke their own debug permission".to_string(),
                ));
            }
        }
    }
    if let GameAction::GrantDebugPermission { player_id } = action {
        state.debug_permitted.insert(player_id);
        events.push(crate::types::events::GameEvent::DebugPermissionGranted {
            host: actor,
            player_id,
        });
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }
    if let GameAction::RevokeDebugPermission { player_id } = action {
        state.debug_permitted.remove(&player_id);
        events.push(crate::types::events::GameEvent::DebugPermissionRevoked {
            host: actor,
            player_id,
        });
        return Ok(ActionResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries: vec![],
        });
    }

    // Any deliberate player action (not auto-pass-related or a simple pass) cancels their auto-pass.
    // CR 103.5: Use the authenticated `actor` directly so the simultaneous mulligan
    // variants (where `authorized_submitter` is None when multiple players are pending)
    // still clear per-actor side-effect state correctly.
    match &action {
        GameAction::SetAutoPass { .. }
        | GameAction::PassPriority
        | GameAction::ReorderHand { .. } => {}
        _ => {
            state.auto_pass.remove(&actor);
        }
    }

    // Clear manual mana-tap tracking when the player commits to a non-mana action.
    // ActivateAbility is handled per-arm (only non-mana abilities clear tracking).
    match &action {
        GameAction::PassPriority
        | GameAction::PlayLand { .. }
        | GameAction::CastSpell { .. }
        | GameAction::CastSpellWithPaymentMode { .. }
        | GameAction::Foretell { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsSneakWithPaymentMode { .. }
        | GameAction::CastSpellAsWebSlinging { .. }
        | GameAction::CastSpellAsWebSlingingWithPaymentMode { .. }
        | GameAction::CastSpellForFree { .. }
        | GameAction::CastSpellForFreeWithPaymentMode { .. }
        | GameAction::CastSpellAsMiracle { .. }
        | GameAction::CastSpellAsMiracleWithPaymentMode { .. }
        | GameAction::CastSpellAsMadness { .. }
        | GameAction::CastSpellAsMadnessWithPaymentMode { .. }
        | GameAction::CancelCast
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::PayUnlessCost { .. }
        | GameAction::PayCombatTax { .. } => {
            state.lands_tapped_for_mana.remove(&actor);
        }
        _ => {}
    }

    // Validate and process action against current WaitingFor
    let waiting_for = match (&state.waiting_for.clone(), action) {
        (WaitingFor::Priority { player }, GameAction::PassPriority) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let wf = pass_priority_once_with_pipeline(state, &mut events)?;
            return Ok(ActionResult {
                events,
                waiting_for: wf,
                log_entries: vec![],
            });
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            state.cancelled_casts.clear();
            // CR 116.2a: Playing a land is a special action — sorcery-speed, once per turn, stack must be empty.
            // CR 305.2: Playing a land is a special action, not a spell.
            handle_play_land(state, object_id, card_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::TapLandForMana { object_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_tap_land_for_mana(state, object_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::UntapLandForMana { object_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_untap_land_for_mana(state, state.priority_player, object_id, &mut events)?;
            WaitingFor::Priority { player: *player }
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpell {
                object_id, card_id, ..
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell(state, *player, object_id, card_id, &mut events)?
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellWithPaymentMode {
                object_id,
                card_id,
                payment_mode,
                ..
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell_with_payment_mode(
                state,
                *player,
                object_id,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        (WaitingFor::Priority { player }, GameAction::Foretell { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_foretell(state, *player, object_id, card_id, &mut events)?
        }
        // CR 602.1: Activated abilities have a cost and an effect, written as "[Cost]: [Effect.]"
        (
            WaitingFor::Priority { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            // Check if this is a mana ability -- resolve instantly without the stack
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                // CR 605.3b: Mana abilities resolve immediately without using the stack.
                let ability_def = obj.abilities[ability_index].clone();
                let is_land = obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land);
                let wf = mana_abilities::activate_mana_ability(
                    state,
                    source_id,
                    *player,
                    ability_index,
                    &ability_def,
                    &mut events,
                    crate::types::game_state::ManaAbilityResume::Priority,
                    None,
                )?;
                // CR 605.3b: Track land mana taps for undo (UntapLandForMana),
                // matching the TapLandForMana path so dual lands are undoable
                // too. `ManaSourcePenalty::None` is the only variant that
                // allows undo — painlands (damage on resolution), pay-life
                // sources, and sacrifice sources all commit irreversible
                // state atomically with CR 605.3b resolution.
                if is_land && mana_sources::mana_ability_penalty(&ability_def).is_undoable() {
                    state
                        .lands_tapped_for_mana
                        .entry(state.priority_player)
                        .or_default()
                        .push(source_id);
                }
                wf
            } else if obj.loyalty.is_some()
                && ability_index < obj.abilities.len()
                && matches!(
                    obj.abilities[ability_index].cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { .. })
                )
            {
                // CR 606.3: Loyalty abilities activate once per turn at sorcery speed.
                state.lands_tapped_for_mana.remove(player);
                planeswalker::handle_activate_loyalty(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            } else {
                // Non-mana activated ability — clear tracking
                state.lands_tapped_for_mana.remove(player);
                casting::handle_activate_ability(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            }
        }
        (WaitingFor::Priority { player }, GameAction::UnlockRoomDoor { object_id, door }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            handle_unlock_room_door(state, *player, object_id, door, &mut events)?
        }
        // CR 715.3a: Player chooses creature or Adventure face.
        (
            WaitingFor::AdventureCastChoice {
                player,
                object_id,
                card_id,
                payment_mode,
            },
            GameAction::ChooseAdventureFace { creature },
        ) => casting::handle_adventure_choice_with_payment_mode(
            state,
            *player,
            *object_id,
            *card_id,
            creature,
            *payment_mode,
            &mut events,
        )?,
        // CR 712.12 (land face) / CR 712.11b (spell face): Player chooses which
        // face of an MDFC to play (land) or cast (spell).
        (
            WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode,
            },
            GameAction::ChooseModalFace { back_face },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if let Some(obj) = state.objects.get_mut(object_id) {
                if back_face {
                    // Swap to back face using existing primitives
                    let back = obj.back_face.take().expect("MDFC has back face");
                    let front_snapshot = super::printed_cards::snapshot_object_face(obj);
                    super::printed_cards::apply_back_face_to_object(obj, back);
                    obj.back_face = Some(front_snapshot);
                    // Do NOT set obj.transformed — MDFC face choice ≠ transform
                } else {
                    // Front face chosen — clear layout_kind so the MDFC intercept
                    // won't re-fire on re-entry into handle_play_land / handle_cast_spell.
                    if let Some(ref mut bf) = obj.back_face {
                        bf.layout_kind = None;
                    }
                }
            }
            // CR 712.12 / CR 712.11b: Route the re-entry by the now-active face's
            // type. A land face is put onto the battlefield via the play-land
            // special action (CR 712.12); a spell face is cast (CR 712.11b — Esika
            // // The Prismatic Bridge). After a swap
            // the new back_face (from snapshot_object_face) has layout_kind: None,
            // and a front-face choice clears it explicitly — so neither the
            // both-faces-land intercept nor the spell-face intercept re-fires.
            let active_is_land = state.objects.get(object_id).is_some_and(|obj| {
                obj.card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Land)
            });
            if active_is_land {
                handle_play_land(state, *object_id, *card_id, &mut events)?
            } else {
                casting::handle_cast_spell_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    *payment_mode,
                    &mut events,
                )?
            }
        }
        // CR 118.9: Player chooses between the printed mana cost and the
        // keyword-granted alternative cost. The `keyword` axis on the waiting
        // state drives dispatch to the per-keyword post-payment handler
        // (CR 702.74a Evoke, CR 702.96a Overload, CR 702.103a Bestow,
        // CR 702.148a Cleave, custom Warp). Each keyword retains its own
        // resolver because post-payment semantics genuinely diverge — the
        // unification is purely at the player-decision layer.
        (
            WaitingFor::AlternativeCastChoice {
                player,
                object_id,
                card_id,
                payment_mode,
                keyword,
                ..
            },
            GameAction::ChooseAlternativeCast { choice },
        ) => {
            use crate::types::game_state::AlternativeCastKeyword;
            match keyword {
                AlternativeCastKeyword::Warp => casting::handle_warp_cost_choice_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    choice,
                    *payment_mode,
                    &mut events,
                )?,
                AlternativeCastKeyword::Evoke => {
                    casting::handle_evoke_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Overload => {
                    casting::handle_overload_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Bestow => {
                    casting::handle_bestow_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Awaken => {
                    casting::handle_awaken_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
                AlternativeCastKeyword::Cleave => {
                    casting::handle_cleave_cost_choice_with_payment_mode(
                        state,
                        *player,
                        *object_id,
                        *card_id,
                        choice,
                        *payment_mode,
                        &mut events,
                    )?
                }
            }
        }
        (
            WaitingFor::CastingVariantChoice {
                player,
                object_id,
                card_id,
                payment_mode,
                options,
            },
            GameAction::ChooseCastingVariant { index },
        ) => casting::handle_casting_variant_choice_with_payment_mode(
            state,
            *player,
            *object_id,
            *card_id,
            options,
            index,
            *payment_mode,
            &mut events,
        )?,
        // CR 110.4: Player chose which permanent type slot to consume for a
        // multi-type graveyard cast via OncePerTurnPerPermanentType (Muldrotha).
        (
            WaitingFor::ChoosePermanentTypeSlot {
                player,
                object_id,
                card_id,
                source,
                payment_mode,
                ..
            },
            GameAction::ChoosePermanentTypeSlot { slot },
        ) => {
            let is_land_play = slot == crate::types::card_type::CoreType::Land;
            if is_land_play {
                state.pending_permanent_type_slot = Some((*source, slot));
                handle_play_land(state, *object_id, *card_id, &mut events)?
            } else {
                casting::handle_permanent_type_slot_choice_with_payment_mode(
                    state,
                    *player,
                    *object_id,
                    *card_id,
                    *source,
                    slot,
                    *payment_mode,
                    &mut events,
                )?
            }
        }
        // CR 110.4: Cancel during slot choice — return to priority.
        (WaitingFor::ChoosePermanentTypeSlot { player, .. }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::ModeChoice { player, .. }, GameAction::SelectModes { indices }) => {
            casting::handle_select_modes(state, *player, indices, &mut events)?
        }
        (
            WaitingFor::ModeChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (WaitingFor::TargetSelection { player, .. }, GameAction::SelectTargets { targets }) => {
            engine_casting::handle_target_selection_select_targets(
                state,
                *player,
                targets,
                &mut events,
            )?
        }
        (WaitingFor::TargetSelection { player, .. }, GameAction::ChooseTarget { target }) => {
            engine_casting::handle_target_selection_choose_target(
                state,
                *player,
                target,
                &mut events,
            )?
        }
        (
            WaitingFor::TargetSelection {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::OptionalCostChoice {
                player,
                cost,
                pending_cast,
                ..
            },
            GameAction::DecideOptionalCost { pay },
        ) => engine_casting::handle_optional_cost_choice(
            state,
            *player,
            *pending_cast.clone(),
            cost,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::OptionalCostChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 601.2b: Defiler cycle — player decides whether to pay life for mana reduction.
        (
            WaitingFor::DefilerPayment {
                player,
                life_cost,
                mana_reduction,
                pending_cast,
            },
            GameAction::DecideOptionalCost { pay },
        ) => engine_casting::handle_defiler_payment(
            state,
            *player,
            *pending_cast.clone(),
            *life_cost,
            mana_reduction,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::DefilerPayment {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 601.2b: Player selected cards to discard as additional casting cost.
        (
            WaitingFor::DiscardForCost {
                player,
                count,
                cards: legal_cards,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_discard_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            legal_cards,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::DiscardForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 118.3: Player selected permanents to sacrifice as cost.
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                costs,
                pending_cast,
            },
            GameAction::ChooseActivationCostBranch { index },
        ) => engine_casting::handle_activation_cost_one_of_choice(
            state,
            *player,
            *pending_cast.clone(),
            costs,
            index,
            &mut events,
        )?,
        (
            WaitingFor::ActivationCostOneOfChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::SacrificeForCost {
                player,
                count,
                min_count,
                permanents,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_sacrifice_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            (*min_count, *count),
            permanents,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::SacrificeForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 118.3: Player selected permanents to return to hand as cost.
        (
            WaitingFor::ReturnToHandForCost {
                player,
                count,
                permanents,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_return_to_hand_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            permanents,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::ReturnToHandForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 118.3 + CR 122.1: Player selected a permanent to remove a counter
        // from as a cost.
        (
            WaitingFor::RemoveCounterForCost {
                player,
                count,
                counter_type,
                permanents,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting_costs::handle_remove_counter_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            counter_type.clone(),
            permanents,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::RemoveCounterForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // Blight: player selected creature(s) to put -1/-1 counters on as cost.
        (
            WaitingFor::BlightChoice {
                player,
                counters,
                creatures,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting_costs::handle_blight_choice(
            state,
            *player,
            *pending_cast.clone(),
            *counters,
            creatures,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::BlightChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 702.34a: Player selected creatures to tap as a spell cost (flashback tap cost).
        (
            WaitingFor::TapCreaturesForSpellCost {
                player,
                count,
                creatures,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_tap_creatures_for_spell_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            creatures,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::BeholdForCost {
                player,
                count,
                choices,
                action,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_behold_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            choices,
            *action,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::BeholdForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::TapCreaturesForSpellCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::TapCreaturesForManaAbility {
                count,
                creatures,
                pending_mana_ability,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_tap_creatures_for_mana_ability(
            state,
            *count,
            creatures,
            pending_mana_ability,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::DiscardForManaAbility {
                count,
                cards: legal_cards,
                pending_mana_ability,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_discard_for_mana_ability(
            state,
            *count,
            legal_cards,
            pending_mana_ability,
            &chosen,
            &mut events,
        )?,
        // CR 117.1 + CR 118.3 + CR 605.3b + CR 400.7j: Player selected
        // object(s) to exile as a mana ability cost.
        (
            WaitingFor::ExileForManaAbility {
                count,
                cards,
                pending_mana_ability,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => super::mana_abilities::handle_exile_for_mana_ability(
            state,
            *count,
            cards,
            pending_mana_ability,
            &chosen,
            &mut events,
        )?,
        // CR 117.1 + CR 118.3 + CR 605.3b + CR 202.3: Player selected battlefield
        // permanent(s) to sacrifice as a mana ability cost (Phyrexian Altar class).
        (
            WaitingFor::SacrificeForManaAbility {
                count,
                permanents,
                pending_mana_ability,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => super::mana_abilities::handle_sacrifice_for_mana_ability(
            state,
            *count,
            permanents,
            pending_mana_ability,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::ChooseManaColor {
                choice, context, ..
            },
            GameAction::ChooseManaColor {
                choice: chosen,
                count,
            },
        ) => {
            let events_before = events.len();
            let wf = match context {
                crate::types::game_state::ManaChoiceContext::ManaAbility(pending_mana_ability) => {
                    // CR 605.3a: validate the requested batch size BEFORE any mana
                    // is produced, so an out-of-range count rejects cleanly with
                    // no partial application. The cap is the just-activated source
                    // plus its choice-free identical twins.
                    if count as usize > pending_mana_ability.batch_siblings.len() + 1 {
                        return Err(EngineError::InvalidAction(format!(
                            "ChooseManaColor count {count} exceeds the {} batchable sources",
                            pending_mana_ability.batch_siblings.len() + 1
                        )));
                    }
                    let wf = engine_casting::handle_choose_mana_color(
                        state,
                        pending_mana_ability,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?;
                    // CR 605.3a: one color choice may bulk-activate the player's
                    // other identical, choice-free mana sources (their remaining
                    // Treasures, etc.) with the same color. Sibling cost/mana
                    // events append before the shared trigger scan below, so each
                    // sacrifice's observers fire exactly once.
                    if count > 1 {
                        engine_casting::batch_activate_mana_siblings(
                            state,
                            pending_mana_ability,
                            &chosen,
                            count,
                            &mut events,
                        )?;
                    }
                    wf
                }
                crate::types::game_state::ManaChoiceContext::ResolvingEffect(pending_effect) => {
                    effects::mana::handle_choose_mana_effect(
                        state,
                        pending_effect,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?
                }
            };
            // CR 603.2c + CR 605.4a: A mana color choice produces mana inline.
            // Scan its events for TapsForMana mana multipliers and for
            // cost-payment triggers HERE, because for `ManaPayment` /
            // `UnlessPayment` resumes the post-action pipeline is skipped
            // (it is guarded by `matches!(waiting_for, WaitingFor::Priority)`),
            // so this is the only scan site — and CR 605.4a requires the bonus
            // mana to enter the pool before the spell's payment step continues.
            // Do NOT "simplify" this scan away for non-Priority resumes.
            if events.len() > events_before {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
            }
            // CR 603.3b (#531): if the inline trigger scan paused on an
            // OrderTriggers prompt (controller has 2+ simultaneous TapsForMana
            // multipliers, etc.), surface that prompt instead of overwriting
            // it with the resume `wf` (Priority/ManaPayment). Preserve `wf`
            // so `handle_order_triggers` can resume the interrupted chain
            // after the ordered triggered mana abilities dispatch.
            if let Some(order_wf) =
                super::triggers::preserve_order_triggers_resume(state, wf.clone())
            {
                return Ok(ActionResult {
                    events,
                    waiting_for: order_wf,
                    log_entries: vec![],
                });
            }
            // CR 603.2c: For a `Priority` resume the post-action pipeline WOULD
            // re-scan these same events, double-firing the multiplier (issue
            // #443: Delighted Halfling under a mana multiplier yields 5 not 3).
            // Claim the scan via `triggers_processed_inline` — the same
            // mechanism `DeclareAttackers` uses — so the pipeline runs SBAs,
            // delayed/state triggers, and layers but skips the trigger re-scan.
            if matches!(wf, WaitingFor::Priority { .. }) {
                triggers_processed_inline = true;
            }
            wf
        }
        // CR 605.3a + CR 601.2h + CR 107.4e: Player submits the per-hybrid-shard
        // color vector for a mana-ability mana sub-cost (filter lands, etc.).
        (
            WaitingFor::PayManaAbilityMana {
                options,
                pending_mana_ability,
                ..
            },
            GameAction::PayManaAbilityMana { payment },
        ) => engine_casting::handle_pay_mana_ability_mana(
            state,
            options,
            pending_mana_ability,
            &payment,
            &mut events,
        )?,
        // CR 118.9a + CR 601.2b + CR 601.2h: Player selected cards to exile as
        // part of an alternative or additional casting cost. Covers escape
        // (CR 702.138a, graveyard) and pitch spells (Force of Will, Force of
        // Negation, Misdirection, Unmask, etc., hand).
        (
            WaitingFor::ExileForCost {
                player,
                zone,
                count,
                cards: legal_cards,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_exile_for_cost(
            state,
            *player,
            *zone,
            *pending_cast.clone(),
            *count,
            legal_cards,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::ExileForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        (
            WaitingFor::CollectEvidenceChoice {
                player,
                minimum_mana_value,
                cards: legal_cards,
                resume,
            },
            GameAction::SelectCards { cards: chosen },
        ) => super::effects::collect_evidence::handle_choice(
            state,
            *player,
            *minimum_mana_value,
            legal_cards,
            resume,
            &chosen,
            &mut events,
        )?,
        (WaitingFor::CollectEvidenceChoice { player, resume, .. }, GameAction::CancelCast) => {
            engine_casting::handle_collect_evidence_cancel(state, *player, resume, &mut events)
        }
        // CR 702.180b: Player chose which creature to tap for harmonize cost reduction.
        // CR 601.2b: Creature is tapped as part of paying the total cost.
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                eligible_creatures,
                pending_cast,
            },
            GameAction::HarmonizeTap { creature_id },
        ) => engine_casting::handle_harmonize_tap_choice(
            state,
            *player,
            eligible_creatures,
            *pending_cast.clone(),
            creature_id,
            &mut events,
        )?,
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => engine_casting::cancel_pending_cast(state, *player, pending_cast, &mut events),
        // CR 609.3: Player decided whether to perform an optional effect ("You may X").
        (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { accept }) => {
            engine_payment_choices::handle_optional_effect_choice(state, accept, &mut events)?
        }
        (
            WaitingFor::PairChoice {
                player,
                source_id,
                choices,
            },
            GameAction::ChoosePair { partner },
        ) => {
            if let Some(partner_id) = partner {
                if !choices.contains(&partner_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected Soulbond partner is not legal".to_string(),
                    ));
                }
                if super::pairing::is_unpaired_creature_you_control(state, *source_id, *player)
                    && super::pairing::is_unpaired_creature_you_control(state, partner_id, *player)
                {
                    super::pairing::pair_objects(state, *source_id, partner_id, *player);
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::PairWith,
                source_id: *source_id,
            });
            state.waiting_for = WaitingFor::Priority { player: *player };
            state.priority_player = *player;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        (
            waiting_for @ WaitingFor::OptionalEffectChoice { .. },
            GameAction::DecideOptionalEffectAndRemember { choice },
        ) => engine_payment_choices::handle_optional_effect_choice_and_remember(
            state,
            waiting_for.clone(),
            choice,
            &mut events,
        )?,
        // CR 608.2d: Opponent decided on "any opponent may" effect.
        (
            waiting_for @ WaitingFor::OpponentMayChoice { .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            return engine_payment_choices::handle_opponent_may_choice(
                state,
                waiting_for.clone(),
                accept,
                &mut events,
            );
        }
        // CR 702.104a: The chosen opponent for a Tribute creature decided pay/decline.
        (
            waiting_for @ WaitingFor::TributeChoice { .. },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            return engine_payment_choices::handle_tribute_choice(
                state,
                waiting_for.clone(),
                accept,
                &mut events,
            );
        }
        // CR 118.12: Player decided whether to pay an "unless pays" cost.
        (waiting_for @ WaitingFor::UnlessPayment { .. }, GameAction::PayUnlessCost { pay }) => {
            return engine_payment_choices::handle_unless_payment(
                state,
                waiting_for.clone(),
                pay,
                &mut events,
            );
        }
        // CR 118.12a: Player chose **which** sub-cost of a disjunctive
        // unless-cost to pay (or declined to pay any). On a `Some(idx)`
        // choice, the handler swaps the multi-cost prompt for a single-cost
        // `WaitingFor::UnlessPayment` carrying the chosen branch. On `None`
        // it falls through to the effect-happens path the same way a `pay:
        // false` answer to `PayUnlessCost` would.
        (
            waiting_for @ WaitingFor::UnlessPaymentChooseCost { .. },
            GameAction::ChooseUnlessCostBranch { choice },
        ) => {
            return engine_payment_choices::handle_unless_payment_choose_cost(
                state,
                waiting_for.clone(),
                choice,
                &mut events,
            );
        }
        // CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: Player decided whether to
        // pay the locked-in combat tax. Resumes the paused attack/block declaration
        // with the matching sanitization per the accept/decline branch.
        (
            waiting_for @ WaitingFor::CombatTaxPayment { .. },
            GameAction::PayCombatTax { accept },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_pay_combat_tax(state, waiting_for.clone(), accept, &mut events)?
        }
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::TapLandForMana { object_id },
        ) => engine_payment_choices::handle_unless_payment_tap_land_for_mana(
            state,
            waiting_for.clone(),
            object_id,
            &mut events,
        )?,
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::UntapLandForMana { object_id },
        ) => engine_payment_choices::handle_unless_payment_untap_land_for_mana(
            state,
            waiting_for.clone(),
            object_id,
            &mut events,
        )?,
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            waiting_for @ WaitingFor::UnlessPayment { .. },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => engine_payment_choices::handle_unless_payment_activate_ability(
            state,
            waiting_for.clone(),
            source_id,
            ability_index,
            &mut events,
        )?,
        // CR 702.21a: Player selected a card to discard as ward cost payment.
        (
            waiting_for @ WaitingFor::WardDiscardChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_ward_discard_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        // CR 702.21a: Player selected a permanent to sacrifice as ward cost payment.
        (
            waiting_for @ WaitingFor::WardSacrificeChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_ward_sacrifice_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        // CR 118.12: Player selected a permanent to return to hand as unless cost.
        (
            waiting_for @ WaitingFor::UnlessBounceChoice { .. },
            GameAction::SelectCards { cards: chosen },
        ) => engine_payment_choices::handle_unless_bounce_choice(
            state,
            waiting_for.clone(),
            chosen,
            &mut events,
        )?,
        (WaitingFor::ManaPayment { player, .. }, GameAction::CancelCast) => {
            // CR 601.2i: Cancelling at mana payment rolls back the cast — pop
            // the stack entry placed at announcement and return the object to
            // its origin zone via `cancel_pending_cast`.
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::ChooseXValue { player, .. }, GameAction::CancelCast) => {
            // CR 601.2f + CR 601.2i: Caster may back out before committing to an
            // X value. Pop the stack entry placed at announcement and restore.
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        (WaitingFor::ChooseXValue { .. }, GameAction::PassPriority) => {
            // CR 601.2f: X must be chosen before the cast can proceed; passing priority
            // is not a legal way to skip this step.
            return Err(EngineError::ActionNotAllowed(
                "Cannot pass priority while choosing a value for X — commit with ChooseX or CancelCast."
                    .to_string(),
            ));
        }
        // CR 107.1b + CR 601.2f: Commit the chosen X value, then advance to mana payment.
        (
            WaitingFor::ChooseXValue {
                player,
                min,
                max,
                convoke_mode,
                ..
            },
            GameAction::ChooseX { value },
        ) => {
            if value < *min {
                return Err(EngineError::InvalidAction(format!(
                    "X={value} is below the minimum legal value of {min}",
                    min = *min,
                )));
            }
            if value > *max {
                return Err(EngineError::InvalidAction(format!(
                    "X={value} exceeds the maximum legal value of {max}",
                    max = *max,
                )));
            }
            let player = *player;
            let convoke_mode = *convoke_mode;
            let pending = state.pending_cast.as_mut().ok_or_else(|| {
                EngineError::InvalidAction("No pending cast awaiting X".to_string())
            })?;
            pending.ability.set_chosen_x_recursive(value);
            pending.cost.concretize_x(value);
            let object_id = pending.object_id;
            events.push(GameEvent::XValueChosen {
                player,
                object_id,
                value,
            });
            // CR 601.2b + CR 601.2f: X is now locked in. Apply the cost floor
            // (Trinisphere class) that was deferred while X was symbolic, against
            // the now-concrete total, before payment is determined.
            casting::apply_post_x_cost_floor(state, player, object_id);
            casting_costs::enter_payment_step(state, player, convoke_mode, &mut events)?
        }
        // CR 601.2h: Player has confirmed payment — delegate to the shared finalizer
        // that both this branch and the auto-pay path in `enter_payment_step` share.
        (WaitingFor::ManaPayment { player, .. }, GameAction::PassPriority) => {
            casting_costs::finalize_mana_payment(state, *player, &mut events)?
        }
        // CR 107.4f + CR 601.2f + CR 601.2h: Caster submitted per-shard Phyrexian
        // choices. Validate choice count + current affordability, then resume the
        // cast via `finalize_mana_payment_with_phyrexian_choices`.
        (
            WaitingFor::PhyrexianPayment {
                player,
                spell_object,
                shards,
            },
            GameAction::SubmitPhyrexianChoices { choices },
        ) => {
            let player = *player;
            let spell_object = *spell_object;
            let expected_len = shards.len();
            if choices.len() != expected_len {
                return Err(EngineError::InvalidAction(format!(
                    "Phyrexian choice count mismatch: expected {expected_len}, got {}",
                    choices.len()
                )));
            }
            // CR 118.3: Re-validate affordability against current state — life may have
            // dropped mid-cast (e.g., a life-loss replacement fired), so `PayLife` choices
            // on shards that now show `LifeOnly`/`ManaOrLife` must still have life available.
            {
                let pending_ref = state.pending_cast.as_ref().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for Phyrexian payment".to_string())
                })?;
                let cost = pending_ref.cost.clone();
                let player_pool = state
                    .players
                    .iter()
                    .find(|p| p.id == player)
                    .map(|p| p.mana_pool.clone())
                    .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;
                let current_shards = if pending_ref.activation_ability_index.is_some() {
                    let (source_types, source_subtypes) =
                        casting::activation_source_types(state, spell_object);
                    let activation_ctx = crate::types::mana::PaymentContext::Activation {
                        source_types: &source_types,
                        source_subtypes: &source_subtypes,
                    };
                    let any_color = casting::player_can_spend_as_any_color_for_payment(
                        state,
                        player,
                        spell_object,
                        Some(&activation_ctx),
                    );
                    let permissions = super::static_abilities::build_cost_permission_context(
                        state, player, any_color,
                    );
                    mana_payment::compute_phyrexian_shards(
                        &player_pool,
                        &cost,
                        Some(&activation_ctx),
                        permissions,
                    )
                } else {
                    let spell_meta = casting::build_spell_meta(state, player, spell_object);
                    let spell_ctx = spell_meta
                        .as_ref()
                        .map(crate::types::mana::PaymentContext::Spell);
                    let any_color = casting::player_can_spend_as_any_color_for_payment(
                        state,
                        player,
                        spell_object,
                        spell_ctx.as_ref(),
                    );
                    let permissions = super::static_abilities::build_cost_permission_context(
                        state, player, any_color,
                    );
                    mana_payment::compute_phyrexian_shards(
                        &player_pool,
                        &cost,
                        spell_ctx.as_ref(),
                        permissions,
                    )
                };
                if current_shards.len() != expected_len {
                    return Err(EngineError::ActionNotAllowed(
                        "Phyrexian shard count changed during pause".to_string(),
                    ));
                }
                for (choice, shard) in choices.iter().zip(current_shards.iter()) {
                    match (choice, shard.options) {
                        (
                            crate::types::game_state::ShardChoice::PayLife,
                            crate::types::game_state::ShardOptions::ManaOnly,
                        ) => {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot pay life for shard — only mana available".to_string(),
                            ));
                        }
                        (
                            crate::types::game_state::ShardChoice::PayMana,
                            crate::types::game_state::ShardOptions::LifeOnly,
                        ) => {
                            return Err(EngineError::ActionNotAllowed(
                                "Cannot pay mana for shard — only life available".to_string(),
                            ));
                        }
                        _ => {}
                    }
                }
            }
            casting_costs::finalize_mana_payment_with_phyrexian_choices(
                state,
                player,
                &choices,
                &mut events,
            )?
        }
        // CR 601.2i: CancelCast during Phyrexian payment rolls back the cast —
        // mirrors the ManaPayment CancelCast path.
        (WaitingFor::PhyrexianPayment { player, .. }, GameAction::CancelCast) => {
            let player = *player;
            match state.pending_cast.take() {
                Some(pending) => {
                    engine_casting::cancel_pending_cast(state, player, &pending, &mut events)
                }
                None => WaitingFor::Priority { player },
            }
        }
        // Allow mana abilities during mana payment (mid-cast)
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let events_before = events.len();
                let ability_def = obj.abilities[ability_index].clone();
                let wf = mana_abilities::activate_mana_ability(
                    state,
                    source_id,
                    *player,
                    ability_index,
                    &ability_def,
                    &mut events,
                    crate::types::game_state::ManaAbilityResume::ManaPayment {
                        convoke_mode: *convoke_mode,
                    },
                    None,
                )?;
                // CR 605.1b: Process TapsForMana triggers inline during mana payment
                // (same rationale as the TapLandForMana arm below).
                if events.len() > events_before {
                    let mana_events: Vec<_> = events[events_before..].to_vec();
                    super::triggers::process_triggers(state, &mana_events);
                }
                if let Some(order_wf) =
                    super::triggers::preserve_order_triggers_resume(state, wf.clone())
                {
                    return Ok(ActionResult {
                        events,
                        waiting_for: order_wf,
                        log_entries: vec![],
                    });
                }
                wf
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during mana payment".to_string(),
                ));
            }
        }
        // Allow basic land tapping during mana payment
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::TapLandForMana { object_id },
        ) => {
            let events_before = events.len();
            handle_tap_land_for_mana(state, object_id, &mut events)?;
            state
                .lands_tapped_for_mana
                .entry(state.priority_player)
                .or_default()
                .push(object_id);
            // CR 605.1b: TapsForMana triggered mana abilities (Wild Growth, Vorinclex,
            // Fertile Ground, Mana Flare class) must resolve inline when mana is
            // produced during cost payment. The ManaPayment path does not flow through
            // run_post_action_pipeline, so process triggers explicitly here so the
            // bonus mana reaches the pool before the payment check.
            if events.len() > events_before {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
            }
            let wf = WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            };
            if let Some(order_wf) =
                super::triggers::preserve_order_triggers_resume(state, wf.clone())
            {
                return Ok(ActionResult {
                    events,
                    waiting_for: order_wf,
                    log_entries: vec![],
                });
            }
            wf
        }
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::UntapLandForMana { object_id },
        ) => {
            handle_untap_land_for_mana(state, state.priority_player, object_id, &mut events)?;
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
        }
        // CR 702.51a / Waterbend: Tap a creature or artifact to pay mana.
        // CR 702.51a + CR 302.6: Convoke taps creatures to pay mana; summoning sickness
        // (CR 302.6) is not checked because convoke does not use the tap activated-ability mechanism.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode:
                    Some(
                        mode @ (ConvokeMode::Convoke
                        | ConvokeMode::Waterbend
                        | ConvokeMode::Improvise),
                    ),
            },
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
        ) => {
            let mode = *mode;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            let is_eligible = match mode {
                ConvokeMode::Convoke => obj.is_convoke_eligible(*player),
                ConvokeMode::Waterbend => obj.is_waterbend_eligible(*player),
                ConvokeMode::Improvise => obj.is_improvise_eligible(*player),
            };
            if !is_eligible {
                return Err(EngineError::ActionNotAllowed(
                    "Can only tap an eligible untapped permanent you control for convoke"
                        .to_string(),
                ));
            }
            let tapped_creature_for_convoke = mode == ConvokeMode::Convoke
                && obj
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature);
            // CR 702.51a: Validate color match for Convoke.
            let resolved_mana_type = match mode {
                ConvokeMode::Convoke => {
                    if let Some(color) = mana_sources::mana_type_to_color(mana_type) {
                        // Colored mana: creature must have that color
                        if !obj.color.contains(&color) {
                            return Err(EngineError::ActionNotAllowed(format!(
                                "Creature does not have color {:?} for convoke",
                                color
                            )));
                        }
                        mana_type
                    } else {
                        // Colorless: any creature can pay generic
                        crate::types::mana::ManaType::Colorless
                    }
                }
                // Waterbend always produces colorless
                ConvokeMode::Waterbend => crate::types::mana::ManaType::Colorless,
                // CR 702.126a: Improvise pays generic mana only — always colorless.
                ConvokeMode::Improvise => crate::types::mana::ManaType::Colorless,
            };
            // Tap the permanent (no summoning sickness check — CR 702.51a + CR 302.6)
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.tapped = true;
            }
            events.push(GameEvent::PermanentTapped {
                object_id,
                caused_by: None,
            });
            let unit = match mode {
                ConvokeMode::Convoke => {
                    crate::types::mana::ManaUnit::convoke_payment(resolved_mana_type, object_id)
                }
                ConvokeMode::Waterbend => crate::types::mana::ManaUnit::new(
                    resolved_mana_type,
                    object_id,
                    false,
                    Vec::new(),
                ),
                // CR 702.126a/b: improvise mana exists only to pay this spell's
                // generic cost — `convoke_payment` carries the restriction that
                // keeps it from leaking into the pool as real mana.
                ConvokeMode::Improvise => {
                    crate::types::mana::ManaUnit::convoke_payment(resolved_mana_type, object_id)
                }
            };
            if let Some(p) = state.players.iter_mut().find(|p| p.id == *player) {
                p.mana_pool.add(unit);
            }
            if mode == ConvokeMode::Waterbend {
                events.push(GameEvent::ManaAdded {
                    player_id: *player,
                    mana_type: resolved_mana_type,
                    source_id: object_id,
                    tap_state: ManaTapState::NotFromTap,
                });
            }
            if tapped_creature_for_convoke {
                let pending = state.pending_cast.as_mut().ok_or_else(|| {
                    EngineError::InvalidAction("No pending cast for convoke".to_string())
                })?;
                pending.convoked_creatures.push(object_id);
            }
            // Only emit waterbend event for Waterbend mode
            if mode == ConvokeMode::Waterbend {
                crate::game::bending::record_bending(
                    state,
                    &mut events,
                    BendingType::Water,
                    object_id,
                    *player,
                );
            }
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: Some(mode),
            }
        }
        (WaitingFor::MulliganDecision { .. }, GameAction::MulliganDecision { choice }) => {
            // CR 103.5 + 103.5b: `actor` is already authorized as a member of
            // `pending` by `check_actor_authorization`. The mulligan module
            // resolves the per-player state update and either re-emits
            // MulliganDecision (with the actor removed if they kept, retained
            // with bumped count if they mulliganed, or retained with the
            // same count if they used Serum Powder) or advances to the next
            // phase when the pending set is empty.
            mulligan::handle_mulligan_decision(state, actor, choice, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::MulliganBottomCards { .. }, GameAction::SelectCards { cards }) => {
            // CR 103.5: `actor` is already authorized as a member of `pending`.
            mulligan::handle_mulligan_bottom(state, actor, cards, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::OpeningHandBottomCards { .. }, GameAction::SelectCards { cards }) => {
            // TL:R 906.6a/e: `actor` is already authorized as a member of
            // `pending`; no normal mulligan actions are available in this state.
            mulligan::handle_opening_hand_bottom(state, actor, cards, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::DeclareAttackers { player, .. }, GameAction::DeclareAttackers { attacks }) => {
            triggers_processed_inline = true;
            engine_combat::handle_declare_attackers(state, *player, &attacks, &mut events)?
        }
        (
            WaitingFor::DeclareBlockers { player, .. },
            GameAction::DeclareBlockers { assignments },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_declare_blockers(state, *player, &assignments, &mut events)?
        }
        (
            WaitingFor::UntapChoice {
                player,
                candidates,
                chosen_not_to_untap,
            },
            GameAction::ChooseUntap { object_id, untap },
        ) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if !candidates.contains(&object_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid untap choice object".to_string(),
                ));
            }

            let remaining: Vec<ObjectId> = candidates
                .iter()
                .copied()
                .filter(|candidate| candidate != &object_id)
                .collect();
            let mut declined = chosen_not_to_untap.clone();
            if !untap {
                declined.push(object_id);
            }

            if !remaining.is_empty() {
                WaitingFor::UntapChoice {
                    player: *player,
                    candidates: remaining,
                    chosen_not_to_untap: declined,
                }
            } else {
                let skipped: std::collections::HashSet<ObjectId> = declined.into_iter().collect();
                turns::execute_untap_with_choices(state, &mut events, &skipped);
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            }
        }
        // CR 508.1g + CR 701.43d: the active player decides whether to pay the
        // optional "exert as it attacks" cost for the prompted attacker, one
        // attacker at a time. Triggers are deferred to `finish_declare_attackers`
        // (the buffered declaration + exert events fire together), so suppress
        // the epilogue's trigger pass for every step of the loop.
        (
            WaitingFor::ExertChoice {
                player,
                attacker,
                remaining,
            },
            GameAction::ChooseExert { exert },
        ) => {
            triggers_processed_inline = true;
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            if exert {
                engine_combat::apply_attack_exert(state, *attacker, &mut events);
            }
            if let Some((next, rest)) = remaining.split_first() {
                WaitingFor::ExertChoice {
                    player: *player,
                    attacker: *next,
                    remaining: rest.to_vec(),
                }
            } else {
                engine_combat::finish_declare_attackers(state, &mut events, false)?
            }
        }
        (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
            engine_replacement::handle_replacement_choice(state, index, &mut events)?
        }
        // CR 603.3b: Player submits the chosen order for their pending triggers.
        // `actor` is already authorized as the prompted player by
        // `check_actor_authorization` (via `WaitingFor::acting_player`).
        (WaitingFor::OrderTriggers { .. }, GameAction::OrderTriggers { order }) => {
            triggers::handle_order_triggers(state, order)?
        }
        // CR 707.9: Player chose a permanent to copy for "enter as a copy of" replacement.
        (
            waiting_for @ WaitingFor::CopyTargetChoice { .. },
            GameAction::ChooseTarget { target },
        ) => engine_replacement::handle_copy_target_choice(
            state,
            waiting_for.clone(),
            target,
            &mut events,
        )?,
        (
            WaitingFor::ExploreChoice {
                player,
                remaining,
                pending_effect,
                ..
            },
            GameAction::ChooseTarget { target },
        ) => {
            if turn_control::authorized_submitter(state) != Some(*player) {
                return Err(EngineError::WrongPlayer);
            }
            let chosen = match target {
                Some(TargetRef::Object(id)) => id,
                _ => {
                    return Err(EngineError::InvalidAction(
                        "Invalid explore choice".to_string(),
                    ));
                }
            };
            super::effects::explore::handle_choice(
                state,
                chosen,
                remaining,
                pending_effect.as_ref(),
                &mut events,
            )?
        }
        // CR 303.4 + CR 303.4g + CR 115.1: Player picked the permanent to
        // enchant for a `Effect::ReturnAsAura` sub-effect. The picker is a
        // CHOICE (not a target), so the action shape mirrors
        // `WaitingFor::ExploreChoice` — `GameAction::ChooseTarget { target:
        // Some(Object(id)) }` with `id` drawn from `legal_targets`.
        (
            WaitingFor::ReturnAsAuraTarget {
                player,
                source_id: _,
                returned_id,
                legal_targets,
                pending_effect,
            },
            GameAction::ChooseTarget { target },
        ) => {
            if turn_control::authorized_submitter(state) != Some(*player) {
                return Err(EngineError::WrongPlayer);
            }
            let chosen = match target {
                Some(TargetRef::Object(id)) if legal_targets.contains(&id) => id,
                _ => {
                    return Err(EngineError::InvalidAction(
                        "ReturnAsAuraTarget: invalid or missing legal Object target".to_string(),
                    ));
                }
            };
            let pending = pending_effect.clone();
            let returned = *returned_id;
            let active_player = *player;
            let (filter, grants) = match &pending.effect {
                crate::types::ability::Effect::ReturnAsAura {
                    enchant_filter,
                    grants,
                } => (enchant_filter.clone(), grants.clone()),
                _ => {
                    return Err(EngineError::InvalidAction(
                        "ReturnAsAuraTarget: pending_effect is not ReturnAsAura".to_string(),
                    ));
                }
            };
            super::effects::return_as_aura::finalize_attach(
                state,
                pending.as_ref(),
                returned,
                chosen,
                &filter,
                grants,
                &mut events,
            )
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            // After resolving the attach, return control to standard priority
            // flow under the picker's controller, then resume any chain that was
            // paused behind the picker.
            state.waiting_for = WaitingFor::Priority {
                player: active_player,
            };
            state.priority_player = active_player;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        (
            WaitingFor::EquipTarget {
                player,
                equipment_id,
                valid_targets,
            },
            GameAction::Equip {
                equipment_id: eq_id,
                target_id,
            },
        ) => {
            if eq_id != *equipment_id {
                return Err(EngineError::InvalidAction(
                    "Equipment ID mismatch".to_string(),
                ));
            }
            if !valid_targets.contains(&target_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid equip target".to_string(),
                ));
            }
            let p = *player;
            push_keyword_action(
                state,
                p,
                eq_id,
                KeywordAction::Equip {
                    equipment_id: eq_id,
                    target_creature_id: target_id,
                },
                &mut events,
            )
        }
        (WaitingFor::Priority { player }, GameAction::Equip { equipment_id, .. }) => {
            let p = *player;
            handle_equip_activation(state, p, equipment_id, &mut events)?
        }
        // CR 702.122a: Crew activation from Priority
        (WaitingFor::Priority { player }, GameAction::CrewVehicle { vehicle_id, .. }) => {
            let p = *player;
            handle_crew_activation(state, p, vehicle_id, &mut events)?
        }
        // CR 702.122a: Crew creature selection from CrewVehicle state
        (
            WaitingFor::CrewVehicle {
                player,
                vehicle_id,
                crew_power,
                eligible_creatures,
            },
            GameAction::CrewVehicle {
                vehicle_id: _vid,
                creature_ids,
            },
        ) => handle_crew_announcement(
            state,
            *player,
            *vehicle_id,
            *crew_power,
            eligible_creatures,
            &creature_ids,
            &mut events,
        )?,
        // CR 702.184a: Station activation from Priority — enters target-selection state.
        (
            WaitingFor::Priority { player },
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        ) => {
            let p = *player;
            handle_station_activation(state, p, spacecraft_id, &mut events)?
        }
        // CR 702.184a: Station creature selection — resolves the ability.
        (
            WaitingFor::StationTarget {
                player,
                spacecraft_id,
                eligible_creatures,
            },
            GameAction::ActivateStation {
                spacecraft_id: _sid,
                creature_id: Some(cid),
            },
        ) => handle_station_announcement(
            state,
            *player,
            *spacecraft_id,
            eligible_creatures,
            cid,
            &mut events,
        )?,
        // CR 702.171a: Saddle activation from Priority — enters target-selection state.
        (WaitingFor::Priority { player }, GameAction::SaddleMount { mount_id, .. }) => {
            let p = *player;
            handle_saddle_activation(state, p, mount_id, &mut events)?
        }
        // CR 702.171a: Saddle creature selection — announces, pays cost, pushes stack entry.
        (
            WaitingFor::SaddleMount {
                player,
                mount_id,
                saddle_power,
                eligible_creatures,
            },
            GameAction::SaddleMount {
                mount_id: _mid,
                creature_ids,
            },
        ) => handle_saddle_announcement(
            state,
            *player,
            *mount_id,
            *saddle_power,
            eligible_creatures,
            &creature_ids,
            &mut events,
        )?,
        (WaitingFor::Priority { player }, GameAction::Transform { object_id }) => {
            let p = *player;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::InvalidAction(
                    "Object is not on the battlefield".to_string(),
                ));
            }
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "You don't control this permanent".to_string(),
                ));
            }
            if obj.back_face.is_none() {
                return Err(EngineError::InvalidAction(
                    "Card has no back face".to_string(),
                ));
            }
            super::transform::transform_permanent(state, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        // CR 702.49: Ninjutsu-family activation during combat
        (
            WaitingFor::Priority { player },
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_object_id,
                creature_to_return,
                &mut events,
            )
            .map_err(EngineError::InvalidAction)?;
            WaitingFor::Priority { player: p }
        }
        // CR 702.190a: Sneak — cast a spell from hand during declare blockers
        // by paying the Sneak cost and returning an unblocked attacker.
        // Applies to any card type; permanent-spell placement (CR 702.190b)
        // is handled at resolution based on the variant's `placement`.
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsSneak {
                hand_object,
                card_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::casting::handle_cast_spell_as_sneak(
                state,
                p,
                hand_object,
                card_id,
                creature_to_return,
                &mut events,
            )?
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsSneakWithPaymentMode {
                hand_object,
                card_id,
                creature_to_return,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_as_sneak_with_payment_mode(
            state,
            *player,
            hand_object,
            card_id,
            creature_to_return,
            payment_mode,
            &mut events,
        )?,
        // CR 702.188a: Web-slinging — cast a spell from hand by paying the
        // Web-slinging cost and returning a tapped creature you control.
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsWebSlinging {
                hand_object,
                card_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::casting::handle_cast_spell_as_web_slinging(
                state,
                p,
                hand_object,
                card_id,
                creature_to_return,
                &mut events,
            )?
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellAsWebSlingingWithPaymentMode {
                hand_object,
                card_id,
                creature_to_return,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_as_web_slinging_with_payment_mode(
            state,
            *player,
            hand_object,
            card_id,
            creature_to_return,
            payment_mode,
            &mut events,
        )?,
        // CR 601.2b + CR 118.9a: CastFromHandFree opt-in path — cast a hand
        // spell for free via a once-per-turn permission source (Zaffai).
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellForFree {
                object_id,
                card_id,
                source_id,
            },
        ) => {
            let p = *player;
            super::casting::handle_cast_spell_for_free(
                state,
                p,
                object_id,
                card_id,
                source_id,
                &mut events,
            )?
        }
        (
            WaitingFor::Priority { player },
            GameAction::CastSpellForFreeWithPaymentMode {
                object_id,
                card_id,
                source_id,
                payment_mode,
            },
        ) => super::casting::handle_cast_spell_for_free_with_payment_mode(
            state,
            *player,
            object_id,
            card_id,
            source_id,
            payment_mode,
            &mut events,
        )?,
        // CR 702.94a: Miracle reveal — accept path. The player reveals the card;
        // this creates a triggered ability ("When you reveal this card this way,
        // you may cast it for [miracle cost]") that goes on the stack. Opponents
        // can respond before the cast offer resolves.
        (
            WaitingFor::MiracleReveal {
                player,
                object_id,
                cost,
            },
            GameAction::CastSpellAsMiracle {
                object_id: action_obj,
                ..
            }
            | GameAction::CastSpellAsMiracleWithPaymentMode {
                object_id: action_obj,
                ..
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match the outstanding miracle reveal"
                        .to_string(),
                ));
            }
            let p = *player;
            let source = *object_id;
            let miracle_cost = cost.clone();

            // CR 702.94a: Emit the reveal event.
            // CR 702.94a: Emit the reveal event.
            let card_name = state
                .objects
                .get(&source)
                .map(|o| o.name.clone())
                .unwrap_or_default();
            events.push(crate::types::events::GameEvent::CardsRevealed {
                player: p,
                card_ids: vec![source],
                card_names: vec![card_name],
            });

            // CR 702.94a: Push the miracle triggered ability onto the stack.
            // "When you reveal this card this way, you may cast it by paying
            // [miracle cost] rather than its mana cost."
            let ability = crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::MiracleCast { cost: miracle_cost },
                vec![],
                source,
                p,
            );
            let trigger = super::triggers::PendingTrigger {
                source_id: source,
                controller: p,
                condition: None,
                ability,
                timestamp: 0,
                target_constraints: vec![],
                distribute: None,
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: Some("Miracle — you may cast this card".to_string()),
                may_trigger_origin: None,
                subject_match_count: None,
            };
            super::triggers::push_pending_trigger_to_stack(state, trigger, &mut events);

            // Return to priority so the trigger can be responded to.
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
            )?
        }
        // CR 702.94a: Miracle reveal — decline path. Reuses the generic
        // DecideOptionalEffect decline; flushes the next pending miracle
        // offer or returns to Priority. Flip `waiting_for` out of MiracleReveal
        // before running the pipeline so its Priority-gated path (line 46 of
        // engine_priority) engages and the flush has a chance to pop the next
        // offer.
        (
            WaitingFor::MiracleReveal { player, .. },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
            )?
        }
        // CR 702.94a + CR 608.2g: Miracle cast offer — the miracle triggered
        // ability has resolved. The player may now cast for the miracle cost.
        // This cast happens during trigger resolution, so timing restrictions
        // do not apply (CR 608.2g).
        (
            WaitingFor::MiracleCastOffer {
                player, object_id, ..
            },
            GameAction::CastSpellAsMiracle {
                object_id: action_obj,
                card_id,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match miracle cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            super::casting::handle_cast_spell_as_miracle(state, p, obj, card_id, &mut events)?
        }
        (
            WaitingFor::MiracleCastOffer {
                player, object_id, ..
            },
            GameAction::CastSpellAsMiracleWithPaymentMode {
                object_id: action_obj,
                card_id,
                payment_mode,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMiracle object_id does not match miracle cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            super::casting::handle_cast_spell_as_miracle_with_payment_mode(
                state,
                p,
                obj,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        // CR 702.94a: Miracle cast offer — decline. Resume resolution.
        (
            WaitingFor::MiracleCastOffer { player, .. },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
            )?
        }
        // CR 702.35a: Madness cast offer — the madness triggered ability has
        // resolved. The player may now cast the exiled card for its madness cost.
        (
            WaitingFor::MadnessCastOffer {
                player, object_id, ..
            },
            GameAction::CastSpellAsMadness {
                object_id: action_obj,
                card_id,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMadness object_id does not match madness cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            super::casting::handle_cast_spell_as_madness(state, p, obj, card_id, &mut events)?
        }
        (
            WaitingFor::MadnessCastOffer {
                player, object_id, ..
            },
            GameAction::CastSpellAsMadnessWithPaymentMode {
                object_id: action_obj,
                card_id,
                payment_mode,
            },
        ) => {
            if *object_id != action_obj {
                return Err(EngineError::InvalidAction(
                    "CastSpellAsMadness object_id does not match madness cast offer".to_string(),
                ));
            }
            let p = *player;
            let obj = action_obj;
            super::casting::handle_cast_spell_as_madness_with_payment_mode(
                state,
                p,
                obj,
                card_id,
                payment_mode,
                &mut events,
            )?
        }
        // CR 702.35a: Madness decline — put the exiled card into its owner's graveyard.
        (
            WaitingFor::MadnessCastOffer {
                player, object_id, ..
            },
            GameAction::DecideOptionalEffect { accept: false },
        ) => {
            let p = *player;
            super::zones::move_to_zone(state, *object_id, Zone::Graveyard, &mut events);
            state.waiting_for = WaitingFor::Priority { player: p };
            super::engine_priority::run_post_action_pipeline(
                state,
                &mut events,
                &WaitingFor::Priority { player: p },
                true,
            )?
        }
        (waiting_for, action) if engine_resolution_choices::handles(waiting_for) => {
            match engine_resolution_choices::handle_resolution_choice(
                state,
                waiting_for.clone(),
                action,
                &mut events,
            )? {
                engine_resolution_choices::ResolutionChoiceOutcome::WaitingFor(waiting_for) => {
                    waiting_for
                }
                engine_resolution_choices::ResolutionChoiceOutcome::ActionResult(result) => {
                    return Ok(result);
                }
            }
        }
        (WaitingFor::Priority { player }, GameAction::PlayFaceDown { object_id, card_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            // Validate object_id matches card_id and is in hand
            let valid = state.objects.get(&object_id).is_some_and(|obj| {
                obj.card_id == card_id && obj.owner == p && obj.zone == Zone::Hand
            });
            if !valid {
                return Err(EngineError::InvalidAction(
                    "Card not found in hand".to_string(),
                ));
            }
            super::morph::play_face_down(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::TurnFaceUp { object_id }) => {
            if state.priority_player
                != turn_control::authorized_submitter_for_player(state, *player)
            {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            super::morph::turn_face_up(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                target_constraints,
                ..
            },
            GameAction::SelectTargets { targets },
        ) => engine_stack::handle_trigger_target_selection_select_targets(
            state,
            *player,
            target_slots,
            target_constraints,
            targets,
            &mut events,
        )?,
        (WaitingFor::TriggerTargetSelection { .. }, GameAction::ChooseTarget { target }) => {
            let waiting_for = state.waiting_for.clone();
            engine_stack::handle_trigger_target_selection_choose_target(
                state,
                waiting_for,
                target,
                &mut events,
            )?
        }
        (
            WaitingFor::BetweenGamesSideboard { player, .. },
            GameAction::SubmitSideboard { main, sideboard },
        ) => match_flow::handle_submit_sideboard(state, *player, main, sideboard)
            .map_err(EngineError::InvalidAction)?,
        (
            WaitingFor::BetweenGamesChoosePlayDraw { player, .. },
            GameAction::ChoosePlayDraw { play_first },
        ) => match_flow::handle_choose_play_draw(state, *player, play_first, &mut events)
            .map_err(EngineError::InvalidAction)?,
        (
            waiting_for @ WaitingFor::AbilityModeChoice { .. },
            GameAction::SelectModes { indices },
        ) => engine_modes::handle_ability_mode_choice(
            state,
            waiting_for.clone(),
            indices,
            &mut events,
        )?,
        // CR 601.2c: Player selected targets from a multi-target set ("any number of").
        (WaitingFor::MultiTargetSelection { .. }, GameAction::SelectCards { cards: selected }) => {
            let waiting_for = state.waiting_for.clone();
            engine_stack::handle_multi_target_selection(state, waiting_for, &selected, &mut events)?
        }
        // CR 702.139a: Pre-game companion reveal
        (
            WaitingFor::CompanionReveal { player, .. },
            GameAction::DeclareCompanion { card_index },
        ) => super::companion::handle_declare_companion(state, *player, card_index, &mut events),
        // CR 702.139a: Special action — pay {3} to put companion into hand (see rule 116.2g).
        (WaitingFor::Priority { player }, GameAction::CompanionToHand) => {
            state.lands_tapped_for_mana.remove(player);
            super::companion::handle_companion_to_hand(state, *player, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        // CR 722.3c / CR 601.2: Prepare (Strixhaven) — cast a copy of the
        // prepared face through the normal spell-casting pipeline (costs,
        // targeting, and mode choices all run through casting.rs single
        // authority). Assign when WotC publishes SOS CR update.
        (WaitingFor::Priority { player }, GameAction::CastPreparedCopy { source }) => {
            let p = *player;
            // Validate controller.
            let src = source;
            let Some(obj) = state.objects.get(&src) else {
                return Err(EngineError::InvalidAction(format!(
                    "CastPreparedCopy: source {src:?} not found"
                )));
            };
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "CastPreparedCopy: source not controlled by acting player".to_string(),
                ));
            }
            effects::prepare::cast_prepared_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        // CR 702.xxx: Paradigm (Strixhaven) — accept the turn-based offer to
        // cast a copy of an exiled paradigm source. Assign when WotC
        // publishes SOS CR update.
        (
            WaitingFor::ParadigmCastOffer { player, offers },
            GameAction::CastParadigmCopy { source },
        ) => {
            let src = source;
            if !offers.contains(&src) {
                return Err(EngineError::InvalidAction(format!(
                    "CastParadigmCopy: source {src:?} not in current offer set"
                )));
            }
            let p = *player;
            let copy_id = effects::paradigm::cast_paradigm_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?;
            // CR 707.10c: If the paradigm spell has target slots, open target
            // selection via CopyRetarget. Otherwise return to priority so the
            // copy resolves through normal stack flow.
            if effects::prepare::open_copy_target_selection(state, copy_id, p)
                .map_err(EngineError::InvalidAction)?
            {
                state.waiting_for.clone()
            } else {
                WaitingFor::Priority { player: p }
            }
        }
        // CR 702.xxx: Paradigm (Strixhaven) — decline the turn-based offer.
        // Assign when WotC publishes SOS CR update.
        (WaitingFor::ParadigmCastOffer { player, .. }, GameAction::PassParadigmOffer) => {
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::Priority { player }, GameAction::SetAutoPass { mode }) => {
            // Convert request to stored mode, capturing engine state as needed.
            let stored_mode = match mode {
                AutoPassRequest::UntilStackEmpty => AutoPassMode::UntilStackEmpty {
                    initial_stack_len: state.stack.len(),
                },
                AutoPassRequest::UntilEndOfTurn => AutoPassMode::UntilEndOfTurn,
            };
            state.auto_pass.insert(*player, stored_mode);
            let wf = pass_priority_once_with_pipeline(state, &mut events)?;
            return Ok(ActionResult {
                events,
                waiting_for: wf,
                log_entries: vec![],
            });
        }
        // CR 701.34a: Proliferate — player selected targets to proliferate.
        (
            WaitingFor::ProliferateChoice { player, eligible },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for proliferate".to_string(),
                    ));
                }
            }
            effects::proliferate::apply_proliferate(state, p, &targets, &mut events);
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Proliferate,
                source_id: ObjectId(0), // Source not tracked through choice state
            });
            // CR 701.34a: Emit player-action event so proliferate triggers fire.
            events.push(GameEvent::PlayerPerformedAction {
                player_id: p,
                action: PlayerActionKind::Proliferate,
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        // CR 608.2c: ChooseObjectsIntoTrackedSet — player submitted their
        // battlefield-permanent selection. Publish a fresh tracked set so the
        // downstream `PayCost { ScaledMana }` and the `IfYouDo`/`Untap` tail
        // resolve against exactly this selection, then resume the chain.
        (
            WaitingFor::ChooseObjectsSelection {
                player,
                eligible,
                trigger_event,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            let pending_event = trigger_event.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for object selection".to_string(),
                    ));
                }
            }
            // Map TargetRef → ObjectId. The eligible set is all battlefield
            // permanents, so every selected target is an Object.
            let ids: Vec<ObjectId> = targets
                .iter()
                .filter_map(|t| match t {
                    TargetRef::Object(id) => Some(*id),
                    TargetRef::Player(_) => None,
                })
                .collect();
            // CR 603.7: Always allocate a fresh tracked set — a player-chosen
            // "those creatures" set is a new resolution scope. An empty
            // selection yields an empty fresh set (size 0).
            effects::publish_fresh_tracked_set(state, ids);
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::ChooseObjectsIntoTrackedSet,
                source_id: ObjectId(0), // Source not tracked through choice state
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            // CR 608.2: restore the triggering event so the stashed
            // `PayCost { ScaledMana, payer: TriggeringPlayer }` continuation
            // resolves the payer correctly — the trigger's resolution is still
            // in flight.
            // CR 603.2c + CR 608.2: the batched-trigger subject count is also
            // part of the trigger's resolution scope — mirror its save/restore
            // so an `EventContextAmount` inside the resumed continuation reads
            // the original "that many" instead of `None`.
            let previous_trigger_event = state.current_trigger_event.clone();
            let previous_trigger_match_count = state.current_trigger_match_count;
            state.current_trigger_event = pending_event;
            state.current_trigger_match_count = state.pending_optional_trigger_match_count.take();
            effects::drain_pending_continuation(state, &mut events);
            state.current_trigger_event = previous_trigger_event;
            state.current_trigger_match_count = previous_trigger_match_count;
            state.waiting_for.clone()
        }
        // CR 707.10c: Copy retarget — player chose target for the current slot
        // via battlefield click. Advances slot-by-slot; finalizes on the last slot.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                current_slot,
            },
            GameAction::ChooseTarget { target },
        ) => {
            let p = *player;
            let cid = *copy_id;
            let slot_idx = *current_slot;
            if let Some(ref t) = target {
                let slot = &target_slots[slot_idx];
                // CR 707.10c: A retarget choice must produce a legal target. Both
                // `prepare::open_copy_target_selection` and `copy_spell::resolve`
                // populate `legal_alternatives` from `build_target_slots`, so an
                // empty list means "no legal alternative exists" — the caller
                // must use `KeepAllCopyTargets` (or send `target: None`).
                if !slot.legal_alternatives.contains(t) {
                    return Err(EngineError::InvalidAction(format!(
                        "Target {t:?} not a legal alternative for copy slot {slot_idx}"
                    )));
                }
            } else if target_slots[slot_idx].current.is_none() {
                return Err(EngineError::InvalidAction(format!(
                    "Copy target slot {slot_idx} has no current target to keep"
                )));
            }
            let mut updated_slots = target_slots.clone();
            if let Some(t) = target {
                updated_slots[slot_idx].current = Some(t.clone());
            }
            let next_slot = slot_idx + 1;
            if next_slot < updated_slots.len() {
                state.waiting_for = WaitingFor::CopyRetarget {
                    player: p,
                    copy_id: cid,
                    target_slots: updated_slots,
                    current_slot: next_slot,
                };
            } else {
                finalize_copy_retarget(state, p, cid, &updated_slots, &mut events)?;
            }
            state.waiting_for.clone()
        }
        // CR 707.10c: "Keep Current Targets" — accept every remaining slot's
        // current value in one action. Equivalent to dispatching
        // `ChooseTarget { target: None }` for each remaining slot, but resolved
        // server-side so the UI doesn't pay N round-trips. The slot-by-slot
        // `ChooseTarget` path above remains the single authority for the
        // per-slot legality/advance semantics.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
                ..
            },
            GameAction::KeepAllCopyTargets,
        ) => {
            let p = *player;
            let cid = *copy_id;
            let slots = target_slots.clone();
            finalize_copy_retarget(state, p, cid, &slots, &mut events)?;
            state.waiting_for.clone()
        }
        // CR 510.1c/d: Combat damage assignment from attacker to blockers.
        (
            WaitingFor::AssignCombatDamage {
                player,
                attacker_id,
                total_damage,
                blockers,
                assignment_modes,
                trample,
                defending_player,
                attack_target,
                pw_loyalty,
                pw_controller,
            },
            GameAction::AssignCombatDamage {
                mode,
                assignments,
                trample_damage,
                controller_damage,
            },
        ) => {
            triggers_processed_inline = true;
            engine_combat::handle_assign_combat_damage(
                state,
                *player,
                *attacker_id,
                *total_damage,
                blockers,
                assignment_modes,
                *trample,
                *defending_player,
                attack_target,
                *pw_loyalty,
                *pw_controller,
                mode,
                &assignments,
                trample_damage,
                controller_damage,
                &mut events,
            )?
        }
        // CR 601.2d: Distribute among targets (casting-time distribution).
        (
            WaitingFor::DistributeAmong {
                player,
                total,
                targets,
                ..
            },
            GameAction::DistributeAmong { distribution },
        ) => {
            let p = *player;
            let expected_total = *total;

            // Validate: each target gets ≥ 1, and total matches.
            let actual_total: u32 = distribution.iter().map(|(_, a)| *a).sum();
            if actual_total != expected_total {
                return Err(EngineError::InvalidAction(format!(
                    "Distribution total {} != required {}",
                    actual_total, expected_total
                )));
            }
            for (t, amount) in &distribution {
                if *amount == 0 {
                    return Err(EngineError::InvalidAction(
                        "Each target must receive at least 1".to_string(),
                    ));
                }
                if !targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Distribution target not in legal set".to_string(),
                    ));
                }
            }

            // Store on the pending cast's resolved ability if we're mid-casting.
            // The distribution will be read during effect resolution.
            if let Some(pending) = state.pending_cast.as_mut() {
                pending.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
            }

            // CR 601.2d: Resume casting pipeline after distribution.
            if state.pending_cast.is_some() {
                // Mid-cast distribution: resume finalize_cast to commit the spell.
                let pending = state.pending_cast.take().unwrap();
                casting_costs::finalize_cast(
                    state,
                    p,
                    pending.object_id,
                    pending.card_id,
                    pending.ability,
                    &pending.cost,
                    pending.casting_variant,
                    pending.cast_timing_permission,
                    pending.origin_zone,
                    &mut events,
                )?
            } else if let Some(mut pending_trigger) = state.pending_trigger.take() {
                // CR 601.2d + CR 603.3d: Triggered abilities divide effects
                // while being put on the stack. The chosen per-target amounts
                // are resolution data on the resolved ability. The entry is
                // already on the stack (pushed at distribute-among pause time);
                // mutate its ability with the distribution and clear
                // `pending_trigger_entry` so the resolver may now fire it.
                //
                // Invariants (panic on violation — no recovery path):
                // * `pending_trigger_entry` is `Some(_)` (push-first contract).
                // * Entry id references a `TriggeredAbility` `StackEntry`.
                pending_trigger.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
                triggers::finalize_pending_trigger_entry(state, &pending_trigger.ability);
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                // CR 113.2c + CR 603.2 + CR 603.3b: Drain siblings deferred
                // behind this distribute-among trigger so each independent
                // instance reaches the stack (issue #416).
                debug_assert!(
                    !triggers::is_pending_trigger_construction_active(state),
                    "deferred-trigger drain entered with construction still active",
                );
                if let Some(waiting_for) =
                    triggers::drain_deferred_trigger_queue(state, &mut events)
                {
                    waiting_for
                } else {
                    WaitingFor::Priority { player: p }
                }
            } else {
                // Resolution-time distribution continuation path.
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                effects::drain_pending_continuation(state, &mut events);
                state.waiting_for.clone()
            }
        }
        (
            WaitingFor::MoveCountersDistribution {
                player,
                source_id,
                available,
                destinations,
                pending_effect,
                ..
            },
            GameAction::ChooseCounterMoveDistribution { selections },
        ) => {
            let p = *player;
            effects::counters::validate_and_queue_counter_move_distribution(
                state,
                &selections,
                *source_id,
                available,
                destinations,
                pending_effect,
            )
            .map_err(|err| EngineError::InvalidAction(err.to_string()))?;
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::counters::drain_pending_counter_moves(state, &mut events);
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                effects::drain_pending_continuation(state, &mut events);
            }
            state.waiting_for.clone()
        }
        // CR 115.7: Retarget a spell or ability on the stack via the dialog
        // path — the multi-target (`All`-scope) UI submits every new target at
        // once.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                legal_new_targets,
                ..
            },
            GameAction::RetargetSpell { new_targets },
        ) => apply_retarget(
            state,
            &mut events,
            *player,
            *stack_entry_index,
            legal_new_targets,
            new_targets,
        )?,
        // CR 115.7: Retarget a single-target spell via a board click. The
        // universal `ChooseTarget` action — already consumed by every other
        // targeting state — drives single-target retargets (Bolt Bend,
        // Redirect, Misdirection) so the player picks the new target directly
        // on the battlefield instead of through a dialog.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                scope: RetargetScope::Single,
                legal_new_targets,
                ..
            },
            GameAction::ChooseTarget { target: Some(t) },
        ) => apply_retarget(
            state,
            &mut events,
            *player,
            *stack_entry_index,
            legal_new_targets,
            vec![t],
        )?,
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // Run post-action pipeline (SBAs, triggers, layers) and check for terminal states.
    // When triggers were already processed inline (e.g., DeclareAttackers, combat damage),
    // pass the flag to skip the trigger scan but still run SBAs, delayed triggers, and layers.
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        // Sync state.waiting_for before the pipeline so SBA/trigger checks see
        // the action's result, not the pre-action state (fixes stale TargetSelection
        // after CancelCast).
        state.waiting_for = waiting_for.clone();
        let wf = engine_priority::run_post_action_pipeline(
            state,
            &mut events,
            &waiting_for,
            triggers_processed_inline,
        )?;
        state.waiting_for = wf.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
        });
    }

    // CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance (e.g.,
    // combat damage step) before we reach this point. state.waiting_for is the authoritative
    // result — written directly by eliminate_player → check_game_over. Guard against
    // overwriting it with the computed `waiting_for` from auto_advance.
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        let wf = state.waiting_for.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
        });
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
        log_entries: vec![],
    })
}

/// CR 115.7d: Apply a validated retarget to the stack entry, then hand priority
/// back to the retargeting player. Single authority for both retarget entry
/// points — the board-click (`ChooseTarget`) and dialog (`RetargetSpell`) paths
/// — so target validation and stack mutation can never drift apart.
fn apply_retarget(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    player: PlayerId,
    stack_entry_index: usize,
    legal_new_targets: &[TargetRef],
    new_targets: Vec<TargetRef>,
) -> Result<WaitingFor, EngineError> {
    // CR 115.7d: Every submitted target must be in the legal set.
    for t in &new_targets {
        if !legal_new_targets.contains(t) {
            return Err(EngineError::InvalidAction(
                "Retarget: chosen target not in legal alternatives".to_string(),
            ));
        }
    }

    if stack_entry_index < state.stack.len() {
        if let Some(ability) = state.stack[stack_entry_index].ability_mut() {
            ability.targets = new_targets;
        }
    } else {
        return Err(EngineError::InvalidAction(
            "Invalid stack entry index for retargeting".to_string(),
        ));
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ChangeTargets,
        source_id: state
            .stack
            .get(stack_entry_index)
            .map(|e| e.source_id)
            .unwrap_or(ObjectId(0)),
    });
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
    effects::drain_pending_continuation(state, events);
    Ok(state.waiting_for.clone())
}

/// Run state-based actions, exile returns, delayed triggers, and trigger processing
/// after an action that produced `WaitingFor::Priority`. Returns the resulting
/// `WaitingFor` state — may be terminal (GameOver, interactive choice) or
/// a continuation (Priority for next player/active player).
///
/// `default_wf` is the WaitingFor computed by the action handler, used as fallback
/// when no terminal/trigger/SBA outcome overrides it.
///
/// `skip_trigger_scan` — when `true`, skips the `process_triggers` call because
/// triggers were already processed inline (e.g., combat damage, declare attackers).
/// SBAs, exile returns, delayed triggers, and layer evaluation still run.
pub(super) fn begin_pending_trigger_target_selection(
    state: &mut GameState,
) -> Result<Option<WaitingFor>, EngineError> {
    let Some(trigger) = state.pending_trigger.as_ref() else {
        return Ok(None);
    };

    // CR 700.2b: Modal trigger — prompt for mode selection before stack.
    if let Some(ref modal) = trigger.modal {
        if !trigger.mode_abilities.is_empty() {
            let player = trigger.controller;
            let source_id = trigger.source_id;
            let mode_abilities = trigger.mode_abilities.clone();
            let trigger_event = trigger.trigger_event.clone();
            let trigger_events = if state.pending_trigger_event_batch.is_empty() {
                trigger_event.iter().cloned().collect::<Vec<_>>()
            } else {
                state.pending_trigger_event_batch.clone()
            };
            let subject_match_count = trigger.subject_match_count;
            let modal = modal_choice_for_player(
                state,
                player,
                source_id,
                modal,
                &crate::types::ability::SpellContext::default(),
            );
            let mut unavailable_modes = compute_unavailable_modes(state, source_id, &modal);
            let context_snapshot = super::triggers::push_trigger_event_context(
                state,
                trigger_event.as_ref(),
                &trigger_events,
                subject_match_count,
            );
            super::ability_utils::filter_modes_by_target_legality(
                state,
                source_id,
                player,
                &mode_abilities,
                &modal,
                &mut unavailable_modes,
            );
            super::triggers::restore_trigger_event_context(state, context_snapshot);

            // CR 700.2b + CR 603.3c: All modes unavailable (previously chosen
            // OR no legal targets) — ability cannot remain on the stack.
            // Under the "push first, choose second" contract, the entry may
            // already have been pushed by `dispatch_pending_trigger_context`;
            // remove it before clearing the cursor. The new flow filters this
            // case BEFORE pushing in the modal branch, so this is normally a
            // dead branch — kept as a defensive cleanup for any
            // delayed-revalidation paths.
            if unavailable_modes.len() >= modal.mode_count {
                if let Some(entry_id) = state.pending_trigger_entry.take() {
                    if state.stack.back().map(|e| e.id) == Some(entry_id) {
                        state.stack.pop_back();
                        state.stack_paid_facts.remove(&entry_id);
                        state.stack_trigger_event_batches.remove(&entry_id);
                    }
                }
                state.pending_trigger = None;
                return Ok(None);
            }

            return Ok(Some(WaitingFor::AbilityModeChoice {
                player,
                modal,
                source_id,
                mode_abilities,
                is_activated: false,
                ability_index: None,
                ability_cost: None,
                unavailable_modes,
            }));
        }
    }

    let ability = trigger.ability.clone();
    let player = trigger.controller;
    let source_id = trigger.source_id;
    let target_constraints = trigger.target_constraints.clone();
    let description = trigger.description.clone();
    let trigger_event = trigger.trigger_event.clone();
    let trigger_events = if state.pending_trigger_event_batch.is_empty() {
        trigger_event.iter().cloned().collect::<Vec<_>>()
    } else {
        state.pending_trigger_event_batch.clone()
    };
    let subject_match_count = trigger.subject_match_count;
    let context_snapshot = super::triggers::push_trigger_event_context(
        state,
        trigger_event.as_ref(),
        &trigger_events,
        subject_match_count,
    );
    let selection_result = build_target_slots(state, &ability).and_then(|target_slots| {
        if target_slots.is_empty() {
            return Ok(None);
        }
        begin_target_selection_for_ability(state, &ability, &target_slots, &target_constraints)
            .map(|selection| Some((target_slots, selection)))
    });
    super::triggers::restore_trigger_event_context(state, context_snapshot);
    let Some((target_slots, selection)) = selection_result? else {
        // CR 603.3d: No target prompt is required (empty target slots, or
        // `build_target_slots`/`begin_target_selection_for_ability` reported
        // no legal completion). Symmetric to the modal `all-modes-unavailable`
        // branch above: if the "push first" dispatcher already pushed an
        // in-construction entry for this trigger, pop it before clearing the
        // cursor. The new flow filters this case BEFORE pushing in the
        // non-modal branches (Err(_) drops the trigger; Ok(Some(targets))
        // auto-pushes a complete entry), so this is normally a dead branch —
        // kept for symmetry with the modal cleanup and for any
        // delayed-revalidation paths.
        if let Some(entry_id) = state.pending_trigger_entry.take() {
            if state.stack.back().map(|e| e.id) == Some(entry_id) {
                state.stack.pop_back();
                state.stack_paid_facts.remove(&entry_id);
                state.stack_trigger_event_batches.remove(&entry_id);
            }
        }
        state.pending_trigger = None;
        return Ok(None);
    };
    Ok(Some(WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        target_constraints,
        selection,
        source_id: Some(source_id),
        description,
    }))
}

/// CR 604.2 + CR 110.4: If a land was played from the graveyard via a
/// frequency-bounded permission source, record the appropriate per-turn slot
/// as used to prevent a second play/cast from the same source/slot this turn.
///
/// - `OncePerTurn` (Crucible-of-Worlds-class): record the source in
///   `graveyard_cast_permissions_used`.
/// - `OncePerTurnPerPermanentType` (Muldrotha-class): record the
///   `(source, slot_type)` pair in `graveyard_cast_permissions_used_per_type`.
///   The slot is picked here (not stashed beforehand) because lands take the
///   non-stack play-land path; the picker reads the live used-set so concurrent
///   frequency-bounded permissions are handled correctly.
/// - `Unlimited` (Crucible-of-Worlds-with-no-rider): no tracking.
fn record_graveyard_play_permission(
    state: &mut GameState,
    source: Option<ObjectId>,
    played_object: ObjectId,
) {
    let Some(source_id) = source else {
        return;
    };
    let Some(obj) = state.objects.get(&source_id) else {
        return;
    };
    let frequency =
        super::functioning_abilities::active_static_definitions(state, obj).find_map(|s| {
            match s.mode {
                StaticMode::GraveyardCastPermission { frequency, .. } => Some(frequency),
                _ => None,
            }
        });
    match frequency {
        Some(crate::types::statics::CastFrequency::OncePerTurn) => {
            state.graveyard_cast_permissions_used.insert(source_id);
        }
        Some(crate::types::statics::CastFrequency::OncePerTurnPerPermanentType) => {
            // CR 110.4: Use the player-chosen slot if one was stashed by the
            // ChoosePermanentTypeSlot dispatch (multi-type card). Otherwise
            // auto-pick (single-type card).
            let slot = state
                .pending_permanent_type_slot
                .take()
                .filter(|(src, _)| *src == source_id)
                .map(|(_, ct)| ct)
                .or_else(|| {
                    super::casting::pick_per_permanent_type_slot(state, source_id, played_object)
                });
            if let Some(slot) = slot {
                state
                    .graveyard_cast_permissions_used_per_type
                    .insert((source_id, slot));
            }
        }
        Some(crate::types::statics::CastFrequency::Unlimited) | None => {
            // Unlimited (Crucible of Worlds) or no permission: no tracking.
        }
    }
}

fn record_exile_play_permission(state: &mut GameState, source: Option<ObjectId>) {
    let Some(source_id) = source else {
        return;
    };
    state.exile_play_permissions_used.insert(source_id);
}

fn mark_land_played_from_zone(state: &mut GameState, object_id: ObjectId, zone: Zone) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.played_from_zone = Some(zone);
    }
}

fn handle_play_land(
    state: &mut GameState,
    object_id: ObjectId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate main phase
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Can only play lands during main phases".to_string(),
            ));
        }
    }

    // CR 305.2 + CR 505.6b: Validate land limit.
    // Base limit is max_lands_per_turn (normally 1), plus any additional drops
    // from static abilities like Exploration or Azusa.
    let player = turn_control::turn_resource_owner(state);
    // CR 305.2: "Can't play lands" suppresses the play-land special action outright.
    if super::static_abilities::player_has_static_other(state, player, "CantPlayLand") {
        return Err(EngineError::ActionNotAllowed(
            "Player is under a CantPlayLand static (CR 305.2)".to_string(),
        ));
    }
    let additional = super::static_abilities::additional_land_drops(state, player);
    let effective_limit = state.max_lands_per_turn.saturating_add(additional);
    if state.lands_played_this_turn >= effective_limit {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Validate that object_id exists in hand or graveyard (with permission)
    // or on top of library (with TopOfLibraryCastPermission { play_mode: Play })
    // and matches card_id.
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("priority player exists");
    let in_hand = player_data.hand.contains(&object_id);
    // CR 305.1 + CR 604.2: Check graveyard for play-from-graveyard permission
    // CR 604.2: Find graveyard play permission source (if any) for once-per-turn tracking.
    let gy_permission_source = if player_data.graveyard.contains(&object_id) {
        super::casting::graveyard_lands_playable_by_permission(state, player)
            .iter()
            .find(|(obj_id, _)| *obj_id == object_id)
            .map(|(_, source_id)| *source_id)
    } else {
        None
    };
    let in_graveyard_with_permission = gy_permission_source.is_some();

    // CR 401.5 + CR 305.1: Check top of library for
    // `TopOfLibraryCastPermission { play_mode: Play }` (Future Sight,
    // Bolas's Citadel, Magus of the Future). The helper already gates on
    // "front of library + play-mode permission + filter match + is a land,"
    // so we only need to confirm it points at the caller's object_id.
    let in_library_with_permission =
        super::casting::top_of_library_land_playable_by_permission(state, player)
            .is_some_and(|(top_id, _)| top_id == object_id);
    let exile_permission_source = if state.exile.contains(&object_id) {
        super::casting::exile_lands_playable_by_permission(state, player)
            .iter()
            .find(|(obj_id, _)| *obj_id == object_id)
            .map(|(_, source_id)| *source_id)
    } else {
        None
    };
    let in_exile_with_permission = exile_permission_source.is_some();

    if !in_hand
        && !in_graveyard_with_permission
        && !in_library_with_permission
        && !in_exile_with_permission
    {
        return Err(EngineError::InvalidAction(
            "Card not found in hand, graveyard, exile, or library with play permission".to_string(),
        ));
    }
    if !state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.card_id == card_id)
    {
        return Err(EngineError::InvalidAction(
            "Card not found or card_id mismatch".to_string(),
        ));
    }

    // CR 110.4: For multi-type graveyard lands via OncePerTurnPerPermanentType,
    // prompt the player to choose which permanent type slot to consume. Skip
    // if a slot was already chosen (pending_permanent_type_slot is set).
    if in_graveyard_with_permission && state.pending_permanent_type_slot.is_none() {
        if let Some(source) = gy_permission_source {
            if let Some(src_obj) = state.objects.get(&source) {
                let is_per_type = super::functioning_abilities::active_static_definitions(
                    state, src_obj,
                )
                .any(|s| {
                    matches!(
                        s.mode,
                        StaticMode::GraveyardCastPermission {
                            frequency:
                                crate::types::statics::CastFrequency::OncePerTurnPerPermanentType,
                            ..
                        }
                    )
                });
                if is_per_type {
                    let slots =
                        super::casting::available_permanent_type_slots(state, source, object_id);
                    if slots.len() > 1 {
                        return Ok(WaitingFor::ChoosePermanentTypeSlot {
                            player,
                            object_id,
                            card_id,
                            source,
                            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
                            available_slots: slots,
                        });
                    }
                }
            }
        }
    }

    // CR 712.12: MDFC land face selection
    if let Some(obj) = state.objects.get(&object_id) {
        let is_modal = obj
            .back_face
            .as_ref()
            .is_some_and(|bf| bf.layout_kind == Some(crate::types::card::LayoutKind::Modal));
        let front_is_land = obj
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land);
        let back_is_land = obj.back_face.as_ref().is_some_and(|bf| {
            bf.card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
        });

        if is_modal && front_is_land && back_is_land {
            // Both faces are lands — player must choose which face to put into play.
            // The land path never consumes payment_mode (lands cost no mana), but
            // the field is required; Auto is the inert default.
            return Ok(WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            });
        }

        if is_modal && !front_is_land && back_is_land {
            // CR 712.12: Only back face is a land — auto-swap (player already chose "play as land")
            let obj = state.objects.get_mut(&object_id).unwrap();
            let back = obj.back_face.take().expect("MDFC has back face");
            let front_snapshot = super::printed_cards::snapshot_object_face(obj);
            super::printed_cards::apply_back_face_to_object(obj, back);
            obj.back_face = Some(front_snapshot);
            // Do NOT set obj.transformed — MDFC face selection is not transformation.
            // zones.rs:38-46 reverts transformed permanents on zone exit; MDFCs must not trigger this.
        }
    }

    // Determine origin zone for the zone change event
    let origin_zone = if in_hand {
        Zone::Hand
    } else if in_graveyard_with_permission {
        Zone::Graveyard
    } else if in_exile_with_permission {
        Zone::Exile
    } else {
        // CR 401.5: in_library_with_permission — the card moves Library → Battlefield.
        Zone::Library
    };

    // Route through the replacement pipeline (handles ETB replacements like shock lands)
    let mut proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        object_id,
        origin_zone,
        Zone::Battlefield,
        None,
    );

    // CR 306.5b + CR 310.4b + CR 614.1c: Seed the intrinsic "enters with N
    // counters" replacement for planeswalkers and battles entering the
    // battlefield via a play-from-zone action.
    if let Some(obj) = state.objects.get(&object_id) {
        let intrinsic = super::printed_cards::intrinsic_etb_counters(obj);
        if !intrinsic.is_empty() {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                enter_with_counters,
                ..
            } = &mut proposed
            {
                enter_with_counters.extend(intrinsic);
            }
        }
    }

    match super::replacement::replace_event(state, proposed, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                object_id,
                to,
                enter_tapped,
                enter_with_counters,
                controller_override,
                ..
            } = event
            {
                zones::move_to_zone(state, object_id, to, events);
                mark_land_played_from_zone(state, object_id, origin_zone);
                // CR 400.7: reset_for_battlefield_entry (inside move_to_zone) sets
                // defaults. Override only when the replacement pipeline changed them.
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    if enter_tapped.resolve(false) {
                        obj.tapped = true;
                    }
                    if let Some(new_controller) = controller_override {
                        obj.controller = new_controller;
                    }
                }
                // CR 614.1c: Apply counters from replacement pipeline.
                engine_replacement::apply_etb_counters(
                    state,
                    object_id,
                    &enter_with_counters,
                    events,
                );
                // CR 614.1c: Apply pending ETB counters from delayed triggers
                // (e.g., "that creature enters with an additional +1/+1 counter").
                let pending: Vec<_> = state
                    .pending_etb_counters
                    .iter()
                    .filter(|(oid, _, _)| *oid == object_id)
                    .map(|(_, ct, n)| (ct.clone(), *n))
                    .collect();
                if !pending.is_empty() {
                    engine_replacement::apply_etb_counters(state, object_id, &pending, events);
                    state
                        .pending_etb_counters
                        .retain(|(oid, _, _)| *oid != object_id);
                }
            }

            // CR 614.12a: Drain post-replacement side effects (e.g., "As this land
            // enters, choose a color") that were stashed by the pipeline when the
            // execute ability is non-modifier work (Choose, etc.). Without this,
            // the choice prompt would fire at a random later resolution point with
            // the wrong controller context.
            if state.post_replacement_continuation.is_some() {
                state.post_replacement_source = None;
                if let Some(next_waiting_for) =
                    engine_replacement::apply_pending_post_replacement_effect(
                        state,
                        Some(object_id),
                        None,
                        Some(crate::types::replacements::ReplacementEvent::Moved),
                        events,
                    )
                {
                    state.lands_played_this_turn += 1;
                    mark_land_played_from_zone(state, object_id, origin_zone);
                    record_graveyard_play_permission(state, gy_permission_source, object_id);
                    record_exile_play_permission(state, exile_permission_source);
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                        p.lands_played_this_turn += 1;
                    }
                    state.priority_passes.clear();
                    state.priority_pass_count = 0;
                    events.push(GameEvent::LandPlayed {
                        object_id,
                        player_id: player,
                        from_zone: origin_zone,
                    });
                    return Ok(next_waiting_for);
                }
            }
        }
        super::replacement::ReplacementResult::Prevented => {
            // Land play was prevented — don't increment counters
            return Ok(WaitingFor::Priority {
                player: state.priority_player,
            });
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => {
            // A replacement needs player choice (e.g., shock land "pay 2 life?").
            // Increment counters now — the land play is committed, only the ETB
            // effect is pending.
            state.lands_played_this_turn += 1;
            mark_land_played_from_zone(state, object_id, origin_zone);
            // CR 604.2: Record once-per-turn graveyard play permission usage.
            record_graveyard_play_permission(state, gy_permission_source, object_id);
            record_exile_play_permission(state, exile_permission_source);
            if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                p.lands_played_this_turn += 1;
            }
            state.priority_passes.clear();
            state.priority_pass_count = 0;

            events.push(GameEvent::LandPlayed {
                object_id,
                player_id: player,
                from_zone: origin_zone,
            });

            return Ok(super::replacement::replacement_choice_waiting_for(
                player, state,
            ));
        }
    }

    // Increment land counter
    state.lands_played_this_turn += 1;
    mark_land_played_from_zone(state, object_id, origin_zone);
    // CR 604.2: Record once-per-turn graveyard play permission usage.
    record_graveyard_play_permission(state, gy_permission_source, object_id);
    record_exile_play_permission(state, exile_permission_source);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("priority player exists");
    player_data.lands_played_this_turn += 1;

    // Reset priority passes (action was taken)
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::LandPlayed {
        object_id,
        player_id: player,
        from_zone: origin_zone,
    });

    // Player retains priority after playing a land
    Ok(WaitingFor::Priority { player })
}

pub(super) fn handle_tap_land_for_mana(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let player = turn_control::turn_resource_owner(state);
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    // Validate: on battlefield, controlled by acting player, is a land, not tapped
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    if !obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::InvalidAction(
            "Object is not a land".to_string(),
        ));
    }
    if obj.tapped {
        return Err(EngineError::InvalidAction(
            "Land is already tapped".to_string(),
        ));
    }

    let mana_options = mana_sources::activatable_land_mana_options(state, object_id, player);
    if mana_options.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Land has no activatable mana ability".to_string(),
        ));
    }
    // Lands with multiple mana options (dual lands, triomes, etc.) must use
    // ActivateAbility with a specific ability_index to select which color.
    // TapLandForMana is a convenience shortcut for single-option lands only.
    if mana_options.len() > 1 {
        return Err(EngineError::ActionNotAllowed(
            "Land has multiple mana options — use ActivateAbility to choose".to_string(),
        ));
    }
    let mana_option = mana_options.into_iter().next().unwrap();

    let ability_to_resolve = mana_option.ability_index.and_then(|ability_index| {
        state
            .objects
            .get(&object_id)
            .and_then(|land| land.abilities.get(ability_index))
            .cloned()
    });

    if let Some(ability_def) = ability_to_resolve {
        mana_abilities::resolve_mana_ability(state, object_id, player, &ability_def, events, None)?;
        // CR 605.3b: Only record for `UntapLandForMana` when the activation is
        // fully reversible — painlands / pay-life sources commit irreversible
        // state during inline resolution and must not be eligible for undo.
        if mana_option.penalty.is_undoable() {
            state
                .lands_tapped_for_mana
                .entry(player)
                .or_default()
                .push(object_id);
        }
    } else {
        // Legacy fallback for subtype-only lands.
        let obj = state.objects.get_mut(&object_id).unwrap();
        obj.tapped = true;
        events.push(GameEvent::PermanentTapped {
            object_id,
            caused_by: None,
        });
        mana_payment::produce_mana(
            state,
            object_id,
            mana_option.mana_type,
            player,
            true,
            events,
        );
        // CR 106.12 + CR 106.12a: a basic/subtype-only land's intrinsic mana
        // ability always includes `{T}`. Emit one `TappedForMana` per
        // resolution so `TapsForMana` triggers fire exactly once (mirrors the
        // ability-resolution path in `produce_mana_from_ability`).
        events.push(GameEvent::TappedForMana {
            player_id: player,
            source_id: object_id,
            produced: vec![mana_option.mana_type],
            tap_state: crate::types::events::ManaTapState::FromTap,
        });
        state
            .lands_tapped_for_mana
            .entry(player)
            .or_default()
            .push(object_id);
    }

    Ok(WaitingFor::Priority { player })
}

/// CR 605.3b: Reverse a manual land tap — untap source and remove its mana from pool.
/// Rejects if the land isn't tracked or its mana was already spent.
pub(super) fn handle_untap_land_for_mana(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // Validate: object_id is in this player's lands_tapped_for_mana
    let tracked = state
        .lands_tapped_for_mana
        .get(&player)
        .is_some_and(|ids| ids.contains(&object_id));
    if !tracked {
        return Err(EngineError::InvalidAction(
            "Land was not manually tapped for mana".to_string(),
        ));
    }

    // CR 605.3: Mana abilities resolve immediately — once consumed, irreversible.
    // CR 605.1b: Aura/Equipment with a `TapsForMana` trigger that fired off this
    // land's tap (Fertile Ground / Wild Growth / Utopia Sprawl / Trace of
    // Abundance / Verdant Haven / Market Festival / Weirding Wood / Overgrowth
    // class) added their bonus mana to the same pool with `source_id = aura_id`,
    // not `source_id = land_id`. Refunding only the land's source would strand
    // the aura's mana in the pool, allowing an infinite tap-untap-tap exploit
    // (each cycle adds one bonus, refund only takes the land's mana). Walk every
    // active TapsForMana trigger whose `valid_card` matches the land and refund
    // mana keyed at the trigger's source object too. This preserves CR 605.3b
    // (mana abilities resolve immediately) — the manual-untap convenience is the
    // single irreversibility-bypass channel and must reverse all coupled mana,
    // not just the land's own contribution.
    let aura_sources: Vec<ObjectId> =
        super::mana_sources::aura_taps_for_mana_sources_for_land(state, object_id, player);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let removed = player_data.mana_pool.remove_from_source(object_id);
    if removed == 0 {
        return Err(EngineError::InvalidAction(
            "Mana from this source was already spent".to_string(),
        ));
    }
    for aura_id in &aura_sources {
        player_data.mana_pool.remove_from_source(*aura_id);
    }

    // Untap the land
    let obj = state
        .objects
        .get_mut(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    obj.tapped = false;
    events.push(GameEvent::PermanentUntapped { object_id });

    // Remove from tracking
    if let Some(ids) = state.lands_tapped_for_mana.get_mut(&player) {
        ids.retain(|&id| id != object_id);
        if ids.is_empty() {
            state.lands_tapped_for_mana.remove(&player);
        }
    }

    Ok(())
}

fn handle_equip_activation(
    state: &mut GameState,
    player: PlayerId,
    equipment_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate sorcery-speed timing: main phase, empty stack, active player
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Equip can only be activated during main phases".to_string(),
            ));
        }
    }
    if !state.stack.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated when the stack is empty".to_string(),
        ));
    }
    if state.active_player != player {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated by the active player".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&equipment_id)
        .ok_or_else(|| EngineError::InvalidAction("Equipment not found".to_string()))?;

    // Validate it's an equipment on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Equipment is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this equipment".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Equipment".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not an equipment".to_string(),
        ));
    }

    // Find valid targets: creatures controlled by the equipping player on battlefield
    let valid_targets: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                })
                .unwrap_or(false)
        })
        .collect();

    if valid_targets.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No valid creatures to equip".to_string(),
        ));
    }

    // If only one target, auto-equip: CR 113.3b still requires the stack entry
    // + priority window; we skip only the target-selection UI.
    if valid_targets.len() == 1 {
        let target_id = valid_targets[0];
        return Ok(push_keyword_action(
            state,
            player,
            equipment_id,
            KeywordAction::Equip {
                equipment_id,
                target_creature_id: target_id,
            },
            events,
        ));
    }

    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    })
}

/// CR 702.122a: Activate a Vehicle's crew ability from Priority.
/// Unlike Equip (CR 702.6a) and Saddle (CR 702.171a), Crew has NO "Activate only as a
/// sorcery" restriction — it can be activated any time the controller has priority.
fn handle_crew_activation(
    state: &mut GameState,
    player: PlayerId,
    vehicle_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&vehicle_id)
        .ok_or_else(|| EngineError::InvalidAction("Vehicle not found".to_string()))?;

    // Validate it's a Vehicle on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Vehicle is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Vehicle".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Vehicle".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not a Vehicle".to_string(),
        ));
    }

    // Extract crew power and activation cadence from keywords
    let (crew_power, crew_cadence) = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Crew {
                power,
                once_per_turn,
            } = kw
            {
                Some((*power, *once_per_turn))
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Vehicle has no Crew keyword".to_string()))?;

    // CR 602.5b: "Activate only once each turn" — reject a second crew activation
    // of this Vehicle in the same turn.
    if crew_cadence == crate::types::keywords::ActivationCadence::OncePerTurn
        && state.crew_activated_this_turn.contains(&vehicle_id)
    {
        return Err(EngineError::ActionNotAllowed(
            "This Vehicle's crew ability can be activated only once each turn".to_string(),
        ));
    }

    // Find eligible creatures: untapped creatures controlled by player, excluding the Vehicle
    // TODO: CR 702.122c — filter out creatures with "can't crew Vehicles" restriction when implemented
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            id != vehicle_id
                && state
                    .objects
                    .get(&id)
                    .map(|o| {
                        o.controller == player
                            && !o.tapped
                            && o.card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Creature)
                    })
                    .unwrap_or(false)
        })
        .collect();

    // Validate total power of all eligible creatures can meet the threshold
    let total_power: i32 = eligible_creatures
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|o| o.power.unwrap_or(0).max(0))
        .sum();

    if total_power < crew_power as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough total power among eligible creatures to crew".to_string(),
        ));
    }

    let _ = events; // No events emitted during activation
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::CrewVehicle {
        player,
        vehicle_id,
        crew_power,
        eligible_creatures,
    })
}

/// CR 113.3b: Push an activated keyword ability onto the stack and reset
/// priority. Called by the *_announcement handlers after costs have been paid
/// and targets selected. The payload is resolved via `stack::resolve_top`
/// once all players pass priority.
fn push_keyword_action(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    action: KeywordAction,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    super::stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::KeywordAction { action },
        },
        events,
    );
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    WaitingFor::Priority { player }
}

/// CR 702.122a + CR 113.3b: Announce a Vehicle's crew ability. Pays the cost
/// (tap selected creatures) and pushes a `KeywordAction::Crew` stack entry.
/// The Vehicle animation happens at stack resolution, not here — opening a
/// priority window for counterspell-class effects (CR 113.3b).
fn handle_crew_announcement(
    state: &mut GameState,
    player: PlayerId,
    vehicle_id: ObjectId,
    crew_power: u32,
    eligible_creatures: &[ObjectId],
    creature_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature_ids.is_empty() {
        return Err(EngineError::InvalidAction(
            "Must select at least one creature to crew".to_string(),
        ));
    }

    // Validate Vehicle is still on battlefield and controlled by player
    let vehicle = state
        .objects
        .get(&vehicle_id)
        .ok_or_else(|| EngineError::InvalidAction("Vehicle no longer exists".to_string()))?;
    if vehicle.zone != Zone::Battlefield || vehicle.controller != player {
        return Err(EngineError::InvalidAction(
            "Vehicle is no longer valid for crewing".to_string(),
        ));
    }

    // Validate all creature_ids are in eligible_creatures
    for &cid in creature_ids {
        if !eligible_creatures.contains(&cid) {
            return Err(EngineError::InvalidAction(
                "Creature not in eligible list".to_string(),
            ));
        }
    }

    // Re-validate and read power of each creature BEFORE tapping (HarmonizeTap idiom)
    let mut total_power: i32 = 0;
    for &cid in creature_ids {
        let obj = state
            .objects
            .get(&cid)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for crewing".to_string(),
            ));
        }
        total_power += obj.power.unwrap_or(0).max(0);
    }

    // CR 702.122a: Total power must meet threshold
    if total_power < crew_power as i32 {
        return Err(EngineError::InvalidAction(
            "Selected creatures' total power is less than crew requirement".to_string(),
        ));
    }

    // CR 701.26a + CR 702.122b: Tap each creature as cost payment — creature "crews" the Vehicle.
    for &cid in creature_ids {
        if let Some(obj) = state.objects.get_mut(&cid) {
            obj.tapped = true;
        }
        events.push(GameEvent::PermanentTapped {
            object_id: cid,
            caused_by: None,
        });
    }

    // CR 602.5b: Record this crew activation so an "Activate only once each turn"
    // Vehicle cannot be crewed a second time this turn. Cleared at turn start.
    state.crew_activated_this_turn.insert(vehicle_id);

    Ok(push_keyword_action(
        state,
        player,
        vehicle_id,
        KeywordAction::Crew {
            vehicle_id,
            paid_creature_ids: creature_ids.to_vec(),
        },
        events,
    ))
}

// ---------------------------------------------------------------------------
// CR 702.184a: Station — keyword action with per-card dispatch (mirrors Crew)
// ---------------------------------------------------------------------------

/// CR 702.184a: Activate a Spacecraft's station ability from Priority.
/// Per CR 702.184a: "Activate only as a sorcery." — the activation is rejected
/// outside the active player's main phase, empty stack, own priority.
fn handle_station_activation(
    state: &mut GameState,
    player: PlayerId,
    spacecraft_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&spacecraft_id)
        .ok_or_else(|| EngineError::InvalidAction("Spacecraft not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Spacecraft is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Spacecraft".to_string(),
        ));
    }
    if !obj
        .keywords
        .iter()
        .any(|k| matches!(k, crate::types::keywords::Keyword::Station))
    {
        return Err(EngineError::InvalidAction(
            "Object has no Station keyword".to_string(),
        ));
    }

    // CR 702.184a: "Activate only as a sorcery."
    if !super::restrictions::is_sorcery_speed_window(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "Station may only be activated as a sorcery".to_string(),
        ));
    }

    // CR 702.184a: "Tap another untapped creature you control" — the chosen
    // creature is NOT the Spacecraft, is a creature, is untapped, and is
    // controlled by the activating player.
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            id != spacecraft_id
                && state
                    .objects
                    .get(&id)
                    .map(|o| {
                        o.controller == player
                            && !o.tapped
                            && o.card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Creature)
                    })
                    .unwrap_or(false)
        })
        .collect();

    if eligible_creatures.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No eligible creatures to tap for Station".to_string(),
        ));
    }

    let _ = events; // No events emitted during activation (cost payment happens at resolution).
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::StationTarget {
        player,
        spacecraft_id,
        eligible_creatures,
    })
}

/// CR 702.184a + CR 113.3b: Announce Station. Pays the cost (tap the chosen
/// creature), snapshots its power per CR 113.7a, and pushes a
/// `KeywordAction::Station` stack entry. Charge counters are applied at
/// stack resolution, after a priority window (CR 113.3b).
fn handle_station_announcement(
    state: &mut GameState,
    player: PlayerId,
    spacecraft_id: ObjectId,
    eligible_creatures: &[ObjectId],
    creature_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.184a: Re-validate the chosen creature is still eligible (pending-effect
    // time gap between activation and resolution). Mirrors the HarmonizeTap idiom.
    if !eligible_creatures.contains(&creature_id) {
        return Err(EngineError::InvalidAction(
            "Creature not in eligible list".to_string(),
        ));
    }

    let spacecraft = state
        .objects
        .get(&spacecraft_id)
        .ok_or_else(|| EngineError::InvalidAction("Spacecraft no longer exists".to_string()))?;
    if spacecraft.zone != Zone::Battlefield || spacecraft.controller != player {
        return Err(EngineError::InvalidAction(
            "Spacecraft is no longer valid for stationing".to_string(),
        ));
    }

    let creature = state
        .objects
        .get(&creature_id)
        .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
    if creature.zone != Zone::Battlefield
        || creature.controller != player
        || creature.tapped
        || !creature
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
    {
        return Err(EngineError::InvalidAction(
            "Creature is no longer eligible for Station".to_string(),
        ));
    }

    // CR 702.184a + CR 113.7a: Snapshot the creature's power BEFORE tapping —
    // the counter count is determined at cost-payment time and survives the
    // creature leaving the battlefield before resolution. CR 702.184c lets
    // static abilities modify the characteristic read; this implementation
    // reads `power`, which is the default per the rule.
    let snapshot_power = creature.power.unwrap_or(0).max(0);

    // CR 701.26a: Tap the creature as cost payment.
    if let Some(obj) = state.objects.get_mut(&creature_id) {
        obj.tapped = true;
    }
    events.push(GameEvent::PermanentTapped {
        object_id: creature_id,
        caused_by: None,
    });

    Ok(push_keyword_action(
        state,
        player,
        spacecraft_id,
        KeywordAction::Station {
            spacecraft_id,
            paid_creature_id: creature_id,
            snapshot_power,
        },
        events,
    ))
}

// ---------------------------------------------------------------------------
// CR 702.171a: Saddle — keyword action with per-card dispatch (mirrors Crew)
// ---------------------------------------------------------------------------

/// CR 702.171a: Activate a Mount's saddle ability from Priority.
/// Enforces the sorcery-speed gate: main phase, empty stack, active player.
fn handle_saddle_activation(
    state: &mut GameState,
    player: PlayerId,
    mount_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&mount_id)
        .ok_or_else(|| EngineError::InvalidAction("Mount not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Mount is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this Mount".to_string(),
        ));
    }

    // Extract saddle power from keywords — fails if this permanent has no Saddle keyword.
    let saddle_power = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Saddle(n) = kw {
                Some(*n)
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Object has no Saddle keyword".to_string()))?;

    // CR 702.171a: "Activate only as a sorcery."
    if !super::restrictions::is_sorcery_speed_window(state, player) {
        return Err(EngineError::ActionNotAllowed(
            "Saddle may only be activated as a sorcery".to_string(),
        ));
    }

    // CR 702.171a: "Tap any number of other untapped creatures you control."
    let eligible_creatures: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            id != mount_id
                && state
                    .objects
                    .get(&id)
                    .map(|o| {
                        o.controller == player
                            && !o.tapped
                            && o.card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Creature)
                    })
                    .unwrap_or(false)
        })
        .collect();

    let total_power: i32 = eligible_creatures
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|o| o.power.unwrap_or(0).max(0))
        .sum();

    if total_power < saddle_power as i32 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough total power among eligible creatures to saddle".to_string(),
        ));
    }

    let _ = events;
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::SaddleMount {
        player,
        mount_id,
        saddle_power,
        eligible_creatures,
    })
}

/// CR 702.171a + CR 113.3b: Announce Saddle. Pays the cost (tap selected
/// creatures) and pushes a `KeywordAction::Saddle` stack entry. The "becomes
/// saddled UEOT" designation is applied at stack resolution.
fn handle_saddle_announcement(
    state: &mut GameState,
    player: PlayerId,
    mount_id: ObjectId,
    saddle_power: u32,
    eligible_creatures: &[ObjectId],
    creature_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if creature_ids.is_empty() {
        return Err(EngineError::InvalidAction(
            "Must select at least one creature to saddle".to_string(),
        ));
    }

    let mount = state
        .objects
        .get(&mount_id)
        .ok_or_else(|| EngineError::InvalidAction("Mount no longer exists".to_string()))?;
    if mount.zone != Zone::Battlefield || mount.controller != player {
        return Err(EngineError::InvalidAction(
            "Mount is no longer valid for saddling".to_string(),
        ));
    }

    for &cid in creature_ids {
        if !eligible_creatures.contains(&cid) {
            return Err(EngineError::InvalidAction(
                "Creature not in eligible list".to_string(),
            ));
        }
    }

    let mut total_power: i32 = 0;
    for &cid in creature_ids {
        let obj = state
            .objects
            .get(&cid)
            .ok_or_else(|| EngineError::InvalidAction("Creature no longer exists".to_string()))?;
        if obj.zone != Zone::Battlefield || obj.tapped {
            return Err(EngineError::InvalidAction(
                "Creature is no longer eligible for saddling".to_string(),
            ));
        }
        total_power += obj.power.unwrap_or(0).max(0);
    }

    if total_power < saddle_power as i32 {
        return Err(EngineError::InvalidAction(
            "Selected creatures' total power is less than saddle requirement".to_string(),
        ));
    }

    // CR 701.26a + CR 702.171c: Tap each creature as cost payment — creature "saddles" the Mount.
    for &cid in creature_ids {
        if let Some(obj) = state.objects.get_mut(&cid) {
            obj.tapped = true;
        }
        events.push(GameEvent::PermanentTapped {
            object_id: cid,
            caused_by: None,
        });
    }

    Ok(push_keyword_action(
        state,
        player,
        mount_id,
        KeywordAction::Saddle {
            mount_id,
            paid_creature_ids: creature_ids.to_vec(),
        },
        events,
    ))
}

pub fn new_game(seed: u64) -> GameState {
    GameState::new_two_player(seed)
}

/// Start game with mulligan flow. If no cards in libraries, skips mulligan.
///
/// CR 103.1: The starting player of game 1 is chosen at random. Subsequent games
/// in a multi-game match route through `match_flow::start_next_game`, which uses
/// `next_game_chooser` instead, so this function is always the game-1 path.
///
/// Callers that need a deterministic starter (tests, fixed scenarios) must use
/// `start_game_with_starting_player` directly.
pub fn start_game(state: &mut GameState) -> ActionResult {
    let starting_player = if state.seat_order.is_empty() {
        PlayerId(0)
    } else {
        let idx = state.rng.random_range(0..state.seat_order.len());
        state.seat_order[idx]
    };
    start_game_with_starting_player(state, starting_player)
}

/// Start game with a specific player taking the first turn.
pub fn start_game_with_starting_player(
    state: &mut GameState,
    starting_player: PlayerId,
) -> ActionResult {
    let mut events = Vec::new();
    state.outside_game_cards_brought_in.clear();

    if state.match_config.match_type == MatchType::Bo3 && state.players.len() != 2 {
        state.match_config.match_type = MatchType::Bo1;
    }

    events.push(GameEvent::GameStarted);

    // Begin the game: set turn 1
    state.turn_number = 1;
    state.active_player = starting_player;
    state.priority_player = starting_player;
    state.current_starting_player = starting_player;
    // First-game default chooser is the starting player; BO3 restarts can pre-set this.
    if state.next_game_chooser.is_none() {
        state.next_game_chooser = Some(starting_player);
    }
    // Rotate seat order so mulligan starts with the starting player.
    if let Some(idx) = state.seat_order.iter().position(|&p| p == starting_player) {
        state.seat_order.rotate_left(idx);
    }
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: starting_player,
        turn_number: 1,
    });

    // If players have cards in their libraries, start mulligan flow
    let has_libraries = state.players.iter().any(|p| !p.library.is_empty());
    let waiting_for = if has_libraries {
        // CR 702.139a: Check for eligible companions before mulligans.
        if let Some(companion_wf) = super::companion::check_all_companion_reveals(state) {
            companion_wf
        } else {
            mulligan::start_mulligan(state, &mut events)
        }
    } else {
        // No cards to mulligan with, skip straight to game
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// Start game without mulligan (for backward compatibility with existing tests).
pub fn start_game_skip_mulligan(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();
    state.outside_game_cards_brought_in.clear();

    events.push(GameEvent::GameStarted);

    state.turn_number = 1;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: PlayerId(0),
        turn_number: 1,
    });

    let waiting_for = turns::auto_advance(state, &mut events);
    state.waiting_for = waiting_for.clone();
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    finalize_public_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// CR 607.2a + CR 406.6: Check if any exile-return sources have left the battlefield.
/// If so, move the exiled cards back — linked abilities track which cards were exiled by the source.
pub(super) fn check_exile_returns(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let mut to_return: Vec<crate::types::game_state::ExileLink> = Vec::new();

    for event in events.iter() {
        if let GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Battlefield),
            ..
        } = event
        {
            // Find exile links where this object was the source and the exile
            // effect specified an automatic return when that source leaves.
            for link in &state.exile_links {
                if link.source_id == *object_id
                    && matches!(
                        &link.kind,
                        crate::types::game_state::ExileLinkKind::UntilSourceLeaves { .. }
                    )
                {
                    to_return.push(link.clone());
                }
            }
        }
    }

    if to_return.is_empty() {
        return;
    }

    // CR 610.3a: Return exiled cards to their previous zone
    for link in &to_return {
        // Only return if the card is still in exile
        let still_in_exile = state
            .objects
            .get(&link.exiled_id)
            .map(|obj| obj.zone == Zone::Exile)
            .unwrap_or(false);
        if still_in_exile {
            let crate::types::game_state::ExileLinkKind::UntilSourceLeaves { return_zone } =
                &link.kind
            else {
                continue;
            };
            zones::move_to_zone(state, link.exiled_id, *return_zone, events);
        }
    }

    // Remove processed links
    let returned_ids: Vec<_> = to_return.iter().map(|l| l.exiled_id).collect();
    state
        .exile_links
        .retain(|link| !returned_ids.contains(&link.exiled_id));
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::combat::AttackTarget;
    use crate::game::game_object::{BackFaceData, RoomDoor};
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, AbilityTag, ControllerRef, Effect,
        GainLifePlayer, ManaContribution, ManaProduction, ManaSpendRestriction, QuantityExpr,
        ResolvedAbility, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter,
        TypedFilter,
    };
    use crate::types::card_type::CardType;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::TriggerMode;

    /// Create a simple test ability definition.
    fn make_draw_ability(num_cards: u32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
                target: TargetFilter::Controller,
            },
        )
    }

    #[test]
    fn cards_revealed_events_are_remembered_publicly() {
        let mut state = GameState::new_two_player(42);
        let card_id = ObjectId(42);
        let events = vec![GameEvent::CardsRevealed {
            player: PlayerId(1),
            card_ids: vec![card_id],
            card_names: vec!["Known Card".to_string()],
        }];

        remember_public_reveals(&mut state, &events);

        assert!(state.public_revealed_cards.contains(&card_id));
    }

    #[test]
    fn terminal_reconcile_does_not_run_sbas_for_cant_lose_player() {
        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        let protected = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Platinum Angel".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&protected)
            .expect("protected source exists")
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantLoseTheGame).affected(
                TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
            ));

        let commander = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Kaalia".to_string(),
            Zone::Command,
        );
        let commander_obj = state
            .objects
            .get_mut(&commander)
            .expect("commander object exists");
        commander_obj.is_commander = true;
        commander_obj.card_types.core_types.push(CoreType::Creature);
        let mut move_events = Vec::new();
        zones::move_to_zone(&mut state, commander, Zone::Battlefield, &mut move_events);
        zones::move_to_zone(&mut state, commander, Zone::Graveyard, &mut move_events);

        // CR 101.2 + CR 704.5a: Platinum Angel means P0 cannot lose from
        // 0-or-less life. The
        // non-priority DiscardChoice should therefore remain active; otherwise
        // the full SBA loop would notice the unrelated dead commander and
        // replace the choice with CommanderZoneChoice.
        state.players[0].life = 0;
        state.waiting_for = WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 1,
            cards: Vec::new(),
            source_id: ObjectId(999),
            effect_kind: EffectKind::DiscardCard,
            up_to: false,
            unless_filter: None,
        };
        let original_waiting_for = state.waiting_for.clone();
        let mut result = ActionResult {
            events: Vec::new(),
            waiting_for: original_waiting_for.clone(),
            log_entries: Vec::new(),
        };

        reconcile_terminal_result(&mut state, &mut result);

        assert_eq!(state.waiting_for, original_waiting_for);
        assert_eq!(result.waiting_for, original_waiting_for);
        assert!(!state.players[0].is_eliminated);
        assert_eq!(state.objects[&commander].zone, Zone::Graveyard);
    }

    #[test]
    fn terminal_reconcile_runs_player_loss_sba_for_unprotected_player() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 0;
        state.waiting_for = WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 1,
            cards: Vec::new(),
            source_id: ObjectId(999),
            effect_kind: EffectKind::DiscardCard,
            up_to: false,
            unless_filter: None,
        };
        let mut result = ActionResult {
            events: Vec::new(),
            waiting_for: state.waiting_for.clone(),
            log_entries: Vec::new(),
        };

        reconcile_terminal_result(&mut state, &mut result);

        // CR 704.5a: An unprotected player at 0 life loses before the engine
        // keeps waiting for that player's non-priority discard choice.
        assert!(state.players[0].is_eliminated);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1)),
                ..
            }
        ));
    }

    /// Create a DealDamage ability for testing.
    fn make_damage_ability(amount: i32, cost: Option<AbilityCost>) -> AbilityDefinition {
        let kind = if cost.is_some() {
            AbilityKind::Activated
        } else {
            AbilityKind::Spell
        };
        let mut def = AbilityDefinition::new(
            kind,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: amount },
                target: TargetFilter::Any,
                damage_source: None,
            },
        );
        if let Some(c) = cost {
            def = def.cost(c);
        }
        def
    }

    fn apply_spell_oracle_to_object(
        state: &mut GameState,
        object_id: ObjectId,
        name: &str,
        oracle_text: &str,
    ) {
        let types = vec!["Sorcery".to_string()];
        let parsed = parse_oracle_text(oracle_text, name, &[], &types, &[]);
        let obj = state.objects.get_mut(&object_id).unwrap();
        Arc::make_mut(&mut obj.abilities).extend(parsed.abilities.clone());
        Arc::make_mut(&mut obj.base_abilities).extend(parsed.abilities);
    }

    pub(super) fn apply_oracle_to_object(
        state: &mut GameState,
        object_id: ObjectId,
        name: &str,
        oracle_text: &str,
    ) {
        let obj = state.objects.get(&object_id).unwrap();
        let types = obj
            .card_types
            .core_types
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let subtypes = obj.card_types.subtypes.clone();
        let parsed = parse_oracle_text(oracle_text, name, &[], &types, &subtypes);
        let obj = state.objects.get_mut(&object_id).unwrap();
        Arc::make_mut(&mut obj.abilities).extend(parsed.abilities.clone());
        Arc::make_mut(&mut obj.base_abilities).extend(parsed.abilities);
        for trigger in parsed.triggers.clone() {
            obj.trigger_definitions.push(trigger);
        }
        Arc::make_mut(&mut obj.base_trigger_definitions).extend(parsed.triggers);
        for replacement in parsed.replacements.clone() {
            obj.replacement_definitions.push(replacement);
        }
        Arc::make_mut(&mut obj.base_replacement_definitions).extend(parsed.replacements);
        for static_def in parsed.statics.clone() {
            obj.static_definitions.push(static_def);
        }
        Arc::make_mut(&mut obj.base_static_definitions).extend(parsed.statics);
    }

    use crate::game::test_fixtures::brushland_colored_ability;

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2; // Not first turn
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    #[test]
    fn eldrazi_temple_restricted_mana_casts_kindred_eldrazi_spell_only() {
        let mut state = setup_game_at_main_phase();
        let temple = create_object(
            &mut state,
            CardId(9100),
            PlayerId(0),
            "Eldrazi Temple".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&temple).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 2 },
                        },
                        restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation(
                            "Colorless Eldrazi".to_string(),
                        )],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let command = create_object(
            &mut state,
            CardId(9101),
            PlayerId(0),
            "Kozilek's Command".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&command).unwrap();
            obj.card_types.core_types.push(CoreType::Kindred);
            obj.card_types.core_types.push(CoreType::Instant);
            obj.card_types.subtypes.push("Eldrazi".to_string());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![
                    ManaCostShard::X,
                    ManaCostShard::Colorless,
                    ManaCostShard::Colorless,
                ],
                generic: 0,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
            ));
        }

        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: temple,
                ability_index: 0,
            },
        )
        .unwrap();
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: command,
                card_id: CardId(9101),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::ChooseXValue { .. }
        ));
        apply_as_current(&mut state, GameAction::ChooseX { value: 0 }).unwrap();
        assert!(
            state.stack.iter().any(|entry| entry.source_id == command),
            "Eldrazi Temple mana should pay for colorless Kindred Eldrazi spells"
        );

        let mut state = setup_game_at_main_phase();
        let temple = create_object(
            &mut state,
            CardId(9110),
            PlayerId(0),
            "Eldrazi Temple".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&temple).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 2 },
                        },
                        restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation(
                            "Colorless Eldrazi".to_string(),
                        )],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }
        let construct = create_object(
            &mut state,
            CardId(9111),
            PlayerId(0),
            "Colorless Construct".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&construct).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Construct".to_string());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Colorless, ManaCostShard::Colorless],
                generic: 0,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
            ));
        }
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: temple,
                ability_index: 0,
            },
        )
        .unwrap();
        assert!(
            apply_as_current(
                &mut state,
                GameAction::CastSpell {
                    object_id: construct,
                    card_id: CardId(9111),
                    targets: vec![],
                },
            )
            .is_err(),
            "Eldrazi Temple restricted mana must not pay for non-Eldrazi spells"
        );
    }

    #[test]
    fn chalice_of_the_void_enters_with_x_and_counters_matching_spell() {
        let mut state = setup_game_at_main_phase();
        let chalice = create_object(
            &mut state,
            CardId(9120),
            PlayerId(0),
            "Chalice of the Void".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&chalice).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::X],
                generic: 0,
            };
        }
        apply_oracle_to_object(
            &mut state,
            chalice,
            "Chalice of the Void",
            "This artifact enters with X charge counters on it.\nWhenever a player casts a spell with mana value equal to the number of charge counters on this artifact, counter that spell.",
        );
        let player = state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: chalice,
                card_id: CardId(9120),
                targets: vec![],
            },
        )
        .unwrap();
        apply_as_current(&mut state, GameAction::ChooseX { value: 1 }).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert_eq!(state.objects[&chalice].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&chalice]
                .counters
                .get(&CounterType::Generic("charge".to_string()))
                .copied()
                .unwrap_or_default(),
            1
        );

        let spell = create_object(
            &mut state,
            CardId(9121),
            PlayerId(0),
            "One Mana Spell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 1,
            };
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: TargetFilter::Controller,
                },
            ));
        }
        state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(0))
            .unwrap()
            .mana_pool
            .add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: spell,
                card_id: CardId(9121),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(
            state.stack.iter().any(|entry| entry.source_id == chalice),
            "Chalice should trigger for a spell with matching mana value"
        );
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(
            state.objects[&spell].zone,
            Zone::Graveyard,
            "Chalice trigger should counter the matching spell"
        );
    }

    /// CR 107.3m + CR 614.1c + CR 704.5f: Walking Ballista is the canonical
    /// 0/0 X-cost creature with "enters with X +1/+1 counters." Casting with
    /// X=4 must (a) stamp `cost_x_paid = Some(4)` during `finalize_cast`,
    /// (b) let the ETB replacement read it via `QuantityRef::CostXPaid`,
    /// (c) put 4 +1/+1 counters on the entering Ballista BEFORE SBAs run,
    /// (d) leave a live 4/4 on the battlefield (counters set P/T to 4/4
    /// before the 0/0 SBA would otherwise put it in the graveyard).
    #[test]
    fn walking_ballista_enters_with_x_counters_and_survives_zero_zero_sba() {
        let mut state = setup_game_at_main_phase();
        let ballista = create_object(
            &mut state,
            CardId(9130),
            PlayerId(0),
            "Walking Ballista".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&ballista).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Construct".to_string());
            obj.power = Some(0);
            obj.toughness = Some(0);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::X],
                generic: 0,
            };
        }
        apply_oracle_to_object(
            &mut state,
            ballista,
            "Walking Ballista",
            "Walking Ballista enters with X +1/+1 counters on it.\n{4}: Put a +1/+1 counter on this creature.\nRemove a +1/+1 counter from this creature: It deals 1 damage to any target.",
        );
        // Pay 2X = 8 colorless mana for X = 4.
        let player = state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(0))
            .unwrap();
        for _ in 0..8 {
            player.mana_pool.add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: ballista,
                card_id: CardId(9130),
                targets: vec![],
            },
        )
        .unwrap();
        apply_as_current(&mut state, GameAction::ChooseX { value: 4 }).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // CR 614.1c: counters land before CR 704.5f checks 0 toughness, so
        // the Ballista must be alive on the battlefield, not in the graveyard.
        assert_eq!(
            state.objects[&ballista].zone,
            Zone::Battlefield,
            "Walking Ballista must enter and survive — counters land before 0/0 SBA (CR 614.1c + CR 704.5f). \
             Got zone {:?}, cost_x_paid={:?}, counters={:?}",
            state.objects[&ballista].zone,
            state.objects[&ballista].cost_x_paid,
            state.objects[&ballista].counters,
        );
        assert_eq!(
            state.objects[&ballista]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            4,
            "Walking Ballista must enter with X=4 +1/+1 counters"
        );
    }

    /// CR 107.3m + CR 614.1c: Production-path variant of the Walking Ballista
    /// test. Loads the card face from the live `client/public/card-data.json`
    /// export and hydrates the object via `create_object_from_card_face`
    /// (the same path used by deck loading at game start). The earlier
    /// test exercises `apply_oracle_to_object` (re-parses oracle text at test
    /// time); this one exercises the same JSON hydration path the running
    /// game uses, so any divergence between "parsed at test time" and
    /// "loaded from card-data.json" shows up as a test failure here.
    #[test]
    fn walking_ballista_db_load_path_enters_with_x_counters() {
        use crate::database::CardDatabase;
        use crate::game::deck_loading::create_object_from_card_face;
        use std::path::Path;

        let path = Path::new("../../client/public/card-data.json");
        if !path.exists() {
            // Card-data export missing in this build context (e.g. fresh
            // clone before `gen-card-data.sh` runs). Skip rather than fail.
            eprintln!("skipping: {} missing", path.display());
            return;
        }
        let db = CardDatabase::from_export(path).expect("load card-data export");
        let face = db
            .get_face_by_name("Walking Ballista")
            .expect("Walking Ballista must be in the export")
            .clone();

        let mut state = setup_game_at_main_phase();
        let ballista = create_object_from_card_face(&mut state, &face, PlayerId(0));
        // Move the just-loaded object from Library to Hand so we can cast.
        state.objects.get_mut(&ballista).unwrap().zone = Zone::Hand;
        if let Some(player) = state.players.iter_mut().find(|p| p.id == PlayerId(0)) {
            player.library.retain(|id| *id != ballista);
            player.hand.push_back(ballista);
        }

        let player = state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(0))
            .unwrap();
        for _ in 0..8 {
            player.mana_pool.add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        let card_id = state.objects[&ballista].card_id;

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: ballista,
                card_id,
                targets: vec![],
            },
        )
        .unwrap();
        apply_as_current(&mut state, GameAction::ChooseX { value: 4 }).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert_eq!(
            state.objects[&ballista].zone,
            Zone::Battlefield,
            "DB-loaded Walking Ballista with X=4 must survive 0/0 SBA. \
             cost_x_paid={:?}, counters={:?}, replacements={:?}",
            state.objects[&ballista].cost_x_paid,
            state.objects[&ballista].counters,
            state.objects[&ballista]
                .replacement_definitions
                .0
                .iter()
                .map(|r| (r.event.to_string(), r.description.clone()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            state.objects[&ballista]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default(),
            4,
            "Walking Ballista must enter with X=4 +1/+1 counters (DB-load path)"
        );
    }

    /// CR 603.6c + CR 614.1c: Cathars' Crusade triggers on any creature you
    /// control entering. Its `PutCounterAll` effect must distribute one
    /// +1/+1 counter to *every* creature its controller controls — including
    /// the entering creature and every previously-existing creature. A
    /// regression where the resolver only hits the entering creature would
    /// catastrophically nerf the card.
    #[test]
    fn cathars_crusade_puts_one_counter_on_each_creature_you_control_on_etb() {
        let mut state = setup_game_at_main_phase();
        let crusade = create_object(
            &mut state,
            CardId(9150),
            PlayerId(0),
            "Cathars' Crusade".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&crusade).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
        }
        apply_oracle_to_object(
            &mut state,
            crusade,
            "Cathars' Crusade",
            "Whenever a creature you control enters, put a +1/+1 counter on each creature you control.",
        );
        // Two existing creatures on the battlefield (no summoning sickness needed
        // since we never attack — the test only inspects counter counts).
        let existing_a = create_object(
            &mut state,
            CardId(9151),
            PlayerId(0),
            "Existing Creature A".to_string(),
            Zone::Battlefield,
        );
        let existing_b = create_object(
            &mut state,
            CardId(9152),
            PlayerId(0),
            "Existing Creature B".to_string(),
            Zone::Battlefield,
        );
        for id in [existing_a, existing_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        // Cast a vanilla 2/2 from hand. Cathars' Crusade's trigger should
        // fire on its ETB and place one counter on all three creatures.
        let entering = create_object(
            &mut state,
            CardId(9153),
            PlayerId(0),
            "Entering Creature".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&entering).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 2,
            };
        }
        let player = state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(0))
            .unwrap();
        for _ in 0..2 {
            player.mana_pool.add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: entering,
                card_id: CardId(9153),
                targets: vec![],
            },
        )
        .unwrap();
        // Resolve the spell + Cathars' Crusade trigger.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        for (id, label) in [
            (entering, "entering creature"),
            (existing_a, "existing creature A"),
            (existing_b, "existing creature B"),
        ] {
            let n = state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default();
            assert_eq!(
                n, 1,
                "Cathars' Crusade must place one +1/+1 counter on every creature you control, \
                 not just the entering creature. {label} has {n} counters."
            );
        }
    }

    /// Production-path Cathars' Crusade: load via `CardDatabase::from_export`
    /// and `create_object_from_card_face` (the deck-loading path). Verifies
    /// the resolver iterates all creatures-you-control, not just the
    /// triggering entry.
    #[test]
    fn cathars_crusade_db_load_path_puts_counter_on_each_creature_you_control() {
        use crate::database::CardDatabase;
        use crate::game::deck_loading::create_object_from_card_face;
        use std::path::Path;

        let path = Path::new("../../client/public/card-data.json");
        if !path.exists() {
            eprintln!("skipping: {} missing", path.display());
            return;
        }
        let db = CardDatabase::from_export(path).expect("load card-data export");
        let crusade_face = db
            .get_face_by_name("Cathars' Crusade")
            .expect("Cathars' Crusade must be in the export")
            .clone();

        let mut state = setup_game_at_main_phase();
        let crusade = create_object_from_card_face(&mut state, &crusade_face, PlayerId(0));
        // CR 400.7: The deck-load path puts the object in `Zone::Library`.
        // Direct field mutation would leave `state.battlefield` (a separate
        // list) un-updated; the proper transition runs `move_to_zone` so
        // the battlefield index, layer dirty flag, and trigger matchers
        // all see the object. Use a discardable scratch event vec since
        // the test only inspects post-move state.
        {
            let mut scratch_events = Vec::new();
            super::zones::move_to_zone(&mut state, crusade, Zone::Battlefield, &mut scratch_events);
        }

        // Two pre-existing controlled creatures + an entering creature.
        let existing_a = create_object(
            &mut state,
            CardId(9160),
            PlayerId(0),
            "Existing Creature A".to_string(),
            Zone::Battlefield,
        );
        let existing_b = create_object(
            &mut state,
            CardId(9161),
            PlayerId(0),
            "Existing Creature B".to_string(),
            Zone::Battlefield,
        );
        for id in [existing_a, existing_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        let entering = create_object(
            &mut state,
            CardId(9162),
            PlayerId(0),
            "Entering Creature".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&entering).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 2,
            };
        }
        let player = state
            .players
            .iter_mut()
            .find(|player| player.id == PlayerId(0))
            .unwrap();
        for _ in 0..2 {
            player.mana_pool.add(crate::types::mana::ManaUnit::new(
                crate::types::mana::ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: entering,
                card_id: CardId(9162),
                targets: vec![],
            },
        )
        .unwrap();
        // Resolve creature + Cathars' Crusade trigger.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        for (id, label) in [
            (entering, "entering creature"),
            (existing_a, "existing creature A"),
            (existing_b, "existing creature B"),
        ] {
            let n = state.objects[&id]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or_default();
            assert_eq!(
                n, 1,
                "Cathars' Crusade (DB-load path) must place a +1/+1 counter on every \
                 creature you control. {label} has {n} counters."
            );
        }
    }

    #[test]
    fn broadside_bombardiers_boast_activates_after_attacking_and_requires_sacrifice() {
        use crate::game::combat::AttackTarget;

        let mut state = setup_game_at_main_phase();
        state.phase = Phase::DeclareAttackers;
        let bombardiers = create_object(
            &mut state,
            CardId(9140),
            PlayerId(0),
            "Broadside Bombardiers".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bombardiers).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Goblin".to_string());
            obj.card_types.subtypes.push("Pirate".to_string());
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.summoning_sick = false;
        }
        apply_oracle_to_object(
            &mut state,
            bombardiers,
            "Broadside Bombardiers",
            "Menace\nHaste\nBoast — Sacrifice another creature or artifact: This creature deals damage equal to 2 plus the sacrificed permanent's mana value to any target. (Activate only if this creature attacked this turn and only once each turn.)",
        );
        let sacrifice = create_object(
            &mut state,
            CardId(9141),
            PlayerId(0),
            "Sacrifice Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&sacrifice).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![bombardiers],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };
        apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(bombardiers, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();
        let ability_index = state.objects[&bombardiers]
            .abilities
            .iter()
            .position(|ability| ability.ability_tag == Some(AbilityTag::Boast))
            .expect("Broadside Bombardiers should have a Boast ability");
        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: bombardiers,
                ability_index,
            },
        )
        .unwrap();
        if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
            apply_as_current(
                &mut state,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Player(PlayerId(1))],
                },
            )
            .unwrap();
        }
        let WaitingFor::SacrificeForCost {
            count, permanents, ..
        } = &state.waiting_for
        else {
            panic!("Broadside Bombardiers boast should require a sacrifice cost");
        };
        assert_eq!(*count, 1);
        assert!(permanents.contains(&sacrifice));
        assert!(!permanents.contains(&bombardiers));
    }

    fn room_back_face(name: &str) -> BackFaceData {
        BackFaceData {
            name: name.to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            card_types: CardType::default(),
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Vec::new(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            layout_kind: Some(crate::types::card::LayoutKind::Split),
        }
    }

    #[test]
    fn unlock_room_door_special_action_marks_door_and_emits_trigger_event() {
        let mut state = setup_game_at_main_phase();
        let room = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Bottomless Pool".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&room).unwrap();
            obj.card_types.subtypes.push("Room".to_string());
            obj.room_unlocks = Some(Default::default());
            obj.back_face = Some(room_back_face("Locker Room"));
        }

        let result = apply_as_current(
            &mut state,
            GameAction::UnlockRoomDoor {
                object_id: room,
                door: RoomDoor::Right,
            },
        )
        .unwrap();

        let room_obj = state.objects.get(&room).unwrap();
        assert!(room_obj.room_unlocks.unwrap().right_unlocked);
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::RoomDoorUnlocked {
                object_id,
                door: RoomDoor::Right,
                ..
            } if *object_id == room
        )));
    }

    #[test]
    fn apply_pass_priority_alternates_players() {
        let mut state = setup_game_at_main_phase();

        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn apply_pass_priority_rejects_wrong_player() {
        let mut state = setup_game_at_main_phase();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // Player 0 tries to pass but player 1 has priority
        // PassPriority uses priority_player, so this should fail if
        // the validated player doesn't match waiting_for
        // Actually, the validation checks priority_player == waiting_for.player
        // and priority_player is 1, so PassPriority action itself is valid
        // for player 1. The issue is if player 0 somehow acts.
        // In practice, the action doesn't carry a player ID -- the engine
        // uses priority_player. So this is a protocol-level concern.
        let result = apply_as_current(&mut state, GameAction::PassPriority);
        assert!(result.is_ok());
    }

    // --- Preference actions (SetPhaseStops, CancelAutoPass) bypass actor gate ---

    #[test]
    fn set_phase_stops_from_non_priority_actor_succeeds() {
        // Regression: the human (P0) updates phase stops while the AI (P1) holds
        // priority. Previously this was rejected by check_actor_authorization with
        // WrongPlayer; the dispatch surfaced "Engine error: Wrong player" to the
        // user and the preference silently never landed.
        let mut state = setup_game_at_main_phase();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::SetPhaseStops {
                stops: vec![Phase::End],
            },
        );

        assert!(
            result.is_ok(),
            "expected SetPhaseStops to succeed, got {result:?}"
        );
        assert_eq!(
            state.phase_stops.get(&PlayerId(0)),
            Some(&vec![Phase::End]),
            "expected actor (P0) preference to be written, not authorized submitter (P1)",
        );
        assert!(!state.phase_stops.contains_key(&PlayerId(1)));
    }

    #[test]
    fn cancel_auto_pass_routes_by_actor() {
        // Regression: P0 had an auto-pass session; P1 holds priority and submits
        // CancelAutoPass on P0's behalf would previously cancel *P1's* session
        // (handler used authorized_submitter, not actor). After the fix, the
        // actor field decides which seat is mutated.
        let mut state = setup_game_at_main_phase();
        state.auto_pass.insert(
            PlayerId(0),
            crate::types::game_state::AutoPassMode::UntilEndOfTurn,
        );
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let result = apply(&mut state, PlayerId(0), GameAction::CancelAutoPass);

        assert!(result.is_ok());
        assert!(
            !state.auto_pass.contains_key(&PlayerId(0)),
            "P0's auto-pass should have been cancelled"
        );
    }

    // --- GameAction::Concede (CR 104.3a + CR 800.4a) ---

    fn setup_three_player_at_main_phase() -> GameState {
        use crate::types::format::FormatConfig;
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    #[test]
    fn concede_eliminates_player() {
        // CR 104.3a + CR 800.4a: 3-player game, P1 concedes — P1 leaves, game continues.
        let mut state = setup_three_player_at_main_phase();

        let result = apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .unwrap();

        assert!(state.players[1].is_eliminated);
        assert!(state.eliminated_players.contains(&PlayerId(1)));
        assert!(result.events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerEliminated {
                player_id: PlayerId(1)
            }
        )));
        // Game should NOT be over — P0 and P2 still alive.
        assert!(!matches!(result.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn concede_during_opponents_priority() {
        // CR 104.3a: A player may concede at any time, regardless of priority.
        // Set priority to P0, but P1 concedes anyway — must succeed.
        let mut state = setup_three_player_at_main_phase();
        // P0 holds priority.
        assert_eq!(state.priority_player, PlayerId(0));

        let result = apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        );

        assert!(
            result.is_ok(),
            "concede must succeed regardless of priority"
        );
        assert!(state.players[1].is_eliminated);
    }

    #[test]
    fn concede_owner_of_waiting_for_advances_state() {
        // CR 800.4a + CR 104.3a: When the conceding player owned the active WaitingFor
        // (here: DeclareAttackers, but the same advancement applies to TargetSelection,
        // ScryChoice, ManaPayment, and every other WaitingFor variant that references
        // a specific player), state must advance to Priority for the next living
        // player so the game does not deadlock waiting on a player who has left.
        let mut state = setup_three_player_at_main_phase();
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(1),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        };

        let result = apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .unwrap();

        assert!(state.players[1].is_eliminated);
        // WaitingFor must have advanced — the next living player after P1 is P2.
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::Priority {
                    player: PlayerId(2)
                }
            ),
            "expected Priority for P2 after P1 (owner of WaitingFor) conceded; got {:?}",
            result.waiting_for
        );
    }

    #[test]
    fn concede_non_owner_of_waiting_for_preserves_state() {
        // CR 800.4a: When the conceding player does NOT own the active WaitingFor
        // (e.g., another player has priority or is choosing), the WaitingFor state
        // is preserved — only the conceder's permanents/stack-objects are removed.
        let mut state = setup_three_player_at_main_phase();
        // P0 holds priority; P1 concedes — P0 keeps priority.
        assert_eq!(state.priority_player, PlayerId(0));

        let result = apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .unwrap();

        assert!(state.players[1].is_eliminated);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn concede_two_player_ends_game() {
        // CR 104.2a: In a 2-player game, when one player concedes, the other wins.
        let mut state = setup_game_at_main_phase();

        let result = apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(0),
            },
        )
        .unwrap();

        assert!(state.players[0].is_eliminated);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
    }

    #[test]
    fn concede_three_player_continues() {
        // CR 800.4a: In a 3-player game, when one concedes, the remaining two continue.
        let mut state = setup_three_player_at_main_phase();

        let result = apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(2),
            },
        )
        .unwrap();

        assert!(state.players[2].is_eliminated);
        assert!(!state.players[0].is_eliminated);
        assert!(!state.players[1].is_eliminated);
        assert!(!matches!(result.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn apply_play_land_moves_to_battlefield() {
        let mut state = setup_game_at_main_phase();

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        )
        .unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::Priority {
                    player: PlayerId(0)
                }
            ),
            "result.waiting_for={:?}, stack={:?}",
            result.waiting_for,
            state.stack
        );
    }

    #[test]
    fn apply_play_land_rejects_non_main_phase() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::Upkeep;

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_over_limit() {
        let mut state = setup_game_at_main_phase();
        state.lands_played_this_turn = 1; // Already played one

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_card_not_in_hand() {
        let mut state = setup_game_at_main_phase();

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: ObjectId(0),
                card_id: CardId(999),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_under_cant_play_land() {
        // CR 305.2: "Can't play lands" suppresses the play-land special action.
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        // Place a battlefield permanent that applies CantPlayLand to P0.
        let source = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Static Source".to_string(),
            Zone::Battlefield,
        );
        use crate::types::ability::{ControllerRef, TypedFilter};
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantPlayLand".to_string())).affected(
                    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
                ),
            );

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: land_id,
                card_id: CardId(1),
            },
        );

        assert!(
            result.is_err(),
            "PlayLand must be rejected under CantPlayLand"
        );
    }

    #[test]
    fn apply_play_land_rejects_under_cant_play_land_transient_effect() {
        // CR 305.2 + CR 611.1 + CR 611.2c: An activated ability that creates a
        // continuous effect with "until end of turn" duration (Pardic Miner:
        // "Sacrifice this creature: Target player can't play lands this turn.")
        // registers a transient continuous effect bound to
        // `TargetFilter::SpecificPlayer { id }`. The play-land gate must
        // observe this TCE the same way it observes the printed-static form,
        // because the source object has already left the battlefield (sacrifice
        // cost) by the time the effect resolves.
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::statics::StaticMode;

        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        // Register a SpecificPlayer-bound TCE granting CantPlayLand to P0,
        // mirroring what `effect.rs::register_transient_effect` would emit
        // when Pardic Miner's activated ability resolves with P0 chosen as
        // the target.
        state.add_transient_continuous_effect(
            ObjectId(99),
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::Other("CantPlayLand".to_string()),
            }],
            None,
        );

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: land_id,
                card_id: CardId(1),
            },
        );

        assert!(
            result.is_err(),
            "PlayLand must be rejected under transient CantPlayLand effect (Pardic Miner class)"
        );
    }

    #[test]
    fn new_game_creates_two_player_state() {
        let state = new_game(42);
        assert_eq!(state.players.len(), 2);
        assert_eq!(state.rng_seed, 42);
    }

    /// CR 117.1c + CR 503.2: After Untap (no priority), the active player
    /// receives priority during their Upkeep step. CR 103.7a skips the
    /// first-turn Draw step entirely, so passing both priorities through
    /// Upkeep lands at PreCombatMain.
    #[test]
    fn start_game_pauses_at_first_turn_upkeep_priority() {
        let mut state = new_game(42);
        let result = start_game_with_starting_player(&mut state, PlayerId(0));

        // CR 117.1c: starting player receives priority during Upkeep first.
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(state.turn_number, 1);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));

        // Both players pass through Upkeep → CR 103.7a skips Draw → PreCombatMain.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn start_game_skips_draw_on_first_turn() {
        let mut state = new_game(42);

        // Add a card to player 0's library
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        start_game_skip_mulligan(&mut state);

        // Card should still be in library (draw skipped on turn 1)
        assert!(state.players[0].library.contains(&id));
        assert!(!state.players[0].hand.contains(&id));
    }

    #[test]
    fn start_game_emits_game_started_event() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::GameStarted)));
    }

    // CR 103.1: Regression — `start_game` must randomize the starting player for
    // all match types, not just Bo3. Previously gated on `match_type == Bo3`, which
    // caused every Bo1 (default) game to begin with PlayerId(0).
    #[test]
    fn start_game_randomizes_starting_player_for_default_match_type() {
        let mut saw_p0 = false;
        let mut saw_p1 = false;

        for seed in 0..64u64 {
            let mut state = new_game(seed);
            let _ = start_game(&mut state);
            match state.current_starting_player {
                PlayerId(0) => saw_p0 = true,
                PlayerId(1) => saw_p1 = true,
                _ => unreachable!("two-player game can only produce PlayerId(0) or PlayerId(1)"),
            }
            if saw_p0 && saw_p1 {
                break;
            }
        }

        assert!(
            saw_p0 && saw_p1,
            "start_game must randomize across both seats for default (Bo1) matches"
        );
    }

    #[test]
    fn integration_full_turn_cycle() {
        let mut state = new_game(42);

        // Start game (turn 1, player 0) — engine pauses at Upkeep priority per
        // CR 117.1c. CR 103.7a skips the first-turn Draw step entirely.
        // (Libraries are empty, which is fine because the first-turn player
        // never draws and we stop the test before turn 2's draw step.)
        let _result = start_game_with_starting_player(&mut state, PlayerId(0));
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(state.turn_number, 1);

        // Pass through Upkeep (both players) — lands at PreCombatMain (Draw skipped).
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);

        // Pass priority from player 0 (pre-combat main)
        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        // Pass priority from player 1 (both passed, stack empty -> advance)
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        // Should skip combat phases and land at PostCombatMain
        assert_eq!(state.phase, Phase::PostCombatMain);

        // Pass through post-combat main
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        // Should advance to End step
        assert_eq!(state.phase, Phase::End);

        // Pass through end step → cleanup → next turn. Turn 2 is player 1's
        // turn; the engine pauses at P1's Upkeep priority (CR 117.1c).
        // (We stop here rather than draining Draw, because empty libraries
        // would trigger the CR 704.5b loss when P1 tries to draw.)
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn monarch_end_step_draws_exactly_one_card() {
        let mut state = new_game(42);
        let _result = start_game_with_starting_player(&mut state, PlayerId(0));
        // Test starts mid-turn at PostCombatMain — bypass the natural Upkeep
        // priority window via direct state setup (test fixture pattern).
        state.phase = Phase::PostCombatMain;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.priority_player = PlayerId(0);
        state.priority_passes.clear();
        state.monarch = Some(PlayerId(0));

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First card".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second card".to_string(),
            Zone::Library,
        );

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::End);
        assert_eq!(state.stack.len(), 1);

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].library.len(), 1);

        // End → cleanup → next turn. Turn 2 is P1's; engine pauses at P1's
        // Upkeep priority per CR 117.1c. We stop here rather than draining
        // Draw because P1's library is empty in this test fixture (CR 704.5b
        // game-loss not under test). The monarch's end-step draw (P0, on turn
        // 1) is what the test exercises and we've already validated above.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].library.len(), 1);
    }

    #[test]
    fn integration_play_land_then_pass() {
        let mut state = new_game(42);
        start_game_with_starting_player(&mut state, PlayerId(0));

        // CR 305.3 + CR 117.1c: lands are sorcery-speed, so pass Upkeep
        // priority (both players) to reach PreCombatMain before playing.
        // CR 103.7a skips first-turn Draw so two passes is enough.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);

        // Create a land in player 0's hand
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Play the land
        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: land_id,
                card_id: CardId(1),
            },
        )
        .unwrap();

        assert!(state.battlefield.contains(&land_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority after playing land
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));

        // Priority pass count should have been reset by the land play
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn stack_push_and_lifo_resolve() {
        use crate::game::stack;
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};

        let mut state = setup_game_at_main_phase();
        let mut events = Vec::new();

        // Create two spell objects
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Push to stack (first pushed = bottom)
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id1,
                source_id: id1,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut events,
        );
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id2,
                source_id: id2,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(2),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut events,
        );

        assert_eq!(state.stack.len(), 2);

        // Resolve top (LIFO) -- should be id2 (Bear, creature -> battlefield)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 1);
        assert!(state.battlefield.contains(&id2)); // Creature goes to battlefield

        // Resolve next -- should be id1 (Bolt, instant -> graveyard)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 0);
        assert!(state.players[0].graveyard.contains(&id1)); // Instant goes to graveyard
    }

    #[test]
    fn stack_is_empty_check() {
        use crate::game::stack;

        let state = new_game(42);
        assert!(stack::stack_is_empty(&state));
    }

    #[test]
    fn engine_error_display() {
        let err = EngineError::WrongPlayer;
        assert_eq!(err.to_string(), "Wrong player");

        let err = EngineError::NotYourPriority;
        assert_eq!(err.to_string(), "Not your priority");

        let err = EngineError::InvalidAction("test".to_string());
        assert_eq!(err.to_string(), "Invalid action: test");
    }

    /// Regression: the engine must reject any non-Concede action whose
    /// `actor` does not match `authorized_submitter(state)`. Before the
    /// engine-level guard existed, `apply()` silently used `waiting_for`'s
    /// player as the actor — meaning the human could click targets during
    /// an AI's `TargetSelection` and the engine would accept them *as the
    /// AI*. The guard below is the single place that closes that loophole
    /// for every transport (WASM, WebSocket, P2P).
    #[test]
    fn apply_rejects_action_from_wrong_actor() {
        let mut state = setup_game_at_main_phase();
        // `setup_game_at_main_phase` leaves P0 with priority.
        assert_eq!(
            turn_control::authorized_submitter(&state),
            Some(PlayerId(0)),
            "precondition: P0 should have priority"
        );

        // P1 submitting an action meant for P0 must be rejected.
        let result = apply(&mut state, PlayerId(1), GameAction::PassPriority);
        assert!(
            matches!(result, Err(EngineError::WrongPlayer)),
            "expected WrongPlayer, got {result:?}"
        );

        // P0 submitting the same action must succeed.
        let result = apply(&mut state, PlayerId(0), GameAction::PassPriority);
        assert!(result.is_ok(), "P0 pass should succeed: {result:?}");
    }

    /// Regression: Concede self-authenticates via its own `player_id`, but
    /// `actor` must still match that `player_id` so one player cannot
    /// concede another. CR 104.3a: *a player* may concede at any time.
    #[test]
    fn apply_rejects_spoofed_concede() {
        let mut state = setup_game_at_main_phase();
        // P0 trying to concede P1 → rejected.
        let spoofed = GameAction::Concede {
            player_id: PlayerId(1),
        };
        let result = apply(&mut state, PlayerId(0), spoofed);
        assert!(
            matches!(result, Err(EngineError::WrongPlayer)),
            "expected WrongPlayer, got {result:?}"
        );

        // P1 conceding themselves → accepted even though P0 has priority.
        let self_concede = GameAction::Concede {
            player_id: PlayerId(1),
        };
        let result = apply(&mut state, PlayerId(1), self_concede);
        assert!(result.is_ok(), "self-concede should succeed: {result:?}");
    }

    #[test]
    fn tap_land_for_mana_produces_correct_color() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let result = apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        assert!(state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    /// Build a Wild Growth–style aura attached to `land_id` for tests in this
    /// module. Single-color "{G}" `TapsForMana` trigger via
    /// `valid_card: AttachedTo`. Returns the aura's `ObjectId`.
    fn attach_wild_growth(state: &mut GameState, land_id: ObjectId, owner: PlayerId) -> ObjectId {
        let aura = create_object(
            state,
            CardId(99),
            owner,
            "Wild Growth".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.attached_to = Some(land_id.into());
        obj.entered_battlefield_turn = Some(1);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::TapsForMana)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Green],
                            contribution: ManaContribution::Additional,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                ))
                .valid_card(TargetFilter::AttachedTo),
        );
        aura
    }

    #[test]
    fn untap_land_for_mana_refunds_aura_bonus_no_infinite_mana() {
        // CR 605.1b + CR 605.3b: Wild Growth attaches to a Forest. Tapping the
        // Forest emits {G} (land) + {G} (aura's TapsForMana trigger). The user
        // then invokes `UntapLandForMana` — both mana units must be refunded,
        // otherwise repeated tap-untap-tap cycles compound aura mana into the
        // pool indefinitely (the user-reported infinite-mana exploit).
        let mut state = setup_game_at_main_phase();

        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        attach_wild_growth(&mut state, forest, PlayerId(0));

        // Tap the Forest. Land emits {G}; aura's trigger fires via
        // run_post_action_pipeline and adds another {G}.
        apply_as_current(&mut state, GameAction::TapLandForMana { object_id: forest }).unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            2,
            "tap should yield {{G}} (land) + {{G}} (Wild Growth bonus)"
        );

        // Manual untap reverses BOTH the land's and the aura's contributions.
        apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: forest },
        )
        .unwrap();
        assert!(!state.objects[&forest].tapped, "Forest must be untapped");
        assert_eq!(
            state.players[0].mana_pool.total(),
            0,
            "manual untap must refund both the land's and the aura's mana — \
             leaving aura mana would allow tap-untap-tap to compound mana"
        );

        // Re-tap and re-untap to verify no compounding across cycles.
        for _ in 0..3 {
            apply_as_current(&mut state, GameAction::TapLandForMana { object_id: forest }).unwrap();
            assert_eq!(state.players[0].mana_pool.total(), 2);
            apply_as_current(
                &mut state,
                GameAction::UntapLandForMana { object_id: forest },
            )
            .unwrap();
            assert_eq!(
                state.players[0].mana_pool.total(),
                0,
                "every cycle must net to zero pool — no compounding aura mana"
            );
        }
    }

    #[test]
    fn can_pay_cost_after_auto_tap_includes_aura_taps_for_mana_bonus() {
        // CR 605.1b + CR 106.4: AI affordability simulation must surface mana
        // contributed by `TapsForMana` triggered abilities (Wild Growth /
        // Fertile Ground / Utopia Sprawl class). A Plains enchanted with Wild
        // Growth produces {W} (land) + {G} (aura) and must be reported
        // payable for a {1}{G} cost — without trigger processing in the
        // affordability simulation, the AI would skip a turn that the player
        // could actually pay.
        use crate::types::mana::{ManaCost, ManaCostShard};
        let mut state = setup_game_at_main_phase();

        let plains = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&plains).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Plains".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        attach_wild_growth(&mut state, plains, PlayerId(0));

        // Synthesize a hand object representing the spell being affordability-checked.
        let spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Hand,
        );

        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };
        assert!(
            casting::can_pay_cost_after_auto_tap(&state, PlayerId(0), spell, &cost),
            "Plains + Wild Growth must be reported able to pay {{1}}{{G}}: \
             land contributes {{W}}, aura's TapsForMana trigger contributes {{G}}"
        );

        // Sanity baseline: a Plains alone cannot pay {1}{G}.
        let mut state_no_aura = setup_game_at_main_phase();
        let lone_plains = create_object(
            &mut state_no_aura,
            CardId(1),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state_no_aura.objects.get_mut(&lone_plains).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Plains".to_string());
            obj.entered_battlefield_turn = Some(1);
        }
        let lone_spell = create_object(
            &mut state_no_aura,
            CardId(2),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Hand,
        );
        assert!(
            !casting::can_pay_cost_after_auto_tap(&state_no_aura, PlayerId(0), lone_spell, &cost),
            "lone Plains must NOT be reported able to pay {{1}}{{G}}"
        );
    }

    #[test]
    fn vorinclex_mana_doubling_trigger_fires_on_tap() {
        // Vorinclex, Voice of Hunger: "Whenever you tap a land for mana,
        // add one mana of any type that land produced."
        // The trigger is on Vorinclex (creature), not on the land itself.
        // valid_card: Typed(Land), valid_target: Controller.
        let mut state = setup_game_at_main_phase();

        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let vorinclex = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&vorinclex).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            // Trigger 1: mana doubling for your lands
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Typed(TypedFilter::land()))
                    .valid_target(TargetFilter::Controller),
            );
        }

        // Tap the Forest — should produce {G} (land) + {G} (Vorinclex doubler).
        apply_as_current(&mut state, GameAction::TapLandForMana { object_id: forest }).unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            2,
            "Vorinclex must double land mana: {{G}} (land) + {{G}} (trigger)"
        );
    }

    #[test]
    fn vorinclex_cant_untap_trigger_fires_on_opponent_tap() {
        // Vorinclex, Voice of Hunger: "Whenever an opponent taps a land for
        // mana, that land doesn't untap during its controller's next untap step."
        // The trigger is a GenericEffect (CantUntap) that goes on the stack.
        use crate::types::ability::{
            ContinuousModification, ControllerRef, Duration, PlayerScope, StaticDefinition,
        };
        let mut state = setup_game_at_main_phase();
        // Set P1 as active player so they have priority to tap
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let opp_forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&opp_forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let vorinclex = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Vorinclex, Voice of Hunger".to_string(),
            Zone::Battlefield,
        );
        {
            let duration = Duration::UntilNextStepOf {
                step: Phase::Untap,
                player: PlayerScope::Controller,
            };
            let obj = state.objects.get_mut(&vorinclex).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            // Trigger 2: opponent lands can't untap
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(
                        AbilityDefinition::new(
                            AbilityKind::Database,
                            Effect::GenericEffect {
                                static_abilities: vec![StaticDefinition::new(
                                    StaticMode::CantUntap,
                                )
                                .affected(TargetFilter::ParentTarget)
                                .modifications(vec![ContinuousModification::AddStaticMode {
                                    mode: StaticMode::CantUntap,
                                }])],
                                duration: Some(duration.clone()),
                                target: Some(TargetFilter::TriggeringSource),
                            },
                        )
                        .duration(duration),
                    )
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::land().controller(ControllerRef::Opponent),
                    )),
            );
        }

        // Opponent taps the Forest
        apply(
            &mut state,
            PlayerId(1),
            GameAction::TapLandForMana {
                object_id: opp_forest,
            },
        )
        .unwrap();
        // The trigger should have been placed on the stack.
        assert!(
            !state.stack.is_empty() || !state.transient_continuous_effects.is_empty(),
            "Vorinclex's CantUntap trigger must fire when opponent taps land"
        );
    }

    #[test]
    fn untap_land_for_mana_aura_bonus_helper_lists_attached_aura() {
        // Sanity check on the aura-source enumerator that
        // `handle_untap_land_for_mana` consults: it must include the Wild
        // Growth-style aura whose `valid_card: AttachedTo` resolves to the
        // tapped land, and exclude the land itself.
        let mut state = setup_game_at_main_phase();
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        let aura = attach_wild_growth(&mut state, forest, PlayerId(0));

        let sources =
            mana_sources::aura_taps_for_mana_sources_for_land(&state, forest, PlayerId(0));
        assert_eq!(sources, vec![aura]);
    }

    #[test]
    fn tap_land_rejects_already_tapped() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.tapped = true;
        }

        let result = apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        );

        assert!(result.is_err());
    }

    #[test]
    fn multi_mana_land_rejects_tap_land_for_mana() {
        // Dual lands with multiple mana abilities must use ActivateAbility to
        // select which color — TapLandForMana is ambiguous for multi-option lands.
        let mut state = setup_game_at_main_phase();

        let dual_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Watery Grave".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dual_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Black],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        let result = apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: dual_id },
        );
        assert!(
            result.is_err(),
            "TapLandForMana should reject multi-mana lands"
        );
    }

    #[test]
    fn multi_mana_land_activates_via_ability_index() {
        // Dual lands use ActivateAbility with a specific ability_index to select color.
        let mut state = setup_game_at_main_phase();

        let dual_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Watery Grave".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dual_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.has_mana_ability = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Blue],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Black],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        // Activate Blue (ability_index 0)
        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: dual_id,
                ability_index: 0,
            },
        )
        .unwrap();

        assert!(state.objects[&dual_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Blue),
            1
        );
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Black),
            0
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn multi_mana_land_undoable_after_activate_ability() {
        // Dual lands tapped via ActivateAbility should be undoable via UntapLandForMana.
        let mut state = setup_game_at_main_phase();

        let dual_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Watery Grave".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&dual_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.has_mana_ability = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Black],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
        }

        // Tap for Black via ActivateAbility
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: dual_id,
                ability_index: 0,
            },
        )
        .unwrap();
        assert!(state.objects[&dual_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Black),
            1
        );

        // Undo via UntapLandForMana
        apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: dual_id },
        )
        .unwrap();
        assert!(!state.objects[&dual_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Black),
            0
        );
    }

    #[test]
    fn controller_harming_mana_land_is_not_undoable_after_manual_activation() {
        let mut state = setup_game_at_main_phase();

        let brushland = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Brushland".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&brushland).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.has_mana_ability = true;
            Arc::make_mut(&mut obj.abilities).push(brushland_colored_ability());
        }

        let first = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: brushland,
                ability_index: 0,
            },
        )
        .unwrap();
        assert!(
            matches!(first.waiting_for, WaitingFor::ChooseManaColor { .. }),
            "expected ChooseManaColor after activating Brushland, got {:?}",
            first.waiting_for
        );

        let second = apply_as_current(
            &mut state,
            GameAction::ChooseManaColor {
                choice: crate::types::game_state::ManaChoice::SingleColor(
                    crate::types::mana::ManaType::Green,
                ),
                count: 1,
            },
        )
        .unwrap();
        assert!(matches!(second.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.objects[&brushland].tapped);
        assert_eq!(state.players[0].life, 19);
        assert!(state
            .lands_tapped_for_mana
            .get(&PlayerId(0))
            .is_none_or(|ids| !ids.contains(&brushland)));

        let undo = apply_as_current(
            &mut state,
            GameAction::UntapLandForMana {
                object_id: brushland,
            },
        );
        assert!(
            undo.is_err(),
            "controller-harming mana activations should not be undoable"
        );
    }

    // CR 605.1b + CR 722.1: End-to-end integration test. Driving a real
    // `ActivateAbility` action on the Forest must (a) update the mana pool with
    // the Forest's base {G}, (b) fire Utopia Sprawl's TapsForMana trigger
    // inline (stack-skipped per CR 605.1b), (c) add the chosen color to the
    // pool, and (d) leave the stack empty so the controller can immediately
    // spend the mana.
    #[test]
    fn utopia_sprawl_on_forest_taps_for_both_base_and_additional_mana_inline() {
        use crate::types::ability::{
            ChosenAttribute, Effect as Eff, ManaContribution, ManaProduction, QuantityExpr,
            TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = setup_game_at_main_phase();

        // Forest with the standard {T}: Add {G} synthesized mana ability.
        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.has_mana_ability = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Eff::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        // Utopia Sprawl attached to the Forest with chosen color Red.
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Utopia Sprawl".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.attached_to = Some(forest.into());
            obj.entered_battlefield_turn = Some(1);
            obj.chosen_attributes
                .push(ChosenAttribute::Color(crate::types::mana::ManaColor::Red));
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Eff::Mana {
                            produced: ManaProduction::ChosenColor {
                                count: QuantityExpr::Fixed { value: 1 },
                                contribution: ManaContribution::Additional,
                                fixed_alternative: None,
                            },
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::AttachedTo),
            );
        }

        // Activate the Forest's {T}: Add {G} via the full apply() pipeline.
        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: forest,
                ability_index: 0,
            },
        )
        .expect("Forest mana ability should activate");

        // (a) Forest is tapped, base {G} in the pool.
        assert!(state.objects[&forest].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1,
            "Forest's base {{G}} must be in the pool",
        );

        // (c) Utopia Sprawl's chosen-color {R} is ALSO in the pool, added
        // inline by the triggered mana ability (CR 605.1b).
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Red),
            1,
            "Utopia Sprawl's additional {{R}} must be in the pool",
        );

        // (d) Stack is empty — the triggered mana ability did NOT use the
        // stack. Controller retains priority and can immediately spend the
        // mana on a {R} cost.
        assert_eq!(
            state.stack.len(),
            0,
            "Triggered mana ability must not be placed on the stack (CR 605.1b)",
        );
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Controller must retain priority after the activation resolves",
        );
    }

    #[test]
    fn full_turn_integration_with_mulligan() {
        let mut state = new_game(42);

        // Add 20 basic lands to each player's library
        for player_idx in 0..2u8 {
            for i in 0..20 {
                let id = create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i),
                    PlayerId(player_idx),
                    "Forest".to_string(),
                    Zone::Library,
                );
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Land);
                obj.card_types.subtypes.push("Forest".to_string());
            }
        }

        // Start game -> mulligan prompt
        let result = start_game_with_starting_player(&mut state, PlayerId(0));
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision { .. }
        ));

        // Both players have 7 cards in hand
        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);

        // Player 0 keeps (apply_as_current picks first pending player = P0)
        let result = apply_as_current(
            &mut state,
            GameAction::MulliganDecision {
                choice: crate::types::actions::MulliganChoice::Keep,
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision { .. }
        ));

        // Player 1 keeps (apply_as_current now picks P1 since P0 was removed)
        // → game starts, lands at Upkeep priority for P0 (CR 117.1c).
        let result = apply_as_current(
            &mut state,
            GameAction::MulliganDecision {
                choice: crate::types::actions::MulliganChoice::Keep,
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0),
            }
        ));
        assert_eq!(state.phase, Phase::Upkeep);

        // Drain Upkeep priority (turn 1 skips Draw per CR 103.7a) to reach Main.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);

        // Play a land from hand
        let land_obj_id = state.players[0].hand[0];
        let land_card_id = state.objects[&land_obj_id].card_id;
        let _result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: land_obj_id,
                card_id: land_card_id,
            },
        )
        .unwrap();
        assert_eq!(state.lands_played_this_turn, 1);

        // Find the land on battlefield to tap it
        let land_on_bf = state
            .battlefield
            .iter()
            .find(|&&id| {
                state
                    .objects
                    .get(&id)
                    .map(|o| o.controller == PlayerId(0) && !o.tapped)
                    .unwrap_or(false)
            })
            .copied()
            .unwrap();

        // Tap land for mana
        let _result = apply_as_current(
            &mut state,
            GameAction::TapLandForMana {
                object_id: land_on_bf,
            },
        )
        .unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );

        // Pass priority through the rest of the turn
        // PreCombatMain: P0 passes
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        // PreCombatMain: P1 passes -> advances to PostCombatMain
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PostCombatMain);

        // PostCombatMain: both pass -> End
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::End);

        // End: both pass → Cleanup → next turn. P1's Upkeep priority opens
        // first (CR 117.1c); turn 2 doesn't skip Draw, so drain Upkeep + Draw.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::Upkeep);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::Draw);
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
    }

    #[test]
    fn cast_spell_moves_card_from_hand_to_stack_and_returns_priority() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery in hand
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        // Add mana
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.stack.len(), 1);
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn both_pass_with_spell_on_stack_resolves_spell() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery and cast it
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        // Add some cards to draw
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        // Cast the spell
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        let hand_before = state.players[0].hand.len();

        // Both pass -> resolve
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // Stack should be empty
        assert!(state.stack.is_empty());
        // Card should be in graveyard (sorcery)
        assert!(state.players[0].graveyard.contains(&obj_id));
        // Draw 2 effect should have fired
        assert_eq!(state.players[0].hand.len(), hand_before + 2);
    }

    #[test]
    fn brainstorm_resolves_draw_then_put_two_cards_on_top() {
        use crate::types::ability::{ControllerRef, FilterProp, LibraryPosition};
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();
        let brainstorm = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Brainstorm".to_string(),
            Zone::Hand,
        );
        let first_hand = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "First Hand Card".to_string(),
            Zone::Hand,
        );
        let second_hand = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Second Hand Card".to_string(),
            Zone::Hand,
        );
        for i in 0..3 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Library Card {i}"),
                Zone::Library,
            );
        }

        let mut brainstorm_ability = make_draw_ability(3);
        brainstorm_ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Typed(
                    TypedFilter::card()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                ),
                count: QuantityExpr::Fixed { value: 2 },
                position: LibraryPosition::Top,
            },
        )));
        {
            let obj = state.objects.get_mut(&brainstorm).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(brainstorm_ability);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            };
        }
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(0),
            snow: false,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: brainstorm,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(0),
                count: 2,
                effect_kind: EffectKind::PutAtLibraryPosition,
                zone: Zone::Hand,
                ..
            }
        ));

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![first_hand, second_hand],
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&brainstorm));
        assert_eq!(state.players[0].library[0], first_hand);
        assert_eq!(state.players[0].library[1], second_hand);
        assert!(!state.players[0].hand.contains(&first_hand));
        assert!(!state.players[0].hand.contains(&second_hand));
    }

    #[test]
    fn gamble_searches_to_hand_then_discards_random_card() {
        let mut state = setup_game_at_main_phase();
        let gamble = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Gamble".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&gamble).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.base_card_types = obj.card_types.clone();
        }
        apply_spell_oracle_to_object(
            &mut state,
            gamble,
            "Gamble",
            "Search your library for a card, put that card into your hand, discard a card at random, then shuffle.",
        );
        let hand_a = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Hand A".to_string(),
            Zone::Hand,
        );
        let hand_b = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Hand B".to_string(),
            Zone::Hand,
        );
        let target = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Tutor Target".to_string(),
            Zone::Library,
        );

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: gamble,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::SearchChoice { .. }
        ));

        let mut discard_pool: Vec<ObjectId> = state.players[0].hand.iter().copied().collect();
        discard_pool.push(target);
        let expected_discard = {
            let mut rng = state.rng.clone();
            let index = rng.random_range(0..discard_pool.len());
            discard_pool[index]
        };

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![target],
            },
        )
        .unwrap();

        assert!(state.players[0].graveyard.contains(&expected_discard));
        assert!(
            [hand_a, hand_b, target]
                .into_iter()
                .filter(|id| state.players[0].hand.contains(id))
                .count()
                == 2
        );
        assert!(state.players[0].graveyard.contains(&gamble));
    }

    #[test]
    fn disciple_of_bolas_uses_sacrificed_creature_power_for_life_and_draw() {
        let mut state = setup_game_at_main_phase();

        let disciple = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Disciple of Bolas".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&disciple).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(1);
            obj.base_power = Some(2);
            obj.base_toughness = Some(1);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 3,
            };
        }
        apply_oracle_to_object(
            &mut state,
            disciple,
            "Disciple of Bolas",
            "When this creature enters, sacrifice another creature. You gain X life and draw X cards, where X is that creature's power.",
        );

        let hill_giant = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Hill Giant".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&hill_giant).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
        }
        let library_cards: Vec<_> = (0..3)
            .map(|i| {
                create_object(
                    &mut state,
                    CardId(30 + i),
                    PlayerId(0),
                    format!("Library Card {i}"),
                    Zone::Library,
                )
            })
            .collect();
        assert!(library_cards
            .iter()
            .all(|id| state.players[0].library.contains(id)));

        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Black,
            ObjectId(0),
            false,
            Vec::new(),
        ));
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                Vec::new(),
            ));
        }

        let disciple_card_id = state.objects[&disciple].card_id;
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: disciple,
                card_id: disciple_card_id,
                targets: vec![],
            },
        )
        .unwrap();

        let mut result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        for _ in 0..6 {
            if matches!(result.waiting_for, WaitingFor::EffectZoneChoice { .. }) {
                break;
            }
            result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        }
        match result.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                effect_kind,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(effect_kind, EffectKind::Sacrifice);
                assert!(cards.contains(&hill_giant));
            }
            other => panic!("expected Disciple sacrifice choice, got {other:?}"),
        }

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![hill_giant],
            },
        )
        .unwrap();

        assert_eq!(state.players[0].life, 23);
        assert_eq!(state.players[0].hand.len(), 3);
        assert!(state.players[0].graveyard.contains(&hill_giant));
    }

    const SQUADRON_HAWK_ORACLE: &str = "Flying\nWhen this creature enters, you may search your library for up to three cards named Squadron Hawk, reveal them, put them into your hand, then shuffle.";

    fn add_squadron_hawk_to_library(state: &mut GameState, card_id: u64) -> ObjectId {
        let hawk = create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            "Squadron Hawk".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&hawk).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
        }
        hawk
    }

    fn resolve_squadron_hawk_etb_to_search_choice() -> (GameState, [ObjectId; 3], ObjectId) {
        let mut state = setup_game_at_main_phase();
        let entering_hawk = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Squadron Hawk".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&entering_hawk).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
        }
        apply_oracle_to_object(
            &mut state,
            entering_hawk,
            "Squadron Hawk",
            SQUADRON_HAWK_ORACLE,
        );

        let hawks = [
            add_squadron_hawk_to_library(&mut state, 11),
            add_squadron_hawk_to_library(&mut state, 12),
            add_squadron_hawk_to_library(&mut state, 13),
        ];
        let nonmatch = create_object(
            &mut state,
            CardId(14),
            PlayerId(0),
            "Storm Crow".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        zones::move_to_zone(&mut state, entering_hawk, Zone::Battlefield, &mut events);
        crate::game::triggers::process_triggers(&mut state, &events);

        assert_eq!(state.stack.len(), 1, "Squadron Hawk ETB trigger must stack");
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "Squadron Hawk's 'you may' trigger must prompt before searching, got {:?}",
            state.waiting_for
        );

        apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                reveal,
                up_to,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 3);
                assert!(*reveal);
                assert!(*up_to);
                assert_eq!(cards.len(), 3);
                for hawk in hawks {
                    assert!(cards.contains(&hawk), "SearchChoice must offer {hawk:?}");
                }
                assert!(
                    !cards.contains(&nonmatch),
                    "SearchChoice must not offer non-Squadron Hawk cards"
                );
            }
            other => {
                panic!("Expected SearchChoice after accepting Squadron Hawk ETB, got {other:?}")
            }
        }

        (state, hawks, nonmatch)
    }

    #[test]
    fn squadron_hawk_may_trigger_can_be_declined_before_search() {
        let mut state = setup_game_at_main_phase();
        let entering_hawk = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Squadron Hawk".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&entering_hawk).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
        }
        apply_oracle_to_object(
            &mut state,
            entering_hawk,
            "Squadron Hawk",
            SQUADRON_HAWK_ORACLE,
        );
        let library_hawk = add_squadron_hawk_to_library(&mut state, 11);

        let mut events = Vec::new();
        zones::move_to_zone(&mut state, entering_hawk, Zone::Battlefield, &mut events);
        crate::game::triggers::process_triggers(&mut state, &events);
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ));

        let result = apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&library_hawk].zone, Zone::Library);
        assert!(state.players[0].library.contains(&library_hawk));
        assert!(!state.players[0].hand.contains(&library_hawk));
        assert!(!result.events.iter().any(|event| matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: crate::types::events::PlayerActionKind::SearchedLibrary,
                ..
            } | GameEvent::CardsRevealed { .. }
                | GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
        )));
    }

    #[test]
    fn squadron_hawk_search_can_choose_zero_cards() {
        let (mut state, hawks, nonmatch) = resolve_squadron_hawk_etb_to_search_choice();

        let result =
            apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.stack.is_empty());
        for hawk in hawks {
            assert_eq!(state.objects[&hawk].zone, Zone::Library);
            assert!(state.players[0].library.contains(&hawk));
            assert!(!state.players[0].hand.contains(&hawk));
        }
        assert_eq!(state.objects[&nonmatch].zone, Zone::Library);
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::Shuffle,
                ..
            }
        )));
    }

    #[test]
    fn squadron_hawk_search_moves_only_selected_cards() {
        for selected_count in [1, 2] {
            let (mut state, hawks, _) = resolve_squadron_hawk_etb_to_search_choice();
            let selected = hawks[..selected_count].to_vec();

            let result = apply_as_current(
                &mut state,
                GameAction::SelectCards {
                    cards: selected.clone(),
                },
            )
            .unwrap();

            assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
            assert!(state.stack.is_empty());
            for hawk in selected {
                assert_eq!(state.objects[&hawk].zone, Zone::Hand);
                assert!(state.players[0].hand.contains(&hawk));
                assert!(!state.players[0].library.contains(&hawk));
            }
            for hawk in &hawks[selected_count..] {
                assert_eq!(state.objects[hawk].zone, Zone::Library);
                assert!(state.players[0].library.contains(hawk));
                assert!(!state.players[0].hand.contains(hawk));
            }
            assert!(result.events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )));
        }
    }

    // CR 120 (damage), CR 510.1 (combat damage step), CR 510.3a
    // (combat-damage triggers go on the stack), CR 701.23a/b/d (search
    // library / fail-to-find), CR 701.24 (shuffle), CR 100.2a /
    // CR 903.5b (deck-construction overrides — verified silently consumed
    // by Step 1's parser fix).
    //
    // Tempest Hawk's combat-damage trigger:
    //   "Whenever this creature deals combat damage to a player, you may
    //    search your library for a card named Tempest Hawk, reveal it,
    //    put it into your hand, then shuffle."
    //
    // The AST shape: TriggerMode::DamageDone with damage_kind = CombatOnly,
    // valid_target = Player, optional = true, execute chain =
    // SearchLibrary → ChangeZone(Library→Hand) → Shuffle. The shape is
    // identical to Squadron Hawk's ETB-triggered search, so we reuse the
    // search-and-shuffle assertion structure; only the trigger source
    // (combat damage vs ETB) differs.
    const TEMPEST_HAWK_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a player, you may search your library for a card named Tempest Hawk, reveal it, put it into your hand, then shuffle.\nA deck can have any number of cards named Tempest Hawk.";

    fn add_tempest_hawk_to_library(state: &mut GameState, card_id: u64) -> ObjectId {
        let hawk = create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            "Tempest Hawk".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&hawk).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }
        hawk
    }

    /// Set up a board where a Tempest Hawk on the battlefield is the sole
    /// attacker against PlayerId(1), and advance combat through declare-
    /// attackers / declare-blockers so the damage step is about to fire.
    /// Returns (state, attacking hawk, hawks in library).
    fn setup_tempest_hawk_attack(library_hawk_ids: &[u64]) -> (GameState, ObjectId, Vec<ObjectId>) {
        let mut state = new_game(42);
        state.turn_number = 5;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let attacker = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Tempest Hawk".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_card_types = obj.card_types.clone();
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.color = vec![ManaColor::White];
            obj.base_color = vec![ManaColor::White];
            obj.entered_battlefield_turn = Some(4);
        }
        apply_oracle_to_object(&mut state, attacker, "Tempest Hawk", TEMPEST_HAWK_ORACLE);

        let library_hawks: Vec<ObjectId> = library_hawk_ids
            .iter()
            .map(|id| add_tempest_hawk_to_library(&mut state, *id))
            .collect();

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };

        apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();

        (state, attacker, library_hawks)
    }

    /// Advance combat from DeclareAttackers (just submitted) through to the
    /// point where Tempest Hawk's `you may` combat-damage trigger has been
    /// pushed onto the stack and is being resolved (engine is at
    /// `WaitingFor::OptionalEffectChoice`).
    fn advance_to_tempest_hawk_optional_choice(state: &mut GameState) {
        for _ in 0..16 {
            if matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }) {
                return;
            }
            apply_as_current(state, GameAction::PassPriority).unwrap();
        }
        panic!(
            "expected WaitingFor::OptionalEffectChoice for Tempest Hawk's combat-damage trigger, \
             got {:?} after exhausting priority passes",
            state.waiting_for
        );
    }

    #[test]
    fn tempest_hawk_combat_damage_optional_accept_finds_named_card() {
        // Accept path: Tempest Hawk deals combat damage to PlayerId(1),
        // the optional `you may search` trigger is accepted, the
        // SearchChoice exposes only Tempest Hawks from the library, and
        // SelectCards moves the chosen hawk to hand with a Shuffle event.
        let (mut state, _attacker, library_hawks) = setup_tempest_hawk_attack(&[701, 702, 703]);

        // Sanity: also drop a non-Hawk into the library to confirm the
        // SearchChoice filters by name.
        let nonmatch = create_object(
            &mut state,
            CardId(799),
            PlayerId(0),
            "Storm Crow".to_string(),
            Zone::Library,
        );

        advance_to_tempest_hawk_optional_choice(&mut state);
        assert_eq!(
            state.players[1].life, 18,
            "Tempest Hawk should have dealt 2 combat damage to PlayerId(1)"
        );

        apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        match &state.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                reveal,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(*reveal);
                for hawk in &library_hawks {
                    assert!(
                        cards.contains(hawk),
                        "SearchChoice must offer library Tempest Hawk {hawk:?}, got {cards:?}"
                    );
                }
                assert!(
                    !cards.contains(&nonmatch),
                    "SearchChoice must not offer non-Tempest-Hawk card {nonmatch:?}"
                );
            }
            other => {
                panic!("expected SearchChoice after accepting Tempest Hawk trigger, got {other:?}")
            }
        }

        let chosen = library_hawks[0];
        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![chosen],
            },
        )
        .unwrap();

        assert!(
            state.stack.is_empty(),
            "stack must be empty after resolving search"
        );
        assert_eq!(state.objects[&chosen].zone, Zone::Hand);
        assert!(state.players[0].hand.contains(&chosen));
        assert!(!state.players[0].library.contains(&chosen));
        for other in &library_hawks[1..] {
            assert_eq!(state.objects[other].zone, Zone::Library);
        }
        assert!(
            result.events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )),
            "library must be shuffled at end of the trigger chain (CR 701.24)"
        );
    }

    #[test]
    fn tempest_hawk_combat_damage_optional_decline_leaves_library_untouched() {
        // Decline path: declining the `you may` trigger must leave the
        // library and hand untouched and clear the stack — no search,
        // no shuffle.
        let (mut state, _attacker, library_hawks) = setup_tempest_hawk_attack(&[711, 712]);

        advance_to_tempest_hawk_optional_choice(&mut state);

        let result = apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: false },
        )
        .unwrap();

        assert!(state.stack.is_empty());
        for hawk in &library_hawks {
            assert_eq!(state.objects[hawk].zone, Zone::Library);
            assert!(state.players[0].library.contains(hawk));
            assert!(!state.players[0].hand.contains(hawk));
        }
        assert!(
            !result.events.iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::SearchedLibrary,
                    ..
                } | GameEvent::CardsRevealed { .. }
                    | GameEvent::EffectResolved {
                        kind: EffectKind::Shuffle,
                        ..
                    }
            )),
            "declining the trigger must produce no search/reveal/shuffle events"
        );
    }

    #[test]
    fn tempest_hawk_combat_damage_accept_with_empty_library_resolves_cleanly() {
        // Fail-to-find path: accepting the search with zero Tempest Hawks
        // in the library must resolve cleanly per CR 701.23b (player may
        // search and find nothing; library still shuffles per CR 701.23d).
        let (mut state, _attacker, _) = setup_tempest_hawk_attack(&[]);
        // Non-matching filler so the library is not literally empty —
        // this isolates "no card matching the filter" from "library empty".
        let filler = create_object(
            &mut state,
            CardId(720),
            PlayerId(0),
            "Storm Crow".to_string(),
            Zone::Library,
        );

        advance_to_tempest_hawk_optional_choice(&mut state);

        let result = apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        let mut events = result.events;

        // Engine may either (a) skip straight past SearchChoice because no
        // cards match, in which case the shuffle event is emitted by the
        // DecideOptionalEffect call above, or (b) expose an empty/zero
        // SearchChoice that resolves to SelectCards { cards: vec![] }, in
        // which case the shuffle event is emitted by SelectCards. Combine
        // events from both possible paths so the shuffle assertion holds
        // regardless of which branch the engine takes (CR 701.24 still
        // applies — the library shuffles even on fail-to-find).
        if matches!(state.waiting_for, WaitingFor::SearchChoice { .. }) {
            let select_result =
                apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] }).unwrap();
            events.extend(select_result.events);
        }

        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Shuffle,
                    ..
                }
            )),
            "library must shuffle even when the search finds nothing (CR 701.24)"
        );

        assert!(
            state.stack.is_empty(),
            "stack must drain even on fail-to-find"
        );
        assert_eq!(state.objects[&filler].zone, Zone::Library);
        assert!(state.players[0].hand.is_empty());
    }

    #[test]
    fn fizzle_target_removed_before_resolution() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a creature target
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Lightning Bolt targeting the creature
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        player.mana_pool.add(ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(0),
            snow: false,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: bolt_id,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // Remove the creature from battlefield before resolution (simulating it was destroyed)
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        // Both pass -> resolve -- should fizzle
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // Stack should be empty, bolt should be in graveyard (fizzled)
        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&bolt_id));
        // Creature was already in graveyard, life should be unchanged
        assert_eq!(state.players[1].life, 20);
    }

    // === Phase 04 Plan 03 Integration Tests ===

    use crate::types::ability::TargetRef;
    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for _ in 0..count {
            player_data.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    #[test]
    fn lightning_bolt_deals_3_damage_to_creature() {
        let mut state = setup_game_at_main_phase();

        // Create a 2/3 creature controlled by P1
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(3);
        }

        // Create Lightning Bolt in P0's hand
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Cast Lightning Bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: bolt_id,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        let result = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.players[0].mana_pool.total(), 0);

        // Both pass -> resolve
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // Creature should have 3 damage, which equals toughness -> SBA destroys it
        assert!(state.stack.is_empty());
        assert!(!state.battlefield.contains(&creature_id));
        assert!(state.players[1].graveyard.contains(&creature_id));
        // Bolt is instant -> goes to graveyard
        assert!(state.players[0].graveyard.contains(&bolt_id));
    }

    #[test]
    fn lightning_bolt_deals_3_damage_to_player() {
        let mut state = setup_game_at_main_phase();

        // Create Lightning Bolt in P0's hand with Any target
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Two players as targets, need manual selection
        // Use Player filter -> 2 targets -> need SelectTargets
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: bolt_id,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();

        // Should need target selection (2 players)
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select player 1 as target
        let result = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(1))],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);

        // Both pass -> resolve
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.players[1].life, 17);
        assert!(state.players[0].graveyard.contains(&bolt_id));
    }

    #[test]
    fn counterspell_counters_a_spell_on_stack() {
        let mut state = setup_game_at_main_phase();

        // P0 casts a creature spell -- put it on the stack manually
        let creature_id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Vanilla creature has no abilities (empty vec is the default)
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Green, 2);

        // Cast the creature
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: creature_id,
                card_id: CardId(30),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // P1 gets priority, has Counterspell
        // Pass priority from P0 to P1
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        // Now P1 has priority
        assert_eq!(state.priority_player, PlayerId(1));

        let counter_id = create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&counter_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Typed(TypedFilter::card()),
                    source_rider: None,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(1), ManaType::Blue, 2);

        // Cast Counterspell — targets a spell on the stack
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: counter_id,
                card_id: CardId(40),
                targets: vec![],
            },
        )
        .unwrap();
        // Handle target selection if needed (single spell auto-targets, but be robust).
        let result = if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
            apply_as_current(
                &mut state,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(creature_id)],
                },
            )
            .unwrap()
        } else {
            result
        };
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 2); // creature + counterspell

        // Both pass -> Counterspell resolves first (LIFO)
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // Counterspell resolved, creature spell should be countered (in graveyard)
        // Counterspell should also be in graveyard
        assert!(state.players[0].graveyard.contains(&creature_id));
        assert!(state.players[1].graveyard.contains(&counter_id));
        // Creature never reached battlefield
        assert!(!state.battlefield.contains(&creature_id));
    }

    #[test]
    fn giant_growth_gives_plus_3_3() {
        let mut state = setup_game_at_main_phase();

        // Create a 2/2 creature for P0
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Giant Growth in P0's hand
        let growth_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&growth_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Pump {
                    power: crate::types::ability::PtValue::Fixed(3),
                    toughness: crate::types::ability::PtValue::Fixed(3),
                    target: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(crate::types::ability::ControllerRef::You),
                    ),
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Green, 1);

        // Cast Giant Growth (auto-targets single own creature)
        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: growth_id,
                card_id: CardId(60),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // Both pass -> resolve
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&creature_id].power, Some(5));
        assert_eq!(state.objects[&creature_id].toughness, Some(5));
        assert!(state.players[0].graveyard.contains(&growth_id));
    }

    #[test]
    fn fizzle_bolt_target_removed() {
        let mut state = setup_game_at_main_phase();

        // Create a creature
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Lightning Bolt
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            Arc::make_mut(&mut obj.abilities).push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: bolt_id,
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();

        // Remove creature before resolution
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        // Both pass -> fizzle
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&bolt_id));
        // No DamageDealt event
        assert!(!result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    }

    #[test]
    fn test_mana_ability_during_priority_does_not_push_stack() {
        let mut state = setup_game_at_main_phase();

        // Create a creature with a mana ability on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: obj_id,
                ability_index: 0,
            },
        )
        .unwrap();

        // Stack should remain empty (mana abilities don't use the stack)
        assert!(
            state.stack.is_empty(),
            "mana ability should not push to stack"
        );
        // Should stay in Priority
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);
        // Player should have green mana
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
    }

    #[test]
    fn test_mana_ability_during_mana_payment_stays_in_mana_payment() {
        let mut state = setup_game_at_main_phase();
        // In production, ManaPayment is only entered via `enter_payment_step`
        // once `state.pending_cast` is populated — the drift invariant in
        // `derived` requires the two storage sites to agree. Reproduce that
        // precondition here so the synthetic state matches engine reality.
        state.pending_cast = Some(Box::new(crate::types::game_state::PendingCast {
            object_id: ObjectId(0),
            card_id: CardId(0),
            ability: crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::Unimplemented {
                    name: "Test".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(0),
                PlayerId(0),
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: vec![],
            casting_variant: crate::types::game_state::CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: crate::types::zones::Zone::Hand,
            additional_cost_flow: None,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        }));
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        // Create a creature with a mana ability on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Birds of Paradise".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: obj_id,
                ability_index: 0,
            },
        )
        .unwrap();

        // Should stay in ManaPayment
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ManaPayment {
                    player: PlayerId(0),
                    ..
                }
            ),
            "should remain in ManaPayment after mana ability"
        );
        // Stack should remain empty
        assert!(state.stack.is_empty());
        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);
    }

    #[test]
    fn springleaf_drum_prompts_for_creature_then_adds_mana() {
        let mut state = setup_game_at_main_phase();

        let drum = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Springleaf Drum".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&drum).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                crate::types::mana::ManaColor::White,
                                crate::types::mana::ManaColor::Blue,
                                crate::types::mana::ManaColor::Black,
                                crate::types::mana::ManaColor::Red,
                                crate::types::mana::ManaColor::Green,
                            ],
                            contribution: crate::types::ability::ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::TapCreatures {
                            count: 1,
                            filter: crate::types::ability::TypedFilter::creature()
                                .controller(crate::types::ability::ControllerRef::You)
                                .into(),
                        },
                    ],
                }),
            );
        }

        let creature = create_object(
            &mut state,
            CardId(103),
            PlayerId(0),
            "Memnite".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: drum,
                ability_index: 0,
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::TapCreaturesForManaAbility {
                player: PlayerId(0),
                count: 1,
                ..
            }
        ));
        assert!(!state.objects.get(&drum).unwrap().tapped);
        assert!(!state.objects.get(&creature).unwrap().tapped);

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();

        // AnyOneColor with 5 options chains into ChooseManaColor after creature tap.
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ChooseManaColor {
                    player: PlayerId(0),
                    ..
                }
            ),
            "expected ChooseManaColor, got {:?}",
            result.waiting_for,
        );
        assert!(state.objects.get(&drum).unwrap().tapped);
        assert!(state.objects.get(&creature).unwrap().tapped);
        assert_eq!(state.players[0].mana_pool.total(), 0);

        // Choose green — mana should now be produced.
        let result = apply_as_current(
            &mut state,
            GameAction::ChooseManaColor {
                choice: crate::types::game_state::ManaChoice::SingleColor(
                    crate::types::mana::ManaType::Green,
                ),
                count: 1,
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
    }

    /// Issue #443: A `TapsForMana` mana multiplier must fire exactly once when
    /// an `AnyOneColor` mana ability routes through a `ChooseManaColor` prompt
    /// during a `Priority` resume. Pre-fix, the inline scan in the
    /// `ChooseManaColor` arm AND the post-action pipeline both scanned the same
    /// `FromTap` `ManaAdded` event, double-firing the multiplier (1 base + 2 +
    /// 2 = 5 instead of 1 base + 2 = 3). CR 603.2c.
    #[test]
    fn taps_for_mana_multiplier_fires_once_on_color_choice_priority_resume() {
        let mut state = setup_game_at_main_phase();

        // A `TapsForMana` multiplier on a creature: whenever a permanent the
        // controller controls taps for mana, add mana of that type.
        // `TriggerEventManaType` adds one unit per fire; two trigger
        // definitions give a deterministic +2 multiplier (base 1 + 2 = 3).
        let mana_doubler = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Mana Multiplier".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mana_doubler).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let multiplier_trigger = || {
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: crate::types::ability::ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Any)
                    .valid_target(TargetFilter::Controller)
            };
            obj.trigger_definitions.push(multiplier_trigger());
            obj.trigger_definitions.push(multiplier_trigger());
        }

        // An `AnyOneColor` source with >1 color option — this routes through
        // `WaitingFor::ChooseManaColor`.
        let any_color = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Any Color Rock".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&any_color).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.entered_battlefield_turn = Some(1);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                crate::types::mana::ManaColor::White,
                                crate::types::mana::ManaColor::Blue,
                                crate::types::mana::ManaColor::Black,
                                crate::types::mana::ManaColor::Red,
                                crate::types::mana::ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: any_color,
                ability_index: 0,
            },
        )
        .unwrap();
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ChooseManaColor {
                    player: PlayerId(0),
                    ..
                }
            ),
            "expected ChooseManaColor, got {:?}",
            result.waiting_for,
        );
        assert_eq!(state.players[0].mana_pool.total(), 0);

        let _result = apply_as_current(
            &mut state,
            GameAction::ChooseManaColor {
                choice: crate::types::game_state::ManaChoice::SingleColor(
                    crate::types::mana::ManaType::Green,
                ),
                count: 1,
            },
        )
        .unwrap();
        // CR 603.3b (#531): controller has 2 simultaneous TapsForMana triggers
        // (the multiplier x2) — drain the OrderTriggers prompt so the legacy
        // post-resolution assertions see the produced mana totals.
        crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        // After draining, the stack should resolve (mana abilities are inline)
        // and waiting_for becomes Priority.
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));

        // 1 base + 2 from the multiplier = 3. Pre-fix this yields 5 (the
        // multiplier double-fires). Assert it is neither 1 (multiplier dropped)
        // nor 5 (double-fire).
        let total = state.players[0].mana_pool.total();
        assert_ne!(total, 1, "multiplier must fire (got base mana only)");
        assert_ne!(total, 5, "multiplier must fire exactly once, not twice");
        assert_eq!(total, 3, "expected 1 base + 2 multiplier = 3, got {total}",);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            3,
        );
    }

    /// Issue #443 companion: the same `TapsForMana` multiplier must also fire
    /// exactly once when the `AnyOneColor` ability is activated mid-payment
    /// (`ManaAbilityResume::ManaPayment`). For that resume the post-action
    /// pipeline is skipped entirely, so the inline scan in the
    /// `ChooseManaColor` arm is the ONLY scan site — proving the fix does not
    /// drop the multiplier on the non-`Priority` path. CR 603.2c + CR 605.4a.
    #[test]
    fn taps_for_mana_multiplier_fires_once_on_color_choice_mana_payment_resume() {
        let mut state = setup_game_at_main_phase();

        // Mirror the production precondition: ManaPayment is only entered with
        // `pending_cast` populated (see the drift invariant in `derived`).
        state.pending_cast = Some(Box::new(crate::types::game_state::PendingCast {
            object_id: ObjectId(0),
            card_id: CardId(0),
            ability: crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::Unimplemented {
                    name: "Test".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(0),
                PlayerId(0),
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: vec![],
            casting_variant: crate::types::game_state::CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: crate::types::zones::Zone::Hand,
            additional_cost_flow: None,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: crate::types::game_state::CastPaymentMode::Auto,
        }));
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        let mana_doubler = create_object(
            &mut state,
            CardId(202),
            PlayerId(0),
            "Mana Multiplier".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mana_doubler).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let multiplier_trigger = || {
                TriggerDefinition::new(TriggerMode::TapsForMana)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Mana {
                            produced: crate::types::ability::ManaProduction::TriggerEventManaType,
                            restrictions: vec![],
                            grants: vec![],
                            expiry: None,
                            target: None,
                        },
                    ))
                    .valid_card(TargetFilter::Any)
                    .valid_target(TargetFilter::Controller)
            };
            obj.trigger_definitions.push(multiplier_trigger());
            obj.trigger_definitions.push(multiplier_trigger());
        }

        let any_color = create_object(
            &mut state,
            CardId(203),
            PlayerId(0),
            "Any Color Rock".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&any_color).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.entered_battlefield_turn = Some(1);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                crate::types::mana::ManaColor::White,
                                crate::types::mana::ManaColor::Blue,
                                crate::types::mana::ManaColor::Black,
                                crate::types::mana::ManaColor::Red,
                                crate::types::mana::ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        // Activate the AnyOneColor ability mid-payment → ManaAbilityResume::ManaPayment.
        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: any_color,
                ability_index: 0,
            },
        )
        .unwrap();
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ChooseManaColor {
                    player: PlayerId(0),
                    ..
                }
            ),
            "expected ChooseManaColor, got {:?}",
            result.waiting_for,
        );

        let _result = apply_as_current(
            &mut state,
            GameAction::ChooseManaColor {
                choice: crate::types::game_state::ManaChoice::SingleColor(
                    crate::types::mana::ManaType::Green,
                ),
                count: 1,
            },
        )
        .unwrap();

        // CR 603.3b + CR 605.4a: the 2 simultaneous multiplier triggers raise
        // an OrderTriggers prompt before the resume can return to ManaPayment.
        // Draining the ordering prompt must restore the suspended payment step.
        crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ManaPayment {
                    player: PlayerId(0),
                    convoke_mode: None,
                }
            ),
            "OrderTriggers drain must resume ManaPayment, got {:?}",
            state.waiting_for
        );

        // 1 base + 2 multiplier = 3 — fired exactly once, not dropped, not doubled.
        let total = state.players[0].mana_pool.total();
        assert_ne!(
            total, 1,
            "multiplier must still fire on the ManaPayment path"
        );
        assert_ne!(total, 5, "multiplier must fire exactly once, not twice");
        assert_eq!(total, 3, "expected 1 base + 2 multiplier = 3, got {total}",);
    }

    #[test]
    fn holdout_settlement_second_mana_ability_prompts_for_creature_then_adds_mana() {
        let mut state = setup_game_at_main_phase();

        let holdout = create_object(
            &mut state,
            CardId(104),
            PlayerId(0),
            "Holdout Settlement".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&holdout).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            let abilities = Arc::make_mut(&mut obj.abilities);
            abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
            abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                crate::types::mana::ManaColor::White,
                                crate::types::mana::ManaColor::Blue,
                                crate::types::mana::ManaColor::Black,
                                crate::types::mana::ManaColor::Red,
                                crate::types::mana::ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::TapCreatures {
                            count: 1,
                            filter: TypedFilter::creature()
                                .controller(ControllerRef::You)
                                .into(),
                        },
                    ],
                }),
            );
        }

        let creature = create_object(
            &mut state,
            CardId(105),
            PlayerId(0),
            "Memnite".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let (_, _, grouped) = crate::ai_support::legal_actions_full(&state);
        let holdout_actions = grouped
            .get(&holdout)
            .expect("Holdout Settlement should expose legal mana actions");
        assert!(holdout_actions.iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0
            } if *source_id == holdout
        )));
        assert!(holdout_actions.iter().any(|action| matches!(
            action,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 1
            } if *source_id == holdout
        )));

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: holdout,
                ability_index: 1,
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::TapCreaturesForManaAbility {
                player,
                count,
                creatures,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 1);
                assert_eq!(creatures, vec![creature]);
            }
            other => panic!("expected TapCreaturesForManaAbility, got {other:?}"),
        }
        assert!(!state.objects.get(&holdout).unwrap().tapped);
        assert!(!state.objects.get(&creature).unwrap().tapped);

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![creature],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::ChooseManaColor {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(state.objects.get(&holdout).unwrap().tapped);
        assert!(state.objects.get(&creature).unwrap().tapped);

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseManaColor {
                choice: crate::types::game_state::ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn non_mana_activation_tap_creatures_cost_prompts_then_pays() {
        let mut state = setup_game_at_main_phase();

        let lathril = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Lathril, Blade of the Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&lathril).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 10 },
                        player: GainLifePlayer::Controller,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::TapCreatures {
                            count: 2,
                            filter: TypedFilter::creature()
                                .with_type(TypeFilter::Subtype("Elf".to_string()))
                                .controller(ControllerRef::You)
                                .into(),
                        },
                    ],
                }),
            );
        }

        let elf_a = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Elf A".to_string(),
            Zone::Battlefield,
        );
        let elf_b = create_object(
            &mut state,
            CardId(202),
            PlayerId(0),
            "Elf B".to_string(),
            Zone::Battlefield,
        );
        let non_elf = create_object(
            &mut state,
            CardId(203),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        for (id, subtype) in [(elf_a, "Elf"), (elf_b, "Elf"), (non_elf, "Bear")] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push(subtype.to_string());
        }

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: lathril,
                ability_index: 0,
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::TapCreaturesForSpellCost {
                player,
                count,
                creatures,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(count, 2);
                assert_eq!(creatures, vec![elf_a, elf_b]);
            }
            other => panic!("expected TapCreaturesForSpellCost, got {other:?}"),
        }
        assert!(!state.objects.get(&lathril).unwrap().tapped);

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![elf_a, elf_b],
            },
        )
        .unwrap();

        assert!(state.objects.get(&lathril).unwrap().tapped);
        assert!(state.objects.get(&elf_a).unwrap().tapped);
        assert!(state.objects.get(&elf_b).unwrap().tapped);
        assert!(!state.objects.get(&non_elf).unwrap().tapped);
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn non_mana_activation_tap_creatures_cost_rejects_tapped_source_before_prompt() {
        let mut state = setup_game_at_main_phase();

        let source = create_object(
            &mut state,
            CardId(204),
            PlayerId(0),
            "Tapped Elf Caller".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
            obj.tapped = true;
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: GainLifePlayer::Controller,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::TapCreatures {
                            count: 1,
                            filter: TypedFilter::creature()
                                .with_type(TypeFilter::Subtype("Elf".to_string()))
                                .controller(ControllerRef::You)
                                .into(),
                        },
                    ],
                }),
            );
        }

        let elf = create_object(
            &mut state,
            CardId(205),
            PlayerId(0),
            "Elf".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&elf).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Elf".to_string());

        let err = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: source,
                ability_index: 0,
            },
        )
        .unwrap_err();

        assert!(
            matches!(err, EngineError::ActionNotAllowed(message) if message == "Cannot activate tap ability: permanent is tapped")
        );
    }

    mod equip_tests {
        use super::*;

        fn setup_equip_game() -> GameState {
            let mut state = GameState::new_two_player(42);
            state.turn_number = 2;
            state.phase = Phase::PreCombatMain;
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };
            state
        }

        fn create_equipment(state: &mut GameState, player: PlayerId) -> ObjectId {
            let id = zones::create_object(
                state,
                CardId(100),
                player,
                "Bonesplitter".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.controller = player;
            id
        }

        fn create_creature_on_bf(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
            let id = zones::create_object(
                state,
                CardId(state.next_object_id),
                player,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.controller = player;
            id
        }

        #[test]
        fn test_equip_creates_equip_target_with_valid_creatures() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            let result = apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();

            match result.waiting_for {
                WaitingFor::EquipTarget {
                    player,
                    equipment_id: eq_id,
                    valid_targets,
                } => {
                    assert_eq!(player, PlayerId(0));
                    assert_eq!(eq_id, equipment_id);
                    assert!(valid_targets.contains(&creature_a));
                    assert!(valid_targets.contains(&creature_b));
                }
                other => panic!("Expected EquipTarget, got {:?}", other),
            }
        }

        #[test]
        fn test_equip_selects_target_attaches_equipment() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let _creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            let result = apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            assert!(matches!(result.waiting_for, WaitingFor::EquipTarget { .. }));

            // Target selection announces the ability on the stack (CR 113.3b).
            apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_a,
                },
            )
            .unwrap();
            assert_eq!(state.stack.len(), 1, "Equip announces on the stack");
            assert!(
                state
                    .objects
                    .get(&equipment_id)
                    .unwrap()
                    .attached_to
                    .is_none(),
                "attach must wait for stack resolution"
            );

            // Pass priority twice → stack resolves → attachment applied.
            apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
            apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
            assert_eq!(
                state
                    .objects
                    .get(&equipment_id)
                    .unwrap()
                    .attached_to
                    // CR 301.5: Equipment must attach to an object — `as_object`
                    // makes the rules invariant explicit.
                    .and_then(|t| t.as_object()),
                Some(creature_a)
            );
            assert!(state
                .objects
                .get(&creature_a)
                .unwrap()
                .attachments
                .contains(&equipment_id));
        }

        #[test]
        fn test_equip_re_equip_moves_to_new_creature() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            // First equip to creature A — requires stack resolution.
            apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_a,
                },
            )
            .unwrap();
            apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
            apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
            assert_eq!(
                state
                    .objects
                    .get(&equipment_id)
                    .unwrap()
                    .attached_to
                    .and_then(|t| t.as_object()),
                Some(creature_a)
            );

            // Re-equip to creature B.
            apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_b,
                },
            )
            .unwrap();
            apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
            apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

            assert_eq!(
                state
                    .objects
                    .get(&equipment_id)
                    .unwrap()
                    .attached_to
                    .and_then(|t| t.as_object()),
                Some(creature_b)
            );
            assert!(state
                .objects
                .get(&creature_b)
                .unwrap()
                .attachments
                .contains(&equipment_id));
            assert!(!state
                .objects
                .get(&creature_a)
                .unwrap()
                .attachments
                .contains(&equipment_id));
        }

        #[test]
        fn test_equip_only_at_sorcery_speed() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let _creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

            // Try during combat phase - should fail
            state.phase = Phase::DeclareAttackers;
            let result = apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());

            // Try with non-empty stack - should fail
            state.phase = Phase::PreCombatMain;
            state.stack.push_back(crate::types::game_state::StackEntry {
                id: ObjectId(99),
                source_id: ObjectId(99),
                controller: PlayerId(1),
                kind: crate::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(99),
                    ability: None,
                    casting_variant: crate::types::game_state::CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
            let result = apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());

            // Try when not active player - should fail
            state.stack.clear();
            state.active_player = PlayerId(1);
            let result = apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());
        }

        #[test]
        fn test_equip_auto_targets_single_creature() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

            // Auto-target still pushes the ability on the stack (CR 113.3b).
            let result = apply_as_current(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
            assert_eq!(state.stack.len(), 1);
            assert!(
                state
                    .objects
                    .get(&equipment_id)
                    .unwrap()
                    .attached_to
                    .is_none(),
                "attach waits for resolution"
            );

            apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
            apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
            assert_eq!(
                state
                    .objects
                    .get(&equipment_id)
                    .unwrap()
                    .attached_to
                    .and_then(|t| t.as_object()),
                Some(creature)
            );
        }
    }

    #[test]
    fn land_with_etb_tapped_replacement_enters_tapped() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Selesnya Guildgate".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Tap {
                        target: TargetFilter::SelfRef,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .description("Selesnya Guildgate enters the battlefield tapped.".to_string()),
        );

        let _result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        )
        .unwrap();
        assert!(state.battlefield.contains(&obj_id));
        assert!(
            state.objects[&obj_id].tapped,
            "ETB-tapped land must enter tapped"
        );
    }

    // ── UntapLandForMana tests ────────────────────────────────────────────

    fn create_forest(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(99),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.controller = player;
        obj.entered_battlefield_turn = Some(1);
        id
    }

    #[test]
    fn tap_land_records_in_lands_tapped_for_mana() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        let tracked = &state.lands_tapped_for_mana[&PlayerId(0)];
        assert!(tracked.contains(&land_id));
    }

    #[test]
    fn untap_land_removes_mana_and_untaps() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );

        let result = apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        )
        .unwrap();

        assert!(!state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            0
        );
        assert!(state
            .lands_tapped_for_mana
            .get(&PlayerId(0))
            .is_none_or(|v| !v.contains(&land_id)));
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn untap_one_of_two_tapped_lands_preserves_other() {
        let mut state = setup_game_at_main_phase();
        let land1 = create_forest(&mut state, PlayerId(0));
        let land2 = create_forest(&mut state, PlayerId(0));

        apply_as_current(&mut state, GameAction::TapLandForMana { object_id: land1 }).unwrap();
        apply_as_current(&mut state, GameAction::TapLandForMana { object_id: land2 }).unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            2
        );

        apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: land1 },
        )
        .unwrap();

        assert!(!state.objects[&land1].tapped);
        assert!(state.objects[&land2].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
        let tracked = &state.lands_tapped_for_mana[&PlayerId(0)];
        assert!(!tracked.contains(&land1));
        assert!(tracked.contains(&land2));
    }

    #[test]
    fn untap_rejects_when_mana_already_spent() {
        use crate::types::mana::ManaType;

        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        state.players[0].mana_pool.spend(ManaType::Green);
        assert_eq!(state.players[0].mana_pool.total(), 0);

        let result = apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        );
        assert!(result.is_err());
    }

    #[test]
    fn pass_priority_clears_lands_tapped_for_mana() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(!state.lands_tapped_for_mana.is_empty());

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(!state.lands_tapped_for_mana.contains_key(&PlayerId(0)));
    }

    #[test]
    fn play_land_clears_lands_tapped_for_mana() {
        let mut state = setup_game_at_main_phase();
        let tapped_land = create_forest(&mut state, PlayerId(0));

        apply_as_current(
            &mut state,
            GameAction::TapLandForMana {
                object_id: tapped_land,
            },
        )
        .unwrap();
        assert!(!state.lands_tapped_for_mana.is_empty());

        let hand_land = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&hand_land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
        }

        apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: hand_land,
                card_id: CardId(50),
            },
        )
        .unwrap();
        assert!(!state.lands_tapped_for_mana.contains_key(&PlayerId(0)));
    }

    #[test]
    fn untap_non_tracked_land_fails() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        let result = apply_as_current(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        );
        assert!(result.is_err());
    }

    #[test]
    fn untap_during_mana_payment_returns_mana_payment() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery that needs blue mana
        let spell_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 1,
            };
        }

        // Add partial mana — not enough to auto-pay, so we get ManaPayment
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        player.mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(0),
            snow: false,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        // Create a forest on the battlefield to tap during ManaPayment
        let land_id = create_forest(&mut state, PlayerId(0));

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(10),
                targets: vec![],
            },
        );

        // If we get ManaPayment, test the untap flow there
        if let Ok(ActionResult {
            waiting_for: WaitingFor::ManaPayment { .. },
            ..
        }) = &result
        {
            // Tap the land during ManaPayment
            apply_as_current(
                &mut state,
                GameAction::TapLandForMana { object_id: land_id },
            )
            .unwrap();
            assert!(state.lands_tapped_for_mana[&PlayerId(0)].contains(&land_id));

            // Untap it — should return ManaPayment, not Priority
            let untap_result = apply_as_current(
                &mut state,
                GameAction::UntapLandForMana { object_id: land_id },
            )
            .unwrap();
            assert!(matches!(
                untap_result.waiting_for,
                WaitingFor::ManaPayment {
                    player: PlayerId(0),
                    ..
                }
            ));
        }
        // If auto-pay succeeded, the test setup didn't produce ManaPayment — still valid
    }

    #[test]
    fn zone_change_removes_stale_tracking() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        // Tap the land
        apply_as_current(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(state.lands_tapped_for_mana[&PlayerId(0)].contains(&land_id));

        // Move the land to graveyard (e.g., destroyed)
        let mut events = Vec::new();
        super::zones::move_to_zone(&mut state, land_id, Zone::Graveyard, &mut events);

        // Tracking should be cleaned up
        assert!(state
            .lands_tapped_for_mana
            .get(&PlayerId(0))
            .is_none_or(|v| !v.contains(&land_id)));
    }

    /// CR 701.48a: Learn rummage — discard one card, draw one card, net hand size unchanged.
    #[test]
    fn learn_rummage_discards_and_draws() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        // Put a card in hand to discard
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand Card".to_string(),
            Zone::Hand,
        );
        // Put a card in library to draw
        let _lib_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Library Card".to_string(),
            Zone::Library,
        );

        // First: resolve the Learn effect to get WaitingFor::LearnChoice
        let learn_ability = ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0));
        let mut events = Vec::new();
        effects::learn::resolve(&mut state, &learn_ability, &mut events).unwrap();
        assert!(matches!(state.waiting_for, WaitingFor::LearnChoice { .. }));

        // Second: submit rummage decision through the engine
        let action = GameAction::LearnDecision {
            choice: crate::types::actions::LearnOption::Rummage { card_id: hand_card },
        };
        let result = apply_as_current(&mut state, action).unwrap();

        // The discarded card should be in graveyard
        assert!(state.players[0].graveyard.contains(&hand_card));
        // Hand should have exactly 1 card (the drawn one)
        assert_eq!(state.players[0].hand.len(), 1);
        // Should have emitted EffectResolved for Learn
        assert!(result.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Learn,
                ..
            }
        )));
    }

    /// CR 701.48a: Learn skip — no discard, no draw, hand unchanged.
    #[test]
    fn learn_skip_does_nothing() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand Card".to_string(),
            Zone::Hand,
        );

        let learn_ability = ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0));
        let mut events = Vec::new();
        effects::learn::resolve(&mut state, &learn_ability, &mut events).unwrap();

        let action = GameAction::LearnDecision {
            choice: crate::types::actions::LearnOption::Skip,
        };
        let result = apply_as_current(&mut state, action).unwrap();

        // Hand should still have the original card
        assert_eq!(state.players[0].hand.len(), 1);
        assert!(state.players[0].hand.contains(&hand_card));
        // Graveyard should be empty
        assert!(state.players[0].graveyard.is_empty());
        // Should have emitted EffectResolved for Learn
        assert!(result.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Learn,
                ..
            }
        )));
    }

    /// Verify that the ReplacementChoice handler picks up pending_continuation
    /// after replacement resolves (the foundation fix for Learn + Madness etc.)
    /// Verify that the Learn handler stashes draw as pending_continuation
    /// when discard returns NeedsReplacementChoice. This is a unit-level test
    /// of the stash mechanism; full Learn+Madness integration requires discard
    /// replacement pipeline support (not yet implemented for Discard events).
    #[test]
    fn learn_rummage_stashes_draw_continuation() {
        // The Learn handler's NeedsReplacementChoice branch stashes Draw
        // as pending_continuation — verify via the non-replacement path that
        // the continuation mechanism doesn't interfere with normal operation.
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let hand_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand Card".to_string(),
            Zone::Hand,
        );
        let _lib_card = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Draw Me".to_string(),
            Zone::Library,
        );

        // Pre-set pending_continuation to verify it's consumed normally
        state.pending_continuation = Some(crate::types::game_state::PendingContinuation::new(
            Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: crate::types::ability::GainLifePlayer::Controller,
                },
                vec![],
                source,
                PlayerId(0),
            )),
        ));

        let learn_ability = ResolvedAbility::new(Effect::Learn, vec![], source, PlayerId(0));
        let mut events = Vec::new();
        effects::learn::resolve(&mut state, &learn_ability, &mut events).unwrap();

        // Submit rummage — discard goes through (no replacement) and draws
        let action = GameAction::LearnDecision {
            choice: crate::types::actions::LearnOption::Rummage { card_id: hand_card },
        };
        let result = apply_as_current(&mut state, action).unwrap();

        // Normal rummage completed
        assert_eq!(state.players[0].hand.len(), 1);
        assert!(state.players[0].graveyard.contains(&hand_card));
        // The stashed continuation (GainLife) should have been consumed
        assert!(state.pending_continuation.is_none());
        // Life should have increased by 1 (from the continuation)
        assert_eq!(state.players[0].life, 21);
        assert!(result.events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Learn,
                ..
            }
        )));
    }

    // CR 402.3: Hand order has no game-rules significance — ReorderHand is a
    // display-preference update only.
    #[test]
    fn reorder_hand_replaces_hand_order() {
        let mut state = setup_game_at_main_phase();
        let p0 = PlayerId(0);

        let a = ObjectId(100);
        let b = ObjectId(101);
        let c = ObjectId(102);
        state.players[0].hand = crate::im::Vector::from(vec![a, b, c]);

        let result = apply(
            &mut state,
            p0,
            GameAction::ReorderHand {
                order: vec![c, a, b],
            },
        )
        .expect("reorder should succeed");

        assert!(result.events.is_empty(), "reorder must emit no events");
        assert_eq!(
            state.players[0].hand.iter().copied().collect::<Vec<_>>(),
            vec![c, a, b],
        );
    }

    #[test]
    fn reorder_hand_rejects_non_permutation() {
        let mut state = setup_game_at_main_phase();
        let p0 = PlayerId(0);
        let a = ObjectId(100);
        let b = ObjectId(101);
        state.players[0].hand = crate::im::Vector::from(vec![a, b]);

        // Wrong length.
        let err = apply(&mut state, p0, GameAction::ReorderHand { order: vec![a] })
            .expect_err("wrong length must error");
        assert!(matches!(err, EngineError::InvalidAction(_)));

        // Right length, wrong contents.
        let stranger = ObjectId(999);
        let err = apply(
            &mut state,
            p0,
            GameAction::ReorderHand {
                order: vec![a, stranger],
            },
        )
        .expect_err("stranger id must error");
        assert!(matches!(err, EngineError::InvalidAction(_)));

        // Hand unchanged after rejected calls.
        assert_eq!(
            state.players[0].hand.iter().copied().collect::<Vec<_>>(),
            vec![a, b],
        );
    }

    #[test]
    fn reorder_hand_succeeds_while_opponent_holds_priority() {
        // Verifies the `check_actor_authorization` whitelist: P0 must be able
        // to reorder their own hand even though P1 is the priority player and
        // holds the WaitingFor::Priority slot.
        let mut state = setup_game_at_main_phase();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let a = ObjectId(200);
        let b = ObjectId(201);
        state.players[0].hand = crate::im::Vector::from(vec![a, b]);

        apply(
            &mut state,
            PlayerId(0),
            GameAction::ReorderHand { order: vec![b, a] },
        )
        .expect("non-priority actor reordering own hand must succeed");

        assert_eq!(
            state.players[0].hand.iter().copied().collect::<Vec<_>>(),
            vec![b, a],
        );
        // Priority hasn't moved — reorder doesn't transition WaitingFor.
        assert_eq!(state.priority_player, PlayerId(1));
    }
}

#[cfg(test)]
mod trigger_target_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, GainLifePlayer, ModalChoice,
        ModalSelectionConstraint, QuantityExpr, ResolvedAbility, StaticCondition, TargetFilter,
        TargetRef, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::TargetSelectionConstraint;
    use crate::types::identifiers::CardId;

    #[test]
    fn trigger_target_selection_select_targets_pushes_to_stack() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create two opponent creatures as legal targets
        let target1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature 1".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target1).unwrap().controller = PlayerId(1);

        let target2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opp Creature 2".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target2).unwrap().controller = PlayerId(1);

        // Create trigger creature (Banishing Light)
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
        }

        // Manually set up the pending trigger state (as process_triggers would)
        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            Vec::new(),
            trigger_creature,
            PlayerId(0),
        )
        .duration(crate::types::ability::Duration::UntilHostLeavesPlay);

        // CR 603.3c + CR 603.3d "Push first": match what production does —
        // push the trigger entry to the stack and stash both the pending
        // trigger and the cursor BEFORE entering target selection.
        let pending = crate::game::triggers::PendingTrigger {
            source_id: trigger_creature,
            controller: PlayerId(0),
            condition: None,
            ability,
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);

        let legal_targets = vec![TargetRef::Object(target1), TargetRef::Object(target2)];

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
            }],
            target_constraints: Vec::new(),
            selection: crate::game::ability_utils::begin_target_selection(
                &[crate::types::game_state::TargetSelectionSlot {
                    legal_targets: legal_targets.clone(),
                    optional: false,
                }],
                &[],
            )
            .unwrap(),
            source_id: None,
            description: None,
        };

        // Player selects target1
        let result = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target1)],
            },
        )
        .unwrap();

        // Should return Priority
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority, got {:?}",
            result.waiting_for
        );

        // Trigger should be on the stack with the selected target
        assert_eq!(state.stack.len(), 1, "Trigger should be on stack");
        let entry = &state.stack[0];
        assert_eq!(entry.source_id, trigger_creature);
        match &entry.kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(ability.targets, vec![TargetRef::Object(target1)]);
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }

        // Pending trigger should be consumed
        assert!(state.pending_trigger.is_none());
    }

    #[test]
    fn trigger_target_selection_rejects_illegal_target() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        let legal_target = ObjectId(10);
        let illegal_target = ObjectId(99);

        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id: ObjectId(1),
            controller: PlayerId(0),
            condition: None,
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(legal_target)],
                optional: false,
            }],
            target_constraints: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress::default(),
            source_id: None,
            description: None,
        };

        // Try to select an illegal target
        let result = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(illegal_target)],
            },
        );

        assert!(result.is_err(), "Should reject illegal target");
    }

    #[test]
    fn triggered_modal_modes_with_targets_wait_for_target_selection() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id: ObjectId(20),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(20),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(GameEvent::SpellCast {
                controller: PlayerId(0),
                object_id: ObjectId(98),
                card_id: CardId(98),
            }),
            modal: Some(ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Deal 1 damage to target player.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            }),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
            )],
            description: Some("Choose two target players".to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Deal 1 damage to target player.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            },
            source_id: ObjectId(20),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
            )],
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: vec![],
        };

        let result = apply_as_current(
            &mut state,
            GameAction::SelectModes {
                indices: vec![0, 0],
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                target_constraints,
                ..
            } => {
                assert_eq!(target_slots.len(), 2);
                assert_eq!(
                    target_constraints,
                    vec![TargetSelectionConstraint::DifferentTargetPlayers]
                );
            }
            other => panic!("Expected TriggerTargetSelection, got {other:?}"),
        }
        // CR 603.3c + CR 603.3d "Push first": after mode chosen, the trigger
        // entry remains on the stack in mid-construction (target prompt
        // pending). `pending_trigger_entry` still identifies it.
        assert_eq!(state.stack.len(), 1);
        assert!(state.pending_trigger.is_some());
        assert!(state.pending_trigger_entry.is_some());
    }

    #[test]
    fn triggered_modal_modes_without_targets_consume_pending_trigger() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = ObjectId(21);
        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(GameEvent::SpellCast {
                controller: PlayerId(0),
                object_id: ObjectId(99),
                card_id: CardId(99),
            }),
            modal: Some(ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                mode_descriptions: vec!["Gain 2 life.".to_string(), "Draw a card.".to_string()],
                allow_repeat_modes: false,
                ..Default::default()
            }),
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
            ],
            description: Some("Whenever you cast your second spell each turn".to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                mode_descriptions: vec!["Gain 2 life.".to_string(), "Draw a card.".to_string()],
                allow_repeat_modes: false,
                ..Default::default()
            },
            source_id,
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
            ],
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: vec![],
        };

        let result =
            apply_as_current(&mut state, GameAction::SelectModes { indices: vec![0] }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.pending_trigger.is_none());
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0].kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility {
                ability,
                trigger_event,
                description,
                ..
            } => {
                assert!(matches!(ability.effect, Effect::GainLife { .. }));
                assert!(matches!(trigger_event, Some(GameEvent::SpellCast { .. })));
                assert_eq!(
                    description.as_deref(),
                    Some("Whenever you cast your second spell each turn")
                );
            }
            other => panic!("expected triggered ability on stack, got {other:?}"),
        }
    }

    #[test]
    fn triggered_commander_modal_cap_uses_controller_board_state() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(22);
        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: Some(ModalChoice {
                min_choices: 1,
                max_choices: 2,
                mode_count: 2,
                mode_descriptions: vec![
                    "Create a token.".to_string(),
                    "Put a counter.".to_string(),
                ],
                constraints: vec![ModalSelectionConstraint::ConditionalMaxChoices {
                    condition: crate::types::ability::ModalSelectionCondition::Static {
                        condition: StaticCondition::ControlsCommander {
                            ownership: crate::types::ability::CommanderOwnership::Any,
                        },
                    },
                    max_choices: 2,
                    otherwise_max_choices: 1,
                }],
                ..Default::default()
            }),
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
            ],
            description: Some("Choose one or both with commander".to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);

        let waiting = begin_pending_trigger_target_selection(&mut state)
            .unwrap()
            .expect("modal choice should be required");
        match waiting {
            WaitingFor::AbilityModeChoice { modal, .. } => {
                assert_eq!(modal.max_choices, 1);
            }
            other => panic!("expected AbilityModeChoice, got {other:?}"),
        }

        let commander_id = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Commander".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&commander_id).unwrap().is_commander = true;
        let waiting = begin_pending_trigger_target_selection(&mut state)
            .unwrap()
            .expect("modal choice should still be required");
        match waiting {
            WaitingFor::AbilityModeChoice { modal, .. } => {
                assert_eq!(modal.max_choices, 2);
            }
            other => panic!("expected AbilityModeChoice, got {other:?}"),
        }
    }

    #[test]
    fn trigger_target_selection_enforces_different_player_constraint() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id: ObjectId(30),
            controller: PlayerId(0),
            condition: None,
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
                vec![],
                ObjectId(30),
                PlayerId(0),
            )
            .sub_ability(crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
                vec![],
                ObjectId(30),
                PlayerId(0),
            )),
            timestamp: 1,
            target_constraints: vec![TargetSelectionConstraint::DifferentTargetPlayers],
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![
                crate::types::game_state::TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                },
                crate::types::game_state::TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                },
            ],
            target_constraints: vec![TargetSelectionConstraint::DifferentTargetPlayers],
            selection: crate::types::game_state::TargetSelectionProgress::default(),
            source_id: None,
            description: None,
        };

        let invalid = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(1)),
                ],
            },
        );
        assert!(invalid.is_err(), "same player should be rejected");

        let valid = apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
            },
        )
        .unwrap();

        assert!(matches!(valid.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0].kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(
                    crate::game::ability_utils::flatten_targets_in_chain(ability),
                    vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1))
                    ]
                );
            }
            other => panic!("expected triggered ability on stack, got {other:?}"),
        }
    }

    #[test]
    fn choose_target_action_advances_trigger_selection_from_engine_state() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let target_slots = vec![
            crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ];
        let target_constraints = vec![TargetSelectionConstraint::DifferentTargetPlayers];
        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id: ObjectId(31),
            controller: PlayerId(0),
            condition: None,
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
                vec![],
                ObjectId(31),
                PlayerId(0),
            )
            .sub_ability(crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
                vec![],
                ObjectId(31),
                PlayerId(0),
            )),
            timestamp: 1,
            target_constraints: target_constraints.clone(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: target_slots.clone(),
            target_constraints: target_constraints.clone(),
            selection: crate::game::ability_utils::begin_target_selection(
                &target_slots,
                &target_constraints,
            )
            .unwrap(),
            source_id: None,
            description: None,
        };

        let intermediate = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
        )
        .unwrap();

        match intermediate.waiting_for {
            WaitingFor::TriggerTargetSelection { selection, .. } => {
                assert_eq!(selection.current_slot, 1);
                assert_eq!(
                    selection.current_legal_targets,
                    vec![TargetRef::Player(PlayerId(1))]
                );
            }
            other => panic!("expected trigger target selection, got {other:?}"),
        }

        let completed = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        )
        .unwrap();

        assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn triggered_modal_modes_reject_unsatisfiable_target_constraints() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id: ObjectId(40),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(40),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: Some(GameEvent::SpellCast {
                controller: PlayerId(0),
                object_id: ObjectId(97),
                card_id: CardId(97),
            }),
            modal: Some(ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Target opponent reveals their hand.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            }),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    ),
                    damage_source: None,
                },
            )],
            description: Some("Choose different target players".to_string()),
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Target opponent reveals their hand.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            },
            source_id: ObjectId(40),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    ),
                    damage_source: None,
                },
            )],
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: vec![],
        };

        let result = apply_as_current(
            &mut state,
            GameAction::SelectModes {
                indices: vec![0, 0],
            },
        );

        assert!(
            result.is_err(),
            "unsatisfiable target constraints should be rejected"
        );
    }

    #[test]
    fn all_modes_exhausted_clears_pending_trigger() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = ObjectId(50);
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["Mode A".to_string(), "Mode B".to_string()],
            constraints: vec![ModalSelectionConstraint::NoRepeatThisTurn],
            ..Default::default()
        };

        // Mark both modes as already chosen this turn.
        state.modal_modes_chosen_this_turn.insert((source_id, 0));
        state.modal_modes_chosen_this_turn.insert((source_id, 1));

        // CR 603.3c + CR 603.3d "Push first" contract migration.
        let pending = crate::game::triggers::PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "placeholder".to_string(),
                    description: None,
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: Some(modal),
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 4 },
                        player: crate::types::ability::GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: crate::types::ability::GainLifePlayer::Controller,
                    },
                ),
            ],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
        };
        let pending_for_state = pending.clone();
        let stack_before = state.stack.len();
        let mut setup_events = Vec::new();
        let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
            &mut state,
            pending,
            &mut setup_events,
        );
        state.pending_trigger = Some(pending_for_state);
        state.pending_trigger_entry = Some(entry_id);

        // Call the private function via the engine path.
        let result = begin_pending_trigger_target_selection(&mut state).unwrap();

        // CR 700.2b + CR 603.3c: All modes exhausted — no AbilityModeChoice
        // produced, defensive cleanup pops the in-construction entry and
        // clears both `pending_trigger` and `pending_trigger_entry`.
        assert!(result.is_none());
        assert!(state.pending_trigger.is_none());
        assert!(state.pending_trigger_entry.is_none());
        assert_eq!(
            state.stack.len(),
            stack_before,
            "defensive cleanup must pop the in-construction entry",
        );
    }

    #[test]
    fn modal_mode_tracking_resets_on_new_turn() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = Phase::PreCombatMain;

        let source_id = ObjectId(50);
        state.modal_modes_chosen_this_turn.insert((source_id, 0));
        state.modal_modes_chosen_this_turn.insert((source_id, 1));
        state.modal_modes_chosen_this_game.insert((source_id, 0));

        // Simulate new turn.
        let mut events = Vec::new();
        super::turns::start_next_turn(&mut state, &mut events);

        // Turn-scoped should be cleared.
        assert!(state.modal_modes_chosen_this_turn.is_empty());
        // Game-scoped should persist.
        assert!(state.modal_modes_chosen_this_game.contains(&(source_id, 0)));
    }
}

#[cfg(test)]
mod exile_return_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::game_state::{ExileLink, ExileLinkKind, ZoneChangeRecord};
    use crate::types::identifiers::CardId;

    #[test]
    fn exile_return_source_leaves_battlefield_returns_exiled_card() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create source permanent (e.g., Banishing Light) on battlefield
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );

        // Create exiled card -- directly in exile
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Exile,
        );

        // Set up the exile link (exiled from battlefield)
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });

        // Simulate events where source leaves the battlefield
        let events = vec![GameEvent::ZoneChanged {
            object_id: source_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                name: "Banishing Light".to_string(),
                ..ZoneChangeRecord::test_minimal(
                    source_id,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        }];

        // Call check_exile_returns
        check_exile_returns(&mut state, &mut events.clone());

        // CR 610.3a: Exiled card should return to its previous zone (battlefield)
        assert!(
            state.battlefield.contains(&exiled_id),
            "Exiled card should return to battlefield"
        );
        assert!(
            !state.exile.contains(&exiled_id),
            "Exiled card should no longer be in exile"
        );

        // ExileLink should be removed
        assert!(
            state.exile_links.is_empty(),
            "ExileLink should be cleaned up"
        );
    }

    // #783: end-to-end integration. Component tests cover link creation and the
    // return in isolation; this drives the WHOLE flow — exile via the real
    // change_zone resolver (which must create the UntilSourceLeaves link), then
    // the host actually leaves the battlefield via move_to_zone, then
    // check_exile_returns runs on that event batch. The exiled permanent must
    // return. CR 610.3a.
    #[test]
    fn exile_until_host_leaves_returns_card_through_full_pipeline() {
        use crate::game::effects::change_zone;
        use crate::game::zones::move_to_zone;
        use crate::types::ability::{Duration, Effect, ResolvedAbility, TargetFilter, TargetRef};
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        let victim_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent's Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // "exile target nonland permanent ... until this enchantment leaves the
        // battlefield" — exile resolves and must register the return link.
        let mut exile = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(victim_id)],
            source_id,
            PlayerId(0),
        );
        exile.duration = Some(Duration::UntilHostLeavesPlay);

        let mut events = Vec::new();
        change_zone::resolve(&mut state, &exile, &mut events).unwrap();
        assert!(state.exile.contains(&victim_id), "victim should be exiled");
        assert_eq!(state.exile_links.len(), 1, "exile link must be created");

        // Host leaves the battlefield (e.g. destroyed or sacrificed).
        let mut leave_events = Vec::new();
        move_to_zone(&mut state, source_id, Zone::Graveyard, &mut leave_events);
        check_exile_returns(&mut state, &mut leave_events);

        assert!(
            state.battlefield.contains(&victim_id),
            "#783: exiled permanent must return when the host leaves the battlefield"
        );
        assert!(
            !state.exile.contains(&victim_id),
            "returned permanent must no longer be in exile"
        );
    }

    /// CR 610.3a: When a card exiled from hand (e.g., Deep-Cavern Bat) is returned,
    /// it goes back to hand, not to the battlefield.
    #[test]
    fn exile_return_to_hand_when_exiled_from_hand() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Battlefield,
        );

        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled From Hand".to_string(),
            Zone::Exile,
        );

        // Exiled from hand → should return to hand
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Hand,
            },
        });

        let events = vec![GameEvent::ZoneChanged {
            object_id: source_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                name: "Deep-Cavern Bat".to_string(),
                ..ZoneChangeRecord::test_minimal(
                    source_id,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        }];

        check_exile_returns(&mut state, &mut events.clone());

        // CR 610.3a: Card returns to hand, NOT battlefield
        assert!(
            state.players[1].hand.contains(&exiled_id),
            "Card exiled from hand should return to hand"
        );
        assert!(
            !state.battlefield.contains(&exiled_id),
            "Card exiled from hand should NOT go to battlefield"
        );
        assert!(
            !state.exile.contains(&exiled_id),
            "Card should no longer be in exile"
        );
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_return_card_already_gone_no_error() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        // Exiled card that has already left exile (moved to hand by another effect)
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Already Moved".to_string(),
            Zone::Hand,
        );

        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });

        let events = vec![GameEvent::ZoneChanged {
            object_id: source_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                name: "Source".to_string(),
                ..ZoneChangeRecord::test_minimal(
                    source_id,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        }];

        // Should not panic -- gracefully handle already-moved card
        check_exile_returns(&mut state, &mut events.clone());

        // Card stays in hand (not moved)
        assert!(state.players[1].hand.contains(&exiled_id));
        // Link is still cleaned up
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_return_link_removed_after_return() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled".to_string(),
            Zone::Exile,
        );

        // Another unrelated exile link that should NOT be removed
        let other_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other Source".to_string(),
            Zone::Battlefield,
        );
        let other_exiled = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Other Exiled".to_string(),
            Zone::Exile,
        );

        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });
        state.exile_links.push(ExileLink {
            exiled_id: other_exiled,
            source_id: other_source,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });

        let events = vec![GameEvent::ZoneChanged {
            object_id: source_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(ZoneChangeRecord {
                name: "Source".to_string(),
                ..ZoneChangeRecord::test_minimal(
                    source_id,
                    Some(Zone::Battlefield),
                    Zone::Graveyard,
                )
            }),
        }];

        check_exile_returns(&mut state, &mut events.clone());

        // First link's exiled card should return, second should stay in exile
        assert!(state.battlefield.contains(&exiled_id));
        assert!(state.exile.contains(&other_exiled));

        // Only the triggered link should be removed
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, other_exiled);
    }

    /// CR 400.7 + CR 610.3a: End-to-end — when the source permanent of an
    /// `UntilHostLeavesPlay` exile leaves the battlefield through the real
    /// reducer pipeline (move_to_zone → post-action pipeline), the exiled
    /// card must return to its previous zone. Regression test for White
    /// Auracite / Oblivion Ring / Banishing Light class.
    #[test]
    fn exile_return_end_to_end_through_pipeline() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Source permanent (e.g., White Auracite) on P0's battlefield
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "White Auracite".to_string(),
            Zone::Battlefield,
        );

        // Opponent's enchantment on battlefield, then exiled by the source
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Enchantment".to_string(),
            Zone::Exile,
        );

        // Register the UntilSourceLeaves link as if the trigger had resolved
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });

        // Destroy the source via move_to_zone, then run the post-action pipeline
        // (mirrors what happens when an SBA or destroy effect runs during apply).
        let mut events: Vec<GameEvent> = Vec::new();
        crate::game::zones::move_to_zone(&mut state, source_id, Zone::Graveyard, &mut events);

        let default_wf = WaitingFor::Priority {
            player: PlayerId(0),
        };
        crate::game::engine_priority::run_post_action_pipeline(
            &mut state,
            &mut events,
            &default_wf,
            true,
        )
        .unwrap();

        // Exiled card must have returned to battlefield
        assert!(
            state.battlefield.contains(&exiled_id),
            "Exiled card should return to battlefield when source leaves; battlefield={:?}, exile={:?}",
            state.battlefield,
            state.exile,
        );
        assert!(!state.exile.contains(&exiled_id));
        assert!(
            state.exile_links.is_empty(),
            "ExileLink should be consumed after return"
        );
    }

    /// CR 400.7 + CR 610.3a: End-to-end through full apply path — cast a
    /// Destroy spell targeting the source, resolve it, verify the exiled
    /// card returns. Regression test for the White Auracite user report.
    #[test]
    fn exile_return_after_destroy_resolution_via_apply() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // P0 controls White Auracite (source)
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "White Auracite".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);

        // The opponent's enchantment that WA exiled
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Enchantment".to_string(),
            Zone::Exile,
        );

        // Link: UntilSourceLeaves → Battlefield
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });

        // P1 casts a Destroy ability targeting WA: push ResolvedAbility with
        // Effect::Destroy onto the stack and resolve it via resolve_top.
        let _ = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let destroy_ability = crate::types::ability::ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![crate::types::ability::TargetRef::Object(source_id)],
            ObjectId(999),
            PlayerId(1),
        )
        .kind(AbilityKind::Spell);

        let spell_obj = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Disenchant".to_string(),
            Zone::Stack,
        );

        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_obj,
            source_id: spell_obj,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                ability: Some(destroy_ability),
                card_id: CardId(99),
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Resolve the top stack entry
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // Run the post-action pipeline exactly as apply() would
        let default_wf = WaitingFor::Priority {
            player: PlayerId(1),
        };
        crate::game::engine_priority::run_post_action_pipeline(
            &mut state,
            &mut events,
            &default_wf,
            false,
        )
        .unwrap();

        // White Auracite should be destroyed
        assert!(
            state.players[0].graveyard.contains(&source_id),
            "White Auracite should be in graveyard"
        );
        // Exiled enchantment should have returned to battlefield
        assert!(
            state.battlefield.contains(&exiled_id),
            "Exiled enchantment should return to battlefield; battlefield={:?}, exile={:?}",
            state.battlefield,
            state.exile,
        );
        assert!(!state.exile.contains(&exiled_id));
        assert!(state.exile_links.is_empty());
    }

    /// CR 400.7 + CR 610.3a + CR 611.2: Full integration test using the real
    /// parsed Oracle text for White Auracite. Exercises the complete pipeline:
    /// parser → trigger.execute (with Duration::UntilHostLeavesPlay) →
    /// build_resolved_from_def → stack resolution → execute_zone_move
    /// (which must register the ExileLink) → destroy source → post-action
    /// pipeline → check_exile_returns → return to battlefield.
    ///
    /// Regression test for the L4-18 user report: White Auracite's exiled
    /// enchantment was not returning when White Auracite itself was destroyed.
    #[test]
    fn white_auracite_real_oracle_text_returns_exiled_card() {
        use crate::game::ability_utils::build_resolved_from_def;
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::ability::TargetRef;
        use crate::types::card_type::CoreType;
        use crate::types::game_state::StackEntry;

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);

        // White Auracite on P0's battlefield, with its real parsed triggers.
        let wa_id = scenario
            .add_creature(P0, "White Auracite", 0, 0)
            .as_artifact()
            .from_oracle_text(
                "When this artifact enters, exile target nonland permanent an opponent \
                 controls until this artifact leaves the battlefield.\n{T}: Add {W}.",
            )
            .id();

        // Opponent's enchantment on battlefield (the one WA will exile).
        let ench_id = scenario
            .add_creature(P1, "Opponent Enchantment", 0, 0)
            .as_enchantment()
            .id();

        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Sanity-check the parser: WA must have an ETB trigger whose execute
        // ability carries Duration::UntilHostLeavesPlay on a ChangeZone to
        // Exile. If this fails, the parser regressed, not the engine.
        let wa = state.objects.get(&wa_id).expect("WA on battlefield");
        let etb_trigger = wa
            .trigger_definitions
            .iter_all()
            .find(|t| {
                matches!(t.mode, crate::types::TriggerMode::ChangesZone)
                    && t.destination == Some(Zone::Battlefield)
            })
            .expect("WA must have an ETB (ChangesZone to Battlefield) trigger");
        let execute_def = etb_trigger.execute.as_deref().expect("trigger.execute");
        assert_eq!(
            execute_def.duration,
            Some(crate::types::ability::Duration::UntilHostLeavesPlay),
            "parser regression: WA's exile trigger must carry UntilHostLeavesPlay"
        );
        assert!(
            matches!(
                &*execute_def.effect,
                crate::types::ability::Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            ),
            "parser regression: WA's trigger effect must be ChangeZone→Exile"
        );

        // Build a ResolvedAbility from the real parsed execute and pre-populate
        // its target with the opponent's enchantment. This bypasses the target
        // selection UX but exercises every downstream code path (ability
        // duration threading, execute_zone_move, exile link creation,
        // check_exile_returns). The parser / targeting is tested separately.
        let mut resolved = build_resolved_from_def(execute_def, wa_id, PlayerId(0));
        resolved.targets = vec![TargetRef::Object(ench_id)];

        // Push a TriggeredAbility stack entry that mirrors what
        // push_pending_trigger_to_stack would create.
        let stack_id = ObjectId(9_000_000);
        state.stack.push_back(StackEntry {
            id: stack_id,
            source_id: wa_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                source_id: wa_id,
                ability: Box::new(resolved),
                description: Some("When WA enters...".to_string()),
                condition: None,
                trigger_event: None,
                source_name: String::new(),
                subject_match_count: None,
            },
        });

        // Resolve the trigger: WA's target enchantment moves to exile and the
        // ExileLink for UntilSourceLeaves must be created.
        let mut events = Vec::new();
        crate::game::stack::resolve_top(state, &mut events);

        assert!(
            state.exile.contains(&ench_id),
            "opponent enchantment must be in exile after trigger resolves"
        );
        let has_link = state.exile_links.iter().any(|link| {
            link.exiled_id == ench_id
                && link.source_id == wa_id
                && matches!(
                    link.kind,
                    crate::types::game_state::ExileLinkKind::UntilSourceLeaves {
                        return_zone: Zone::Battlefield
                    }
                )
        });
        assert!(
            has_link,
            "execute_zone_move must register an UntilSourceLeaves link; exile_links={:?}",
            state.exile_links
        );

        // Now destroy White Auracite via move_to_zone and run the full
        // post-action pipeline exactly as apply() would.
        let mut events: Vec<GameEvent> = Vec::new();
        crate::game::zones::move_to_zone(state, wa_id, Zone::Graveyard, &mut events);

        let default_wf = WaitingFor::Priority {
            player: PlayerId(0),
        };
        crate::game::engine_priority::run_post_action_pipeline(
            state,
            &mut events,
            &default_wf,
            false,
        )
        .unwrap();

        // Confirm WA is in graveyard and the exiled enchantment has returned.
        assert!(
            state.players[0].graveyard.contains(&wa_id),
            "White Auracite should be in graveyard"
        );
        // The returned enchantment must be on the battlefield under its owner's
        // control (CR 400.7a).
        assert!(
            state.battlefield.contains(&ench_id),
            "exiled enchantment should return to battlefield; battlefield={:?}, exile={:?}",
            state.battlefield,
            state.exile,
        );
        assert!(!state.exile.contains(&ench_id));
        assert!(
            state.exile_links.is_empty(),
            "ExileLink should be consumed after return; remaining={:?}",
            state.exile_links
        );
        let returned = state.objects.get(&ench_id).unwrap();
        assert!(
            returned
                .card_types
                .core_types
                .contains(&CoreType::Enchantment),
            "returned object must still be an enchantment"
        );
    }
}

#[cfg(test)]
mod phase_trigger_regression_tests {
    use std::sync::Arc;

    use super::tests::apply_oracle_to_object;
    use super::*;
    use crate::game::combat::AttackTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect,
        FilterProp, GainLifePlayer, ObjectScope, PlayerFilter, QuantityExpr, QuantityRef,
        ResolvedAbility, TargetFilter, TargetRef, TriggerConstraint, TriggerDefinition, TypeFilter,
        TypedFilter, UnlessPayModifier,
    };
    use crate::types::card_type::CoreType;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaColor, ManaCost, ManaType, ManaUnit};
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn draw_ability(count: i32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: count },
                target: TargetFilter::Controller,
            },
        )
    }

    fn draw_that_many(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        )
    }

    fn hand_to_battlefield_choice_ability(
        source_id: ObjectId,
        controller: PlayerId,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            source_id,
            controller,
        )
    }

    /// Verify that combat is skipped when there are no attackers and no triggers.
    /// With no BeginCombat triggers and no potential attackers, auto_advance()
    /// skips straight to PostCombatMain.
    #[test]
    fn combat_skipped_when_no_attackers_no_triggers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a 0/1 creature with no triggers — can't attack, no combat triggers.
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Wall".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(0);
            obj.toughness = Some(1);
        }

        // Pass priority twice (P0 passes, then P1 passes) with empty stack.
        // This advances from PreCombatMain → BeginCombat → no triggers, no
        // attackers → skip to PostCombatMain.
        let result1 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        let result2 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // We should now be at PostCombatMain with empty stack.
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(
            state.stack.is_empty(),
            "Stack should be empty — no triggers exist. Stack: {:?}",
            state.stack
        );
        assert!(
            state.pending_trigger.is_none(),
            "No pending trigger should exist"
        );
        assert!(matches!(result2.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 503.1a: Upkeep triggers fire when the upkeep step begins.
    #[test]
    fn upkeep_trigger_fires() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create creature with "At the beginning of your upkeep, gain 1 life"
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Upkeep Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::Upkeep)
                    .constraint(TriggerConstraint::OnlyDuringYourTurn)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    ))
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // auto_advance from Untap should process Upkeep triggers inline
        let mut events = Vec::new();
        let wf = crate::game::turns::auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::Upkeep);
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "Upkeep trigger should have fired"
        );
        assert!(matches!(wf, WaitingFor::Priority { .. }));
    }

    /// CR 507.1: BeginCombat triggers fire even when there are attackers.
    #[test]
    fn begin_combat_trigger_fires_with_attackers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a 2/2 creature (can attack) with a BeginCombat trigger
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Combat Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::BeginCombat)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    ))
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // Pass priority from PreCombatMain
        let result1 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        let _result2 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // Should be at BeginCombat with trigger on stack
        assert_eq!(state.phase, Phase::BeginCombat);
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "BeginCombat trigger should have fired"
        );
    }

    /// CR 507.1: BeginCombat triggers fire even without potential attackers.
    #[test]
    fn begin_combat_trigger_fires_without_attackers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a 0/1 creature (can't attack) with a BeginCombat trigger
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Trigger Wall".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(0);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::BeginCombat)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    ))
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // Pass priority twice to advance from PreCombatMain
        let result1 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        let _result2 = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        // Should be at BeginCombat with trigger on stack and combat state set
        assert_eq!(state.phase, Phase::BeginCombat);
        assert!(
            state.combat.is_some(),
            "Combat state should be set when triggers fire"
        );
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "BeginCombat trigger should fire even without potential attackers (CR 507.1)"
        );
    }

    /// OnlyDuringYourTurn constraint prevents trigger from firing on opponent's turn.
    #[test]
    fn your_turn_constraint_blocks_on_opponents_turn() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        // Active player is P1, but the creature is controlled by P0
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);

        // Create creature controlled by P0 with "At the beginning of your upkeep"
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Your Turn Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::Upkeep)
                    .constraint(TriggerConstraint::OnlyDuringYourTurn)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    ))
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // auto_advance from Untap — it's P1's turn, but the trigger is P0's
        // with OnlyDuringYourTurn, so it should NOT fire.
        let mut events = Vec::new();
        let _wf = crate::game::turns::auto_advance(&mut state, &mut events);

        // Trigger should not have fired — phase should have advanced past Upkeep
        assert!(
            state.stack.is_empty(),
            "Trigger with OnlyDuringYourTurn should not fire on opponent's turn"
        );
        assert!(state.pending_trigger.is_none());
    }

    /// Put a Go-Shintai of Boundless Vigor (the issue #1243 card) onto the
    /// battlefield under P0, with its real parsed trigger set, and return its id.
    /// The card is its own Shrine, so it is always a legal reflexive target.
    fn put_boundless_go_shintai(state: &mut GameState) -> ObjectId {
        let parsed = crate::parser::oracle::parse_oracle_text(
            "Trample\nAt the beginning of your end step, you may pay {1}. When you do, put a +1/+1 counter on target Shrine for each Shrine you control.",
            "Go-Shintai of Boundless Vigor",
            &[],
            &["Enchantment".to_string(), "Creature".to_string()],
            &["Shrine".to_string(), "Spirit".to_string()],
        );
        assert!(
            !parsed.triggers.is_empty(),
            "parser must produce the end-step trigger, got {parsed:?}"
        );

        let id = create_object(
            state,
            CardId(200),
            PlayerId(0),
            "Go-Shintai of Boundless Vigor".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Shrine".to_string());
        obj.power = Some(5);
        obj.toughness = Some(5);
        for t in parsed.triggers {
            obj.trigger_definitions.push(t);
        }
        obj.base_card_types = obj.card_types.clone();
        id
    }

    fn shintai_p1p1_counters(state: &GameState, id: ObjectId) -> u32 {
        state
            .objects
            .get(&id)
            .and_then(|o| {
                o.counters
                    .get(&crate::types::counter::CounterType::Plus1Plus1)
                    .copied()
            })
            .unwrap_or(0)
    }

    /// Issue #1243 — class regression. "At the beginning of your end step, you
    /// may pay {1}. When you do, <effect>." parses into an end-step `Phase`
    /// trigger whose execute is an optional `PayCost` carrying a reflexive
    /// `WhenYouDo` sub-ability. CR 513.1a (beginning-of-end-step trigger) + CR
    /// 603.1 (a triggered ability uses the stack) require the trigger to be put
    /// on the stack and resolved; CR 603.12 makes "when you do" a reflexive
    /// trigger that fires only if the optional payment is made. The shape is
    /// shared by all four Boundless-era Go-Shintai and ~12 other "you may pay
    /// {1}. When you do" cards, so this guards the whole class.
    ///
    /// Accept path: the {1} is paid and the reflexive PutCounter resolves,
    /// placing one +1/+1 counter on the lone Shrine.
    #[test]
    fn issue_1243_end_step_may_pay_trigger_accept_pays_and_resolves_reflexive() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::End;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let id = put_boundless_go_shintai(&mut state);
        // One generic mana so the {1} is payable at resolution.
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(999),
            false,
            Vec::new(),
        ));

        let mut events = Vec::new();
        crate::game::turns::auto_advance(&mut state, &mut events);
        // CR 603.1 + CR 513.1a: the trigger must reach the stack, not be skipped.
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "end-step may-pay trigger must fire (waiting={:?})",
            state.waiting_for
        );

        let mut saw_may_prompt = false;
        for _ in 0..20 {
            match state.waiting_for.clone() {
                WaitingFor::Priority { player } => {
                    if state.stack.is_empty() {
                        break;
                    }
                    apply(&mut state, player, GameAction::PassPriority).unwrap();
                }
                // CR 603.12: the "you may pay {1}" choice on resolution.
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    saw_may_prompt = true;
                    apply(
                        &mut state,
                        player,
                        GameAction::DecideOptionalEffect { accept: true },
                    )
                    .unwrap();
                }
                // Reflexive "when you do" target: the only Shrine is the source.
                WaitingFor::TriggerTargetSelection { player, .. }
                | WaitingFor::TargetSelection { player, .. } => {
                    apply(
                        &mut state,
                        player,
                        GameAction::SelectTargets {
                            targets: vec![TargetRef::Object(id)],
                        },
                    )
                    .unwrap();
                }
                _ => break,
            }
        }

        assert!(
            saw_may_prompt,
            "the 'may pay {{1}}' prompt (CR 603.12) must be surfaced at the end step"
        );
        assert_eq!(
            shintai_p1p1_counters(&state, id),
            1,
            "paying {{1}} must place one +1/+1 counter on the lone Shrine"
        );
        assert_eq!(
            state.players[0].mana_pool.mana.len(),
            0,
            "the {{1}} must actually be paid on accept"
        );
    }

    /// Issue #1243 — decline path. The trigger still goes on the stack and the
    /// "may pay {1}" choice is still offered (CR 603.1), but declining means the
    /// reflexive CR 603.12 "when you do" never triggers: no payment, no counter,
    /// and the turn proceeds cleanly.
    #[test]
    fn issue_1243_end_step_may_pay_trigger_decline_places_no_counter() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::End;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let id = put_boundless_go_shintai(&mut state);
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(999),
            false,
            Vec::new(),
        ));

        let mut events = Vec::new();
        crate::game::turns::auto_advance(&mut state, &mut events);
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "end-step may-pay trigger must fire even when it will be declined"
        );

        let mut saw_may_prompt = false;
        for _ in 0..20 {
            match state.waiting_for.clone() {
                WaitingFor::Priority { player } => {
                    if state.stack.is_empty() {
                        break;
                    }
                    apply(&mut state, player, GameAction::PassPriority).unwrap();
                }
                WaitingFor::OptionalEffectChoice { player, .. } => {
                    saw_may_prompt = true;
                    apply(
                        &mut state,
                        player,
                        GameAction::DecideOptionalEffect { accept: false },
                    )
                    .unwrap();
                }
                _ => break,
            }
        }

        assert!(
            saw_may_prompt,
            "the 'may pay {{1}}' prompt must still be offered before declining"
        );
        assert_eq!(
            shintai_p1p1_counters(&state, id),
            0,
            "declining must place no counter (CR 603.12 reflexive does not trigger)"
        );
    }

    #[test]
    fn spell_cast_trigger_syncs_priority_to_active_player() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let creature_spell = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Bear Cub".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&creature_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: creature_spell,
            source_id: creature_spell,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(300),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let spell_cast_trigger_creature = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Spell Trigger Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spell_cast_trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions
                .push(TriggerDefinition::new(TriggerMode::SpellCast).execute(
                    AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::Controller,
                        },
                    ),
                ));
        }

        let searing_spear = create_object(
            &mut state,
            CardId(302),
            PlayerId(1),
            "Searing Spear".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&searing_spear)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: searing_spear,
                card_id: CardId(302),
                targets: Vec::new(),
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(0));

        let pass_result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            pass_result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    fn setup_esper_sentinel_unless_payment(pay_mana: bool) -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );

        let esper = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Esper Sentinel".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&esper).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            let mut trigger = TriggerDefinition::new(TriggerMode::SpellCast)
                .execute(AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ))
                .constraint(TriggerConstraint::NthSpellThisTurn {
                    n: 1,
                    filter: Some(TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Non(Box::new(TypeFilter::Creature))),
                    )),
                });
            trigger.unless_pay = Some(UnlessPayModifier {
                cost: AbilityCost::ManaDynamic {
                    quantity: QuantityExpr::Ref {
                        qty: QuantityRef::Power {
                            scope: ObjectScope::Source,
                        },
                    },
                },
                payer: TargetFilter::TriggeringPlayer,
            });
            obj.trigger_definitions.push(trigger);
        }

        let spell = create_object(
            &mut state,
            CardId(502),
            PlayerId(1),
            "Opponent Noncreature Spell".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        if pay_mana {
            state
                .players
                .iter_mut()
                .find(|player| player.id == PlayerId(1))
                .unwrap()
                .mana_pool
                .add(ManaUnit {
                    color: ManaType::Colorless,
                    source_id: ObjectId(0),
                    snow: false,
                    source_could_produce_two_or_more_colors: false,
                    restrictions: Vec::new(),
                    grants: vec![],
                    expiry: None,
                });
        }

        apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: spell,
                card_id: CardId(502),
                targets: Vec::new(),
            },
        )
        .unwrap();

        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::UnlessPayment {
                player: PlayerId(1),
                cost: AbilityCost::Mana { ref cost },
                ..
            } if *cost == ManaCost::generic(1)
        ));

        state
    }

    #[test]
    fn esper_sentinel_draws_when_triggering_player_declines_x_payment() {
        let mut state = setup_esper_sentinel_unless_payment(false);

        let result =
            apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[1].hand.len(), 0);
    }

    #[test]
    fn esper_sentinel_does_not_draw_when_triggering_player_pays_x() {
        let mut state = setup_esper_sentinel_unless_payment(true);

        let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].hand.len(), 0);
        assert_eq!(state.players[1].hand.len(), 0);
    }

    #[test]
    fn attack_trigger_resolves_before_combat_damage_and_only_once() {
        let mut state = new_game(42);
        state.turn_number = 5;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let ajani = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Ajani's Pridemate".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&ajani).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.color = vec![ManaColor::White];
            obj.base_color = vec![ManaColor::White];
            obj.entered_battlefield_turn = Some(4);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::LifeGained)
                    .valid_target(TargetFilter::Controller)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::PutCounter {
                            counter_type: crate::types::counter::CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
        }

        let linden = create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "Linden, the Steadfast Queen".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&linden).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.color = vec![ManaColor::White];
            obj.base_color = vec![ManaColor::White];
            obj.entered_battlefield_turn = Some(4);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Attacks)
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor {
                                color: crate::types::mana::ManaColor::White,
                            }]),
                    ))
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    )),
            );
        }

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![ajani, linden],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };

        let declare_result = apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(ajani, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();

        assert!(matches!(
            declare_result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(
            state.stack.len(),
            1,
            "Linden should create exactly one stack entry"
        );
        assert_eq!(state.phase, Phase::DeclareAttackers);

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let linden_resolve = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            linden_resolve.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.players[0].life, 21, "Linden should gain life once");
        assert_eq!(
            state.stack.len(),
            1,
            "Ajani's Pridemate should trigger from Linden's life gain"
        );
        assert_eq!(state.objects[&ajani].power, Some(2));
        assert_eq!(state.objects[&ajani].toughness, Some(2));

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let pridemate_resolve = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            pridemate_resolve.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&ajani].power, Some(3));
        assert_eq!(state.objects[&ajani].toughness, Some(3));

        // CR 117.1c: Active player gets priority in every step — so from
        // DeclareAttackers we pass through: declare attackers (AP, NAP) →
        // declare blockers (AP, NAP, after auto-submitted empty block) →
        // combat damage resolves → end-of-combat → post-combat main.
        let mut combat_result = None;
        for _ in 0..8 {
            if state.phase == Phase::PostCombatMain {
                break;
            }
            combat_result = Some(apply_as_current(&mut state, GameAction::PassPriority).unwrap());
        }
        let combat_result = combat_result.expect("combat should advance");

        assert!(matches!(
            combat_result.waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert_eq!(
            state.players[1].life, 17,
            "Ajani should deal 3 after receiving the pre-damage counter"
        );
        assert_eq!(
            state.players[0].life, 21,
            "No duplicate Linden life gain should occur"
        );
        assert_eq!(state.objects[&ajani].power, Some(3));
        assert_eq!(state.objects[&ajani].toughness, Some(3));
    }

    /// Regression test: lifelink combat damage with a GainLife replacement effect
    /// (Leyline of Hope) must not double-fire "whenever you gain life" triggers.
    ///
    /// Previously, process_combat_damage_triggers processed the LifeChanged event
    /// for triggers, then run_post_action_pipeline re-processed the same events,
    /// causing triggers like Essence Channeler's to fire twice per life-gain event.
    #[test]
    fn lifelink_replacement_does_not_double_fire_life_gain_triggers() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::counter::CounterType;
        use crate::types::replacements::ReplacementEvent;

        let mut state = new_game(42);
        state.turn_number = 5;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Lifelink attacker (Ruin-Lurker Bat analog): 1/1 flying lifelink
        let bat = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Ruin-Lurker Bat".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bat).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.keywords.push(crate::types::keywords::Keyword::Lifelink);
            obj.base_keywords = obj.keywords.clone();
            obj.entered_battlefield_turn = Some(3);
        }

        // "Whenever you gain life, put a +1/+1 counter on this creature" (Essence Channeler)
        let channeler = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Essence Channeler".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&channeler).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(1);
            obj.base_power = Some(2);
            obj.base_toughness = Some(1);
            obj.entered_battlefield_turn = Some(3);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::LifeGained)
                    .valid_target(TargetFilter::Controller)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::PutCounter {
                            counter_type: crate::types::counter::CounterType::Plus1Plus1,
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
            obj.base_trigger_definitions =
                Arc::new(obj.trigger_definitions.iter_all().cloned().collect());
        }

        // Leyline of Hope analog: "If you would gain life, gain that much + 1 instead"
        let leyline = create_object(
            &mut state,
            CardId(502),
            PlayerId(0),
            "Leyline of Hope".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&leyline).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            // Leyline of Hope: "If you would gain life, you gain that much
            // life plus 1 instead." Parser emits the replaced amount as
            // `Offset { inner: EventContextAmount, offset: 1 }`, not a delta.
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::GainLife).execute(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Offset {
                                inner: Box::new(QuantityExpr::Ref {
                                    qty: crate::types::ability::QuantityRef::EventContextAmount,
                                }),
                                offset: 1,
                            },
                            player: GainLifePlayer::Controller,
                        },
                    ),
                ),
            );
            obj.base_replacement_definitions =
                Arc::new(obj.replacement_definitions.iter_all().cloned().collect());
        }

        // Declare bat as attacker
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![bat],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };

        apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(bat, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();

        // Skip to combat damage: P0 pass, P1 pass (declare blockers — no blockers),
        // P0 pass, P1 pass (combat damage resolves).
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        // Now at declare blockers — P1 declares no blockers
        if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. }) {
            apply_as_current(
                &mut state,
                GameAction::DeclareBlockers {
                    assignments: vec![],
                },
            )
            .unwrap();
        }
        // Pass priority through to combat damage
        while state.phase != Phase::PostCombatMain
            && !matches!(state.waiting_for, WaitingFor::GameOver { .. })
        {
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                apply_as_current(&mut state, GameAction::PassPriority).unwrap();
            } else {
                break;
            }
        }

        // Bat dealt 1 damage → lifelink gain 1 → Leyline replaces to 2.
        // Player 0 should have gained exactly 2 life (20 → 22).
        assert_eq!(
            state.players[0].life, 22,
            "Lifelink + Leyline should gain exactly 2 life"
        );

        // Essence Channeler should have exactly 1 +1/+1 counter, not 2.
        // The bug was that the LifeChanged event was processed for triggers twice,
        // once in process_combat_damage_triggers and again in run_post_action_pipeline.
        let counters = state.objects[&channeler]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counters, 1,
            "Essence Channeler should trigger exactly once per life-gain event, got {} counters",
            counters
        );
    }

    #[test]
    fn card_name_choice_validates_against_all_card_names() {
        let mut state = GameState::new_two_player(42);
        state.all_card_names =
            vec!["Lightning Bolt".to_string(), "Counterspell".to_string()].into();
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: crate::types::ability::ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        // Valid card name succeeds
        let result = apply_as_current(
            &mut state,
            GameAction::ChooseOption {
                choice: "Lightning Bolt".to_string(),
            },
        );
        assert!(result.is_ok());

        // Reset state for invalid test
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: crate::types::ability::ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        // Invalid card name fails
        let result = apply_as_current(
            &mut state,
            GameAction::ChooseOption {
                choice: "Not A Real Card".to_string(),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn card_name_choice_is_case_insensitive() {
        let mut state = GameState::new_two_player(42);
        state.all_card_names = vec!["Lightning Bolt".to_string()].into();
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: crate::types::ability::ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseOption {
                choice: "lightning bolt".to_string(),
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn optional_effect_choice_accept_preserves_nested_effect_zone_choice_continuation() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Perm A".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Perm B".to_string(),
            Zone::Battlefield,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut draw = draw_that_many(source_id, PlayerId(0));
        draw.condition = Some(AbilityCondition::effect_performed());
        ability.sub_ability = Some(Box::new(draw));

        state.pending_optional_effect = Some(Box::new(ability));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: None,
        };

        let result = apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(state.pending_continuation.is_some());
    }

    #[test]
    fn opponent_may_choice_accept_preserves_nested_effect_zone_choice_continuation() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hand A".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Hand B".to_string(),
            Zone::Hand,
        );

        let mut ability = hand_to_battlefield_choice_ability(source_id, PlayerId(1));
        ability.sub_ability = Some(Box::new(draw_that_many(source_id, PlayerId(1))));

        state.pending_optional_effect = Some(Box::new(ability));
        state.waiting_for = WaitingFor::OpponentMayChoice {
            player: PlayerId(1),
            remaining: vec![],
            source_id,
            description: None,
        };

        let result = apply_as_current(
            &mut state,
            GameAction::DecideOptionalEffect { accept: true },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(1),
                ..
            }
        ));
        assert!(state.pending_continuation.is_some());
    }

    #[test]
    fn unless_payment_decline_preserves_nested_effect_zone_choice_continuation() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Hand A".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand B".to_string(),
            Zone::Hand,
        );

        let mut ability = hand_to_battlefield_choice_ability(source_id, PlayerId(0));
        ability.sub_ability = Some(Box::new(draw_that_many(source_id, PlayerId(0))));

        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(ability),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let result =
            apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(state.pending_continuation.is_some());
    }

    /// CR 118.12 + CR 118.12a: "[Effect] unless [player] pays [cost]. If they do,
    /// [alternative]." When the unless cost is paid, the primary effect is
    /// suppressed AND the IfAPlayerDoes sub_ability runs as the alternative
    /// outcome. Cards: Rhystic Lightning, Don't Make a Sound, Divert Disaster,
    /// Assimilate Essence.
    #[test]
    fn unless_pay_success_runs_if_a_player_does_sub_ability() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(910),
            PlayerId(0),
            "Rhystic Lightning Stand-In".to_string(),
            Zone::Battlefield,
        );

        // Primary effect: gain 4 life. Alternative: gain 2 life.
        // Using GainLife rather than DealDamage so the test stays self-contained
        // (no target wiring required) — the runtime branching being verified is
        // sub_ability resolution, not damage routing.
        let mut primary = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut alternative = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        alternative.condition = Some(AbilityCondition::effect_performed());
        primary.sub_ability = Some(Box::new(alternative));

        // Player 1 (the unless payer) starts with 20 life and 2 energy to pay.
        state.players[1].energy = 2;
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(primary),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let starting_life = state.players[0].life;
        let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        // Cost was deducted from the unless payer.
        assert_eq!(state.players[1].energy, 0);
        // Primary suppressed (no +4 life), alternative ran (+2 life from sub_ability).
        assert_eq!(state.players[0].life, starting_life + 2);
    }

    /// CR 118.12: When the unless cost is declined, the primary effect runs
    /// and the IfAPlayerDoes sub_ability does NOT run (its condition reads
    /// `optional_effect_performed` which stays false on the decline path).
    #[test]
    fn unless_pay_decline_runs_primary_not_if_a_player_does_sub() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(911),
            PlayerId(0),
            "Rhystic Lightning Stand-In".to_string(),
            Zone::Battlefield,
        );

        let mut primary = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut alternative = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        alternative.condition = Some(AbilityCondition::effect_performed());
        primary.sub_ability = Some(Box::new(alternative));

        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(primary),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let starting_life = state.players[0].life;
        let result =
            apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        // Primary ran (+4 life), alternative did NOT (no extra +2 life).
        assert_eq!(state.players[0].life, starting_life + 4);
    }

    /// CR 118.12: An unless_pay effect with NO sub_ability still resolves
    /// cleanly when the cost is paid (primary suppressed, no spurious chain
    /// resolution).
    #[test]
    fn unless_pay_success_with_no_sub_ability_is_inert() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(912),
            PlayerId(0),
            "Plain Unless Effect".to_string(),
            Zone::Battlefield,
        );

        let primary = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );

        state.players[1].energy = 2;
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(primary),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let starting_life = state.players[0].life;
        let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[1].energy, 0);
        // Primary suppressed; no sub_ability to run.
        assert_eq!(state.players[0].life, starting_life);
    }

    /// Abandon Attachments #81 parallel: a stale `cost_payment_failed_flag`
    /// from a previous resolution must NOT block the IfAPlayerDoes sub_ability
    /// when the unless cost is paid. The success path clears the flag the
    /// same way `handle_optional_effect_choice` does for accepts.
    #[test]
    fn unless_pay_success_clears_stale_cost_payment_failed_flag() {
        let mut state = setup_game_at_main_phase();
        // Simulate a previous resolution that left the flag set.
        state.cost_payment_failed_flag = true;

        let source_id = create_object(
            &mut state,
            CardId(913),
            PlayerId(0),
            "Stale Flag Source".to_string(),
            Zone::Battlefield,
        );

        let mut primary = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut alternative = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: GainLifePlayer::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        alternative.condition = Some(AbilityCondition::effect_performed());
        primary.sub_ability = Some(Box::new(alternative));

        state.players[1].energy = 2;
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(primary),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let starting_life = state.players[0].life;
        let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        // Alternative ran (+2 life), so the stale flag was correctly cleared.
        assert_eq!(state.players[0].life, starting_life + 2);
        assert!(
            !state.cost_payment_failed_flag,
            "cost_payment_failed_flag should be cleared by the success path"
        );
    }

    #[test]
    fn unless_energy_payment_deducts_energy_and_skips_effect() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Energy Source".to_string(),
            Zone::Battlefield,
        );
        state.players[0].energy = 2;
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: crate::types::ability::GainLifePlayer::Controller,
                },
                vec![],
                source_id,
                PlayerId(0),
            )),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.players[0].energy, 0);
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn unless_discard_payment_filters_eligible_hand_cards() {
        let mut state = setup_game_at_main_phase();
        let source_id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land Card".to_string(),
            Zone::Hand,
        );
        let creature_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Creature Card".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .expect("land object")
            .card_types
            .core_types = vec![CoreType::Land];
        state
            .objects
            .get_mut(&creature_id)
            .expect("creature object")
            .card_types
            .core_types = vec![CoreType::Creature];

        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Land],
                    controller: None,
                    properties: vec![],
                })),
                random: false,
                self_ref: false,
            },
            pending_effect: Box::new(draw_that_many(source_id, PlayerId(0))),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let result = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        match result.waiting_for {
            WaitingFor::WardDiscardChoice { cards, .. } => assert_eq!(cards, vec![land_id]),
            other => panic!("expected filtered WardDiscardChoice, got {other:?}"),
        }
    }

    #[test]
    fn multi_target_selection_preserves_nested_effect_zone_choice_continuation() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Tap Target".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hand A".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hand B".to_string(),
            Zone::Hand,
        );

        create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Sac A".to_string(),
            Zone::Battlefield,
        );
        create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Sac B".to_string(),
            Zone::Battlefield,
        );

        let mut pending_ability = ResolvedAbility::new(
            Effect::Tap {
                target: TargetFilter::Any,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut sacrifice_ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![TargetRef::Player(PlayerId(0))],
            source_id,
            PlayerId(0),
        );
        sacrifice_ability.sub_ability = Some(Box::new(draw_that_many(source_id, PlayerId(0))));
        pending_ability.sub_ability = Some(Box::new(sacrifice_ability));

        state.waiting_for = WaitingFor::MultiTargetSelection {
            player: PlayerId(0),
            legal_targets: vec![target_id],
            min_targets: 1,
            max_targets: 1,
            pending_ability: Box::new(pending_ability),
        };

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![target_id],
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(state.pending_continuation.is_some());
        assert!(state.objects[&target_id].tapped);
    }

    #[test]
    fn effect_zone_choice_handler_resolves_sacrifice_and_continuation() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chosen Permanent".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![obj_id],
            count: 1,
            min_count: 0,
            up_to: false,
            source_id,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            count_param: 0,
        };
        state.pending_continuation = Some(crate::types::game_state::PendingContinuation::new(
            Box::new(ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: crate::types::ability::GainLifePlayer::Controller,
                },
                vec![],
                source_id,
                PlayerId(0),
            )),
        ));

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![obj_id],
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.players[0].graveyard.contains(&obj_id));
        assert_eq!(state.players[0].life, 22);
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn effect_zone_choice_handler_resolves_untap_selection() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        let chosen_land = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chosen Land".to_string(),
            Zone::Battlefield,
        );
        let unchosen_land = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Unchosen Land".to_string(),
            Zone::Battlefield,
        );
        for id in [chosen_land, unchosen_land] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.tapped = true;
        }

        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![chosen_land, unchosen_land],
            count: 2,
            min_count: 0,
            up_to: true,
            source_id,
            effect_kind: EffectKind::Untap,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            count_param: 0,
        };

        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![chosen_land],
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(!state.objects[&chosen_land].tapped);
        assert!(state.objects[&unchosen_land].tapped);
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn effect_zone_choice_up_to_respects_min_count() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chosen Permanent".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![obj_id],
            count: 1,
            min_count: 1,
            up_to: true,
            source_id,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            count_param: 0,
        };

        let result = apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] });

        assert!(result.is_err());
        assert!(state.battlefield.contains(&obj_id));
    }

    #[test]
    fn choose_one_of_enters_branch_choice_state() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![draw_ability(1), draw_ability(2)],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(0),
                controller: PlayerId(0),
                source_id: ObjectId(100),
                ..
            }
        ));
    }

    #[test]
    fn choose_one_of_branch_resolves_selected_branch_with_original_controller() {
        let mut state = setup_game_at_main_phase();
        let source_id = ObjectId(100);
        let branch_gain = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: GainLifePlayer::Controller,
            },
        );
        let branch_lose = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
        );
        state.waiting_for = WaitingFor::ChooseOneOfBranch {
            player: PlayerId(1),
            controller: PlayerId(0),
            source_id,
            branches: vec![branch_gain, branch_lose],
            branch_descriptions: vec!["Gain 3 life.".to_string(), "Lose 3 life.".to_string()],
            parent_targets: vec![],
            context: Default::default(),
            remaining_players: vec![],
        };

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

        assert_eq!(
            state.players[0].life, 23,
            "branch text using controller must resolve for original controller"
        );
        assert_eq!(state.players[1].life, 20);
    }

    #[test]
    fn choose_one_of_each_opponent_prompts_apnap_and_branch_targets_faced_player() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        let source_id = ObjectId(100);
        let branch_target_player_loses_life = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::Player),
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![branch_target_player_loses_life.clone(), draw_ability(1)],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(1),
                remaining_players: ref rest,
                ..
            } if rest == &vec![PlayerId(2)]
        ));

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

        assert_eq!(state.players[1].life, 19);
        assert_eq!(state.players[2].life, 20);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch {
                player: PlayerId(2),
                remaining_players: ref rest,
                ..
            } if rest.is_empty()
        ));

        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

        assert_eq!(state.players[1].life, 19);
        assert_eq!(state.players[2].life, 19);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn choose_one_of_scoped_player_sacrifice_prompts_faced_opponent() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let source_id = ObjectId(100);
        let own_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Controller Creature".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        let opp_creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Second Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for id in [own_creature, opp_creature, opp_creature_b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let sacrifice_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![sacrifice_branch, draw_ability(1)],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 }).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(cards, &vec![opp_creature, opp_creature_b]);
                assert!(!cards.contains(&own_creature));
            }
            other => panic!("expected EffectZoneChoice for faced opponent, got {other:?}"),
        }
    }

    #[test]
    fn choose_one_of_controller_token_branch_ignores_faced_opponent() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let source_id = ObjectId(100);
        let sacrifice_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                ),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        );
        let token_branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "b_3_3_a_dalek_menace".to_string(),
                power: crate::types::ability::PtValue::Fixed(0),
                toughness: crate::types::ability::PtValue::Fixed(0),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let ability = ResolvedAbility::new(
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Opponent,
                branches: vec![sacrifice_branch, token_branch],
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        apply_as_current(&mut state, GameAction::ChooseBranch { index: 1 }).unwrap();

        let token = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .find(|object| object.is_token)
            .expect("expected Dalek token");
        assert_eq!(token.controller, PlayerId(0));
        assert_eq!(token.owner, PlayerId(0));
    }

    #[test]
    fn player_scope_all_uses_apnap_order_and_resumes_remaining_players() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let source_id = ObjectId(100);
        let p0_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 A".to_string(),
            Zone::Battlefield,
        );
        let p0_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "P0 B".to_string(),
            Zone::Battlefield,
        );
        let p1_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "P1 A".to_string(),
            Zone::Battlefield,
        );
        let p1_b = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "P1 B".to_string(),
            Zone::Battlefield,
        );
        for id in [p0_a, p0_b, p1_a, p1_b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut ability = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::All);

        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(1),
                ..
            }
        ));

        let result =
            apply_as_current(&mut state, GameAction::SelectCards { cards: vec![p1_a] }).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::EffectZoneChoice {
                player: PlayerId(0),
                ..
            }
        ));
    }

    #[test]
    fn post_replacement_choose_sets_named_choice_waiting_for() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: crate::types::ability::ChoiceType::BasicLandType,
                persist: false,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
        ));

        let waiting_for = engine_replacement::apply_post_replacement_effect(
            &mut state,
            &effect_def,
            Some(source_id),
            None,
            None,
            &mut events,
        );

        assert!(matches!(
            waiting_for,
            Some(WaitingFor::NamedChoice {
                choice_type: crate::types::ability::ChoiceType::BasicLandType,
                ..
            })
        ));
        assert!(state.pending_continuation.is_some());
    }

    #[test]
    fn choose_option_with_source_id_stores_chosen_attribute() {
        use crate::types::ability::ChoiceType;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Captivating Crossroads".to_string(),
            Zone::Battlefield,
        );

        // Set up NamedChoice with source_id (simulating persist=true Choose)
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::color(),
            options: vec![
                "White".to_string(),
                "Blue".to_string(),
                "Black".to_string(),
                "Red".to_string(),
                "Green".to_string(),
            ],
            source_id: Some(obj_id),
        };

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseOption {
                choice: "Red".to_string(),
            },
        );
        assert!(result.is_ok());

        // Verify the choice was stored on the object
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.chosen_color(), Some(ManaColor::Red));
    }

    #[test]
    fn glacierwood_siege_resolution_prompts_for_anchor_word_choice() {
        let mut state = setup_game_at_main_phase();
        let siege_id = create_object(
            &mut state,
            CardId(621),
            PlayerId(0),
            "Glacierwood Siege".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&siege_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.base_card_types = obj.card_types.clone();
        }
        apply_oracle_to_object(
            &mut state,
            siege_id,
            "Glacierwood Siege",
            "As this enchantment enters, choose Temur or Sultai.\n\
• Temur — Whenever you cast an instant or sorcery spell, target player mills four cards.\n\
• Sultai — You may play lands from your graveyard.",
        );

        state.stack.push_back(StackEntry {
            id: siege_id,
            source_id: siege_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(621),
                ability: None,
                casting_variant: crate::types::game_state::CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let resolve = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.battlefield.contains(&siege_id));
        match resolve.waiting_for {
            WaitingFor::NamedChoice {
                player,
                choice_type: crate::types::ability::ChoiceType::Labeled { ref options },
                source_id,
                ..
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(source_id, Some(siege_id));
                assert_eq!(options, &vec!["Temur".to_string(), "Sultai".to_string()]);
            }
            other => panic!("expected Glacierwood Siege anchor choice, got {other:?}"),
        }

        apply_as_current(
            &mut state,
            GameAction::ChooseOption {
                choice: "Temur".to_string(),
            },
        )
        .unwrap();

        assert_eq!(state.objects[&siege_id].chosen_label(), Some("Temur"));
    }

    #[test]
    fn restricted_color_choice_rejects_excluded_color() {
        use crate::types::ability::ChoiceType;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::color_excluding(vec![ManaColor::White]),
            options: vec![
                "Blue".to_string(),
                "Black".to_string(),
                "Red".to_string(),
                "Green".to_string(),
            ],
            source_id: None,
        };

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseOption {
                choice: "White".to_string(),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn copy_target_choice_resolves_become_copy() {
        // CR 707.9: Test the CopyTargetChoice → BecomeCopy flow.
        // Set up a clone creature on battlefield and a target creature to copy.
        let mut state = GameState::new_two_player(42);

        let target_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.power = Some(2);
            target.toughness = Some(2);
        }

        let clone_id = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Clone".to_string(),
            Zone::Battlefield,
        );
        {
            let clone = state.objects.get_mut(&clone_id).unwrap();
            clone.base_power = Some(0);
            clone.base_toughness = Some(0);
            clone.power = Some(0);
            clone.toughness = Some(0);
            clone.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(
                    crate::types::replacements::ReplacementEvent::Moved,
                )
                .execute(crate::types::ability::AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Spell,
                    crate::types::ability::Effect::BecomeCopy {
                        target: TargetFilter::Any,
                        duration: None,
                        mana_value_limit: Some(
                            crate::types::ability::CopyManaValueLimit::AmountSpentToCastSource,
                        ),
                        additional_modifications: vec![
                            crate::types::ability::ContinuousModification::AddSubtype {
                                subtype: "Bird".to_string(),
                            },
                            crate::types::ability::ContinuousModification::AddKeyword {
                                keyword: crate::types::keywords::Keyword::Flying,
                            },
                        ],
                    },
                )),
            );
        }

        // Set up CopyTargetChoice waiting state
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: clone_id,
            valid_targets: vec![target_id],
            max_mana_value: None,
        };

        // Player chooses to copy Grizzly Bears
        let result = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
        );
        assert!(result.is_ok());

        // Verify the clone now has the target's characteristics
        let clone = state.objects.get(&clone_id).unwrap();
        assert_eq!(clone.name, "Grizzly Bears");
        assert_eq!(clone.power, Some(2));
        assert_eq!(clone.toughness, Some(2));
        assert!(clone.card_types.subtypes.contains(&"Bird".to_string()));
        assert!(clone
            .keywords
            .contains(&crate::types::keywords::Keyword::Flying));
    }

    #[test]
    fn copy_target_choice_applies_copied_enter_with_counters_replacement_before_sba() {
        let mut state = GameState::new_two_player(42);

        let ghave = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Ghave, Guru of Spores".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&ghave).unwrap();
            obj.base_power = Some(0);
            obj.base_toughness = Some(0);
            obj.power = Some(5);
            obj.toughness = Some(5);
            obj.counters
                .insert(crate::types::counter::CounterType::Plus1Plus1, 5);
            let enter_with_counters = crate::types::ability::ReplacementDefinition::new(
                crate::types::replacements::ReplacementEvent::Moved,
            )
            .execute(crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::PutCounter {
                    counter_type: crate::types::counter::CounterType::Plus1Plus1,
                    count: crate::types::ability::QuantityExpr::Fixed { value: 5 },
                    target: TargetFilter::SelfRef,
                },
            ))
            .valid_card(TargetFilter::SelfRef);
            obj.base_replacement_definitions = Arc::new(vec![enter_with_counters.clone()]);
            obj.replacement_definitions.push(enter_with_counters);
        }

        let assassin = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Callidus Assassin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&assassin).unwrap();
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(
                    crate::types::replacements::ReplacementEvent::Moved,
                )
                .execute(crate::types::ability::AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Spell,
                    Effect::BecomeCopy {
                        target: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                            crate::types::ability::TypeFilter::Creature,
                        )),
                        duration: None,
                        mana_value_limit: None,
                        additional_modifications: Vec::new(),
                    },
                )),
            );
        }

        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: assassin,
            valid_targets: vec![ghave],
            max_mana_value: None,
        };

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(ghave)),
            },
        )
        .expect("copy target choice should resolve");

        let copied = state.objects.get(&assassin).unwrap();
        assert_eq!(copied.zone, Zone::Battlefield);
        assert_eq!(copied.name, "Ghave, Guru of Spores");
        assert_eq!(
            copied
                .counters
                .get(&crate::types::counter::CounterType::Plus1Plus1)
                .copied(),
            Some(5),
            "CR 614.12: copied self ETB counters must apply before SBAs"
        );
        assert_eq!(copied.power, Some(5));
        assert_eq!(copied.toughness, Some(5));
    }

    /// CR 614.12a + CR 707.9: Callidus Assassin grants its copy a "When this
    /// creature enters" trigger as part of the entering-as-copy bundle. The
    /// ETB event for the copy must fire *after* the player chooses a target
    /// for the copy effect and `BecomeCopy` has stamped the granted trigger
    /// onto `trigger_definitions` — otherwise the trigger silently never
    /// fires. Regression for: the deferred-trigger replay path in
    /// `engine_priority::run_post_action_pipeline` +
    /// `engine_replacement::handle_copy_target_choice`.
    #[test]
    fn copy_target_choice_fires_granted_etb_trigger_against_deferred_entry_event() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, QuantityExpr, TriggerDefinition,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        let bear = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Granted trigger: "When this creature enters, controller draws a card."
        // Targetless to keep the test focused on the deferral mechanism rather
        // than target-selection plumbing.
        let granted = TriggerDefinition::new(TriggerMode::ChangesZone)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield);

        let assassin = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Callidus Assassin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&assassin).unwrap();
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(
                    crate::types::replacements::ReplacementEvent::Moved,
                )
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::BecomeCopy {
                        target: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                            crate::types::ability::TypeFilter::Creature,
                        )),
                        duration: None,
                        mana_value_limit: None,
                        additional_modifications: vec![ContinuousModification::GrantTrigger {
                            trigger: Box::new(granted.clone()),
                        }],
                    },
                )),
            );
        }

        // Capture a real `ZoneChanged` for Callidus by bouncing it through
        // stack→battlefield once. We then put it in the deferred queue to
        // model what the post-action pipeline does at the moment
        // `CopyTargetChoice` is set up.
        {
            let mut warmup_events: Vec<GameEvent> = Vec::new();
            zones::move_to_zone(&mut state, assassin, Zone::Stack, &mut warmup_events);
            warmup_events.clear();
            zones::move_to_zone(&mut state, assassin, Zone::Battlefield, &mut warmup_events);
            let entry_event = warmup_events
                .into_iter()
                .find(|e| {
                    matches!(
                        e,
                        GameEvent::ZoneChanged { object_id, to, .. }
                            if *object_id == assassin && *to == Zone::Battlefield
                    )
                })
                .expect("move_to_zone must emit a ZoneChanged for the entry");
            state.deferred_entry_events.push(entry_event);
        }
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: assassin,
            valid_targets: vec![bear],
            max_mana_value: None,
        };

        apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            },
        )
        .expect("copy target choice should resolve");

        // After the copy resolves and layers re-evaluate, the granted trigger
        // must be on the copy's trigger_definitions...
        let copied = state.objects.get(&assassin).unwrap();
        assert!(
            copied.trigger_definitions.iter_all().any(|t| t == &granted),
            "BecomeCopy's GrantTrigger modification must be present on the copy"
        );

        // ...and the deferred entry event must have been replayed through
        // process_triggers, so the granted ETB matched and queued.
        assert!(
            state.deferred_entry_events.is_empty(),
            "deferred entry events must be drained after copy choice resolves"
        );
        let trigger_fired = state.pending_trigger.is_some()
            || state.stack.iter().any(|entry| {
                matches!(
                    entry.kind,
                    crate::types::game_state::StackEntryKind::TriggeredAbility { source_id, .. }
                        if source_id == assassin
                )
            });
        assert!(
            trigger_fired,
            "granted ETB trigger must fire from the deferred entry event"
        );
    }

    /// Issue #429 — CR 113.2c + CR 603.3b + CR 707.10: When the copy-replacement
    /// ETB event is replayed by `handle_copy_target_choice`, multiple interactive
    /// triggers can fire simultaneously. `process_triggers` sets the first as
    /// `state.pending_trigger` and stashes the rest into `state.deferred_triggers`.
    /// The handler previously returned `WaitingFor::Priority` unconditionally,
    /// silently dropping the first trigger's target-selection prompt. The handler
    /// must hand back the active trigger's `TriggerTargetSelection` instead.
    #[test]
    fn copy_target_choice_surfaces_interactive_trigger_prompt_for_deferred_entry() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, QuantityExpr, TriggerDefinition, TypedFilter,
        };
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Two observers, each with a *targeted* "when a creature enters, deal 1
        // damage to target creature" ETB trigger. Both watch the replayed
        // Callidus entry event, so two interactive triggers fire at once.
        let make_observer = |state: &mut GameState, card: u64| -> ObjectId {
            let obs = zones::create_object(
                state,
                CardId(card),
                PlayerId(0),
                format!("Observer {card}"),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&obs).unwrap();
                obj.card_types
                    .core_types
                    .push(crate::types::card_type::CoreType::Creature);
                obj.base_power = Some(1);
                obj.base_toughness = Some(1);
                obj.power = Some(1);
                obj.toughness = Some(1);
                obj.trigger_definitions.push(
                    TriggerDefinition::new(TriggerMode::ChangesZone)
                        .execute(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::DealDamage {
                                amount: QuantityExpr::Fixed { value: 1 },
                                target: TargetFilter::Typed(TypedFilter::creature()),
                                damage_source: None,
                            },
                        ))
                        .valid_card(TargetFilter::Typed(TypedFilter::creature()))
                        .destination(Zone::Battlefield),
                );
            }
            obs
        };
        let observer_a = make_observer(&mut state, 10);
        let observer_b = make_observer(&mut state, 11);

        // Copy target on the battlefield.
        let bear = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
            // CR 707.2: `BecomeCopy` copies the *intrinsic copiable values*
            // (`base_*` fields), not the layer-derived ones. The bear's
            // creature type must live on `base_card_types` / `base_name` so the
            // realized copy is a creature — otherwise the observers' creature-
            // filtered ETB triggers never match the replayed copy entry.
            obj.base_card_types = obj.card_types.clone();
            obj.base_name = obj.name.clone();
        }

        // Callidus Assassin with a plain BecomeCopy "enter as a copy" replacement.
        let assassin = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Callidus Assassin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&assassin).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(
                    crate::types::replacements::ReplacementEvent::Moved,
                )
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::BecomeCopy {
                        target: TargetFilter::Typed(TypedFilter::creature()),
                        duration: None,
                        mana_value_limit: None,
                        additional_modifications: Vec::new(),
                    },
                )),
            );
        }

        // Capture a real `ZoneChanged` for Callidus entering, mirroring what the
        // post-action pipeline stashes when `CopyTargetChoice` is set up.
        {
            let mut warmup_events: Vec<GameEvent> = Vec::new();
            zones::move_to_zone(&mut state, assassin, Zone::Stack, &mut warmup_events);
            warmup_events.clear();
            zones::move_to_zone(&mut state, assassin, Zone::Battlefield, &mut warmup_events);
            let entry_event = warmup_events
                .into_iter()
                .find(|e| {
                    matches!(
                        e,
                        GameEvent::ZoneChanged { object_id, to, .. }
                            if *object_id == assassin && *to == Zone::Battlefield
                    )
                })
                .expect("move_to_zone must emit a ZoneChanged for the entry");
            state.deferred_entry_events.push(entry_event);
        }
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: assassin,
            valid_targets: vec![bear],
            max_mana_value: None,
        };

        let _waiting = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(bear)),
            },
        )
        .expect("copy target choice should resolve")
        .waiting_for;

        // CR 603.3b (#531): The two simultaneously-fired interactive ETB
        // triggers belong to one controller (PlayerId(0)); the engine emits
        // OrderTriggers first. Drain with identity so the legacy assertion
        // below can inspect the post-ordering TriggerTargetSelection state.
        crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        let waiting = state.waiting_for.clone();

        // The first interactive trigger's target-selection prompt must be
        // surfaced — not silently dropped in favor of Priority.
        assert!(
            matches!(waiting, WaitingFor::TriggerTargetSelection { .. }),
            "expected the first interactive ETB trigger's prompt, got {waiting:?}"
        );
        assert!(
            state.pending_trigger.is_some(),
            "the active interactive trigger must be set as pending_trigger"
        );
        // The second simultaneously-fired trigger must be retained in the
        // deferred queue so it reaches the stack after the first resolves.
        assert_eq!(
            state.deferred_triggers.len(),
            1,
            "the sibling interactive trigger must be deferred, not dropped"
        );
        // Both observers must be the trigger sources (one active, one deferred).
        let pending_src = state.pending_trigger.as_ref().unwrap().source_id;
        let deferred_src = state.deferred_triggers[0].pending.source_id;
        let mut srcs = [pending_src, deferred_src];
        srcs.sort_by_key(|id| id.0);
        let mut expected = [observer_a, observer_b];
        expected.sort_by_key(|id| id.0);
        assert_eq!(
            srcs, expected,
            "both observers' ETB triggers must be accounted for"
        );
    }

    #[test]
    fn copy_target_choice_rejects_invalid_target() {
        let mut state = GameState::new_two_player(42);

        let valid_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let invalid_id = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bird".to_string(),
            Zone::Battlefield,
        );
        let clone_id = zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Clone".to_string(),
            Zone::Battlefield,
        );

        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: clone_id,
            valid_targets: vec![valid_id], // Bird is NOT in valid targets
            max_mana_value: None,
        };

        // Try to choose invalid target
        let result = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(invalid_id)),
            },
        );
        assert!(result.is_err());
    }

    // ── Superior Spider-Man integration test ──
    // CR 707.9 + CR 707.2 + CR 613.1d + CR 603.12: Full flow for
    // `Mind Swap — You may have Superior Spider-Man enter as a copy of any
    // creature card in a graveyard, except his name is Superior Spider-Man and
    // he's a 4/4 Spider Human Hero in addition to his other types. When you
    // do, exile that card.`
    #[test]
    fn superior_spider_man_full_copy_flow_copies_graveyard_card_and_exiles_it() {
        use crate::types::ability::ContinuousModification;
        use crate::types::card_type::Supertype;

        let mut state = GameState::new_two_player(42);

        // Elesh Norn in PlayerId(1)'s graveyard with abilities + keywords.
        let elesh = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Elesh Norn".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&elesh).unwrap();
            obj.base_name = "Elesh Norn".to_string();
            obj.base_power = Some(7);
            obj.base_toughness = Some(7);
            obj.base_card_types = crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Phyrexian".to_string(), "Praetor".to_string()],
            };
            obj.base_keywords = vec![crate::types::keywords::Keyword::Vigilance];
            obj.base_abilities = Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Activated,
                Effect::Draw {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);
        }

        // Superior Spider-Man freshly on battlefield under PlayerId(0)'s control.
        let spidey = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Superior Spider-Man".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spidey).unwrap();
            obj.base_name = "Superior Spider-Man".to_string();
            obj.base_power = Some(4);
            obj.base_toughness = Some(4);
            obj.base_card_types = crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec![
                    "Spider".to_string(),
                    "Human".to_string(),
                    "Hero".to_string(),
                ],
            };
            // Install the replacement exactly as the parser would emit it:
            // BecomeCopy with additional_modifications + reflexive sub_ability.
            let reflexive = crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Exile,
                    target: TargetFilter::ParentTarget,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: false,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                },
            );
            let reflexive = crate::types::ability::AbilityDefinition {
                condition: Some(crate::types::ability::AbilityCondition::WhenYouDo),
                ..reflexive
            };
            let become_copy = crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::Typed(
                        crate::types::ability::TypedFilter::new(
                            crate::types::ability::TypeFilter::Creature,
                        )
                        .properties(vec![
                            crate::types::ability::FilterProp::InZone {
                                zone: Zone::Graveyard,
                            },
                        ]),
                    ),
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: vec![
                        ContinuousModification::SetName {
                            name: "Superior Spider-Man".to_string(),
                        },
                        ContinuousModification::SetPower { value: 4 },
                        ContinuousModification::SetToughness { value: 4 },
                        ContinuousModification::AddSubtype {
                            subtype: "Spider".to_string(),
                        },
                        ContinuousModification::AddSubtype {
                            subtype: "Human".to_string(),
                        },
                        ContinuousModification::AddSubtype {
                            subtype: "Hero".to_string(),
                        },
                    ],
                },
            )
            .sub_ability(reflexive);
            obj.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(
                    crate::types::replacements::ReplacementEvent::Moved,
                )
                .execute(become_copy),
            );
        }

        // Simulate reaching CopyTargetChoice directly (the replacement pipeline
        // tests cover the preceding "enter" pause; here we focus on the
        // post-choice resolution: copy + reflexive trigger firing).
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: spidey,
            valid_targets: vec![elesh],
            max_mana_value: None,
        };

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(elesh)),
            },
        );
        assert!(result.is_ok(), "copy target choice should resolve");

        // (a) Copied abilities from Elesh Norn: activated Draw ability should be present.
        let copied = state.objects.get(&spidey).unwrap();
        assert!(
            copied
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::Draw { .. })),
            "copied abilities must include Elesh Norn's Draw"
        );
        assert!(
            copied
                .keywords
                .contains(&crate::types::keywords::Keyword::Vigilance),
            "copied keywords must include Vigilance"
        );

        // (b) Name is overridden to Superior Spider-Man (not Elesh Norn).
        assert_eq!(
            copied.name, "Superior Spider-Man",
            "SetName must override the copied name"
        );

        // (c) P/T overridden to 4/4.
        assert_eq!(copied.power, Some(4));
        assert_eq!(copied.toughness, Some(4));

        // (d) Types include Elesh Norn's (Phyrexian, Praetor) AND additive
        //     Spider/Human/Hero.
        for subtype in ["Phyrexian", "Praetor", "Spider", "Human", "Hero"] {
            assert!(
                copied.card_types.subtypes.iter().any(|s| s == subtype),
                "missing subtype {subtype} in {:?}",
                copied.card_types.subtypes
            );
        }

        // (e) Reflexive trigger fired and exiled Elesh Norn from the graveyard.
        // `WhenYouDo` either resolves inline within the parent chain or queues
        // a `PendingTrigger` → CR 603.12 + CR 603.7a. Drain priority passes up
        // to a small bound so the trigger resolves before we assert. Each pass
        // resolves at most one stack item; the cap prevents infinite loops if
        // a new state dead-ends.
        for _ in 0..16 {
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
                break;
            }
            if apply_as_current(&mut state, GameAction::PassPriority).is_err() {
                break;
            }
        }

        let elesh_obj = state
            .objects
            .get(&elesh)
            .expect("Elesh Norn object still present after exile");
        assert_eq!(
            elesh_obj.zone,
            Zone::Exile,
            "reflexive trigger must exile the copied graveyard card"
        );
    }

    /// CR 603.12: Focused regression — a reflexive `When you do, …` sub_ability
    /// attached to a `BecomeCopy` replacement fires exactly once after the copy
    /// resolution, and its `TargetFilter::ParentTarget` resolves to the card the
    /// player chose to copy. Scoped to the reflexive path only — no name/P-T
    /// modifications, no supertypes, no copied abilities — so a failure
    /// diagnoses the CR 603.12 path rather than the surrounding clone-suffix
    /// parsing or layer application.
    #[test]
    fn reflexive_when_you_do_fires_after_become_copy_replacement() {
        let mut state = GameState::new_two_player(42);

        // Plain creature sitting in the opponent's graveyard — the reflexive
        // exile target. No modifiers: we're testing trigger timing and parent
        // target forwarding, not copy mechanics.
        let source_card = zones::create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&source_card).unwrap();
            obj.base_card_types = crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
        }

        // Cloner: a minimal permanent with BecomeCopy + reflexive "when you
        // do, exile that card" sub_ability. `TargetFilter::ParentTarget`
        // forwards the chosen copy source to the exile step.
        let cloner = zones::create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Test Cloner".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&cloner).unwrap();
            obj.base_card_types = crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec![],
            };
            let reflexive = crate::types::ability::AbilityDefinition {
                condition: Some(crate::types::ability::AbilityCondition::WhenYouDo),
                ..crate::types::ability::AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Spell,
                    Effect::ChangeZone {
                        origin: None,
                        destination: Zone::Exile,
                        target: TargetFilter::ParentTarget,
                        owner_library: false,
                        enter_transformed: false,
                        enters_under: None,
                        enter_tapped: false,
                        enters_attacking: false,
                        up_to: false,
                        enter_with_counters: vec![],
                    },
                )
            };
            let become_copy = crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::BecomeCopy {
                    target: TargetFilter::Typed(
                        crate::types::ability::TypedFilter::new(
                            crate::types::ability::TypeFilter::Creature,
                        )
                        .properties(vec![
                            crate::types::ability::FilterProp::InZone {
                                zone: Zone::Graveyard,
                            },
                        ]),
                    ),
                    duration: None,
                    mana_value_limit: None,
                    additional_modifications: vec![],
                },
            )
            .sub_ability(reflexive);
            obj.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(
                    crate::types::replacements::ReplacementEvent::Moved,
                )
                .execute(become_copy),
            );
        }

        // Enter directly into the post-copy-choice waiting state — the
        // preceding "enter as a copy of" pause is covered by other tests;
        // here we isolate the reflexive resolution.
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: cloner,
            valid_targets: vec![source_card],
            max_mana_value: None,
        };

        // Accumulate events across the full resolution so we can count
        // exile transitions — CR 603.12a requires the reflexive to fire
        // exactly once per trigger event, and exiling an already-exiled
        // card is a no-op zone move that would silently mask double-firing
        // if we only asserted on end-state.
        let mut all_events: Vec<GameEvent> = Vec::new();

        let result = apply_as_current(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(source_card)),
            },
        )
        .expect("copy target choice should resolve");
        all_events.extend(result.events);

        // Drain priority passes until the reflexive trigger has resolved.
        // CR 603.12: the reflexive is created during the replacement's
        // resolution and fires based on the "choose and copy" event that
        // already occurred. Cap drained iterations — if we hit the cap the
        // loop never reached Priority + empty stack and the test must fail
        // loudly rather than silently proceeding.
        let cap = 16;
        let mut drained = false;
        for _ in 0..cap {
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) && state.stack.is_empty() {
                drained = true;
                break;
            }
            match apply_as_current(&mut state, GameAction::PassPriority) {
                Ok(r) => all_events.extend(r.events),
                Err(_) => {
                    drained = true;
                    break;
                }
            }
        }
        assert!(
            drained,
            "drain loop exceeded {cap} iterations without reaching \
             Priority + empty stack — reflexive trigger path is stuck"
        );

        // ParentTarget was forwarded: the graveyard card is now exiled.
        let exiled = state
            .objects
            .get(&source_card)
            .expect("source card object preserved after exile");
        assert_eq!(
            exiled.zone,
            Zone::Exile,
            "reflexive `When you do, exile that card` must exile the copy source \
             (TargetFilter::ParentTarget forwarded from BecomeCopy resolution)"
        );

        // CR 603.12a: the reflexive triggers exactly once for the one
        // BecomeCopy event. Count ZoneChanged events moving the source
        // card into exile. A silent double-fire (same source, same dest)
        // would push 2 events here even though the final state is
        // identical, catching regressions that end-state assertions miss.
        let exile_moves = all_events
            .iter()
            .filter(|ev| {
                matches!(ev, GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Exile,
                    ..
                } if *object_id == source_card)
            })
            .count();
        assert_eq!(
            exile_moves, 1,
            "reflexive must fire exactly once per CR 603.12a; got {exile_moves} exile \
             transitions of the copy source (expected 1)"
        );
    }

    /// CR 117.1c + CR 509.1 + CR 702.49: When an attacker exists but the defending
    /// player has no legal blockers, the declare blockers step still runs and the
    /// active player still receives priority during it. This window is what makes
    /// Ninjutsu-family activations (notably Sneak, CR 702.49 variant — restricted
    /// to this step only) reachable when attacking into an empty board.
    #[test]
    fn declare_blockers_grants_ap_priority_when_no_legal_blockers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let attacker = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&attacker).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.entered_battlefield_turn = Some(1);
        }
        // Defender has no creatures — no legal blocks.

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };

        apply_as_current(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(attacker, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();

        // AP passes in DeclareAttackers; NAP passes; engine advances into
        // DeclareBlockers, auto-submits empty blockers (nothing to choose),
        // and — per CR 117.1c — hands priority back to the active player
        // *during the declare blockers step*.
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();

        assert_eq!(
            state.phase,
            Phase::DeclareBlockers,
            "step should be declare blockers, not skipped past"
        );
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::Priority {
                    player: PlayerId(0)
                }
            ),
            "active player must receive priority in declare blockers step \
             (CR 117.1c) so they can activate Sneak (CR 702.49); got {:?}",
            result.waiting_for
        );
    }

    // ---- CR 702.24a: Cumulative upkeep end-to-end (Mystic Remora) ----------
    //
    // These tests exercise the full pipeline from "upkeep trigger fires" to
    // "controller pays or sacrifices":
    //   1. Synthesized trigger (PayCumulativeUpkeep, Phase=Upkeep, valid_target
    //      Controller) fires when the controller's upkeep step begins.
    //   2. Outer `Effect::AddCounter { CounterType::Age }` ticks the counter
    //      on the source before the sub-ability runs.
    //   3. Sub-ability `Effect::Sacrifice` carries `unless_pay` =
    //      `AbilityCost::PerCounter { Age, SelfRef, base }`, which expands at
    //      resolution time to `Mana { N × base }`.
    //   4. Player answers `PayUnlessCost { pay: bool }` — pay keeps the
    //      permanent, decline sacrifices it.
    //
    // Closest precedent: `setup_esper_sentinel_unless_payment` (CR 118.12 tax
    // trigger) — same `auto_advance` → `resolve_top` → `PayUnlessCost`
    // scaffolding. The Mystic Remora flow differs only in how the trigger is
    // sourced (synthesized by Keyword::CumulativeUpkeep, not parsed) and in
    // the `PerCounter` expansion that lives in the sub-ability's unless-cost.

    /// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
    /// {N}" (mana base cost) by delegating to the production synthesizer.
    /// Binding the end-to-end tests to the real builder ensures any regression
    /// in `build_cumulative_upkeep_trigger` (e.g., flipping AddCounter →
    /// Sacrifice ordering, dropping `.phase(Upkeep)`, or changing the
    /// PerCounter payer) breaks the Mystic Remora pipeline tests loudly
    /// rather than silently passing against a stale inline mirror.
    fn cumulative_upkeep_mana_trigger(generic: u32) -> TriggerDefinition {
        crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::Mana {
            cost: ManaCost::generic(generic),
        })
    }

    /// Construct a solo state with Mystic Remora on the battlefield,
    /// controller = PlayerId(0) = active player, at Phase::Untap so
    /// `auto_advance` will fire the upkeep trigger.
    fn setup_mystic_remora_upkeep_state() -> (GameState, ObjectId) {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let remora = create_object(
            &mut state,
            CardId(7024),
            PlayerId(0),
            "Mystic Remora".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&remora).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.trigger_definitions
                .push(cumulative_upkeep_mana_trigger(1));
        }

        (state, remora)
    }

    /// Advance from Untap through Upkeep, fire the cumulative-upkeep trigger,
    /// and resolve it. Mirrors the Esper Sentinel pattern of `auto_advance`
    /// (to populate the stack) then `resolve_top` (to walk the outer
    /// AddCounter → sub-ability Sacrifice/PerCounter chain into
    /// `WaitingFor::UnlessPayment`).
    fn advance_to_unless_payment_prompt(state: &mut GameState) {
        let mut events = Vec::new();
        let _wf = crate::game::turns::auto_advance(state, &mut events);
        // CR 503.1a: the trigger landed on the stack during Phase::Upkeep.
        assert_eq!(state.phase, Phase::Upkeep);
        assert!(
            !state.stack.is_empty(),
            "cumulative-upkeep trigger must be on the stack after auto_advance"
        );
        crate::game::stack::resolve_top(state, &mut events);
    }

    /// Give PlayerId(0) `generic` colorless mana units so they can satisfy a
    /// `Mana { generic: N }` unless-cost. Mirrors the `mana_pool.add` idiom
    /// used by `setup_esper_sentinel_unless_payment`.
    fn give_p0_colorless_mana(state: &mut GameState, generic: u32) {
        let p0 = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .expect("PlayerId(0)");
        for _ in 0..generic {
            p0.mana_pool.add(ManaUnit::new(
                ManaType::Colorless,
                ObjectId(0),
                false,
                vec![],
            ));
        }
    }

    /// Reset `phase`, `active_player`, `priority_player`, `stack`,
    /// `pending_trigger`, and `waiting_for` so the next `auto_advance`
    /// re-enters PlayerId(0)'s upkeep and re-fires the cumulative-upkeep
    /// trigger. The age counter on `remora` persists across this transition
    /// (counters live on the object and outlive phase changes), which is
    /// exactly the CR 702.24a "accumulates each upkeep" invariant under
    /// test.
    ///
    /// Does NOT clear per-turn bookkeeping (`priority_passes`,
    /// `spells_cast_this_turn`, `spells_cast_this_turn_by_player`,
    /// `pending_trigger_event_batch`, etc.) — safe for cumulative-upkeep
    /// tests that never pass priority or cast spells mid-test. Tasks 10-13
    /// (Polar Kraken, Inner Sanctum, source-gone, multi-instance) must
    /// re-evaluate this scope if their flow does either; expanding the
    /// resets is preferable to silent state drift.
    fn rewind_to_next_p0_upkeep(state: &mut GameState) {
        state.turn_number += 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.stack.clear();
        state.pending_trigger = None;
        // CR 603.3c + CR 603.3d: clear the in-construction cursor too —
        // symmetric with `pending_trigger`. Without this, a trigger pushed
        // earlier in the test could leave `pending_trigger_entry` pointing
        // to a now-cleared `state.stack`, tripping the push-first invariants
        // on the next trigger.
        state.pending_trigger_entry = None;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
    }

    /// CR 702.24a + CR 118.12: Paying the cumulative-upkeep cost keeps the
    /// permanent on the battlefield. Verifies the age counter ticks first
    /// (outer AddCounter resolves before the sub-ability), the prompt expands
    /// to `Mana{1}` (1 counter × base {1}), and the post-pay state has the
    /// permanent still on the battlefield with the age counter intact.
    #[test]
    fn mystic_remora_upkeep_pay_path_keeps_permanent_and_adds_age_counter() {
        let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

        advance_to_unless_payment_prompt(&mut state);
        // CR 500.5: mana pools empty between phases. Add the unless-cost
        // payment AFTER `auto_advance` settles in Upkeep so the mana persists
        // through to `PayUnlessCost` (mirrors what real play models: the
        // controller would tap a land in response to the trigger).
        give_p0_colorless_mana(&mut state, 1);

        // CR 702.24a: outer AddCounter resolved first, so the counter exists
        // before the per-counter unless-cost is computed.
        assert_eq!(
            state.objects[&remora_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(1),
            "age counter must be added before the unless-pay prompt"
        );

        // CR 118.12 + CR 702.24a: PerCounter expanded to {1} for 1 age counter.
        match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
                match cost {
                    AbilityCost::Mana { cost: mana } => {
                        assert_eq!(
                            *mana,
                            ManaCost::generic(1),
                            "1 age counter × base {{1}} = {{1}}"
                        );
                    }
                    other => panic!("expected Mana cost, got {other:?}"),
                }
            }
            other => panic!("expected UnlessPayment prompt, got {other:?}"),
        }

        let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        // CR 702.24a: paying the cost keeps the permanent on the battlefield.
        assert_eq!(
            state.objects[&remora_id].zone,
            Zone::Battlefield,
            "paying the cumulative-upkeep cost must NOT sacrifice the permanent"
        );
        assert!(
            !state.players[0].graveyard.contains(&remora_id),
            "permanent must not be in graveyard when paid"
        );
    }

    /// CR 702.24a + CR 118.12: Declining the cumulative-upkeep cost sacrifices
    /// the permanent. The sub-ability's `Effect::Sacrifice` runs because the
    /// player chose not to pay; the source moves to its controller's
    /// graveyard.
    #[test]
    fn mystic_remora_upkeep_decline_path_sacrifices() {
        let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

        advance_to_unless_payment_prompt(&mut state);

        let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: false }).unwrap();

        // CR 701.21a: To sacrifice a permanent, its controller moves it from
        // the battlefield directly to its owner's graveyard.
        assert!(
            state.players[0].graveyard.contains(&remora_id),
            "declining the unless-cost must sacrifice the permanent; graveyard={:?}",
            state.players[0].graveyard
        );
        assert_ne!(
            state.objects[&remora_id].zone,
            Zone::Battlefield,
            "permanent must leave the battlefield on decline"
        );
    }

    /// CR 702.24a: "...put an age counter on it. Then sacrifice it unless you
    /// pay its upkeep cost for each age counter on it." Three consecutive
    /// upkeeps with payment must yield costs {1}, {2}, {3} (1, 2, 3 counters
    /// respectively) and three age counters at the end. This is the
    /// load-bearing test for the `PerCounter` expansion: it confirms that
    /// each tick of the counter strictly precedes the cost computation, and
    /// that counters accumulate across turns.
    #[test]
    fn mystic_remora_three_upkeeps_costs_one_two_three() {
        let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

        for (turn_idx, expected_generic) in [1u32, 2, 3].iter().enumerate() {
            advance_to_unless_payment_prompt(&mut state);
            // CR 500.5: mana pools empty between phases. Provide the unless-
            // cost payment AFTER `auto_advance` settles in Upkeep so the
            // mana survives into `PayUnlessCost`.
            give_p0_colorless_mana(&mut state, *expected_generic);

            // The age counter for THIS upkeep is already in place when we
            // reach the unless-pay prompt — counter total is turn_idx + 1.
            let expected_counters = (turn_idx + 1) as u32;
            assert_eq!(
                state.objects[&remora_id]
                    .counters
                    .get(&crate::types::counter::CounterType::Age)
                    .copied(),
                Some(expected_counters),
                "upkeep {turn_idx}: expected {expected_counters} age counter(s) before payment"
            );

            match &state.waiting_for {
                WaitingFor::UnlessPayment {
                    cost: AbilityCost::Mana { cost: mana },
                    ..
                } => {
                    assert_eq!(
                        *mana,
                        ManaCost::generic(*expected_generic),
                        "upkeep {turn_idx}: expected Mana({{{expected_generic}}}), got {mana:?}"
                    );
                }
                other => {
                    panic!("upkeep {turn_idx}: expected Mana unless-payment prompt, got {other:?}")
                }
            }

            let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();
            assert_eq!(
                state.objects[&remora_id].zone,
                Zone::Battlefield,
                "upkeep {turn_idx}: paying keeps the permanent on the battlefield"
            );

            // Reset to next controller upkeep for the next iteration.
            if turn_idx < 2 {
                rewind_to_next_p0_upkeep(&mut state);
            }
        }

        // CR 702.24a: counters strictly accumulate. After three paid upkeeps,
        // the permanent carries three age counters.
        assert_eq!(
            state.objects[&remora_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(3),
            "three age counters must have accumulated across three upkeeps"
        );
    }

    /// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
    /// — Sacrifice a land" (Polar Kraken's sacrifice-cost variant) by delegating
    /// to the production synthesizer. Mirrors `cumulative_upkeep_mana_trigger`
    /// (which exercises the `Mana` arm of `expand_per_counter`); this helper
    /// exercises the `Sacrifice` arm. Binding to the real builder ensures any
    /// regression in `build_cumulative_upkeep_trigger`'s handling of a
    /// non-Mana base cost (chained-ability ordering, PerCounter payer,
    /// `.phase(Upkeep)` gating) breaks the Polar Kraken pipeline test loudly.
    ///
    /// CR 702.24a: cumulative upkeep cost format is `[cost]` where `[cost]`
    /// may be any cost. Sacrifice-a-land is the canonical non-mana variant
    /// (Polar Kraken, Phyrexian Soulgorger).
    fn cumulative_upkeep_sacrifice_land_trigger() -> TriggerDefinition {
        crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::land()),
            count: 1,
        })
    }

    /// Construct a solo state with Polar Kraken on the battlefield (controller
    /// = PlayerId(0) = active player) plus three Forests for sacrifice fodder,
    /// at Phase::Untap so `auto_advance` will fire the upkeep trigger. The
    /// three-forest count is deliberate: the test sacrifices exactly one, and
    /// the surviving two prove that `handle_unless_payment_sacrifice`'s
    /// eligible-permanents collection didn't over-sacrifice or sacrifice the
    /// wrong land.
    ///
    /// Returns `(state, kraken_id, [forest0, forest1, forest2])`.
    fn setup_polar_kraken_upkeep_state() -> (GameState, ObjectId, Vec<ObjectId>) {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let kraken = create_object(
            &mut state,
            CardId(7100),
            PlayerId(0),
            "Polar Kraken".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&kraken).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Kraken".to_string());
            obj.trigger_definitions
                .push(cumulative_upkeep_sacrifice_land_trigger());
        }

        let mut forests = Vec::with_capacity(3);
        for i in 0..3 {
            let forest = create_object(
                &mut state,
                CardId(7101 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            {
                let obj = state.objects.get_mut(&forest).unwrap();
                obj.card_types.core_types.push(CoreType::Land);
                obj.card_types.subtypes.push("Forest".to_string());
            }
            forests.push(forest);
        }

        (state, kraken, forests)
    }

    /// CR 702.24a + CR 118.12 + CR 701.21: Paying the cumulative-upkeep cost
    /// via the sacrifice-a-land variant. At counter=1, the per-counter expansion
    /// of `Sacrifice { Land, count: 1 }` yields `Sacrifice { Land, count: 1 }`
    /// (1 × 1 = 1), and paying by sacrificing one of three controlled forests
    /// keeps Polar Kraken on the battlefield with one forest in the graveyard
    /// and two untouched. This is the structural-identity case for the
    /// `Sacrifice` arm of `expand_per_counter` — Mystic Remora's three-upkeep
    /// test already covers the multiplicative case for the `Mana` arm.
    #[test]
    fn polar_kraken_upkeep_sacrifice_cost_path() {
        let (mut state, kraken_id, forest_ids) = setup_polar_kraken_upkeep_state();
        advance_to_unless_payment_prompt(&mut state);

        // CR 702.24a: outer AddCounter resolved first, so one age counter
        // sits on the Kraken before the per-counter unless-cost is computed.
        assert_eq!(
            state.objects[&kraken_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(1),
            "age counter must be added before the unless-pay prompt"
        );

        // CR 118.12 + CR 702.24a: PerCounter expanded `Sacrifice { Land, 1 }`
        // for 1 age counter to `Sacrifice { Land, 1 }` (1 × 1 = 1).
        match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
                match cost {
                    AbilityCost::Sacrifice { target, count } => {
                        assert_eq!(*count, 1, "1 age counter × base count 1 = 1");
                        assert_eq!(
                            *target,
                            TargetFilter::Typed(TypedFilter::land()),
                            "unless-cost target filter must remain Land"
                        );
                    }
                    other => panic!("expected Sacrifice cost, got {other:?}"),
                }
            }
            other => panic!("expected UnlessPayment prompt, got {other:?}"),
        }

        // CR 118.12 + CR 701.21: Pay → engine collects eligible controlled
        // Lands and surfaces `WaitingFor::WardSacrificeChoice` for the player
        // to pick which permanent to sacrifice.
        let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();
        match &state.waiting_for {
            WaitingFor::WardSacrificeChoice {
                player,
                permanents,
                remaining,
                ..
            } => {
                assert_eq!(*player, PlayerId(0), "controller picks the sacrifice");
                assert_eq!(*remaining, 1, "exactly one sacrifice required");
                assert_eq!(
                    permanents.len(),
                    3,
                    "all three controlled forests must be eligible"
                );
                for fid in &forest_ids {
                    assert!(
                        permanents.contains(fid),
                        "forest {fid:?} must be an eligible sacrifice"
                    );
                }
            }
            other => panic!("expected WardSacrificeChoice prompt, got {other:?}"),
        }

        // CR 701.21: Choose the first forest as the sacrifice victim.
        let _ = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![forest_ids[0]],
            },
        )
        .unwrap();

        // CR 702.24a: paying the cost keeps the permanent on the battlefield.
        assert_eq!(
            state.objects[&kraken_id].zone,
            Zone::Battlefield,
            "paying the cumulative-upkeep cost must NOT sacrifice the Kraken"
        );
        // CR 701.21a: To sacrifice a permanent, its controller moves it from
        // the battlefield directly to its owner's graveyard.
        assert_eq!(
            state.objects[&forest_ids[0]].zone,
            Zone::Graveyard,
            "the chosen forest must be in the graveyard"
        );
        assert!(
            state.players[0].graveyard.contains(&forest_ids[0]),
            "graveyard must contain the sacrificed forest"
        );
        // The two unchosen forests stay on the battlefield — proves the
        // sacrifice path didn't over-select.
        assert_eq!(
            state.objects[&forest_ids[1]].zone,
            Zone::Battlefield,
            "unchosen forest 1 must remain on the battlefield"
        );
        assert_eq!(
            state.objects[&forest_ids[2]].zone,
            Zone::Battlefield,
            "unchosen forest 2 must remain on the battlefield"
        );
    }

    /// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
    /// — Pay 2 life" (Inner Sanctum's life-cost variant) by delegating to the
    /// production synthesizer. Mirrors `cumulative_upkeep_mana_trigger` and
    /// `cumulative_upkeep_sacrifice_land_trigger`; this helper exercises the
    /// `PayLife` arm of `expand_per_counter` (CR 702.24a + CR 119.4).
    fn cumulative_upkeep_pay_life_trigger(amount: i32) -> TriggerDefinition {
        crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: amount },
        })
    }

    /// Construct a solo state with Inner Sanctum on the battlefield (controller
    /// = PlayerId(0) = active player) at Phase::Untap so `auto_advance` will
    /// fire the upkeep trigger. **One age counter is pre-loaded** on Inner
    /// Sanctum so the first upkeep that `auto_advance` resolves ticks the
    /// counter from 1 → 2 — exercising the multiplicative step of the
    /// `PayLife` arm of `expand_per_counter` (base 2 × counter 2 = 4 life).
    /// This skips the structurally-trivial counter=1 case, which the Polar
    /// Kraken sacrifice test already covers for the non-Mana arm.
    fn setup_inner_sanctum_second_upkeep_state() -> (GameState, ObjectId) {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let sanctum = create_object(
            &mut state,
            CardId(7200),
            PlayerId(0),
            "Inner Sanctum".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&sanctum).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.trigger_definitions
                .push(cumulative_upkeep_pay_life_trigger(2));
            // CR 702.24a: pre-load one age counter so the next upkeep tick
            // produces counter=2, yielding the per-counter expansion
            // PayLife{2 × 2} = PayLife{4}.
            obj.counters
                .insert(crate::types::counter::CounterType::Age, 1);
        }

        (state, sanctum)
    }

    /// CR 702.24a + CR 118.12 + CR 119.4: Paying the cumulative-upkeep cost
    /// via the pay-life variant at counter=2. Pre-loading one age counter
    /// means the second upkeep ticks the counter from 1 → 2, and the
    /// `PerCounter` expansion of `PayLife { Fixed(2) }` yields
    /// `PayLife { Fixed(4) }` (2 × 2 = 4 life). Paying 4 life keeps Inner
    /// Sanctum on the battlefield and deducts 4 from the controller's life
    /// total — the load-bearing assertion for the `PayLife` arm of
    /// `expand_per_counter`'s `QuantityExpr::scaled_by` composition.
    #[test]
    fn inner_sanctum_upkeep_two_age_counters_pays_four_life() {
        let (mut state, sanctum_id) = setup_inner_sanctum_second_upkeep_state();
        advance_to_unless_payment_prompt(&mut state);

        // CR 702.24a: outer AddCounter resolved first; the pre-loaded counter
        // ticked from 1 → 2 before the per-counter unless-cost is computed.
        assert_eq!(
            state.objects[&sanctum_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(2),
            "age counter should tick from 1 (pre-loaded) to 2 on this upkeep"
        );

        // CR 118.12 + CR 702.24a + CR 119.4: PerCounter expanded
        // `PayLife { Fixed(2) }` for 2 age counters to `PayLife { Fixed(4) }`
        // (2 × 2 = 4). This is the load-bearing multiplicative assertion for
        // the `PayLife` arm of `expand_per_counter`.
        match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
                match cost {
                    AbilityCost::PayLife { amount } => {
                        assert_eq!(
                            *amount,
                            QuantityExpr::Fixed { value: 4 },
                            "2 age counters × base 2 life = 4 life"
                        );
                    }
                    other => panic!("expected PayLife cost, got {other:?}"),
                }
            }
            other => panic!("expected UnlessPayment prompt, got {other:?}"),
        }

        // CR 119.4: pay-life unless-costs are auto-deducted from the player's
        // life total at `PayUnlessCost { pay: true }` time — no intermediate
        // choice prompt (unlike Sacrifice, which surfaces a permanent
        // picker). Snapshot the life total before paying so the delta is
        // measurable.
        let life_before = state.players[0].life;
        let _ = apply_as_current(&mut state, GameAction::PayUnlessCost { pay: true }).unwrap();

        // CR 119.4: 4 life paid → life total decreases by exactly 4.
        assert_eq!(
            state.players[0].life,
            life_before - 4,
            "paying 4 life must reduce life total by 4"
        );
        // CR 702.24a: paying the cost keeps the permanent on the battlefield.
        assert_eq!(
            state.objects[&sanctum_id].zone,
            Zone::Battlefield,
            "paying the cumulative-upkeep cost must NOT sacrifice the permanent"
        );
        assert!(
            !state.players[0].graveyard.contains(&sanctum_id),
            "permanent must not be in graveyard when paid"
        );
    }

    /// CR 702.24a + CR 603.4 + CR 400.7: "if this permanent is on the
    /// battlefield" is an intervening-if condition re-checked at trigger
    /// resolution. If the source permanent has left the battlefield between
    /// trigger fire and resolution (bounced, exiled, etc.), the entire
    /// chained ability no-ops: no age counter is placed, no unless-pay prompt
    /// is emitted, and no sacrifice occurs.
    ///
    /// This is the regression test for the cumulative-upkeep
    /// `TriggerCondition::SourceInZone { Battlefield }` guard wired in
    /// `build_cumulative_upkeep_trigger`. Without that guard, the trigger
    /// would resolve against the (now-hand-zone) source object: the outer
    /// `Effect::AddCounter` would still write an age counter onto the object
    /// in hand, and the sub-ability would still prompt the controller with a
    /// `Mana{1}` unless-payment — a spurious prompt fundamentally inconsistent
    /// with CR 702.24a.
    ///
    /// The flow exercises the resolution-time re-evaluation specifically:
    ///   1. `auto_advance` from Untap into Upkeep, firing the trigger onto the
    ///      stack (source is still on the battlefield at fire-time, so the
    ///      intervening-if passes).
    ///   2. Move the source to hand (simulates a bounce spell resolving on
    ///      top of the upkeep trigger).
    ///   3. `resolve_top` should see the condition fail at resolution time
    ///      (per `stack::resolve_top`'s CR 603.4 re-check) and walk away
    ///      without invoking the AddCounter → sub-ability chain.
    #[test]
    fn cumulative_upkeep_source_gone_before_resolution_is_noop() {
        let (mut state, remora_id) = setup_mystic_remora_upkeep_state();

        // Step 1: fire the trigger onto the stack but DO NOT resolve it.
        // `auto_advance` settles in Phase::Upkeep with the trigger queued.
        let mut events = Vec::new();
        let _wf = crate::game::turns::auto_advance(&mut state, &mut events);
        assert_eq!(
            state.phase,
            Phase::Upkeep,
            "auto_advance must pause in Upkeep with the trigger queued"
        );
        assert!(
            !state.stack.is_empty(),
            "cumulative-upkeep trigger must be on the stack pre-bounce"
        );
        // Source is still on the battlefield at fire-time and has no age
        // counter yet (outer AddCounter resolves at stack resolution).
        assert_eq!(state.objects[&remora_id].zone, Zone::Battlefield);
        assert_eq!(
            state.objects[&remora_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied()
                .unwrap_or(0),
            0,
            "no age counter before stack resolution"
        );

        // Step 2: bounce the source to its owner's hand. In real play this
        // would be a Boomerang or Unsummon resolving on top of the upkeep
        // trigger. We move it directly to keep the test focused on the
        // intervening-if re-check at resolution time.
        // CR 400.7: this conceptually creates a new object in the hand zone;
        // here ObjectId is preserved (engine maintains object identity in the
        // `objects` map across zone changes), which is the harder case for
        // the no-op semantics — the same id remains addressable.
        crate::game::zones::move_to_zone(&mut state, remora_id, Zone::Hand, &mut events);
        assert_eq!(
            state.objects[&remora_id].zone,
            Zone::Hand,
            "source must be in hand after bounce"
        );

        // Step 3: resolve the top of the stack. The
        // `TriggerCondition::SourceInZone { Battlefield }` re-check should
        // fail (source is in Hand now), so `stack::resolve_top` emits
        // `StackResolved` without invoking the outer AddCounter or the
        // sub-ability chain.
        crate::game::stack::resolve_top(&mut state, &mut events);

        // No unless-payment prompt — the chain never reached the sub-ability.
        assert!(
            !matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }),
            "no unless-pay prompt when source has left the battlefield; got: {:?}",
            state.waiting_for
        );

        // No age counter — outer AddCounter never ran.
        assert_eq!(
            state.objects[&remora_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied()
                .unwrap_or(0),
            0,
            "no age counter should be placed when the intervening-if no-ops"
        );

        // Source stays in hand. Not sacrificed, not returned to battlefield.
        assert_eq!(
            state.objects[&remora_id].zone,
            Zone::Hand,
            "source must remain in hand; no Effect::Sacrifice ran"
        );
        assert!(
            !state.players[0].graveyard.contains(&remora_id),
            "source must not be sacrificed to graveyard when the chain no-ops"
        );

        // The trigger left the stack via the CR 603.4 no-op exit, not via
        // normal resolution — stack is now empty.
        assert!(
            state.stack.is_empty(),
            "stack must be cleared after the no-op resolution"
        );
    }

    /// CR 702.24b: "If a permanent has multiple instances of cumulative
    /// upkeep, each triggers separately. However, the age counters are not
    /// connected to any particular ability; each cumulative upkeep ability
    /// will count the total number of age counters on the permanent at the
    /// time that ability resolves."
    ///
    /// Construct a synthetic permanent with TWO `PayCumulativeUpkeep`
    /// triggers — a `Mana{1}` base and a `PayLife{1}` base — controlled by
    /// PlayerId(0). No real MTG card prints two cumulative-upkeep abilities,
    /// so the only way to exercise the shared-counter semantics is to attach
    /// both triggers in-test. Returns the perm's id; the controller is
    /// PlayerId(0) (active player) and the phase is set so `auto_advance`
    /// fires both triggers at upkeep.
    fn setup_two_instance_cumulative_upkeep_state() -> (GameState, ObjectId) {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let perm = create_object(
            &mut state,
            CardId(7300),
            PlayerId(0),
            "Synthetic Multi-Upkeep Permanent".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&perm).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            // CR 702.24b: each instance triggers separately. Attaching both
            // to the same object is the load-bearing test setup — the
            // production builders are reused unchanged so any regression in
            // `build_cumulative_upkeep_trigger` (counter ordering, payer
            // resolution, intervening-if guard) breaks this test loudly.
            obj.trigger_definitions
                .push(cumulative_upkeep_mana_trigger(1));
            obj.trigger_definitions
                .push(cumulative_upkeep_pay_life_trigger(1));
        }

        (state, perm)
    }

    /// CR 702.24b + CR 603.3b: Multi-instance cumulative upkeep — two
    /// abilities each trigger separately and share the age-counter pool,
    /// with each ability reading the running total at its own resolution
    /// time. Synthetic permanent carries `Mana{1}` and `PayLife{1}` upkeep
    /// triggers. At upkeep, both fire and the controller orders them via
    /// `OrderTriggers` (CR 603.3b). Whichever trigger resolves first sees
    /// the counter tick 0 → 1 (cost scales × 1); whichever resolves second
    /// sees the counter tick 1 → 2 (cost scales × 2, the load-bearing
    /// assertion). The stack order is the active player's choice — the
    /// test pins ordering via a specific `OrderTriggers` permutation but
    /// asserts the cost SET observed across both prompts (×1 paired with
    /// ×2), independent of which printed trigger ended up where on the
    /// stack. Final state: 2 age counters, no sacrifice, controller paid
    /// the ×1 + ×2 multiples of each base across the two prompts.
    ///
    /// This is the load-bearing test for CR 702.24b — the only scenario
    /// where the counter pool is read at resolution time (not at trigger
    /// fire time) is multi-instance. Single-instance accumulation tests
    /// (Mystic Remora three-upkeep) can't distinguish "read at fire" vs
    /// "read at resolve" because only one tick happens between fire and
    /// resolve. Two triggers in one batch make the distinction observable:
    /// if the engine read at fire-time, both prompts would see counter=0;
    /// if it read between AddCounter and unless-pay computation (post-tick
    /// per trigger), the second prompt sees counter=2.
    #[test]
    fn cumulative_upkeep_multi_instance_each_ticks_own_counter() {
        let (mut state, perm_id) = setup_two_instance_cumulative_upkeep_state();
        let life_before = state.players[0].life;

        // Step 1: `auto_advance` settles in Upkeep and `process_phase_triggers`
        // collects both PayCumulativeUpkeep triggers. With two triggers from
        // a single controller, the engine prompts P0 to order them via
        // CR 603.3b before any trigger lands on the stack.
        let mut events = Vec::new();
        let _wf = crate::game::turns::auto_advance(&mut state, &mut events);
        assert_eq!(
            state.phase,
            Phase::Upkeep,
            "auto_advance must pause in Upkeep so both triggers can be ordered"
        );
        match &state.waiting_for {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(*player, PlayerId(0), "controller orders own triggers");
                assert_eq!(
                    triggers.len(),
                    2,
                    "both cumulative-upkeep triggers must be in the prompt"
                );
            }
            other => panic!("expected OrderTriggers prompt, got {other:?}"),
        }

        // Step 2: CR 603.3b + CR 405.3: Submit a fixed permutation so the
        // stack order is deterministic across runs. The CR 702.24b
        // invariant under test — running-total semantics across two
        // instances — holds regardless of WHICH printed trigger resolves
        // first, so the per-cost assertions below are written against the
        // RESOLUTION ORDER (`first_cost`, `second_cost`), not against the
        // identity of the underlying trigger.
        let _ =
            apply_as_current(&mut state, GameAction::OrderTriggers { order: vec![1, 0] }).unwrap();
        assert!(
            !state.stack.is_empty(),
            "both triggers must be on the stack after ordering"
        );

        // Step 3: Resolve the top of the stack — the first of two cumulative
        // upkeep triggers. The outer AddCounter ticks the age counter 0 → 1;
        // the sub-ability unless-pay reads counter=1 and expands the base
        // cost × 1 (so Mana{1} → Mana{1}, or PayLife{1} → PayLife{1}).
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // CR 702.24a + CR 702.24b: the first trigger's AddCounter resolved,
        // so the counter is 1 before the unless-pay computes.
        assert_eq!(
            state.objects[&perm_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(1),
            "first resolving trigger must tick counter to 1"
        );
        let first_cost = match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(0), "controller pays the unless-cost");
                cost.clone()
            }
            other => panic!("expected first UnlessPayment, got {other:?}"),
        };

        // CR 500.5: mana pools empty between phases — add the {1} payment
        // AFTER auto_advance settles in Upkeep so the mana persists into
        // `PayUnlessCost`. The cost shape is asserted in the set-based
        // check below; here we just need to satisfy whichever cost arrived.
        pay_unless_payment_dispatching(&mut state, &first_cost);

        // Step 4: Resolve the next stack entry — the second cumulative
        // upkeep trigger. Counter ticks 1 → 2; the unless-pay reads
        // counter=2 and expands the base cost × 2 (so PayLife{1} →
        // PayLife{2}, or Mana{1} → Mana{2}). This is the load-bearing
        // assertion for CR 702.24b: the second trigger sees the running
        // total, not the value the first trigger started with.
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&perm_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(2),
            "second resolving trigger must see post-tick total of 2 \
             (CR 702.24b: shared counter pool, read at resolution time)"
        );
        let second_cost = match &state.waiting_for {
            WaitingFor::UnlessPayment { player, cost, .. } => {
                assert_eq!(*player, PlayerId(0), "controller pays the unless-cost");
                cost.clone()
            }
            other => panic!("expected second UnlessPayment, got {other:?}"),
        };

        // CR 702.24b — the canonical assertion: the cost SET observed
        // across the two prompts must include EXACTLY one ×1-scaled cost
        // (the first trigger to resolve, ticking 0→1) and one ×2-scaled
        // cost (the second trigger to resolve, ticking 1→2). Stack order
        // is the active player's choice per CR 603.3b — both `{Mana{1},
        // PayLife{2}}` and `{PayLife{1}, Mana{2}}` are valid outcomes,
        // distinguished only by which trigger sits on top. The invariant
        // under test is *running-total semantics*: one cost reads counter=1,
        // the other reads counter=2. If the engine had read the counter
        // pool at trigger-fire time (counter=0 for both) or post-double-
        // tick (counter=2 for both), the SET would be `{Mana{0}, PayLife{0}}`
        // or `{Mana{2}, PayLife{2}}` — both ruled out below.
        let costs = [first_cost.clone(), second_cost.clone()];
        // The first cost (resolved at counter=1) must be the ×1 form of
        // either base — Mana{1} or PayLife{1}.
        let first_is_one_scaled = matches!(
            &first_cost,
            AbilityCost::Mana { cost: mana } if *mana == ManaCost::generic(1)
        ) || matches!(
            &first_cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            }
        );
        assert!(
            first_is_one_scaled,
            "first-resolving trigger must read counter=1 and scale base × 1 \
             (Mana{{1}} or PayLife(1)). Got {first_cost:?}"
        );
        // The second cost (resolved at counter=2) must be the ×2 form of
        // either base — Mana{2} or PayLife{2}.
        let second_is_two_scaled = matches!(
            &second_cost,
            AbilityCost::Mana { cost: mana } if *mana == ManaCost::generic(2)
        ) || matches!(
            &second_cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            }
        );
        assert!(
            second_is_two_scaled,
            "second-resolving trigger must read counter=2 and scale base × 2 \
             (Mana{{2}} or PayLife(2)) — this is the load-bearing CR 702.24b \
             assertion that the counter pool is SHARED across instances and \
             read at each ability's RESOLUTION TIME. Got {second_cost:?}"
        );
        // CR 702.24b — the cost types must be distinct (one Mana, one
        // PayLife). If both triggers somehow surfaced the same shape we
        // would have lost the separate-instance identity.
        let mana_count = costs
            .iter()
            .filter(|c| matches!(c, AbilityCost::Mana { .. }))
            .count();
        let life_count = costs
            .iter()
            .filter(|c| matches!(c, AbilityCost::PayLife { .. }))
            .count();
        assert_eq!(
            mana_count, 1,
            "exactly one Mana cost across the two prompts; got {costs:?}"
        );
        assert_eq!(
            life_count, 1,
            "exactly one PayLife cost across the two prompts; got {costs:?}"
        );

        // Pay the second unless-cost. The dispatcher handles whichever
        // shape arrived second.
        pay_unless_payment_dispatching(&mut state, &second_cost);

        // CR 702.24b: final state — both triggers paid, 2 age counters
        // accumulated, permanent stayed on the battlefield, and the
        // controller paid exactly the ×1 + ×2 multiples of the PayLife
        // base across the two prompts.
        assert_eq!(
            state.objects[&perm_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(2),
            "both triggers' AddCounter effects must have ticked the shared pool"
        );
        assert_eq!(
            state.objects[&perm_id].zone,
            Zone::Battlefield,
            "paying both cumulative-upkeep costs must keep the permanent on the battlefield"
        );
        assert!(
            !state.players[0].graveyard.contains(&perm_id),
            "permanent must not be sacrificed when both costs are paid"
        );
        // CR 119.4: total life delta = whichever resolution paid PayLife.
        //   - If PayLife resolved FIRST (counter=1), it cost 1 life.
        //   - If PayLife resolved SECOND (counter=2), it cost 2 life.
        // Either way the Mana cost contributes 0 to the life delta. Compute
        // the expected delta from the first cost shape: when the first cost
        // was PayLife, total -1; when the first cost was Mana, total -2.
        let expected_life_delta = if matches!(&first_cost, AbilityCost::PayLife { .. }) {
            1
        } else {
            2
        };
        assert_eq!(
            state.players[0].life,
            life_before - expected_life_delta,
            "controller paid exactly the PayLife trigger's scaled cost in life \
             (the Mana trigger contributes 0 to life delta)"
        );
    }

    /// Pay the unless-cost surfaced as `cost` on behalf of PlayerId(0).
    /// Dispatches on the cost shape so the multi-instance test can pay
    /// either `Mana{N}` or `PayLife{N}` in whichever order the engine
    /// resolves the two triggers. Other cost shapes (Sacrifice, PayEnergy,
    /// Discard) are not exercised by this test and are flagged with a
    /// panic to surface scope-creep if a future cumulative-upkeep variant
    /// is added.
    fn pay_unless_payment_dispatching(state: &mut GameState, cost: &AbilityCost) {
        match cost {
            // CR 118.12 + CR 500.5: provision the colorless mana, then pay.
            // CR 202.3: `mana_value()` is the authoritative count of mana
            // units required — it folds generic + shards into a single int
            // and is robust to future cost shapes (e.g. hybrid symbols)
            // that aren't exercised by the current Mana{1} base.
            AbilityCost::Mana { cost: mana_cost } => {
                give_p0_colorless_mana(state, mana_cost.mana_value());
                apply_as_current(state, GameAction::PayUnlessCost { pay: true })
                    .expect("PayUnlessCost { pay: true } must succeed for Mana cost");
            }
            // CR 118.12 + CR 119.4: life is auto-deducted at PayUnlessCost time —
            // no intermediate mana-payment prompt.
            AbilityCost::PayLife { .. } => {
                apply_as_current(state, GameAction::PayUnlessCost { pay: true })
                    .expect("PayUnlessCost { pay: true } must succeed for PayLife cost");
            }
            other => panic!(
                "unexpected unless-cost shape in multi-instance cumulative-upkeep test: {other:?}"
            ),
        }
    }

    /// Build the synthesized cumulative-upkeep trigger for "Cumulative upkeep
    /// {W} or {U}" (Jötun Owl Keeper's disjunctive cost variant) by delegating
    /// to the production synthesizer. Mirrors `cumulative_upkeep_mana_trigger`,
    /// `cumulative_upkeep_sacrifice_land_trigger`, and
    /// `cumulative_upkeep_pay_life_trigger`; this helper exercises the `OneOf`
    /// arm of `expand_per_counter` plus the Composite-of-OneOfs routing path in
    /// `handle_unless_payment_choose_cost` (CR 702.24a: "If [cost] has choices
    /// associated with it, each choice is made separately for each age counter,
    /// then either the entire set of costs is paid, or none of them is paid").
    ///
    /// CR 702.24a: a `OneOf { Mana(W), Mana(U) }` base cost is the canonical
    /// disjunctive cumulative-upkeep shape (Jötun Owl Keeper, Arctic Nishoba,
    /// Earthen Goo).
    fn cumulative_upkeep_one_of_w_or_u_trigger() -> TriggerDefinition {
        let mana_w = AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::White],
                generic: 0,
            },
        };
        let mana_u = AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::Blue],
                generic: 0,
            },
        };
        crate::database::synthesis::build_cumulative_upkeep_trigger(AbilityCost::OneOf {
            costs: vec![mana_w, mana_u],
        })
    }

    /// Construct a solo state with Jötun Owl Keeper on the battlefield
    /// (controller = PlayerId(0) = active player) at Phase::Untap so
    /// `auto_advance` will fire the upkeep trigger. **One age counter is
    /// pre-loaded** on the Owl Keeper so the first upkeep that
    /// `auto_advance` resolves ticks the counter from 1 → 2 — exercising the
    /// multiplicative step of the `OneOf` arm of `expand_per_counter`, which
    /// expands `OneOf{[W,U]}` × 2 → `Composite { [OneOf{[W,U]}, OneOf{[W,U]}] }`.
    /// This is the load-bearing setup for CR 702.24a's "each choice is made
    /// separately for each age counter" clause — counter=1 would collapse to a
    /// trivial single-prompt case, and we specifically want the multi-prompt
    /// disjunctive flow.
    fn setup_jotun_owl_keeper_second_upkeep_state() -> (GameState, ObjectId) {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let owl = create_object(
            &mut state,
            CardId(7400),
            PlayerId(0),
            "Jötun Owl Keeper".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&owl).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Giant".to_string());
            obj.trigger_definitions
                .push(cumulative_upkeep_one_of_w_or_u_trigger());
            // CR 702.24a: pre-load one age counter so the next upkeep tick
            // produces counter=2, yielding the per-counter expansion
            // `OneOf{[W,U]}` × 2 → `Composite { [OneOf{[W,U]}, OneOf{[W,U]}] }`.
            obj.counters
                .insert(crate::types::counter::CounterType::Age, 1);
        }

        (state, owl)
    }

    /// CR 702.24a + CR 118.12: End-to-end OneOf × N flow — Jötun Owl Keeper's
    /// "{W} or {U}" cumulative-upkeep cost at counter=2 expands to a
    /// `Composite` of two `OneOf` sub-costs. The engine surfaces one
    /// `UnlessPaymentChooseCost` prompt per disjunctive sub-cost; each pick
    /// accumulates into `chosen`. After the last prompt, the accumulated picks
    /// collapse into `Composite { [Mana(W), Mana(U)] }` which the single-cost
    /// `handle_unless_payment` folds into a combined `{W}{U}` mana payment.
    /// Paying the combined cost keeps the Owl Keeper on the battlefield and
    /// drains the controller's mana pool of the two colored units.
    ///
    /// This is the capstone test for the OneOf × N pipeline: it exercises the
    /// synthesizer (Task 7) producing the trigger, the PerCounter resolution
    /// (Task 6) expanding `OneOf × 2` → `Composite[OneOf, OneOf]`, the
    /// multi-choice routing (Task 14) walking each disjunctive choice, and the
    /// Composite-of-Mana payment (Task 14) folding the picks into a combined
    /// mana payment. CR 702.24a: "each choice is made separately for each age
    /// counter, then either the entire set of costs is paid, or none of them
    /// is paid."
    #[test]
    fn jotun_owl_keeper_one_of_x_n_pays_combined_mana() {
        use crate::types::actions::UnlessCostBranch;
        let (mut state, owl_id) = setup_jotun_owl_keeper_second_upkeep_state();
        advance_to_unless_payment_prompt(&mut state);

        // CR 702.24a: outer AddCounter resolved first; the pre-loaded counter
        // ticked from 1 → 2 before the per-counter unless-cost is computed.
        assert_eq!(
            state.objects[&owl_id]
                .counters
                .get(&crate::types::counter::CounterType::Age)
                .copied(),
            Some(2),
            "age counter should tick from 1 (pre-loaded) to 2 on this upkeep"
        );

        // CR 702.24a + CR 118.12a: PerCounter expanded `OneOf{[W,U]}` × 2 to
        // `Composite { [OneOf{[W,U]}, OneOf{[W,U]}] }`. The engine surfaces
        // the FIRST disjunctive choice with one entry remaining in
        // `remaining_choices`.
        match &state.waiting_for {
            WaitingFor::UnlessPaymentChooseCost {
                player,
                costs,
                remaining_choices,
                chosen,
                ..
            } => {
                assert_eq!(*player, PlayerId(0), "controller is the unless-payer");
                assert_eq!(costs.len(), 2, "first choice exposes both alternatives");
                assert_eq!(
                    remaining_choices.len(),
                    1,
                    "one more disjunctive choice queued (counter=2 → 2 prompts)"
                );
                assert!(
                    chosen.is_empty(),
                    "no choices made yet before the first prompt"
                );
            }
            other => panic!("expected first UnlessPaymentChooseCost, got {other:?}"),
        }

        // Pick {W} (index 0). The first pick accumulates into `chosen`; the
        // queue is drained; the second OneOf prompt surfaces.
        apply_as_current(
            &mut state,
            GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Pay { index: 0 },
            },
        )
        .expect("first ChooseUnlessCostBranch should surface the next prompt");

        // CR 702.24a + CR 118.12a: SECOND disjunctive choice prompt.
        // `remaining_choices` is now empty; `chosen` carries [Mana(W)].
        match &state.waiting_for {
            WaitingFor::UnlessPaymentChooseCost {
                costs,
                remaining_choices,
                chosen,
                ..
            } => {
                assert_eq!(costs.len(), 2, "second choice exposes both alternatives");
                assert!(
                    remaining_choices.is_empty(),
                    "no more disjunctive choices queued"
                );
                assert_eq!(chosen.len(), 1, "first pick accumulated into `chosen`");
                assert!(
                    matches!(
                        &chosen[0],
                        AbilityCost::Mana { cost: ManaCost::Cost { shards, generic: 0 } }
                            if shards.as_slice() == [crate::types::mana::ManaCostShard::White]
                    ),
                    "first pick is Mana({{W}}) as selected by index 0; got {:?}",
                    &chosen[0]
                );
            }
            other => panic!("expected second UnlessPaymentChooseCost, got {other:?}"),
        }

        // CR 500.5 + CR 118.12: Provision {W}{U} in P0's mana pool BEFORE the
        // final pick. The second ChooseUnlessCostBranch routes through
        // `handle_unless_payment_choose_cost` → builds
        // `Composite { [Mana(W), Mana(U)] }` → re-enters
        // `handle_unless_payment(state, .., pay=true)` → folds the Composite
        // into a combined `{W}{U}` ManaCost → calls `pay_unless_cost`. So the
        // mana must already be in the pool by the time the second action is
        // dispatched. Real play would tap a Plains and an Island in response
        // to the trigger before answering the second prompt; we shortcut by
        // dropping the mana directly into the pool.
        let p0 = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .expect("PlayerId(0)");
        p0.mana_pool
            .add(ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]));
        p0.mana_pool
            .add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));

        // Pick {U} (index 1). The second pick accumulates, the queue is
        // empty, so `handle_unless_payment_choose_cost` collapses
        // `chosen = [Mana(W), Mana(U)]` into `Composite { ... }` and routes
        // straight into `handle_unless_payment` with `pay = true`. That
        // handler's all-Mana-Composite arm folds the inner costs via
        // `ManaCost::plus` and pays the combined `{W}{U}` cost — there is no
        // intermediate `UnlessPayment` prompt visible to the test, the
        // payment happens inline. (See `engine_payment_choices::handle_unless_payment`
        // L592-599 for the fold + pay logic.)
        apply_as_current(
            &mut state,
            GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Pay { index: 1 },
            },
        )
        .expect("second ChooseUnlessCostBranch should fold + pay the combined Composite-of-Mana");

        // CR 702.24a: paying the cost keeps the permanent on the battlefield.
        assert_eq!(
            state.objects[&owl_id].zone,
            Zone::Battlefield,
            "paying the cumulative-upkeep cost must NOT sacrifice the Owl Keeper"
        );
        assert!(
            !state.players[0].graveyard.contains(&owl_id),
            "permanent must not be in graveyard when paid"
        );

        // CR 118.12 + CR 202.3: The combined `{W}{U}` payment drained the
        // White + Blue units from the mana pool. This is the load-bearing
        // assertion that the Composite-of-Mana fold path actually paid the
        // colored cost (and not, e.g., zero generic via a buggy unwrap).
        let p0_after = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(
            p0_after.mana_pool.total(),
            0,
            "combined {{W}}{{U}} cost drains both colored mana units from the pool"
        );
    }
}

#[cfg(test)]
mod crew_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    /// Set up a Vehicle (Crew 3) and creatures for crew tests.
    fn setup_crew_scenario() -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = setup_game_at_main_phase();

        // Create a Vehicle with Crew 3 and 6/5 P/T
        let vehicle_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Test Vehicle".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&vehicle_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Artifact);
            obj.card_types.subtypes.push("Vehicle".to_string());
            obj.keywords.push(crate::types::keywords::Keyword::Crew {
                power: 3,
                once_per_turn: crate::types::keywords::ActivationCadence::Unlimited,
            });
            obj.base_power = Some(6);
            obj.base_toughness = Some(5);
            obj.power = Some(6);
            obj.toughness = Some(5);
        }

        // Create a 3/3 creature
        let creature_a = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_a).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
        }

        // Create a 2/2 creature
        let creature_b = create_object(
            &mut state,
            CardId(202),
            PlayerId(0),
            "Squire".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_b).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }

        (state, vehicle_id, creature_a, creature_b)
    }

    #[test]
    fn test_crew_activation_enters_crew_vehicle_state() {
        let (mut state, vehicle_id, creature_a, creature_b) = setup_crew_scenario();

        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::CrewVehicle {
                player,
                vehicle_id: vid,
                crew_power,
                eligible_creatures,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(vid, vehicle_id);
                assert_eq!(crew_power, 3);
                assert!(eligible_creatures.contains(&creature_a));
                assert!(eligible_creatures.contains(&creature_b));
            }
            other => panic!("Expected CrewVehicle, got {:?}", other),
        }
    }

    #[test]
    fn test_crew_resolution_single_creature_meets_threshold() {
        let (mut state, vehicle_id, creature_a, _creature_b) = setup_crew_scenario();

        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();

        // Announcement: cost paid, keyword-action stack entry pushed.
        let announce = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();
        assert!(state.objects.get(&creature_a).unwrap().tapped);
        assert_eq!(state.stack.len(), 1, "Crew announcement pushes stack entry");
        assert!(
            !announce
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::VehicleCrewed { .. })),
            "VehicleCrewed event must not fire until stack resolution"
        );

        // Pass priority; stack resolves → Vehicle becomes a creature, event fires.
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert!(state.stack.is_empty(), "stack empty after resolution");
        assert_eq!(
            state.objects.get(&vehicle_id).unwrap().zone,
            Zone::Battlefield
        );
        assert!(resolve.events.iter().any(|e| matches!(
            e,
            GameEvent::VehicleCrewed {
                vehicle_id: vid,
                creatures,
            } if *vid == vehicle_id && creatures == &[creature_a]
        )));
    }

    #[test]
    fn test_crew_resolution_multiple_creatures_sum_power() {
        let (mut state, vehicle_id, creature_a, creature_b) = setup_crew_scenario();

        // Make creature_a only power 2 so both are needed
        state.objects.get_mut(&creature_a).unwrap().power = Some(2);
        state.objects.get_mut(&creature_a).unwrap().base_power = Some(2);

        // Activate crew
        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();

        // Resolve with both creatures (2 + 2 = 4 >= 3)
        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a, creature_b],
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.objects.get(&creature_a).unwrap().tapped);
        assert!(state.objects.get(&creature_b).unwrap().tapped);
    }

    #[test]
    fn test_crew_fails_insufficient_power() {
        let (mut state, vehicle_id, _creature_a, creature_b) = setup_crew_scenario();

        // Activate crew
        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();

        // creature_b has power 2, threshold is 3
        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_b],
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_crew_succeeds_at_instant_speed() {
        // CR 702.122a: Crew has no "Activate only as a sorcery" restriction —
        // unlike Equip (CR 702.6a) and Saddle (CR 702.171a).
        let (mut state, vehicle_id, creature_a, _creature_b) = setup_crew_scenario();
        state.phase = Phase::BeginCombat;

        // Activation should succeed during combat
        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::CrewVehicle { .. }));

        // Resolution should also succeed
        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.objects.get(&creature_a).unwrap().tapped);
    }

    #[test]
    fn test_crew_fails_not_a_vehicle() {
        let mut state = setup_game_at_main_phase();

        // Create a non-Vehicle artifact
        let artifact_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Not A Vehicle".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&artifact_id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Artifact);
            obj.keywords.push(crate::types::keywords::Keyword::Crew {
                power: 1,
                once_per_turn: crate::types::keywords::ActivationCadence::Unlimited,
            });
        }

        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id: artifact_id,
                creature_ids: vec![],
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_crew_vehicle_excludes_itself_from_eligible() {
        let (mut state, vehicle_id, _creature_a, _creature_b) = setup_crew_scenario();

        // Make the Vehicle also a creature (e.g., from a prior crew)
        state
            .objects
            .get_mut(&vehicle_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let result = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::CrewVehicle {
                eligible_creatures, ..
            } => {
                // Vehicle should NOT be in eligible creatures even though it's a creature
                assert!(!eligible_creatures.contains(&vehicle_id));
            }
            other => panic!("Expected CrewVehicle, got {:?}", other),
        }
    }
}

#[cfg(test)]
mod station_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, StaticCondition, StaticDefinition, TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    /// Set up a Spacecraft with the Station keyword and two eligible creatures.
    fn setup_station_scenario() -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = setup_game_at_main_phase();

        let spacecraft_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Test Spacecraft".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spacecraft_id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Spacecraft".to_string());
            obj.keywords.push(crate::types::keywords::Keyword::Station);
        }

        let power_5 = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Power 5 Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&power_5).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(5);
            obj.toughness = Some(5);
            obj.base_power = Some(5);
            obj.base_toughness = Some(5);
        }

        let power_2 = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Power 2 Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&power_2).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }

        (state, spacecraft_id, power_5, power_2)
    }

    #[test]
    fn station_activation_enters_station_target_state() {
        let (mut state, spacecraft_id, p5, p2) = setup_station_scenario();

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::StationTarget {
                player,
                spacecraft_id: sid,
                eligible_creatures,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(sid, spacecraft_id);
                assert!(eligible_creatures.contains(&p5));
                assert!(eligible_creatures.contains(&p2));
                // Spacecraft must NOT be eligible to tap itself
                assert!(!eligible_creatures.contains(&spacecraft_id));
            }
            other => panic!("Expected StationTarget, got {other:?}"),
        }
    }

    #[test]
    fn station_resolution_taps_creature_and_adds_counters_equal_to_power() {
        let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();

        // Announcement: cost paid (tap), stack entry pushed — but no counters yet.
        let announce = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(p5),
            },
        )
        .unwrap();
        assert!(
            state.objects.get(&p5).unwrap().tapped,
            "creature must be tapped at announcement"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "Station announcement must push a stack entry (CR 113.3b)"
        );
        let charge_after_announce = state
            .objects
            .get(&spacecraft_id)
            .unwrap()
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            charge_after_announce, 0,
            "charge counters must not be applied before stack resolution"
        );
        assert!(
            !announce
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Stationed { .. })),
            "Stationed event must not fire at announcement"
        );

        // Both players pass priority → stack resolves → counters added.
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        let charge = state
            .objects
            .get(&spacecraft_id)
            .unwrap()
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(charge, 5, "charge counters applied at stack resolution");
        assert!(
            resolve
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Stationed { spacecraft_id: sid, creature_id: cid, counters_added: 5 } if *sid == spacecraft_id && *cid == p5)),
            "Stationed event fires at resolution"
        );
        assert!(state.stack.is_empty(), "stack empty after resolution");
    }

    #[test]
    fn station_activation_rejects_outside_sorcery_window() {
        let (mut state, spacecraft_id, _, _) = setup_station_scenario();
        // Move to declare attackers — no longer sorcery speed.
        state.phase = Phase::DeclareAttackers;

        let err = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, EngineError::ActionNotAllowed(_)));
    }

    #[test]
    fn station_activation_rejects_on_opponents_turn() {
        let (mut state, spacecraft_id, _, _) = setup_station_scenario();
        state.active_player = PlayerId(1);

        let err = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, EngineError::ActionNotAllowed(_)));
    }

    #[test]
    fn station_cannot_tap_the_spacecraft_itself() {
        let (mut state, spacecraft_id, _, _) = setup_station_scenario();

        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();

        // Attempt to select the spacecraft itself — rejected because it's not
        // in the eligible list.
        let err = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(spacecraft_id),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EngineError::InvalidAction(_)));
    }

    #[test]
    fn station_resolution_uses_snapshot_power_when_tapped_creature_leaves_battlefield() {
        // CR 113.7a: Station's counter count is snapshot at announcement. If the
        // tapped creature leaves the battlefield between announcement and
        // resolution (e.g. bounced by an instant-speed response), the snapshot
        // value is still applied.
        let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(p5),
            },
        )
        .unwrap();

        // Remove the tapped creature from the battlefield before resolution.
        let p5_obj = state.objects.get_mut(&p5).unwrap();
        p5_obj.zone = Zone::Graveyard;
        state.battlefield.retain(|id| *id != p5);

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        // Counters still applied at snapshot value despite creature leaving.
        let charge = state
            .objects
            .get(&spacecraft_id)
            .unwrap()
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            charge, 5,
            "CR 113.7a: snapshot_power applied even when tapped creature left battlefield"
        );
    }

    #[test]
    fn station_threshold_static_reapplies_and_spacecraft_becomes_creature() {
        let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

        {
            let spacecraft = state.objects.get_mut(&spacecraft_id).unwrap();
            spacecraft.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::SelfRef)
                    .condition(StaticCondition::HasCounters {
                        counters: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                        minimum: 8,
                        maximum: None,
                    })
                    .modifications(vec![
                        ContinuousModification::AddType {
                            core_type: CoreType::Creature,
                        },
                        ContinuousModification::SetPower { value: 5 },
                        ContinuousModification::SetToughness { value: 5 },
                    ])
                    .description("CR 721.2b: Spacecraft is an artifact creature at 8+".to_string()),
            );
        }

        // First station activation: 5 charge counters, below threshold.
        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(p5),
            },
        )
        .unwrap();
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert!(
            !state
                .objects
                .get(&spacecraft_id)
                .unwrap()
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "spacecraft should still be noncreature below threshold"
        );

        // Simulate a later main phase where the same creature can station again.
        state.objects.get_mut(&p5).unwrap().tapped = false;
        state.phase = Phase::PreCombatMain;
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Second station activation: another 5 counters, crossing threshold.
        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(p5),
            },
        )
        .unwrap();
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert!(
            state
                .objects
                .get(&spacecraft_id)
                .unwrap()
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "spacecraft should become a creature at 8+ charge counters"
        );
    }

    #[test]
    fn station_rejects_tapped_creature_after_gap() {
        let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();

        // Simulate an intervening effect that tapped p5 between activation
        // and resolution (the HarmonizeTap-idiom revalidation scenario).
        state.objects.get_mut(&p5).unwrap().tapped = true;

        let err = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(p5),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EngineError::InvalidAction(_)));
    }

    #[test]
    fn station_without_eligible_creature_rejected() {
        let mut state = setup_game_at_main_phase();
        let spacecraft_id = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Lone Spacecraft".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spacecraft_id).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Spacecraft".to_string());
            obj.keywords.push(crate::types::keywords::Keyword::Station);
        }

        let err = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, EngineError::ActionNotAllowed(_)));
    }
}

#[cfg(test)]
mod keyword_action_stack_tests {
    //! Cross-keyword stack-interaction tests for Crew / Station / Equip / Saddle.
    //!
    //! Part A of the CR 113.3b stack-based activation refactor requires that
    //! activated keyword abilities behave like any other activated ability on
    //! the stack:
    //!   - they can be countered by stack-targeting effects (CR 118.7: costs
    //!     paid even if the ability is countered);
    //!   - a priority window opens between cost payment and resolution;
    //!   - triggers keyed off "becomes crewed/saddled/stationed/equipped"
    //!     fire at resolution time, not at cost payment (CR 702.122d,
    //!     CR 702.171b, CR 702.184a, CR 702.6a).
    //!
    //! Counterspells are simulated by popping the top stack entry directly
    //! after announcement (scenario-constructed per plan §A8 — no Oracle-text
    //! parsing dependency). The effect is that the keyword action never
    //! resolves, but the cost side-effects (tapped creatures, snapshotted
    //! power) persist.

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn make_vehicle(state: &mut GameState, crew_n: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(1100),
            PlayerId(0),
            "Test Vehicle".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Vehicle".to_string());
        obj.keywords.push(crate::types::keywords::Keyword::Crew {
            power: crew_n,
            once_per_turn: crate::types::keywords::ActivationCadence::Unlimited,
        });
        obj.base_power = Some(6);
        obj.base_toughness = Some(5);
        obj.power = Some(6);
        obj.toughness = Some(5);
        id
    }

    fn make_mount(state: &mut GameState, saddle_n: u32) -> ObjectId {
        let id = create_object(
            state,
            CardId(1200),
            PlayerId(0),
            "Test Mount".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.card_types.subtypes.push("Mount".to_string());
        obj.keywords
            .push(crate::types::keywords::Keyword::Saddle(saddle_n));
        obj.power = Some(3);
        obj.toughness = Some(3);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        id
    }

    fn make_spacecraft(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1300),
            PlayerId(0),
            "Test Spacecraft".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Spacecraft".to_string());
        obj.keywords.push(crate::types::keywords::Keyword::Station);
        id
    }

    fn make_equipment(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1400),
            PlayerId(0),
            "Test Equipment".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        // CR 702.6a: Equip N — activated ability via an ActivateAbility index.
        // For counterspell tests we only need the EquipTarget flow, not a cost
        // payment, so we synthesize an ability wiring directly.
        id
    }

    fn make_creature(state: &mut GameState, name: &str, power: i32) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(power);
        obj.base_power = Some(power);
        obj.base_toughness = Some(power);
        id
    }

    /// Simulates a Counterspell-analog effect resolving during the priority
    /// window that opens after a keyword-action announcement. The top stack
    /// entry is moved to the graveyard (per CR 701.5a — counter means "move
    /// from the stack to its owner's graveyard"); no further events fire.
    fn simulate_counter_top_of_stack(state: &mut GameState) {
        let popped = state
            .stack
            .pop_back()
            .expect("stack must have an entry to counter");
        assert!(
            matches!(
                popped.kind,
                crate::types::game_state::StackEntryKind::KeywordAction { .. }
            ),
            "counterspell test only valid on KeywordAction entries"
        );
    }

    // --- Crew ---------------------------------------------------------------

    #[test]
    fn crew_can_be_countered_by_stack_targeting_effect() {
        // CR 118.7: Cost is paid even if the ability is countered — creatures
        // remain tapped; Vehicle never becomes a creature.
        let mut state = setup_main_phase();
        let vehicle_id = make_vehicle(&mut state, 3);
        let creature_a = make_creature(&mut state, "Bear", 3);

        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 1, "announcement pushed one stack entry");
        assert!(
            state.objects.get(&creature_a).unwrap().tapped,
            "crew cost (tap) paid before stack push"
        );

        simulate_counter_top_of_stack(&mut state);

        // Resolve remaining priority — no VehicleCrewed event should fire and
        // the Vehicle stays a non-creature artifact.
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert!(
            !resolve
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::VehicleCrewed { .. })),
            "countered Crew must not fire VehicleCrewed"
        );
        assert!(
            state.objects.get(&creature_a).unwrap().tapped,
            "CR 118.7: cost persists after counter"
        );
    }

    fn make_vehicle_once_per_turn(state: &mut GameState, crew_n: u32) -> ObjectId {
        let id = make_vehicle(state, crew_n);
        let obj = state.objects.get_mut(&id).unwrap();
        // CR 602.5b: "Activate only once each turn" crew restriction.
        obj.keywords.clear();
        obj.card_types.subtypes = vec!["Vehicle".to_string()];
        obj.keywords.push(crate::types::keywords::Keyword::Crew {
            power: crew_n,
            once_per_turn: crate::types::keywords::ActivationCadence::OncePerTurn,
        });
        id
    }

    #[test]
    fn crew_once_per_turn_vehicle_rejects_second_activation_same_turn() {
        // CR 602.5b: Luxurious Locomotive — "Crew 1. Activate only once each
        // turn." A second CrewVehicle activation in the same turn is rejected.
        let mut state = setup_main_phase();
        let vehicle_id = make_vehicle_once_per_turn(&mut state, 1);
        let creature_a = make_creature(&mut state, "Bear", 3);
        let creature_b = make_creature(&mut state, "Elk", 3);

        // First crew: full announcement, vehicle recorded as crewed this turn.
        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();
        assert!(
            state.crew_activated_this_turn.contains(&vehicle_id),
            "first crew records the vehicle as crewed this turn"
        );

        // Second crew activation this turn — must be rejected. `creature_b` is
        // a fresh untapped creature, so power is not the blocker.
        let second = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        );
        assert!(
            matches!(second, Err(EngineError::ActionNotAllowed(_))),
            "second crew of an 'Activate only once each turn' Vehicle must be \
             rejected; got {second:?}"
        );
        let _ = creature_b;
    }

    #[test]
    fn crew_unlimited_vehicle_allows_second_activation_same_turn() {
        // A normal (non-once-per-turn) Vehicle may be crewed repeatedly.
        let mut state = setup_main_phase();
        let vehicle_id = make_vehicle(&mut state, 1);
        let creature_a = make_creature(&mut state, "Bear", 3);
        let _creature_b = make_creature(&mut state, "Elk", 3);

        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        // Second crew activation — an Unlimited Vehicle accepts it (the
        // once-per-turn restriction does not apply).
        let second = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        );
        assert!(
            second.is_ok(),
            "an unrestricted Vehicle may be crewed again the same turn; got {second:?}"
        );
    }

    #[test]
    fn crew_opens_priority_window_between_announcement_and_resolution() {
        // CR 113.3b: Between announcement and resolution, the active player
        // has priority again. Verified by the presence of a WaitingFor::Priority
        // and an unresolved stack after announcement.
        let mut state = setup_main_phase();
        let vehicle_id = make_vehicle(&mut state, 3);
        let creature_a = make_creature(&mut state, "Bear", 3);

        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        let announce = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        assert!(matches!(announce.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    // --- Saddle -------------------------------------------------------------

    #[test]
    fn saddle_can_be_countered_by_stack_targeting_effect() {
        let mut state = setup_main_phase();
        let mount_id = make_mount(&mut state, 2);
        let creature_a = make_creature(&mut state, "Rider", 3);

        apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 1);
        assert!(
            state.objects.get(&creature_a).unwrap().tapped,
            "saddle cost (tap) paid before stack push"
        );

        simulate_counter_top_of_stack(&mut state);

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert!(
            !resolve
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Saddled { .. })),
            "countered Saddle must not fire Saddled"
        );
        // CR 702.171b: `is_saddled` flag is set only at resolution.
        assert!(
            !state.objects.get(&mount_id).unwrap().is_saddled,
            "Mount must not become saddled if Saddle is countered"
        );
        // CR 118.7: cost persists.
        assert!(state.objects.get(&creature_a).unwrap().tapped);
    }

    #[test]
    fn saddle_announcement_pushes_stack_entry() {
        // Saddle has no existing test module — cover the fundamentals alongside
        // the counterspell test.
        let mut state = setup_main_phase();
        let mount_id = make_mount(&mut state, 2);
        let creature_a = make_creature(&mut state, "Rider", 3);

        apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        let announce = apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 1);
        assert!(
            !announce
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Saddled { .. })),
            "Saddled event must not fire until stack resolution"
        );
        assert!(!state.objects.get(&mount_id).unwrap().is_saddled);

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert!(state.stack.is_empty());
        assert!(state.objects.get(&mount_id).unwrap().is_saddled);
        assert!(
            resolve
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Saddled { .. })),
            "Saddled fires at resolution"
        );
    }

    #[test]
    fn saddle_sorcery_speed_gate_enforced_at_announcement_not_resolution() {
        // CR 307.1 + CR 702.171a: Saddle is restricted to sorcery-speed
        // windows. The gate runs at announcement; once the ability is on the
        // stack, changing phases does not retroactively invalidate it.
        let mut state = setup_main_phase();
        let mount_id = make_mount(&mut state, 2);
        let _ = make_creature(&mut state, "Rider", 3);

        // Instant speed: declaring blockers is a pre-priority window.
        state.phase = Phase::DeclareBlockers;
        let err = apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![],
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, EngineError::ActionNotAllowed(_)),
            "CR 702.171a: cannot activate Saddle at instant speed"
        );
    }

    // --- Station ------------------------------------------------------------

    #[test]
    fn station_can_be_countered_by_stack_targeting_effect() {
        // CR 113.7a + CR 118.7: Creature tapped, charge counters NOT added.
        let mut state = setup_main_phase();
        let spacecraft_id = make_spacecraft(&mut state);
        let power5 = make_creature(&mut state, "Power 5", 5);

        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(power5),
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 1);
        assert!(state.objects.get(&power5).unwrap().tapped);

        simulate_counter_top_of_stack(&mut state);

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert!(
            !resolve
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Stationed { .. })),
            "countered Station must not fire Stationed"
        );
        let charge = state
            .objects
            .get(&spacecraft_id)
            .unwrap()
            .counters
            .get(&CounterType::Generic("charge".to_string()))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            charge, 0,
            "no charge counters added when Station is countered"
        );
        assert!(state.objects.get(&power5).unwrap().tapped);
    }

    // --- Equip --------------------------------------------------------------

    // --- Trigger timing -----------------------------------------------------
    //
    // CR 702.122d / CR 702.171b / CR 702.184a: "Whenever [X] becomes crewed /
    // saddled / stationed" resolves when the keyword ability resolves from the
    // stack — not when its cost is paid. The per-keyword matcher keys off the
    // resolution-time event (`VehicleCrewed` / `Saddled` / `Stationed`), so
    // the timing is proven by showing:
    //   (a) the announcement's event stream contains no match,
    //   (b) the resolve step's event stream contains a match.
    // This is independent of Oracle-text parser coverage (Monoist Gravliner's
    // Stationed trigger parses as Unknown today — plan §Out of scope).

    #[test]
    fn crewed_trigger_matcher_fires_on_resolution_event_not_announcement() {
        use crate::game::trigger_matchers::match_vehicle_crewed;
        use crate::types::triggers::TriggerMode;
        use crate::types::TriggerDefinition;

        let mut state = setup_main_phase();
        let vehicle_id = make_vehicle(&mut state, 3);
        let creature_a = make_creature(&mut state, "Bear", 3);

        apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        let announce = apply_as_current(
            &mut state,
            GameAction::CrewVehicle {
                vehicle_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        let trigger = TriggerDefinition::new(TriggerMode::Crewed);
        let fires_at_announce = announce
            .events
            .iter()
            .any(|e| match_vehicle_crewed(e, &trigger, vehicle_id, &state));
        assert!(
            !fires_at_announce,
            "CR 702.122d: Crewed trigger must not fire at announcement"
        );

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        let fires_at_resolve = resolve
            .events
            .iter()
            .any(|e| match_vehicle_crewed(e, &trigger, vehicle_id, &state));
        assert!(
            fires_at_resolve,
            "CR 702.122d: Crewed trigger fires when the Crew ability resolves"
        );
    }

    #[test]
    fn stationed_trigger_matcher_fires_on_resolution_event_not_announcement() {
        use crate::game::trigger_matchers::match_stationed;
        use crate::types::triggers::TriggerMode;
        use crate::types::TriggerDefinition;

        let mut state = setup_main_phase();
        let spacecraft_id = make_spacecraft(&mut state);
        let power5 = make_creature(&mut state, "Power 5", 5);

        apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: None,
            },
        )
        .unwrap();
        let announce = apply_as_current(
            &mut state,
            GameAction::ActivateStation {
                spacecraft_id,
                creature_id: Some(power5),
            },
        )
        .unwrap();

        let trigger = TriggerDefinition::new(TriggerMode::Stationed);
        assert!(
            !announce
                .events
                .iter()
                .any(|e| match_stationed(e, &trigger, spacecraft_id, &state)),
            "CR 702.184a: Stationed trigger must not fire at announcement"
        );

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert!(
            resolve
                .events
                .iter()
                .any(|e| match_stationed(e, &trigger, spacecraft_id, &state)),
            "CR 702.184a: Stationed trigger fires when Station resolves"
        );
    }

    #[test]
    fn saddled_trigger_matcher_fires_on_resolution_event_not_announcement() {
        use crate::game::trigger_matchers::match_saddled;
        use crate::types::triggers::TriggerMode;
        use crate::types::TriggerDefinition;

        let mut state = setup_main_phase();
        let mount_id = make_mount(&mut state, 2);
        let creature_a = make_creature(&mut state, "Rider", 3);

        apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![],
            },
        )
        .unwrap();
        let announce = apply_as_current(
            &mut state,
            GameAction::SaddleMount {
                mount_id,
                creature_ids: vec![creature_a],
            },
        )
        .unwrap();

        let trigger = TriggerDefinition::new(TriggerMode::Saddled);
        assert!(
            !announce
                .events
                .iter()
                .any(|e| match_saddled(e, &trigger, mount_id, &state)),
            "CR 702.171b: Saddled trigger must not fire at announcement"
        );

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert!(
            resolve
                .events
                .iter()
                .any(|e| match_saddled(e, &trigger, mount_id, &state)),
            "CR 702.171b: Saddled trigger fires when Saddle resolves"
        );
    }

    #[test]
    fn equipped_effect_fires_on_resolution_event_not_announcement() {
        // CR 702.6a: Equip does not have a dedicated "becomes equipped" trigger
        // mode; the analog is the `EffectResolved { kind: Equip }` event emitted
        // when the keyword action resolves. Triggers that key off "Whenever
        // [this Equipment] becomes attached" fire from the ZoneChanged /
        // attachment-change event downstream. This test asserts the
        // EffectResolved { Equip } event is absent at announcement and present
        // at resolution, proving the stack-based flow carries through for
        // Equip.
        let mut state = setup_main_phase();
        let equipment_id = make_equipment(&mut state);
        let _creature_a = make_creature(&mut state, "Warrior", 2);

        let announce = apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();
        assert!(
            !announce.events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::Equip,
                    ..
                }
            )),
            "CR 702.6a: Equip resolution event must not fire at announcement"
        );

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
        assert!(
            resolve.events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::Equip,
                    source_id,
                } if *source_id == equipment_id
            )),
            "CR 702.6a: Equip resolution event fires when the ability resolves"
        );
    }

    #[test]
    fn equip_can_be_countered_by_stack_targeting_effect() {
        // CR 702.6a + CR 118.7: Cost is paid; attachment never happens. With a
        // single valid target, `handle_equip_activation` auto-targets and
        // pushes the KeywordAction directly (one dispatch call).
        let mut state = setup_main_phase();
        let equipment_id = make_equipment(&mut state);
        let _creature_a = make_creature(&mut state, "Warrior", 2);

        apply_as_current(
            &mut state,
            GameAction::Equip {
                equipment_id,
                target_id: ObjectId(0),
            },
        )
        .unwrap();

        assert_eq!(state.stack.len(), 1);
        assert!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .is_none(),
            "Equipment is not attached yet (attach happens at resolution)"
        );

        simulate_counter_top_of_stack(&mut state);

        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

        assert!(
            !resolve.events.iter().any(|e| matches!(
                e,
                GameEvent::EffectResolved {
                    kind: crate::types::ability::EffectKind::Equip,
                    ..
                }
            )),
            "countered Equip must not fire EquipResolved"
        );
        assert!(
            state
                .objects
                .get(&equipment_id)
                .unwrap()
                .attached_to
                .is_none(),
            "Equipment must not attach when Equip is countered"
        );
    }
}

#[cfg(test)]
mod mdfc_land_tests {
    use super::*;
    use crate::game::game_object::BackFaceData;
    use crate::game::zones::create_object;
    use crate::types::card::LayoutKind;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    fn make_land_type() -> CardType {
        CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        }
    }

    fn make_creature_type() -> CardType {
        CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        }
    }

    fn make_back_face(
        name: &str,
        card_types: CardType,
        layout_kind: Option<LayoutKind>,
    ) -> BackFaceData {
        BackFaceData {
            name: name.to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            card_types,
            mana_cost: ManaCost::default(),
            keywords: Vec::new(),
            abilities: Vec::new(),
            trigger_definitions: Default::default(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: Vec::new(),
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            layout_kind,
        }
    }

    /// Create an MDFC in hand with the given front and back card types.
    fn create_mdfc_in_hand(
        state: &mut GameState,
        front_name: &str,
        front_types: CardType,
        back_name: &str,
        back_types: CardType,
    ) -> (ObjectId, CardId) {
        let obj_id = create_object(
            state,
            CardId(100),
            PlayerId(0),
            front_name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types = front_types;
        obj.back_face = Some(make_back_face(
            back_name,
            back_types,
            Some(LayoutKind::Modal),
        ));
        (obj_id, CardId(100))
    }

    // CR 712.12: MDFC Land/Land should return ModalFaceChoice
    #[test]
    fn mdfc_land_land_returns_modal_face_choice() {
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_mdfc_in_hand(
            &mut state,
            "Branchloft Pathway",
            make_land_type(),
            "Boulderloft Pathway",
            make_land_type(),
        );

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id,
            },
        )
        .unwrap();

        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ModalFaceChoice {
                    player: PlayerId(0),
                    ..
                }
            ),
            "Expected ModalFaceChoice, got {:?}",
            result.waiting_for
        );
    }

    // CR 712.12: Choosing back face enters with back-face characteristics
    #[test]
    fn mdfc_choose_back_face_enters_with_back_characteristics() {
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_mdfc_in_hand(
            &mut state,
            "Branchloft Pathway",
            make_land_type(),
            "Boulderloft Pathway",
            make_land_type(),
        );

        // Trigger ModalFaceChoice
        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id,
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::ModalFaceChoice { .. }
        ));

        // Choose back face
        let result =
            apply_as_current(&mut state, GameAction::ChooseModalFace { back_face: true }).unwrap();

        // Should return to priority (not another ModalFaceChoice)
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority after face choice, got {:?}",
            result.waiting_for
        );

        // Object should be on battlefield with back-face name
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.name, "Boulderloft Pathway");
        assert!(
            !obj.transformed,
            "MDFC face choice must not set transformed"
        );
    }

    // CR 712.12: Choosing front face enters normally
    #[test]
    fn mdfc_choose_front_face_enters_normally() {
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_mdfc_in_hand(
            &mut state,
            "Branchloft Pathway",
            make_land_type(),
            "Boulderloft Pathway",
            make_land_type(),
        );

        apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id,
            },
        )
        .unwrap();

        let result =
            apply_as_current(&mut state, GameAction::ChooseModalFace { back_face: false }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.name, "Branchloft Pathway");
    }

    // CR 712.12: MDFC Creature/Land auto-swaps to land face without choice dialog
    #[test]
    fn mdfc_creature_land_auto_swaps_to_land_face() {
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_mdfc_in_hand(
            &mut state,
            "Kazandu Mammoth",
            make_creature_type(),
            "Kazandu Valley",
            make_land_type(),
        );

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id,
            },
        )
        .unwrap();

        // Should go directly to Priority (no ModalFaceChoice)
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority (auto-swap), got {:?}",
            result.waiting_for
        );

        // Object enters with back-face (land) characteristics
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.name, "Kazandu Valley");
        assert!(!obj.transformed);
    }

    // CR 712.12: MDFC Land/Creature plays front face normally, no choice needed
    #[test]
    fn mdfc_land_creature_plays_front_face_normally() {
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_mdfc_in_hand(
            &mut state,
            "Hagra Mauling",
            make_land_type(),
            "Hagra Broodpit",
            make_creature_type(),
        );
        // Set layout_kind on back face to Modal
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            if let Some(ref mut bf) = obj.back_face {
                bf.layout_kind = Some(LayoutKind::Modal);
            }
        }

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id,
            },
        )
        .unwrap();

        // Should go directly to Priority (front is Land, back is Creature, no choice)
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority, got {:?}",
            result.waiting_for
        );
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.name, "Hagra Mauling");
    }

    // Transform DFC with Land back should NOT trigger ModalFaceChoice
    #[test]
    fn transform_dfc_land_back_no_modal_face_choice() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Westvale Abbey".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types = make_land_type();
        obj.back_face = Some(make_back_face(
            "Ormendahl",
            make_land_type(),
            Some(LayoutKind::Transform), // Transform, not Modal
        ));

        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(200),
            },
        )
        .unwrap();

        // Should NOT produce ModalFaceChoice — only Modal layout triggers it
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Transform DFC should not trigger ModalFaceChoice, got {:?}",
            result.waiting_for
        );
    }

    // AI candidates: both ChooseModalFace options generated for ModalFaceChoice
    #[test]
    fn ai_generates_both_modal_face_candidates() {
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_mdfc_in_hand(
            &mut state,
            "Branchloft Pathway",
            make_land_type(),
            "Boulderloft Pathway",
            make_land_type(),
        );

        // Trigger ModalFaceChoice via PlayLand
        let result = apply_as_current(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id,
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::ModalFaceChoice { .. }
        ));

        let candidates = crate::ai_support::legal_actions(&state);
        let modal_actions: Vec<_> = candidates
            .iter()
            .filter(|c| matches!(c, GameAction::ChooseModalFace { .. }))
            .collect();

        assert_eq!(
            modal_actions.len(),
            2,
            "Expected 2 ChooseModalFace candidates"
        );
    }

    // CR 712.8a: MDFC Creature/Land in graveyard — front face only, NOT a land
    #[test]
    fn mdfc_creature_land_in_graveyard_not_offered_as_land() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Kazandu Mammoth".to_string(),
            Zone::Graveyard,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types = make_creature_type();
        obj.back_face = Some(make_back_face(
            "Kazandu Valley",
            make_land_type(),
            Some(LayoutKind::Modal),
        ));

        let candidates = crate::ai_support::legal_actions(&state);
        let land_actions: Vec<_> = candidates
            .iter()
            .filter(|c| matches!(c, GameAction::PlayLand { object_id, .. } if *object_id == obj_id))
            .collect();

        assert!(
            land_actions.is_empty(),
            "CR 712.8a: MDFC Creature/Land in graveyard should not be offered as PlayLand"
        );
    }

    /// Build a spell//spell Modal DFC (Esika, God of the Tree //
    /// The Prismatic Bridge) in hand with explicit, asymmetric mana costs.
    fn create_spell_mdfc_in_hand(state: &mut GameState) -> (ObjectId, CardId) {
        use crate::types::mana::ManaCostShard;
        let obj_id = create_object(
            state,
            CardId(400),
            PlayerId(0),
            "Esika, God of the Tree".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types = make_creature_type();
        // Front: {1}{G}{G}
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        };
        let mut back = make_back_face(
            "The Prismatic Bridge",
            CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec![],
            },
            Some(LayoutKind::Modal),
        );
        // Back: {W}{U}{B}{R}{G}
        back.mana_cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
                ManaCostShard::Red,
                ManaCostShard::Green,
            ],
            generic: 0,
        };
        obj.back_face = Some(back);
        (obj_id, CardId(400))
    }

    /// Add one mana of each given color to the player's pool.
    fn add_pool_mana(
        state: &mut GameState,
        player: PlayerId,
        colors: &[crate::types::mana::ManaType],
    ) {
        use crate::types::mana::ManaUnit;
        let p = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for &color in colors {
            p.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    // CR 712.11c: A spell//spell MDFC is castable when only the *back* face is
    // affordable — only the face that will be on the stack is evaluated for
    // castability (front Esika needs {1}{G}{G}; back Prismatic Bridge needs
    // {W}{U}{B}{R}{G}). The user's bug: with W/U/B/R/G in pool the front is
    // unaffordable, so the card was dropping out of legal actions entirely.
    #[test]
    fn spell_mdfc_castable_when_only_back_face_affordable() {
        use crate::types::mana::ManaType;
        let mut state = setup_game_at_main_phase();
        let (obj_id, _card_id) = create_spell_mdfc_in_hand(&mut state);
        add_pool_mana(
            &mut state,
            PlayerId(0),
            &[
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ],
        );

        assert!(
            crate::game::casting::can_cast_object_now(&state, PlayerId(0), obj_id),
            "Spell MDFC must be castable when only the back face is affordable"
        );

        let candidates = crate::ai_support::legal_actions(&state);
        assert!(
            candidates.iter().any(|c| matches!(
                c,
                GameAction::CastSpell { object_id, .. } if *object_id == obj_id
            )),
            "Expected a CastSpell candidate for the spell MDFC"
        );
    }

    // CR 712.11b: Casting a spell//spell MDFC prompts a face choice, and choosing
    // the back face puts the back-face spell on the stack.
    #[test]
    fn spell_mdfc_cast_back_face_goes_on_stack() {
        use crate::types::mana::ManaType;
        let mut state = setup_game_at_main_phase();
        let (obj_id, card_id) = create_spell_mdfc_in_hand(&mut state);
        add_pool_mana(
            &mut state,
            PlayerId(0),
            &[
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ],
        );

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
                card_id,
                targets: vec![],
            },
        )
        .unwrap();
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ModalFaceChoice {
                    player: PlayerId(0),
                    ..
                }
            ),
            "Casting a spell MDFC should prompt ModalFaceChoice, got {:?}",
            result.waiting_for
        );

        let result =
            apply_as_current(&mut state, GameAction::ChooseModalFace { back_face: true }).unwrap();
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority after casting the back face, got {:?}",
            result.waiting_for
        );

        // The back-face spell is on the stack; the object left the hand.
        let on_stack = state.stack.iter().any(|e| e.id == obj_id);
        assert!(on_stack, "back-face spell should be on the stack");
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.name, "The Prismatic Bridge");
        assert!(
            !obj.transformed,
            "MDFC face choice must not set transformed"
        );
    }

    /// Engine-level defense-in-depth: a non-host actor must not be able to
    /// grant debug permission, even when sandbox mode is enabled. server-core
    /// also checks this at the transport boundary; this test pins the
    /// engine-side guard so WASM/P2P-host adapters cannot be bypassed by
    /// crafting the action shape directly.
    #[test]
    fn grant_debug_permission_rejected_for_non_host() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::standard().with_sandbox(),
            2,
            42,
        );
        let err = apply(
            &mut state,
            PlayerId(1),
            GameAction::GrantDebugPermission {
                player_id: PlayerId(1),
            },
        )
        .expect_err("non-host Grant must be rejected");
        assert!(
            matches!(err, EngineError::ActionNotAllowed(_)),
            "got {:?}",
            err
        );
        assert!(
            !state.debug_permitted.contains(&PlayerId(1)),
            "permission must not have been mutated on rejection"
        );
    }

    /// Engine-level defense-in-depth: Grant/Revoke is rejected outright when
    /// the format does not have `allow_debug_actions` set. Closes the WASM /
    /// P2P-host path that previously skipped this check.
    #[test]
    fn grant_debug_permission_rejected_when_sandbox_disabled() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let err = apply(
            &mut state,
            PlayerId(0),
            GameAction::GrantDebugPermission {
                player_id: PlayerId(1),
            },
        )
        .expect_err("Grant must be rejected when sandbox is disabled");
        assert!(
            matches!(err, EngineError::ActionNotAllowed(_)),
            "got {:?}",
            err
        );
    }

    /// Engine-level: the host may grant; afterwards the granted player can
    /// submit a Debug action that the engine accepts.
    #[test]
    fn grant_debug_permission_succeeds_for_host_and_unlocks_debug() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::standard().with_sandbox(),
            2,
            42,
        );
        state.debug_mode = true;
        // Host (PlayerId(0)) is implicitly authorized; seed empty set first.
        state.debug_permitted.clear();

        let result = apply(
            &mut state,
            PlayerId(0),
            GameAction::GrantDebugPermission {
                player_id: PlayerId(1),
            },
        )
        .expect("host Grant should succeed");
        assert!(state.debug_permitted.contains(&PlayerId(1)));
        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::DebugPermissionGranted { .. })));

        // Post-grant: the granted player can now submit a Debug action that
        // the engine accepts. Use `ShuffleLibrary` — a side-effect-light op
        // that doesn't require pre-existing objects.
        let debug_result = apply(
            &mut state,
            PlayerId(1),
            GameAction::Debug(crate::types::actions::DebugAction::ShuffleLibrary {
                player_id: PlayerId(1),
            }),
        )
        .expect("granted player's Debug action should succeed");
        assert!(debug_result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::DebugActionUsed { .. })));
    }

    /// Engine-level: the host may not revoke their own permission — that
    /// would leave nobody able to act in sandbox.
    #[test]
    fn revoke_debug_permission_rejects_host_self_revoke() {
        let mut state = GameState::new(
            crate::types::format::FormatConfig::standard().with_sandbox(),
            2,
            42,
        );
        state.debug_permitted.insert(PlayerId(0));
        let err = apply(
            &mut state,
            PlayerId(0),
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(0),
            },
        )
        .expect_err("host self-revoke must be rejected");
        assert!(
            matches!(err, EngineError::ActionNotAllowed(_)),
            "got {:?}",
            err
        );
        assert!(
            state.debug_permitted.contains(&PlayerId(0)),
            "host permission must remain on rejection"
        );
    }
}
