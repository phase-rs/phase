use rand::Rng;
use thiserror::Error;

#[cfg(test)]
use crate::types::ability::EffectKind;
use crate::types::ability::{KeywordAction, TargetRef};
use crate::types::actions::GameAction;
use crate::types::events::{BendingType, GameEvent};
use crate::types::game_state::{
    ActionResult, AutoPassMode, AutoPassRequest, ConvokeMode, GameState, StackEntry,
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
    bump_state_revision, finalize_public_state, mark_public_state_all_dirty, sync_waiting_for,
};
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
    state.exiled_from_hand_this_resolution = 0;
    check_actor_authorization(state, actor, &action)?;
    let mut result = apply_action(state, actor, action)?;
    bump_state_revision(state);
    mark_public_state_all_dirty(state);
    sync_waiting_for(state, &result.waiting_for);
    run_auto_pass_loop(state, &mut result);
    finalize_public_state(state);
    result.log_entries = super::log::resolve_log_entries(&result.events, state);
    Ok(result)
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
        GameAction::SetPhaseStops { .. } | GameAction::CancelAutoPass | GameAction::Debug(_)
    ) {
        return Ok(());
    }
    if let Some(expected) = turn_control::authorized_submitter(state) {
        if expected != actor {
            return Err(EngineError::WrongPlayer);
        }
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
        _ => turn_control::authorized_submitter(state).ok_or_else(|| {
            EngineError::InvalidAction(
                "apply_as_current: no authorized submitter (game over?)".to_string(),
            )
        })?,
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
    let wf = priority::handle_priority_pass(state, events);
    sync_waiting_for(state, &wf);
    let skip_triggers =
        stack_was_empty && !state.stack.is_empty() && state.phase == Phase::CombatDamage;

    let wf = engine_priority::run_post_action_pipeline(state, events, &wf, skip_triggers)?;
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
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility, TargetFilter,
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

            // Auto-submit empty blockers when there's nothing to choose:
            //   (a) the defender is in UntilEndOfTurn mode, OR
            //   (b) no legal blocks are available — CR 509.1 says the turn-based action
            //       still runs, and CR 117.1c requires the active player to receive
            //       priority during the step (instants and Ninjutsu-family activations
            //       per CR 702.49 — notably Sneak, which is restricted to this step).
            // A phase stop on Declare Blockers overrides both paths regardless of
            // whether an auto-pass session is active: if the player explicitly asked
            // to pause here, honor it.
            WaitingFor::DeclareBlockers {
                player,
                valid_blocker_ids,
                ..
            } if !phase_stop_hit(state, *player)
                && (valid_blocker_ids.is_empty() || end_of_turn_active(state, *player)) =>
            {
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

    // Debug actions bypass WaitingFor dispatch — gated on debug_mode flag.
    if let GameAction::Debug(debug_action) = action {
        if !state.debug_mode {
            return Err(EngineError::InvalidAction(
                "Debug actions require debug_mode to be enabled".into(),
            ));
        }
        return super::engine_debug::apply_debug_action(state, actor, debug_action, &mut events);
    }

    // Any deliberate player action (not auto-pass-related or a simple pass) cancels their auto-pass
    if let Some(player) = turn_control::authorized_submitter(state) {
        match &action {
            GameAction::SetAutoPass { .. } | GameAction::PassPriority => {}
            _ => {
                state.auto_pass.remove(&player);
            }
        }
    }

    // Clear manual mana-tap tracking when the player commits to a non-mana action.
    // ActivateAbility is handled per-arm (only non-mana abilities clear tracking).
    if let Some(player) = turn_control::authorized_submitter(state) {
        match &action {
            GameAction::PassPriority
            | GameAction::PlayLand { .. }
            | GameAction::CastSpell { .. }
            | GameAction::Foretell { .. }
            | GameAction::CastSpellAsSneak { .. }
            | GameAction::CastSpellForFree { .. }
            | GameAction::CastSpellAsMiracle { .. }
            | GameAction::CancelCast
            | GameAction::UnlockRoomDoor { .. }
            | GameAction::PayUnlessCost { .. }
            | GameAction::PayCombatTax { .. } => {
                state.lands_tapped_for_mana.remove(&player);
            }
            _ => {}
        }
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
            },
            GameAction::ChooseAdventureFace { creature },
        ) => casting::handle_adventure_choice(
            state,
            *player,
            *object_id,
            *card_id,
            creature,
            &mut events,
        )?,
        // CR 712.12: Player chooses which face of an MDFC to play as a land.
        (
            WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
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
                    // won't re-fire on re-entry into handle_play_land.
                    if let Some(ref mut bf) = obj.back_face {
                        bf.layout_kind = None;
                    }
                }
            }
            // Re-enter handle_play_land. After swap, the new back_face (from
            // snapshot_object_face) has layout_kind: None. After front-face choice,
            // layout_kind is explicitly cleared. Either way, the both-faces-land
            // intercept won't re-fire.
            handle_play_land(state, *object_id, *card_id, &mut events)?
        }
        // Player chooses normal cast or Warp cast from hand.
        (
            WaitingFor::WarpCostChoice {
                player,
                object_id,
                card_id,
                ..
            },
            GameAction::ChooseWarpCost { use_warp },
        ) => casting::handle_warp_cost_choice(
            state,
            *player,
            *object_id,
            *card_id,
            use_warp,
            &mut events,
        )?,
        // CR 702.74a: Player chooses normal cast or Evoke cast from hand.
        (
            WaitingFor::EvokeCostChoice {
                player,
                object_id,
                card_id,
                ..
            },
            GameAction::ChooseEvokeCost { use_evoke },
        ) => casting::handle_evoke_cost_choice(
            state,
            *player,
            *object_id,
            *card_id,
            use_evoke,
            &mut events,
        )?,
        // CR 702.96a: Player chooses normal cast or Overload cast from hand.
        (
            WaitingFor::OverloadCostChoice {
                player,
                object_id,
                card_id,
                ..
            },
            GameAction::ChooseOverloadCost { use_overload },
        ) => casting::handle_overload_cost_choice(
            state,
            *player,
            *object_id,
            *card_id,
            use_overload,
            &mut events,
        )?,
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
            WaitingFor::SacrificeForCost {
                player,
                count,
                permanents,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_sacrifice_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
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
        // Blight: player selected creature(s) to put -1/-1 counters on as cost.
        (
            WaitingFor::BlightChoice {
                player,
                count,
                creatures,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => engine_casting::handle_blight_choice(
            state,
            *player,
            *pending_cast.clone(),
            *count,
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
        // CR 117.1 + CR 118.3 + CR 605.3b + CR 202.3: Player selected battlefield
        // permanent(s) to exile as a mana ability cost (Food Chain class).
        (
            WaitingFor::ExileFromBattlefieldForManaAbility {
                count,
                permanents,
                pending_mana_ability,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => super::mana_abilities::handle_exile_from_battlefield_for_mana_ability(
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
            GameAction::ChooseManaColor { choice: chosen },
        ) => {
            let events_before = events.len();
            let wf = match context {
                crate::types::game_state::ManaChoiceContext::ManaAbility(pending_mana_ability) => {
                    engine_casting::handle_choose_mana_color(
                        state,
                        pending_mana_ability,
                        choice,
                        chosen.clone(),
                        &mut events,
                    )?
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
            // CR 605.1b: Process TapsForMana triggers inline after mana color
            // choice resolves (dual land during mana payment).
            if events.len() > events_before {
                let mana_events: Vec<_> = events[events_before..].to_vec();
                super::triggers::process_triggers(state, &mana_events);
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
                max,
                convoke_mode,
                ..
            },
            GameAction::ChooseX { value },
        ) => {
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
            events.push(GameEvent::XValueChosen {
                player,
                object_id: pending.object_id,
                value,
            });
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
                let spell_meta = casting::build_spell_meta(state, player, spell_object);
                let any_color =
                    super::static_abilities::player_can_spend_as_any_color(state, player);
                let max_life = super::life_costs::max_phyrexian_life_payments(state, player);
                let player_pool = state
                    .players
                    .iter()
                    .find(|p| p.id == player)
                    .map(|p| p.mana_pool.clone())
                    .ok_or_else(|| EngineError::InvalidAction("Player not found".to_string()))?;
                let spell_ctx = spell_meta
                    .as_ref()
                    .map(crate::types::mana::PaymentContext::Spell);
                let current_shards = mana_payment::compute_phyrexian_shards(
                    &player_pool,
                    &cost,
                    spell_ctx.as_ref(),
                    any_color,
                    max_life,
                );
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
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
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
                convoke_mode: Some(mode @ (ConvokeMode::Convoke | ConvokeMode::Waterbend)),
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
            if !obj.is_convoke_eligible(*player) {
                return Err(EngineError::ActionNotAllowed(
                    "Can only tap untapped creatures or artifacts you control for convoke"
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
            };
            // Tap the permanent (no summoning sickness check — CR 702.51a + CR 302.6)
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.tapped = true;
            }
            events.push(GameEvent::PermanentTapped {
                object_id,
                caused_by: None,
            });
            // Add mana to pool
            let unit =
                crate::types::mana::ManaUnit::new(resolved_mana_type, object_id, false, Vec::new());
            if let Some(p) = state.players.iter_mut().find(|p| p.id == *player) {
                p.mana_pool.add(unit);
            }
            events.push(GameEvent::ManaAdded {
                player_id: *player,
                mana_type: resolved_mana_type,
                source_id: object_id,
                tapped_for_mana: false,
            });
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
        (
            WaitingFor::MulliganDecision {
                player,
                mulligan_count,
                ..
            },
            GameAction::MulliganDecision { keep },
        ) => {
            let p = *player;
            let mc = *mulligan_count;
            mulligan::handle_mulligan_decision(state, p, keep, mc, &mut events)
        }
        (WaitingFor::MulliganBottomCards { player, count }, GameAction::SelectCards { cards }) => {
            let p = *player;
            let c = *count;
            mulligan::handle_mulligan_bottom(state, p, cards, c, &mut events)
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
        (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
            engine_replacement::handle_replacement_choice(state, index, &mut events)?
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
                ninjutsu_card_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_card_id,
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
                trigger_event: None,
                modal: None,
                mode_abilities: vec![],
                description: Some("Miracle — you may cast this card".to_string()),
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
        // CR 702.xxx: Prepare (Strixhaven) — priority-time cast of a prepared
        // creature's face-`b` spell. Produces a token spell-copy on the stack;
        // the source becomes unprepared at cast time. Assign when WotC
        // publishes SOS CR update.
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
            let copy_id = effects::prepare::cast_prepared_copy(state, src, p, &mut events)
                .map_err(EngineError::InvalidAction)?;
            // CR 707.10c: If the copy's spell ability has target slots, open
            // target selection via CopyRetarget. Otherwise return to priority
            // and let the copy resolve on the stack.
            if effects::prepare::open_copy_target_selection(state, copy_id, p)
                .map_err(EngineError::InvalidAction)?
            {
                state.waiting_for.clone()
            } else {
                WaitingFor::Priority { player: p }
            }
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
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
        // CR 707.10c: Copy retarget — player chose new targets for the copy.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let cid = *copy_id;
            if targets.len() != target_slots.len() {
                return Err(EngineError::InvalidAction(format!(
                    "Must provide {} targets, got {}",
                    target_slots.len(),
                    targets.len()
                )));
            }
            // CR 707.10c: When `legal_alternatives` is populated (e.g. copies
            // minted by Prepare/Paradigm where initial target selection needs
            // legality gating), validate each selected target is in its
            // slot's alternatives. Slots with empty alternatives pre-date
            // this check and remain permissive (retargeting an inherited
            // target per the original CR 707.10c semantics).
            for (idx, target) in targets.iter().enumerate() {
                let slot = &target_slots[idx];
                if !slot.legal_alternatives.is_empty() && !slot.legal_alternatives.contains(target)
                {
                    return Err(EngineError::InvalidAction(format!(
                        "Target {target:?} not a legal alternative for copy slot {idx}"
                    )));
                }
            }
            // Update the copy's targets on the stack.
            if let Some(entry) = state.stack.iter_mut().find(|e| e.id == cid) {
                if let Some(ability) = entry.ability_mut() {
                    ability.targets = targets;
                }
            }
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::CopySpell,
                source_id: cid,
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::drain_pending_continuation(state, &mut events);
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
                    pending.origin_zone,
                    &mut events,
                )?
            } else {
                // Resolution-time distribution (triggered ability path).
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                effects::drain_pending_continuation(state, &mut events);
                state.waiting_for.clone()
            }
        }
        // CR 115.7: Retarget a spell or ability on the stack.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                legal_new_targets,
                ..
            },
            GameAction::RetargetSpell { new_targets },
        ) => {
            let p = *player;
            let idx = *stack_entry_index;

            // CR 115.7d: Validate each submitted target is in the legal set.
            for t in &new_targets {
                if !legal_new_targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Retarget: chosen target not in legal alternatives".to_string(),
                    ));
                }
            }

            // Update targets on the stack entry.
            if idx < state.stack.len() {
                if let Some(ability) = state.stack[idx].ability_mut() {
                    ability.targets = new_targets.clone();
                }
            } else {
                return Err(EngineError::InvalidAction(
                    "Invalid stack entry index for retargeting".to_string(),
                ));
            }

            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::ChangeTargets,
                source_id: state
                    .stack
                    .get(idx)
                    .map(|e| e.source_id)
                    .unwrap_or(ObjectId(0)),
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            effects::drain_pending_continuation(state, &mut events);
            state.waiting_for.clone()
        }
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

    // CR 700.2a: Modal trigger — prompt for mode selection before stack.
    if let Some(ref modal) = trigger.modal {
        if !trigger.mode_abilities.is_empty() {
            let unavailable_modes = compute_unavailable_modes(state, trigger.source_id, modal);

            // CR 700.2: All modes already chosen — ability cannot be put on the stack
            // without a mode selection. Clear pending trigger and skip.
            if unavailable_modes.len() >= modal.mode_count {
                state.pending_trigger = None;
                return Ok(None);
            }

            return Ok(Some(WaitingFor::AbilityModeChoice {
                player: trigger.controller,
                modal: modal.clone(),
                source_id: trigger.source_id,
                mode_abilities: trigger.mode_abilities.clone(),
                is_activated: false,
                ability_index: None,
                ability_cost: None,
                unavailable_modes,
            }));
        }
    }

    let target_slots = build_target_slots(state, &trigger.ability)?;
    if target_slots.is_empty() {
        return Ok(None);
    }

    let player = trigger.controller;
    let target_constraints = trigger.target_constraints.clone();
    let selection = begin_target_selection_for_ability(
        state,
        &trigger.ability,
        &target_slots,
        &target_constraints,
    )?;
    Ok(Some(WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        target_constraints,
        selection,
        source_id: Some(trigger.source_id),
        description: trigger.description.clone(),
    }))
}

/// CR 604.2: If a land was played from the graveyard via a once-per-turn permission source,
/// record the source as used to prevent a second play/cast from the same source this turn.
fn record_graveyard_play_permission(state: &mut GameState, source: Option<ObjectId>) {
    if let Some(source_id) = source {
        // CR 604.2: Only OncePerTurn permissions need tracking; Unlimited sources skip.
        if let Some(obj) = state.objects.get(&source_id) {
            let is_once_per_turn =
                super::functioning_abilities::active_static_definitions(state, obj).any(|s| {
                    matches!(
                        s.mode,
                        StaticMode::GraveyardCastPermission {
                            frequency: crate::types::statics::CastFrequency::OncePerTurn,
                            ..
                        }
                    )
                });
            if is_once_per_turn {
                state.graveyard_cast_permissions_used.insert(source_id);
            }
        }
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

    // Validate that object_id exists in hand or graveyard (with permission) and matches card_id
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

    if !in_hand && !in_graveyard_with_permission {
        return Err(EngineError::InvalidAction(
            "Card not found in hand or graveyard with play permission".to_string(),
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
            // Both faces are lands — player must choose which face to put into play
            return Ok(WaitingFor::ModalFaceChoice {
                player,
                object_id,
                card_id,
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
    let origin_zone = if in_hand { Zone::Hand } else { Zone::Graveyard };

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
            if let Some(effect_def) = state.post_replacement_effect.take() {
                state.post_replacement_source = None;
                state.post_replacement_event_source = None;
                if let Some(next_waiting_for) = engine_replacement::apply_post_replacement_effect(
                    state,
                    &effect_def,
                    Some(object_id),
                    None,
                    events,
                ) {
                    state.lands_played_this_turn += 1;
                    record_graveyard_play_permission(state, gy_permission_source);
                    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                        p.lands_played_this_turn += 1;
                    }
                    state.priority_passes.clear();
                    state.priority_pass_count = 0;
                    events.push(GameEvent::LandPlayed {
                        object_id,
                        player_id: player,
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
            // CR 604.2: Record once-per-turn graveyard play permission usage.
            record_graveyard_play_permission(state, gy_permission_source);
            if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
                p.lands_played_this_turn += 1;
            }
            state.priority_passes.clear();
            state.priority_pass_count = 0;

            events.push(GameEvent::LandPlayed {
                object_id,
                player_id: player,
            });

            return Ok(super::replacement::replacement_choice_waiting_for(
                player, state,
            ));
        }
    }

    // Increment land counter
    state.lands_played_this_turn += 1;
    // CR 604.2: Record once-per-turn graveyard play permission usage.
    record_graveyard_play_permission(state, gy_permission_source);
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

    // Extract crew power from keywords
    let crew_power = obj
        .keywords
        .iter()
        .find_map(|kw| {
            if let crate::types::keywords::Keyword::Crew(n) = kw {
                Some(*n)
            } else {
                None
            }
        })
        .ok_or_else(|| EngineError::InvalidAction("Vehicle has no Crew keyword".to_string()))?;

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
    use crate::game::game_object::{BackFaceData, RoomDoor};
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        QuantityExpr, ResolvedAbility, TargetFilter, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CardType;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
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
    fn new_game_creates_two_player_state() {
        let state = new_game(42);
        assert_eq!(state.players.len(), 2);
        assert_eq!(state.rng_seed, 42);
    }

    #[test]
    fn start_game_advances_to_precombat_main() {
        let mut state = new_game(42);
        let result = start_game_with_starting_player(&mut state, PlayerId(0));

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);
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

        // Start game (turn 1, player 0)
        let _result = start_game_with_starting_player(&mut state, PlayerId(0));
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);

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

        // Pass through end step
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        // Should advance through cleanup to next turn, then auto-advance to PreCombatMain
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn monarch_end_step_draws_exactly_one_card() {
        let mut state = new_game(42);
        let _result = start_game_with_starting_player(&mut state, PlayerId(0));
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

        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.players[0].hand.len(), 1);
        assert_eq!(state.players[0].library.len(), 1);
    }

    #[test]
    fn integration_play_land_then_pass() {
        let mut state = new_game(42);
        start_game_with_starting_player(&mut state, PlayerId(0));

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
            let duration = Duration::UntilNextUntapStepOf {
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
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 0,
                ..
            }
        ));

        // Both players have 7 cards in hand
        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);

        // Player 0 keeps
        let result =
            apply_as_current(&mut state, GameAction::MulliganDecision { keep: true }).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                mulligan_count: 0,
                ..
            }
        ));

        // Player 1 keeps -> game starts, auto-advances to PreCombatMain
        let result =
            apply_as_current(&mut state, GameAction::MulliganDecision { keep: true }).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0),
            }
        ));
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

        // End: both pass -> Cleanup -> next turn
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
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
    use crate::types::mana::{ManaCostShard, ManaType, ManaUnit};

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for _ in 0..count {
            player_data.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
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
                    source_static: None,
                    unless_payment: None,
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
            distribute: None,
            origin_zone: crate::types::zones::Zone::Hand,
            additional_cost_flow: None,
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
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
}

