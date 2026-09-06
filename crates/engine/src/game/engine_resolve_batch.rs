use serde::{Deserialize, Serialize};

use crate::ai_support::AiDecisionContract;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, AutoPassMode, GameState, ResolveAllConsentRun, StackResolutionPolicy, WaitingFor,
};
use crate::types::log::GameLogEntry;
use crate::types::player::PlayerId;

use super::engine::{
    apply_action_boundary_with_stack_limit, install_stack_resolution_session,
    resume_stack_resolution_session_runner, take_and_restore_stack_resolution_session,
    PublicFinalizeMode,
};
use super::public_state::{bump_state_revision, finalize_display_state};
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
    /// Every action applied during the legacy callback batch (including priority
    /// passes fast-forwarded by `seed_remaining_priority_cycle_passes`, which
    /// are semantically equivalent to — but bypass — an explicit
    /// `PassPriority` through `apply`), in submission order. `#[serde(skip)]`:
    /// this never reaches the JS-visible result shape. Ready-consumer Resolve
    /// All is replayed atomically by the transport-owned replay boundary rather
    /// than by appending these internal actions individually.
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

/// The restore-time classification of persisted stack automation.
///
/// This is intentionally a read-only classifier. A generic saved-game decode
/// may inspect it to decide whether an explicit engine resume is useful, but
/// it must never resolve a stack entry merely by reconstructing a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestoredStackAutomation {
    None,
    ActiveSession,
    LegacyResolveAllReady,
    Repair,
}

/// Outcome of an explicit restored-stack automation resume.
///
/// `Progressed` carries the ordinary engine boundary result; repairs are
/// deliberately observable as a separate zero-resolution outcome so a caller
/// cannot mistake a stale authorization repair for successful fast-forwarding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "camelCase")]
pub enum RestoredStackAutomationResult {
    Noop(ActionResult),
    Progressed(ActionResult),
    ZeroResolutionRepair(ActionResult),
}

impl RestoredStackAutomationResult {
    pub fn action_result(&self) -> &ActionResult {
        match self {
            Self::Noop(result) | Self::Progressed(result) | Self::ZeroResolutionRepair(result) => {
                result
            }
        }
    }
}

/// The maximum number of engine-authored log entries included in a restored
/// automation transport presentation.
///
/// The full [`ActionResult`] remains available to the engine, but one resumed
/// stack session can legitimately emit thousands of lifecycle events.  A
/// transport boundary needs a bounded tail so it can deliver the final state
/// rather than rejecting the whole update as oversized.
pub const MAX_RESTORED_STACK_AUTOMATION_LOG_ENTRIES: usize = 128;

/// The transport-visible kind of a completed restored stack automation.
///
/// This intentionally carries no rules payload. The completed game state is
/// the authority for the next interaction; the presentation only explains why
/// one update represents an automated burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RestoredStackAutomationOutcome {
    Noop,
    Progressed,
    ZeroResolutionRepair,
}

/// Bounded, engine-authored presentation for one restored stack automation
/// transition.
///
/// `omitted_event_count` counts the complete internal event slice deliberately
/// withheld from the transport form. `automated_resolution_count` is derived
/// from its exact `StackResolved` events only for a progressed run; a no-op or
/// repair is never presented as a resolution even if a malformed internal
/// result were to carry unrelated events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoredStackAutomationPresentation {
    pub outcome: RestoredStackAutomationOutcome,
    pub automated_resolution_count: u32,
    pub omitted_event_count: u32,
    pub log_entries: Vec<GameLogEntry>,
}

/// A completed restored stack automation transition and its bounded transport
/// presentation.
///
/// The complete result is retained for engine-owned lifecycle work and tests,
/// but skipped by serde so consumers cannot accidentally put an unbounded
/// event burst on the wire. The presentation is derived exactly once, after
/// the ordinary session runner has completed.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoredStackAutomationResume {
    #[serde(skip)]
    result: RestoredStackAutomationResult,
    pub presentation: RestoredStackAutomationPresentation,
}

