//! Reducer-backed safety witness for AI payment continuations.
//!
//! The engine is the sole authority for mana spending restrictions, payment
//! ordering, and the nested mana-ability state machine. This module therefore
//! never estimates capacity or infers payable colors: it accepts an AI edge
//! only after bounded reducer simulation reaches the matching root's real stack
//! finalization.

use std::collections::BTreeSet;

use crate::ai_support::legal_actions;
use crate::game::engine::apply_as_current_for_simulation;
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CollectEvidenceResume, CostResume, DeferredLifeCostResume, GameState, ManaAbilityCostCursor,
    ManaAbilityResume, ManaChoiceContext, PendingCast, PendingCostMoveResume, PendingManaAbility,
    StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;

/// Maximum reducer applications made while witnessing one proposed successor.
///
/// The supplied action counts as one application. The bound is per witness,
/// not per candidate list: callers that inspect `A` raw payment candidates may
/// therefore cause at most `PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS * A`
/// applications.
pub const PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS: usize = 64;

/// Mode-free identity of the announced spell or activated ability being paid.
///
/// CR 601.2f–i / CR 602.2b: the total cost is locked, paid, and then finalized
/// as the specific announced spell or activated ability. `ConvokeMode` is an
/// immediate-carrier grammar guard, not root identity: several later carriers
/// do not preserve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentContinuationRoot {
    Spell {
        object_id: ObjectId,
        card_id: CardId,
        payer: PlayerId,
    },
    Activation {
        source_id: ObjectId,
        ability_index: usize,
        payer: PlayerId,
    },
}

/// A payment state classification for AI consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentContinuationState {
    /// This is not one of the spell/activation mana-payment carriers owned by
    /// this oracle. Existing one-step AI behavior remains authoritative.
    NotAffiliated,
    /// The state is a supported carrier for this exact payment root.
    Affiliated(PaymentContinuationRoot),
    /// The state advertises an in-flight payment root, but its typed authority
    /// cannot prove the root safely. Consumers must fail closed, never fall
    /// through to an unrelated first-legal policy.
    UnsupportedAffiliated(PaymentContinuationUnsupported),
}

/// Why an affiliated-looking carrier cannot be witnessed safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentContinuationUnsupported {
    MissingPendingCast,
    MissingOuterPayer,
    PayerMismatch,
    RootMismatch,
    UnsupportedDeferredManaRoot,
    MissingSpellPlaceholder,
}

/// An accepted edge together with its already-applied immediate successor.
///
/// Reusing this state prevents AI callers from applying the selected reducer
/// edge a second time after the oracle already proved its completion witness.
#[derive(Debug, Clone)]
pub struct AcceptedPaymentSuccessor {
    pub action: GameAction,
    pub state: GameState,
}

/// Classify the current payment carrier without guessing cross-carrier state.
pub fn classify_payment_continuation(state: &GameState) -> PaymentContinuationState {
    if let Some(deferred) = state.pending_deferred_life_cost_resume.as_ref() {
        return classify_deferred_life_root(state, deferred);
    }

    match &state.waiting_for {
        // CR 601.2g–h: during the ordinary mana-payment window, the visible
        // payer and live pending cast jointly identify the payment root.
        WaitingFor::ManaPayment { player, .. } | WaitingFor::ManaSourceSelection { player, .. } => {
            classify_global_root(state, *player)
        }
        // CR 601.2f–h: submitting Phyrexian choices remains part of the same
        // cost payment. The prompt's object must agree with the announced root.
        WaitingFor::PhyrexianPayment {
            player,
            spell_object,
            ..
        } => match root_from_global(state, *player) {
            Ok(root) if root.object_id() == *spell_object => {
                PaymentContinuationState::Affiliated(root)
            }
            Ok(_) => PaymentContinuationState::UnsupportedAffiliated(
                PaymentContinuationUnsupported::RootMismatch,
            ),
            Err(reason) => PaymentContinuationState::UnsupportedAffiliated(reason),
        },
        WaitingFor::PayCost {
            resume: CostResume::ManaAbility { mana_ability },
            ..
        }
        | WaitingFor::PayManaAbilityMana {
            pending_mana_ability: mana_ability,
            ..
        }
        | WaitingFor::PayAmountChoice {
            pending_mana_ability: Some(mana_ability),
            ..
        } => classify_pending_mana_ability(state, mana_ability),
        WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(mana_ability),
            ..
        } => classify_pending_mana_ability(state, mana_ability),
        WaitingFor::CollectEvidenceChoice { resume, .. } => match resume.as_ref() {
            CollectEvidenceResume::ManaAbility {
                pending_mana_ability,
            } => classify_pending_mana_ability(state, pending_mana_ability),
            CollectEvidenceResume::Casting { .. } | CollectEvidenceResume::Effect { .. } => {
                PaymentContinuationState::NotAffiliated
            }
        },
        _ => classify_parked_cost_move_root(state),
    }
}

