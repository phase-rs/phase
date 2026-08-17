use serde::{Deserialize, Serialize};

use crate::ai_support::AiDecisionContract;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ResolveAllConsentRun, WaitingFor};
use crate::types::log::GameLogEntry;
use crate::types::player::PlayerId;

use super::engine::{apply_action_boundary_with_stack_limit, PublicFinalizeMode};
use super::public_state::finalize_display_state;
use super::{interaction, stack, topology, turn_control};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveAllFastForwardResult {
    pub events: Vec<GameEvent>,
    pub waiting_for: WaitingFor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_entries: Vec<GameLogEntry>,
    pub items_resolved: u32,
    /// Stack depth at this chunk's entry. The frontend latches the first
    /// chunk's `total` as the storm-origin denominator for progress display.
    pub total: u32,
    /// Every action applied during this batch (including priority passes
    /// fast-forwarded by `seed_remaining_priority_cycle_passes`, which are
    /// semantically equivalent to — but bypass — an explicit `PassPriority`
    /// through `apply`), in submission order. `#[serde(skip)]`: this is
    /// consumed in-process by the WASM bridge to extend the Replay system's
    /// recording (see `crates/engine-wasm/src/lib.rs::resolve_all`) and must
    /// never reach the JS-visible result shape.
    #[serde(skip)]
    pub recorded_actions: Vec<(PlayerId, GameAction)>,
}

#[derive(Debug, Clone)]
pub enum ResolveAllCallbackDecision {
    /// A non-requester AI decision verified against the exact current
    /// engine-issued candidate domain. Raw non-pass actions are deliberately
    /// not accepted by Resolve All.
    Proposal {
        contract: AiDecisionContract,
        action: GameAction,
    },
    /// Narrow internal shortcut for priority passing. This remains raw because
    /// seeded passes represent future seats and are never individually
    /// dispatched as a current prompt.
    Action(GameAction),
    Stop,
}

/// Resolves the greatest prefix which has already received every priority
/// representative's explicit, run-scoped Resolve All consent.
///
/// Unlike the legacy callback fast-forward below, this path never asks an AI
/// (or any future priority holder) whether to pass.  Consent is the sole
/// authority. Each prospective resolution is materialized on a clone through
/// a complete, ordinary priority cycle, with `Some(1)` preventing any of the
/// existing stack batchers from consuming more than its one stack entry. The
/// clone is committed only when it settles to an unchanged-topology Priority
/// checkpoint with exactly that entry removed. This is intentionally a
/// greatest-safe-prefix proof, not loop detection or state equality.
pub fn resolve_all_ready_prefix(
    state: &mut GameState,
    requester: PlayerId,
) -> ResolveAllFastForwardResult {
    let total = state.stack.len() as u32;
    let mut events = Vec::new();
    let mut log_entries = Vec::new();
    let mut recorded_actions = Vec::new();
    let mut items_resolved = 0;

    let Some(run) = ready_consent_run(state, requester).cloned() else {
        if matches!(&state.waiting_for, WaitingFor::ResolveAllReady { .. }) {
            turn_control::invalidate_resolve_all_consent(state);
            finalize_display_state(state);
            interaction::ensure_interaction_authority(state);
        }
        return ResolveAllFastForwardResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries,
            items_resolved,
            total,
            recorded_actions,
        };
    };

    // Ready is deliberately inert. Materialize the saved priority checkpoint
    // only inside this Resolve All consumer; ordinary actions can never pass
    // through Ready.
    state.waiting_for = WaitingFor::Priority {
        player: run.priority_snapshot.waiting_player,
    };

    let resolution_cap = if run.max_resolutions == 0 {
        u32::MAX
    } else {
        run.max_resolutions
    };

    while items_resolved < resolution_cap && !state.stack.is_empty() {
        let mut proof = state.clone();
        let stack_before = proof.stack.len();
        let Some((boundary, mut actions)) = materialize_one_consented_resolution(&mut proof, &run)
        else {
            break;
        };

        // CR 117.5 + CR 704.3 + CR 603.3b: do not collapse across any new
        // checkpoint work. In particular, if item N causes a shuffle/dies/etc.
        // trigger, this refuses N and leaves it on the live stack for ordinary
        // priority, while earlier committed entries remain collapsed.
        if stack_resolved_count(&boundary.events) != 1
            || proof.stack.len().saturating_add(1) != stack_before
            || !matches!(proof.waiting_for, WaitingFor::Priority { .. })
            || !stack::priority_checkpoint_is_settled(&proof)
            || !consent_authorization_matches(&proof, &run)
        {
            break;
        }

        items_resolved += 1;
        events.extend(boundary.events);
        log_entries.extend(boundary.log_entries);
        recorded_actions.append(&mut actions);
        *state = proof;
    }

    // Authorization is one run only. Once the proved prefix ends (including
    // a zero-length or cap boundary), return the remaining stack to ordinary
    // priority; no later stack entry inherits this consent.
    turn_control::invalidate_resolve_all_consent(state);
    finalize_display_state(state);
    interaction::ensure_interaction_authority(state);

    ResolveAllFastForwardResult {
        events,
        waiting_for: state.waiting_for.clone(),
        log_entries,
        items_resolved,
        total,
        recorded_actions,
    }
}

