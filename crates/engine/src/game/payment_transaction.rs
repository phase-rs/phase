//! Replayable, staged resolution-time composite payments.
//!
//! The descriptor in [`GameState::payment_transaction`] is the only durable
//! transaction authority. It stores the untouched base implicitly (the live
//! state) plus a serializable resolution root and the actions already accepted
//! against its shadow. Every materialization starts from that base and runs the
//! normal resolution/action pipeline; no second payment implementation lives
//! here.

use crate::game::effects;
use crate::game::engine::{self, EngineError};
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, GameState, ResolutionPaymentTransaction, ResolutionPaymentTranscriptEntry,
    ResolvingTriggerContext,
};
use crate::types::log::GameLogEntry;

#[derive(Debug)]
enum ReplayOutcome {
    Paused {
        shadow: GameState,
    },
    Completed {
        shadow: GameState,
        events: Vec<GameEvent>,
        log_entries: Vec<GameLogEntry>,
    },
    Failed,
}

fn replay_error(error: EffectError) -> EngineError {
    EngineError::InvalidAction(format!(
        "staged payment transaction replay failed: {error:?}"
    ))
}

/// The transaction boundary owns only the Composite PayCost instruction. All
/// outgoing chain edges stay on the durable root so the ordinary chain walker
/// can resume them after the payment commits or aborts.
fn payment_only_root(ability: &ResolvedAbility) -> ResolvedAbility {
    let mut payment = ability.clone();
    payment.sub_ability = None;
    payment.else_ability = None;
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
    };
    *state = base;
    state.payment_transaction = Some(Box::new(transaction));
    state.waiting_for = shadow.waiting_for;
    state.cost_payment_failed_flag = false;
    state.payment_transaction_replay = false;
    state.payment_transaction_just_handled = true;
    Ok(())
}

fn replay(
    state: &GameState,
    transaction: &ResolutionPaymentTransaction,
) -> Result<ReplayOutcome, EngineError> {
    let mut shadow = state.clone();
    shadow.payment_transaction = None;
    shadow.payment_transaction_replay = true;
    shadow.payment_transaction_just_handled = false;
    shadow.cost_payment_failed_flag = false;
    shadow.waiting_for = transaction.base_waiting_for.clone();
    restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());

    let mut buffered_events = Vec::new();
    let mut buffered_log_entries: Vec<GameLogEntry> = Vec::new();
    let payment_root = payment_only_root(&transaction.root);
    // CR 608.2c + decision 1: replaying the root is the paused clause's own
    // frame resumed on the shadow; the transcript's answers attribute through
    // their answer sites.
    effects::attribute_to_paused_clause(
        &mut shadow,
        &mut buffered_events,
        effects::ClauseSite::Resumption,
        |shadow, buffered_events| {
            effects::resolve_ability_chain(shadow, &payment_root, buffered_events, 0)
        },
    )
    .map_err(replay_error)?;
    restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
    // Keep the replay guard live while transcript actions drain the suspended
    // continuation. A remaining Composite must use the ordinary sequential
    // shadow authority, never open a nested descriptor of its own.
    shadow.payment_transaction_replay = true;

    if shadow.cost_payment_failed_flag {
        return Ok(ReplayOutcome::Failed);
    }

    for entry in &transaction.transcript {
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
            return Ok(ReplayOutcome::Failed);
        }
        shadow.payment_transaction_replay = true;
        restore_trigger_context(&mut shadow, transaction.resolving_trigger_context.as_ref());
    }

    shadow.payment_transaction_replay = false;

    if returned_to_base_waiting(&shadow, &transaction.base_waiting_for) {
        Ok(ReplayOutcome::Completed {
            shadow,
            events: buffered_events,
            log_entries: buffered_log_entries,
        })
    } else {
        Ok(ReplayOutcome::Paused { shadow })
    }
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
    // CR 608.2c + decision 1: the rider is the paused instruction's own
    // continuation on every path (committed, failed, abandoned by a departure).
    effects::attribute_to_paused_clause(
        state,
        events,
        effects::ClauseSite::Resumption,
        |state, events| effects::resolve_ability_chain(state, &continuation, events, 1),
    )
    .map_err(replay_error)
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

    match replay(state, &transaction)? {
        ReplayOutcome::Failed => {
            let mut restored = state.clone();
            restored.payment_transaction = None;
            restored.payment_transaction_replay = false;
            restored.payment_transaction_just_handled = false;
            restored.waiting_for = transaction.base_waiting_for;
            restored.cost_payment_failed_flag = true;
            *state = restored;
            let mut continuation_events = Vec::new();
            resolve_root_continuation(
                state,
                &transaction.root,
                false,
                transaction.resolving_trigger_context.as_ref(),
                &mut continuation_events,
            )?;
            Ok(ActionResult::applied(
                continuation_events,
                state.waiting_for.clone(),
            ))
        }
        ReplayOutcome::Paused { shadow, .. } => {
            let waiting_for = shadow.waiting_for.clone();
            let mut projected = state.clone();
            projected.payment_transaction = Some(Box::new(transaction));
            projected.waiting_for = waiting_for.clone();
            projected.cost_payment_failed_flag = false;
            projected.payment_transaction_replay = false;
            projected.payment_transaction_just_handled = false;
            *state = projected;
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
            let waiting_for = shadow.waiting_for.clone();
            *state = shadow;
            Ok(ActionResult::applied(events, waiting_for).with_log_entries(log_entries))
        }
    }
}