impl RestoredStackAutomationResume {
    pub(crate) fn from_completed(result: RestoredStackAutomationResult) -> Self {
        let (outcome, automated_resolution_count) = match &result {
            RestoredStackAutomationResult::Noop(_) => (RestoredStackAutomationOutcome::Noop, 0),
            RestoredStackAutomationResult::Progressed(action_result) => (
                RestoredStackAutomationOutcome::Progressed,
                action_result
                    .events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
                    .count()
                    .try_into()
                    .unwrap_or(u32::MAX),
            ),
            RestoredStackAutomationResult::ZeroResolutionRepair(_) => {
                (RestoredStackAutomationOutcome::ZeroResolutionRepair, 0)
            }
        };
        let action_result = result.action_result();
        let omitted_event_count = action_result.events.len().try_into().unwrap_or(u32::MAX);
        let log_entries = bounded_restored_stack_automation_log_tail(&action_result.log_entries);

        Self {
            result,
            presentation: RestoredStackAutomationPresentation {
                outcome,
                automated_resolution_count,
                omitted_event_count,
                log_entries,
            },
        }
    }

    /// The complete, engine-internal action result for this transition.
    pub fn action_result(&self) -> &ActionResult {
        self.result.action_result()
    }
}

fn bounded_restored_stack_automation_log_tail(entries: &[GameLogEntry]) -> Vec<GameLogEntry> {
    let keep_from = entries
        .len()
        .saturating_sub(MAX_RESTORED_STACK_AUTOMATION_LOG_ENTRIES);
    entries[keep_from..].to_vec()
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
    resolve_all_ready_prefix_with(state, requester, ResolveAllContinuation::AutoPassRemainder)
}

/// What to do with the remainder of the stack when the prefix proof stops
/// short.
///
/// The two answers belong to different situations, not to a preference. A live
/// session honors the requester's durable intent; a session being restored has
/// nobody connected to observe an unattended run-out, so it hands an
/// actionable state back and stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveAllContinuation {
    /// Resume the requester's retained auto-pass, or install the ordinary
    /// `UntilStackEmpty` fallback when they have no retained preference, so
    /// a proof that could not collapse the whole batch does not require manual
    /// priority passes. The live-session answer.
    AutoPassRemainder,
    /// Stop at ordinary priority and install nothing.
    ///
    /// Used at restore. The auto-pass path resolves the remaining stack through
    /// the ordinary pipeline, which can end the game — and a restore has no
    /// socket attached and no caller positioned to emit a ranked result or a
    /// terminal artifact, so a game that ended there would be registered as a
    /// live session parked in `GameOver`. Handing priority back instead loses
    /// nothing: the consent is discarded either way, and a reconnecting player
    /// can simply ask for Resolve All again.
    StopAtPriority,
}

