//! Replayable, staged resolution-time composite payments.
//!
//! The descriptor in [`GameState::payment_transaction`] is the only durable
//! transaction authority. It stores the untouched base implicitly (the live
//! state) plus a serializable resolution root and the actions already accepted
//! against its shadow. Every materialization starts from that base and runs the
//! normal resolution/action pipeline; no second payment implementation lives
//! here.
//!
//! A staged root is a fresh payment choice at `begin` and a latched re-entry on
//! every replay (CR 118.12). An outside action that changes the base keeps the
//! accepted prefix and its cursor in a hidden recovery checkpoint.

use crate::game::effects;
use crate::game::engine::{self, EngineError};
use crate::game::{engine_priority, triggers};
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, GameState, PaymentRecoveryCheckpoint, ResolutionPaymentOrigin,
    ResolutionPaymentTransaction, ResolutionPaymentTranscriptEntry, ResolvingTriggerContext,
    WaitingFor,
};
use crate::types::log::GameLogEntry;
use crate::types::player::PlayerId;

#[derive(Debug)]
enum ReplayOutcome {
    Paused {
        shadow: GameState,
        /// Events emitted by the accepted prefix. Discarded on an ordinary
        /// re-materialization (the next replay regenerates them), but published
        /// when an outside action turns this paused prefix into the committed
        /// base (CR 614.17a + CR 118.12).
        events: Vec<GameEvent>,
        log_entries: Vec<GameLogEntry>,
    },
    Completed {
        shadow: GameState,
        events: Vec<GameEvent>,
        log_entries: Vec<GameLogEntry>,
    },
    Failed {
        /// The life-change failure the shadow recorded before the payment
        /// failed ([`recorded_failure`]).
        recorded_failure: Option<PlayerId>,
    },
}

fn replay_error(error: EffectError) -> EngineError {
    EngineError::InvalidAction(format!(
        "staged payment transaction replay failed: {error:?}"
    ))
}

/// The transaction boundary owns only the Composite PayCost instruction, already
/// admitted by its condition. All outgoing chain edges stay on the durable root
/// so the ordinary chain walker can resume them after the payment commits or
/// aborts.
fn payment_only_root(ability: &ResolvedAbility) -> ResolvedAbility {
    let mut payment = ability.clone();
    payment.sub_ability = None;
    payment.else_ability = None;
    // CR 608.2c + CR 118.12: the chain walker evaluated this instruction's
    // condition, in its own resolution context, before `pay::resolve` opened the
    // transaction; that evaluation is the fresh one. The shadow re-materializes
    // only the payment and must not ask again: at `begin` its depth-0 entry has
    // reset the resolution-local state the condition may read, and on `replay`
    // canonical state may have changed through an outside-transaction action
    // such as another player's concession (CR 104.3a + CR 800.4a). A false
    // verdict there would skip the payment and settle the choice as paid, or
    // strand the recorded answers.
    payment.condition = None;
    payment
}

fn restore_trigger_context(state: &mut GameState, context: Option<&ResolvingTriggerContext>) {
    let Some(context) = context else {
        return;
    };
    state.current_trigger_event = context.event.clone();
    state.current_trigger_events = context.events.clone();
    state.current_trigger_match_count = context.match_count;
    state.die_result_this_resolution = context.die_result;
}

fn returned_to_base_waiting(
    state: &GameState,
    base_waiting_for: &crate::types::game_state::WaitingFor,
) -> bool {
    if state.waiting_for == *base_waiting_for {
        return true;
    }
    // A recorded answer is submitted by the authorized controller, so the
    // ordinary priority handoff may name that submitter rather than the seat
    // that held priority before the resolution began. Both are the same settled
    // priority boundary; a live continuation is the discriminator for a still-
    // paused payment.
    matches!(
        (&state.waiting_for, base_waiting_for),
        (
            crate::types::game_state::WaitingFor::Priority { .. },
            crate::types::game_state::WaitingFor::Priority { .. }
        )
    ) && state.active_ability_continuation().is_none()
}

/// Begin a staged transaction from the resolution-time `PayCost` authority.
///
/// The caller has not yet committed any payment mutation. The root is replayed
/// on a shadow with the transient staging guard set, so the shadow uses the
/// ordinary payment authority and cannot recursively create another descriptor.
/// The root is judged fresh here, once; `replay` re-materializes it latched.
pub(crate) fn begin(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let base = state.clone();
    let resolving_trigger_context = ResolvingTriggerContext::capture(&base);
    let mut shadow = base.clone();
    shadow.payment_transaction = None;
    shadow.payment_transaction_replay = true;
    shadow.payment_transaction_just_handled = false;
    let mut buffered_events = Vec::new();
    let payment_root = payment_only_root(ability);
    effects::resolve_ability_chain(&mut shadow, &payment_root, &mut buffered_events, 0)?;
    restore_trigger_context(&mut shadow, resolving_trigger_context.as_ref());
    shadow.payment_transaction_replay = false;

    // A failed composite owns no partial payment and produces no payment events.
    // Keep the canonical state byte-for-byte at the pre-payment base.
    if shadow.cost_payment_failed_flag {
        *state = base;
        record_failure(state, recorded_failure(&shadow));
        state.cost_payment_failed_flag = true;
        // No shadow was committed, so the ordinary chain walker must still
        // descend into the printed abort/independent continuation. A success
        // or pause owns the complete root; a failure owns only the payment
        // clause and leaves the generic CR 608.2c tail authority live.
        state.payment_transaction_just_handled = false;
        return Ok(());
    }

    // A synchronous success can commit the payment shadow atomically. Leave the
    // transient handoff flag clear so the *ordinary* chain walker continues into
    // the printed rider outside this transaction boundary.
    if shadow.waiting_for == base.waiting_for {
        shadow.payment_transaction = None;
        shadow.payment_transaction_replay = false;
        shadow.payment_transaction_just_handled = false;
        *state = shadow;
        events.extend(buffered_events);
        return Ok(());
    }

    // An interactive choice paused the shadow. Persist only the replayable
    // descriptor and the public projected prompt; all resource/object changes
    // stay out of the canonical state until a later replay commits.
    let owner = shadow
        .waiting_for
        .acting_players()
        .first()
        .copied()
        .unwrap_or(ability.controller);
    let transaction = ResolutionPaymentTransaction {
        wire_version: 1,
        owner,
        root: Box::new(ability.clone()),
        transcript: Vec::new(),
        base_waiting_for: base.waiting_for.clone(),
        resolving_trigger_context,
        recovered: None,
    };
    *state = base;
    record_failure(state, recorded_failure(&shadow));
    state.payment_transaction = Some(Box::new(transaction));
    state.waiting_for = shadow.waiting_for;
    state.cost_payment_failed_flag = false;
    state.payment_transaction_replay = false;
    state.payment_transaction_just_handled = true;
    Ok(())
}

/// The life, player-counter or energy change a payment shadow could not
/// represent in this action, if it recorded one.
fn recorded_failure(shadow: &GameState) -> Option<PlayerId> {
    shadow
        .ingress_life_window
        .and_then(|window| window.failed_player)
}

/// Record `failure` in `state`'s open ingress window, where the action boundary
/// rejects it (a concession instead retires the payment). Every outcome of a
/// payment shadow carries what it recorded: completed, paused or failed, the
/// shadow's own state is discarded or replaces the live one, and a failure
/// recorded in it must not be discarded with it. A shadow records a failure
/// only inside an action's window (outside one there is no window and the
/// change keeps its compatibility skip); `replay` reopens the shadow's window
/// after the transcript answers only to hold such a failure.
fn record_failure(state: &mut GameState, failure: Option<PlayerId>) {
    let (Some(player), Some(window)) = (failure, state.ingress_life_window.as_mut()) else {
        return;
    };
    window.failed_player.get_or_insert(player);
}

fn replay(
    state: &GameState,
    transaction: &ResolutionPaymentTransaction,
) -> Result<ReplayOutcome, EngineError> {
    // CR 614.17a + CR 118.12: a recovery checkpoint is the accepted prefix and
    // its open cursor on the departure-adjusted base. Replaying the root from
    // the canonical base again would re-execute work the player already
    // completed, so the checkpoint is the authority while it exists.
    let (mut shadow, mut buffered_events, mut buffered_log_entries, consumed) =
        if let Some(checkpoint) = transaction.recovered.as_deref() {
            if checkpoint.consumed > transaction.transcript.len() {
                return Err(EngineError::InvalidAction(
                    "recovery checkpoint consumed beyond its transcript".to_string(),
                ));
            }
            let mut shadow = hydrated_checkpoint(state, checkpoint);
            shadow.payment_transaction_replay = true;
            restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
            (
                shadow,
                checkpoint.events.clone(),
                checkpoint.log_entries.clone(),
                checkpoint.consumed.min(transaction.transcript.len()),
            )
        } else {
            let mut shadow = state.clone();
            shadow.payment_transaction = None;
            shadow.payment_transaction_replay = true;
            shadow.payment_transaction_just_handled = false;
            shadow.cost_payment_failed_flag = false;
            shadow.waiting_for = transaction.base_waiting_for.clone();
            restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());

            let mut buffered = Vec::new();
            let payment_root = payment_only_root(&transaction.root);
            // CR 118.12 + CR 614.17a: `begin` judged the root's choice fresh; every
            // replay re-materializes that choice against canonical state, which
            // outside-transaction actions such as another player's concession
            // (CR 104.3a + CR 800.4a) can change. Re-gating would unpay a choice
            // already made, so the root pays through the latched entry. Each event
            // is still clamped as it happens.
            shadow.resolving_head_payment_origin = ResolutionPaymentOrigin::LatchedSuffix;
            let root_outcome =
                effects::resolve_ability_chain(&mut shadow, &payment_root, &mut buffered, 0);
            shadow.resolving_head_payment_origin = ResolutionPaymentOrigin::FreshChoice;
            root_outcome.map_err(replay_error)?;
            restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
            // Keep the replay guard live while transcript actions drain the
            // suspended continuation. A remaining Composite must use the ordinary
            // sequential shadow authority, never open a nested descriptor of its own.
            shadow.payment_transaction_replay = true;

            if shadow.cost_payment_failed_flag {
                return Ok(ReplayOutcome::Failed {
                    recorded_failure: recorded_failure(&shadow),
                });
            }

            if !transaction.transcript.is_empty() {
                // CR 603.2c + CR 603.3b: initial direct emissions have no ActionResult
                // scan; collect them once while their claims and owners are live.
                triggers::collect_paused_exit_triggers(&mut shadow, &buffered, 0);
                // CR 603.2c: these coordinates name only the initial buffer.
                // Recorded actions own distinct inner buffers, so the closed
                // claims end here.
                shadow.consumed_before_priority_trigger_events.clear();
            }
            (shadow, buffered, Vec::new(), 0usize)
        };
    shadow.payment_transaction_replay = true;
    // Each transcript answer runs in an action boundary of its own, whose
    // transient cleanup closes the window the prefix recorded in; a failure the
    // prefix recorded is carried past them.
    let prefix_failure = recorded_failure(&shadow);

    for entry in transaction.transcript.iter().skip(consumed) {
        restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
        let result = engine::apply_admitted_recorded_action(
            &mut shadow,
            entry.authenticated_actor,
            entry.semantic_owner,
            entry.action.clone(),
        )?;
        buffered_events.extend(result.events);
        buffered_log_entries.extend(result.log_entries);
        if shadow.cost_payment_failed_flag {
            return Ok(ReplayOutcome::Failed {
                recorded_failure: prefix_failure,
            });
        }
        shadow.payment_transaction_replay = true;
        restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
    }

    shadow.payment_transaction_replay = false;
    if let Some(player) = prefix_failure {
        shadow
            .ingress_life_window
            .get_or_insert_with(Default::default)
            .failed_player
            .get_or_insert(player);
    }

    if returned_to_base_waiting(&shadow, &transaction.base_waiting_for) {
        Ok(ReplayOutcome::Completed {
            shadow,
            events: buffered_events,
            log_entries: buffered_log_entries,
        })
    } else {
        Ok(ReplayOutcome::Paused {
            shadow,
            events: buffered_events,
            log_entries: buffered_log_entries,
        })
    }
}

