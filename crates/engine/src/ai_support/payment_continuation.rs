//! Reducer-backed safety witness for AI payment continuations.
//!
//! The engine is the sole authority for mana spending restrictions, payment
//! ordering, and the nested mana-ability state machine. This module therefore
//! never estimates capacity or infers payable colors: it accepts an AI edge
//! only after bounded reducer simulation reaches the matching root's real stack
//! finalization.

use std::collections::{BTreeSet, VecDeque};

use crate::ai_support::candidate_actions;
use crate::game::engine::{apply_as_current_for_simulation, apply_interaction_for_simulation};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CollectEvidenceResume, CostResume, DeferredLifeCostResume, GameState, ManaAbilityCostCursor,
    ManaAbilityResume, ManaChoiceContext, PendingCast, PendingCostMoveCompletion,
    PendingCostMoveResume, PendingManaAbility, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Maximum non-cancellation roots the bounded batch can safely enumerate.
pub const PAYMENT_CONTINUATION_MAX_ROOTS: usize = 64;
/// Maximum reducer applications made while witnessing one payment decision.
///
/// Root actions and their continuation search share this bound, so inspecting
/// many engine-issued options cannot multiply the work of the payment oracle.
/// Each decision uses a smaller root-proportional slice, with this value as its
/// ceiling; a full 64-root wave still leaves one continuation application per
/// root after its first reducer application.
pub const PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS: usize = PAYMENT_CONTINUATION_MAX_ROOTS * 4;
const PAYMENT_CONTINUATION_MIN_REDUCER_ATTEMPTS: usize = 16;

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
    pub step: PaymentContinuationStep,
}

/// What the accepted edge's own reducer step did to the payment root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentContinuationStep {
    /// CR 601.2h–i / CR 602.2b: this step paid the total cost and finalized
    /// the announced root.
    Finalizes,
    /// The same root is still being paid after this step; a later action
    /// finalizes it.
    Continues,
}

/// The completedness of one decision-wide payment proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentContinuationBatchStatus {
    NotAffiliated,
    UnsupportedAffiliated(PaymentContinuationUnsupported),
    /// Every supplied root was considered within the shared bound. An empty
    /// certificate vector is the only proof that no supplied payment finishes.
    Complete,
    /// The search stopped before it could make a no-payment claim. Consumers
    /// must not use partial certificates as evidence that payment is available.
    Indeterminate(PaymentContinuationIndeterminate),
}

/// Why a batch payment witness could not prove a complete answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentContinuationIndeterminate {
    OverRootCapacity,
    AttemptBudgetExhausted,
    MissingFinalizationBaseline,
    PartialDirectPaymentSearch,
    /// An offer search stopped at its first completion; the other roots were
    /// not judged.
    StoppedAtFirstCompletion,
    /// An offer search met a branch it may not certify or expand: one that
    /// left the supported payment carriers, lost the root without finalizing
    /// it, or wasn't information-safe.
    UnexploredBranch,
}

/// Index-aligned result for exactly the input action slice.
#[derive(Debug, Clone)]
pub struct PaymentContinuationBatch {
    pub status: PaymentContinuationBatchStatus,
    pub successors: Vec<Option<AcceptedPaymentSuccessor>>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PaymentContinuationWitnessCounters {
    pub root_applies: usize,
    pub continuation_applies: usize,
    pub total_attempts: usize,
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
        | WaitingFor::ManaAbilityManaPayment {
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
    let batch = witness_payment_continuations(state, std::slice::from_ref(action));
    batch.successors.into_iter().next().flatten()
}

/// Witness all raw actions for one exact payment decision with a shared bounded
/// reducer search. The output has one entry for every input position; callers
/// must retain that position rather than re-associating equivalent actions.
pub fn witness_payment_continuations(
    state: &GameState,
    actions: &[GameAction],
) -> PaymentContinuationBatch {
    witness_payment_continuations_inner(state, actions, SearchPolicy::EveryRoot, None).0
}

#[cfg(feature = "test-support")]
pub fn witness_payment_continuations_with_counters(
    state: &GameState,
    actions: &[GameAction],
    counters: &mut PaymentContinuationWitnessCounters,
) -> PaymentContinuationBatch {
    witness_payment_continuations_inner(state, actions, SearchPolicy::EveryRoot, Some(counters)).0
}

/// What one payment search is asked to establish, and what it may spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchPolicy {
    /// An AI payment decision: a certificate or a refusal for every root,
    /// within a slice proportional to the roots inspected and capped by
    /// [`PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS`].
    EveryRoot,
    /// An offer: is there any completion? Exactly `attempts` reducer
    /// applications; stops at the first finalization and expands children
    /// depth-first. A branch that leaves the supported carriers, or isn't
    /// information-safe relative to `origin`, is never certified and makes a
    /// no-completion claim impossible.
    AnyRoot { attempts: usize, origin: RngMark },
}

/// The game generator's position when an offer witness starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RngMark {
    word_pos: u128,
    outcome_draws: u64,
}

