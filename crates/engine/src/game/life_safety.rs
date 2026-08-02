//! Clone-local, receipt-bound life-cost preview used by the tactical AI.
//!
//! This is deliberately analysis state rather than game state: it is never
//! serialized, displayed, or used to resolve a real game action.

use crate::ai_support::CandidateAction;
use crate::game::engine;
use crate::game::turn_control::authorized_submitter_for_player;
use crate::types::ability::{
    AbilityCost, AdditionalCost, AdditionalCostOrigin, AdditionalCostRepeatability,
};
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, PendingCast, WaitingFor};
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;

/// The only life-safety information exposed outside the engine crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateLifeSafety {
    Unsafe {
        before: i32,
        after: i32,
        committed: u32,
    },
    NotUnsafe,
}

#[derive(Debug, Clone)]
enum ArmedLifeCostRoot {
    DefilerPayment(Box<DefilerPaymentWitness>),
    OptionalAdditionalCost(Box<OptionalAdditionalCostWitness>),
}

/// The full generated decision and reducer-facing Defiler offer.
///
/// This mirrors the optional-cost witness: accepting the same amount for the
/// same payer is not sufficient provenance for an AI veto.
#[derive(Debug, Clone)]
struct DefilerPaymentWitness {
    player: PlayerId,
    offered_life_cost: u32,
    mana_reduction: ManaCost,
    pending_cast: PendingCast,
    candidate: CandidateAction,
}

/// The selected, direct-life branch of a cast/activation additional-cost prompt.
///
/// This deliberately records the reducer-facing prompt rather than inferring a
/// life payment from a later payer/amount match. CR 601.2b selects an optional
/// additional cost and CR 602.2b applies that casting process to activations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionalAdditionalCostBranch {
    OptionalAccepted,
    ChoicePreferred,
    ChoiceFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OptionalAdditionalCostPath {
    Queue {
        origin: AdditionalCostOrigin,
        origin_ordinal: u32,
    },
    Flow {
        repeatability: AdditionalCostRepeatability,
    },
    /// A card's own once-only Optional/Choice cost reaches the prompt directly
    /// before any queue or flow carrier is installed.
    Direct,
}