/// CR 608.2c + CR 117.3b + CR 603.3b: the canonical tail and all outer
/// frames complete before this caller hands the whole due batch to settlement.
fn settle_canonical_tail(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    carrier: Option<crate::types::identifiers::ObjectId>,
) -> Result<(), EngineError> {
    let Some(carrier) = carrier else {
        return Ok(());
    };
    engine::resume_pending_continuation_if_priority(state, events)?;
    let completed = Some(carrier).filter(|_| {
        state.resolving_stack_entry.is_none()
            && matches!(
                state.waiting_for,
                crate::types::game_state::WaitingFor::Priority { .. }
            )
            && state.game_end.is_none()
            && state.payment_transaction.is_none()
            && !state.payment_transaction_replay
            && !triggers::is_pending_trigger_construction_active(state)
    });
    let default_wf = state.waiting_for.clone();
    let waiting_for = engine_priority::run_post_action_pipeline_for_canonical_tail(
        state,
        events,
        event_start,
        &default_wf,
        completed,
    )?;
    crate::game::public_state::sync_waiting_for(state, &waiting_for);
    retire_terminal_transaction(state);
    Ok(())
}

/// Resume only the printed continuation after a staged payment boundary.
///
/// The replay shadow has already evaluated the payment root and is discarded
/// on failure. Re-entering the original root would pay/choose again, so hand
/// the root's first child to the ordinary chain walker with the canonical
/// payment-failure signal. Its existing condition/sibling logic then suppresses
/// IfYouDo/WhenYouDo clauses and executes unconditional sequential siblings.
fn resolve_root_continuation(
    state: &mut GameState,
    root: &ResolvedAbility,
    payment_succeeded: bool,
    resolving_trigger_context: Option<&ResolvingTriggerContext>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let Some(sub) = root.sub_ability.as_deref() else {
        return Ok(());
    };
    // The ordinary parent-to-child handoff records whether this PayCost
    // instruction actually performed. A staged replay evaluates the payment
    // root without its rider, so carry the same outcome bit onto the synthetic
    // parent context before re-entering the existing chain authority. The
    // child still owns all SubAbilityLink/condition/else evaluation.
    let mut parent = root.clone();
    parent.context.optional_effect_performed |= payment_succeeded;
    let mut continuation = sub.clone();
    effects::apply_parent_chain_context(&mut continuation, &parent, None, state);
    restore_trigger_context(state, resolving_trigger_context);
    effects::resolve_ability_chain(state, &continuation, events, 1).map_err(replay_error)
}

/// Apply one player action against the live transaction's replayed shadow.
///
/// Pause keeps the canonical base and extends the transcript. Completion swaps
/// in the materialized shadow and returns the complete buffered event stream.
/// Failure aborts the descriptor and restores its base without publishing any
/// payment events.
pub(crate) fn apply_pending_action(
    state: &mut GameState,
    actor: crate::types::player::PlayerId,
    action: GameAction,
) -> Result<ActionResult, EngineError> {
    let mut transaction = state
        .payment_transaction
        .as_ref()
        .ok_or_else(|| EngineError::InvalidAction("no staged payment transaction".to_string()))?
        .as_ref()
        .clone();
    let semantic_owner = state
        .waiting_for
        .acting_players()
        .first()
        .copied()
        .unwrap_or(transaction.owner);
    transaction
        .transcript
        .push(ResolutionPaymentTranscriptEntry {
            authenticated_actor: actor,
            semantic_owner,
            action,
        });

    let outcome = replay(state, &transaction)?;
    settle_replay_outcome(state, transaction, outcome)
}

/// Settle one materialization of the staged payment. Pause keeps the canonical
/// base and stores `transaction` (with its transcript) and its current prompt.
/// Completion swaps in the materialized shadow and returns the complete
/// buffered event stream. Failure aborts the descriptor and restores its base
/// without publishing any payment events.
fn settle_replay_outcome(
    state: &mut GameState,
    transaction: ResolutionPaymentTransaction,
    outcome: ReplayOutcome,
) -> Result<ActionResult, EngineError> {
    match outcome {
        ReplayOutcome::Failed { recorded_failure } => {
            record_failure(state, recorded_failure);
            let mut restored = state.clone();
            restored.payment_transaction = None;
            restored.payment_transaction_replay = false;
            restored.payment_transaction_just_handled = false;
            restored.waiting_for = transaction.base_waiting_for;
            restored.cost_payment_failed_flag = true;
            *state = restored;
            let carrier = state.resolving_stack_entry.as_ref().map(|entry| entry.id);
            let mut continuation_events = Vec::new();
            resolve_root_continuation(
                state,
                &transaction.root,
                false,
                transaction.resolving_trigger_context.as_ref(),
                &mut continuation_events,
            )?;
            settle_canonical_tail(state, &mut continuation_events, 0, carrier)?;
            Ok(ActionResult::applied(
                continuation_events,
                state.waiting_for.clone(),
            ))
        }
        ReplayOutcome::Paused {
            shadow,
            events,
            log_entries,
        } => {
            let waiting_for = shadow.waiting_for.clone();
            let shadow_failure = recorded_failure(&shadow);
            let mut transaction = transaction;
            if transaction.recovered.is_some() {
                transaction.recovered = Some(Box::new(PaymentRecoveryCheckpoint {
                    state: Box::new(checkpoint_state(&shadow)),
                    events,
                    log_entries,
                    consumed: transaction.transcript.len(),
                }));
            }
            let mut projected = state.clone();
            projected.payment_transaction = Some(Box::new(transaction));
            projected.waiting_for = waiting_for.clone();
            projected.cost_payment_failed_flag = false;
            projected.payment_transaction_replay = false;
            projected.payment_transaction_just_handled = false;
            *state = projected;
            record_failure(state, shadow_failure);
            Ok(ActionResult::applied(Vec::new(), waiting_for))
        }
        ReplayOutcome::Completed {
            mut shadow,
            mut events,
            log_entries,
        } => {
            shadow.payment_transaction = None;
            shadow.payment_transaction_replay = false;
            shadow.payment_transaction_just_handled = false;
            let carrier = shadow.resolving_stack_entry.as_ref().map(|entry| entry.id);
            let continuation_event_start = events.len();
            // The runtime-only authorities (card DB, names, host stream,
            // interaction binding, session) belong to the live state.
            hydrate_runtime_authorities(&mut shadow, state);
            // The payment is committed. Continue the printed rider through the
            // ordinary chain walker, now outside the transaction, so a later
            // pause/failure cannot restore the pre-payment base.
            resolve_root_continuation(
                &mut shadow,
                &transaction.root,
                true,
                transaction.resolving_trigger_context.as_ref(),
                &mut events,
            )?;
            settle_canonical_tail(&mut shadow, &mut events, continuation_event_start, carrier)?;
            let waiting_for = shadow.waiting_for.clone();
            *state = shadow;
            Ok(ActionResult::applied(events, waiting_for).with_log_entries(log_entries))
        }
    }
}

/// CR 104.3a + CR 800.4a + CR 118.12: another player's concession is an action
/// outside the transaction that changes the canonical base every replay starts
/// from (their permanents leave, and the effects they generated end). The
/// staged payment is re-materialized once on that base so the canonical prompt
/// is the one the payment now actually waits on: a payment the new base lets
/// complete without the pending answer commits (its rider continues), one still
/// paused exposes its current prompt, and a failed one aborts with its printed
/// continuation. The root stays latched on replay, so the departure never
/// unpays a choice already made (CR 614.17a); each event is judged as it
/// happens on the new base — e.g. a draw a can't-draw effect now forbids is no
/// longer replaceable (CR 614.17c), so a Dredge choice offered before the
/// departure is withdrawn. A replay the transcript can no longer follow keeps
/// the existing descriptor.
pub(crate) fn settle_after_outside_action(
    state: &mut GameState,
    pre_action: Option<&GameState>,
    departing: PlayerId,
) -> Result<Option<ActionResult>, EngineError> {
    let Some(mut transaction) = state.payment_transaction.as_deref().cloned() else {
        return Ok(None);
    };

    // A checkpoint parked on a prompt outside the checkpoint contract
    // ([`departure_can_change_parked_prompt`]) is dropped: the payment
    // re-materializes on the departed base below.
    if transaction
        .recovered
        .as_deref()
        .is_some_and(|checkpoint| departure_can_change_parked_prompt(&checkpoint.state))
    {
        transaction.recovered = None;
        if let Some(live) = state.payment_transaction.as_deref_mut() {
            live.recovered = None;
        }
    }

    // CR 614.17a + CR 118.12: a live checkpoint already holds the accepted
    // prefix and its cursor. Mirror this departure onto it; replaying the root
    // from the canonical base would re-execute completed work.
    if transaction.recovered.is_some() {
        let checkpoint = transaction
            .recovered
            .as_deref()
            .expect("the checkpoint was matched");
        let mut shadow = hydrated_checkpoint(state, checkpoint);
        shadow.payment_transaction_replay = true;
        let mut events = checkpoint.events.clone();
        let mut log_entries = checkpoint.log_entries.clone();
        let mut mirrored = Vec::new();
        crate::game::elimination::concede(&mut shadow, departing, &mut mirrored);
        restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
        // CR 104.3a: the checkpoint can only stay if it includes this departure.
        // A parked draw that no longer settles on the departed shadow is a
        // failed recovery; the caller's failed-recovery boundary retires the
        // checkpoint instead of keeping one from before the departure.
        resettle_parked_replacement_after_departure(&mut shadow, &mut events, &mut log_entries)?;
        shadow.payment_transaction_replay = false;
        return settle_recovered_outcome(state, transaction, shadow, events, log_entries).map(Some);
    }

    // CR 614.17a + CR 118.12: any concession can change the effects that made a
    // synchronous leg or a recorded answer happen. Preserve the pre-departure
    // prefix whenever the pre-action base is available, even when replaying the
    // new base would succeed.
    if let Some(pre_action) = pre_action {
        if let Some(settled) =
            settle_accepted_prefix_after_departure(state, pre_action, &transaction, departing)?
        {
            return Ok(Some(settled));
        }
    }

    // No preserved prefix (no pre-action base, or a parked prompt outside the
    // checkpoint contract): the departure-adjusted replay is the fallback. It
    // re-materializes the payment on the departed base, where the cost
    // authority derives each prompt afresh.
    let outcome = match replay(state, &transaction) {
        Ok(outcome) => outcome,
        Err(error) => {
            tracing::debug!(
                ?error,
                "a staged payment's transcript no longer replays after an outside action"
            );
            return Ok(None);
        }
    };
    if let ReplayOutcome::Paused { shadow, .. } = &outcome {
        if shadow.waiting_for == state.waiting_for {
            return Ok(None);
        }
    }
    settle_replay_outcome(state, transaction, outcome).map(Some)
}

/// CR 614.17a + CR 118.12: re-materialize the accepted prefix (including a
/// synchronously paid leg) on the pre-action base, mirror the departure there,
/// and either commit the completed payment or keep the prefix plus its exact
/// open cursor as the recovery checkpoint.
fn settle_accepted_prefix_after_departure(
    state: &mut GameState,
    pre_action: &GameState,
    transaction: &ResolutionPaymentTransaction,
    departing: PlayerId,
) -> Result<Option<ActionResult>, EngineError> {
    let Ok(ReplayOutcome::Paused {
        mut shadow,
        mut events,
        mut log_entries,
    }) = replay(pre_action, transaction)
    else {
        return Ok(None);
    };
    if departure_can_change_parked_prompt(&shadow) {
        return Ok(None);
    }
    shadow.payment_transaction = None;
    shadow.payment_transaction_replay = true;
    shadow.payment_transaction_just_handled = false;
    if transaction.transcript.is_empty() {
        // CR 603.2: a synchronously paid prefix has no ActionResult scan, so its
        // draw/other events must be collected here before the checkpoint parks.
        triggers::collect_paused_exit_triggers(&mut shadow, &events, 0);
        shadow.consumed_before_priority_trigger_events.clear();
    }
    let mut mirrored = Vec::new();
    crate::game::elimination::concede(&mut shadow, departing, &mut mirrored);
    restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
    // A parked prompt that no longer settles on the departed prefix is a failed
    // recovery, as on the checkpoint branch: the caller's failed-recovery
    // boundary retires the payment instead of replaying on the changed base.
    resettle_parked_replacement_after_departure(&mut shadow, &mut events, &mut log_entries)?;
    shadow.payment_transaction_replay = false;
    settle_recovered_outcome(state, transaction.clone(), shadow, events, log_entries).map(Some)
}