impl RngMark {
    fn of(state: &GameState) -> Self {
        Self {
            word_pos: state.rng.get_word_pos(),
            outcome_draws: state.rng.outcome_draws(),
        }
    }
}

/// CR 601.2h + CR 400.2: an offer certifies only what the caster could know.
/// A step is information-safe when it neither draws from the game's random
/// generator (random costs, shuffles, coin flips, die rolls) nor moves or
/// shows a card from a hidden zone.
fn step_is_information_safe(origin: RngMark, state: &GameState, events: &[GameEvent]) -> bool {
    RngMark::of(state) == origin
        && !events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    from: Some(Zone::Library),
                    ..
                } | GameEvent::CardDrawn { .. }
                    | GameEvent::CardsDrawn { .. }
                    | GameEvent::Milled { .. }
                    | GameEvent::CardsRevealed { .. }
                    | GameEvent::HiddenSearchViewed { .. }
                    | GameEvent::Clash { .. }
                    | GameEvent::DieRolled { .. }
                    | GameEvent::CoinFlipped { .. }
                    | GameEvent::PlanarDieRolled { .. }
            )
        })
}

/// Ceiling on reducer applications spent certifying one targeted cast offer.
/// The announcement, the target steps and the payment search share it.
pub const TARGETED_CAST_OFFER_MAX_REDUCER_ATTEMPTS: usize = 64;

/// What [`witness_targeted_cast_offer`] established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetedCastVerdict {
    /// A reducer path from the announcement finalized the spell (CR 601.2i).
    Completed,
    /// No completion among the engine-issued candidates within the bound, and
    /// every branch stayed on a supported, information-safe carrier. Not a
    /// proof that none exists: some candidate domains are truncated.
    NotFound,
    /// No claim: the bound ran out, or a branch reached a carrier this witness
    /// doesn't drive or a step that isn't information-safe.
    Indeterminate,
}

/// A verdict and the reducer applications it cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetedCastWitness {
    pub verdict: TargetedCastVerdict,
    pub reducer_attempts: usize,
}

/// CR 601.2a-i: certify that announcing the targeted Auto cast `action` can be
/// carried to completion, by driving the real reducer on scratch copies of
/// `state`: announce it as `actor` for `semantic_owner` (the decision slot the
/// offer names), choose targets with the engine's own target candidates, then
/// search the payment step with the bounded payment-continuation witness. Only
/// a real finalization of the same announced spell is `Completed`.
pub fn witness_targeted_cast_offer(
    state: &GameState,
    actor: PlayerId,
    semantic_owner: PlayerId,
    action: &GameAction,
    max_attempts: usize,
) -> TargetedCastWitness {
    let mut attempts = 0;
    let verdict = targeted_cast_verdict(
        state,
        actor,
        semantic_owner,
        action,
        max_attempts,
        &mut attempts,
    );
    TargetedCastWitness {
        verdict,
        reducer_attempts: attempts,
    }
}