#[derive(Debug, Clone)]
struct OptionalAdditionalCostWitness {
    player: PlayerId,
    cost: AdditionalCost,
    times_kicked: u32,
    origin: AdditionalCostOrigin,
    gift_kind: Option<crate::types::keywords::GiftKind>,
    pending_cast: PendingCast,
    candidate: CandidateAction,
    branch: OptionalAdditionalCostBranch,
    materialized_cost: AbilityCost,
    path: OptionalAdditionalCostPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptState {
    Idle,
    Active { token: u64, payer: PlayerId },
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LifeMutationReceipt {
    token: u64,
    payer: PlayerId,
    before: i32,
    after: i32,
    committed: u32,
}

/// Private probe state carried only by the preview clone.
///
/// It is intentionally excluded from serde and semantic game equality. Normal
/// clones preserve it so a reducer can observe its own armed action; viewer
/// projections explicitly reset it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LifeSafetyProbe {
    armed_root: Option<ArmedLifeCostRoot>,
    attempt: Option<AttemptState>,
    receipt: Option<LifeMutationReceipt>,
    /// Any continuation observed around the raw action is deliberately treated
    /// as indeterminate. A receipt already bound to this root still wins.
    carrier_observed: bool,
    next_token: u64,
}

fn is_complete_generated_candidate(state: &GameState, candidate: &CandidateAction) -> bool {
    let (Some(semantic_owner), Some(actor)) =
        (candidate.metadata.semantic_owner, candidate.metadata.actor)
    else {
        return false;
    };
    actor == authorized_submitter_for_player(state, semantic_owner)
        && crate::ai_support::candidate_actions(state)
            .iter()
            .any(|emitted| {
                emitted.action == candidate.action
                    && emitted.metadata.semantic_owner == Some(semantic_owner)
                    && emitted.metadata.actor == Some(actor)
                    && emitted.metadata.tactical_class == candidate.metadata.tactical_class
            })
}

fn selected_optional_life_branch(
    cost: &AdditionalCost,
    pay: bool,
) -> Option<(OptionalAdditionalCostBranch, AbilityCost)> {
    match (cost, pay) {
        (
            AdditionalCost::Optional {
                cost: AbilityCost::PayLife { .. },
                ..
            },
            true,
        ) => Some((
            OptionalAdditionalCostBranch::OptionalAccepted,
            match cost {
                AdditionalCost::Optional { cost, .. } => cost.clone(),
                AdditionalCost::Kicker { .. }
                | AdditionalCost::Choice(_, _)
                | AdditionalCost::Required(_) => unreachable!("matched Optional above"),
            },
        )),
        (AdditionalCost::Choice(preferred @ AbilityCost::PayLife { .. }, _), true) => Some((
            OptionalAdditionalCostBranch::ChoicePreferred,
            preferred.clone(),
        )),
        (AdditionalCost::Choice(_, fallback @ AbilityCost::PayLife { .. }), false) => Some((
            OptionalAdditionalCostBranch::ChoiceFallback,
            fallback.clone(),
        )),
        (AdditionalCost::Optional { .. }, false)
        | (AdditionalCost::Optional { .. }, true)
        | (AdditionalCost::Choice(_, _), true)
        | (AdditionalCost::Choice(_, _), false)
        | (AdditionalCost::Kicker { .. }, true | false)
        | (AdditionalCost::Required(_), true | false) => None,
    }
}

fn optional_additional_cost_path(pending: &PendingCast) -> OptionalAdditionalCostPath {
    if let Some(instance) = pending.additional_cost_queue.first() {
        return OptionalAdditionalCostPath::Queue {
            origin: instance.origin,
            origin_ordinal: instance.origin_ordinal,
        };
    }
    match pending.additional_cost_flow.as_ref() {
        Some(AdditionalCost::Optional { repeatability, .. }) => OptionalAdditionalCostPath::Flow {
            repeatability: *repeatability,
        },
        Some(AdditionalCost::Kicker { .. })
        | Some(AdditionalCost::Choice(_, _))
        | Some(AdditionalCost::Required(_))
        | None => OptionalAdditionalCostPath::Direct,
    }
}

fn extract_life_cost_root(
    state: &GameState,
    candidate: &CandidateAction,
) -> Option<ArmedLifeCostRoot> {
    let semantic_owner = candidate.metadata.semantic_owner?;
    if !is_complete_generated_candidate(state, candidate) {
        return None;
    }

    match (&state.waiting_for, &candidate.action) {
        (
            WaitingFor::DefilerPayment {
                player,
                life_cost,
                mana_reduction,
                pending_cast,
            },
            GameAction::DecideOptionalCost { pay: true },
        ) if *player == semantic_owner => Some(ArmedLifeCostRoot::DefilerPayment(Box::new(
            DefilerPaymentWitness {
                player: *player,
                offered_life_cost: *life_cost,
                mana_reduction: mana_reduction.clone(),
                pending_cast: *pending_cast.clone(),
                candidate: candidate.clone(),
            },
        ))),
        (
            WaitingFor::OptionalCostChoice {
                player,
                cost,
                times_kicked,
                origin,
                gift_kind,
                pending_cast,
            },
            GameAction::DecideOptionalCost { pay },
        ) if *player == semantic_owner => {
            let (branch, materialized_cost) = selected_optional_life_branch(cost, *pay)?;
            let path = optional_additional_cost_path(pending_cast);
            Some(ArmedLifeCostRoot::OptionalAdditionalCost(Box::new(
                OptionalAdditionalCostWitness {
                    player: *player,
                    cost: cost.clone(),
                    times_kicked: *times_kicked,
                    origin: *origin,
                    gift_kind: gift_kind.clone(),
                    pending_cast: *pending_cast.clone(),
                    candidate: candidate.clone(),
                    branch,
                    materialized_cost,
                    path,
                },
            )))
        }
        // CombatTaxPayment / PayCombatTax, raw mana payment, and all other
        // mechanical choices are deliberately neutral. CR 508.1d / 509.1c
        // combat costs are declaration decisions, not cast/activation costs.
        _ => None,
    }
}

/// Preview a generated candidate through the raw reducer boundary.
///
/// CR 118.3b + CR 119.4: a life payment is decided by the actual team-life
/// mutation, not by the requested cost or a later event summary. Any route
/// without one exact, positive post-replacement receipt remains neutral.
pub fn preview_candidate_life_safety(
    state: &GameState,
    candidate: &CandidateAction,
) -> CandidateLifeSafety {
    let Some(root) = extract_life_cost_root(state, candidate) else {
        return CandidateLifeSafety::NotUnsafe;
    };

    let mut preview = state.clone();
    preview.life_safety_probe.arm(root);
    observe_boundary_carrier(&mut preview);
    let applied = engine::apply_interaction_pre_reconciliation_for_life_safety(
        &mut preview,
        candidate.metadata.actor.expect("validated above"),
        candidate.metadata.semantic_owner.expect("validated above"),
        candidate.action.clone(),
    );
    if applied.is_err() {
        return CandidateLifeSafety::NotUnsafe;
    }

    observe_boundary_carrier(&mut preview);
    preview.life_safety_probe.take_lethal_receipt()
}

/// Marks work that can carry a raw reducer action beyond its direct payment
/// seam. The preview never converts such a route into a positive result: only
/// an already-bound post-replacement receipt can produce `Unsafe`.
pub(crate) fn observe_boundary_carrier(state: &mut GameState) {
    let allowed_root_waiting = matches!(
        state.waiting_for,
        WaitingFor::OptionalCostChoice { .. } | WaitingFor::DefilerPayment { .. }
    );
    let has_carrier = (!allowed_root_waiting
        && !matches!(state.waiting_for, WaitingFor::Priority { .. }))
        || !state.resolution_stack.is_empty()
        || state.pending_resolution_completion.is_some()
        || state.pending_deferred_life_cost_resume.is_some()
        || state.pending_cost_move_resume.is_some()
        || state.pending_replacement.is_some()
        || state.pending_trigger.is_some()
        || !state.pending_trigger_event_batch.is_empty()
        || state.pending_trigger_entry.is_some()
        || state.pending_trigger_order.is_some()
        || state.pending_scoped_library_search.is_some()
        || state.pending_library_search_delivery.is_some();
    if has_carrier && state.life_safety_probe.armed_root.is_some() {
        state.life_safety_probe.carrier_observed = true;
    }
}

impl LifeSafetyProbe {
    fn arm(&mut self, root: ArmedLifeCostRoot) {
        self.armed_root = Some(root);
        self.attempt = Some(AttemptState::Idle);
        self.receipt = None;
        self.carrier_observed = false;
    }