/// Return an already-applied successor only when a bounded reducer search can
/// finish the same announced root.
///
/// CR 601.2h / CR 602.2b: partial payment cannot certify success. The witness
/// rejects cancellation and requires the actual spell/ability finalization
/// delta after the original root has disappeared from every typed authority.
pub fn witness_payment_continuation(
    state: &GameState,
    action: &GameAction,
) -> Option<AcceptedPaymentSuccessor> {
    let PaymentContinuationState::Affiliated(root) = classify_payment_continuation(state) else {
        return None;
    };
    if matches!(action, GameAction::CancelCast) {
        return None;
    }

    let baseline = WitnessBaseline::capture(state, &root)?;
    let mut successor = state.clone();
    let first_result = apply_as_current_for_simulation(&mut successor, action.clone()).ok()?;
    let mut attempts = 1;
    record_witness_attempts(attempts);
    let mut events = first_result.events;

    if witness_completion(&successor, &root, &baseline, &mut events, &mut attempts) {
        record_witness_attempts(attempts);
        Some(AcceptedPaymentSuccessor {
            action: action.clone(),
            state: successor,
        })
    } else {
        record_witness_attempts(attempts);
        None
    }
}

#[derive(Debug, Clone)]
struct WitnessBaseline {
    pre_stack_ids: BTreeSet<ObjectId>,
    completion: CompletionBaseline,
}

#[derive(Debug, Clone)]
enum CompletionBaseline {
    Spell {
        entry_id: ObjectId,
        object_id: ObjectId,
        card_id: CardId,
        controller: PlayerId,
    },
    Activation,
}

impl WitnessBaseline {
    fn capture(state: &GameState, root: &PaymentContinuationRoot) -> Option<Self> {
        let pre_stack_ids = state.stack.iter().map(|entry| entry.id).collect();
        let completion = match root {
            PaymentContinuationRoot::Spell {
                object_id,
                card_id,
                payer,
            } => {
                let entry = state.stack.iter().find(|entry| entry.id == *object_id)?;
                let StackEntryKind::Spell {
                    card_id: entry_card_id,
                    ability: None,
                    actual_mana_spent: 0,
                    ..
                } = &entry.kind
                else {
                    return None;
                };
                if *entry_card_id != *card_id
                    || entry.controller != *payer
                    || state.stack_paid_facts.contains_key(object_id)
                {
                    return None;
                }
                CompletionBaseline::Spell {
                    entry_id: entry.id,
                    object_id: *object_id,
                    card_id: *card_id,
                    controller: *payer,
                }
            }
            PaymentContinuationRoot::Activation { .. } => CompletionBaseline::Activation,
        };
        Some(Self {
            pre_stack_ids,
            completion,
        })
    }
}