/// Returns whether the frozen Ready consent run authorizes this requester.
/// Transport callers must reject an unauthorized request before mutating the
/// authoritative session; the resolver's fail-closed invalidation remains its
/// defense-in-depth boundary.
pub fn resolve_all_ready_requester_is_authorized(state: &GameState, requester: PlayerId) -> bool {
    ready_consent_run(state, requester).is_some()
}

/// Validates the frozen Phase-1 consent against the live topology before the
/// Ready state is materialized. A changed controller, eliminated player, or
/// stale requester fails closed without invoking a speculative callback.
pub fn resolve_all_ready_is_authorized(state: &GameState, requester: PlayerId) -> bool {
    ready_consent_run(state, requester).is_some()
}

fn ready_consent_run(state: &GameState, requester: PlayerId) -> Option<&ResolveAllConsentRun> {
    let WaitingFor::ResolveAllReady { epoch } = &state.waiting_for else {
        return None;
    };
    let run = state.resolve_all_consent_run.as_ref().filter(|run| {
        state.auto_pass.is_empty()
            && run.epoch == *epoch
            && run.participants.iter().all(|p| p.granted)
    })?;
    (run.participants
        .iter()
        .any(|participant| participant.authorized_submitter == requester)
        && state.priority_player == run.priority_snapshot.priority_player
        && state.priority_pass_count == run.priority_snapshot.priority_pass_count
        && state.priority_passes == run.priority_snapshot.priority_passes
        && consent_authorization_matches(state, run))
    .then_some(run)
}

fn consent_authorization_matches(state: &GameState, run: &ResolveAllConsentRun) -> bool {
    let mut representatives = topology::priority_pass_participants(state);
    let current =
        topology::priority_pass_representative(state, run.priority_snapshot.waiting_player);
    let Some(current_index) = representatives
        .iter()
        .position(|representative| *representative == current)
    else {
        return false;
    };
    representatives.rotate_left(current_index);
    representatives.len() == run.participants.len()
        && run.participants.iter().all(|frozen| {
            representatives.contains(&frozen.representative)
                && turn_control::authorized_submitter_for_player(state, frozen.representative)
                    == frozen.authorized_submitter
        })
}

/// Performs exactly one actual priority cycle on a proof clone. Every seeded
/// pass is recorded in application order so replay can submit the same normal
/// `PassPriority` actions without a hidden batch-only transition.
fn materialize_one_consented_resolution(
    state: &mut GameState,
    run: &ResolveAllConsentRun,
) -> Option<(
    crate::types::game_state::ActionResult,
    Vec<(PlayerId, GameAction)>,
)> {
    let WaitingFor::Priority { player } = &state.waiting_for else {
        return None;
    };
    let player = *player;
    if !consent_authorization_matches(state, run) {
        return None;
    }
    let actor = turn_control::authorized_submitter_for_player(state, player);
    let mut recorded = Vec::new();
    seed_remaining_consented_priority_passes(state, player, &mut recorded)?;
    let boundary = apply_action_boundary_with_stack_limit(
        state,
        actor,
        player,
        GameAction::PassPriority,
        PublicFinalizeMode::DeferredDisplay,
        Some(1),
    )
    .ok()?;
    recorded.insert(0, (actor, GameAction::PassPriority));
    Some((boundary, recorded))
}