/// CR 614.17a + CR 118.12: commit a re-materialized prefix that reached the
/// payment base, or store it (with its cursor) as the live checkpoint. The
/// descriptor keeps its original root, so the printed rider and the payment's
/// suffix authority stay attached to it.
fn settle_recovered_outcome(
    state: &mut GameState,
    transaction: ResolutionPaymentTransaction,
    shadow: GameState,
    events: Vec<GameEvent>,
    log_entries: Vec<GameLogEntry>,
) -> Result<ActionResult, EngineError> {
    // CR 118.12: a suffix the departure made unpayable fails the payment, as
    // it does on an ordinary replay (`replay`): a failed shadow is neither a
    // completed payment nor a prompt to keep, whatever wait it stopped at.
    if shadow.cost_payment_failed_flag {
        let mut failed = transaction;
        failed.recovered = None;
        return settle_replay_outcome(
            state,
            failed,
            ReplayOutcome::Failed {
                recorded_failure: recorded_failure(&shadow),
            },
        );
    }
    if returned_to_base_waiting(&shadow, &transaction.base_waiting_for) {
        let mut completed = transaction;
        completed.recovered = None;
        return settle_replay_outcome(
            state,
            completed,
            ReplayOutcome::Completed {
                shadow,
                events,
                log_entries,
            },
        );
    }
    let mut paused = transaction;
    paused.recovered = Some(Box::new(PaymentRecoveryCheckpoint {
        state: Box::new(checkpoint_state(&shadow)),
        events: events.clone(),
        log_entries: log_entries.clone(),
        consumed: paused.transcript.len(),
    }));
    settle_replay_outcome(
        state,
        paused,
        ReplayOutcome::Paused {
            shadow,
            events,
            log_entries,
        },
    )
}

/// The stored checkpoint never carries the descriptor, the transient replay
/// flags or the storing action's ingress window.
fn checkpoint_state(shadow: &GameState) -> GameState {
    let mut checkpoint = shadow.clone();
    checkpoint.payment_transaction = None;
    checkpoint.payment_transaction_replay = false;
    checkpoint.payment_transaction_just_handled = false;
    checkpoint.ingress_life_window = None;
    // CR 701.24a / CR 706.1a: a shuffle or roll sampled on the shadow advances its stream;
    // stamp the serialized high-water so the recorded position travels with
    // the checkpoint even when the resolver did not capture it explicitly.
    let sampled = checkpoint.rng.get_word_pos();
    if sampled > checkpoint.rng_word_pos {
        checkpoint.rng_word_pos = sampled;
    }
    checkpoint
}

/// Adopt the live host's fresh entropy while keeping any advancement the
/// checkpoint shadow already accepted on the same seed.
fn adopt_host_stream(shadow: &mut GameState, live: &GameState) {
    let shadow_seed = shadow.rng_seed;
    let shadow_position = shadow.rng.get_word_pos();
    shadow.rng = live.rng.clone();
    shadow.rng_seed = live.rng_seed;
    shadow.rng_word_pos = live.rng_word_pos;
    if shadow_seed == live.rng_seed && shadow_position > shadow.rng.get_word_pos() {
        shadow.rng.set_word_pos(shadow_position);
        shadow.rng_word_pos = shadow_position;
    }
}

/// CR 104.3a + CR 118.12: mirror an outside, submitter-scoped action onto the
/// recovery checkpoint. The canonical state already carries it; without this,
/// settling the checkpoint would roll the action back (a revoked debug
/// capability, for example). The action's events were published canonically and
/// are not duplicated into the checkpoint's buffer.
pub(crate) fn mirror_outside_action(state: &mut GameState, actor: PlayerId, action: &GameAction) {
    let Some(transaction) = state.payment_transaction.as_deref() else {
        return;
    };
    let Some(checkpoint) = transaction.recovered.as_deref() else {
        return;
    };
    let mut shadow = hydrated_checkpoint(state, checkpoint);
    shadow.payment_transaction_replay = true;
    // CR 402.3 + CR 118.12: a hand reorder names only the live canonical hand;
    // the checkpoint also holds the accepted prefix's drawn cards. Preserve the
    // player's canonical order and keep the extra cards in the hand.
    if let GameAction::ReorderHand { order } = action {
        let Some(player) = shadow.players.get(actor.0 as usize) else {
            return;
        };
        let Some(canonical) = state.players.get(actor.0 as usize) else {
            return;
        };
        let hand: Vec<crate::types::identifiers::ObjectId> = player.hand.iter().copied().collect();
        let canonical_hand: std::collections::HashSet<_> = canonical.hand.iter().copied().collect();
        let hand_set: std::collections::HashSet<_> = hand.iter().copied().collect();
        let order_set: std::collections::HashSet<_> = order.iter().copied().collect();
        // CR 402.3 + CR 118.12: the order names the canonical hand. The
        // accepted prefix can have removed one of its cards (the checkpoint
        // hand no longer holds it), so reorder the surviving IDs only and
        // leave the hidden prefix cards in their own slots.
        if order_set.len() != order.len()
            || order
                .iter()
                .any(|id| !hand_set.contains(id) && !canonical_hand.contains(id))
        {
            return;
        }
        let surviving_order: Vec<_> = order
            .iter()
            .copied()
            .filter(|id| hand_set.contains(id))
            .collect();
        let surviving_in_hand = hand.iter().filter(|id| order_set.contains(id)).count();
        if surviving_order.len() != surviving_in_hand {
            return;
        }
        let mut merged = hand.clone();
        let mut ordered = surviving_order.iter();
        for slot in &mut merged {
            if order_set.contains(slot) {
                *slot = *ordered.next().expect("counts match");
            }
        }
        shadow.players[actor.0 as usize].hand = merged.into_iter().collect();
    } else if engine::apply_admitted_recorded_action(&mut shadow, actor, actor, action.clone())
        .is_err()
    {
        return;
    }
    shadow.payment_transaction_replay = false;
    let mut updated = transaction.clone();
    updated.recovered = Some(Box::new(PaymentRecoveryCheckpoint {
        state: Box::new(checkpoint_state(&shadow)),
        events: checkpoint.events.clone(),
        log_entries: checkpoint.log_entries.clone(),
        consumed: updated.transcript.len(),
    }));
    **state
        .payment_transaction
        .as_mut()
        .expect("the descriptor is live") = updated;
}

/// CR 104.1 + CR 104.3a: a terminal game retires a parked payment descriptor;
/// its prompt is never shown in sideboarding or the results view.
pub(crate) fn retire_terminal_transaction(state: &mut GameState) {
    if state.game_end.is_some() {
        state.payment_transaction = None;
        state.payment_transaction_replay = false;
        state.payment_transaction_just_handled = false;
    }
}

/// CR 104.3a + CR 800.4a + CR 400.7: whether the live recovery checkpoint, if
/// any, can still execute after a departure: its seats are the canonical
/// seats ([`checkpoint_seats_match`]) and its parked prompt names no object
/// that left its zone (a departing owner's objects leave the game). The
/// concession boundary keeps a recovery only when this holds; checked restore
/// enforces the same two checks.
pub(crate) fn checkpoint_describes_board(state: &GameState) -> bool {
    checkpoint_seats_match(state)
        && state
            .payment_transaction
            .as_deref()
            .and_then(|transaction| transaction.recovered.as_deref())
            .is_none_or(|checkpoint| {
                crate::types::game_state::recovered_prompt_names_live_objects(&checkpoint.state)
            })
}

/// CR 104.3a + CR 800.4a: whether the live recovery checkpoint, if any,
/// describes the canonical seats: the same players with the same departures.
/// A checkpoint taken before a departure cannot execute on the board after
/// it. Checked restore and the concession boundary both enforce this.
pub(crate) fn checkpoint_seats_match(state: &GameState) -> bool {
    let Some(checkpoint) = state
        .payment_transaction
        .as_deref()
        .and_then(|transaction| transaction.recovered.as_deref())
    else {
        return true;
    };
    let seats = |state: &GameState| {
        state
            .players
            .iter()
            .map(|player| (player.id, player.is_eliminated))
            .collect::<Vec<_>>()
    };
    seats(&checkpoint.state) == seats(state)
}

/// The one preparation of a recovery checkpoint for execution: its persisted
/// nested state without the descriptor or transient guards, with the live
/// state's runtime authorities. Replay, the second-concession settlement, the
/// outside-action mirror, the admission probe and checked restore all start
/// from it.
pub(crate) fn hydrated_checkpoint(
    live: &GameState,
    checkpoint: &PaymentRecoveryCheckpoint,
) -> GameState {
    let mut shadow = (*checkpoint.state).clone();
    shadow.payment_transaction = None;
    shadow.payment_transaction_replay = false;
    shadow.payment_transaction_just_handled = false;
    shadow.cost_payment_failed_flag = false;
    hydrate_runtime_authorities(&mut shadow, live);
    shadow
}

/// The one field list of runtime authorities a payment shadow takes from the
/// live state, whether it starts from a recovery checkpoint or replaces the
/// live state on settlement. These fields are `#[serde(skip)]` or session
/// state, never part of the accepted payment.
fn hydrate_runtime_authorities(shadow: &mut GameState, live: &GameState) {
    shadow.card_db = live.card_db.clone();
    shadow.card_face_registry = live.card_face_registry.clone();
    shadow.meld_pair_registry = live.meld_pair_registry.clone();
    shadow.booster_shelf = live.booster_shelf.clone();
    shadow.all_card_names = live.all_card_names.clone();
    shadow.log_player_names = live.log_player_names.clone();
    // CR 701.24a + CR 706.1a + CR 118.12: the accepted prefix's outcomes (including any
    // recorded shuffle permutation) are already in the shadow's state; the
    // future stream belongs to the restored host, whose fresh reseed must not
    // be overwritten by the checkpoint's older cursor. If the action consumed
    // entropy on the same seed, keep the advanced position so the host does not
    // replay those words.
    adopt_host_stream(shadow, live);
    shadow.interaction_session_id = live.interaction_session_id.clone();
    shadow.interaction_generation = live.interaction_generation;
    shadow.next_interaction_serial = live.next_interaction_serial.clone();
    shadow.active_interaction_slots = live.active_interaction_slots.clone();
    // Session/authority state is not part of the accepted payment: a restored
    // or revoked capability decision belongs to the live session, not to the
    // checkpoint snapshot.
    shadow.debug_mode = live.debug_mode;
    shadow.debug_permitted = live.debug_permitted.clone();
    shadow.format_config = live.format_config.clone();
    // The ingress window belongs to the live action: a player-resource change
    // the shadow cannot represent is recorded where this action's boundary
    // rejects it (a concession instead retires the payment and keeps the
    // departure). A failure the shadow already recorded in this action, while
    // replaying the accepted prefix, is kept; a checkpoint never carries one
    // (`checkpoint_state`).
    let recorded = shadow
        .ingress_life_window
        .and_then(|window| window.failed_player);
    shadow.ingress_life_window = live.ingress_life_window.map(|mut window| {
        window.failed_player = window.failed_player.or(recorded);
        window
    });
}

/// CR 614.17c + CR 616.1 + CR 800.4a: settle a parked replacement prompt the
/// departure just changed, so the checkpoint never stores a prompt the game
/// would no longer offer. A draw that can no longer happen settles as the
/// no-replacement outcome — the draw is clamped and its payment resumes — for
/// optional and mandatory ordering prompts alike. A prompt whose applicable
/// replacements the departure changed (a candidate's source left the game, or
/// an effect the departed player's permanent suppressed applies again) is
/// answered by re-proposing the event, which offers exactly the replacements
/// that now apply (`replacement::parked_prompt_is_stale`).
fn resettle_parked_replacement_after_departure(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    log_entries: &mut Vec<GameLogEntry>,
) -> Result<(), EngineError> {
    if !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }) {
        return Ok(());
    }
    let Some(pending) = state.pending_replacement.as_ref() else {
        return Ok(());
    };
    if !crate::game::replacement::parked_prompt_is_stale(state, pending) {
        return Ok(());
    }
    let Some(actor) = state.waiting_for.acting_players().first().copied() else {
        return Ok(());
    };
    let result = engine::apply_admitted_recorded_action(
        state,
        actor,
        actor,
        GameAction::ChooseReplacement { index: 0 },
    )?;
    events.extend(result.events);
    log_entries.extend(result.log_entries);
    Ok(())
}