fn witness_completion(
    state: &GameState,
    root: &PaymentContinuationRoot,
    baseline: &WitnessBaseline,
    events: &mut Vec<GameEvent>,
    attempts: &mut usize,
) -> bool {
    if !root_present(state, root) {
        return finalized_root_matches(state, root, baseline, events);
    }
    if *attempts >= PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS {
        return false;
    }

    match classify_payment_continuation(state) {
        PaymentContinuationState::Affiliated(current_root) if current_root == *root => {}
        PaymentContinuationState::Affiliated(_)
        | PaymentContinuationState::NotAffiliated
        | PaymentContinuationState::UnsupportedAffiliated(_) => return false,
    }

    let mut actions = legal_actions(state);
    actions.sort_by(|left, right| left.cmp_stable(right));
    for next_action in actions {
        if matches!(next_action, GameAction::CancelCast)
            || *attempts >= PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS
        {
            continue;
        }

        *attempts += 1;
        let mut next_state = state.clone();
        let Ok(result) = apply_as_current_for_simulation(&mut next_state, next_action) else {
            continue;
        };
        let event_len = events.len();
        events.extend(result.events);
        if witness_completion(&next_state, root, baseline, events, attempts) {
            return true;
        }
        events.truncate(event_len);
    }
    false
}

fn finalized_root_matches(
    state: &GameState,
    root: &PaymentContinuationRoot,
    baseline: &WitnessBaseline,
    events: &[GameEvent],
) -> bool {
    match (&baseline.completion, root) {
        (
            CompletionBaseline::Spell {
                entry_id,
                object_id,
                card_id,
                controller,
            },
            PaymentContinuationRoot::Spell { .. },
        ) => {
            let Some(entry) = state.stack.iter().find(|entry| entry.id == *entry_id) else {
                return false;
            };
            let StackEntryKind::Spell {
                card_id: entry_card_id,
                actual_mana_spent: entry_spent,
                ..
            } = &entry.kind
            else {
                return false;
            };
            let Some(paid) = state.stack_paid_facts.get(object_id) else {
                return false;
            };
            *entry_card_id == *card_id
                && entry.controller == *controller
                && *entry_spent == paid.actual_mana_spent
                && events.iter().any(|event| {
                    matches!(
                        event,
                        GameEvent::SpellCast {
                            card_id: event_card_id,
                            controller: event_controller,
                            object_id: event_object_id,
                            ..
                        } if *event_card_id == *card_id
                            && *event_controller == *controller
                            && *event_object_id == *object_id
                    )
                })
        }
        (
            CompletionBaseline::Activation,
            PaymentContinuationRoot::Activation {
                source_id,
                ability_index,
                payer,
            },
        ) => {
            state.stack.iter().any(|entry| {
                !baseline.pre_stack_ids.contains(&entry.id)
                    && entry.source_id == *source_id
                    && entry.controller == *payer
                    && matches!(
                        &entry.kind,
                        StackEntryKind::ActivatedAbility {
                            source_id: entry_source_id,
                            ability,
                        } if *entry_source_id == *source_id
                            && ability.ability_index == Some(*ability_index)
                    )
            }) && events.iter().any(|event| {
                matches!(
                    event,
                    GameEvent::AbilityActivated {
                        player_id,
                        source_id: event_source_id,
                        ..
                    } if *player_id == *payer && *event_source_id == *source_id
                )
            })
        }
        _ => false,
    }
}

fn classify_global_root(state: &GameState, payer: PlayerId) -> PaymentContinuationState {
    match root_from_global(state, payer) {
        Ok(root) => PaymentContinuationState::Affiliated(root),
        Err(reason) => PaymentContinuationState::UnsupportedAffiliated(reason),
    }
}

fn classify_pending_mana_ability(
    state: &GameState,
    pending: &PendingManaAbility,
) -> PaymentContinuationState {
    match root_from_pending_mana_ability(state, pending) {
        Ok(Some(root)) => PaymentContinuationState::Affiliated(root),
        Ok(None) => PaymentContinuationState::NotAffiliated,
        Err(reason) => PaymentContinuationState::UnsupportedAffiliated(reason),
    }
}