fn seed_remaining_consented_priority_passes(
    state: &mut GameState,
    current_seat: PlayerId,
    recorded: &mut Vec<(PlayerId, GameAction)>,
) -> Option<()> {
    let current_rep = topology::priority_pass_representative(state, current_seat);
    let participants = topology::priority_pass_participants(state);
    let current_idx = participants.iter().position(|seat| *seat == current_rep)?;
    for offset in 1..participants.len() {
        let representative = participants[(current_idx + offset) % participants.len()];
        if !state.priority_passes.contains(&representative) {
            let actor = turn_control::authorized_submitter_for_player(state, representative);
            state.priority_passes.insert(representative);
            recorded.push((actor, GameAction::PassPriority));
        }
    }
    Some(())
}

enum PriorityCycleFastForward {
    Seeded,
    CannotSeed,
    Stop,
}

pub fn resolve_all_fast_forward<F>(
    state: &mut GameState,
    requester: PlayerId,
    max_resolutions: u32,
    mut choose_non_requester_action: F,
) -> ResolveAllFastForwardResult
where
    F: FnMut(&GameState, PlayerId) -> ResolveAllCallbackDecision,
{
    let total = state.stack.len();
    let resolution_cap = if max_resolutions == 0 {
        u32::MAX
    } else {
        max_resolutions
    };
    // CR 117.4: fast-forwarding priority is only a shortcut over repeated
    // passes. The guard is not progress accounting; StackResolved events are.
    let max_iterations = total
        .saturating_mul(state.players.len())
        .saturating_mul(4)
        .clamp(100, 20_000);

    let mut events = Vec::new();
    let mut log_entries = Vec::new();
    let mut items_resolved = 0u32;
    let mut deferred_display_pending = false;
    let mut recorded_actions: Vec<(PlayerId, GameAction)> = Vec::new();

    for _ in 0..max_iterations {
        let semantic_priority_seat = match &state.waiting_for {
            WaitingFor::Priority { player } => *player,
            WaitingFor::GameOver { .. } => break,
            _ => break,
        };

        if state.stack.is_empty() || state.stack.len() > total {
            break;
        }

        let actor = turn_control::authorized_submitter_for_player(state, semantic_priority_seat);
        let (action, semantic_owner, mode, stop_after_boundary) = if actor == requester {
            (
                GameAction::PassPriority,
                semantic_priority_seat,
                PublicFinalizeMode::DeferredDisplay,
                false,
            )
        } else {
            if deferred_display_pending {
                finalize_display_state(state);
                deferred_display_pending = false;
            }
            match choose_non_requester_action(state, actor) {
                ResolveAllCallbackDecision::Action(GameAction::PassPriority) => (
                    GameAction::PassPriority,
                    semantic_priority_seat,
                    PublicFinalizeMode::DeferredDisplay,
                    false,
                ),
                ResolveAllCallbackDecision::Proposal { contract, action }
                    if contract.permits(state, actor, &action) =>
                {
                    (
                        action,
                        contract.semantic_owner,
                        PublicFinalizeMode::Immediate,
                        true,
                    )
                }
                // Raw non-pass values and stale/nonmember proposals must stop
                // the batch rather than escaping Resolve All's action-boundary
                // contract.
                ResolveAllCallbackDecision::Action(_)
                | ResolveAllCallbackDecision::Proposal { .. } => break,
                ResolveAllCallbackDecision::Stop => break,
            }
        };

        let mut seeded_actions: Vec<(PlayerId, GameAction)> = Vec::new();
        if matches!(action, GameAction::PassPriority) && !state.stack.is_empty() {
            match seed_remaining_priority_cycle_passes(
                state,
                semantic_priority_seat,
                requester,
                &mut choose_non_requester_action,
                &mut seeded_actions,
            ) {
                PriorityCycleFastForward::Seeded | PriorityCycleFastForward::CannotSeed => {}
                PriorityCycleFastForward::Stop => break,
            }
        }

        let remaining_resolution_cap = resolution_cap.saturating_sub(items_resolved).max(1);
        let stack_resolution_limit =
            matches!(action, GameAction::PassPriority).then_some(remaining_resolution_cap);
        let action_for_record = action.clone();
        let Ok(boundary) = apply_action_boundary_with_stack_limit(
            state,
            actor,
            semantic_owner,
            action,
            mode,
            stack_resolution_limit,
        ) else {
            break;
        };
        // `actor` holds priority right now (per `WaitingFor::Priority`), so a
        // legal replay must submit its action before any of the seeded
        // passes below — those represent *later* seats in the priority
        // rotation. Seeding mutates `state.priority_passes` directly ahead
        // of this `apply` call (see `seed_remaining_priority_cycle_passes`)
        // so the engine's own full-cycle-resolved check fires correctly,
        // but that internal mutation order must not leak into the recorded
        // order: `apply` rejects an action from any actor other than the
        // current `WaitingFor` seat, so recording the seeded entries first
        // would make the exported replay un-submittable from the original
        // state.
        recorded_actions.push((actor, action_for_record));
        recorded_actions.extend(seeded_actions);

        if matches!(mode, PublicFinalizeMode::DeferredDisplay) {
            deferred_display_pending = true;
        }

        let resolved_this_boundary = stack_resolved_count(&boundary.events);
        let halted = has_resolution_halted(&boundary.events);
        events.extend(boundary.events);
        log_entries.extend(boundary.log_entries);

        if resolved_this_boundary > 0 {
            items_resolved = items_resolved.saturating_add(resolved_this_boundary);
            if items_resolved >= resolution_cap {
                break;
            }
        }
        if halted || stop_after_boundary {
            break;
        }
    }

    if deferred_display_pending {
        finalize_display_state(state);
    }

    ResolveAllFastForwardResult {
        events,
        waiting_for: state.waiting_for.clone(),
        log_entries,
        items_resolved,
        total: total as u32,
        recorded_actions,
    }
}