/// The checkpoint contract. A departure keeps a staged payment's accepted
/// prefix on a checkpoint only while the prompt it is parked on is one another
/// player's departure cannot change: a replacement choice (re-settled through
/// the replacement authority) or a discard from the payer's own hand
/// (CR 400.3: a hand holds only its owner's cards). Every other prompt is
/// outside it. A selection among battlefield permanents or among cards in
/// graveyards may list objects another player owns, or objects eligible only
/// through an effect another player's departure ends (CR 800.4a), and no
/// printed card reaches such a prompt in a staged payment: the five
/// resolution-time `PayCost { Composite }` nodes in the card data are all
/// `[Mana, PayLife]`. The guard routes everything outside the contract away
/// from the checkpoint: the payment keeps its descriptor and transcript and
/// re-materializes on the departed base, where the cost authority derives the
/// prompt and its eligible objects afresh and a cost that can no longer be
/// paid fails whole, as a replay of the transcript on that board finds it.
fn departure_can_change_parked_prompt(shadow: &GameState) -> bool {
    !matches!(
        shadow.waiting_for,
        WaitingFor::ReplacementChoice { .. } | WaitingFor::DiscardChoice { .. }
    )
}

/// CR 104.3a: a concession must stand even when the staged payment's recovery or
/// its printed rider fails. Retire the transaction and its prompt, and publish a
/// valid priority boundary; the canonical departure stays.
pub(crate) fn abandon_failed_recovery(state: &mut GameState) {
    state.payment_transaction = None;
    state.payment_transaction_replay = false;
    state.payment_transaction_just_handled = false;
    state.cost_payment_failed_flag = false;
    state.pending_replacement = None;
    if !matches!(
        state.waiting_for,
        WaitingFor::Priority { .. } | WaitingFor::GameOver { .. }
    ) {
        // CR 800.4j: never hand priority to the seat that just left.
        let player = if state
            .players
            .get(state.active_player.0 as usize)
            .is_some_and(|seat| seat.is_eliminated)
        {
            crate::game::players::next_player_in_turn_order(state, state.active_player)
        } else {
            state.active_player
        };
        state.waiting_for = WaitingFor::Priority { player };
    }
}

/// Materialize the public shadow used by viewer projections, client wires, and
/// legal-action enumeration. A replay failure fails closed to the untouched
/// base rather than exposing a partially mutated shadow.
pub(crate) fn project(state: &GameState) -> GameState {
    // CR 104.1 + CR 104.3a: a terminal game has no live payment prompt; never
    // project a stale one into sideboarding or the results view.
    if state.game_end.is_some() {
        let mut base = state.clone();
        base.payment_transaction = None;
        base.payment_transaction_replay = false;
        base.payment_transaction_just_handled = false;
        return base;
    }
    let Some(transaction) = state.payment_transaction.as_ref() else {
        return state.clone();
    };
    match replay(state, transaction) {
        Ok(ReplayOutcome::Paused { mut shadow, .. })
        | Ok(ReplayOutcome::Completed { mut shadow, .. }) => {
            shadow.payment_transaction = None;
            shadow.payment_transaction_replay = false;
            shadow.payment_transaction_just_handled = false;
            shadow
        }
        Ok(ReplayOutcome::Failed { .. }) | Err(_) => {
            let mut base = state.clone();
            base.payment_transaction = None;
            base.payment_transaction_replay = false;
            base.payment_transaction_just_handled = false;
            base.waiting_for = transaction.base_waiting_for.clone();
            base
        }
    }
}

/// Materialize the canonical, uncommitted base for an unscoped client wire.
///
/// A `ClientGameStateRef` without a viewer has no authenticated actor and must
/// never replay a staged transaction merely to serialize it. The caller routes
/// this base through the explicit unseated visibility projection; keeping the
/// payment boundary here free of a fabricated `PlayerId` prevents topology
/// lookups from accidentally granting private access.
pub(crate) fn project_without_viewer(state: &GameState) -> GameState {
    let mut base = state.clone();
    base.payment_transaction = None;
    base.payment_transaction_replay = false;
    base.payment_transaction_just_handled = false;
    base
}

/// Materialize only for a viewer entitled to answer the current prompt. Other
/// viewers keep the canonical base (plus the public prompt) so an uncommitted
/// hand-to-graveyard move, life change, or other public mutation cannot be
/// observed and later rolled back in front of them.
pub(crate) fn project_for_viewer(
    state: &GameState,
    viewer: crate::types::player::PlayerId,
) -> GameState {
    let Some(_transaction) = state.payment_transaction.as_ref() else {
        return state.clone();
    };
    let shadow = project(state);
    // CR 723.5: shadow access follows the exact submitter set that
    // admits the next action. A semantic seat is not enough when a controller
    // effect routes its prompt to another actor; fail closed to the canonical
    // base unless the viewer is that currently authorized submitter.
    let viewer_is_authorized =
        crate::game::turn_control::authorized_submitters(state).contains(&viewer);
    if viewer_is_authorized {
        return shadow;
    }

    let mut base = state.clone();
    base.payment_transaction = None;
    base.payment_transaction_replay = false;
    base.payment_transaction_just_handled = false;
    // Keep the current public prompt while retaining every resource/object
    // field at the canonical pre-payment base.
    base.waiting_for = shadow.waiting_for;
    base
}

/// Retire a staged payment when its payer/root owner leaves the game.
///
/// CR 800.4a removes a leaving player's paused resolution authority along with
/// the player. The canonical base remains live for the elimination sweep; only
/// the durable payment descriptor and its transient replay guards are retired.
/// An unrelated player's departure must not cancel another player's payment.
pub(crate) fn abandon_for_owner_departure(
    state: &mut GameState,
    departing: crate::types::player::PlayerId,
) -> Option<ResolutionPaymentTransaction> {
    if state
        .payment_transaction
        .as_ref()
        .is_some_and(|transaction| transaction.owner == departing)
    {
        let transaction = state
            .payment_transaction
            .take()
            .map(|transaction| *transaction);
        state.payment_transaction_replay = false;
        state.payment_transaction_just_handled = false;
        state.cost_payment_failed_flag = true;
        return transaction;
    }
    None
}

/// Resume only a staged payment's printed continuation after its payer leaves.
///
/// The payer's departure makes the payment unsuccessful, but it does not remove
/// an otherwise-surviving ability controller's unconditional sequential tail.
/// The caller performs the CR 800.4a leave sweep first, then invokes this helper
/// only when the root controller is still in the game.
pub(crate) fn resolve_abandoned_continuation(
    state: &mut GameState,
    transaction: &ResolutionPaymentTransaction,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    resolve_root_continuation(
        state,
        &transaction.root,
        false,
        transaction.resolving_trigger_context.as_ref(),
        events,
    )
}

/// Actions outside the staged payment boundary never enter its transcript.
/// Keep this shared with restore validation: replay treats each transcript
/// entry as already admitted, so persisted entries must obey the same split.
pub(crate) fn is_outside_transaction(action: &GameAction) -> bool {
    // CR 104.3a: concession bypasses every WaitingFor and reaches the canonical
    // elimination authority, not a replayed payment choice.
    action.is_submitter_scoped() || matches!(action, GameAction::Concede { .. })
}

/// Whether the transaction path should own this action. Actor-scoped display,
/// debug/capability, concession, and unrelated global actions remain ordinary
/// reducer actions while a payment prompt is open. For the remaining actions,
/// materialize the current replay shadow and reuse the ordinary reducer as the
/// admission authority instead of maintaining a second `(WaitingFor,
/// GameAction)` table. The shadow is required here because prompts such as a
/// replacement choice carry transient continuation records that deliberately do
/// not live on the canonical pre-payment state.
pub(crate) fn owns_action(state: &GameState, action: &GameAction) -> bool {
    let Some(transaction) = state.payment_transaction.as_ref() else {
        return false;
    };
    if is_outside_transaction(action) {
        return false;
    }
    let semantic_owner = state
        .waiting_for
        .acting_players()
        .first()
        .copied()
        .unwrap_or(transaction.owner);
    let actor = crate::game::turn_control::authorized_submitter_for_player(state, semantic_owner);
    // A recovery checkpoint is the materialized authority for actions already
    // admitted outside the payment; probing the canonical base would reject an
    // action whose cards were drawn into the hidden checkpoint.
    let probe = match transaction.recovered.as_deref() {
        Some(checkpoint) => hydrated_checkpoint(state, checkpoint),
        None => project(state),
    };
    admits(probe, actor, action)
}