fn classify_parked_cost_move_root(state: &GameState) -> PaymentContinuationState {
    let Some(resume) = state.pending_cost_move_resume.as_ref() else {
        return PaymentContinuationState::NotAffiliated;
    };
    match resume {
        PendingCostMoveResume::ManaAbilityPayment { pending, cursor } => {
            match root_from_pending_mana_and_cursor(state, pending, cursor) {
                Ok(Some(root)) => PaymentContinuationState::Affiliated(root),
                Ok(None) => PaymentContinuationState::NotAffiliated,
                Err(reason) => PaymentContinuationState::UnsupportedAffiliated(reason),
            }
        }
        PendingCostMoveResume::CollectEvidencePayment { resume, .. } => match resume.as_ref() {
            CollectEvidenceResume::ManaAbility {
                pending_mana_ability,
            } => classify_pending_mana_ability(state, pending_mana_ability),
            CollectEvidenceResume::Casting { .. } | CollectEvidenceResume::Effect { .. } => {
                PaymentContinuationState::NotAffiliated
            }
        },
        PendingCostMoveResume::DelveManaPayment { player, .. } => {
            classify_global_root(state, *player)
        }
        // CR 602.2b: The parked mill leg retains the announced activation's
        // serialized payment root until the replacement choice completes.
        PendingCostMoveResume::ActivationMillPayment { player, pending } => {
            PaymentContinuationState::Affiliated(root_from_pending_cast(pending, *player))
        }
        // These are distinct cost/resolution continuations. They intentionally
        // retain their existing policies rather than being misidentified from a
        // coincidental PendingCast elsewhere in state.
        PendingCostMoveResume::Cast { .. }
        | PendingCostMoveResume::SacrificeForCost { .. }
        | PendingCostMoveResume::WardSacrificePayment { .. }
        | PendingCostMoveResume::ReplacementMayCost { .. }
        | PendingCostMoveResume::Foretell { .. }
        | PendingCostMoveResume::UnlessBouncePayment { .. }
        | PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
        // CR 701.9b: a parked random unless-discard holds no pending cast and
        // no mana-ability cursor — the game picks the cards with no player
        // input — so like its counter-addition sibling it affiliates with no
        // payment-continuation root.
        | PendingCostMoveResume::RandomDiscardUnlessPayment(..)
        | PendingCostMoveResume::LoyaltyActivation { .. } => {
            PaymentContinuationState::NotAffiliated
        }
    }
}

fn classify_deferred_life_root(
    state: &GameState,
    deferred: &DeferredLifeCostResume,
) -> PaymentContinuationState {
    match deferred {
        DeferredLifeCostResume::Cast {
            player,
            pending: Some(pending),
            ..
        } => PaymentContinuationState::Affiliated(root_from_pending_cast(pending, *player)),
        DeferredLifeCostResume::Cast { pending: None, .. } => {
            PaymentContinuationState::UnsupportedAffiliated(
                PaymentContinuationUnsupported::MissingPendingCast,
            )
        }
        DeferredLifeCostResume::ManaRoot { player, resume, .. } => match resume.as_ref() {
            ManaAbilityResume::ManaPayment {
                outer_player: Some(outer_player),
                ..
            } if outer_player == player => classify_global_root(state, *player),
            ManaAbilityResume::ManaPayment { .. } => {
                PaymentContinuationState::UnsupportedAffiliated(
                    PaymentContinuationUnsupported::PayerMismatch,
                )
            }
            ManaAbilityResume::ManaSourceSelection {
                player: selection_player,
                ..
            } if selection_player == player => classify_global_root(state, *player),
            ManaAbilityResume::ManaSourceSelection { .. } => {
                PaymentContinuationState::UnsupportedAffiliated(
                    PaymentContinuationUnsupported::PayerMismatch,
                )
            }
            ManaAbilityResume::PhyrexianCastPayment { .. }
            | ManaAbilityResume::FinalizePendingManaPayment { .. } => {
                PaymentContinuationState::UnsupportedAffiliated(
                    PaymentContinuationUnsupported::UnsupportedDeferredManaRoot,
                )
            }
            ManaAbilityResume::Priority
            | ManaAbilityResume::CompanionToHand { .. }
            | ManaAbilityResume::EndContinuousEffect { .. }
            | ManaAbilityResume::TurnFaceUp { .. }
            | ManaAbilityResume::UnlessPayment { .. }
            | ManaAbilityResume::EffectPayCost { .. } => PaymentContinuationState::NotAffiliated,
        },
        DeferredLifeCostResume::PayAmount { .. } => PaymentContinuationState::NotAffiliated,
    }
}