    fn take_lethal_receipt(&mut self) -> CandidateLifeSafety {
        let receipt = self.receipt.take();
        let armed_payer = match &self.armed_root {
            Some(ArmedLifeCostRoot::DefilerPayment(witness)) => witness.player,
            Some(ArmedLifeCostRoot::OptionalAdditionalCost(witness)) => witness.player,
            None => return CandidateLifeSafety::NotUnsafe,
        };
        self.armed_root = None;
        self.attempt = None;
        let carrier_observed = self.carrier_observed;
        self.carrier_observed = false;
        if carrier_observed && receipt.is_none() {
            return CandidateLifeSafety::NotUnsafe;
        }
        match receipt {
            Some(receipt)
                if receipt.token != 0 && receipt.payer == armed_payer && receipt.after <= 0 =>
            {
                CandidateLifeSafety::Unsafe {
                    before: receipt.before,
                    after: receipt.after,
                    committed: receipt.committed,
                }
            }
            Some(_) | None => CandidateLifeSafety::NotUnsafe,
        }
    }

    fn record_mutation(&mut self, payer: PlayerId, before: i32, after: i32, loss_amount: u32) {
        let Some(AttemptState::Active {
            token,
            payer: expected_payer,
        }) = self.attempt
        else {
            return;
        };
        let Some(committed) = before
            .checked_sub(after)
            .and_then(|delta| u32::try_from(delta).ok())
        else {
            self.attempt = Some(AttemptState::Invalid);
            return;
        };
        if expected_payer != payer
            || committed == 0
            || committed != loss_amount
            || self.receipt.is_some()
        {
            self.attempt = Some(AttemptState::Invalid);
            return;
        }
        self.receipt = Some(LifeMutationReceipt {
            token,
            payer,
            before,
            after,
            committed,
        });
        self.attempt = Some(AttemptState::Invalid);
    }
}

fn activate_attempt(probe: &mut LifeSafetyProbe, player: PlayerId) {
    if probe.attempt != Some(AttemptState::Idle) {
        probe.attempt = Some(AttemptState::Invalid);
        return;
    }
    probe.next_token = probe.next_token.checked_add(1).unwrap_or(0);
    if probe.next_token == 0 {
        probe.attempt = Some(AttemptState::Invalid);
        return;
    }
    probe.attempt = Some(AttemptState::Active {
        token: probe.next_token,
        payer: player,
    });
}

/// Arms the exact accepted Defiler offer at its cost authority.
///
/// CR 118.11: replacements can modify the action used to pay the cost, so the
/// offered amount is a witness for this attempt, never the receipt amount.
pub(crate) fn begin_defiler_payment_attempt(
    state: &mut GameState,
    player: PlayerId,
    pending_cast: &PendingCast,
    life_cost: u32,
    mana_reduction: &ManaCost,
) {
    let matches_root = matches!(
        (&state.life_safety_probe.armed_root, &state.waiting_for),
        (
            Some(ArmedLifeCostRoot::DefilerPayment(witness)),
            WaitingFor::DefilerPayment {
                player: waiting_player,
                life_cost: waiting_life_cost,
                mana_reduction: waiting_reduction,
                pending_cast: waiting_pending,
            },
        ) if witness.player == player
            && witness.offered_life_cost == life_cost
            && witness.mana_reduction == *mana_reduction
            && witness.pending_cast == *pending_cast
            && *waiting_player == player
            && *waiting_life_cost == life_cost
            && waiting_reduction == mana_reduction
            && waiting_pending.as_ref() == pending_cast
            && witness.candidate.action == GameAction::DecideOptionalCost { pay: true }
            && witness.candidate.metadata.semantic_owner == Some(player)
            && witness.candidate.metadata.actor
                == Some(authorized_submitter_for_player(state, player))
            && is_complete_generated_candidate(state, &witness.candidate)
    );
    let probe = &mut state.life_safety_probe;
    if !matches_root {
        probe.attempt = Some(AttemptState::Invalid);
        return;
    }

    activate_attempt(probe, player);
}

/// Arms a selected direct `PayLife` additional-cost branch at the common
/// additional-cost payment authority. The full prompt and queue/flow carrier
/// are revalidated here because the candidate decision itself is only intent;
/// the reducer's materialized cost is the binding authority.
pub(crate) fn begin_optional_additional_cost_attempt(
    state: &mut GameState,
    player: PlayerId,
    pending_before: &PendingCast,
    additional_cost: &AdditionalCost,
    pay: bool,
    materialized_cost: &AbilityCost,
    pending_after: &PendingCast,
) {
    let matches_root = match (&state.life_safety_probe.armed_root, &state.waiting_for) {
        (
            Some(ArmedLifeCostRoot::OptionalAdditionalCost(witness)),
            WaitingFor::OptionalCostChoice {
                player: waiting_player,
                cost: waiting_cost,
                times_kicked: waiting_times_kicked,
                origin: waiting_origin,
                gift_kind: waiting_gift_kind,
                pending_cast: waiting_pending,
            },
        ) => {
            let selected = selected_optional_life_branch(additional_cost, pay);
            let matches_selected = selected.is_some_and(|(branch, cost)| {
                branch == witness.branch
                    && cost == witness.materialized_cost
                    && &cost == materialized_cost
            });
            let matches_prompt = witness.player == player
                && witness.candidate.action == GameAction::DecideOptionalCost { pay }
                && witness.candidate.metadata.semantic_owner == Some(player)
                && witness.candidate.metadata.actor
                    == Some(authorized_submitter_for_player(state, player))
                && witness.cost == *additional_cost
                && witness.times_kicked == *waiting_times_kicked
                && witness.origin == *waiting_origin
                && witness.gift_kind == *waiting_gift_kind
                && witness.pending_cast == *pending_before
                && *waiting_player == player
                && *waiting_cost == *additional_cost
                && waiting_pending.as_ref() == pending_before
                && is_complete_generated_candidate(state, &witness.candidate);
            let matches_path = match &witness.path {
                OptionalAdditionalCostPath::Queue {
                    origin,
                    origin_ordinal,
                } => {
                    pending_before
                        .additional_cost_queue
                        .first()
                        .is_some_and(|instance| {
                            instance.origin == *origin
                                && instance.origin_ordinal == *origin_ordinal
                                && instance.cost == *additional_cost
                        })
                        && match additional_cost {
                            AdditionalCost::Optional {
                                repeatability: AdditionalCostRepeatability::Repeatable,
                                ..
                            } => {
                                pending_after.additional_cost_queue
                                    == pending_before.additional_cost_queue
                            }
                            AdditionalCost::Optional {
                                repeatability: AdditionalCostRepeatability::Once,
                                ..
                            }
                            | AdditionalCost::Choice(_, _)
                            | AdditionalCost::Kicker { .. }
                            | AdditionalCost::Required(_) => {
                                pending_after.additional_cost_queue
                                    == pending_before.additional_cost_queue[1..]
                            }
                        }
                }
                OptionalAdditionalCostPath::Flow { repeatability } => {
                    let matches_flow = |pending: &PendingCast| {
                        matches!(
                            &pending.additional_cost_flow,
                            Some(AdditionalCost::Optional {
                                cost,
                                repeatability: flow_repeatability,
                            }) if flow_repeatability == repeatability && cost == materialized_cost
                        )
                    };
                    matches_flow(pending_before)
                        && if repeatability.is_once() {
                            pending_after.additional_cost_flow.is_none()
                                && pending_after.additional_cost_decided
                        } else {
                            matches_flow(pending_after)
                        }
                }
                OptionalAdditionalCostPath::Direct => {
                    pending_before.additional_cost_queue.is_empty()
                        && pending_before.additional_cost_flow.is_none()
                        && pending_after.additional_cost_queue.is_empty()
                        && pending_after.additional_cost_flow.is_none()
                }
            };
            matches_selected
                && matches_prompt
                && matches_path
                && pending_after.object_id == pending_before.object_id
                && pending_after.card_id == pending_before.card_id
        }
        (Some(ArmedLifeCostRoot::DefilerPayment(_)), _)
        | (None, _)
        | (Some(ArmedLifeCostRoot::OptionalAdditionalCost(_)), _) => false,
    };
    let probe = &mut state.life_safety_probe;
    if !matches_root {
        probe.attempt = Some(AttemptState::Invalid);
        return;
    }
    activate_attempt(probe, player);
}

/// Records the authoritative, post-replacement player-life edit.
pub(crate) fn record_life_mutation_receipt(
    state: &mut GameState,
    payer: PlayerId,
    before: i32,
    after: i32,
    loss_amount: u32,
) {
    state
        .life_safety_probe
        .record_mutation(payer, before, after, loss_amount);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_support::{ActionMetadata, TacticalClass};
    use crate::game::visibility::filter_state_for_viewer;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AdditionalCostRepeatability, Effect, QuantityExpr, QuantityModification,
        ReplacementDefinition, ResolvedAbility, StaticDefinition, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::CastPaymentMode;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::phase::Phase;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn defiler_root(player: PlayerId) -> ArmedLifeCostRoot {
        let source = ObjectId(17);
        ArmedLifeCostRoot::DefilerPayment(Box::new(DefilerPaymentWitness {
            player,
            offered_life_cost: 2,
            mana_reduction: ManaCost::zero(),
            pending_cast: PendingCast::new(
                source,
                CardId(17),
                ResolvedAbility::new(
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 0 },
                        target: TargetFilter::Controller,
                    },
                    Vec::new(),
                    source,
                    player,
                ),
                ManaCost::zero(),
            ),
            candidate: CandidateAction {
                action: GameAction::DecideOptionalCost { pay: true },
                metadata: ActionMetadata::for_actor(Some(player), TacticalClass::Selection),
            },
        }))
    }

    fn carrier_waiting_for(player: PlayerId) -> WaitingFor {
        WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids: Vec::new(),
            valid_attack_targets: Vec::new(),
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        }
    }

    #[test]
    fn post_replacement_receipt_uses_actual_mutation_not_offered_cost() {
        let mut probe = LifeSafetyProbe {
            attempt: Some(AttemptState::Active {
                token: 1,
                payer: PlayerId(0),
            }),
            ..Default::default()
        };
        probe.record_mutation(PlayerId(0), 3, -1, 4);
        assert_eq!(
            probe.receipt,
            Some(LifeMutationReceipt {
                token: 1,
                payer: PlayerId(0),
                before: 3,
                after: -1,
                committed: 4,
            })
        );
    }

    #[test]
    fn optional_additional_cost_witness_arms_only_selected_direct_life_branches() {
        let life = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        };
        let mana = AbilityCost::Mana {
            cost: ManaCost::zero(),
        };

        assert!(matches!(
            selected_optional_life_branch(
                &AdditionalCost::Optional {
                    cost: life.clone(),
                    repeatability: AdditionalCostRepeatability::Once,
                },
                true,
            ),
            Some((
                OptionalAdditionalCostBranch::OptionalAccepted,
                AbilityCost::PayLife { .. }
            ))
        ));
        assert!(selected_optional_life_branch(
            &AdditionalCost::Optional {
                cost: life.clone(),
                repeatability: AdditionalCostRepeatability::Once,
            },
            false,
        )
        .is_none());
        assert!(matches!(
            selected_optional_life_branch(&AdditionalCost::Choice(life, mana), true),
            Some((
                OptionalAdditionalCostBranch::ChoicePreferred,
                AbilityCost::PayLife { .. }
            ))
        ));
        assert!(selected_optional_life_branch(
            &AdditionalCost::Choice(
                AbilityCost::Mana {
                    cost: ManaCost::zero(),
                },
                AbilityCost::Mana {
                    cost: ManaCost::zero(),
                },
            ),
            false,
        )
        .is_none());
    }

    #[test]
    fn once_only_optional_life_cost_keeps_its_receipt_authority_after_target_deferral() {
        let player = PlayerId(0);
        let life_cost = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 2 },
        };
        let additional_cost = AdditionalCost::Optional {
            cost: life_cost.clone(),
            repeatability: AdditionalCostRepeatability::Once,
        };
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(91),
            player,
        );
        let mut pending_before =
            PendingCast::new(ObjectId(91), CardId(91), ability, ManaCost::zero());
        pending_before.additional_cost_flow = Some(additional_cost.clone());
        pending_before.deferred_target_selection = true;

        let mut state = GameState::new_two_player(42);
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::OptionalCostChoice {
            player,
            cost: additional_cost.clone(),
            times_kicked: 0,
            origin: AdditionalCostOrigin::Other,
            gift_kind: None,
            pending_cast: Box::new(pending_before.clone()),
        };
        let candidate = crate::ai_support::candidate_actions(&state)
            .into_iter()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::DecideOptionalCost { pay: true }
                )
            })
            .expect("the optional-life prompt must emit its accepted decision");
        let root = extract_life_cost_root(&state, &candidate)
            .expect("the generated optional-life decision must have a receipt root");
        state.life_safety_probe.arm(root);

        let mut pending_after = pending_before.clone();
        pending_after.additional_cost_flow = None;
        pending_after.additional_cost_decided = true;
        begin_optional_additional_cost_attempt(
            &mut state,
            player,
            &pending_before,
            &additional_cost,
            true,
            &life_cost,
            &pending_after,
        );

        assert!(matches!(
            state.life_safety_probe.attempt,
            Some(AttemptState::Active {
                payer: PlayerId(0),
                ..
            })
        ));
    }

    #[test]
    fn carrier_boundary_without_a_receipt_is_neutral_but_a_bound_receipt_survives_later_carrier() {
        let player = PlayerId(0);
        let mut no_edit_state = GameState::new_two_player(42);
        no_edit_state.life_safety_probe.arm(defiler_root(player));
        no_edit_state.waiting_for = carrier_waiting_for(player);
        observe_boundary_carrier(&mut no_edit_state);
        assert_eq!(
            no_edit_state.life_safety_probe.take_lethal_receipt(),
            CandidateLifeSafety::NotUnsafe,
            "a carrier alone cannot manufacture a life-cost veto"
        );

        let mut receipt_state = GameState::new_two_player(42);
        receipt_state.life_safety_probe.arm(defiler_root(player));
        activate_attempt(&mut receipt_state.life_safety_probe, player);
        receipt_state
            .life_safety_probe
            .record_mutation(player, 2, 0, 2);
        receipt_state.waiting_for = carrier_waiting_for(player);
        observe_boundary_carrier(&mut receipt_state);
        assert_eq!(
            receipt_state.life_safety_probe.take_lethal_receipt(),
            CandidateLifeSafety::Unsafe {
                before: 2,
                after: 0,
                committed: 2,
            },
            "a later continuation cannot erase the post-replacement receipt"
        );
    }

    #[test]
    fn raw_defiler_payment_keeps_the_reduced_mana_continuation_before_terminal_reconciliation() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };
        state.players[0].life = 2;

        let spell_id = create_object(
            &mut state,
            CardId(90_002),
            player,
            "Defiler-proof green permanent".to_string(),
            Zone::Hand,
        );
        let spell = state
            .objects
            .get_mut(&spell_id)
            .expect("the test spell must exist");
        spell.card_types.core_types.push(CoreType::Creature);
        spell.color = vec![ManaColor::Green];
        spell.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        };

        let defiler_id = create_object(
            &mut state,
            CardId(90_003),
            player,
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&defiler_id)
            .expect("the test Defiler must exist")
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }));

        crate::game::apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_002),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Manual,
            },
        )
        .expect("the real green permanent cast must enter DefilerPayment");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DefilerPayment { .. }
        ));

        let replacement_id = create_object(
            &mut state,
            CardId(90_006),
            player,
            "Life-loss replacement proof".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement_id)
            .expect("the replacement source must exist")
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(QuantityModification::Plus { value: 0 })]
        .into();

        let candidate = crate::ai_support::candidate_actions(&state)
            .into_iter()
            .find(|candidate| {
                matches!(
                    candidate.action,
                    GameAction::DecideOptionalCost { pay: true }
                )
            })
            .expect("the generated candidate set must contain the accepted Defiler decision");
        assert_eq!(
            preview_candidate_life_safety(&state, &candidate),
            CandidateLifeSafety::Unsafe {
                before: 2,
                after: 0,
                committed: 2,
            },
            "the generated Defiler decision must bind the post-replacement receipt"
        );

        let root = extract_life_cost_root(&state, &candidate)
            .expect("the generated Defiler decision must provide a receipt root");
        let mut raw_state = state.clone();
        raw_state.life_safety_probe.arm(root);
        engine::apply_interaction_pre_reconciliation_for_life_safety(
            &mut raw_state,
            candidate
                .metadata
                .actor
                .expect("the generated Defiler candidate must have an actor"),
            candidate
                .metadata
                .semantic_owner
                .expect("the generated Defiler candidate must have a semantic owner"),
            candidate.action,
        )
        .expect("the raw Defiler decision must preserve its payment continuation");

        assert_eq!(raw_state.players[0].life, 0);
        assert!(matches!(
            raw_state.waiting_for,
            WaitingFor::ManaPayment {
                player: PlayerId(0),
                ..
            }
        ));
        assert_eq!(
            raw_state
                .pending_cast
                .as_deref()
                .expect("the mana-payment continuation must retain its pending cast")
                .cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            },
            "Defiler's accepted payment must retain the one-shard-reduced cost"
        );
        observe_boundary_carrier(&mut raw_state);
        assert_eq!(
            raw_state.life_safety_probe.take_lethal_receipt(),
            CandidateLifeSafety::Unsafe {
                before: 2,
                after: 0,
                committed: 2,
            },
            "the raw mana-payment carrier must retain the bound receipt"
        );
    }

    #[test]
    fn probe_is_excluded_from_state_identity_serialization_and_viewer_projections() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        let semantic_baseline = state.clone();
        state.life_safety_probe.arm(defiler_root(player));
        state.life_safety_probe.next_token = 41;

        let ordinary_clone = state.clone();
        assert!(ordinary_clone.life_safety_probe.armed_root.is_some());
        assert_eq!(ordinary_clone.life_safety_probe.next_token, 41);

        assert_eq!(
            state, semantic_baseline,
            "clone-local analysis cannot change semantic game-state equality"
        );
        let serialized = serde_json::to_value(&state).expect("game state serializes");
        assert!(
            serialized.get("life_safety_probe").is_none(),
            "clone-local analysis must not appear in serialized game state"
        );

        let viewer_state = filter_state_for_viewer(&state, player);
        assert!(viewer_state.life_safety_probe.armed_root.is_none());
        assert!(viewer_state.life_safety_probe.attempt.is_none());
        assert!(viewer_state.life_safety_probe.receipt.is_none());
        assert!(!viewer_state.life_safety_probe.carrier_observed);
        assert_eq!(viewer_state.life_safety_probe.next_token, 0);
    }
}