/// Materialize the public shadow used by viewer projections, client wires, and
/// legal-action enumeration. A replay failure fails closed to the untouched
/// base rather than exposing a partially mutated shadow.
pub(crate) fn project(state: &GameState) -> GameState {
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
        Ok(ReplayOutcome::Failed) | Err(_) => {
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
    let mut probe = project(state);
    // The probe must exercise the ordinary WaitingFor/action authority on the
    // materialized shadow, not recursively re-enter this transaction or create
    // a nested descriptor. `project` has already cleared the transaction and
    // transient guards while preserving the prompt's continuation payload.
    probe.payment_transaction = None;
    probe.payment_transaction_replay = true;
    probe.payment_transaction_just_handled = false;
    crate::game::engine::apply_recorded_action(&mut probe, actor, action.clone()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::game::deck_loading::DeckEntry;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, Effect, EffectOutcomeSignal,
        PlayerFilter, QuantityExpr, QuantityModification, ReplacementDefinition, SubAbilityLink,
        TargetFilter, TargetRef,
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

    /// What the parked owner's completion reports, through its tail.
    #[derive(Clone, Copy)]
    enum Measure {
        /// The life the clause's span records as lost (CR 119.3).
        LifeLost,
        /// The life the clause's span records as gained (CR 119.3).
        LifeGained,
    }

    /// Parks a clause owner with its remaining legs above it (decision 5
    /// placement). Its publication template is a life instruction, so on
    /// completion the clause publishes, as `PreviousEffectAmount`, the life its
    /// span recorded as lost (or gained); its tail then gives P1 exactly that
    /// much life. P1's life is touched by nothing else, so its gain reads the
    /// clause span: each attributed event counted once.
    fn park_owner_below_legs(state: &mut GameState, root: &ResolvedAbility, measure: Measure) {
        use crate::types::ability::{AggregateFunction, DamageChannel, QuantityRef};
        use crate::types::resolution::{
            ClauseAttribution, ClausePublication, PendingPlayerScopeClause, ResolutionFrame,
        };
        let fixed = QuantityExpr::Fixed { value: 1 };
        let template_effect = match measure {
            Measure::LifeLost => Effect::LoseLife {
                amount: fixed.clone(),
                target: None,
            },
            Measure::LifeGained => Effect::GainLife {
                amount: fixed.clone(),
                player: TargetFilter::Controller,
            },
        };
        let template =
            ResolvedAbility::new(template_effect, Vec::new(), root.source_id, PlayerId(0));
        let tail = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::PreviousEffectAmount {
                        channel: DamageChannel::Total,
                        aggregate: AggregateFunction::Sum,
                    },
                },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            root.source_id,
            PlayerId(1),
        );
        let owner = PendingPlayerScopeClause {
            source_id: root.source_id,
            tail: Box::new(crate::types::game_state::PendingContinuation::new(
                Box::new(tail),
                state,
            )),
            publication: ClausePublication::Recorded {
                outer: Box::new(template.clone()),
                scoped_template: Box::new(template),
                matching_players: vec![PlayerId(0)],
            },
            events: Vec::new(),
            raised_prompt: None,
            attribution: ClauseAttribution::Closed,
        };
        state
            .resolution_stack
            .push_inner(ResolutionFrame::PlayerScopeClause(Box::new(owner)));
        let legs = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 100 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            root.source_id,
            PlayerId(0),
        );
        state.park_ability_continuation(crate::types::game_state::PendingContinuation::new(
            Box::new(legs),
            state,
        ));
    }

    /// Runs `root` as the clause's own leg (a resumption bracket), as the driver
    /// does when it resumes the clause's frames.
    fn resolve_as_clause_leg(state: &mut GameState, root: &ResolvedAbility) {
        let mut events = Vec::new();
        effects::attribute_to_paused_clause(
            state,
            &mut events,
            effects::ClauseSite::Resumption,
            |state, events| effects::resolve_ability_chain(state, root, events, 0),
        )
        .expect("the leg resolves");
    }

    /// The life the completed clause reported to P1, after the action settles.
    fn reported_by_clause(mut state: GameState, action: GameAction) -> (i32, GameState) {
        let before = state.players[1].life;
        crate::game::engine::apply_as_current(&mut state, action)
            .expect("the payment answer is accepted");
        (state.players[1].life - before, state)
    }

    /// CR 608.2c + decision 1 (R1.12): a staged payment inside a clause leg
    /// attributes each of its events to the clause exactly once — the replayed
    /// root and the transcript's answer on the commit, and the rider on every
    /// path. The owner reports the attributed life through its tail.
    #[test]
    fn staged_payment_in_a_clause_leg_attributes_each_event_once() {
        // Committed: the Phyrexian shard is paid with 2 life, once.
        let (mut state, root) = phyrexian_energy_case(1);
        park_owner_below_legs(&mut state, &root, Measure::LifeLost);
        resolve_as_clause_leg(&mut state, &root);
        assert!(
            state.payment_transaction.is_some(),
            "reach guard: the payment is staged"
        );
        let (reported, state) = reported_by_clause(state, GameAction::ChooseBranch { index: 0 });
        assert!(
            state
                .resolution_stack
                .topmost_player_scope_clause()
                .is_none(),
            "reach guard: the clause completed"
        );
        assert_eq!(
            reported, 2,
            "the shard's 2 life is the clause's work, counted once"
        );

        // Failed: no energy, so the composite cannot complete. The payment's
        // events never happened; the rider (7) runs and is the clause's work. The
        // owner stays parked under its legs, so its span is read directly.
        let (mut state, root) = phyrexian_energy_case(0);
        park_owner_below_legs(&mut state, &root, Measure::LifeGained);
        resolve_as_clause_leg(&mut state, &root);
        assert!(
            state.payment_transaction.is_some(),
            "reach guard: the payment is staged"
        );
        crate::game::engine::apply_as_current(&mut state, GameAction::ChooseBranch { index: 0 })
            .expect("the branch choice settles the failed payment");
        assert!(
            state.payment_transaction.is_none(),
            "reach guard: the payment settled"
        );
        assert_eq!(owner_life_changes(&state), vec![7], "only the rider, once");

        // Synchronous: both 6-life payments complete inside the leg; the legs and
        // the owner then resume as the driver would.
        let (mut state, root) = life_composite_case(13);
        park_owner_below_legs(&mut state, &root, Measure::LifeLost);
        let before = state.players[1].life;
        resolve_as_clause_leg(&mut state, &root);
        assert!(
            state.payment_transaction.is_none(),
            "reach guard: nothing was staged"
        );
        let mut events = Vec::new();
        effects::drain_pending_continuation(&mut state, &mut events);
        effects::resume_resolution_frames(&mut state, &mut events);
        assert!(state
            .resolution_stack
            .topmost_player_scope_clause()
            .is_none());
        assert_eq!(
            state.players[1].life - before,
            12,
            "both payments, once each"
        );
    }

    /// The life changes in the parked owner's span, in order.
    fn owner_life_changes(state: &GameState) -> Vec<i32> {
        let owner = state
            .resolution_stack
            .topmost_player_scope_clause()
            .expect("the owner is still parked");
        owner
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged { amount, .. } => Some(*amount),
                _ => None,
            })
            .collect()
    }

    /// CR 104.3a + CR 800.4a + decision 1 (R1.12): the payer leaves while the
    /// payment is staged. The abandoned rider is still the clause's work; the
    /// departure is not.
    #[test]
    fn staged_payment_payer_departure_keeps_only_the_rider() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.players[1].energy = 0;
        let payer_card = create_object(
            &mut state,
            CardId(925),
            PlayerId(1),
            "payer card".to_string(),
            Zone::Hand,
        );
        create_object(
            &mut state,
            CardId(926),
            PlayerId(1),
            "payer alternate".to_string(),
            Zone::Hand,
        );
        let source = create_object(
            &mut state,
            CardId(927),
            PlayerId(0),
            "source".to_string(),
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
        let mut rider = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        rider.sub_link = SubAbilityLink::SequentialSibling;
        root.sub_ability = Some(Box::new(rider));
        park_owner_below_legs(&mut state, &root, Measure::LifeGained);
        resolve_as_clause_leg(&mut state, &root);
        assert!(
            state.payment_transaction.is_some(),
            "reach guard: the payment is staged"
        );
        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .expect("the payer concedes");
        assert_eq!(
            state.objects.get(&payer_card).map(|object| object.zone),
            Some(Zone::Exile),
            "reach guard: the payer's card left the game with them"
        );
        assert_eq!(
            owner_life_changes(&state),
            vec![3],
            "only the abandoned rider is clause work"
        );
        let owner = state
            .resolution_stack
            .topmost_player_scope_clause()
            .expect("the owner is still parked");
        assert!(
            !owner.events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == payer_card
            )),
            "the departure is not the clause's work"
        );
    }
}