/// [`resolve_all_ready_prefix`], with an explicit answer for the stack that the
/// proof could not collapse.
pub fn resolve_all_ready_prefix_with(
    state: &mut GameState,
    requester: PlayerId,
    continuation: ResolveAllContinuation,
) -> ResolveAllFastForwardResult {
    let total = state.stack.len() as u32;
    let mut events = Vec::new();
    let mut log_entries = Vec::new();
    let mut recorded_actions = Vec::new();
    let mut items_resolved = 0;
    let mut proof_stopped = false;

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

    let resolution_cap = run.max_resolutions.max_resolutions().unwrap_or(u32::MAX);

    while items_resolved < resolution_cap && !state.stack.is_empty() {
        let mut proof = state.clone();
        let stack_before = proof.stack.len();
        // Durable auto-pass intent remains live while Resolve All is asking
        // for consent, but the clone may materialize exactly one consented
        // resolution. Suspend the whole clone-local map: an UntilStackEmpty
        // entry could otherwise run beyond this proof's one-entry limit.
        let suspended_auto_passes = std::mem::take(&mut proof.auto_pass);
        let materialized = materialize_one_consented_resolution(&mut proof, &run);
        proof.auto_pass = suspended_auto_passes;
        let Some((boundary, mut actions)) = materialized else {
            proof_stopped = true;
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
            proof_stopped = true;
            break;
        }

        items_resolved += 1;
        events.extend(boundary.events);
        log_entries.extend(boundary.log_entries);
        recorded_actions.append(&mut actions);
        *state = proof;
    }

    // Authorization is one run only. Once the proved prefix ends, no later
    // stack entry inherits this consent. A proof failure is different from a
    // requested cap: it only rejects collapsing this sequence, not the
    // requester's durable intent to avoid manual priority passes.
    turn_control::invalidate_resolve_all_consent(state);
    if continuation == ResolveAllContinuation::AutoPassRemainder {
        if proof_stopped
            && !state
                .auto_pass
                .contains_key(&run.priority_snapshot.waiting_player)
        {
            // Keep the previous fallback for a requester without a retained
            // turn-boundary preference. The ordinary loop below also preserves
            // its pre-cast guard: it leaves this request armed until an allowed
            // action can resume it.
            super::engine::install_until_stack_empty_auto_pass(
                state,
                run.priority_snapshot.waiting_player,
            );
        }
        let mut resumed = ResolveAllFastForwardResult {
            events,
            waiting_for: state.waiting_for.clone(),
            log_entries,
            items_resolved,
            total,
            recorded_actions,
        };
        super::engine::resume_auto_pass_after_resolve_all(state, &mut resumed);
        events = resumed.events;
        log_entries = resumed.log_entries;
        items_resolved = resumed.items_resolved;
        recorded_actions = resumed.recorded_actions;
    }
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

/// Whether `requester` may hand the current `WaitingFor::ResolveAllReady`
/// latch to [`resolve_all_ready_prefix`].
///
/// Deliberately one axis — entitlement — and not two. Whether the frozen run
/// is still *coherent* with the live game is a second question, and it is
/// answered inside [`resolve_all_ready_prefix`], which re-derives it and
/// either collapses the consented prefix or repairs the latch back to
/// priority. Returning that answer here as well would compute a decision at
/// the transport, hand it to a caller with no use for it, and then recompute
/// it in the callee — one decision spread across a layer boundary.
///
/// Callers that want to know which of the two happened should read
/// [`ResolveAllFastForwardResult::items_resolved`], which the resolver already
/// reports, rather than a pre-flight guess that may be stale by the time the
/// resolver runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveAllReadyAccess {
    /// The resolver may run. Either the requester is one of the run's frozen
    /// submitters, or no run bears this epoch at all — in which case there is
    /// no submitter list left to check anyone against, and the resolver's
    /// repair is the latch's only exit.
    ///
    /// Admitting the incoherent case is deliberate. The repair resolves
    /// nothing, so it confers no advantage, and refusing every seat would make
    /// an unadvanceable state permanent. It is still a mutation — it clears the
    /// recorded priority passes — so a seat that did not start the run can
    /// restart a pass cycle. That is the price of having any exit at all, and
    /// the caller is token-authenticated at the transport regardless.
    Admitted,
    /// Not a Ready latch, or a run bearing this epoch exists and this requester
    /// is not among its frozen submitters. Transports must reject without
    /// mutating the session.
    Refused,
}

/// Single authority on whether `requester` may touch the current Ready latch.
///
/// A changed controller, eliminated player, or drifted priority cursor does
/// NOT refuse: those are the engine's own bookkeeping going out of date rather
/// than an unentitled caller, and the resolver's repair is the only exit from
/// the latch.
pub fn resolve_all_ready_access(state: &GameState, requester: PlayerId) -> ResolveAllReadyAccess {
    let WaitingFor::ResolveAllReady { epoch } = &state.waiting_for else {
        return ResolveAllReadyAccess::Refused;
    };
    // Without a run bearing this epoch there is no frozen submitter list, so no
    // seat can prove ownership. Refusing every seat would brick the game.
    let Some(run) = state
        .resolve_all_consent_run
        .as_ref()
        .filter(|run| run.epoch == *epoch)
    else {
        return ResolveAllReadyAccess::Admitted;
    };
    if run
        .participants
        .iter()
        .any(|participant| participant.authorized_submitter == requester)
    {
        ResolveAllReadyAccess::Admitted
    } else {
        ResolveAllReadyAccess::Refused
    }
}