fn seed_remaining_priority_cycle_passes<F>(
    state: &mut GameState,
    current_seat: PlayerId,
    requester: PlayerId,
    choose_non_requester_action: &mut F,
    seeded_actions: &mut Vec<(PlayerId, GameAction)>,
) -> PriorityCycleFastForward
where
    F: FnMut(&GameState, PlayerId) -> ResolveAllCallbackDecision,
{
    let current_rep = topology::priority_pass_representative(state, current_seat);
    let participants = topology::priority_pass_participants(state);
    let Some(current_idx) = participants.iter().position(|&seat| seat == current_rep) else {
        return PriorityCycleFastForward::CannotSeed;
    };
    let mut seeded = Vec::new();

    for offset in 1..participants.len() {
        let seat = participants[(current_idx + offset) % participants.len()];
        let representative = topology::priority_pass_representative(state, seat);

        if !state.priority_passes.contains(&representative) {
            let actor = turn_control::authorized_submitter_for_player(state, representative);
            if actor != requester {
                match choose_non_requester_action(state, actor) {
                    ResolveAllCallbackDecision::Action(GameAction::PassPriority) => {}
                    ResolveAllCallbackDecision::Action(_)
                    | ResolveAllCallbackDecision::Proposal { .. } => {
                        return PriorityCycleFastForward::CannotSeed;
                    }
                    ResolveAllCallbackDecision::Stop => return PriorityCycleFastForward::Stop,
                }
            }
            seeded.push((representative, actor));
        }
    }

    // These representatives never went through `apply` — they're the
    // documented fast-forward shortcut over an explicit `PassPriority` each
    // (see the module doc comment). Recorded as if they had been, so replay
    // reconstruction (which only knows how to replay via `apply`) reproduces
    // the same end state. Appended to a caller-local scratch buffer, not
    // directly to the batch's `recorded_actions` — the caller must record
    // `current_seat`'s own pass *before* these (it holds priority right
    // now), even though the state mutation below necessarily happens before
    // `current_seat`'s actual `apply` call. See the call site.
    for (seat, actor) in seeded {
        state.priority_passes.insert(seat);
        seeded_actions.push((actor, GameAction::PassPriority));
    }

    PriorityCycleFastForward::Seeded
}

fn stack_resolved_count(events: &[GameEvent]) -> u32 {
    events
        .iter()
        .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
        .count() as u32
}