fn root_from_global(
    state: &GameState,
    payer: PlayerId,
) -> Result<PaymentContinuationRoot, PaymentContinuationUnsupported> {
    state
        .pending_cast
        .as_deref()
        .map(|pending| root_from_pending_cast(pending, payer))
        .ok_or(PaymentContinuationUnsupported::MissingPendingCast)
}

fn root_from_pending_cast(pending: &PendingCast, payer: PlayerId) -> PaymentContinuationRoot {
    match pending.activation_ability_index {
        Some(ability_index) => PaymentContinuationRoot::Activation {
            source_id: pending.object_id,
            ability_index,
            payer,
        },
        None => PaymentContinuationRoot::Spell {
            object_id: pending.object_id,
            card_id: pending.card_id,
            payer,
        },
    }
}

fn root_from_pending_mana_ability(
    state: &GameState,
    pending: &PendingManaAbility,
) -> Result<Option<PaymentContinuationRoot>, PaymentContinuationUnsupported> {
    let mut root = None;
    record_root_from_resume(state, &pending.resume, &mut root)?;
    if let Some(resume) = pending.cost_move_resume.as_ref() {
        record_root_from_resume(state, resume, &mut root)?;
    }
    Ok(root)
}

fn root_from_pending_mana_and_cursor(
    state: &GameState,
    pending: &PendingManaAbility,
    cursor: &ManaAbilityCostCursor,
) -> Result<Option<PaymentContinuationRoot>, PaymentContinuationUnsupported> {
    let mut root = root_from_pending_mana_ability(state, pending)?;
    record_root_from_cursor(state, cursor, &mut root)?;
    Ok(root)
}

fn record_root_from_cursor(
    state: &GameState,
    cursor: &ManaAbilityCostCursor,
    root: &mut Option<PaymentContinuationRoot>,
) -> Result<(), PaymentContinuationUnsupported> {
    let Some(parent) = cursor.parent.as_deref() else {
        return Ok(());
    };
    let parent_root = root_from_pending_mana_ability(state, &parent.pending)?;
    merge_root(root, parent_root)?;
    record_root_from_cursor(state, &parent.cursor, root)
}

fn record_root_from_resume(
    state: &GameState,
    resume: &ManaAbilityResume,
    root: &mut Option<PaymentContinuationRoot>,
) -> Result<(), PaymentContinuationUnsupported> {
    let next = match resume {
        ManaAbilityResume::ManaPayment {
            outer_player: Some(payer),
            ..
        } => Some(root_from_global(state, *payer)?),
        ManaAbilityResume::ManaPayment {
            outer_player: None, ..
        } => return Err(PaymentContinuationUnsupported::MissingOuterPayer),
        ManaAbilityResume::ManaSourceSelection { player, .. } => {
            Some(root_from_global(state, *player)?)
        }
        ManaAbilityResume::PhyrexianCastPayment { caster, .. } => {
            Some(root_from_global(state, *caster)?)
        }
        ManaAbilityResume::FinalizePendingManaPayment { player } => {
            Some(root_from_global(state, *player)?)
        }
        // Special actions and effect payments are not a CAST's payment root:
        // they carry their own typed continuation and never resume into a
        // pending cast (CR 116.1 — a special action does not use the stack).
        ManaAbilityResume::Priority
        | ManaAbilityResume::CompanionToHand { .. }
        | ManaAbilityResume::EndContinuousEffect { .. }
        | ManaAbilityResume::TurnFaceUp { .. }
        | ManaAbilityResume::UnlessPayment { .. }
        | ManaAbilityResume::EffectPayCost { .. } => None,
    };
    merge_root(root, next)
}