/// The frozen submitter who started a pending, consumable Ready latch.
///
/// `begin_resolve_all_consent` rotates the participant list so its first entry
/// is the initiating representative, making `participants[0]` the run's own
/// record of who asked for the batch. Callers that must drive the latch — the
/// AI-seat hand-off on both the local and the server transport — read the
/// requester from here instead of synthesizing one, so authorization stays
/// frozen at proposal time exactly as `ResolveAllConsentRun` intends.
///
/// INITIATOR, NOT OWNER — the distinction has already cost one wrong test
/// assertion. This names whoever called `BeginResolveAll`, which is frequently
/// NOT the seat whose Grant completed the run. Ownership of a latch follows the
/// FINAL Grant, because that submitter's client is the thing that will send
/// `ResolveAll` for it, so this value must never be used to decide whose latch
/// it is. `GameSession::run_ai` derives that from its own applied batch instead.
///
/// Why passing the initiator to [`resolve_all_ready_prefix_with`] is
/// nonetheless right: there `requester` is a CREDENTIAL and nothing else,
/// consumed once by `ready_consent_run` as a membership test over the frozen
/// submitters, which the initiator always satisfies. Every seat-bearing
/// decision downstream reads the run instead — the Priority restore and the
/// proof-stopped auto-pass both use `run.priority_snapshot.waiting_player`, and
/// the proof loop seeds passes for all participants. So which participant is
/// handed in cannot change the outcome.
///
/// That is NOT true everywhere. In [`resolve_all_fast_forward`] the requester
/// is behaviour-bearing: `actor == requester` auto-passes silently while other
/// seats are routed through the caller's callback. Neither latch consumer goes
/// through that path today; one that did would have to decide owner-vs-initiator
/// on purpose rather than inherit this function's answer.
pub fn pending_resolve_all_ready_requester(state: &GameState) -> Option<PlayerId> {
    if !matches!(state.waiting_for, WaitingFor::ResolveAllReady { .. }) {
        return None;
    }
    let requester = state
        .resolve_all_consent_run
        .as_ref()?
        .participants
        .first()?
        .authorized_submitter;
    ready_consent_run(state, requester)
        .is_some()
        .then_some(requester)
}

/// Classifies persisted stack automation without advancing game state.
///
/// A generic saved-game decode is a state reconstruction boundary, not an
/// implicit priority pass. Call [`resume_restored_stack_automation`] explicitly
/// when the owner is ready to drive a restored stack automation session.
pub fn classify_restored_stack_automation(state: &GameState) -> RestoredStackAutomation {
    if let Some(session) = state.stack_resolution_session.as_ref() {
        return if restored_stack_resolution_session_is_coherent(state, session) {
            RestoredStackAutomation::ActiveSession
        } else {
            RestoredStackAutomation::Repair
        };
    }
    if matches!(state.waiting_for, WaitingFor::ResolveAllReady { .. }) {
        return if restored_legacy_ready_is_coherent(state) {
            RestoredStackAutomation::LegacyResolveAllReady
        } else {
            RestoredStackAutomation::Repair
        };
    }
    // A pending consent prompt is a live decision, not automation to resume, so
    // a coherent one classifies as `None` like any ordinary prompt. An
    // incoherent one is the third unanswerable saved authorization, alongside a
    // stale session and a run-less Ready latch: its representative can neither
    // Grant nor Decline, and `WaitingFor::ResolveAllConsent` has no consumer
    // entry point of its own that could repair it later. `repair_restored_stack_automation`
    // already routes a consent wait to `rebase_invalid_resolve_all_consent`;
    // only this classification was missing.
    if let WaitingFor::ResolveAllConsent {
        epoch,
        representative,
    } = &state.waiting_for
    {
        if !state
            .resolve_all_consent_run
            .as_ref()
            .is_some_and(|run| run.accepts_response_from(*epoch, *representative))
        {
            return RestoredStackAutomation::Repair;
        }
    }
    RestoredStackAutomation::None
}