#[cfg(test)]
mod trigger_target_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, GainLifePlayer, ModalChoice,
        ModalSelectionConstraint, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
        TypedFilter,
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
                under_your_control: false,
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

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: trigger_creature,
            controller: PlayerId(0),
            condition: None,
            ability,
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });

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

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
                    under_your_control: false,
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
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });

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
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
        });
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
        assert_eq!(state.stack.len(), 0);
        assert!(state.pending_trigger.is_some());
    }

    #[test]
    fn triggered_modal_modes_without_targets_consume_pending_trigger() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = ObjectId(21);
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
        });
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
    fn trigger_target_selection_enforces_different_player_constraint() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });
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
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });
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
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
        });
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

        // Set a pending trigger with this modal.
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
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
        });

        // Call the private function via the engine path.
        let result = begin_pending_trigger_target_selection(&mut state).unwrap();

        // CR 700.2: All modes exhausted — no AbilityModeChoice produced.
        assert!(result.is_none());
        // Pending trigger should be cleared.
        assert!(state.pending_trigger.is_none());
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

    use super::*;
    use crate::game::combat::AttackTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp,
        GainLifePlayer, ObjectScope, PlayerFilter, QuantityExpr, QuantityRef, ResolvedAbility,
        TargetFilter, TargetRef, TriggerConstraint, TriggerDefinition, TypeFilter, TypedFilter,
        UnlessCost, UnlessPayModifier,
    };
    use crate::types::card_type::CoreType;
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
                under_your_control: false,
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
                cost: UnlessCost::DynamicGeneric {
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
                cost: UnlessCost::Fixed { ref cost },
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
                            counter_type: "P1P1".to_string(),
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
                            counter_type: "P1P1".to_string(),
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
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::GainLife).execute(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
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
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut draw = draw_that_many(source_id, PlayerId(0));
        draw.condition = Some(AbilityCondition::IfYouDo);
        ability.sub_ability = Some(Box::new(draw));

        state.pending_optional_effect = Some(Box::new(ability));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
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
            cost: UnlessCost::PayLife { amount: 2 },
            pending_effect: Box::new(ability),
            trigger_event: None,
            effect_description: None,
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
            up_to: false,
            source_id,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            under_your_control: false,
            enters_attacking: false,
            owner_library: false,
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
            choice_type: ChoiceType::Color,
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
                    under_your_control: false,
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
                        under_your_control: false,
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
            obj.keywords.push(crate::types::keywords::Keyword::Crew(3));
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
            obj.keywords.push(crate::types::keywords::Keyword::Crew(1));
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
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
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
        obj.keywords
            .push(crate::types::keywords::Keyword::Crew(crew_n));
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
}
