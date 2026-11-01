use std::collections::HashSet;

use crate::game::engine::EngineError;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter};
use crate::types::events::{GameEvent, LifeTotalReading};
use crate::types::game_state::{
    GameState, IngressLifeWindow, PendingEffectResolutionEvent, PendingEffectResolved,
    PendingLifeTotalAssignment, UnrepresentablePolicy, WaitingFor,
};
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::resolved_commands::{ResolvedPlayerEdit, ResolvedPlayerEditReplayInvariantError};

/// Why a replacement-routed life change could not complete synchronously.
///
/// Typed so phase-transition owners can distinguish the replacement-ordering
/// round trip from a substitute effect that has already begun resolving and
/// paused on its own interactive continuation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementDeferred {
    /// CR 616.1: paused on the ordering choice BEFORE anything was applied.
    /// The pipeline reports the real amount when the choice resumes.
    ReplacementChoice,
    /// CR 614.6: the root event already finished for `applied`; what paused is
    /// the substitute effect that must resolve before the original resolution
    /// may continue.
    ///
    /// Carrying the amount is load-bearing, not decorative: a caller that needs
    /// to narrate WHY the life changed (the empty-pool drain's mana burn) learns
    /// the true figure here and nowhere else. Dropping it forced that caller to
    /// park provenance and hope a later resume would hand the number back — and
    /// the resume that completes a substitute is not the one that applied the
    /// root, so the number never came.
    SubstitutionContinuation { applied: u32 },
}

/// A player-resource change (life, a player counter, or energy) whose final
/// result the engine cannot represent: the final amount does not fit an `i32`
/// delta, or the life total, per-turn gained/lost counter, counter total or
/// energy total would overflow. Nothing was mutated.
///
/// Representability is an engine limit, not a game rule: how the change is
/// then treated is chosen by the caller's [`UnrepresentablePolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifeChangeUnrepresentable {
    pub player: PlayerId,
}

impl LifeChangeUnrepresentable {
    /// The single place the compatibility skip policy logs a discarded change.
    fn log_discarded(self, site: &'static str) {
        tracing::warn!(
            player = self.player.0,
            site,
            "engine-unrepresentable player-resource change skipped by compatibility policy"
        );
    }
}

impl From<LifeChangeUnrepresentable> for EngineError {
    /// The typed carrier survives to the action boundary, which recognizes the
    /// failure and restores its snapshot. Clients see only the stable
    /// `InvalidAction` rejection code, never the diagnostic payload.
    fn from(unrepresentable: LifeChangeUnrepresentable) -> Self {
        EngineError::LifeChangeUnrepresentable {
            player: unrepresentable.player,
        }
    }
}

/// Why an action-ingress life change did not complete synchronously.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeChangeError {
    /// The replacement/substitution pipeline paused for player input.
    Deferred(ReplacementDeferred),
    /// The final result is not representable; the ingress caller rejects.
    Unrepresentable(LifeChangeUnrepresentable),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubstitutionDrainOutcome {
    Completed,
    Deferred,
}