fn targeted_cast_verdict(
    state: &GameState,
    actor: PlayerId,
    semantic_owner: PlayerId,
    action: &GameAction,
    max_attempts: usize,
    attempts: &mut usize,
) -> TargetedCastVerdict {
    let GameAction::CastSpell { object_id, .. } = action else {
        return TargetedCastVerdict::Indeterminate;
    };
    if max_attempts == 0 {
        return TargetedCastVerdict::Indeterminate;
    }
    let origin = RngMark::of(state);
    *attempts += 1;
    let mut current = state.clone();
    let Ok(announced) =
        apply_interaction_for_simulation(&mut current, actor, semantic_owner, action.clone())
    else {
        return TargetedCastVerdict::Indeterminate;
    };
    let WaitingFor::TargetSelection { pending_cast, .. } = &current.waiting_for else {
        return TargetedCastVerdict::Indeterminate;
    };
    if pending_cast.activation_ability_index.is_some() || pending_cast.object_id != *object_id {
        return TargetedCastVerdict::Indeterminate;
    }
    let root = root_from_pending_cast(pending_cast, pending_cast.ability.controller);
    if !step_is_information_safe(origin, &current, &announced.events) {
        return TargetedCastVerdict::Indeterminate;
    }
    let Some(baseline) = WitnessBaseline::capture(&current, &root) else {
        return TargetedCastVerdict::Indeterminate;
    };

    // CR 601.2c: targets are announced before the total cost is paid. The
    // offer consults this witness only when the mana obligation doesn't depend
    // on them (CR 601.2f), so the first reducer-accepted target per slot is
    // taken; a payment that depends on the chosen target may go unwitnessed,
    // never wrongly witnessed.
    let mut events = Vec::new();
    while matches!(
        &current.waiting_for,
        WaitingFor::TargetSelection { pending_cast, .. } if pending_matches_root(pending_cast, &root)
    ) {
        let mut advanced = false;
        for action in candidate_actions(&current)
            .into_iter()
            .map(|candidate| candidate.action)
            .filter(|action| !matches!(action, GameAction::CancelCast))
        {
            if *attempts == max_attempts {
                return TargetedCastVerdict::Indeterminate;
            }
            *attempts += 1;
            let mut next = current.clone();
            if let Ok(result) = apply_as_current_for_simulation(&mut next, action) {
                if !step_is_information_safe(origin, &next, &result.events) {
                    return TargetedCastVerdict::Indeterminate;
                }
                events.extend(result.events);
                current = next;
                advanced = true;
                break;
            }
        }
        if !advanced {
            return TargetedCastVerdict::NotFound;
        }
    }

    if !root_present(&current, &root) {
        return if finalized_root_matches(&current, &root, &baseline, &events) {
            TargetedCastVerdict::Completed
        } else {
            TargetedCastVerdict::Indeterminate
        };
    }
    if !matches!(
        classify_payment_continuation(&current),
        PaymentContinuationState::Affiliated(ref current_root) if *current_root == root
    ) {
        return TargetedCastVerdict::Indeterminate;
    }
    let actions: Vec<_> = candidate_actions(&current)
        .into_iter()
        .map(|candidate| candidate.action)
        .collect();
    let (batch, used) = witness_payment_continuations_inner(
        &current,
        &actions,
        SearchPolicy::AnyRoot {
            attempts: max_attempts - *attempts,
            origin,
        },
        None,
    );
    *attempts += used;
    if batch.successors.iter().any(Option::is_some) {
        return TargetedCastVerdict::Completed;
    }
    match batch.status {
        PaymentContinuationBatchStatus::Complete => TargetedCastVerdict::NotFound,
        PaymentContinuationBatchStatus::Indeterminate(_)
        | PaymentContinuationBatchStatus::NotAffiliated
        | PaymentContinuationBatchStatus::UnsupportedAffiliated(_) => {
            TargetedCastVerdict::Indeterminate
        }
    }
}