/// The admission authority for an answer to a materialized payment prompt: the
/// ordinary `WaitingFor`/action reducer, run on a copy that cannot recursively
/// re-enter this transaction or create a nested descriptor. Shared by
/// [`owns_action`] and by checked restore of a recovery checkpoint, so a
/// restored prompt is admitted exactly when its answer would be.
pub(crate) fn admits(mut probe: GameState, actor: PlayerId, action: &GameAction) -> bool {
    probe.payment_transaction = None;
    probe.payment_transaction_replay = true;
    probe.payment_transaction_just_handled = false;
    crate::game::engine::apply_recorded_action(&mut probe, actor, action.clone()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 104.3a + CR 118.12: a payment shadow runs in the live action's
    /// ingress window, keeps a failure it already recorded in this action
    /// (replaying the accepted prefix), and a stored checkpoint carries no
    /// window, so a later action never inherits a stale failure.
    #[test]
    fn a_shadow_keeps_its_recorded_failure_in_the_live_ingress_window() {
        use crate::types::game_state::IngressLifeWindow;

        let mut live = GameState::new_two_player(7);
        live.ingress_life_window = Some(IngressLifeWindow::default());
        let mut shadow = live.clone();
        shadow.ingress_life_window = Some(IngressLifeWindow {
            failed_player: Some(PlayerId(0)),
        });
        hydrate_runtime_authorities(&mut shadow, &live);
        assert_eq!(
            shadow
                .ingress_life_window
                .and_then(|window| window.failed_player),
            Some(PlayerId(0)),
            "the failure recorded while replaying is kept"
        );

        let mut stale = live.clone();
        stale.ingress_life_window = None;
        hydrate_runtime_authorities(&mut stale, &live);
        assert_eq!(
            stale.ingress_life_window,
            Some(IngressLifeWindow::default()),
            "a shadow without a window runs in the live action's"
        );

        assert_eq!(
            checkpoint_state(&shadow).ingress_life_window,
            None,
            "a checkpoint does not store the storing action's window"
        );
    }
    use crate::game::game_object::ObjectIncarnationRefExt;

    use crate::game::deck_loading::DeckEntry;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ChooseOneOfKind, Effect,
        EffectOutcomeSignal, PlayerFilter, QuantityExpr, QuantityModification,
        ReplacementDefinition, SubAbilityLink, TargetFilter, TargetRef,
    };
    use crate::types::card::CardFace;
    use crate::types::events::GameEvent;
    use crate::types::game_state::{PersistedGameState, PlayerDeckPool};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaCost, ManaCostShard};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    // The behavioral T1–T4 fixtures live beside the payment authority in the
    // engine test suite; this module keeps one small persistence witness close
    // to the descriptor implementation itself.
    #[test]
    fn descriptor_round_trips_without_serializing_transient_replay_flags() {
        let mut state = GameState::new_two_player(7);
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: crate::types::player::PlayerId(0),
            root: Box::new(crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::GenericEffect {
                    static_abilities: Vec::new(),
                    duration: None,
                    target: None,
                    end_cost: None,
                },
                Vec::new(),
                crate::types::identifiers::ObjectId(1),
                crate::types::player::PlayerId(0),
            )),
            transcript: vec![ResolutionPaymentTranscriptEntry {
                authenticated_actor: PlayerId(1),
                semantic_owner: PlayerId(0),
                action: GameAction::PassPriority,
            }],
            base_waiting_for: crate::types::game_state::WaitingFor::Priority {
                player: crate::types::player::PlayerId(0),
            },
            resolving_trigger_context: None,
            recovered: None,
        }));
        state.payment_transaction_replay = true;
        state.payment_transaction_just_handled = true;
        let value = serde_json::to_value(&state).expect("state serializes");
        assert!(value.get("payment_transaction").is_some());
        assert!(value.get("payment_transaction_replay").is_none());
        assert!(value.get("payment_transaction_just_handled").is_none());
        let restored: GameState = serde_json::from_value(value).expect("state restores");
        assert!(restored.payment_transaction.is_some());
        assert!(!restored.payment_transaction_replay);
        assert!(!restored.payment_transaction_just_handled);
        assert_eq!(
            restored.payment_transaction.as_ref().unwrap().transcript[0].authenticated_actor,
            PlayerId(1)
        );
    }

    #[test]
    fn transcript_legacy_actor_is_explicitly_compatible_but_new_actor_only_wire_rejects() {
        let entry = ResolutionPaymentTranscriptEntry {
            authenticated_actor: PlayerId(1),
            semantic_owner: PlayerId(0),
            action: GameAction::PassPriority,
        };
        let mut legacy = serde_json::to_value(&entry).expect("transcript serializes");
        let object = legacy.as_object_mut().expect("transcript is an object");
        let actor = object
            .remove("authenticated_actor")
            .expect("current actor field is present");
        object.insert("actor".to_string(), actor);
        object.remove("semantic_owner");
        let restored: ResolutionPaymentTranscriptEntry =
            serde_json::from_value(legacy).expect("legacy actor-only transcript restores");
        assert_eq!(restored.authenticated_actor, PlayerId(1));
        assert_eq!(restored.semantic_owner, PlayerId(1));

        let mut current = serde_json::to_value(&entry).expect("transcript serializes");
        current
            .as_object_mut()
            .expect("transcript is an object")
            .remove("semantic_owner");
        assert!(
            serde_json::from_value::<ResolutionPaymentTranscriptEntry>(current).is_err(),
            "a new authenticated_actor-only payload must not invent semantic owner P0"
        );
    }

    #[test]
    fn a_recovery_checkpoint_round_trips_with_its_cursor() {
        let mut state = GameState::new_two_player(7);
        let mut checkpoint_state = GameState::new_two_player(7);
        checkpoint_state.players[0].life = 11;
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::NoOp,
                vec![],
                ObjectId(1),
                PlayerId(0),
            )),
            transcript: vec![ResolutionPaymentTranscriptEntry {
                authenticated_actor: PlayerId(0),
                semantic_owner: PlayerId(0),
                action: GameAction::PassPriority,
            }],
            base_waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            resolving_trigger_context: None,
            recovered: Some(Box::new(PaymentRecoveryCheckpoint {
                state: Box::new(checkpoint_state),
                events: Vec::new(),
                log_entries: Vec::new(),
                consumed: 1,
            })),
        }));
        let value = serde_json::to_value(&state).expect("state serializes");
        let restored: GameState = serde_json::from_value(value).expect("state restores");
        let transaction = restored
            .payment_transaction
            .as_ref()
            .expect("the descriptor survives");
        let checkpoint = transaction
            .recovered
            .as_ref()
            .expect("the recovery checkpoint survives");
        assert_eq!(checkpoint.consumed, 1);
        assert_eq!(checkpoint.state.players[0].life, 11);
    }

    #[test]
    fn a_terminal_game_never_projects_a_parked_payment_prompt() {
        let mut state = GameState::new_two_player(7);
        state.game_end = Some(crate::types::game_state::GameEnd {
            winner: Some(PlayerId(0)),
        });
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::NoOp,
                vec![],
                ObjectId(1),
                PlayerId(0),
            )),
            transcript: Vec::new(),
            base_waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            resolving_trigger_context: None,
            recovered: None,
        }));
        let projected = project(&state);
        assert!(
            projected.payment_transaction.is_none(),
            "a terminal game must not project a parked payment"
        );
        assert_eq!(projected.waiting_for, state.waiting_for);
    }

    #[test]
    fn persisted_payment_transaction_rejects_out_of_range_owner_through_raw_and_trusted() {
        let mut state = GameState::new_two_player(7);
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(ResolvedAbility::new(
                Effect::GenericEffect {
                    static_abilities: Vec::new(),
                    duration: None,
                    target: None,
                    end_cost: None,
                },
                Vec::new(),
                ObjectId(1),
                PlayerId(0),
            )),
            transcript: vec![ResolutionPaymentTranscriptEntry {
                authenticated_actor: PlayerId(0),
                semantic_owner: PlayerId(0),
                action: GameAction::PassPriority,
            }],
            base_waiting_for: crate::types::game_state::WaitingFor::Priority {
                player: PlayerId(0),
            },
            resolving_trigger_context: None,
            recovered: None,
        }));

        for mut wire in [
            serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone())))
                .expect("serialize raw"),
            serde_json::to_value(PersistedGameState::capture(state.clone()))
                .expect("serialize trusted"),
        ] {
            let transaction = if wire.get("payment_transaction").is_some() {
                wire.get_mut("payment_transaction")
                    .expect("payment transaction is persisted")
            } else {
                wire.get_mut("state")
                    .and_then(|value| value.get_mut("payment_transaction"))
                    .expect("trusted payment transaction is persisted")
            };
            transaction["owner"] = serde_json::json!(9);
            let restored = serde_json::from_value::<PersistedGameState>(wire)
                .expect("wire shape remains decodable");
            let error = restored
                .into_game_state()
                .expect_err("restore must reject an out-of-range payment owner");
            assert!(error.to_string().contains("payment transaction is invalid"));
        }
    }

    #[test]
    fn persisted_recovery_checkpoint_rejects_combat_damage_on_its_stack() {
        let mut state = GameState::new_two_player(7);
        let mut checkpoint_state = GameState::new_two_player(7);
        checkpoint_state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        checkpoint_state
            .stack
            .push_back(crate::types::game_state::StackEntry {
                id: crate::types::identifiers::ObjectId(1),
                source_id: crate::types::identifiers::ObjectId(2),
                controller: PlayerId(0),
                kind: crate::types::game_state::StackEntryKind::CombatDamage {
                    sub_step: crate::types::game_state::CombatDamageSubStep::Regular,
                    assignments: Vec::new(),
                },
            });
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::NoOp,
                vec![],
                ObjectId(1),
                PlayerId(0),
            )),
            transcript: Vec::new(),
            base_waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            resolving_trigger_context: None,
            recovered: Some(Box::new(PaymentRecoveryCheckpoint {
                state: Box::new(checkpoint_state),
                events: Vec::new(),
                log_entries: Vec::new(),
                consumed: 0,
            })),
        }));
        let restored = serde_json::from_value::<PersistedGameState>(
            serde_json::to_value(PersistedGameState::capture(state))
                .expect("the trusted shape serializes"),
        )
        .expect("the wire shape remains decodable");
        let error = restored
            .into_game_state()
            .expect_err("restore must reject combat damage in the checkpoint");
        assert!(
            error.to_string().contains("combat damage"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn persisted_recovery_checkpoint_rejects_a_cursor_beyond_its_transcript() {
        let mut state = GameState::new_two_player(7);
        let mut checkpoint_state = GameState::new_two_player(7);
        checkpoint_state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::NoOp,
                vec![],
                ObjectId(1),
                PlayerId(0),
            )),
            transcript: vec![ResolutionPaymentTranscriptEntry {
                authenticated_actor: PlayerId(0),
                semantic_owner: PlayerId(0),
                action: GameAction::PassPriority,
            }],
            base_waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            resolving_trigger_context: None,
            recovered: Some(Box::new(PaymentRecoveryCheckpoint {
                state: Box::new(checkpoint_state),
                events: Vec::new(),
                log_entries: Vec::new(),
                consumed: 2,
            })),
        }));
        let restored = serde_json::from_value::<PersistedGameState>(
            serde_json::to_value(PersistedGameState::capture(state))
                .expect("the trusted shape serializes"),
        )
        .expect("the wire shape remains decodable");
        let error = restored
            .into_game_state()
            .expect_err("restore must reject a checkpoint cursor beyond its transcript");
        assert!(
            error.to_string().contains("recovery checkpoint"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn persisted_payment_transcript_rejects_out_of_band_actions_in_both_wire_shapes() {
        let (mut state, root, card) = discard_exile_case();
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &root, &mut events, 0)
            .expect("the first payment choice pauses");
        apply_pending_action(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards { cards: vec![card] },
        )
        .expect("the admitted choice remains staged");
        assert_eq!(
            state
                .payment_transaction
                .as_ref()
                .expect("payment remains staged")
                .transcript
                .len(),
            1
        );

        for wire in [
            serde_json::to_value(PersistedGameState::Raw(Box::new(state.clone())))
                .expect("raw snapshot serializes"),
            serde_json::to_value(PersistedGameState::capture(state.clone()))
                .expect("trusted snapshot serializes"),
        ] {
            let legitimate = serde_json::from_value::<PersistedGameState>(wire.clone())
                .expect("legitimate wire decodes")
                .into_game_state()
                .expect("an admitted choice restores");
            assert_eq!(
                legitimate
                    .payment_transaction
                    .as_ref()
                    .expect("transaction survives restore")
                    .transcript[0]
                    .action,
                GameAction::SelectCards { cards: vec![card] }
            );

            let mut forged = wire;
            let transaction = if forged.get("payment_transaction").is_some() {
                &mut forged["payment_transaction"]
            } else {
                &mut forged["state"]["payment_transaction"]
            };
            transaction["transcript"] =
                serde_json::to_value(vec![ResolutionPaymentTranscriptEntry {
                    authenticated_actor: PlayerId(0),
                    semantic_owner: PlayerId(0),
                    action: GameAction::Concede {
                        player_id: PlayerId(1),
                    },
                }])
                .expect("forged action serializes");
            let error = serde_json::from_value::<PersistedGameState>(forged)
                .expect("the forged wire still decodes")
                .into_game_state()
                .expect_err("a concession cannot be a staged payment choice");
            assert!(
                error.to_string().contains("outside staged payment"),
                "unexpected restore error: {error}"
            );
        }
    }

    #[test]
    fn unseated_wire_hides_hero_hand_in_one_vs_many_staged_payment() {
        let mut state = GameState::new(crate::types::format::FormatConfig::archenemy(), 4, 42);
        let hero_card = create_object(
            &mut state,
            CardId(920),
            PlayerId(1),
            "R5 OneVsMany hero hand card".to_string(),
            Zone::Hand,
        );
        state.deck_pools = vec![PlayerDeckPool {
            player: PlayerId(1),
            registered_main: std::sync::Arc::new(vec![DeckEntry {
                card: CardFace {
                    name: "R5 private registered deck card".to_string(),
                    ..CardFace::default()
                },
                count: 1,
            }]),
            ..PlayerDeckPool::default()
        }];
        assert_eq!(state.deck_pools[0].registered_main.len(), 1);
        state.active_library_searches.insert(
            crate::types::game_state::ActiveLibrarySearch::try_new(
                PlayerId(0),
                PlayerId(1),
                Some(PlayerId(1)),
                vec![PlayerId(0)],
                vec![(
                    PlayerId(1),
                    Zone::Hand,
                    crate::types::identifiers::ObjectIncarnationRef::from_object(
                        &state.objects[&hero_card],
                    ),
                )],
            )
            .expect("search provenance fixture is coherent"),
        );
        state.waiting_for = crate::types::game_state::WaitingFor::Priority {
            player: PlayerId(0),
        };
        let root = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::GenericEffect {
                static_abilities: Vec::new(),
                duration: None,
                target: None,
                end_cost: None,
            },
            Vec::new(),
            ObjectId(921),
            PlayerId(0),
        );
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(root),
            transcript: Vec::new(),
            base_waiting_for: state.waiting_for.clone(),
            resolving_trigger_context: None,
            recovered: None,
        }));

        let wire = serde_json::to_value(crate::game::derived_views::ClientGameStateRef::wrap(
            &state, None,
        ))
        .expect("unseated OneVsMany wire projection");
        assert_eq!(
            wire["state"]["objects"][hero_card.0.to_string()]["name"],
            "Hidden Card"
        );
        assert!(
            !wire.to_string().contains("R5 OneVsMany hero hand card"),
            "unseated wire leaked a hero hand identity: {wire}"
        );
        assert!(wire["state"].get("payment_transaction").is_none());
        assert_eq!(
            wire["state"]["deck_pools"][0]["registered_main"],
            serde_json::json!([]),
            "unseated wire leaked a registered deck: {wire}"
        );
        assert!(!wire.to_string().contains("R5 private registered deck card"));
        assert!(
            wire["state"].get("active_library_searches").is_none(),
            "unseated wire leaked a private search carrier: {wire}"
        );
        assert!(matches!(
            serde_json::from_value::<crate::types::game_state::WaitingFor>(
                wire["state"]["waiting_for"].clone()
            )
            .expect("public WaitingFor remains decodable"),
            crate::types::game_state::WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    fn life_composite_case(life: i32) -> (GameState, ResolvedAbility) {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = life;
        let root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 6 },
                        },
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 6 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        (state, root)
    }

    fn phyrexian_energy_case(energy: u32) -> (GameState, ResolvedAbility) {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = energy;
        let source = create_object(
            &mut state,
            CardId(904),
            PlayerId(0),
            "R5 Phyrexian shadow source".to_string(),
            Zone::Battlefield,
        );
        let replacement_choice = AbilityDefinition::new(
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
        );
        state
            .objects
            .get_mut(&source)
            .expect("replacement source")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(QuantityModification::Plus { value: 0 })
            .execute(replacement_choice)]
        .into();

        let mut root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: vec![ManaCostShard::PhyrexianBlack],
                                generic: 0,
                            },
                        },
                        AbilityCost::PayEnergy {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let mut rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 7 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        rider.sub_link = SubAbilityLink::SequentialSibling;
        root.sub_ability = Some(Box::new(rider));
        (state, root)
    }

    fn discard_exile_case() -> (GameState, ResolvedAbility, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let card = create_object(
            &mut state,
            CardId(906),
            PlayerId(0),
            "R5 discard then exile".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(908),
            PlayerId(0),
            "R5 alternate discard".to_string(),
            Zone::Hand,
        );
        let root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: crate::types::ability::CardSelectionMode::Chosen,
                            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::Exile {
                            count: 1,
                            zone: Some(Zone::Graveyard),
                            filter: None,
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(907),
            PlayerId(0),
        );
        (state, root, card)
    }

    fn hidden_shadow_case() -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let hidden_card = create_object(
            &mut state,
            CardId(909),
            PlayerId(1),
            "R5 hidden shadow card".to_string(),
            Zone::Hand,
        );
        let root = ResolvedAbility::new(
            Effect::ChangeZone {
                actor: crate::types::ability::LibraryInstructionActor::Controller,
                library_shuffle: Default::default(),
                origin: Some(Zone::Hand),
                destination: Zone::Graveyard,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: Vec::new(),
                conditional_enter_with_counters: Vec::new(),
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![TargetRef::Object(hidden_card)],
            hidden_card,
            PlayerId(0),
        );
        state.waiting_for = crate::types::game_state::WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.payment_transaction = Some(Box::new(ResolutionPaymentTransaction {
            wire_version: 1,
            owner: PlayerId(0),
            root: Box::new(root),
            transcript: Vec::new(),
            base_waiting_for: state.waiting_for.clone(),
            resolving_trigger_context: None,
            recovered: None,
        }));
        (state, hidden_card)
    }

    /// A production-pipeline hidden-information witness for the final R5a gate.
    /// The semantic payer is Player 0, but a turn-control effect routes the
    /// prompt to Player 1. Player 1 does not know Player 0's hand. The first
    /// supported cost pauses for a discard selection; accepting that selection
    /// moves the hidden card into the public graveyard in the shadow, then the
    /// Phyrexian replacement prompt pauses before the unpayable energy suffix.
    fn controlled_discard_phyrexian_case() -> (GameState, ResolvedAbility, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.turn_decision_controller = Some(PlayerId(1));
        state.players[0].energy = 0;
        let hidden_card = create_object(
            &mut state,
            CardId(910),
            PlayerId(0),
            "R5 controller-hidden card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(911),
            PlayerId(0),
            "R5 controller-hidden alternate".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(912),
            PlayerId(0),
            "R5 controlled phyrexian source".to_string(),
            Zone::Battlefield,
        );
        let replacement_choice = AbilityDefinition::new(
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
        );
        state
            .objects
            .get_mut(&source)
            .expect("controlled replacement source")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(QuantityModification::Plus { value: 0 })
            .execute(replacement_choice)]
        .into();
        let mut root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: crate::types::ability::CardSelectionMode::Chosen,
                            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: vec![ManaCostShard::PhyrexianBlack],
                                generic: 0,
                            },
                        },
                        AbilityCost::PayEnergy {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        // Abort discriminator: the accepted/IfYouDo rider must be suppressed
        // after the later energy suffix fails, while the unconditional printed
        // sibling still resolves once on the restored canonical base.
        let mut gated = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        gated.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        let mut unconditional = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        unconditional.sub_link = SubAbilityLink::SequentialSibling;
        gated.sub_ability = Some(Box::new(unconditional));
        root.sub_ability = Some(Box::new(gated));
        (state, root, hidden_card)
    }

    fn committed_composite_with_failing_rider_case() -> (GameState, ResolvedAbility) {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 10;
        state.players[0].energy = 0;
        let source = ObjectId(913);
        let mut root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    }],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        root.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )));
        (state, root)
    }

    fn committed_composite_with_pausing_rider_case() -> (GameState, ResolvedAbility, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 10;
        state.players[0].energy = 0;
        let first = create_object(
            &mut state,
            CardId(914),
            PlayerId(0),
            "R5 rider discard".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(915),
            PlayerId(0),
            "R5 rider alternate".to_string(),
            Zone::Hand,
        );
        let source = ObjectId(916);
        let mut root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    }],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let mut discard_rider = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    selection: crate::types::ability::CardSelectionMode::Chosen,
                    self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        discard_rider.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )));
        root.sub_ability = Some(Box::new(discard_rider));
        (state, root, first)
    }

    fn trigger_context_composite_case() -> (GameState, ResolvedAbility, ObjectId) {
        let mut state = GameState::new_two_player(42);
        state.players[1].life = 20;
        let first = create_object(
            &mut state,
            CardId(917),
            PlayerId(1),
            "R5 context discard".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(918),
            PlayerId(1),
            "R5 context alternate".to_string(),
            Zone::Hand,
        );
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(1),
            amount: 3,
            new_total: crate::types::events::LifeTotalReading::default(),
        });
        state.current_trigger_events = state.current_trigger_event.clone().into_iter().collect();
        let root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: crate::types::ability::CardSelectionMode::Chosen,
                            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Ref {
                                qty: crate::types::ability::QuantityRef::EventContextAmount,
                            },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::TriggeringPlayer,
            },
            Vec::new(),
            ObjectId(919),
            PlayerId(0),
        );
        (state, root, first)
    }

    #[test]
    fn staged_composite_payment_r5_regression_matrix() {
        // T1: the failed composite never mutates canonical life and publishes
        // no payment events.
        let (mut t1, root_t1) = life_composite_case(10);
        let mut t1_events = Vec::new();
        effects::resolve_ability_chain(&mut t1, &root_t1, &mut t1_events, 0)
            .expect("T1 resolution does not error");
        assert_eq!(t1.players[0].life, 10);
        assert!(t1.payment_transaction.is_none());
        assert!(t1_events.is_empty());

        // T2: both life payments commit once, with one event per component.
        let (mut t2, root_t2) = life_composite_case(13);
        let mut t2_events = Vec::new();
        effects::resolve_ability_chain(&mut t2, &root_t2, &mut t2_events, 0)
            .expect("T2 resolution");
        assert_eq!(t2.players[0].life, 1);
        assert!(t2.payment_transaction.is_none());
        assert_eq!(
            t2_events
                .iter()
                .filter(|event| matches!(event, GameEvent::LifeChanged { amount: -6, .. }))
                .count(),
            2,
            "each committed PayLife component emits exactly once"
        );

        // T3: the first component pauses on the replacement choice. Selecting
        // the branch then fails at the energy suffix, restoring BASE exactly.
        let (mut t3, root_t3) = phyrexian_energy_case(0);
        let mut t3_events = Vec::new();
        effects::resolve_ability_chain(&mut t3, &root_t3, &mut t3_events, 0)
            .expect("T3 initial resolution");
        assert!(t3.payment_transaction.is_some());
        assert!(matches!(
            t3.waiting_for,
            crate::types::game_state::WaitingFor::ChooseOneOfBranch { .. }
        ));
        assert_eq!(t3.players[0].life, 20);
        assert_eq!(t3.players[0].energy, 0);
        let t3_result =
            apply_pending_action(&mut t3, PlayerId(0), GameAction::ChooseBranch { index: 0 })
                .expect("T3 branch action");
        assert_eq!(t3.players[0].life, 27);
        assert_eq!(
            t3_result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::LifeChanged {
                        player_id: PlayerId(0),
                        amount: 7,
                        ..
                    }
                ))
                .count(),
            1,
            "the unconditional rider survives the aborted payment exactly once"
        );
        assert!(!t3_result.events.iter().any(|event| matches!(
            event,
            GameEvent::LifeChanged { amount: -2, .. } | GameEvent::EnergyChanged { .. }
        )));
        assert!(t3.payment_transaction.is_none());
        assert_eq!(t3.players[0].life, 27);
        assert_eq!(t3.players[0].energy, 0);
        assert!(matches!(
            t3.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));

        // T4: the same transcript succeeds with one energy, and the final
        // materialized state contains the replacement branch plus rider once.
        let (mut t4, root_t4) = phyrexian_energy_case(1);
        let mut t4_events = Vec::new();
        effects::resolve_ability_chain(&mut t4, &root_t4, &mut t4_events, 0)
            .expect("T4 initial resolution");
        assert!(t4.payment_transaction.is_some());
        let t4_result =
            crate::game::engine::apply_as_current(&mut t4, GameAction::ChooseBranch { index: 0 })
                .expect("T4 boundary branch action");
        assert_eq!(t4.players[0].life, 26);
        assert_eq!(t4.players[0].energy, 0);
        assert!(t4.payment_transaction.is_none());
        assert!(matches!(
            t4.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));
        assert_eq!(
            t4_result
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EnergyChanged {
                        player: PlayerId(0),
                        delta: -1,
                    }
                ))
                .count(),
            1,
            "the suffix payment is committed exactly once"
        );

        // Discard -> Exile: no preflight failure is allowed before the discard
        // choice. The two accepted choices remain shadow-only until commit.
        let (mut discard, root_discard, discarded_card) = discard_exile_case();
        let mut discard_events = Vec::new();
        effects::resolve_ability_chain(&mut discard, &root_discard, &mut discard_events, 0)
            .expect("discard initial resolution");
        assert!(discard.payment_transaction.is_some());
        assert!(matches!(
            discard.waiting_for,
            crate::types::game_state::WaitingFor::DiscardChoice { .. }
        ));
        assert_eq!(discard.objects[&discarded_card].zone, Zone::Hand);
        apply_pending_action(
            &mut discard,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![discarded_card],
            },
        )
        .expect("discard choice");
        assert_eq!(
            discard
                .payment_transaction
                .as_ref()
                .expect("transaction remains paused")
                .transcript[0]
                .authenticated_actor,
            PlayerId(0)
        );
        assert!(matches!(
            discard.waiting_for,
            crate::types::game_state::WaitingFor::EffectZoneChoice { .. }
        ));
        assert_eq!(discard.objects[&discarded_card].zone, Zone::Hand);
        let commit = apply_pending_action(
            &mut discard,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![discarded_card],
            },
        )
        .expect("exile choice");
        assert!(discard.payment_transaction.is_none());
        assert_eq!(discard.objects[&discarded_card].zone, Zone::Exile);
        assert!(!commit.events.is_empty());

        // While the second choice is pending, the owner may see the shadow,
        // but an opponent must retain the canonical hand card. This is the
        // knowledge/rollback witness: the later commit/abort cannot leak a
        // transient public-zone mutation to another viewer.
        let (mut privacy, privacy_root, privacy_card) = discard_exile_case();
        let mut privacy_events = Vec::new();
        effects::resolve_ability_chain(&mut privacy, &privacy_root, &mut privacy_events, 0)
            .expect("privacy initial resolution");
        apply_pending_action(
            &mut privacy,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![privacy_card],
            },
        )
        .expect("privacy discard choice");
        let owner_view = crate::game::visibility::filter_state_for_viewer(&privacy, PlayerId(0));
        let opponent_view = crate::game::visibility::filter_state_for_viewer(&privacy, PlayerId(1));
        assert_eq!(owner_view.objects[&privacy_card].zone, Zone::Graveyard);
        assert_eq!(opponent_view.objects[&privacy_card].zone, Zone::Hand);
        assert!(opponent_view.payment_transaction.is_none());
        let opponent_wire = serde_json::to_value(
            crate::game::derived_views::ClientGameStateRef::wrap(&privacy, Some(PlayerId(1))),
        )
        .expect("opponent wire projection");
        assert_eq!(
            opponent_wire["state"]["objects"][privacy_card.0.to_string()]["zone"],
            serde_json::to_value(Zone::Hand).expect("hand serializes")
        );

        // Even the prompt actor must not gain an identity that was hidden in
        // the canonical base. The shadow moves another player's hand card to a
        // public zone, but the viewer filter applies the canonical knowledge
        // authority before serializing either the filtered or direct wire.
        let (hidden_shadow, hidden_card) = hidden_shadow_case();
        let actor_view =
            crate::game::visibility::filter_state_for_viewer(&hidden_shadow, PlayerId(0));
        assert_eq!(actor_view.objects[&hidden_card].zone, Zone::Graveyard);
        assert_eq!(actor_view.objects[&hidden_card].name, "Hidden Card");
        let actor_wire = serde_json::to_value(
            crate::game::derived_views::ClientGameStateRef::wrap(&hidden_shadow, Some(PlayerId(0))),
        )
        .expect("actor wire projection");
        assert!(
            !actor_wire.to_string().contains("R5 hidden shadow card"),
            "actor wire leaked hidden identity: {actor_wire}"
        );

        // An unscoped client wire has no authenticated actor. It must retain
        // the canonical hidden card and never replay the staged hand-to-
        // graveyard mutation merely to serialize a snapshot.
        let unscoped_wire = serde_json::to_value(
            crate::game::derived_views::ClientGameStateRef::wrap(&hidden_shadow, None),
        )
        .expect("unscoped wire projection");
        assert_eq!(
            unscoped_wire["state"]["objects"][hidden_card.0.to_string()]["zone"],
            serde_json::to_value(Zone::Hand).expect("hand serializes")
        );
        assert_eq!(
            unscoped_wire["state"]["objects"][hidden_card.0.to_string()]["name"],
            "Hidden Card"
        );
        assert!(
            !unscoped_wire.to_string().contains("R5 hidden shadow card"),
            "unscoped wire leaked hidden identity: {unscoped_wire}"
        );
        assert!(unscoped_wire["state"].get("payment_transaction").is_none());

        // R5a discriminator: this is a reachable production path, not a
        // hand-built descriptor. A turn controller (Player 1) submits the
        // semantic payer's (Player 0) discard, so the controller cannot learn
        // the card identity merely because the tentative shadow moved it to a
        // public zone before the later energy failure.
        let (mut controlled, controlled_root, controlled_card) =
            controlled_discard_phyrexian_case();
        let mut controlled_events = Vec::new();
        effects::resolve_ability_chain(
            &mut controlled,
            &controlled_root,
            &mut controlled_events,
            0,
        )
        .expect("controlled hidden-information resolution reaches discard choice");
        assert!(matches!(
            controlled.waiting_for,
            crate::types::game_state::WaitingFor::DiscardChoice { .. }
        ));
        assert_eq!(controlled.objects[&controlled_card].zone, Zone::Hand);
        let discard_selection = GameAction::SelectCards {
            cards: vec![controlled_card],
        };
        crate::game::engine::apply_as_current(&mut controlled, discard_selection)
            .expect("authorized turn controller submits discard");
        assert!(matches!(
            controlled.waiting_for,
            crate::types::game_state::WaitingFor::ChooseOneOfBranch { .. }
        ));
        assert_eq!(controlled.objects[&controlled_card].zone, Zone::Hand);
        let unscoped_controlled_wire = serde_json::to_value(
            crate::game::derived_views::ClientGameStateRef::wrap(&controlled, None),
        )
        .expect("unscoped controlled wire projection");
        assert!(!unscoped_controlled_wire
            .to_string()
            .contains("R5 controller-hidden card"));
        assert!(matches!(
            serde_json::from_value::<crate::types::game_state::WaitingFor>(
                unscoped_controlled_wire["state"]["waiting_for"].clone()
            )
            .expect("public waiting prompt remains decodable"),
            crate::types::game_state::WaitingFor::ChooseOneOfBranch { .. }
        ));
        let semantic_view =
            crate::game::visibility::filter_state_for_viewer(&controlled, PlayerId(0));
        let controller_view =
            crate::game::visibility::filter_state_for_viewer(&controlled, PlayerId(1));
        assert_eq!(semantic_view.objects[&controlled_card].zone, Zone::Hand);
        assert_eq!(
            semantic_view.objects[&controlled_card].name,
            "R5 controller-hidden card"
        );
        assert_eq!(
            controller_view.objects[&controlled_card].zone,
            Zone::Graveyard
        );
        assert_eq!(
            controller_view.objects[&controlled_card].name,
            "R5 controller-hidden card"
        );
        let controller_wire = serde_json::to_value(
            crate::game::derived_views::ClientGameStateRef::wrap(&controlled, Some(PlayerId(1))),
        )
        .expect("controller wire projection");
        assert!(
            controller_wire
                .to_string()
                .contains("R5 controller-hidden card"),
            "authorized controller wire should retain controlled player's private identity: {controller_wire}"
        );
        let abort = crate::game::engine::apply_as_current(
            &mut controlled,
            GameAction::ChooseBranch { index: 0 },
        )
        .expect("replacement choice reaches failing energy suffix");
        assert_eq!(controlled.players[0].life, 23);
        assert_eq!(
            abort
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::LifeChanged {
                        player_id: PlayerId(0),
                        amount: 3,
                        ..
                    }
                ))
                .count(),
            1,
            "the unconditional abort sibling resolves exactly once"
        );
        assert!(
            !abort.events.iter().any(|event| matches!(
                event,
                GameEvent::LifeChanged { amount: 1, .. }
                    | GameEvent::LifeChanged { amount: -1, .. }
                    | GameEvent::EnergyChanged { .. }
            )),
            "discarded payment/replacement prefix must not publish events"
        );
        assert!(controlled.payment_transaction.is_none());
        assert_eq!(controlled.objects[&controlled_card].zone, Zone::Hand);
        assert!(matches!(
            controlled.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));

        // CR 104.3a + CR 800.4a: Concede bypasses the staged-payment
        // transcript and retires only when the payment owner leaves. The
        // two-player fixture reaches the active choice first, then the owner
        // concedes through the public boundary and the normal elimination
        // authority terminalizes the game without publishing payment events.
        let (mut concede, concede_root, _) = controlled_discard_phyrexian_case();
        let mut concede_setup_events = Vec::new();
        effects::resolve_ability_chain(&mut concede, &concede_root, &mut concede_setup_events, 0)
            .expect("concession witness reaches a staged payment prompt");
        let transaction_owner = concede
            .payment_transaction
            .as_ref()
            .expect("concession witness must be staged before Concede")
            .owner;
        assert_eq!(transaction_owner, PlayerId(0));
        assert!(!owns_action(
            &concede,
            &GameAction::Concede {
                player_id: transaction_owner,
            }
        ));
        assert!(
            !owns_action(&concede, &GameAction::PassPriority),
            "an unrelated non-Concede action must not enter the payment transcript"
        );
        let concede_result = crate::game::engine::apply_as_current(
            &mut concede,
            GameAction::Concede {
                player_id: transaction_owner,
            },
        )
        .expect("Concede bypasses the active payment prompt");
        assert!(concede.payment_transaction.is_none());
        assert!(matches!(
            concede.waiting_for,
            crate::types::game_state::WaitingFor::GameOver { .. }
        ));
        assert!(!concede_result.events.iter().any(|event| matches!(
            event,
            GameEvent::LifeChanged { .. } | GameEvent::EnergyChanged { .. }
        )));

        // Descriptor persistence is canonical+transcript only. Viewer and wire
        // projections materialize the public shadow and never expose the root.
        let (mut paused, paused_root) = phyrexian_energy_case(0);
        let mut paused_events = Vec::new();
        effects::resolve_ability_chain(&mut paused, &paused_root, &mut paused_events, 0)
            .expect("privacy pause");
        let mut persisted = serde_json::to_value(&paused).expect("transaction persistence");
        assert!(persisted.get("payment_transaction").is_some());
        assert!(persisted.get("payment_transaction_replay").is_none());
        assert!(persisted.get("payment_transaction_just_handled").is_none());
        let restored: GameState =
            serde_json::from_value(persisted.take()).expect("transaction restore");
        assert!(restored.payment_transaction.is_some());
        assert!(!restored.payment_transaction_replay);
        assert!(!restored.payment_transaction_just_handled);
        let filtered = crate::game::visibility::filter_state_for_viewer(&paused, PlayerId(1));
        assert!(filtered.payment_transaction.is_none());
        assert!(matches!(
            filtered.waiting_for,
            crate::types::game_state::WaitingFor::ChooseOneOfBranch { .. }
        ));
    }

    #[test]
    fn admitted_controller_choice_replays_after_controller_concedes() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_decision_controller = Some(PlayerId(1));
        state.turn_decision_control_timestamp = Some(0);
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: PlayerId(0),
                controller: PlayerId(1),
                timestamp: 0,
                grant_extra_turn_after: false,
                window: crate::types::ability::ControlWindow::NextTurn,
            });
        state.players[0].energy = 1;
        let card = create_object(
            &mut state,
            CardId(922),
            PlayerId(0),
            "R5 controller departure payment card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(923),
            PlayerId(0),
            "R5 controller departure alternate".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(924),
            PlayerId(0),
            "R5 controller departure source".to_string(),
            Zone::Battlefield,
        );
        let replacement_choice = AbilityDefinition::new(
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
        );
        state
            .objects
            .get_mut(&source)
            .expect("departure source")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(QuantityModification::Plus { value: 0 })
            .execute(replacement_choice)]
        .into();

        let root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: crate::types::ability::CardSelectionMode::Chosen,
                            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::Mana {
                            cost: ManaCost::Cost {
                                shards: vec![ManaCostShard::PhyrexianBlack],
                                generic: 0,
                            },
                        },
                        AbilityCost::PayEnergy {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &root, &mut events, 0)
            .expect("controlled payment reaches its first choice");
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::DiscardChoice { .. }
        ));

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![card] },
        )
        .expect("controller submits the admitted discard");
        let transaction = state
            .payment_transaction
            .as_ref()
            .expect("payment remains staged after the first choice");
        assert_eq!(transaction.transcript.len(), 1);
        assert_eq!(transaction.transcript[0].authenticated_actor, PlayerId(1));
        assert_eq!(transaction.transcript[0].semantic_owner, PlayerId(0));
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::ChooseOneOfBranch { .. }
        ));

        crate::game::engine::apply(
            &mut state,
            PlayerId(1),
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .expect("controller departure is independent of the payer transaction");
        assert!(state.payment_transaction.is_some());
        assert!(state.players.iter().any(|player| player.id == PlayerId(0)));

        let resumed = crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::ChooseBranch { index: 0 },
        )
        .expect("payer finishes after controller departure");
        assert!(state.payment_transaction.is_none());
        assert_eq!(state.objects[&card].zone, Zone::Graveyard);
        assert_eq!(state.players[0].energy, 0);
        assert_eq!(
            resumed
                .events
                .iter()
                .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == card))
                .count(),
            1,
            "the accepted controller choice replays once after departure"
        );
    }

    #[test]
    fn payer_departure_preserves_surviving_controller_continuation() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.players[0].life = 20;
        state.players[1].energy = 0;
        let payer_card = create_object(
            &mut state,
            CardId(925),
            PlayerId(1),
            "R5 payer departure card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(926),
            PlayerId(1),
            "R5 payer departure alternate".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(927),
            PlayerId(0),
            "R5 payer departure source".to_string(),
            Zone::Battlefield,
        );
        let mut root = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Discard {
                            count: QuantityExpr::Fixed { value: 1 },
                            filter: None,
                            selection: crate::types::ability::CardSelectionMode::Chosen,
                            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
                        },
                        AbilityCost::PayEnergy {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                    ],
                },
                scale: None,
                payer: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        );
        let mut gated = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        gated.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        let mut unconditional = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        unconditional.sub_link = SubAbilityLink::SequentialSibling;
        gated.sub_ability = Some(Box::new(unconditional));
        root.sub_ability = Some(Box::new(gated));

        let mut setup_events = Vec::new();
        effects::resolve_ability_chain(&mut state, &root, &mut setup_events, 0)
            .expect("payer departure witness reaches a staged discard prompt");
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::DiscardChoice {
                player: PlayerId(1),
                ..
            }
        ));
        assert_eq!(
            state
                .payment_transaction
                .as_ref()
                .expect("payer departure witness must stage before Concede")
                .owner,
            PlayerId(1)
        );

        let result = crate::game::engine::apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .expect("payer may concede during the staged payment prompt");

        assert!(state.payment_transaction.is_none());
        assert!(state.players[1].is_eliminated);
        assert!(!state.players[0].is_eliminated);
        assert_eq!(
            state.players[0].life, 23,
            "the surviving root controller keeps the unconditional continuation"
        );
        assert_eq!(state.objects[&payer_card].zone, Zone::Exile);
        assert!(!result.events.iter().any(|event| matches!(
            event,
            GameEvent::LifeChanged {
                player_id: PlayerId(0),
                amount: 2,
                ..
            }
        )));
        assert!(!result.events.iter().any(|event| matches!(
            event,
            GameEvent::Discarded { object_id, .. } if *object_id == payer_card
        )));
        assert!(result.events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Exile,
                ..
            } if *object_id == payer_card
        )));
    }

    #[test]
    fn successful_composite_does_not_roll_back_when_later_rider_fails() {
        let (mut state, root) = committed_composite_with_failing_rider_case();
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &root, &mut events, 0)
            .expect("composite and rider resolution");

        assert_eq!(
            state.players[0].life, 9,
            "the committed payment stays committed"
        );
        assert_eq!(state.players[0].energy, 0);
        assert!(state.payment_transaction.is_none());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::LifeChanged { amount: -1, .. }))
                .count(),
            1,
            "the successful Composite emits its event exactly once"
        );
    }

    #[test]
    fn successful_composite_stays_committed_across_later_rider_pause_and_failure() {
        let (mut state, root, card) = committed_composite_with_pausing_rider_case();
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &root, &mut events, 0)
            .expect("composite and rider reach discard choice");

        assert_eq!(state.players[0].life, 9);
        assert!(state.payment_transaction.is_none());
        let rider_continuation = state
            .active_ability_continuation()
            .expect("ordinary rider pause must own its continuation");
        assert!(matches!(
            rider_continuation.chain.effect,
            Effect::PayCost {
                cost: AbilityCost::PayEnergy { .. },
                ..
            }
        ));
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::DiscardChoice { .. }
        ));
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards { cards: vec![card] },
        )
        .expect("ordinary rider discard choice");
        assert_eq!(
            state.players[0].life, 9,
            "rider failure cannot restore the old base"
        );
        assert!(state.payment_transaction.is_none());
        assert!(state.cost_payment_failed_flag);
    }

    #[test]
    fn staged_payment_restores_trigger_context_after_serde_roundtrip() {
        let (mut state, mut root, card) = trigger_context_composite_case();
        let mut success_rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 2 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            root.source_id,
            root.controller,
        );
        success_rider.condition = Some(AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        });
        root.sub_ability = Some(Box::new(success_rider));
        let mut events = Vec::new();
        effects::resolve_ability_chain(&mut state, &root, &mut events, 0)
            .expect("context-dependent composite reaches discard choice");
        let transaction = state
            .payment_transaction
            .as_ref()
            .expect("context-dependent payment is staged");
        assert_eq!(transaction.owner, PlayerId(1));
        assert!(transaction.resolving_trigger_context.is_some());
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::DiscardChoice {
                player: PlayerId(1),
                ..
            }
        ));

        let serialized = serde_json::to_value(&state).expect("state serializes");
        let mut restored: GameState = serde_json::from_value(serialized).expect("state restores");
        // The live fields may be present in a local snapshot, but a persisted
        // server pause resumes after the stack driver has cleared them. Make
        // that boundary explicit so this witness proves the durable descriptor
        // is the source of truth for payer/amount context.
        restored.current_trigger_event = None;
        restored.current_trigger_events.clear();
        let resume = crate::game::engine::apply_as_current(
            &mut restored,
            GameAction::SelectCards { cards: vec![card] },
        )
        .expect("restored context resumes the staged payment");

        assert_eq!(restored.players[1].life, 17);
        assert_eq!(
            restored.players[0].life, 22,
            "a successful staged payment must re-enter the ordinary parent-to-rider outcome gate"
        );
        assert_eq!(restored.objects[&card].zone, Zone::Graveyard);
        assert!(restored.payment_transaction.is_none());
        assert!(matches!(
            restored.waiting_for,
            crate::types::game_state::WaitingFor::Priority { .. }
        ));
        assert_eq!(
            resume
                .events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::LifeChanged {
                        player_id: PlayerId(1),
                        amount: -3,
                        ..
                    }
                ))
                .count(),
            1,
            "the restored EventContextAmount drives the same life payment"
        );
    }

    // -----------------------------------------------------------------------
    // CR 608.2c + CR 118.12: the staged shadow re-materializes only the payment
    // the chain walker already admitted; it never re-evaluates the root's
    // condition.
    // -----------------------------------------------------------------------

    mod staged_root_condition {
        use super::*;
        use crate::game::scenario::{GameRunner, GameScenario, P0};
        use crate::types::ability::{Comparator, QuantityRef};
        use crate::types::game_state::WaitingFor;
        use crate::types::phase::Phase;

        /// Stinkweed Imp, verbatim Oracle text: Dredge 5 makes each of P0's draws a replacement choice.
        const STINKWEED_IMP_ORACLE: &str = "Flying\n\
            Whenever this creature deals combat damage to a creature, destroy that creature.\n\
            Dredge 5 (If you would draw a card, you may mill five cards instead. If you do, \
            return this card from your graveyard to your hand.)";

        const LIBRARY: [&str; 12] = [
            "L1", "L2", "L3", "L4", "L5", "L6", "L7", "L8", "L9", "L10", "L11", "L12",
        ];

        fn draw_cost() -> AbilityCost {
            AbilityCost::EffectCost {
                effect: Box::new(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                }),
            }
        }

        fn two_draw_payment(source: ObjectId) -> ResolvedAbility {
            ResolvedAbility::new(
                Effect::PayCost {
                    cost: AbilityCost::Composite {
                        costs: vec![draw_cost(), draw_cost()],
                    },
                    scale: None,
                    payer: TargetFilter::Controller,
                },
                vec![],
                source,
                P0,
            )
        }

        fn gain_life(source: ObjectId, amount: i32) -> ResolvedAbility {
            ResolvedAbility::new(
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: amount },
                    player: TargetFilter::Controller,
                },
                vec![],
                source,
                P0,
            )
        }

        fn opponents_at_least(count: i32) -> AbilityCondition {
            AbilityCondition::QuantityCheck {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::PlayerCount {
                        filter: PlayerFilter::Opponent,
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: count },
            }
        }

        fn hand_size(state: &GameState) -> usize {
            state.players[P0.0 as usize].hand.len()
        }

        /// Three players, every library staged, Stinkweed Imp in P0's graveyard, layers evaluated.
        fn three_player_dredge_board() -> (GameRunner, ObjectId) {
            let mut scenario = GameScenario::new_n_player(3, 42);
            scenario.at_phase(Phase::PreCombatMain);
            for player in 0..3 {
                scenario.with_library_top(PlayerId(player), &LIBRARY);
            }
            let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
            scenario
                .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
                .from_oracle_text(STINKWEED_IMP_ORACLE);
            let mut runner = scenario.build();
            crate::game::layers::evaluate_layers(runner.state_mut());
            (runner, source)
        }

        /// CR 608.2c + CR 118.12 + CR 104.3a + CR 800.4a: another player's concession falsifies the staged
        /// root's own condition between `begin` and `replay`. The walker already admitted the instruction,
        /// so the replayed payment still resumes and every recorded answer still applies.
        ///
        /// Revert probe: keeping `condition` in `payment_only_root` makes the replay skip the payment, so the
        /// first decline is rejected ("replacement choice has no pending replacement") and the transaction
        /// stays live.
        #[test]
        fn a_concession_that_falsifies_the_staged_roots_condition_does_not_wedge_the_payment() {
            let (mut runner, source) = three_player_dredge_board();
            let condition = opponents_at_least(2);
            let mut root = two_draw_payment(source);
            root.condition = Some(condition.clone());
            root.sub_ability = Some(Box::new(gain_life(source, 3)));
            let life_before = runner.life(P0);
            let state = runner.state_mut();
            let hand_before = hand_size(state);

            effects::resolve_ability_chain(state, &root, &mut Vec::new(), 0).unwrap();
            assert!(
                state.payment_transaction.is_some(),
                "the walker staged the payment"
            );
            assert!(matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice { player, .. } if player == P0
            ));
            assert!(effects::evaluate_condition(&condition, state, &root));

            let concession = engine::apply(
                state,
                PlayerId(2),
                GameAction::Concede {
                    player_id: PlayerId(2),
                },
            );
            assert!(concession.is_ok(), "{concession:?}");
            assert!(
                !effects::evaluate_condition(&condition, state, &root),
                "the concession flipped the condition's input on canonical state"
            );
            assert!(matches!(
                project(state).waiting_for,
                WaitingFor::ReplacementChoice { player, .. } if player == P0
            ));

            for leg in 1..=2 {
                let decline = engine::apply(state, P0, GameAction::ChooseReplacement { index: 1 });
                assert!(decline.is_ok(), "decline {leg}: {decline:?}");
            }
            assert_eq!(
                hand_size(state),
                hand_before + 2,
                "both draw legs were paid"
            );
            assert!(!state.cost_payment_failed_flag);
            assert!(state.payment_transaction.is_none());
            assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
            assert_eq!(
                runner.life(P0),
                life_before + 3,
                "the rider runs exactly once"
            );
        }

        /// CR 608.2c: the walker still refuses a staged root whose condition is false from the start, before
        /// `pay::resolve`, so no transaction opens.
        #[test]
        fn a_staged_root_whose_condition_is_false_never_opens_a_transaction() {
            let (mut runner, source) = three_player_dredge_board();
            let mut root = two_draw_payment(source);
            root.condition = Some(opponents_at_least(3));
            let state = runner.state_mut();
            let hand_before = hand_size(state);
            effects::resolve_ability_chain(state, &root, &mut Vec::new(), 0).unwrap();
            assert!(state.payment_transaction.is_none());
            assert_eq!(hand_size(state), hand_before);
            assert!(matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0));
        }

        fn previous_amount_at_least(count: i32) -> AbilityCondition {
            AbilityCondition::PreviousEffectAmount {
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: count },
                channel: Default::default(),
            }
        }

        /// GainLife 2 → PayCost{Composite[Draw 1, Draw 1]} (gated on the previous amount) → GainLife 7, by
        /// P0 on an unrestricted two-player board. Returns (hand delta, life delta, transaction left).
        fn resolve_gated_staged_payment(threshold: i32) -> (usize, i32, bool) {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_library_top(P0, &LIBRARY);
            let source = scenario.add_creature(P0, "Draw Cost Source", 1, 1).id();
            let mut runner = scenario.build();
            let mut payment = two_draw_payment(source);
            payment.condition = Some(previous_amount_at_least(threshold));
            payment.sub_ability = Some(Box::new(gain_life(source, 7)));
            let mut root = gain_life(source, 2);
            root.sub_ability = Some(Box::new(payment));
            let life_before = runner.life(P0);
            let state = runner.state_mut();
            let hand_before = hand_size(state);
            effects::resolve_ability_chain(state, &root, &mut Vec::new(), 0).unwrap();
            assert!(!state.cost_payment_failed_flag);
            let hand_delta = hand_size(state) - hand_before;
            let transaction_left = state.payment_transaction.is_some();
            (hand_delta, runner.life(P0) - life_before, transaction_left)
        }

        /// CR 608.2c + CR 118.12: `begin`'s shadow resolves the payment at depth 0, where the resolution-local
        /// state its condition reads has been reset. The walker's admission stands, so both legs are paid.
        ///
        /// Revert probe: keeping `condition` in `payment_only_root` makes the shadow skip the payment and
        /// `begin` commit it as a synchronous success with nothing drawn (hand +0).
        #[test]
        fn a_staged_payment_honours_a_condition_read_from_the_previous_instruction() {
            assert_eq!(
                resolve_gated_staged_payment(1),
                (2, 9, false),
                "the admitted payment draws both legs, then the rider runs"
            );
            // Negative sibling: the walker refuses a false condition (2 < 5) and skips the payment's rider.
            assert_eq!(resolve_gated_staged_payment(5), (0, 2, false));
        }
    }
}