fn has_resolution_halted(events: &[GameEvent]) -> bool {
    events
        .iter()
        .any(|event| matches!(event, GameEvent::ResolutionHalted { .. }))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, CopyRetargetPermission, Effect,
        ManaContribution, ManaProduction, ResolvedAbility, TargetFilter,
    };
    use crate::types::actions::ResolveAllConsentDecision;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{AutoPassMode, PublicStateDirty, StackEntry, StackEntryKind};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaColor;
    use crate::types::phase::{Phase, PhaseStop, PhaseStopScope};
    use crate::types::zones::Zone;

    use super::super::public_state::{finalize_public_state, mark_public_state_all_dirty};
    use super::*;

    fn no_op_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: Box::new(ResolvedAbility::new(
                    Effect::NoOp,
                    vec![],
                    object_id,
                    controller,
                )),
            },
        }
    }

    fn self_copy_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: Box::new(ResolvedAbility::new(
                    Effect::CopySpell {
                        target: TargetFilter::SelfRef,
                        retarget: CopyRetargetPermission::KeepOriginalTargets,
                        copier: None,
                        additional_modifications: Vec::new(),
                        starting_loyalty_from_casualty_sacrifice: false,
                    },
                    vec![],
                    object_id,
                    controller,
                )),
            },
        }
    }

    fn priority_state(semantic_seat: PlayerId, stack: Vec<StackEntry>) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: semantic_seat,
        };
        state.priority_player = semantic_seat;
        state.stack = stack.into_iter().collect();
        state
    }

    fn two_hg_priority_state(semantic_seat: PlayerId, stack: Vec<StackEntry>) -> GameState {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 7);
        state.active_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: semantic_seat,
        };
        state.priority_player = semantic_seat;
        state.stack = stack.into_iter().collect();
        state
    }

    fn stop_callback(_: &GameState, _: PlayerId) -> ResolveAllCallbackDecision {
        ResolveAllCallbackDecision::Stop
    }

    fn make_mana_land(state: &mut GameState) -> ObjectId {
        let land_id = create_object(
            state,
            CardId(2),
            PlayerId(0),
            "Gemstone Mine".to_string(),
            Zone::Battlefield,
        );
        let land = state.objects.get_mut(&land_id).unwrap();
        land.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        };
        land.card_types = land.base_card_types.clone();
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap);
        land.abilities = std::sync::Arc::new(vec![ability]);
        land_id
    }

    #[test]
    fn counts_net_flat_stack_resolution() {
        let mut state = priority_state(PlayerId(0), vec![self_copy_entry(1, PlayerId(0))]);
        state.priority_passes.insert(PlayerId(1));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 1, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert_eq!(result.total, 1);
        assert_eq!(state.stack.len(), 1);
        assert!(
            result.events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::StackResolved {
                        object_id: ObjectId(1)
                    }
                )
            }),
            "net-flat resolution must count StackResolved even when stack depth remains unchanged"
        );
    }

    #[test]
    fn requester_last_pass_resolves_top_stack_entry() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        state.priority_passes.insert(PlayerId(1));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn all_pass_cycle_resolves_without_intermediate_priority_events() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, _| {
            calls.set(calls.get() + 1);
            ResolveAllCallbackDecision::Action(GameAction::PassPriority)
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
        assert!(
            !result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::PriorityPassed { .. })),
            "Resolve All seeds accepted priority passes instead of emitting every intermediate pass"
        );
        // Both the requester's own pass (which does go through `apply`) and
        // the fast-forward-seeded pass (PlayerId(1), bypassing `apply`
        // entirely — see `seed_remaining_priority_cycle_passes`) must be
        // captured so an exported replay of a Resolve-All-driven game
        // doesn't silently omit real state transitions. The requester must
        // be recorded *first*: it holds priority in the original state, and
        // a replay reconstructing from that state can only legally submit
        // PlayerId(1)'s pass after PlayerId(0)'s — `apply` rejects an
        // action from any actor that isn't the current `WaitingFor` seat.
        assert_eq!(
            result.recorded_actions,
            vec![
                (PlayerId(0), GameAction::PassPriority),
                (PlayerId(1), GameAction::PassPriority),
            ],
            "every action applied (or fast-forward-equivalent pass) during the batch must be \
             recorded in an order a fresh replay reconstruction can legally submit through apply"
        );
    }

    #[test]
    fn two_hg_resolve_all_seeds_only_opposing_team_representative() {
        let mut state = two_hg_priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, actor| {
            calls.set(calls.get() + 1);
            assert_eq!(
                actor,
                PlayerId(2),
                "callback should be for the opposing team representative, not active teammate"
            );
            ResolveAllCallbackDecision::Action(GameAction::PassPriority)
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
        assert!(
            !result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::PriorityPassed { .. })),
            "Resolve All should seed the opposing team pass instead of prompting the active teammate"
        );
    }

    #[test]
    fn raw_future_non_pass_callback_prevents_priority_cycle_seeding_without_applying() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, _| {
            calls.set(calls.get() + 1);
            ResolveAllCallbackDecision::Action(GameAction::SetPhaseStops {
                stops: vec![PhaseStop {
                    phase: Phase::PreCombatMain,
                    scope: PhaseStopScope::AllTurns,
                }],
            })
        });

        assert_eq!(calls.get(), 2);
        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(!state.phase_stops.contains_key(&PlayerId(1)));
    }

    #[test]
    fn soft_cap_stops_after_counted_stack_resolution() {
        let mut state = priority_state(
            PlayerId(0),
            vec![no_op_entry(1, PlayerId(0)), no_op_entry(2, PlayerId(0))],
        );
        state.priority_passes.insert(PlayerId(1));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 1, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn routes_controlled_turn_priority_to_authorized_requester() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.priority_player = PlayerId(0);
        state.priority_passes.insert(PlayerId(0));

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn stops_when_callback_stops_for_non_requester() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(result.events.is_empty());
        assert_eq!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        );
    }

    #[test]
    fn current_contract_proposal_is_applied_for_non_requester_priority() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |state, actor| {
            let contract = AiDecisionContract::issue(state, PlayerId(1));
            assert_eq!(actor, contract.authorized_actor);
            ResolveAllCallbackDecision::Proposal {
                contract,
                action: GameAction::PassPriority,
            }
        });

        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn raw_non_pass_callback_action_is_rejected_without_applying() {
        let mut state = priority_state(PlayerId(1), vec![no_op_entry(1, PlayerId(1))]);
        let calls = Cell::new(0);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |_, _| {
            calls.set(calls.get() + 1);
            ResolveAllCallbackDecision::Action(GameAction::SetPhaseStops {
                stops: vec![PhaseStop {
                    phase: Phase::PreCombatMain,
                    scope: PhaseStopScope::AllTurns,
                }],
            })
        });

        assert_eq!(calls.get(), 1);
        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(!state.phase_stops.contains_key(&PlayerId(1)));
    }

    #[test]
    fn callback_sees_display_finalized_after_deferred_boundary() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        state.active_player = PlayerId(1);
        state.priority_passes.insert(PlayerId(1));
        let land_id = make_mana_land(&mut state);
        mark_public_state_all_dirty(&mut state);
        finalize_public_state(&mut state);
        assert!(state.objects[&land_id].has_mana_ability);
        state.objects.get_mut(&land_id).unwrap().tapped = true;
        mark_public_state_all_dirty(&mut state);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, |callback_state, _| {
            assert_eq!(
                callback_state.public_state_dirty,
                PublicStateDirty::default()
            );
            assert!(!callback_state.objects[&land_id].has_mana_ability);
            ResolveAllCallbackDecision::Stop
        });

        assert_eq!(result.items_resolved, 1);
        assert!(!state.objects[&land_id].has_mana_ability);
    }

    #[test]
    fn final_deferred_boundary_flushes_display_before_return() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        state.priority_passes.insert(PlayerId(1));
        let land_id = make_mana_land(&mut state);
        mark_public_state_all_dirty(&mut state);
        finalize_public_state(&mut state);
        assert!(state.objects[&land_id].has_mana_ability);
        state.objects.get_mut(&land_id).unwrap().tapped = true;
        mark_public_state_all_dirty(&mut state);

        let result = resolve_all_fast_forward(&mut state, PlayerId(0), 0, stop_callback);

        assert_eq!(result.items_resolved, 1);
        assert_eq!(state.public_state_dirty, PublicStateDirty::default());
        assert!(!state.objects[&land_id].has_mana_ability);
    }

    fn ready_state(stack: Vec<StackEntry>) -> GameState {
        ready_state_with_active_player(PlayerId(0), stack)
    }

    fn ready_state_with_active_player(
        active_player: PlayerId,
        stack: Vec<StackEntry>,
    ) -> GameState {
        let mut state = priority_state(PlayerId(0), stack);
        state.active_player = active_player;
        super::super::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::BeginResolveAll { max_resolutions: 0 },
        )
        .expect("priority holder begins the consent run");
        let epoch = match &state.waiting_for {
            WaitingFor::ResolveAllConsent { epoch, .. } => *epoch,
            _ => panic!("second representative should be queued"),
        };
        super::super::engine::apply(
            &mut state,
            PlayerId(1),
            GameAction::RespondResolveAllConsent {
                epoch,
                decision: ResolveAllConsentDecision::Grant,
            },
        )
        .expect("second representative grants");
        assert!(matches!(
            &state.waiting_for,
            WaitingFor::ResolveAllReady { .. }
        ));
        state
    }

    #[test]
    fn ready_consent_uses_the_priority_holder_first_when_active_player_has_passed() {
        let mut state =
            ready_state_with_active_player(PlayerId(1), vec![no_op_entry(1, PlayerId(0))]);

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 1);
        assert!(state.stack.is_empty());
    }

    #[test]
    fn ready_consent_commits_the_greatest_settled_prefix_and_records_passes() {
        // The lower self-copy creates a new stack object. It is deliberately
        // left for ordinary priority, while both safe entries above it commit.
        let mut state = ready_state(vec![
            self_copy_entry(1, PlayerId(0)),
            no_op_entry(2, PlayerId(0)),
            no_op_entry(3, PlayerId(0)),
        ]);
        let run = ready_consent_run(&state, PlayerId(0))
            .expect("the initiating representative remains authorized at Ready")
            .clone();
        let mut proof = state.clone();
        proof.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let (boundary, _) = materialize_one_consented_resolution(&mut proof, &run)
            .expect("a full consent run materializes one ordinary priority cycle");
        assert_eq!(stack_resolved_count(&boundary.events), 1);
        assert_eq!(proof.stack.len(), 2);
        assert!(matches!(proof.waiting_for, WaitingFor::Priority { .. }));
        assert!(stack::priority_checkpoint_is_settled(&proof));
        assert!(consent_authorization_matches(&proof, &run));

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(
            result.items_resolved,
            2,
            "safe-prefix proof unexpectedly stopped: result={result:?}, waiting={:?}, stack_len={}",
            state.waiting_for,
            state.stack.len(),
        );
        assert_eq!(state.stack.len(), 1, "unsafe item remains on the stack");
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
        assert_eq!(
            result.recorded_actions,
            vec![
                (PlayerId(0), GameAction::PassPriority),
                (PlayerId(1), GameAction::PassPriority),
                (PlayerId(0), GameAction::PassPriority),
                (PlayerId(1), GameAction::PassPriority),
            ],
            "the collapsed prefix remains reproducible through ordinary actions"
        );
    }

    #[test]
    fn ready_consent_honors_its_saved_resolution_cap() {
        let mut state = ready_state(vec![
            no_op_entry(1, PlayerId(0)),
            no_op_entry(2, PlayerId(0)),
        ]);
        state
            .resolve_all_consent_run
            .as_mut()
            .expect("Ready retains its frozen run")
            .max_resolutions = 1;

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 1);
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
    }

    #[test]
    fn changed_controller_invalidates_ready_consent_without_resolving() {
        let mut state = ready_state(vec![no_op_entry(1, PlayerId(0))]);
        state.turn_decision_controller = Some(PlayerId(1));
        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
    }

    #[test]
    fn ready_consent_refuses_to_collapse_while_an_auto_pass_preference_is_active() {
        let mut state = ready_state(vec![no_op_entry(1, PlayerId(0))]);
        state.auto_pass.insert(
            PlayerId(0),
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: state.stack.len(),
            },
        );

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
    }
}