/// Returns the batch and the reducer applications it spent.
fn witness_payment_continuations_inner(
    state: &GameState,
    actions: &[GameAction],
    policy: SearchPolicy,
    #[allow(unused_mut, unused_variables)] mut counters: Option<
        &mut PaymentContinuationWitnessCounters,
    >,
) -> (PaymentContinuationBatch, usize) {
    let empty = || vec![None; actions.len()];
    let root = match classify_payment_continuation(state) {
        PaymentContinuationState::NotAffiliated => {
            return (
                PaymentContinuationBatch {
                    status: PaymentContinuationBatchStatus::NotAffiliated,
                    successors: empty(),
                },
                0,
            );
        }
        PaymentContinuationState::UnsupportedAffiliated(reason) => {
            return (
                PaymentContinuationBatch {
                    status: PaymentContinuationBatchStatus::UnsupportedAffiliated(reason),
                    successors: empty(),
                },
                0,
            );
        }
        PaymentContinuationState::Affiliated(root) => root,
    };
    let noncancel_roots = actions
        .iter()
        .filter(|action| !matches!(action, GameAction::CancelCast))
        .count();
    if noncancel_roots > PAYMENT_CONTINUATION_MAX_ROOTS {
        return (
            PaymentContinuationBatch {
                status: PaymentContinuationBatchStatus::Indeterminate(
                    PaymentContinuationIndeterminate::OverRootCapacity,
                ),
                successors: empty(),
            },
            0,
        );
    }
    let Some(baseline) = WitnessBaseline::capture(state, &root) else {
        return (
            PaymentContinuationBatch {
                status: PaymentContinuationBatchStatus::Indeterminate(
                    PaymentContinuationIndeterminate::MissingFinalizationBaseline,
                ),
                successors: empty(),
            },
            0,
        );
    };

    let mut order: Vec<_> = actions
        .iter()
        .enumerate()
        .filter(|(_, action)| !matches!(action, GameAction::CancelCast))
        .collect();
    order.sort_by(|(left_index, left), (right_index, right)| {
        payment_action_priority(left)
            .cmp(&payment_action_priority(right))
            .then_with(|| left.cmp_stable(right))
            .then_with(|| left_index.cmp(right_index))
    });
    let direct_root_count = order
        .iter()
        .take_while(|(_, action)| payment_action_priority(action) == 0)
        .count();
    let partial_direct_payment_search = direct_root_count > 0;
    if partial_direct_payment_search {
        order.truncate(direct_root_count);
    }
    let attempt_budget = match policy {
        SearchPolicy::EveryRoot => (if partial_direct_payment_search {
            direct_root_count * 4
        } else {
            noncancel_roots * 4
        })
        .clamp(
            PAYMENT_CONTINUATION_MIN_REDUCER_ATTEMPTS,
            PAYMENT_CONTINUATION_MAX_REDUCER_ATTEMPTS,
        ),
        SearchPolicy::AnyRoot { attempts, .. } => attempts,
    };
    // `AnyRoot` only: a successor this search may not certify or expand.
    let mut unexplored = false;
    let information_unsafe = |successor: &GameState, step_events: &[GameEvent]| match policy {
        SearchPolicy::EveryRoot => false,
        SearchPolicy::AnyRoot { origin, .. } => {
            !step_is_information_safe(origin, successor, step_events)
        }
    };

    let mut attempts = 0;
    let mut successors = empty();
    let mut queue = VecDeque::new();
    for (index, action) in order {
        if attempts == attempt_budget {
            return (
                indeterminate_batch(
                    PaymentContinuationIndeterminate::AttemptBudgetExhausted,
                    empty(),
                    attempts,
                    counters,
                ),
                attempts,
            );
        }
        attempts += 1;
        #[cfg(feature = "test-support")]
        if let Some(counters) = &mut counters {
            counters.root_applies += 1;
        }
        let mut successor = state.clone();
        let Ok(result) = apply_as_current_for_simulation(&mut successor, action.clone()) else {
            continue;
        };
        if information_unsafe(&successor, &result.events) {
            unexplored = true;
            continue;
        }
        if !root_present(&successor, &root) {
            if finalized_root_matches(&successor, &root, &baseline, &result.events) {
                successors[index] = Some(AcceptedPaymentSuccessor {
                    action: action.clone(),
                    state: successor,
                    step: PaymentContinuationStep::Finalizes,
                });
                if let SearchPolicy::AnyRoot { .. } = policy {
                    return (
                        indeterminate_batch(
                            PaymentContinuationIndeterminate::StoppedAtFirstCompletion,
                            successors,
                            attempts,
                            counters,
                        ),
                        attempts,
                    );
                }
            } else {
                unexplored = true;
            }
            continue;
        }
        if matches!(
            classify_payment_continuation(&successor),
            PaymentContinuationState::Affiliated(ref current_root) if current_root == &root
        ) {
            queue.push_back(WitnessNode {
                root_index: index,
                root_action: action.clone(),
                root_successor: successor.clone(),
                state: successor,
                events: result.events,
                remaining_actions: None,
            });
        } else {
            unexplored = true;
        }
    }

    while let Some(mut node) = queue.pop_front() {
        let Some(next_action) = node.next_action() else {
            continue;
        };
        if attempts == attempt_budget {
            return (
                indeterminate_batch(
                    PaymentContinuationIndeterminate::AttemptBudgetExhausted,
                    successors,
                    attempts,
                    counters,
                ),
                attempts,
            );
        }
        attempts += 1;
        #[cfg(feature = "test-support")]
        if let Some(counters) = &mut counters {
            counters.continuation_applies += 1;
        }
        let mut next_state = node.state.clone();
        let result = apply_as_current_for_simulation(&mut next_state, next_action);

        // One continuation action per dequeue gives every root (and every
        // forked descendant) a turn before a high-branching lane can spend a
        // second reducer attempt.
        if node.has_remaining_actions() {
            queue.push_back(node.clone());
        }

        let Ok(result) = result else {
            continue;
        };
        if information_unsafe(&next_state, &result.events) {
            unexplored = true;
            continue;
        }
        let mut events = node.events.clone();
        events.extend(result.events);
        if !root_present(&next_state, &root) {
            if finalized_root_matches(&next_state, &root, &baseline, &events) {
                successors[node.root_index] = Some(AcceptedPaymentSuccessor {
                    action: node.root_action,
                    state: node.root_successor,
                    step: PaymentContinuationStep::Continues,
                });
                if let SearchPolicy::AnyRoot { .. } = policy {
                    return (
                        indeterminate_batch(
                            PaymentContinuationIndeterminate::StoppedAtFirstCompletion,
                            successors,
                            attempts,
                            counters,
                        ),
                        attempts,
                    );
                }
                if partial_direct_payment_search {
                    #[cfg(feature = "test-support")]
                    if let Some(counters) = &mut counters {
                        counters.total_attempts = attempts;
                    }
                    return (
                        PaymentContinuationBatch {
                            status: PaymentContinuationBatchStatus::Indeterminate(
                                PaymentContinuationIndeterminate::PartialDirectPaymentSearch,
                            ),
                            successors,
                        },
                        attempts,
                    );
                }
            } else {
                unexplored = true;
            }
            continue;
        }
        if matches!(
            classify_payment_continuation(&next_state),
            PaymentContinuationState::Affiliated(ref current_root) if current_root == &root
        ) {
            let child = WitnessNode {
                root_index: node.root_index,
                root_action: node.root_action,
                root_successor: node.root_successor,
                state: next_state,
                events,
                remaining_actions: None,
            };
            if partial_direct_payment_search || matches!(policy, SearchPolicy::AnyRoot { .. }) {
                queue.push_front(child);
            } else {
                queue.push_back(child);
            }
        } else {
            unexplored = true;
        }
    }

    #[cfg(feature = "test-support")]
    if let Some(counters) = &mut counters {
        counters.total_attempts = attempts;
    }
    let status = if partial_direct_payment_search {
        PaymentContinuationBatchStatus::Indeterminate(
            PaymentContinuationIndeterminate::PartialDirectPaymentSearch,
        )
    } else if unexplored && matches!(policy, SearchPolicy::AnyRoot { .. }) {
        PaymentContinuationBatchStatus::Indeterminate(
            PaymentContinuationIndeterminate::UnexploredBranch,
        )
    } else {
        PaymentContinuationBatchStatus::Complete
    };
    (PaymentContinuationBatch { status, successors }, attempts)
}