/// Compatibility no-op for restore code that previously advanced Ready latches.
///
/// The old call sites remain source-compatible while their generic decode stays
/// pure. New owners inspect [`classify_restored_stack_automation`] and choose
/// whether to call [`resume_restored_stack_automation`] explicitly.
pub fn recover_orphaned_resolve_all(_: &mut GameState) -> Option<ResolveAllFastForwardResult> {
    None
}

/// Explicitly resumes coherent persisted stack automation through the ordinary
/// runner, or repairs an incoherent saved authorization without resolving a
/// stack entry.
pub fn resume_restored_stack_automation(state: &mut GameState) -> RestoredStackAutomationResume {
    let result = match classify_restored_stack_automation(state) {
        RestoredStackAutomation::None => RestoredStackAutomationResult::Noop(ActionResult {
            events: Vec::new(),
            waiting_for: state.waiting_for.clone(),
            log_entries: Vec::new(),
        }),
        RestoredStackAutomation::ActiveSession => {
            RestoredStackAutomationResult::Progressed(resume_stack_resolution_session_runner(state))
        }
        RestoredStackAutomation::LegacyResolveAllReady => {
            materialize_restored_legacy_ready_session(state);
            RestoredStackAutomationResult::Progressed(resume_stack_resolution_session_runner(state))
        }
        RestoredStackAutomation::Repair => RestoredStackAutomationResult::ZeroResolutionRepair(
            repair_restored_stack_automation(state),
        ),
    };
    RestoredStackAutomationResume::from_completed(result)
}

fn restored_stack_resolution_session_is_coherent(
    state: &GameState,
    session: &crate::types::game_state::StackResolutionSession,
) -> bool {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. })
        || state.stack.is_empty()
        || session.representatives.is_empty()
        || session.cursor >= session.entries.len()
        || session.entries.len().saturating_sub(session.cursor) != state.stack.len()
        || session
            .budget
            .max_resolutions()
            .is_some_and(|maximum| session.cursor >= maximum as usize)
        || !session
            .verified_pass_representatives
            .is_subset(&session.representatives)
        || (session.policy != StackResolutionPolicy::RecheckNoMeaningfulPriorityAction
            && !session.verified_pass_representatives.is_empty())
    {
        return false;
    }

    let canonical_representatives = topology::canonical_priority_representatives(
        state,
        session.representatives.iter().copied(),
    );
    let live_representatives = topology::priority_pass_participants(state);
    if canonical_representatives != session.representatives
        || !session
            .representatives
            .iter()
            .all(|representative| live_representatives.contains(representative))
    {
        return false;
    }

    let mut expected_auto_pass = session.auto_pass_overlay.baseline.clone();
    let overlay_mode = AutoPassMode::UntilStackEmpty {
        initial_stack_len: session.entries.len(),
        policy: session.policy,
    };
    for representative in &session.representatives {
        expected_auto_pass.insert(*representative, overlay_mode);
    }
    if state.auto_pass.len() != expected_auto_pass.len()
        || !expected_auto_pass
            .iter()
            .all(|(player, mode)| state.auto_pass.get(player) == Some(mode))
    {
        return false;
    }

    state
        .stack
        .iter()
        .rev()
        .zip(session.entries.iter().skip(session.cursor))
        .all(|(entry, fence)| fence.matches_captured_entry(entry))
}

fn restored_legacy_ready_is_coherent(state: &GameState) -> bool {
    let Some(requester) = pending_resolve_all_ready_requester(state) else {
        return false;
    };
    !state.stack.is_empty() && ready_consent_run(state, requester).is_some()
}