fn merge_root(
    existing: &mut Option<PaymentContinuationRoot>,
    next: Option<PaymentContinuationRoot>,
) -> Result<(), PaymentContinuationUnsupported> {
    let Some(next) = next else {
        return Ok(());
    };
    if let Some(existing) = existing {
        if *existing != next {
            return Err(PaymentContinuationUnsupported::RootMismatch);
        }
    } else {
        *existing = Some(next);
    }
    Ok(())
}

fn root_present(state: &GameState, root: &PaymentContinuationRoot) -> bool {
    state
        .pending_cast
        .as_deref()
        .is_some_and(|pending| pending_matches_root(pending, root))
        || state
            .waiting_for
            .pending_cast_ref()
            .is_some_and(|pending| pending_matches_root(pending, root))
        || waiting_for_contains_root(&state.waiting_for, root)
        || pending_cost_move_contains_root(state.pending_cost_move_resume.as_ref(), root)
        || deferred_life_contains_root(state.pending_deferred_life_cost_resume.as_ref(), root)
}

fn waiting_for_contains_root(waiting_for: &WaitingFor, root: &PaymentContinuationRoot) -> bool {
    match waiting_for {
        WaitingFor::PayCost {
            resume: CostResume::ManaAbility { mana_ability },
            ..
        }
        | WaitingFor::PayManaAbilityMana {
            pending_mana_ability: mana_ability,
            ..
        }
        | WaitingFor::PayAmountChoice {
            pending_mana_ability: Some(mana_ability),
            ..
        } => pending_mana_contains_root(mana_ability, root),
        WaitingFor::ChooseManaColor {
            context: ManaChoiceContext::ManaAbility(mana_ability),
            ..
        } => pending_mana_contains_root(mana_ability, root),
        WaitingFor::CollectEvidenceChoice { resume, .. } => match resume.as_ref() {
            CollectEvidenceResume::Casting { pending_cast, .. } => {
                pending_matches_root(pending_cast, root)
            }
            CollectEvidenceResume::ManaAbility {
                pending_mana_ability,
            } => pending_mana_contains_root(pending_mana_ability, root),
            CollectEvidenceResume::Effect { .. } => false,
        },
        _ => false,
    }
}

fn pending_cost_move_contains_root(
    resume: Option<&PendingCostMoveResume>,
    root: &PaymentContinuationRoot,
) -> bool {
    match resume {
        Some(PendingCostMoveResume::Cast {
            pending: Some(pending),
            ..
        }) => pending_matches_root(pending, root),
        Some(PendingCostMoveResume::SacrificeForCost { pending, .. }) => {
            pending_matches_root(pending, root)
        }
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) => {
            pending_mana_contains_root(pending, root) || cursor_contains_root(cursor, root)
        }
        Some(PendingCostMoveResume::ActivationMillPayment { pending, .. }) => {
            pending_matches_root(pending, root)
        }
        Some(PendingCostMoveResume::CollectEvidencePayment { resume, .. }) => match resume.as_ref()
        {
            CollectEvidenceResume::Casting { pending_cast, .. } => {
                pending_matches_root(pending_cast, root)
            }
            CollectEvidenceResume::ManaAbility {
                pending_mana_ability,
            } => pending_mana_contains_root(pending_mana_ability, root),
            CollectEvidenceResume::Effect { .. } => false,
        },
        Some(PendingCostMoveResume::Cast { pending: None, .. })
        | Some(PendingCostMoveResume::WardSacrificePayment { .. })
        | Some(PendingCostMoveResume::ReplacementMayCost { .. })
        | Some(PendingCostMoveResume::Foretell { .. })
        | Some(PendingCostMoveResume::DelveManaPayment { .. })
        | Some(PendingCostMoveResume::UnlessBouncePayment { .. })
        | Some(PendingCostMoveResume::CounterAdditionUnlessPayment { .. })
        // CR 701.9b: holds no pending cast, so it can contain no root.
        | Some(PendingCostMoveResume::RandomDiscardUnlessPayment(..))
        | Some(PendingCostMoveResume::LoyaltyActivation { .. })
        | None => false,
    }
}