fn indeterminate_batch(
    reason: PaymentContinuationIndeterminate,
    successors: Vec<Option<AcceptedPaymentSuccessor>>,
    _attempts: usize,
    #[allow(unused_mut, unused_variables)] mut counters: Option<
        &mut PaymentContinuationWitnessCounters,
    >,
) -> PaymentContinuationBatch {
    #[cfg(feature = "test-support")]
    if let Some(counters) = &mut counters {
        counters.total_attempts = _attempts;
    }
    PaymentContinuationBatch {
        status: PaymentContinuationBatchStatus::Indeterminate(reason),
        successors,
    }
}

#[derive(Debug, Clone)]
struct WitnessNode {
    root_index: usize,
    root_action: GameAction,
    root_successor: GameState,
    state: GameState,
    events: Vec<GameEvent>,
    remaining_actions: Option<(Vec<GameAction>, usize)>,
}

fn payment_action_priority(action: &GameAction) -> u8 {
    match action {
        // Convoke, Improvise, and Delve all use this engine action. It changes
        // only the announced cost and cannot create a stack object, so explore
        // it before mana-source activations that may open a priority window.
        GameAction::TapForConvoke { .. } => 0,
        _ => 1,
    }
}

impl WitnessNode {
    fn next_action(&mut self) -> Option<GameAction> {
        if self.remaining_actions.is_none() {
            // The reducer below remains the legality authority. Reusing the
            // raw engine candidate domain avoids first simulating every broad
            // priority candidate in `legal_actions`, only to simulate it again
            // for the payment-finalization proof.
            let mut actions: Vec<_> = candidate_actions(&self.state)
                .into_iter()
                .map(|candidate| candidate.action)
                .collect();
            actions.sort_by(|left, right| {
                payment_action_priority(left)
                    .cmp(&payment_action_priority(right))
                    .then_with(|| left.cmp_stable(right))
            });
            self.remaining_actions = Some((actions, 0));
        }
        let (actions, next_index) = self.remaining_actions.as_mut().unwrap();
        while *next_index < actions.len() {
            let action = actions[*next_index].clone();
            *next_index += 1;
            if !matches!(action, GameAction::CancelCast) {
                return Some(action);
            }
        }
        None
    }