fn materialize_restored_legacy_ready_session(state: &mut GameState) {
    let requester = pending_resolve_all_ready_requester(state)
        .expect("the Ready classifier admitted a requester");
    let run = ready_consent_run(state, requester)
        .cloned()
        .expect("the Ready classifier admitted a coherent consent run");
    let consumed = state
        .resolve_all_consent_run
        .take()
        .expect("a coherent Ready latch retains its consent run");
    debug_assert_eq!(consumed, run);
    let representatives = run
        .participants
        .iter()
        .map(|participant| participant.representative)
        .collect();
    state.waiting_for = WaitingFor::Priority {
        player: run.priority_snapshot.waiting_player,
    };
    install_stack_resolution_session(
        state,
        representatives,
        run.max_resolutions,
        StackResolutionPolicy::Committed,
        Default::default(),
    );
}

fn repair_restored_stack_automation(state: &mut GameState) -> ActionResult {
    let restored_session = take_and_restore_stack_resolution_session(state);
    let had_consent_wait = matches!(
        state.waiting_for,
        WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. }
    );
    let legacy_ready_without_baseline =
        matches!(state.waiting_for, WaitingFor::ResolveAllReady { .. })
            && state
                .resolve_all_consent_run
                .as_ref()
                .is_none_or(|run| run.auto_pass_baseline.is_none());
    if had_consent_wait && !restored_session {
        turn_control::rebase_invalid_resolve_all_consent(state);
        // Legacy Ready requires an empty live map for coherence. Once that
        // condition is violated there is no captured baseline to restore, so
        // retaining its modes would make the first ordinary priority boundary
        // silently resolve the stack the repair deliberately left intact.
        if legacy_ready_without_baseline {
            state.auto_pass.clear();
        }
    } else {
        state.resolve_all_consent_run = None;
        restore_ordinary_priority_after_stack_automation_repair(state);
    }
    bump_state_revision(state);
    finalize_display_state(state);
    interaction::ensure_interaction_authority(state);
    ActionResult {
        events: Vec::new(),
        waiting_for: state.waiting_for.clone(),
        log_entries: Vec::new(),
    }
}

fn restore_ordinary_priority_after_stack_automation_repair(state: &mut GameState) {
    let preferred = topology::priority_pass_representative(state, state.active_player);
    let player = super::players::is_alive(state, preferred)
        .then_some(preferred)
        .or_else(|| topology::priority_pass_participants(state).first().copied())
        .unwrap_or(preferred);
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = turn_control::authorized_submitter_for_player(state, player);
    state.priority_pass_count = 0;
    state.priority_passes.clear();
}