/// Final settle step of a life event: applies the (possibly replaced) event and
/// reports the amount actually changed.
type LifeSettleFn = fn(
    &mut GameState,
    ProposedEvent,
    UnrepresentablePolicy,
    &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeUnrepresentable>;

/// Runs `body` with a fresh action-ingress window open and returns its result
/// together with the window as it stood at close. Any player-resource
/// settlement (life, player counter, or energy) inside `body` that proves unrepresentable is recorded in the window instead
/// of being skipped, so the ingress caller can reject the action. Windows nest:
/// the prior window is restored untouched on close, so an inner window reports
/// only to its own caller.
pub(crate) fn within_ingress_life_window<T>(
    state: &mut GameState,
    body: impl FnOnce(&mut GameState) -> T,
) -> (T, IngressLifeWindow) {
    let prior = state
        .ingress_life_window
        .replace(IngressLifeWindow::default());
    let result = body(state);
    let window = std::mem::replace(&mut state.ingress_life_window, prior).unwrap_or_default();
    (result, window)
}

/// The action-ingress rule for any player-resource change (life, a player
/// counter, or energy): runs `body` under `UnrepresentablePolicy::Reject`
/// inside a fresh ingress window. The body's own unrepresentable error is
/// reported first; otherwise a failure recorded in the window (a nested
/// ordinary settlement) outranks the body's result, because the whole
/// action is rejected and rolled back by the boundary. A park made inside
/// the window is stamped `Reject`, so its resume is held to the same rule.
/// An engine numeric limit, not a CR 101.3 impossibility.
pub(crate) fn within_rejecting_ingress_window<T>(
    state: &mut GameState,
    body: impl FnOnce(&mut GameState, UnrepresentablePolicy) -> Result<T, LifeChangeUnrepresentable>,
) -> Result<T, LifeChangeUnrepresentable> {
    let (result, window) =
        within_ingress_life_window(state, |state| body(state, UnrepresentablePolicy::Reject));
    match (result, window.failed_player) {
        (Err(own), _) => Err(own),
        (Ok(_), Some(player)) => Err(LifeChangeUnrepresentable { player }),
        (Ok(value), None) => Ok(value),
    }
}

/// CR 119.3: Gain life — adjust the player's life total.
pub fn resolve_gain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, player_filter) = match &ability.effect {
        Effect::GainLife { amount, player } => (amount, player),
        _ => return Err(EffectError::MissingParam("GainLife amount".to_string())),
    };

    // CR 119.3: Who gains the life. `resolve_life_loss_target` is the single
    // authority for player resolution from a TargetFilter — context-refs
    // (Controller, ParentTargetController) resolve via state slots; explicit
    // Player targets come from `ability.targets`.
    // CR 608.2b: a declared player whose target was illegal gains nothing.
    let Some(player_id) = resolve_life_loss_target(state, ability, Some(player_filter)) else {
        return Ok(());
    };

    // CR 119.7: "If an effect says that a player can't gain life ... a replacement
    // effect that would replace a life gain event affecting that player won't do
    // anything." Short-circuit BEFORE the replacement pipeline.
    if crate::game::static_abilities::player_has_cant_gain_life(state, player_id) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let final_amount = resolve_quantity_with_targets(state, amount, ability);

    if final_amount <= 0 {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    let proposed = ProposedEvent::LifeGain {
        player_id,
        amount: final_amount as u32,
        applied: HashSet::new(),
    };

    // CR 614.1a: Route life gain through replacement pipeline.
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_life_gain_after_replacement(state, event, events);
        }
        ReplacementResult::Prevented => {
            // CR 614.1a + CR 614.6 — Issue #317: Cross-event-type
            // substitution ("If you would gain life, draw that many cards
            // instead" — Lich). The applier returned `Prevented` and stashed
            // the substitute effect (e.g., Draw) as a post-replacement
            // continuation. Drain it now so the replacement actually
            // runs in the same resolution step.
            if matches!(
                drain_substitution_continuation(state, events),
                SubstitutionDrainOutcome::Deferred
            ) {
                return Ok(());
            }
        }
        ReplacementResult::NeedsChoice(player) => {
            // CR 616.1: two or more applicable life-gain replacements — the affected
            // player chooses which applies first. Unlike every other arm here, this
            // one returns WITHOUT pushing `EffectResolved`: the prompt owns the rest
            // of the resolution.
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// The proposed `LifeGain` event for an entry, or `None` when the change is
/// vacuous or suppressed before the replacement pipeline.
fn proposed_life_gain(
    state: &GameState,
    player_id: PlayerId,
    amount: u32,
) -> Option<ProposedEvent> {
    if amount == 0 {
        return None;
    }
    // CR 119.7: Short-circuit BEFORE the replacement pipeline — "can't gain life"
    // suppresses the life gain event entirely (and any replacements that would
    // otherwise modify it).
    if crate::game::static_abilities::player_has_cant_gain_life(state, player_id) {
        return None;
    }
    Some(ProposedEvent::LifeGain {
        player_id,
        amount,
        applied: HashSet::new(),
    })
}

/// The proposed `LifeLoss` event for an entry, or `None` when the change is
/// vacuous or suppressed before the replacement pipeline.
fn proposed_life_loss(
    state: &GameState,
    player_id: PlayerId,
    amount: u32,
) -> Option<ProposedEvent> {
    if amount == 0 {
        return None;
    }
    // CR 119.8: A player who can't lose life is unaffected. Short-circuit
    // before the replacement pipeline.
    if crate::game::static_abilities::player_has_cant_lose_life(state, player_id) {
        return None;
    }
    Some(ProposedEvent::LifeLoss {
        player_id,
        amount,
        applied: HashSet::new(),
    })
}

/// Collapses an ordinary-policy core result to the ordinary entries' contract.
/// `IgnoreImpossible` never returns `Unrepresentable` from settlement: with an
/// ingress window it records failure for the public boundary, and without one
/// it uses the logged compatibility skip. This remains a defensive arm.
fn ordinary_life_outcome(result: Result<u32, LifeChangeError>) -> Result<u32, ReplacementDeferred> {
    match result {
        Ok(applied) => Ok(applied),
        Err(LifeChangeError::Deferred(deferred)) => Err(deferred),
        Err(LifeChangeError::Unrepresentable(unrepresentable)) => {
            debug_assert!(
                false,
                "IgnoreImpossible settlement records ingress failure or skips without returning an error"
            );
            unrepresentable.log_discarded("ordinary life entry");
            Ok(0)
        }
    }
}

/// Runs an ingress life change under an open ingress window. A failure
/// recorded anywhere inside the window (a substitute's or continuation's
/// ordinary entry) outranks a deferral, because the whole action is rejected
/// and rolled back by the boundary. The window, policy and fold are the shared
/// `within_rejecting_ingress_window`; a deferral rides through it as a success
/// value.
fn run_ingress_life_change(
    state: &mut GameState,
    proposed: ProposedEvent,
    settle: LifeSettleFn,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeError> {
    let outcome = within_rejecting_ingress_window(state, |state, policy| {
        match apply_life_change_core(state, proposed, settle, policy, events) {
            Ok(applied) => Ok(Ok(applied)),
            Err(LifeChangeError::Deferred(deferred)) => Ok(Err(deferred)),
            Err(LifeChangeError::Unrepresentable(unrepresentable)) => Err(unrepresentable),
        }
    });
    match outcome {
        Ok(Ok(applied)) => Ok(applied),
        Ok(Err(deferred)) => Err(LifeChangeError::Deferred(deferred)),
        Err(unrepresentable) => Err(LifeChangeError::Unrepresentable(unrepresentable)),
    }
}

/// The shared life-change core behind both policies: route `proposed` through
/// the replacement pipeline and settle the result. The policy is chosen by the
/// entry that calls this, never decided here.
///
/// Under `Reject` an unrepresentable final edit returns `Err` BEFORE the
/// stashed post-replacement continuation drains (nothing settles; the action
/// boundary discards the stash with everything else). Under `IgnoreImpossible`
/// the settle step skips only the change the engine's numeric capacity cannot
/// represent and the continuation drains exactly as it does for an applied
/// change. That skip is an engine limit and a compatibility fallback for
/// ordinary resolution outside a public action boundary (and for legacy
/// serialized `PendingReplacement` policy payloads), NOT a CR 101.3
/// impossibility: public boundaries reject the action and restore their entry
/// state instead.
fn apply_life_change_core(
    state: &mut GameState,
    proposed: ProposedEvent,
    settle: LifeSettleFn,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeError> {
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            let applied =
                settle(state, event, policy, events).map_err(LifeChangeError::Unrepresentable)?;
            // CR 614.6 + CR 704.3: Drain any mandatory-post-effect continuation
            // (cross-event substitutes such as Lich; same-event work beyond the
            // applier's amount substitution) inside this resolution step so
            // SBAs and priority never fall between the (possibly substituted)
            // life change and its substitute. Mirrors the drain pattern in
            // `effects/draw.rs::draw_through_replacement`. The `Prevented` arm
            // below already drains via `drain_substitution_continuation`.
            match drain_substitution_continuation(state, events) {
                SubstitutionDrainOutcome::Completed => Ok(applied),
                SubstitutionDrainOutcome::Deferred => {
                    // The root change is FINAL at this point — `applied` is what
                    // the player actually received or paid. Only the substitute
                    // is unfinished.
                    Err(LifeChangeError::Deferred(
                        ReplacementDeferred::SubstitutionContinuation { applied },
                    ))
                }
            }
        }
        ReplacementResult::Prevented => {
            // CR 614.1a + CR 614.6 — Issue #317: Drain substitute effect
            // stashed by cross-event-type replacement (Lich-class).
            match drain_substitution_continuation(state, events) {
                SubstitutionDrainOutcome::Completed => Ok(0),
                SubstitutionDrainOutcome::Deferred => {
                    // Fully prevented: the root changed nothing, whatever the
                    // substitute goes on to do.
                    Err(LifeChangeError::Deferred(
                        ReplacementDeferred::SubstitutionContinuation { applied: 0 },
                    ))
                }
            }
        }
        ReplacementResult::NeedsChoice(player) => {
            // CR 616.1: Multiple competing replacements — player must choose.
            debug_assert!(
                state.ingress_life_window.is_none() || state.pending_replacement.is_some(),
                "an ingress life change that parks must leave its stamped record pending"
            );
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            Err(LifeChangeError::Deferred(
                ReplacementDeferred::ReplacementChoice,
            ))
        }
    }
}

/// Apply life gain, running through the replacement pipeline.
/// Returns the actual amount of life gained (may differ due to replacements like Leyline of Hope).
/// Returns `Err(ReplacementDeferred)` when multiple replacement effects compete and
/// the player must choose which applies first (CR 616.1).
///
/// An engine-unrepresentable result is not a CR 101.3 impossibility. With an
/// open ingress window, the owning public action rejects and restores its entry
/// state; outside one, ordinary resolution and legacy parked records retain the
/// compatibility behavior of logging and skipping the change. Direct action-
/// ingress callers use [`try_apply_life_gain`] to reject it explicitly.
pub fn apply_life_gain(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, ReplacementDeferred> {
    let Some(proposed) = proposed_life_gain(state, player_id, amount) else {
        return Ok(0);
    };
    ordinary_life_outcome(apply_life_change_core(
        state,
        proposed,
        settle_life_gain,
        UnrepresentablePolicy::IgnoreImpossible,
        events,
    ))
}

/// Action-ingress life gain: like [`apply_life_gain`], except that a final
/// result the engine cannot represent is reported as
/// [`LifeChangeError::Unrepresentable`] so the caller rejects the action and
/// the action boundary restores its snapshot. Opens the ingress window for the
/// whole call, so a record parked here is stamped for rejection on resume and
/// an unrepresentable settlement reached through a substitute or continuation
/// is reported too.
pub fn try_apply_life_gain(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeError> {
    let Some(proposed) = proposed_life_gain(state, player_id, amount) else {
        return Ok(0);
    };
    run_ingress_life_change(state, proposed, settle_life_gain, events)
}

/// CR 614.1a + CR 614.6 — Issue #317: Drain a `post_replacement_continuation`
/// stashed by cross-event-type substitution in a life-gain or life-loss
/// replacement. Mirrors the drain points in `engine.rs` (land plays) and
/// `stack.rs` (stack resolution); life-change events have no natural drain
/// site otherwise, so the substitute effect ("draw that many cards instead",
/// Lich) would silently never run.
///
/// `EventContextAmount` in the substitute reads `state.last_effect_count`
/// (CR 615.5 fallback path), which the applier stamps with the prevented
/// amount before returning.
fn drain_substitution_continuation(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> SubstitutionDrainOutcome {
    if !state.has_post_replacement_drain() {
        return SubstitutionDrainOutcome::Completed;
    }

    // A life payment and several other production callers enter this authority
    // while their own prompt is still the ambient `waiting_for`. That prompt is
    // not evidence that the replacement continuation deferred. Run the
    // substitute from a clean synchronous sentinel, then restore the caller's
    // prompt only when the substitute itself completed without raising one.
    let prior_waiting_for = std::mem::replace(
        &mut state.waiting_for,
        WaitingFor::Priority {
            player: state.active_player,
        },
    );
    if let Some(waiting_for) =
        crate::game::engine_replacement::apply_pending_post_replacement_effect(
            state, None, None, None, events,
        )
    {
        state.waiting_for = waiting_for;
        SubstitutionDrainOutcome::Deferred
    } else {
        state.waiting_for = prior_waiting_for;
        SubstitutionDrainOutcome::Completed
    }
}

/// CR 119.3: Settle a post-replacement `ProposedEvent::LifeGain`: the one
/// place the final gain edit is made and its representability is decided.
///
/// Order before anything observable: a recorded replacement-quantity overflow,
/// then the amount must fit an `i32` delta, then the checked scalar edit (life
/// total and gained-this-turn counter) must succeed. An unrepresentable result
/// is handled per `policy`: `Reject` returns the typed error;
/// `IgnoreImpossible` reports `Ok(0)` without a `LifeChanged`. An open ingress
/// window records the failure for the owning public boundary to reject and
/// restore; without one, ordinary resolution and legacy parked records use the
/// logged compatibility skip.
fn settle_life_gain(
    state: &mut GameState,
    event: ProposedEvent,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeUnrepresentable> {
    let ProposedEvent::LifeGain {
        player_id: pid,
        amount: gain_amount,
        ..
    } = event
    else {
        debug_assert!(
            false,
            "settle_life_gain called with non-LifeGain ProposedEvent"
        );
        return Ok(0);
    };
    if let Some(settled) = settle_quantity_overflow(state, pid, policy, "life gain") {
        return settled;
    }
    if ingress_window_has_failed(state) {
        return Ok(0);
    }
    // CR 119.8: gaining 0 life doesn't count as gaining life — no state
    // mutation and no journal command; the event below still fires as before.
    if gain_amount != 0 {
        let edit = i32::try_from(gain_amount)
            .map_err(|_| LifeChangeUnrepresentable { player: pid })
            .and_then(|delta| apply_final_life_edit(state, pid, delta));
        if let Err(unrepresentable) = edit {
            return settle_unrepresentable(state, unrepresentable, policy, "life gain");
        }
    }
    // CR 611.3a + CR 119: gate escalation on a live life-reading static.
    crate::game::layers::mark_layers_full_if_life_reading_static_live(state);
    events.push(GameEvent::LifeChanged {
        player_id: pid,
        amount: gain_amount as i32,
        // CR 119.3: read back after the edit, so a run of life changes carries
        // each intermediate total rather than only the final snapshot's.
        new_total: LifeTotalReading(
            state
                .players
                .iter()
                .find(|player| player.id == pid)
                .map(|player| player.life),
        ),
    });
    Ok(gain_amount)
}

/// Applies the final signed life delta through the checked scalar authority.
fn apply_final_life_edit(
    state: &mut GameState,
    player: PlayerId,
    delta: i32,
) -> Result<(), LifeChangeUnrepresentable> {
    apply_final_player_resource_edit(state, player, ResolvedPlayerEdit::Life { delta })
}

/// Applies a final player-resource gain or life change through the checked
/// scalar authority (`GameState::resolve_and_apply_player_edit`): the one
/// adapter that maps its overflow to `LifeChangeUnrepresentable`. Callers pass
/// a nonzero life delta or a positive counter/energy delta, for which
/// `ResourceOverflow` is the only failure a well-formed final event can
/// produce; every other error is an invariant violation of the caller.
pub(crate) fn apply_final_player_resource_edit(
    state: &mut GameState,
    player: PlayerId,
    edit: ResolvedPlayerEdit,
) -> Result<(), LifeChangeUnrepresentable> {
    match state.resolve_and_apply_player_edit(player, edit) {
        Ok(()) => Ok(()),
        Err(ResolvedPlayerEditReplayInvariantError::ResourceOverflow) => {
            Err(LifeChangeUnrepresentable { player })
        }
        Err(ResolvedPlayerEditReplayInvariantError::UnknownPlayer(_)) => {
            unreachable!("post-replacement player-resource change must target a live player")
        }
        Err(
            ResolvedPlayerEditReplayInvariantError::ZeroDelta
            | ResolvedPlayerEditReplayInvariantError::ResourceUnderflow
            | ResolvedPlayerEditReplayInvariantError::SpeedPreconditionMismatch { .. },
        ) => unreachable!(
            "a nonzero life delta or positive counter/energy delta reports only ResourceOverflow"
        ),
    }
}

/// Whether an open ingress window has already recorded an unrepresentable
/// settlement: the action will be rejected, so no further player-resource edit
/// (life, player counter, or energy) applies.
pub(crate) fn ingress_window_has_failed(state: &GameState) -> bool {
    state
        .ingress_life_window
        .is_some_and(|window| window.failed_player.is_some())
}

/// The failure branch of a settle step, per policy. `Reject` hands the typed
/// error to the caller. `IgnoreImpossible` reports `Ok(0)` for an
/// engine-unrepresentable player-resource change (life, player counter, or
/// energy). An open ingress window records the failure so
/// its public action boundary rejects and restores; with no window, ordinary
/// resolution and legacy parked records log and skip it for compatibility. This
/// fallback is an engine-capacity policy, not CR 101.3 impossibility.
pub(crate) fn settle_unrepresentable(
    state: &mut GameState,
    unrepresentable: LifeChangeUnrepresentable,
    policy: UnrepresentablePolicy,
    site: &'static str,
) -> Result<u32, LifeChangeUnrepresentable> {
    match policy {
        UnrepresentablePolicy::Reject => Err(unrepresentable),
        UnrepresentablePolicy::IgnoreImpossible => {
            match state.ingress_life_window.as_mut() {
                Some(window) => {
                    window.failed_player.get_or_insert(unrepresentable.player);
                }
                // The engine's numeric capacity, not a CR 101.3 impossibility:
                // outside an open ingress window this is the compatibility
                // fallback for ordinary resolution; the log records the skip.
                None => unrepresentable.log_discarded(site),
            }
            Ok(0)
        }
    }
}

/// Consumes a replacement-quantity overflow recorded while this life, player
/// counter or energy event was replaced. The overflowed result exceeds the
/// engine's numeric range, so it takes the same failure branch as any other
/// engine-unrepresentable player-resource change: `Reject` returns the typed error; `IgnoreImpossible` returns `Ok(0)`
/// and either records failure for an open ingress window to reject and restore,
/// or logs and skips under the outside-boundary compatibility policy. This is
/// a capacity limit, not a CR 101.3 impossibility. Taken first so the record is
/// always cleared, even when an ingress window has already failed. `None` when
/// nothing was recorded.
pub(crate) fn settle_quantity_overflow(
    state: &mut GameState,
    pid: PlayerId,
    policy: UnrepresentablePolicy,
    site: &'static str,
) -> Option<Result<u32, LifeChangeUnrepresentable>> {
    state.take_replacement_quantity_overflow().map(|_| {
        settle_unrepresentable(
            state,
            LifeChangeUnrepresentable { player: pid },
            policy,
            site,
        )
    })
}

/// Applies a post-replacement `ProposedEvent::LifeGain` under the compatibility
/// policy. An open ingress window records an engine-unrepresentable result for
/// the public boundary to reject and restore; without one, ordinary resolution
/// and legacy parked records log and skip it for compatibility.
///
/// Extracted from `apply_life_gain`'s Execute arm so the same mutation can be
/// invoked by `resolve_gain`. Caller is responsible for emitting `EffectResolved`.
pub fn apply_life_gain_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> u32 {
    ordinary_settled_amount(settle_life_gain(
        state,
        event,
        UnrepresentablePolicy::IgnoreImpossible,
        events,
    ))
}

/// Resume-path settle for a parked life gain: applies the event the player's
/// `ChooseReplacement` answer delivered under the policy the parked record was
/// stamped with. `Reject` returns the typed error so the boundary restores the
/// record and prompt. `IgnoreImpossible` reports `Ok(0)`; an open ingress
/// window still makes its public action reject and restore, while a legacy
/// payload resumed without one retains the logged compatibility skip.
pub(crate) fn settle_life_gain_after_choice(
    state: &mut GameState,
    event: ProposedEvent,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeUnrepresentable> {
    settle_life_gain(state, event, policy, events)
}

/// Compatibility-policy settle result as a plain amount. `IgnoreImpossible`
/// records ingress failure for the public owner to reject and restore, or uses
/// the logged skip outside a boundary; the `Err` arm is defensive.
fn ordinary_settled_amount(result: Result<u32, LifeChangeUnrepresentable>) -> u32 {
    ordinary_settlement(result, 0, "ordinary life settle")
}

/// Compatibility-policy settle result for any player-resource settle step.
/// `IgnoreImpossible` records ingress failure for the public owner to reject
/// and restore, or uses the logged skip outside a boundary; the `Err` arm is
/// defensive, and yields `skipped`.
pub(crate) fn ordinary_settlement<T>(
    result: Result<T, LifeChangeUnrepresentable>,
    skipped: T,
    site: &'static str,
) -> T {
    result.unwrap_or_else(|unrepresentable| {
        debug_assert!(
            false,
            "compatibility settlement records ingress failure or skips outside a boundary"
        );
        unrepresentable.log_discarded(site);
        skipped
    })
}

/// CR 119.3: An effect that causes a player to lose life adjusts the total by
/// subtracting that much
/// from the player's life total.
/// Returns the actual amount of life lost (may differ due to replacements like doubling).
/// Returns `Err(ReplacementDeferred)` whenever the replacement/substitution
/// pipeline pauses for player input.
///
/// A final loss beyond the engine's numeric capacity is not a CR 101.3
/// impossibility. An open ingress window makes the owning public action reject
/// and restore; without one, ordinary resolution and legacy parked records use
/// the logged compatibility skip. Direct action-ingress callers use
/// [`try_apply_life_loss`] to reject explicitly.
pub fn apply_life_loss(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, ReplacementDeferred> {
    let Some(proposed) = proposed_life_loss(state, player_id, amount) else {
        return Ok(0);
    };
    ordinary_life_outcome(apply_life_change_core(
        state,
        proposed,
        settle_life_loss,
        UnrepresentablePolicy::IgnoreImpossible,
        events,
    ))
}

/// Action-ingress life loss: the loss twin of [`try_apply_life_gain`].
pub fn try_apply_life_loss(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeError> {
    let Some(proposed) = proposed_life_loss(state, player_id, amount) else {
        return Ok(0);
    };
    run_ingress_life_change(state, proposed, settle_life_loss, events)
}

/// CR 120.3a: Damage dealt to a player normally causes that player to lose
/// that much life. This damage-specific seam delegates the resulting life-loss
/// event to the generic CR 119.3 authority.
pub fn apply_damage_life_loss(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<u32, ReplacementDeferred> {
    apply_life_loss(state, player_id, amount, events)
}

/// CR 119.3: Settle a post-replacement `ProposedEvent::LifeLoss`: the loss twin
/// of [`settle_life_gain`] (same order, same per-policy failure branch).
fn settle_life_loss(
    state: &mut GameState,
    event: ProposedEvent,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeUnrepresentable> {
    let ProposedEvent::LifeLoss {
        player_id: pid,
        amount: loss_amount,
        ..
    } = event
    else {
        debug_assert!(
            false,
            "settle_life_loss called with non-LifeLoss ProposedEvent"
        );
        return Ok(0);
    };
    if let Some(settled) = settle_quantity_overflow(state, pid, policy, "life loss") {
        return settled;
    }
    if ingress_window_has_failed(state) {
        return Ok(0);
    }
    // CR 119.8: losing 0 life doesn't count as losing life — no state
    // mutation and no journal command; the event below still fires as before.
    if loss_amount != 0 {
        let before = crate::game::players::team_life_total(state, pid);
        // A loss negates a value that already fit an `i32`, so the negation
        // itself cannot overflow.
        let edit = i32::try_from(loss_amount)
            .map_err(|_| LifeChangeUnrepresentable { player: pid })
            .and_then(|magnitude| apply_final_life_edit(state, pid, -magnitude));
        if let Err(unrepresentable) = edit {
            return settle_unrepresentable(state, unrepresentable, policy, "life loss");
        }
        let after = crate::game::players::team_life_total(state, pid);
        // The clone-local preview observes the real edit before events or a
        // substitution continuation can introduce later, unrelated work.
        crate::game::life_safety::record_life_mutation_receipt(
            state,
            pid,
            before,
            after,
            loss_amount,
        );
    }
    // CR 611.3a + CR 119 + CR 120.3a: gate escalation on a live life-reading
    // static. Also the sink for non-infect player damage (deal_damage.rs).
    crate::game::layers::mark_layers_full_if_life_reading_static_live(state);
    events.push(GameEvent::LifeChanged {
        player_id: pid,
        amount: -(loss_amount as i32),
        // CR 119.3: read back after the edit, so a run of combat-damage life
        // losses carries each intermediate total rather than only the final one.
        new_total: LifeTotalReading(
            state
                .players
                .iter()
                .find(|player| player.id == pid)
                .map(|player| player.life),
        ),
    });
    Ok(loss_amount)
}

/// CR 119.3: Apply a post-replacement `ProposedEvent::LifeLoss` under the
/// compatibility policy. An open ingress window records an engine-unrepresentable
/// result for the public boundary to reject and restore; without one, ordinary
/// resolution and legacy parked records log and skip it for compatibility.
///
/// Extracted from `apply_life_loss`'s Execute arm. Caller is responsible for
/// emitting `EffectResolved`.
pub fn apply_life_loss_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> u32 {
    ordinary_settled_amount(settle_life_loss(
        state,
        event,
        UnrepresentablePolicy::IgnoreImpossible,
        events,
    ))
}

/// Resume-path settle for a parked life loss: the loss twin of
/// [`settle_life_gain_after_choice`].
pub(crate) fn settle_life_loss_after_choice(
    state: &mut GameState,
    event: ProposedEvent,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<u32, LifeChangeUnrepresentable> {
    settle_life_loss(state, event, policy, events)
}

/// Outcome of applying a life-total permutation via `apply_life_totals_assignment`.
///
/// Typed (not a bare bool) so the control-flow signal is self-documenting at
/// call sites: `Applied` means the caller should emit its `EffectResolved`;
/// `Deferred` means a competing replacement (CR 616.1) installed a choice
/// `WaitingFor` and the caller must return without emitting — the resume path
/// completes resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeAssignmentOutcome {
    Applied,
    Deferred,
    /// A receiver's current-to-resulting difference does not fit an `i32`.
    /// Nothing was mutated (every delta is computed before the first edit) and
    /// the failure is recorded in the open ingress window, so the action
    /// boundary rejects the whole action.
    Unrepresentable,
}

/// The signed life change that takes `current` to `target`, or `None` when the
/// difference is not representable as an `i32`.
///
/// `Some(0)` is a real no-op; `None` is a failure. The failure is recorded in
/// the open ingress life window (so an action boundary rejects the whole
/// action), or logged as a discarded change when no window is open. Callers
/// must stop mutating on `None` rather than substituting zero: a partial
/// assignment would be observable if no boundary rolled it back. Shared by
/// every site that feeds the life authority a difference.
pub(super) fn life_total_difference(
    state: &mut GameState,
    player: PlayerId,
    target: i32,
    current: i32,
    site: &'static str,
) -> Option<i32> {
    match target.checked_sub(current) {
        Some(diff) => Some(diff),
        None => {
            let unrepresentable = LifeChangeUnrepresentable { player };
            let _ = settle_unrepresentable(
                state,
                unrepresentable,
                UnrepresentablePolicy::IgnoreImpossible,
                site,
            );
            None
        }
    }
}

/// CR 701.12c / CR 119.7 + CR 119.8: Apply a simultaneous life-total permutation.
///
/// `assignment[i] = (receiver, resulting_life)` — each named player's life total
/// becomes `resulting_life` by *gaining or losing the difference* from a snapshot
/// taken before any mutation (so the permutation is simultaneous and each delta
/// is measured against the pre-resolution total, per CR 701.12c). The changes
/// route through `apply_life_gain` / `apply_life_loss` — not a raw
/// `player.life = ...` set — so replacement effects may modify them and triggered
/// abilities (Blood Artist-likes) trigger on the resulting gain/loss.
///
/// Shared by `ExchangeLifeTotals` (2-slot swap) and `RedistributeLifeTotals`
/// (N-slot controller-chosen permutation). Callers that require all-or-nothing
/// legality (CR 701.12a exchange) must pre-check before calling; the
/// redistribution resolver instead filters illegal receivers out of each
/// enumerated option, so every assignment reaching this helper is already legal.
pub fn apply_life_totals_assignment(
    state: &mut GameState,
    assignment: &[(PlayerId, i32)],
    completion_player: PlayerId,
    completion: Option<PendingEffectResolved>,
    events: &mut Vec<GameEvent>,
) -> Result<LifeAssignmentOutcome, EffectError> {
    // CR 701.12c: snapshot every receiver's current life BEFORE any mutation so
    // each delta is measured against the pre-permutation total. A difference
    // that does not fit an `i32` fails the whole assignment before the first
    // edit, so no receiver's change is partially committed.
    let mut deltas: Vec<(PlayerId, i32)> = Vec::with_capacity(assignment.len());
    for &(pid, new_life) in assignment {
        let old = state
            .players
            .iter()
            .find(|p| p.id == pid)
            .map(|p| p.life)
            .ok_or(EffectError::PlayerNotFound)?;
        let Some(diff) = life_total_difference(state, pid, new_life, old, "life-total assignment")
        else {
            return Ok(LifeAssignmentOutcome::Unrepresentable);
        };
        deltas.push((pid, diff));
    }

    for (index, (pid, diff)) in deltas.iter().copied().enumerate() {
        let deferred = match diff.signum() {
            1 => apply_life_gain(state, pid, diff as u32, events).err(),
            // `unsigned_abs`: a stored difference may be `i32::MIN`.
            -1 => apply_life_loss(state, pid, diff.unsigned_abs(), events).err(),
            _ => None,
        };
        if deferred.is_some() {
            // CR 616.1: a competing replacement required a player choice; the
            // helper installed the WaitingFor and the resume path completes the
            // remaining assignments.
            state.push_life_total_assignment(PendingLifeTotalAssignment {
                completion_player,
                remaining: deltas[index + 1..].to_vec(),
                completion: completion.clone(),
            });
            return Ok(LifeAssignmentOutcome::Deferred);
        }
    }
    Ok(LifeAssignmentOutcome::Applied)
}

pub(crate) fn drain_pending_life_total_assignment(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    while let Some(mut pending) = state.take_active_life_total_assignment() {
        state.waiting_for = WaitingFor::Priority {
            player: pending.completion_player,
        };

        let Some((pid, diff)) = pending.remaining.first().copied() else {
            complete_pending_life_total_assignment(state, pending, events);
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                return;
            }
            continue;
        };

        pending.remaining.remove(0);
        state.push_life_total_assignment(pending);

        let deferred = match diff.signum() {
            1 => apply_life_gain(state, pid, diff as u32, events).err(),
            // `unsigned_abs`: a stored difference may be `i32::MIN`.
            -1 => apply_life_loss(state, pid, diff.unsigned_abs(), events).err(),
            _ => None,
        };
        if deferred.is_some() || !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
    }
}

fn complete_pending_life_total_assignment(
    state: &mut GameState,
    pending: PendingLifeTotalAssignment,
    events: &mut Vec<GameEvent>,
) {
    state.waiting_for = WaitingFor::Priority {
        player: pending.completion_player,
    };

    if let Some(PendingEffectResolved {
        kind,
        source_id,
        resolution_event,
        post_actions,
        player_action,
    }) = pending.completion
    {
        debug_assert!(
            post_actions.is_empty(),
            "life-total assignment completion does not support counter post-actions"
        );
        match resolution_event {
            PendingEffectResolutionEvent::Emit => {
                events.push(GameEvent::EffectResolved {
                    kind,
                    source_id,
                    subject: None,
                });
            }
            PendingEffectResolutionEvent::Suppress => {}
        }
        if let Some(action) = player_action {
            events.push(GameEvent::PlayerPerformedAction {
                player_id: action.player_id,
                action: action.action,
                look_count: None,
                scry_bottom_count: None,
                scry_top_count: None,
            });
        }
    }

    super::drain_pending_continuation(state, events);
}

/// CR 119.3: If an effect causes a player to lose life, adjust their life total.
pub fn resolve_lose(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, target_filter): (i32, Option<&TargetFilter>) = match &ability.effect {
        Effect::LoseLife { amount, target } => (
            crate::game::quantity::resolve_quantity_with_targets(state, amount, ability),
            target.as_ref(),
        ),
        _ => return Err(EffectError::MissingParam("LoseLife amount".to_string())),
    };

    // CR 608.2b: a declared player whose target was illegal loses nothing.
    let Some(target_player_id) = resolve_life_loss_target(state, ability, target_filter) else {
        return Ok(());
    };

    if apply_life_loss(state, target_player_id, amount.max(0) as u32, events).is_err() {
        return Ok(());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

pub(super) fn resolve_life_loss_target(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: Option<&TargetFilter>,
) -> Option<PlayerId> {
    // CR 115.1: When the filter is a context-ref (Controller, etc.) the acting
    // player MUST come from state slots — not `ability.targets`, which inherits
    // the parent's chosen Player target via chain target propagation. Mirrors
    // the Draw/Mill/Discard guard in `resolve_player_for_context_ref`.
    if let Some(filter) = target_filter {
        if filter.is_context_ref() {
            return super::resolve_player_for_context_ref(state, ability, filter);
        }
    }

    // Non-context-ref filters (e.g., explicit Player target on "target opponent
    // loses 2 life"): the chosen player is in `ability.targets`.
    // CR 601.2c: the read skips a separately announced quantity slot, so the
    // primary target keeps its distinct slot identity.
    if let Some(player) =
        crate::game::ability_utils::primary_announced_player(&ability.targets, ability)
    {
        return Some(player);
    }

    // No filter and no Player target: defensive fallback to controller (matches
    // historical behavior for `LoseLife { target: None }`). CR 608.2b: a filter
    // that declared its own target slot names no one once that slot is empty.
    match target_filter {
        Some(filter) if super::declared_player_slot_is_empty(state, ability, filter) => None,
        _ => Some(ability.controller),
    }
}

/// CR 119.5: Set a player's life total to a specific number.
///
/// Per CR 119.5: "If an effect sets a player's life total to a specific number,
/// the player gains or loses the necessary amount of life to end up with the
/// new total." The delta is therefore dispatched as either a `LifeGain` or
/// `LifeLoss` event through [`apply_life_gain`] / [`apply_life_loss`] so
/// the full replacement pipeline fires and the CantGainLife / CantLoseLife
/// short-circuits are consistent with every other life-change event.
pub fn resolve_set_life_total(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount_expr, target) = match &ability.effect {
        Effect::SetLifeTotal { amount, target } => (amount, target),
        _ => return Err(EffectError::MissingParam("SetLifeTotal amount".to_string())),
    };
    // CR 119.5 + CR 608.2f: Resolve which players' life totals are set. The
    // common single-player forms ("your" → Controller, "target player's" →
    // the chosen target) preserve the original single-player behavior. The
    // non-targeted all-players form ("each player's life total becomes N" —
    // Worldfire, issue #2882) expands to every player in APNAP order.
    let target_player_ids: Vec<PlayerId> = if matches!(target, TargetFilter::AllPlayers) {
        crate::game::players::apnap_order(state)
    } else {
        let Some(player) = resolve_life_loss_target(state, ability, Some(target)) else {
            return Ok(());
        };
        vec![player]
    };

    // CR 119.5: Set each player's life total one at a time, decomposing into the
    // matching gain/loss event. apply_life_gain / apply_life_loss each
    // handle their own CR 119.7 / CR 119.8 short-circuits and replacement
    // pipeline routing.
    for target_player_id in target_player_ids {
        // CR 119.5 + CR 109.5: Resolve the new life total per player so a
        // third-person "the number of [X] THEY control" count (Biorhythm,
        // Shaman of Forgotten Ways) binds to each recipient. `scoped_player` is
        // rebound to the player whose life total is being set; the count's
        // `ScopedPlayer` controller reads it, while `original_controller` (and
        // hence any `You`-scoped count) and the ability's targets / chosen_x
        // stay fixed to the caster. Single-player and caster-scoped forms
        // ("becomes 10", "your life total", Repay in Kind's cross-player
        // extremum) are unaffected — they carry no `ScopedPlayer` ref to vary.
        let mut scoped_ability = ability.clone();
        scoped_ability.set_scoped_player_recursive(target_player_id);
        let amount = crate::game::quantity::resolve_quantity_with_targets(
            state,
            amount_expr,
            &scoped_ability,
        );

        // CR 810.9a: "If a cost or effect needs to know the value of an
        // individual player's life total, that cost or effect uses the
        // team's life total instead" — degenerates to `Player::life` outside
        // team-based formats. CR 810.9c: the diff is still applied to only
        // `target_player_id`'s own life, so the team total moves by exactly
        // the gained/lost amount.
        if !state.players.iter().any(|p| p.id == target_player_id) {
            return Err(EffectError::PlayerNotFound);
        }
        let current_life = crate::game::players::team_life_total(state, target_player_id);
        let Some(diff) = life_total_difference(
            state,
            target_player_id,
            amount,
            current_life,
            "set life total",
        ) else {
            // The failure is recorded in the ingress window (or logged when no
            // window is open). Stop before any later player's life is set.
            return Ok(());
        };

        let deferred = match diff.signum() {
            1 => apply_life_gain(state, target_player_id, diff as u32, events).err(),
            // `unsigned_abs`: the difference may be `i32::MIN`.
            -1 => apply_life_loss(state, target_player_id, diff.unsigned_abs(), events).err(),
            _ => None,
        };
        if deferred.is_some() {
            // CR 616.1: A competing replacement required a player choice; the
            // helper already installed the WaitingFor state. Return without
            // emitting EffectResolved — the resume path completes resolution.
            // (A multi-player set that hits a replacement mid-list defers from
            // that player onward, mirroring the original single-player path.)
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::AttachTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AggregateFunction, ChooseOneOfKind, ControllerRef, QuantityExpr, QuantityRef,
        SharedQuality, StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    /// Helper: add a permanent with the given life-lock `StaticMode` affecting
    /// players matching `ControllerRef`. Mirrors `win_lose::tests::add_cant_win_permanent`.
    fn add_life_lock_permanent(
        state: &mut GameState,
        owner: PlayerId,
        mode: StaticMode,
        affected_controller: ControllerRef,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(900),
            owner,
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(mode).affected(TargetFilter::Typed(
                TypedFilter::default().controller(affected_controller),
            )),
        );
        id
    }

    #[test]
    fn gain_life_increases_controller_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 25);
    }

    #[test]
    fn set_life_total_all_players_sets_every_player_no_target() {
        // CR 119.5 + issue #2882: Worldfire's "Each player's life total becomes 1"
        // must set EVERY player to 1 with no targeting prompt — not just the
        // controller and not a single chosen player.
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 15;
        let ability = ResolvedAbility::new(
            Effect::SetLifeTotal {
                target: TargetFilter::AllPlayers,
                amount: QuantityExpr::Fixed { value: 1 },
            },
            vec![], // no Player targets — AllPlayers is a non-targeted scope
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_set_life_total(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life, 1,
            "controller's life should become 1"
        );
        assert_eq!(state.players[1].life, 1, "opponent's life should become 1");
    }

    #[test]
    fn skemfar_shadowsage_gain_life_mode_uses_shared_creature_type_count() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec![
            "Elf".to_string(),
            "Warrior".to_string(),
            "Druid".to_string(),
            "Human".to_string(),
        ];
        let source = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Skemfar Shadowsage".to_string(),
            Zone::Battlefield,
        );
        for (name, subtypes) in [
            ("Elf Warrior", vec!["Elf", "Warrior"]),
            ("Elf Druid", vec!["Elf", "Druid"]),
            ("Human Warrior", vec!["Human", "Warrior"]),
        ] {
            let id = create_object(
                &mut state,
                CardId(902),
                PlayerId(0),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types = vec![CoreType::Creature];
            obj.card_types.subtypes = subtypes
                .into_iter()
                .map(|subtype| subtype.to_string())
                .collect();
        }
        let changeling = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Masked Vandal".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&changeling).unwrap();
        obj.card_types.core_types = vec![CoreType::Creature];
        obj.card_types.subtypes = vec!["Shapeshifter".to_string()];
        obj.keywords.push(Keyword::Changeling);

        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCountBySharedQuality {
                        filter: TargetFilter::Typed(TypedFilter {
                            type_filters: vec![TypeFilter::Creature],
                            controller: Some(ControllerRef::You),
                            properties: Vec::new(),
                        }),
                        quality: SharedQuality::CreatureType,
                        aggregate: AggregateFunction::Max,
                    },
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn lose_life_decreases_target_life() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 17);
    }

    /// CR 115.1 + CR 119.3: Astarion, the Decadent (Feed mode) — "Target
    /// opponent loses life equal to the amount of life they lost this turn."
    /// The third-person "they" anaphor is the effect's player target, so the
    /// amount resolves through `PlayerScope::Target` (the target's *own*
    /// `life_lost_this_turn`), NOT the controller's. The controller's count is
    /// seeded high as a trap: a `Controller`-scoped resolution would drain the
    /// target by 99 instead of 3.
    #[test]
    fn lose_life_amount_target_relative_life_lost_this_turn() {
        use crate::types::ability::PlayerScope;
        let mut state = GameState::new_two_player(42);
        state.players[0].life_lost_this_turn = 99; // controller — must be ignored
        state.players[1].life_lost_this_turn = 3; // target opponent
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::Target,
                    },
                },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        // Target opponent (20 starting) loses 3 — their own life lost this turn.
        assert_eq!(state.players[1].life, 17);
        // Controller is untouched (it is the source, not a target/recipient).
        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn lose_life_parent_target_controller_uses_attack_event_source() {
        let mut state = GameState::new_two_player(42);
        let decree = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Marchesa's Decree".to_string(),
            Zone::Battlefield,
        );
        let attacker = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(0),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(0)),
            )],
            declaration_records: Vec::new(),
        });
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::ParentTargetController),
            },
            vec![],
            decree,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 19);
    }

    #[test]
    fn lose_life_attached_to_resolves_player_host() {
        let mut state = GameState::new_two_player(42);
        let curse = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Curse".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&curse).unwrap().attached_to =
            Some(AttachTarget::Player(PlayerId(1)));

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::AttachedTo),
            },
            vec![],
            curse,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 18);
    }

    #[test]
    fn gain_life_emits_positive_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == 4)));
    }

    #[test]
    fn lose_life_emits_negative_life_changed() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount, .. } if *amount == -2)));
    }

    /// CR 119.3: every `LifeChanged` reports the player's life total
    /// as it stands once that one change is applied — not the total after the
    /// whole action. A run of changes is therefore replayable one at a time,
    /// which is what lets a presentation layer show intermediate totals without
    /// summing amounts (a sum diverges once a replacement alters one of them).
    #[test]
    fn life_changed_reports_the_total_after_each_individual_change() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        for amount in [3, 4] {
            let ability = ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: TargetFilter::Controller,
                },
                vec![],
                ObjectId(100),
                PlayerId(0),
            );
            resolve_gain(&mut state, &ability, &mut events).unwrap();
        }

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 5 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        resolve_lose(&mut state, &ability, &mut events).unwrap();

        let totals: Vec<(i32, Option<i32>)> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id,
                    amount,
                    new_total,
                } if *player_id == PlayerId(0) => Some((*amount, new_total.0)),
                _ => None,
            })
            .collect();

        assert_eq!(totals, vec![(3, Some(23)), (4, Some(27)), (-5, Some(22))]);
        assert_eq!(state.players[0].life, 22);
    }

    /// CR 119.7: "can't gain life" suppresses life gain, life total unchanged.
    #[test]
    fn gain_life_blocked_by_cant_gain_life() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20, "life total must not change");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
            "no LifeChanged event should be emitted"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::GainLife,
                ..
            }
        )));
    }

    /// CR 119.7: `apply_life_gain` must short-circuit before the replacement
    /// pipeline — replacements "won't do anything" per CR 119.7.
    #[test]
    fn apply_life_gain_short_circuits_on_cant_gain() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        let mut events = Vec::new();
        let gained = apply_life_gain(&mut state, PlayerId(0), 3, &mut events).unwrap();

        assert_eq!(gained, 0);
        assert_eq!(state.players[0].life, 20);
    }

    /// CR 119.8: `apply_damage_life_loss` short-circuits for a CantLoseLife player.
    #[test]
    fn apply_damage_life_loss_short_circuits_on_cant_lose() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantLoseLife,
            ControllerRef::You,
        );

        let mut events = Vec::new();
        let lost = apply_damage_life_loss(&mut state, PlayerId(0), 4, &mut events).unwrap();

        assert_eq!(lost, 0);
        assert_eq!(state.players[0].life, 20);
    }

    /// CR 701.12c + CR 616.1: If a life-total assignment pauses on a replacement
    /// choice, the resume path must apply the remaining snapshot deltas.
    #[test]
    fn life_total_assignment_resumes_tail_after_replacement_choice() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{ReplacementDefinition, ReplacementMode};
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 5;

        let shield = create_object(
            &mut state,
            CardId(950),
            PlayerId(0),
            "Life Shield".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&shield)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::LifeReduced)
                    .mode(ReplacementMode::Optional { decline: None })
                    .description("Life Shield".to_string()),
            );

        let mut events = Vec::new();
        let outcome = apply_life_totals_assignment(
            &mut state,
            &[(PlayerId(0), 5), (PlayerId(1), 20)],
            PlayerId(0),
            Some(PendingEffectResolved::new(
                EffectKind::ExchangeLifeTotals,
                ObjectId(100),
            )),
            &mut events,
        )
        .unwrap();

        assert_eq!(outcome, LifeAssignmentOutcome::Deferred);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[1].life, 5);

        let WaitingFor::ReplacementChoice { player, .. } = state.waiting_for.clone() else {
            panic!("expected replacement choice");
        };
        let serialized = serde_json::to_string(
            &crate::types::resolution::ResolutionStateWire::from_game_state(state),
        )
        .expect("life-total assignment prompt serializes as v2 frames");
        let mut state =
            serde_json::from_str::<crate::types::resolution::ResolutionStateWire>(&serialized)
                .expect("life-total assignment prompt restores from v2 frames")
                .into_game_state();
        state.rehydrate_rng();
        state.active_player = player;
        state.priority_player = player;

        let result = apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("accept life-loss replacement");

        assert_eq!(state.players[0].life, 5);
        assert_eq!(state.players[1].life, 20);
        assert!(state.active_life_total_assignment().is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::ExchangeLifeTotals,
                source_id: ObjectId(100),
                ..
            }
        )));
    }

    /// CR 119.8: `resolve_lose` suppresses life loss for CantLoseLife player.
    #[test]
    fn lose_life_blocked_by_cant_lose_life() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(1),
            StaticMode::CantLoseLife,
            ControllerRef::You,
        );

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 20);
    }

    /// CR 119.5 + CR 119.7 + CR 119.8: Set-life-total is gated by both locks.
    /// With both active (Teferi's Protection case), no life change occurs even
    /// from a set-life-to-N effect.
    #[test]
    fn set_life_total_blocked_by_both_locks() {
        let mut state = GameState::new_two_player(42);
        let id = add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );
        // Add the CantLoseLife static to the same permanent.
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

        // Try to set PlayerId(0)'s life to 10 (would lose 10).
        let ability_loss = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 10 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_loss, &mut events).unwrap();
        assert_eq!(state.players[0].life, 20, "life loss must be suppressed");

        // Try to set life to 40 (would gain 20).
        let ability_gain = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 40 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_gain, &mut events).unwrap();
        assert_eq!(state.players[0].life, 20, "life gain must be suppressed");
    }

    /// CR 119.5 + CR 119.8: Setting life to a lower total under CantLoseLife
    /// suppresses only the loss direction — the life total stays the same.
    #[test]
    fn set_life_total_downward_blocked_by_cant_lose_only() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantLoseLife,
            ControllerRef::You,
        );

        // Setting life to 5 would lose 15.
        let ability = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.players[0].life, 20,
            "loss direction must be suppressed"
        );
    }

    /// CR 119.5 + CR 119.7: Setting life to a higher total under CantGainLife
    /// suppresses only the gain direction — the life total stays the same.
    #[test]
    fn set_life_total_upward_blocked_by_cant_gain_only() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        // Setting life to 30 would gain 10.
        let ability = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 30 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.players[0].life, 20,
            "gain direction must be suppressed"
        );
    }

    /// CR 119.5: With no locks, set-life-total routes through the gain/loss
    /// helpers and updates the life total both directions.
    #[test]
    fn set_life_total_both_directions_without_locks() {
        // Upward.
        let mut state = GameState::new_two_player(42);
        let ability_gain = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 30 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_gain, &mut events).unwrap();
        assert_eq!(state.players[0].life, 30, "set-life-up must take effect");

        // Downward.
        let ability_loss = ResolvedAbility::new(
            Effect::SetLifeTotal {
                amount: QuantityExpr::Fixed { value: 5 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_set_life_total(&mut state, &ability_loss, &mut events).unwrap();
        assert_eq!(state.players[0].life, 5, "set-life-down must take effect");
    }

    /// CR 119.7: The lock only affects players matching the static's filter.
    /// An opponent with no lock continues to gain life normally.
    #[test]
    fn cant_gain_life_only_affects_matching_player() {
        let mut state = GameState::new_two_player(42);
        add_life_lock_permanent(
            &mut state,
            PlayerId(0),
            StaticMode::CantGainLife,
            ControllerRef::You,
        );

        // PlayerId(1) is not affected by this permanent's "You" scope.
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(200),
            PlayerId(1),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].life, 25, "opponent still gains life");
    }

    #[test]
    fn lose_life_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained `LoseLife { target: Some(Controller) }`
        // must hit the spell controller, not the parent's inherited Player
        // target. Mirrors the Discard / Shuffle / Mill / Draw guard.
        let mut state = GameState::new_two_player(42);
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;

        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: Some(TargetFilter::Controller),
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_lose(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life,
            p0_life_before - 2,
            "P0 (controller) should lose life — Controller filter resolves to caster"
        );
        assert_eq!(
            state.players[1].life, p1_life_before,
            "P1 must not lose life despite being in ability.targets — Controller filter must not consult inherited targets"
        );
    }

    /// Issue #310 audit: "Each opponent loses N life." (Bloodletting,
    /// Bloodtithe Harvester face, etc.) parses as `Effect::LoseLife
    /// { target: None }` with `player_scope: Opponent` on the surrounding
    /// ability. The player_scope iteration loop must rebind `controller`
    /// to each opponent per CR 608.2 + CR 109.5 so the inner LoseLife
    /// resolver picks the iterated player as the life loser.
    #[test]
    fn player_scope_opponent_lose_life_targets_each_opponent() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::PlayerFilter;

        let mut state = GameState::new_two_player(42);
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;

        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].life, p0_life_before,
            "caster must not lose life from Each opponent loses N life"
        );
        assert_eq!(
            state.players[1].life,
            p1_life_before - 2,
            "opponent must lose 2 life"
        );
    }

    /// CR 119.3 + CR 109.5: Genesis of the Daleks chapter IV — "...and each of
    /// your opponents loses life equal to ...". End-to-end resolution guard for
    /// the parser fix that lifts the "each of your opponents" subject onto the
    /// sub_ability's `player_scope` (leaving `LoseLife.target: None`) instead of
    /// stamping a non-resolvable `Typed(controller=Opponent)` target.
    ///
    /// This drives the actual parsed encoding through `resolve_ability_chain`:
    /// the controller (p0) must NOT lose life and the opponent (p1) MUST. The
    /// AST-only parser test (`genesis_villainous_branch_splits_destroy_and_lose_life`)
    /// asserts the shape; this test asserts the runtime routing, closing the
    /// path-divergence gap where a green AST test masked the inverse drain.
    #[test]
    fn genesis_each_opponent_loses_life_drains_opponent() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::PlayerFilter;

        let mut state = GameState::new_two_player(42);
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;

        // Genesis branch-1 LoseLife encoding: undirected `target: None` with the
        // each-opponent scope lifted onto the ability (the post-fix shape). The
        // amount is fixed here — the bug under test is target routing, not amount
        // resolution (the dynamic `ZoneChangeAggregateThisTurn` amount is covered
        // by the AST parser test).
        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 5 },
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(
            state.players[0].life, p0_life_before,
            "Genesis's controller (p0) must NOT lose life — the each-opponent \
             scope routes the loss to opponents, not the source controller"
        );
        assert_eq!(
            state.players[1].life,
            p1_life_before - 5,
            "each opponent (p1) must lose 5 life"
        );
    }

    /// CR 115.10a + CR 119.3 + CR 608.2c (Wound Reflection / Archfiend of Despair /
    /// Warlock Class L3): "each opponent loses life equal to the life they lost
    /// this turn" resolves with a `LifeLostThisTurn { ScopedPlayer }` amount under
    /// `player_scope: Opponent`. Each iterated opponent must lose its OWN life lost
    /// this turn — NOT the source controller's. The controller's count is seeded
    /// high (99) as a trap: the prior `Controller`-scoped encoding drained every
    /// opponent by 99. This is the canonical runtime regression guard for the
    /// reported bug; the parser fix is covered in oracle_effect/mod.rs.
    #[test]
    fn each_opponent_loses_own_life_lost_uses_scoped_player() {
        use crate::game::effects::resolve_ability_chain;
        use crate::types::ability::{PlayerFilter, PlayerScope};

        // 3-player game so two distinct opponents prove per-iteration scoping.
        let mut state = GameState::new(crate::types::FormatConfig::standard(), 3, 42);
        state.players[0].life_lost_this_turn = 99; // controller — trap, must be ignored
        state.players[1].life_lost_this_turn = 3; // opponent A
        state.players[2].life_lost_this_turn = 5; // opponent B
        let p0_life_before = state.players[0].life;
        let p1_life_before = state.players[1].life;
        let p2_life_before = state.players[2].life;

        let mut ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::LifeLostThisTurn {
                        player: PlayerScope::ScopedPlayer,
                    },
                },
                target: None,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.player_scope = Some(PlayerFilter::Opponent);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Controller untouched — it is the source, not an affected opponent.
        assert_eq!(
            state.players[0].life, p0_life_before,
            "controller must not lose life — the trap value (99) must be ignored"
        );
        // Each opponent loses ITS OWN life lost this turn (3 and 5), not 99.
        assert_eq!(
            state.players[1].life,
            p1_life_before - 3,
            "opponent A must lose its own life lost (3), not the controller's 99"
        );
        assert_eq!(
            state.players[2].life,
            p2_life_before - 5,
            "opponent B must lose its own life lost (5), not the controller's 99"
        );
    }

    /// Issue #317 (Lich): "If you would gain life, draw that many cards
    /// instead." The replacement substitutes a *different* event type
    /// (`Effect::Draw`) for the original `LifeGain` event. CR 614.1a +
    /// CR 614.6: the original event is suppressed, the substitute effect
    /// runs as a post-replacement continuation. `EventContextAmount` in
    /// "draw that many cards" must resolve against the original prevented
    /// gain quantity (via `state.last_effect_count` per the CR 615.5
    /// fallback path).
    #[test]
    fn lich_gain_life_substituted_by_draw_cards_instead() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        // Lich source — its GainLife replacement substitutes Draw.
        let lich = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Lich".to_string(),
            Zone::Battlefield,
        );
        let replacement =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&lich)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        // Stock the controller's library so Draw has cards to pull.
        for i in 0..10 {
            create_object(
                &mut state,
                CardId(600 + i),
                PlayerId(0),
                format!("Library {i}"),
                Zone::Library,
            );
        }

        let p0_life_before = state.players[0].life;
        let p0_hand_before = state.players[0].hand.len();

        // Resolve a "you gain 4 life" effect on Lich's controller.
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        // Life must NOT change — the original LifeGain event is suppressed.
        assert_eq!(
            state.players[0].life, p0_life_before,
            "Lich's controller must not gain life — replacement substitutes Draw"
        );
        // Hand must contain 4 additional cards — "draw that many cards instead".
        assert_eq!(
            state.players[0].hand.len(),
            p0_hand_before + 4,
            "Lich's controller must draw 4 cards (matching the prevented gain amount)"
        );
        // The post-replacement continuation must be drained.
        assert!(
            !state.has_post_replacement_drain(),
            "post_replacement_continuation must be drained after life-gain replacement"
        );
    }

    #[test]
    fn interactive_gain_life_substitution_defers_original_completion() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, PlayerFilter, ReplacementDefinition,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Interactive Lich".to_string(),
            Zone::Battlefield,
        );
        let branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::GainLife)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![branch],
                    kind: ChooseOneOfKind::Ordinary,
                },
            ))]
        .into();

        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(701),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::GainLife,
                    source_id: ObjectId(701),
                    ..
                }
            )),
            "the replaced GainLife must not complete before its substitute choice"
        );
    }

    #[test]
    fn apply_life_gain_reports_interactive_substitution_continuation() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, PlayerFilter, ReplacementDefinition,
        };
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(702),
            PlayerId(0),
            "Interactive Lich".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::GainLife)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: TargetFilter::Controller,
                        },
                    )],
                    kind: ChooseOneOfKind::Ordinary,
                },
            ))]
        .into();

        let outcome = apply_life_gain(&mut state, PlayerId(0), 4, &mut Vec::new());

        // The branch prompt fully REPLACES the gain, so the root added nothing
        // before pausing. The deferral says so, which is the point of carrying
        // the figure: a caller narrating this event must report what actually
        // happened (0), not the 4 that was proposed.
        assert_eq!(
            outcome,
            Err(ReplacementDeferred::SubstitutionContinuation { applied: 0 })
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ChooseOneOfBranch { .. }
        ));
    }

    /// Issue #317: A scaling-shape replacement (`Effect::GainLife { amount:
    /// Multiply { factor: 2, inner: EventContextAmount } }` — Boon Reflection
    /// shape) must still flow through Branch 2 of `gain_life_applier` and
    /// modify the amount — not be misclassified as substitution. This pins
    /// the boundary between "scaling" (same event type, modified amount) and
    /// "substitution" (different event type).
    #[test]
    fn gain_life_scaling_shape_modifies_amount_does_not_substitute() {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let doubler = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Boon Reflection".to_string(),
            Zone::Battlefield,
        );
        let replacement =
            ReplacementDefinition::new(ReplacementEvent::GainLife).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Multiply {
                        factor: 2,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: crate::types::ability::QuantityRef::EventContextAmount,
                        }),
                    },
                    player: TargetFilter::Controller,
                },
            ));
        state
            .objects
            .get_mut(&doubler)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        let p0_life_before = state.players[0].life;
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.players[0].life,
            p0_life_before + 6,
            "Boon Reflection doubles the gain to 6 — scaling, not substitution"
        );
        // CR 614.6: the applier substituted the amount; the `post_effect`
        // filter must suppress stashing the same GainLife execute as a
        // continuation, and the Execute-arm drain must clear any residual
        // template. A leaked Template here would drain on the next zone
        // change as a phantom GainLife — same defect class as Jace
        // empty-library win.
        assert!(
            !state.has_post_replacement_drain(),
            "GainLife→GainLife amount-substitution must not leak a post-replacement \
             continuation; found {:?}",
            state.post_replacement_continuation()
        );
    }

    #[test]
    fn gain_life_target_player_uses_declared_target() {
        // CR 115.1 + CR 119.3: "target player gains N life" — the gaining
        // player is the declared target in ability.targets, not the controller.
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 4 },
                player: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_gain(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.players[1].life, 24, "target player (p1) gains 4 life");
        assert_eq!(state.players[0].life, 20, "controller (p0) is unaffected");
    }

    /// Representability of the final life edit: one fallible core, an explicit
    /// policy per entry (ordinary skip vs. action-ingress rejection).
    mod representability {
        use super::*;
        use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::actions::GameAction;
        use crate::types::game_state::IngressLifeWindow;
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;

        fn scenario() -> GameScenario {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario
        }

        fn add_card(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
            scenario.add_real_card(
                player,
                name,
                Zone::Battlefield,
                crate::test_support::shared_card_db(),
            )
        }

        fn mana(color: ManaType) -> ManaUnit {
            ManaUnit::new(color, ObjectId(9_999), false, Vec::new())
        }

        fn life_changed_count(events: &[GameEvent]) -> usize {
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::LifeChanged { .. }))
                .count()
        }

        fn gain_life_effect_resolved(events: &[GameEvent]) -> bool {
            events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::GainLife,
                        ..
                    }
                )
            })
        }

        fn unrepresentable(player: PlayerId) -> LifeChangeError {
            LifeChangeError::Unrepresentable(LifeChangeUnrepresentable { player })
        }

        /// Outside an ingress window, ordinary-policy helpers use the
        /// compatibility skip (no mutation or `LifeChanged`); `try_*` entries
        /// report an unrepresentable result. One below the limit both apply. A
        /// nested window is restored untouched on close.
        #[test]
        fn ordinary_entries_skip_and_try_entries_reject_unrepresentable_results() {
            let mut state = GameState::new_two_player(42);
            let player = PlayerId(0);

            // Gain: life i32::MAX - 1, gain 2.
            state.players[0].life = i32::MAX - 1;
            let mut events = Vec::new();
            assert_eq!(apply_life_gain(&mut state, player, 2, &mut events), Ok(0));
            assert_eq!(state.players[0].life, i32::MAX - 1);
            assert_eq!(state.players[0].life_gained_this_turn, 0);
            assert_eq!(life_changed_count(&events), 0);
            assert_eq!(
                try_apply_life_gain(&mut state, player, 2, &mut events),
                Err(unrepresentable(player))
            );
            assert_eq!(state.players[0].life, i32::MAX - 1);
            assert_eq!(life_changed_count(&events), 0);
            assert_eq!(state.ingress_life_window, None, "the window closes");

            // Loss: life i32::MIN + 1, lose 2.
            state.players[0].life = i32::MIN + 1;
            assert_eq!(apply_life_loss(&mut state, player, 2, &mut events), Ok(0));
            assert_eq!(state.players[0].life, i32::MIN + 1);
            assert_eq!(state.players[0].life_lost_this_turn, 0);
            assert_eq!(
                try_apply_life_loss(&mut state, player, 2, &mut events),
                Err(unrepresentable(player))
            );
            assert_eq!(state.players[0].life, i32::MIN + 1);
            assert_eq!(life_changed_count(&events), 0);

            // An amount beyond the i32 delta is unrepresentable on its own.
            state.players[0].life = 0;
            assert_eq!(
                apply_life_gain(&mut state, player, u32::MAX, &mut events),
                Ok(0)
            );
            assert_eq!(
                try_apply_life_gain(&mut state, player, u32::MAX, &mut events),
                Err(unrepresentable(player))
            );
            assert_eq!(state.players[0].life, 0);

            // One below the limit both apply.
            state.players[0].life = i32::MAX - 2;
            assert_eq!(apply_life_gain(&mut state, player, 2, &mut events), Ok(2));
            assert_eq!(state.players[0].life, i32::MAX);
            state.players[0].life = i32::MAX - 2;
            assert_eq!(
                try_apply_life_gain(&mut state, player, 2, &mut events),
                Ok(2)
            );
            assert_eq!(state.players[0].life, i32::MAX);
            assert_eq!(life_changed_count(&events), 2);
        }

        /// Windows nest: a `try_*` entry opened inside an outer window reports
        /// only to its own caller and restores the outer window exactly.
        #[test]
        fn try_entry_restores_the_outer_ingress_window() {
            let mut state = GameState::new_two_player(42);
            let player = PlayerId(0);
            let outer = Some(IngressLifeWindow {
                failed_player: Some(PlayerId(1)),
            });
            let mut events = Vec::new();

            state.ingress_life_window = outer;
            state.players[0].life = i32::MAX - 1;
            assert_eq!(
                try_apply_life_gain(&mut state, player, 2, &mut events),
                Err(unrepresentable(player))
            );
            assert_eq!(
                state.ingress_life_window, outer,
                "a failing inner window must leave the outer one untouched"
            );

            state.players[0].life = i32::MAX - 2;
            assert_eq!(
                try_apply_life_gain(&mut state, player, 2, &mut events),
                Ok(2)
            );
            assert_eq!(
                state.ingress_life_window, outer,
                "a representable inner call must restore the outer window exactly"
            );
        }

        /// Inside an open ingress window an ordinary entry records its failure
        /// for the ingress caller instead of skipping silently, and once a failure
        /// is recorded no later settlement in that window edits life.
        #[test]
        fn ordinary_entry_inside_an_ingress_window_records_the_failure() {
            let mut state = GameState::new_two_player(42);
            let player = PlayerId(0);
            let mut events = Vec::new();
            state.players[0].life = i32::MAX - 1;

            let (results, window) = within_ingress_life_window(&mut state, |state| {
                let failing = apply_life_gain(state, player, 2, &mut events);
                // Representable on its own, but the window already failed.
                let later = apply_life_gain(state, player, 1, &mut events);
                (failing, later)
            });

            assert_eq!(results, (Ok(0), Ok(0)));
            assert_eq!(
                window,
                IngressLifeWindow {
                    failed_player: Some(player)
                }
            );
            assert_eq!(state.players[0].life, i32::MAX - 1);
            assert_eq!(life_changed_count(&events), 0);
            assert_eq!(state.ingress_life_window, None, "the window closes");
        }

        /// Activates Lochmere Serpent's "{B}, Sacrifice a Swamp: You gain 1 life
        /// and draw a card." with `doublers` in play, returning the runner, the
        /// activation's events and P0's hand size before it.
        fn activate_serpent_gain(
            start_life: i32,
            doublers: &[&str],
        ) -> (GameRunner, Result<Vec<GameEvent>, EngineError>, usize) {
            let mut scenario = scenario();
            scenario.with_life(P0, start_life);
            let serpent = add_card(&mut scenario, P0, "Lochmere Serpent");
            for doubler in doublers {
                add_card(&mut scenario, P0, doubler);
            }
            let sacrificed = add_card(&mut scenario, P0, "Swamp");
            add_card(&mut scenario, P0, "Swamp");
            scenario.with_mana_pool(P0, vec![mana(ManaType::Black)]);
            scenario.with_library_top(P0, &["Filler One", "Filler Two"]);
            let mut runner = scenario.build();
            let hand_before = runner.state().players[0].hand.len();
            let events = runner
                .activate(serpent, 1)
                .pay_with(&[sacrificed])
                .try_resolve()
                .map(|outcome| outcome.events().to_vec());
            (runner, events, hand_before)
        }

        /// A public resolution whose gain exceeds engine capacity fails
        /// explicitly: the action is rejected and restored, so neither the gain
        /// nor the rest of the instruction ("draw a card") commits.
        #[test]
        fn public_resolution_rejects_the_unrepresentable_gain_and_rolls_back() {
            // Control: one below the limit — 1 doubled to 2 reaches i32::MAX.
            let (runner, events, hand_before) =
                activate_serpent_gain(i32::MAX - 2, &["Rhox Faithmender"]);
            let events = events.expect("the control activation must resolve");
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
                "reach guard: the control activation must settle, got {:?}",
                runner.state().waiting_for
            );
            assert_eq!(runner.life(P0), i32::MAX);
            assert_eq!(runner.state().players[0].life_gained_this_turn, 2);
            assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
            assert!(gain_life_effect_resolved(&events));

            // Engine-unrepresentable: i32::MAX - 1 + 2 overflows; the public
            // action is rejected, so neither the gain nor the draw commits.
            let (runner, events, hand_before) =
                activate_serpent_gain(i32::MAX - 1, &["Rhox Faithmender"]);
            let error = events.expect_err("the unrepresentable gain must reject the action");
            assert!(
                matches!(
                    error,
                    EngineError::LifeChangeUnrepresentable { player } if player == P0
                ),
                "expected the typed unrepresentable-life rejection, got {error:?}"
            );
            assert_eq!(runner.life(P0), i32::MAX - 1);
            assert_eq!(runner.state().players[0].life_gained_this_turn, 0);
            assert_eq!(
                runner.state().players[0].hand.len(),
                hand_before,
                "the rejected resolution must not run the draw"
            );
            assert_eq!(
                runner.state().stack.len(),
                1,
                "the rejected resolution leaves the ability on the stack"
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
        }

        /// A parked life event that resumes beyond engine capacity rejects the
        /// answering public action and restores its prompt; it never commits the
        /// gain or the rest of the instruction.
        #[test]
        fn parked_gain_rejects_unrepresentable_resume_and_keeps_the_prompt() {
            let doublers = ["Rhox Faithmender", "Boon Reflection"];
            for (start_life, applies) in [(i32::MAX - 4, true), (i32::MAX - 3, false)] {
                let (mut runner, result, hand_before) =
                    activate_serpent_gain(start_life, &doublers);
                result.expect("the activation must stop at the ordering prompt");
                assert!(
                    matches!(
                        runner.state().waiting_for,
                        WaitingFor::ReplacementChoice { .. }
                    ),
                    "reach guard: two doublers must park an ordering prompt, got {:?}",
                    runner.state().waiting_for
                );
                assert_eq!(
                    runner
                        .state()
                        .pending_replacement
                        .as_ref()
                        .map(|pending| pending.unrepresentable_life_change),
                    Some(UnrepresentablePolicy::Reject),
                    "a gain parked inside the activation's action window rejects on resume"
                );
                let mut events = Vec::new();
                let mut rejected = None;
                for _ in 0..4 {
                    if !matches!(
                        runner.state().waiting_for,
                        WaitingFor::ReplacementChoice { .. }
                    ) {
                        break;
                    }
                    match runner.act(GameAction::ChooseReplacement { index: 0 }) {
                        Ok(result) => events.extend(result.events),
                        Err(error) => {
                            rejected = Some(error);
                            break;
                        }
                    }
                }
                if applies {
                    assert!(
                        rejected.is_none(),
                        "the representable resume must be accepted: {rejected:?}"
                    );
                    assert!(matches!(
                        runner.state().waiting_for,
                        WaitingFor::Priority { .. }
                    ));
                    assert_eq!(
                        runner.state().players[0].hand.len(),
                        hand_before + 1,
                        "the draw after the gain must run (start life {start_life})"
                    );
                    assert_eq!(runner.life(P0), i32::MAX, "1 doubled twice is 4");
                    assert_eq!(life_changed_count(&events), 1);
                } else {
                    let error =
                        rejected.expect("the unrepresentable resume must reject its answer");
                    assert!(
                        matches!(
                            error,
                            EngineError::LifeChangeUnrepresentable { player } if player == P0
                        ),
                        "expected the typed unrepresentable-life rejection, got {error:?}"
                    );
                    assert!(
                        matches!(
                            runner.state().waiting_for,
                            WaitingFor::ReplacementChoice { .. }
                        ),
                        "the rejected answer must restore its prompt, got {:?}",
                        runner.state().waiting_for
                    );
                    assert_eq!(runner.life(P0), start_life);
                    assert_eq!(runner.state().players[0].life_gained_this_turn, 0);
                    assert_eq!(runner.state().players[0].hand.len(), hand_before);
                    assert_eq!(life_changed_count(&events), 0);
                }
            }
        }

        /// Casts Dazzling Reflection ("You gain life equal to target creature's
        /// power. The next time that creature would deal damage this turn,
        /// prevent that damage.") on a P0 creature of `power` with `doublers` in
        /// play; any ordering prompt is answered with index 0 and must be
        /// accepted. Returns the runner, the cast's events and the spell.
        fn cast_dazzling_reflection(
            power: i32,
            doublers: &[&str],
        ) -> (GameRunner, Result<Vec<GameEvent>, EngineError>, ObjectId) {
            cast_dazzling_reflection_answering(power, doublers, Some(0))
        }

        /// As [`cast_dazzling_reflection`], with the ordering answer chosen by the
        /// caller: `None` halts the cast at the CR 616.1 prompt instead.
        fn cast_dazzling_reflection_answering(
            power: i32,
            doublers: &[&str],
            replacement_choice: Option<usize>,
        ) -> (GameRunner, Result<Vec<GameEvent>, EngineError>, ObjectId) {
            let mut scenario = scenario();
            for doubler in doublers {
                add_card(&mut scenario, P0, doubler);
            }
            let creature = scenario.add_creature(P0, "Test Behemoth", power, 1).id();
            let spell = scenario.add_real_card(
                P0,
                "Dazzling Reflection",
                Zone::Hand,
                crate::test_support::shared_card_db(),
            );
            scenario.with_mana_pool(P0, vec![mana(ManaType::White), mana(ManaType::White)]);
            let mut runner = scenario.build();
            let cast = runner
                .cast(spell)
                // Both of the spell's target slots name the same creature; with
                // Rhox Faithmender also on the battlefield the second slot has two
                // legal creatures, so it must be declared explicitly.
                .target_object(creature)
                .target_object(creature);
            let events = match replacement_choice {
                Some(index) => cast.replacement_choice(index),
                None => cast,
            }
            .try_resolve()
            .map(|outcome| outcome.events().to_vec());
            (runner, events, spell)
        }

        fn prevent_damage_resolved(events: &[GameEvent]) -> bool {
            events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::PreventDamage,
                        ..
                    }
                )
            })
        }

        /// A replacement-quantity overflow (Boon Reflection doubling
        /// 1,100,000,000 to 2,200,000,000, past `i32::MAX`) rejects the resolving
        /// action: the whole spell rolls back, so the gain, the prevention, and
        /// the graveyard move all wait for an explicit caller decision.
        #[test]
        fn ordinary_replacement_quantity_overflow_rejects_the_resolution() {
            // Control A: no doubler — the whole power is gained.
            let (runner, events, _) = cast_dazzling_reflection(1_100_000_000, &[]);
            let events = events.expect("the representable cast must resolve");
            assert_eq!(runner.life(P0), 1_100_000_020);
            assert!(prevent_damage_resolved(&events));

            // Control B: Boon Reflection is live and applies when representable.
            let (runner, events, _) = cast_dazzling_reflection(500_000_000, &["Boon Reflection"]);
            let events = events.expect("the representable cast must resolve");
            assert_eq!(runner.life(P0), 1_000_000_020);
            assert!(prevent_damage_resolved(&events));

            // The overflow: the resolving action is rejected and restored whole.
            let (runner, events, spell) =
                cast_dazzling_reflection(1_100_000_000, &["Boon Reflection"]);
            let error = events.expect_err("the overflow must reject the resolution");
            assert!(
                matches!(
                    error,
                    EngineError::LifeChangeUnrepresentable { player } if player == P0
                ),
                "expected the typed unrepresentable-life rejection, got {error:?}"
            );
            assert_eq!(runner.life(P0), 20);
            assert_eq!(runner.state().players[0].life_gained_this_turn, 0);
            assert!(
                !runner.state().players[0].graveyard.contains(&spell),
                "the rejected resolution leaves the spell on the stack"
            );
            assert_eq!(runner.state().stack.len(), 1);
            assert_eq!(
                runner.state().replacement_quantity_overflow,
                None,
                "the rejected action restores the pre-resolution record"
            );
        }

        /// A choice-resumed ordinary delivery whose later answer overflows
        /// rejects the answering action and restores its ordering prompt.
        #[test]
        fn choice_resumed_ordinary_quantity_overflow_rejects_the_answer() {
            // Reach guard: the same setup, left unanswered, halts at the CR 616.1
            // ordering prompt for the two doublers, so the answer below is a real
            // prompt answer rather than a no-prompt degenerate path.
            let (prompted, _, _) = cast_dazzling_reflection_answering(
                600_000_000,
                &["Rhox Faithmender", "Boon Reflection"],
                None,
            );
            assert!(
                matches!(
                    prompted.state().waiting_for,
                    WaitingFor::ReplacementChoice {
                        candidate_count: 2,
                        ..
                    }
                ),
                "reach guard: two doublers must park an ordering prompt, got {:?}",
                prompted.state().waiting_for
            );

            // 600,000,000 doubled is 1,200,000,000; doubled again overflows i32.
            let (runner, events, spell) =
                cast_dazzling_reflection(600_000_000, &["Rhox Faithmender", "Boon Reflection"]);
            let error = events.expect_err("the overflow must reject the answer");
            assert!(
                matches!(
                    error,
                    EngineError::LifeChangeUnrepresentable { player } if player == P0
                ),
                "expected the typed unrepresentable-life rejection, got {error:?}"
            );
            assert_eq!(runner.life(P0), 20);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::ReplacementChoice { .. }
            ));
            assert!(!runner.state().players[0].graveyard.contains(&spell));
            assert_eq!(runner.state().replacement_quantity_overflow, None);
        }

        /// The settle steps take a recorded overflow under each policy.
        #[test]
        fn settle_takes_the_replacement_quantity_overflow_under_each_policy() {
            let player = PlayerId(0);
            let source = ObjectId(1);
            let gain_event = || ProposedEvent::LifeGain {
                player_id: player,
                amount: 3,
                applied: HashSet::new(),
            };
            let loss_event = || ProposedEvent::LifeLoss {
                player_id: player,
                amount: 3,
                applied: HashSet::new(),
            };
            let mut events = Vec::new();

            // Ordinary entries: the capacity-exceeding change is skipped by the
            // compatibility fallback outside a public action boundary.
            let mut state = GameState::new_two_player(42);
            state.record_replacement_quantity_overflow(source);
            assert_eq!(apply_life_gain(&mut state, player, 3, &mut events), Ok(0));
            assert_eq!(state.replacement_quantity_overflow, None);
            state.record_replacement_quantity_overflow(source);
            assert_eq!(apply_life_loss(&mut state, player, 3, &mut events), Ok(0));
            assert_eq!(state.replacement_quantity_overflow, None);
            assert_eq!(state.players[0].life, 20);
            assert_eq!(state.players[0].life_gained_this_turn, 0);
            assert_eq!(state.players[0].life_lost_this_turn, 0);
            assert_eq!(life_changed_count(&events), 0);

            // Reject policy, through the ingress entries: the loop head stops at
            // the pre-set record and the settle step reports the error.
            state.record_replacement_quantity_overflow(source);
            assert_eq!(
                try_apply_life_gain(&mut state, player, 3, &mut events),
                Err(unrepresentable(player))
            );
            assert_eq!(state.replacement_quantity_overflow, None);
            state.record_replacement_quantity_overflow(source);
            assert_eq!(
                try_apply_life_loss(&mut state, player, 3, &mut events),
                Err(unrepresentable(player))
            );
            assert_eq!(state.replacement_quantity_overflow, None);

            // Reject policy, resume path.
            state.record_replacement_quantity_overflow(source);
            assert_eq!(
                settle_life_gain_after_choice(
                    &mut state,
                    gain_event(),
                    UnrepresentablePolicy::Reject,
                    &mut events
                ),
                Err(LifeChangeUnrepresentable { player })
            );
            assert_eq!(state.replacement_quantity_overflow, None);
            state.record_replacement_quantity_overflow(source);
            assert_eq!(
                settle_life_loss_after_choice(
                    &mut state,
                    loss_event(),
                    UnrepresentablePolicy::Reject,
                    &mut events
                ),
                Err(LifeChangeUnrepresentable { player })
            );
            assert_eq!(state.replacement_quantity_overflow, None);

            // Inside an open window an ordinary entry records the failure.
            state.record_replacement_quantity_overflow(source);
            let (result, window) = within_ingress_life_window(&mut state, |state| {
                apply_life_gain(state, player, 3, &mut events)
            });
            assert_eq!(result, Ok(0));
            assert_eq!(window.failed_player, Some(player));
            assert_eq!(state.replacement_quantity_overflow, None);
            state.record_replacement_quantity_overflow(source);
            let (result, window) = within_ingress_life_window(&mut state, |state| {
                apply_life_loss(state, player, 3, &mut events)
            });
            assert_eq!(result, Ok(0));
            assert_eq!(window.failed_player, Some(player));
            assert_eq!(state.replacement_quantity_overflow, None);

            // A window that already failed still has the record taken.
            state.ingress_life_window = Some(IngressLifeWindow {
                failed_player: Some(PlayerId(1)),
            });
            state.record_replacement_quantity_overflow(source);
            assert_eq!(apply_life_gain(&mut state, player, 3, &mut events), Ok(0));
            assert_eq!(state.replacement_quantity_overflow, None);
            state.record_replacement_quantity_overflow(source);
            assert_eq!(apply_life_loss(&mut state, player, 3, &mut events), Ok(0));
            assert_eq!(state.replacement_quantity_overflow, None);
            state.ingress_life_window = None;

            // An amount of 0 never runs the pipeline: the record is untouched.
            state.record_replacement_quantity_overflow(source);
            assert_eq!(apply_life_gain(&mut state, player, 0, &mut events), Ok(0));
            assert_eq!(
                state.replacement_quantity_overflow,
                Some(crate::types::game_state::ReplacementQuantityOverflow { source })
            );
            assert_eq!(life_changed_count(&events), 0);
        }

        /// CR 119.3: an ordinary change that is representable still applies in
        /// full when it is lethal (effect and damage paths), so the shared edit's
        /// fallibility never turns a legal lethal loss into a skip.
        #[test]
        fn lethal_life_loss_still_applies_through_effect_and_damage_paths() {
            // Sign in Blood: "Target player draws two cards and loses 2 life."
            let mut scenario = scenario();
            scenario.with_life(P1, 2);
            scenario.with_library_top(P1, &["Filler One", "Filler Two", "Filler Three"]);
            let sign = scenario.add_real_card(
                P0,
                "Sign in Blood",
                Zone::Hand,
                crate::test_support::shared_card_db(),
            );
            scenario.with_mana_pool(P0, vec![mana(ManaType::Black), mana(ManaType::Black)]);
            let mut runner = scenario.build();
            let outcome = runner.cast(sign).target_player(P1).resolve();
            // CR 800.4a: the lethal loss eliminates P1 at the boundary and their
            // hand leaves the game with them, so the draw is read from its events
            // (the final hand size is 0 either way). Draw precedes the loss.
            let drawn = outcome
                .events()
                .iter()
                .filter(|event| {
                    matches!(event, GameEvent::CardDrawn { player_id, .. } if *player_id == P1)
                })
                .count();
            assert_eq!(
                drawn, 2,
                "reach guard: the draw half must run before the loss"
            );
            outcome.assert_life_delta(P1, -2);
            assert_eq!(runner.life(P1), 0);
            assert!(
                matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
                "P1 at 0 life must lose to state-based actions, got {:?}",
                runner.state().waiting_for
            );

            // Lightning Bolt: "Lightning Bolt deals 3 damage to any target."
            let mut scenario = self::scenario();
            scenario.with_life(P1, 3);
            let bolt = scenario.add_real_card(
                P0,
                "Lightning Bolt",
                Zone::Hand,
                crate::test_support::shared_card_db(),
            );
            scenario.with_mana_pool(P0, vec![mana(ManaType::Red)]);
            let mut runner = scenario.build();
            let outcome = runner.cast(bolt).target_player(P1).resolve();
            outcome.assert_zone(&[bolt], Zone::Graveyard);
            outcome.assert_life_delta(P1, -3);
            assert_eq!(runner.life(P1), 0);
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::GameOver { .. }
            ));
        }

        /// The sibling difference helper: an `i32` overflow is the receiver's
        /// unrepresentable change and reports `None` (never a substitute zero), while
        /// a representable difference — including `i32::MIN` — passes through.
        #[test]
        fn life_total_difference_reports_overflow_as_failure() {
            let mut state = GameState::new_two_player(42);
            assert_eq!(
                life_total_difference(&mut state, P0, 7, 20, "test"),
                Some(-13)
            );
            assert_eq!(
                life_total_difference(&mut state, P0, 20, 7, "test"),
                Some(13)
            );
            assert_eq!(
                life_total_difference(&mut state, P0, i32::MAX, -1, "test"),
                None
            );
            assert_eq!(
                life_total_difference(&mut state, P0, i32::MIN, 1, "test"),
                None
            );
            assert_eq!(
                life_total_difference(&mut state, P0, i32::MIN, 0, "test")
                    .expect("i32::MIN stays representable as a difference")
                    .unsigned_abs(),
                1 << 31,
                "its magnitude needs unsigned_abs"
            );
        }
    }
}