    fn has_remaining_actions(&self) -> bool {
        self.remaining_actions
            .as_ref()
            .is_some_and(|(actions, next_index)| {
                actions[*next_index..]
                    .iter()
                    .any(|action| !matches!(action, GameAction::CancelCast))
            })
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
                        kind,
                        ..
                    } if *player_id == *payer
                        && *event_source_id == *source_id
                        // CR 605.3b: a mana activation of the same source is not
                        // the paid activation landing on the stack.
                        && *kind != crate::types::events::ActivatedAbilityKind::Mana
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
        // CR 601.2h: a Delve commit parked mid-exile still owns the cast's payment root.
        PendingCostMoveResume::Cast {
            pending: Some(pending),
            player,
            completion: PendingCostMoveCompletion::FinalizeDelvedCast { .. },
            ..
        } => PaymentContinuationState::Affiliated(root_from_pending_cast(pending, *player)),
        // CR 602.2b: The parked mill leg retains the announced activation's
        // serialized payment root until the replacement choice completes.
        PendingCostMoveResume::ActivationMillPayment { player, pending } => {
            PaymentContinuationState::Affiliated(root_from_pending_cast(pending, *player))
        }
        // CR 602.2b + CR 616.1: a counter-cost removal paused on a replacement
        // choice retains its attached cast or activation's payment root.
        PendingCostMoveResume::CounterCostRemovalPayment {
            player,
            pending: Some(pending),
            ..
        } => PaymentContinuationState::Affiliated(root_from_pending_cast(pending, *player)),
        // These are distinct cost/resolution continuations. They intentionally
        // retain their existing policies rather than being misidentified from a
        // coincidental PendingCast elsewhere in state.
        PendingCostMoveResume::SacrificeForCost { pending: Some(pending), player, .. } => {
            PaymentContinuationState::Affiliated(root_from_pending_cast(pending, *player))
        }
        PendingCostMoveResume::Cast { .. }
        | PendingCostMoveResume::SacrificeForCost { pending: None, .. }
        | PendingCostMoveResume::CounterCostRemovalPayment { pending: None, .. }
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
        | PendingCostMoveResume::LoyaltyActivation { .. }
        | PendingCostMoveResume::TurnFaceUpCostMove { .. } => {
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
            ManaAbilityResume::ManaAbilityManaPayment {
                pending_mana_ability,
            } => classify_pending_mana_ability(state, pending_mana_ability),
            ManaAbilityResume::PhyrexianCastPayment { .. }
            | ManaAbilityResume::FinalizePendingManaPayment { .. } => {
                PaymentContinuationState::UnsupportedAffiliated(
                    PaymentContinuationUnsupported::UnsupportedDeferredManaRoot,
                )
            }
            ManaAbilityResume::Priority
            | ManaAbilityResume::CompanionToHand { .. }
            | ManaAbilityResume::PlanarDieRoll { .. }
            | ManaAbilityResume::EndContinuousEffect { .. }
            | ManaAbilityResume::RoomUnlock { .. }
            | ManaAbilityResume::Foretell { .. }
            | ManaAbilityResume::TurnFaceUp { .. }
            | ManaAbilityResume::UnlessPayment { .. }
            | ManaAbilityResume::CombatTaxPayment { .. }
            | ManaAbilityResume::EffectPayCost { .. } => PaymentContinuationState::NotAffiliated,
        },
        DeferredLifeCostResume::PayAmount { .. } | DeferredLifeCostResume::TurnFaceUp { .. } => {
            PaymentContinuationState::NotAffiliated
        }
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
        // The outer mana ability's own root, if it pays a cast.
        ManaAbilityResume::ManaAbilityManaPayment {
            pending_mana_ability,
        } => root_from_pending_mana_ability(state, pending_mana_ability)?,
        // Special actions and effect payments are not a CAST's payment root:
        // they carry their own typed continuation and never resume into a
        // pending cast (CR 116.1 — a special action does not use the stack).
        ManaAbilityResume::Priority
        | ManaAbilityResume::CompanionToHand { .. }
        | ManaAbilityResume::PlanarDieRoll { .. }
        | ManaAbilityResume::EndContinuousEffect { .. }
        | ManaAbilityResume::RoomUnlock { .. }
        | ManaAbilityResume::Foretell { .. }
        | ManaAbilityResume::TurnFaceUp { .. }
        | ManaAbilityResume::UnlessPayment { .. }
        | ManaAbilityResume::CombatTaxPayment { .. }
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
        | WaitingFor::ManaAbilityManaPayment {
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
        Some(PendingCostMoveResume::SacrificeForCost { pending: Some(pending), .. }) => {
            pending_matches_root(pending, root)
        }
        Some(PendingCostMoveResume::ManaAbilityPayment { pending, cursor }) => {
            pending_mana_contains_root(pending, root) || cursor_contains_root(cursor, root)
        }
        Some(PendingCostMoveResume::ActivationMillPayment { pending, .. }) => {
            pending_matches_root(pending, root)
        }
        Some(PendingCostMoveResume::CounterCostRemovalPayment {
            pending: Some(pending),
            ..
        }) => pending_matches_root(pending, root),
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
        | Some(PendingCostMoveResume::SacrificeForCost { pending: None, .. })
        | Some(PendingCostMoveResume::CounterCostRemovalPayment { pending: None, .. })
        | Some(PendingCostMoveResume::WardSacrificePayment { .. })
        | Some(PendingCostMoveResume::ReplacementMayCost { .. })
        | Some(PendingCostMoveResume::Foretell { .. })
        | Some(PendingCostMoveResume::UnlessBouncePayment { .. })
        | Some(PendingCostMoveResume::CounterAdditionUnlessPayment { .. })
        // CR 701.9b: holds no pending cast, so it can contain no root.
        | Some(PendingCostMoveResume::RandomDiscardUnlessPayment(..))
        | Some(PendingCostMoveResume::LoyaltyActivation { .. })
        | Some(PendingCostMoveResume::TurnFaceUpCostMove { .. })
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
        (
            ManaAbilityResume::ManaAbilityManaPayment {
                pending_mana_ability,
            },
            root,
        ) => pending_mana_contains_root(pending_mana_ability, root),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::game_state::PendingSacrificeCostCompletion;
    use crate::types::resolution::OptionalEffectFrame;

    /// CR 601.2h + CR 400.2: an offer step is information-safe only while it
    /// neither draws from the game's generator nor moves or shows a hidden
    /// card. Production payment paths reach both: Vexing Puzzlebox's mana
    /// ability rolls a d20, and a mana ability may reveal the top card of a
    /// library (CR 701.20b: revealing moves nothing, so CR 605.1a keeps it a
    /// mana ability). This pins the predicate itself; the integration tests
    /// drive both through the offer.
    #[test]
    fn offer_steps_touching_hidden_information_are_unsafe() {
        use crate::types::game_state::RandomDraw;
        let mut state = GameState::new_two_player(1);
        let origin = RngMark::of(&state);
        assert!(step_is_information_safe(origin, &state, &[]));
        assert!(step_is_information_safe(
            origin,
            &state,
            &[GameEvent::DamageCleared {
                object_id: ObjectId(1),
            }]
        ));
        assert!(!step_is_information_safe(
            origin,
            &state,
            &[GameEvent::Milled {
                player_id: PlayerId(0),
                object_id: ObjectId(1),
                to: Zone::Graveyard,
            }]
        ));
        assert!(!step_is_information_safe(
            origin,
            &state,
            &[GameEvent::CardsRevealed {
                player: PlayerId(0),
                card_ids: vec![ObjectId(1)],
                card_names: vec!["Hidden".to_string()],
            }]
        ));
        let _ = rand::RngCore::next_u32(state.rng.draw(RandomDraw::Placement));
        assert!(
            !step_is_information_safe(origin, &state, &[]),
            "a shuffle or random order moved the generator"
        );
    }

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
    fn unsupported_payment_batch_does_not_apply_actions() {
        let mut state = GameState::new_two_player(1);
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };

        let batch = witness_payment_continuations(&state, &[GameAction::CancelCast]);
        assert!(matches!(
            batch.status,
            PaymentContinuationBatchStatus::UnsupportedAffiliated(
                PaymentContinuationUnsupported::MissingPendingCast
            )
        ));
        assert!(batch.successors.iter().all(Option::is_none));
    }

    #[test]
    fn resolution_optional_sacrifice_cursor_is_not_a_cast_root() {
        let mut state = GameState::new_two_player(1);
        state.pending_cost_move_resume = Some(PendingCostMoveResume::SacrificeForCost {
            player: PlayerId(0),
            pending: None,
            chosen: Vec::new(),
            paused_at_index: 0,
            completion: PendingSacrificeCostCompletion::ResolutionOptionalPayment {
                frame: Box::new(OptionalEffectFrame {
                    ability: Box::new(ResolvedAbility::new(
                        Effect::NoOp,
                        vec![],
                        ObjectId(7),
                        PlayerId(0),
                    )),
                    trigger_event: None,
                    trigger_events: Vec::new(),
                    trigger_match_count: None,
                    return_result_occurrence: None,
                }),
                selected: Vec::new(),
            },
            deferred_cost_events: Vec::new(),
            departure_record_indices: Vec::new(),
        });

        assert_eq!(
            classify_payment_continuation(&state),
            PaymentContinuationState::NotAffiliated
        );
    }
}