fn ready_consent_run(state: &GameState, requester: PlayerId) -> Option<&ResolveAllConsentRun> {
    let WaitingFor::ResolveAllReady { epoch } = &state.waiting_for else {
        return None;
    };
    let run = state.resolve_all_consent_run.as_ref().filter(|run| {
        state.auto_pass.is_empty()
            && run.epoch == *epoch
            && run.auto_pass_baseline.is_none()
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
    turn_control::resolve_all_consent_authority_matches_live(state, run)
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
        ManaContribution, ManaProduction, QuantityExpr, ResolvedAbility, TargetFilter,
    };
    use crate::types::actions::{ResolveAllConsentDecision, ResolveAllScope};
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        AutoPassMode, PublicStateDirty, StackEntry, StackEntryKind, StackResolutionBudget,
        StackResolutionPolicy, TurnBoundary,
    };
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

    fn draw_entry(id: u64, controller: PlayerId) -> StackEntry {
        let object_id = ObjectId(id);
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object_id,
                ability: Box::new(ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
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
            GameAction::BeginResolveAll {
                max_resolutions: 0,
                scope: ResolveAllScope::Shared,
            },
        )
        .expect("priority holder begins the consent run");
        let epoch = match &state.waiting_for {
            WaitingFor::ResolveAllConsent { epoch, .. } => *epoch,
            _ => panic!("second representative should be queued"),
        };
        // Ready is retained solely for saved games created before the shared
        // stack-resolution session protocol. Model that historical encoding
        // explicitly instead of treating a newly granted consent run as Ready.
        state
            .resolve_all_consent_run
            .as_mut()
            .expect("the pending run must remain available")
            .auto_pass_baseline = None;
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
    fn resolve_all_consent_materializes_the_shared_session_with_its_baseline() {
        let mut state = priority_state(PlayerId(0), vec![no_op_entry(1, PlayerId(0))]);
        let retained = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        };
        state.auto_pass.insert(PlayerId(0), retained);

        super::super::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::BeginResolveAll {
                max_resolutions: 0,
                scope: ResolveAllScope::Shared,
            },
        )
        .expect("priority holder begins the consent run");
        let epoch = match &state.waiting_for {
            WaitingFor::ResolveAllConsent { epoch, .. } => *epoch,
            _ => panic!("second representative should be queued"),
        };

        let result = super::super::engine::apply(
            &mut state,
            PlayerId(1),
            GameAction::RespondResolveAllConsent {
                epoch,
                decision: ResolveAllConsentDecision::Grant,
            },
        )
        .expect("second representative grants");

        assert!(
            result
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::StackResolved { .. })),
            "the final grant drives the shared session through the ordinary runner"
        );
        assert!(state.stack.is_empty());
        assert!(state.stack_resolution_session.is_none());
        assert!(state.resolve_all_consent_run.is_none());
        assert_eq!(state.auto_pass.get(&PlayerId(0)), Some(&retained));
        assert!(!state.auto_pass.contains_key(&PlayerId(1)));
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

        let result = resolve_all_ready_prefix_with(
            &mut state,
            PlayerId(0),
            ResolveAllContinuation::StopAtPriority,
        );

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
            .max_resolutions = StackResolutionBudget::from_legacy_max_resolutions(1);

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
    fn incoherent_legacy_ready_with_turn_boundary_auto_pass_repairs_without_resolving() {
        let mut state = ready_state(vec![
            no_op_entry(1, PlayerId(0)),
            no_op_entry(2, PlayerId(0)),
        ]);
        let retained = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        };
        state.auto_pass.insert(PlayerId(0), retained);

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 2);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
        assert_eq!(state.auto_pass.get(&PlayerId(0)), Some(&retained));
    }

    #[test]
    fn incoherent_legacy_ready_with_stack_empty_auto_pass_repairs_without_resolving() {
        let mut state = ready_state(vec![
            no_op_entry(1, PlayerId(0)),
            no_op_entry(2, PlayerId(0)),
        ]);
        let retained = AutoPassMode::UntilStackEmpty {
            initial_stack_len: state.stack.len(),
            policy: StackResolutionPolicy::Committed,
        };
        state.auto_pass.insert(PlayerId(0), retained);

        let result = resolve_all_ready_prefix_with(
            &mut state,
            PlayerId(0),
            ResolveAllContinuation::StopAtPriority,
        );

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 2);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
        assert_eq!(state.auto_pass.get(&PlayerId(0)), Some(&retained));
    }

    #[test]
    fn incoherent_legacy_ready_preserves_retained_auto_pass() {
        let mut state = ready_state(vec![no_op_entry(1, PlayerId(0))]);
        let retained = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        };
        state.auto_pass.insert(PlayerId(0), retained);

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 1);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.resolve_all_consent_run.is_none());
        assert_eq!(state.auto_pass.get(&PlayerId(0)), Some(&retained));
    }

    #[test]
    fn incoherent_legacy_ready_never_auto_resolves_a_remainder() {
        let mut state = ready_state(vec![
            draw_entry(1, PlayerId(0)),
            no_op_entry(2, PlayerId(0)),
        ]);
        let retained = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        };
        state.auto_pass.insert(PlayerId(0), retained);

        let result = resolve_all_ready_prefix(&mut state, PlayerId(0));

        assert_eq!(result.items_resolved, 0);
        assert_eq!(state.stack.len(), 2);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(!state.players[0].drew_from_empty_library);
        assert!(!state.players[0].is_eliminated);
        assert_eq!(state.auto_pass.get(&PlayerId(0)), Some(&retained));
    }
}