fn deferred_life_contains_root(
    deferred: Option<&DeferredLifeCostResume>,
    root: &PaymentContinuationRoot,
) -> bool {
    matches!(
        deferred,
        Some(DeferredLifeCostResume::Cast {
            pending: Some(pending),
            ..
        }) if pending_matches_root(pending, root)
    )
}

fn pending_mana_contains_root(
    pending: &PendingManaAbility,
    root: &PaymentContinuationRoot,
) -> bool {
    mana_resume_matches_root(&pending.resume, root)
        || pending
            .cost_move_resume
            .as_ref()
            .is_some_and(|resume| mana_resume_matches_root(resume, root))
}

fn cursor_contains_root(cursor: &ManaAbilityCostCursor, root: &PaymentContinuationRoot) -> bool {
    cursor.parent.as_deref().is_some_and(|parent| {
        pending_mana_contains_root(&parent.pending, root)
            || cursor_contains_root(&parent.cursor, root)
    })
}

fn mana_resume_matches_root(resume: &ManaAbilityResume, root: &PaymentContinuationRoot) -> bool {
    match (resume, root) {
        (
            ManaAbilityResume::ManaPayment {
                outer_player: Some(player),
                ..
            },
            PaymentContinuationRoot::Spell { payer, .. }
            | PaymentContinuationRoot::Activation { payer, .. },
        ) => player == payer,
        (
            ManaAbilityResume::PhyrexianCastPayment { caster, .. },
            PaymentContinuationRoot::Spell { payer, .. }
            | PaymentContinuationRoot::Activation { payer, .. },
        ) => caster == payer,
        (
            ManaAbilityResume::FinalizePendingManaPayment { player },
            PaymentContinuationRoot::Spell { payer, .. }
            | PaymentContinuationRoot::Activation { payer, .. },
        ) => player == payer,
        _ => false,
    }
}

fn pending_matches_root(pending: &PendingCast, root: &PaymentContinuationRoot) -> bool {
    match root {
        PaymentContinuationRoot::Spell {
            object_id, card_id, ..
        } => {
            pending.activation_ability_index.is_none()
                && pending.object_id == *object_id
                && pending.card_id == *card_id
        }
        PaymentContinuationRoot::Activation {
            source_id,
            ability_index,
            ..
        } => {
            pending.object_id == *source_id
                && pending.activation_ability_index == Some(*ability_index)
        }
    }
}

impl PaymentContinuationRoot {
    fn object_id(&self) -> ObjectId {
        match self {
            PaymentContinuationRoot::Spell { object_id, .. } => *object_id,
            PaymentContinuationRoot::Activation { source_id, .. } => *source_id,
        }
    }
}

// This counter deliberately records one witness invocation's reducer work;
// callers may invoke the oracle once per raw action, so it is not a global
// candidate-list cap. Kept test-only so production hot paths stay allocation-
// and synchronization-free.
#[cfg(test)]
static LAST_WITNESS_REDUCER_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
fn record_witness_attempts(attempts: usize) {
    LAST_WITNESS_REDUCER_ATTEMPTS.store(attempts, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(test))]
fn record_witness_attempts(_: usize) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_mana_payment_without_a_live_root_fails_closed() {
        let mut state = GameState::new_two_player(1);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        assert_eq!(
            classify_payment_continuation(&state),
            PaymentContinuationState::UnsupportedAffiliated(
                PaymentContinuationUnsupported::MissingPendingCast
            )
        );
    }

    #[test]
    fn witness_attempts_remain_bounded_per_proposed_action() {
        let mut state = GameState::new_two_player(1);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        assert!(witness_payment_continuation(&state, &GameAction::CancelCast).is_none());
        assert!(
            LAST_WITNESS_REDUCER_ATTEMPTS.load(std::sync::atomic::Ordering::Relaxed)
                <= PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS
        );
    }
}
