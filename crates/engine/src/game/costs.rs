//! Ability cost payment authority (L2).
//!
//! This module is the single authority that executes payment of an ability's
//! cost (CLAUDE.md: "Single authority for ability costs"). It owns the only
//! `match` over `AbilityCost` that mutates player/object state to pay a cost,
//! plus the CR 616.1 replacement-pause bookkeeping. Both activation-time
//! (CR 601.2g/h) and resolution-time (CR 118.12) payment flow through it; the
//! caller selects the regime via [`PaymentScope`], which carries the genuine
//! scope differences (CR-confirmed in the unification plan §2): quantity
//! resolution context, mana payment context, and PayLife helper selection
//! (the activation helper additionally applies cast/activation life-payment
//! prohibition statics; plan R4 keeps such forks explicit in the arm).
//!
//! Originally extracted from `casting.rs` as a pure code-motion seam (Phase 1);
//! Phase 2 introduced [`PaymentScope`] and routed the resolution-time
//! `Effect::PayCost` arms (`effects/pay.rs`) through this authority, deleting
//! their duplicate Mana/ManaDynamic/PayLife/PayEnergy/Composite/Discard
//! implementations. Phase 5 added [`can_pay`] — the single affordability
//! authority that composes `AbilityCost::is_payable` (the CR 118.3
//! resource/choice-eligibility gate) with a scope-appropriate check: the
//! relocated A2 clone-and-simulate for activation, the relocated A3 resource
//! match for resolution (`supported_at_resolution` is the shared membership
//! authority for which shapes have a resolution payment arm). The activation
//! flow, the `WaitingFor::PayCost` emission/resume handlers, the cost finder
//! helpers, and the mana planner all remain in `casting.rs`;
//! `casting::can_pay_ability_cost_now` now delegates to [`can_pay`].
//!
//! L1-primitives-only rule (TARGET invariant): code here pays costs through
//! L1 resource primitives (`life_costs`, `effects::counters`, `sacrifice`,
//! `effects::discard`, `zones`, `effects::attach`, and the mana payment path
//! in `casting.rs`) and must never re-implement resource math beyond a direct
//! L1 call. This rule binds the L3 resume handlers too: the
//! `WaitingFor::PayCost` / `WardDiscardChoice` / `WardSacrificeChoice` resume
//! handlers (in `engine.rs`/`engine_payment_choices.rs`) match on
//! `PayCostKind`/`WaitingFor` variants and may call L1 primitives
//! (`sacrifice_permanent`, `discard_as_cost`, …) to execute a player's concrete
//! selection, but they must never match on `AbilityCost` or re-implement the
//! resource math that lives here (risk R8). Known exceptions carried over
//! verbatim, to be collapsed in Phase 5: the `PayEnergy` arm hand-rolls the
//! energy decrement (pending a `players::pay_energy` L1 helper) and the `Tap`
//! arm sets `tapped` directly.

use std::collections::HashSet;

use crate::types::ability::{
    AbilityCost, EffectKind, TargetFilter, TypedFilter, REMOVE_COUNTER_COST_ALL,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CostResume, GameState, ManaAbilityResume, PayCostKind, PendingCostMoveCompletion,
    PendingCostMoveResume, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::casting::{
    ability_mana_payment_excluded_sources, can_pay_effect_mana_cost_after_auto_tap,
    find_eligible_discard_targets, mana_ability_cost_payment_is_paused, pay_ability_mana_cost,
    pay_ability_mana_cost_excluding, pay_effect_mana_cost_with_resume,
};
use super::engine::EngineError;
use super::filter::FilterContext;
use super::life_costs::can_pay_life_cost;
use super::quantity::{resolve_quantity, resolve_quantity_with_targets};
use super::speed::{effective_speed, set_speed};
use super::zone_pipeline::{self, ZoneMoveRequest, ZoneMoveResult};
use crate::types::ability::ResolvedAbility;

/// Helper to find eligible cards for exile cost payment at resolution.
/// Returns cards in the specified zone matching the filter, excluding the source.
fn find_eligible_exile_targets(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    zone: Zone,
    filter: Option<&TargetFilter>,
) -> Vec<ObjectId> {
    let ctx = FilterContext::from_source(state, source_id);
    let player_state = state.players.get(player.0 as usize);

    match zone {
        Zone::Graveyard => {
            // CR 406.6: Check if the filter is controller-scoped. When the filter
            // has controller: None (unrestricted "graveyards"), scan all players'
            // graveyards. When controller: Some(ControllerRef::You) ("your graveyard"),
            // scan only the payer's graveyard.
            let is_unrestricted = filter.is_none_or(|f| {
                matches!(
                    f,
                    TargetFilter::Typed(TypedFilter {
                        controller: None,
                        ..
                    })
                )
            });

            if is_unrestricted {
                // Scan all players' graveyards
                state
                    .players
                    .iter()
                    .flat_map(|p| p.graveyard.iter().copied())
                    .filter(|&id| {
                        id != source_id
                            && filter.is_none_or(|f| {
                                super::filter::matches_target_filter(state, id, f, &ctx)
                            })
                    })
                    .collect()
            } else {
                // Scan only the payer's graveyard (controller-scoped)
                player_state
                    .map(|p| {
                        p.graveyard
                            .iter()
                            .copied()
                            .filter(|&id| {
                                id != source_id
                                    && filter.is_none_or(|f| {
                                        super::filter::matches_target_filter(state, id, f, &ctx)
                                    })
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            }
        }
        Zone::Hand => player_state
            .map(|p| {
                p.hand
                    .iter()
                    .copied()
                    .filter(|&id| {
                        id != source_id
                            && filter.is_none_or(|f| {
                                super::filter::matches_target_filter(state, id, f, &ctx)
                            })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        Zone::Battlefield => state
            .battlefield
            .iter()
            .copied()
            .filter(|&id| {
                state
                    .objects
                    .get(&id)
                    .map(|obj| obj.controller == player)
                    .unwrap_or(false)
                    && id != source_id
                    && filter
                        .is_none_or(|f| super::filter::matches_target_filter(state, id, f, &ctx))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn find_eligible_tap_creatures_targets(
    state: &GameState,
    player: PlayerId,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state.objects.get(id).is_some_and(|obj| {
                obj.controller == player
                    && !obj.tapped
                    && super::filter::matches_target_filter(state, obj.id, filter, &ctx)
            })
        })
        .collect()
}

/// Selects the payment regime for `pay_ability_cost_inner`. The two variants
/// capture the only CR-confirmed differences between activation-time and
/// resolution-time payment (unification plan §2):
///
/// - **Quantity resolution.** Activation resolves dynamic amounts with
///   `resolve_quantity(state, expr, player, source)`; resolution resolves them
///   against the payer-adjusted [`ResolvedAbility`] via
///   `resolve_quantity_with_targets` so event/target refs
///   (`Power { CostPaidObject }`, …) read the right object (CR 608.2k).
/// - **Mana payment context.** Activation uses the CR 601.2g mana-ability
///   window (`pay_ability_mana_cost`); resolution uses the effect-context
///   auto-tap path (`pay_effect_mana_cost`, CR 118.12).
///
/// Failure semantics are also scope-conditioned and handled by the caller:
/// activation maps [`PaymentOutcome::Failed`] to `EngineError::ActionNotAllowed`
/// (CR 601.2h "Unpayable costs can't be paid"); resolution maps it to
/// `cost_payment_failed_flag` (CR 118.12 "if [a player] can't").
pub(crate) enum PaymentScope<'a> {
    Activation {
        excluded_sources: &'a HashSet<ObjectId>,
        /// CR 106.6: Exact activated ability whose mana cost is being paid.
        /// This builds the live activation payment context, including any
        /// source-chosen-color rider and keyword tag.
        ability_index: Option<usize>,
    },
    /// `ability` is normally the PAYER-ADJUSTED `ResolvedAbility` clone
    /// (controller swapped to the resolved payer, per `effects/pay.rs`). All
    /// quantity-resolving arms read it via `resolve_quantity_with_targets`.
    ///
    /// Caveat: the unless-payment adapter (`engine_payment_choices.rs`,
    /// PayLife / PayEnergy arms) intentionally passes the `pending_effect` RAW —
    /// the controller is NOT swapped to the unless-payer — because unless-cost
    /// dynamic quantities can be controller-relative by card text, so a blanket
    /// swap is not obviously correct there. The payer is still threaded
    /// separately (`player`), so the right player's resources are deducted; only
    /// the `QuantityExpr` resolution reads the un-swapped controller. See the
    /// per-arm comments at those call sites.
    Resolution {
        ability: &'a ResolvedAbility,
        cost_move_root: ResolutionCostMoveRoot,
    },
}

/// The owner of a resolution-time non-self cost move. Only an accepted
/// replacement MayCost has an outer replacement to re-enter after an inner
/// `Moved` replacement choice; ordinary `Effect::PayCost` remains on its
/// established choice-driven path.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionCostMoveRoot {
    EffectPayCost,
    ReplacementMayCost,
}

/// A cost payment could not be completed. The reason string is the human-
/// readable failure carried over from the original `EngineError` messages;
/// the activation adapter re-wraps it as `EngineError::ActionNotAllowed`, the
/// resolution adapter discards it and sets `cost_payment_failed_flag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaymentFailure {
    pub reason: String,
}

impl PaymentFailure {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Build a [`PaymentOutcome::Failed`] from a reason string.
fn payment_failed(reason: impl Into<String>) -> PaymentOutcome {
    PaymentOutcome::Failed {
        reason: PaymentFailure::new(reason),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaymentOutcome {
    /// The cost was paid in full.
    Paid,
    /// CR 614.6 + CR 616.1: replacement processing interrupted payment,
    /// either for replacement ordering or for an interactive substitute.
    Paused { remaining_cost: Option<AbilityCost> },
    /// CR 601.2h / CR 118.12: the cost was not (fully) paid. The caller maps
    /// this to the scope-appropriate failure channel (see [`PaymentScope`]).
    Failed { reason: PaymentFailure },
}

fn combine_remaining_costs(
    paused_remaining: Option<AbilityCost>,
    following_costs: &[AbilityCost],
) -> Option<AbilityCost> {
    let mut costs = Vec::new();
    if let Some(cost) = paused_remaining {
        costs.push(cost);
    }
    costs.extend(following_costs.iter().cloned());
    match costs.len() {
        0 => None,
        1 => costs.into_iter().next(),
        _ => Some(AbilityCost::Composite { costs }),
    }
}

/// CR 118.12 + CR 605.3b + CR 616.1: A paused mana-source cost must retain
/// the *whole* concrete suffix that remains unpaid.  In particular, a dynamic
/// mana leaf is resolved before it becomes the leading component of the
/// serialized `EffectPayCost` root.
fn resume_cost_with_concrete_mana(
    resume_cost: Option<&AbilityCost>,
    mana_cost: crate::types::mana::ManaCost,
) -> AbilityCost {
    let concrete = AbilityCost::Mana { cost: mana_cost };
    let Some(resume_cost) = resume_cost else {
        return concrete;
    };
    let mut flattened = Vec::new();
    flatten_cost_components(resume_cost, &mut flattened);
    let first = flattened
        .first_mut()
        .expect("a mana payment suffix is never empty");
    if !matches!(
        first,
        AbilityCost::Mana { .. } | AbilityCost::ManaDynamic { .. }
    ) {
        unreachable!("a mana payment root must begin with mana");
    }
    *first = concrete;
    combine_remaining_costs(None, &flattened).expect("a concrete mana suffix is never empty")
}

/// CR 118.12 + CR 605.3b + CR 616.1: A deferred Phyrexian-style life
/// replacement begins only after the leading mana component was spent. Remove
/// that committed prefix while retaining every later composite component.
pub(crate) fn remaining_cost_after_paid_mana_prefix(cost: &AbilityCost) -> Option<AbilityCost> {
    let mut flattened = Vec::new();
    flatten_cost_components(cost, &mut flattened);
    let first = flattened
        .first()
        .expect("a deferred mana-payment root is never empty");
    assert!(
        matches!(
            first,
            AbilityCost::Mana { .. } | AbilityCost::ManaDynamic { .. }
        ),
        "a deferred mana-payment root must begin with mana"
    );
    flattened.remove(0);
    combine_remaining_costs(None, &flattened)
}

/// Flatten nested Composite nodes only while constructing a serialized payment
/// suffix. The runtime payment order is unchanged; this makes every later leaf
/// explicit so an interrupted nested Composite cannot drop an outer sibling.
fn flatten_cost_components(cost: &AbilityCost, components: &mut Vec<AbilityCost>) {
    match cost {
        AbilityCost::Composite { costs } => {
            for cost in costs {
                flatten_cost_components(cost, components);
            }
        }
        cost => components.push(cost.clone()),
    }
}

/// CR 118.12 + CR 605.3b + CR 616.1: A nested composite carries the unpaid
/// suffix of each enclosing composite into a paused mana-payment root. The
/// root begins with `active_cost`; anything after that prefix belongs to an
/// enclosing composite and remains unpaid when a child component pauses.
fn enclosing_composite_suffix(
    active_cost: &AbilityCost,
    resume_cost: Option<&AbilityCost>,
) -> Vec<AbilityCost> {
    let Some(resume_cost) = resume_cost else {
        return Vec::new();
    };

    let mut active_components = Vec::new();
    flatten_cost_components(active_cost, &mut active_components);
    let mut resume_components = Vec::new();
    flatten_cost_components(resume_cost, &mut resume_components);
    resume_components
        .strip_prefix(active_components.as_slice())
        .expect("an enclosing resume cost begins with its active composite")
        .to_vec()
}

/// CR 118.12 + CR 605.3b + CR 616.1: Builds the concrete unpaid suffix for a
/// composite component, including every enclosing composite's later leaves.
fn composite_cost_suffix(
    leading: Option<&AbilityCost>,
    following: &[AbilityCost],
    enclosing_suffix: &[AbilityCost],
) -> Option<AbilityCost> {
    let mut components = Vec::new();
    if let Some(leading) = leading {
        flatten_cost_components(leading, &mut components);
    }
    for cost in following {
        flatten_cost_components(cost, &mut components);
    }
    components.extend(enclosing_suffix.iter().cloned());
    combine_remaining_costs(None, &components)
}

/// Resolve a cost's dynamic amount in the active scope (plan §2): activation
/// uses `resolve_quantity` (player + source); resolution uses
/// `resolve_quantity_with_targets` against the payer-adjusted ability so
/// event/target refs read the right object (CR 608.2k).
fn resolve_cost_quantity(
    state: &GameState,
    expr: &crate::types::ability::QuantityExpr,
    player: PlayerId,
    source_id: ObjectId,
    scope: &PaymentScope,
) -> i32 {
    match scope {
        PaymentScope::Activation { .. } => resolve_quantity(state, expr, player, source_id),
        PaymentScope::Resolution { ability, .. } => {
            resolve_quantity_with_targets(state, expr, ability)
        }
    }
}

/// CR 118.12 + CR 605.3b: A generic `Effect::PayCost` owns the exact
/// payer-adjusted resolved ability and concrete mana cost while an auto-tapped
/// mana source is paused by a replacement effect. Other resolution roots (in
/// particular `UnlessPayment`) retain their own typed outer context.
fn effect_pay_cost_mana_resume(
    state: &GameState,
    payer: PlayerId,
    scope: &PaymentScope,
    cost: AbilityCost,
) -> Option<ManaAbilityResume> {
    // CR 601.2h + CR 605.3b + CR 616.1: A manual mana-payment window is
    // likewise already an authoritative root.  Preserve it verbatim while a
    // source selected from that window pauses, so replacement resolution
    // returns the player to the same payment flow rather than to priority.
    if let WaitingFor::ManaPayment {
        player,
        convoke_mode,
    }
    | WaitingFor::ManaSourceSelection {
        player,
        convoke_mode,
        ..
    } = &state.waiting_for
    {
        return Some(ManaAbilityResume::ManaPayment {
            outer_player: Some(*player),
            convoke_mode: *convoke_mode,
        });
    }
    // CR 118.12 + CR 605.3b + CR 616.1: `UnlessPayment` is already the
    // authoritative outer payment root.  A mana source paused while funding
    // it must return to that exact prompt, not manufacture an Effect::PayCost
    // retry that would bypass the player's submitted unless-payment flow.
    if let WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    } = &state.waiting_for
    {
        return Some(ManaAbilityResume::UnlessPayment {
            outer_player: Some(*player),
            cost: Box::new(cost.clone()),
            pending_effect: pending_effect.clone(),
            trigger_event: trigger_event.clone(),
            effect_description: effect_description.clone(),
            remaining: remaining.clone(),
        });
    }
    let PaymentScope::Resolution {
        ability,
        cost_move_root: ResolutionCostMoveRoot::EffectPayCost,
    } = scope
    else {
        return None;
    };
    let WaitingFor::Priority { player: return_to } = &state.waiting_for else {
        return None;
    };
    Some(ManaAbilityResume::EffectPayCost {
        payer,
        return_to: *return_to,
        ability: Box::new((*ability).clone()),
        cost: Box::new(cost),
    })
}

/// CR 601.2h + CR 616.1: Pause cost payment for a competing replacement effect.
pub(crate) fn pause_cost_payment_for_replacement_choice(
    state: &mut GameState,
    choice_player: PlayerId,
) {
    state.waiting_for = super::replacement::replacement_choice_waiting_for(choice_player, state);
}

/// CR 601.2h + CR 602.2b + CR 616.1: Move a self-referential activation cost
/// through the zone-change pipeline. The activation caller replaces the
/// provisional continuation with its typed root after this payment function
/// returns.
fn move_self_activation_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    destination: Zone,
    events: &mut Vec<GameEvent>,
) -> Option<PaymentOutcome> {
    match zone_pipeline::move_object(
        state,
        ZoneMoveRequest::cost(source_id, destination, source_id),
        events,
    ) {
        ZoneMoveResult::Done => None,
        ZoneMoveResult::NeedsChoice(choice_player) => {
            state.pending_cost_move_resume = Some(PendingCostMoveResume::Cast {
                player,
                pending: None,
                chosen: vec![source_id],
                paused_at_index: 0,
                destination,
                completion: PendingCostMoveCompletion::FinishPending,
            });
            // A mandatory replacement may have delivered this cost move and
            // surfaced its own post-effect prompt. Only a still-pending CR
            // 616.1 ordering choice is synthesized here; never clobber the
            // live delivery-tail prompt.
            if state.pending_replacement.is_some() {
                pause_cost_payment_for_replacement_choice(state, choice_player);
            }
            Some(PaymentOutcome::Paused {
                remaining_cost: None,
            })
        }
        ZoneMoveResult::NeedsAuraAttachmentChoice => {
            unreachable!("a cost move to Hand or Exile cannot require Aura attachment")
        }
    }
}

/// CR 406.6: Record an "exiled with [source] this turn" relation only for a
/// cost object that actually arrived in exile after replacements applied.
fn record_delivered_cost_exile(state: &mut GameState, exiled_id: ObjectId, source_id: ObjectId) {
    if state
        .objects
        .get(&exiled_id)
        .is_some_and(|object| object.zone == Zone::Exile)
    {
        super::exile_links::push_exiled_with_source_this_turn(state, exiled_id, source_id);
    }
}

/// CR 614.12a + CR 616.1: Continue a forced MayCost exile after the inner
/// replacement choice delivered or prevented its current object. The outer
/// optional replacement resumes only after the whole cost has finished.
pub(crate) fn resume_replacement_may_cost_move(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(PendingCostMoveResume::ReplacementMayCost {
        source_id,
        current,
        remaining,
        paid_count,
        outer_replacement,
    }) = state.pending_cost_move_resume.take()
    else {
        unreachable!("replacement MayCost resume requires its typed continuation")
    };

    record_delivered_cost_exile(state, current, source_id);

    for (index, &object_id) in remaining.iter().enumerate() {
        match zone_pipeline::move_object(
            state,
            ZoneMoveRequest::cost(object_id, Zone::Exile, source_id),
            events,
        ) {
            ZoneMoveResult::Done => record_delivered_cost_exile(state, object_id, source_id),
            ZoneMoveResult::NeedsChoice(choice_player) => {
                state.pending_cost_move_resume = Some(PendingCostMoveResume::ReplacementMayCost {
                    source_id,
                    current: object_id,
                    remaining: remaining[index + 1..].to_vec(),
                    paid_count,
                    outer_replacement,
                });
                pause_cost_payment_for_replacement_choice(state, choice_player);
                return Ok(state.waiting_for.clone());
            }
            ZoneMoveResult::NeedsAuraAttachmentChoice => {
                unreachable!("a cost move to Exile cannot require Aura attachment")
            }
        }
    }

    let Some(outer_replacement) = outer_replacement else {
        return Err(EngineError::InvalidAction(
            "replacement MayCost cost-move resume is missing its outer replacement".to_string(),
        ));
    };
    state.last_effect_count = Some(paid_count);
    state.pending_replacement = Some(*outer_replacement);
    super::engine_replacement::handle_replacement_choice(state, 0, events)
}

pub fn pay_ability_cost_for_activation(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    ability_index: Option<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    pay_ability_cost_for_activation_with_cost_move_replacement(
        state,
        player,
        source_id,
        cost,
        ability_index,
        events,
    )
}

fn pay_ability_cost_for_activation_with_cost_move_replacement(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    ability_index: Option<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    let excluded_sources = ability_mana_payment_excluded_sources(cost, source_id);
    let outcome = pay_ability_cost_inner(
        state,
        player,
        source_id,
        cost,
        events,
        &PaymentScope::Activation {
            excluded_sources: &excluded_sources,
            ability_index,
        },
        None,
    )?;
    // CR 601.2h: "Unpayable costs can't be paid." Activation scope maps a
    // payment failure to an illegal action — the authority's `Failed` is the
    // activation flow's `Err(ActionNotAllowed)`, preserving the pre-Phase-2
    // contract so the `if let Paused` call sites are unaffected.
    match outcome {
        PaymentOutcome::Failed { reason } => Err(EngineError::ActionNotAllowed(reason.reason)),
        paid_or_paused => Ok(paid_or_paused),
    }
}

/// CR 118.12: Pay an ability's cost during the resolution of an
/// `Effect::PayCost`. `ability` is the payer-adjusted clone (see
/// [`PaymentScope::Resolution`]); `payer` is its resolved controller.
pub(crate) fn pay_ability_cost_for_resolution(
    state: &mut GameState,
    payer: PlayerId,
    cost: &AbilityCost,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    let outcome = pay_ability_cost_for_resolution_with_cost_move_root(
        state,
        payer,
        cost,
        ability,
        ResolutionCostMoveRoot::EffectPayCost,
        events,
    )?;
    // CR 118.1 + CR 119.4b: `effects::pay` records a concrete life component
    // on its continuation-owned ability before this authority can pause. Stamp
    // it only after the entire cost finishes, including a mana-root resume.
    // `None` is deliberately distinct from `Some(0)`: mana-only costs retain
    // their preceding amount, while a completed zero-life cost reports zero.
    if outcome == PaymentOutcome::Paid {
        if let Some(amount) = ability.context.pay_cost_paid_life_amount {
            state.last_effect_amount = Some(amount as i32);
        }
    }
    Ok(outcome)
}

/// Pays a replacement's MayCost. Its dedicated root owns the outer
/// replacement state required by [`PendingCostMoveResume::ReplacementMayCost`].
pub(crate) fn pay_ability_cost_for_replacement_may_cost(
    state: &mut GameState,
    payer: PlayerId,
    cost: &AbilityCost,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    pay_ability_cost_for_resolution_with_cost_move_root(
        state,
        payer,
        cost,
        ability,
        ResolutionCostMoveRoot::ReplacementMayCost,
        events,
    )
}

fn pay_ability_cost_for_resolution_with_cost_move_root(
    state: &mut GameState,
    payer: PlayerId,
    cost: &AbilityCost,
    ability: &ResolvedAbility,
    cost_move_root: ResolutionCostMoveRoot,
    events: &mut Vec<GameEvent>,
) -> Result<PaymentOutcome, EngineError> {
    pay_ability_cost_inner(
        state,
        payer,
        ability.source_id,
        cost,
        events,
        &PaymentScope::Resolution {
            ability,
            cost_move_root,
        },
        Some(cost),
    )
}

fn pay_ability_cost_inner(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
    scope: &PaymentScope,
    resume_cost: Option<&AbilityCost>,
) -> Result<PaymentOutcome, EngineError> {
    // CR 118.3 / CR 601.2h: at resolution there is no interactive interceptor or
    // activation-window mana detour, so any shape outside the resolution-payable
    // set has no real payment arm here. One structural guard (shared with
    // `can_pay_resolution` via `supported_at_resolution`) refuses them as
    // `Failed` up front — never a silent `Paid` no-op, never an unintended
    // execution — so a shape that slips past the pre-gate fails loudly into the
    // effect's `cost_payment_failed_flag` branch (CR 118.12).
    if matches!(scope, PaymentScope::Resolution { .. }) && !supported_at_resolution(cost) {
        return Ok(payment_failed(
            "unsupported resolution-time AbilityCost payment shape",
        ));
    }
    match cost {
        AbilityCost::Tap => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: source is not on the battlefield".to_string(),
                ));
            }
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
            // CR 701.26a + CR 508.1f: route the {T}-cost tap through the single
            // authority so a "can't become tapped" source is refused (the primary
            // gate is `check_summoning_sickness_for_cost`; this is the backstop).
            crate::game::restrictions::tap_permanent_for_cost(state, source_id, events)?;
        }
        // CR 107.6: The untap symbol in a cost means "Untap this permanent. A
        // permanent that's already untapped can't be untapped again to pay the
        // cost." Mirrors the `AbilityCost::Tap` arm above: paying is illegal when
        // the source is in the wrong tap state, so the activation fails rather
        // than silently no-op'ing (which would let Umbral Mantle-style {Q} pumps
        // fire on an untapped creature, against the rules).
        AbilityCost::Untap => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot pay untap cost: source is not on the battlefield".to_string(),
                ));
            }
            if !obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot pay untap cost: permanent is already untapped".to_string(),
                ));
            }
            let untapped = crate::game::object_state::resolve_and_apply_object_edit(
                state,
                source_id,
                crate::types::resolved_commands::ResolvedObjectStatus::Tapped,
                false,
            )
            .map_err(|error| EngineError::InvalidAction(error.to_string()))?;
            debug_assert!(untapped, "preflighted untap cost must transition status");
            events.push(GameEvent::PermanentUntapped {
                object_id: source_id,
            });
        }
        AbilityCost::Mana { cost } => match scope {
            // CR 601.2g: Activation pays through the mana-ability window. CR
            // 106.6: restriction enforcement routes through `allows_activation`
            // (not `allows_spell`) via the activation context built from the
            // source permanent's types.
            PaymentScope::Activation {
                excluded_sources,
                ability_index,
                ..
            } => {
                let resume_at_resolution_depth = state.resolution_stack.len();
                let payment = if excluded_sources.is_empty() {
                    pay_ability_mana_cost(state, player, source_id, *ability_index, cost, events)?
                } else {
                    pay_ability_mana_cost_excluding(
                        state,
                        player,
                        source_id,
                        *ability_index,
                        cost,
                        events,
                        excluded_sources,
                        // Top-level ability cost payment: no outer cost on the stack.
                        None,
                    )?
                };
                match payment {
                    super::casting::ManaCostPayment::Paid(()) => {}
                    super::casting::ManaCostPayment::Paused {
                        remaining_life_payments,
                        ..
                    } => {
                        // CR 107.4f + CR 118.3b + CR 119.4 + CR 616.1: The
                        // announcing caller attaches its complete activation
                        // root before returning control to a player.
                        state.pending_deferred_life_cost_resume = Some(
                            crate::types::game_state::DeferredLifeCostResume::Cast {
                                player,
                                pending: None,
                                remaining_life_payments,
                                resume_at_resolution_depth,
                            },
                        );
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
            }
            // CR 118.12: Resolution-time mana payment uses the effect-context
            // auto-tap path. Pre-flight then pay; either step failing is a
            // payment failure (not an engine error).
            PaymentScope::Resolution { .. } => {
                if !can_pay_effect_mana_cost_after_auto_tap(state, player, source_id, cost) {
                    return Ok(payment_failed("insufficient mana"));
                }
                let resume = effect_pay_cost_mana_resume(
                    state,
                    player,
                    scope,
                    resume_cost_with_concrete_mana(resume_cost, cost.clone()),
                );
                if pay_effect_mana_cost_with_resume(
                    state,
                    player,
                    source_id,
                    cost,
                    resume.as_ref(),
                    events,
                )
                .is_err()
                {
                    // CR 118.12 + CR 605.3b + CR 616.1: The mana ability
                    // cursor, rather than the unless-payment handler, owns
                    // the replacement choice and exact resume state.
                    if mana_ability_cost_payment_is_paused(state)
                        || state.pending_deferred_life_cost_resume.is_some()
                    {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                    return Ok(payment_failed("insufficient mana"));
                }
            }
        },
        // CR 118.4 + CR 107.3c: Dynamic-generic mana. At activation it should
        // have been announced/resolved upstream (error). At resolution it
        // resolves the dynamic generic against the payer-adjusted ability and
        // pays it via the effect-context auto-tap path.
        AbilityCost::ManaDynamic { quantity } => match scope {
            PaymentScope::Activation { .. } => {
                return Ok(payment_failed(
                    "ManaDynamic cost should be resolved upstream",
                ));
            }
            PaymentScope::Resolution { .. } => {
                let amount = resolve_cost_quantity(state, quantity, player, source_id, scope);
                let mana_cost = crate::types::mana::ManaCost::generic(amount.max(0) as u32);
                if !can_pay_effect_mana_cost_after_auto_tap(state, player, source_id, &mana_cost) {
                    return Ok(payment_failed("insufficient mana"));
                }
                let resume = effect_pay_cost_mana_resume(
                    state,
                    player,
                    scope,
                    resume_cost_with_concrete_mana(resume_cost, mana_cost.clone()),
                );
                if pay_effect_mana_cost_with_resume(
                    state,
                    player,
                    source_id,
                    &mana_cost,
                    resume.as_ref(),
                    events,
                )
                .is_err()
                {
                    // CR 118.12 + CR 605.3b + CR 616.1: See the concrete
                    // mana-cost arm above; the replacement-aware cursor owns
                    // this pause as well.
                    if mana_ability_cost_payment_is_paused(state)
                        || state.pending_deferred_life_cost_resume.is_some()
                    {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                    return Ok(payment_failed("insufficient mana"));
                }
            }
        },
        AbilityCost::Composite { costs } => {
            let enclosing_suffix = enclosing_composite_suffix(cost, resume_cost);
            for (index, sub_cost) in costs.iter().enumerate() {
                let prior_waiting_for = state.waiting_for.clone();
                let sub_resume_cost = composite_cost_suffix(
                    Some(sub_cost),
                    &costs[index + 1..],
                    &enclosing_suffix,
                )
                .expect("a composite component always has an unpaid suffix");
                let outcome = pay_ability_cost_inner(
                    state,
                    player,
                    source_id,
                    sub_cost,
                    events,
                    scope,
                    matches!(scope, PaymentScope::Resolution { .. }).then_some(&sub_resume_cost),
                )?;
                match outcome {
                    PaymentOutcome::Paid => {
                        // CR 118.12: Some resolution-time sub-costs acquire a
                        // player choice by setting `waiting_for` (currently
                        // `DiscardChoice`). Stop here and preserve later
                        // sub-costs as the continuation so they are paid only
                        // after the choice is committed.
                        if matches!(scope, PaymentScope::Resolution { .. })
                            && state.waiting_for != prior_waiting_for
                        {
                            return Ok(PaymentOutcome::Paused {
                                remaining_cost: composite_cost_suffix(
                                    None,
                                    &costs[index + 1..],
                                    &enclosing_suffix,
                                ),
                            });
                        }
                    }
                    PaymentOutcome::Paused { remaining_cost } => {
                        // CR 118.12 + CR 605.3b + CR 616.1: A typed mana root
                        // already owns this component and every later component.
                        // Never copy that suffix into the generic effect
                        // continuation: it would retry a paid prefix or let the
                        // rider run before the unpaid cost is settled.
                        if matches!(scope, PaymentScope::Resolution { .. })
                            && (mana_ability_cost_payment_is_paused(state)
                                || state.pending_deferred_life_cost_resume.is_some())
                        {
                            return Ok(PaymentOutcome::Paused {
                                remaining_cost: None,
                            });
                        }
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: composite_cost_suffix(
                                remaining_cost.as_ref(),
                                &costs[index + 1..],
                                &enclosing_suffix,
                            ),
                        });
                    }
                    // CR 601.2h: Partial payments are not allowed; resolution-
                    // scope callers pre-gate the whole composite via
                    // `can_pay`, so a mid-composite `Failed` propagates without
                    // committing the remaining sub-costs.
                    failed @ PaymentOutcome::Failed { .. } => return Ok(failed),
                }
            }
        }
        // CR 119.4: Paying life IS losing life. Activation applies direct
        // "can't pay life" statics (`pay_life_as_cast_or_activation_cost`);
        // resolution routes through `pay_life_as_cost` (CR 118.12).
        AbilityCost::PayLife { amount } => {
            let amount = resolve_cost_quantity(state, amount, player, source_id, scope);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            let result = match scope {
                PaymentScope::Activation { .. } => {
                    super::life_costs::pay_life_as_cast_or_activation_cost(
                        state, player, amount, events,
                    )
                }
                PaymentScope::Resolution { .. } => {
                    super::life_costs::pay_life_as_cost(state, player, amount, events)
                }
            };
            match result {
                super::life_costs::PayLifeCostResult::Paid { .. } => {}
                super::life_costs::PayLifeCostResult::PaidWithDeferredSubstitution { .. }
                | super::life_costs::PayLifeCostResult::DeferredReplacementChoice { .. } => {
                    return Ok(PaymentOutcome::Paused {
                        remaining_cost: None,
                    });
                }
                super::life_costs::PayLifeCostResult::InsufficientLife
                | super::life_costs::PayLifeCostResult::Prohibited => {
                    return Ok(payment_failed("Cannot pay life cost"));
                }
            }
        }
        // CR 118.3: Sacrifice as a cost — sacrifice the source (SelfRef) or a chosen permanent.
        AbilityCost::Sacrifice(cost) => {
            if matches!(cost.target, TargetFilter::SelfRef) {
                if super::static_abilities::player_cant_sacrifice_as_cost(state, player, source_id)
                {
                    return Ok(payment_failed("Cannot sacrifice this permanent as a cost"));
                }
                match super::sacrifice::sacrifice_permanent(state, source_id, player, events)? {
                    super::sacrifice::SacrificeOutcome::Complete => {}
                    super::sacrifice::SacrificeOutcome::NeedsReplacementChoice(choice_player) => {
                        pause_cost_payment_for_replacement_choice(state, choice_player);
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
            } else {
                // Non-self sacrifice costs (e.g., "Sacrifice a creature") are handled
                // by the interactive WaitingFor::SacrificeForCost flow — they are
                // intercepted before reaching pay_ability_cost.
            }
        }
        // CR 207.2c + CR 602.1: Discard the source card itself as part of the cost (Channel).
        AbilityCost::Discard {
            self_scope: crate::types::ability::DiscardSelfScope::SourceCard,
            ..
        } => match super::effects::discard::discard_as_cost(state, source_id, player, events) {
            super::effects::discard::DiscardOutcome::Complete => {}
            super::effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) => {
                pause_cost_payment_for_replacement_choice(state, choice_player);
                return Ok(PaymentOutcome::Paused {
                    remaining_cost: None,
                });
            }
        },
        // CR 118.12 + CR 701.9: Resolution-time "discard N cards of your choice"
        // cost (e.g. "discard a card"). The choice of which cards to discard is
        // acquired via a `WaitingFor::DiscardChoice` round-trip when there is a
        // real choice; when the eligible set exactly fills the requirement the
        // discard auto-pays. This shape is resolution-only — the activation
        // flow surfaces hand-discard costs through the `WaitingFor::PayCost`
        // detour before payment, so the activation scope falls through to the
        // interactive-pass-through arm below.
        AbilityCost::Discard {
            count,
            filter,
            selection: crate::types::ability::CardSelectionMode::Chosen,
            self_scope: crate::types::ability::DiscardSelfScope::FromHand,
        } if matches!(scope, PaymentScope::Resolution { .. }) => {
            let count =
                resolve_cost_quantity(state, count, player, source_id, scope).max(0) as usize;
            let eligible = find_eligible_discard_targets(state, player, source_id, filter.as_ref());
            if eligible.len() < count {
                return Ok(payment_failed("not enough cards to discard"));
            }
            if count == 0 {
                // CR 118.12: record the (zero) paid count for downstream chain
                // steps that read `QuantityRef::EventContextAmount`.
                state.last_effect_count = Some(0);
                return Ok(PaymentOutcome::Paid);
            }
            // Forced-choice fast path (plan R4): when the eligible set exactly
            // fills the requirement there is no choice to surface, so the
            // discard executes immediately. This is a runtime check, not a
            // classifier fact.
            if eligible.len() == count {
                for card_id in eligible {
                    if let super::effects::discard::DiscardOutcome::NeedsReplacementChoice(
                        choice_player,
                    ) = super::effects::discard::discard_as_cost(state, card_id, player, events)
                    {
                        pause_cost_payment_for_replacement_choice(state, choice_player);
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                state.last_effect_count = Some(count as i32);
            } else {
                state.waiting_for = WaitingFor::DiscardChoice {
                    player,
                    count,
                    cards: eligible,
                    source_id,
                    effect_kind: EffectKind::PayCost,
                    up_to: false,
                    unless_filter: None,
                    discard_frame: None,
                };
            }
        }
        // CR 118.12 + CR 701.26a: Resolution-time optional "tap N untapped
        // creatures you control" costs need a player selection before the
        // reflexive "When you do" rider can resolve. Surface the same PayCost
        // object-selection state used by activation/casting costs, but resume
        // the current effect chain instead of a spell cast.
        AbilityCost::TapCreatures {
            requirement,
            filter,
        } if matches!(scope, PaymentScope::Resolution { .. }) => {
            let PaymentScope::Resolution { ability, .. } = scope else {
                unreachable!("guarded above");
            };
            let eligible = find_eligible_tap_creatures_targets(state, player, ability, filter);
            // CR 107.3a + CR 118.3 + CR 601.2h: Resolution-time TapCreatures costs
            // use the same `u32::MAX` X-sentinel encoding as activation-time costs,
            // so the payable range must come from the single bounds authority
            // (`sacrifice_cost_bounds`) rather than a raw `as usize` cast on
            // `count`. A fixed (non-X) count degrades to `(count, count)`,
            // preserving the CR 601.2h exact-payment requirement for every
            // existing card (Kitt Kanto's `count: 2`, Meanders Guide's
            // `count: 1`) unchanged, while correcting the previously-hardcoded
            // `min_count: 0` that silently allowed partial payment once the
            // shared selection validator (`pay_tap_creatures_selection`)
            // switched from an exact-match check to a `[min_count, count]`
            // range check.
            //
            // CR 107.3a: compute the selection semantics once from the
            // requirement and carry them verbatim to the completion handler.
            let mode = requirement.selection_mode();
            let (kind, count, min_count) = match requirement {
                crate::types::ability::TapCreaturesRequirement::Count { count } => {
                    let (min_count, max_count) =
                        super::casting::sacrifice_cost_bounds(*count, eligible.len());
                    if eligible.len() < min_count {
                        return Ok(payment_failed("not enough creatures to tap"));
                    }
                    (PayCostKind::TapCreatures { mode }, max_count, min_count)
                }
                crate::types::ability::TapCreaturesRequirement::Aggregate {
                    stat,
                    comparator,
                    value,
                } => {
                    let aggregate = crate::types::ability::TapCreaturesAggregate {
                        stat: *stat,
                        comparator: *comparator,
                        value: *value,
                    };
                    let total_positive_power =
                        super::casting_costs::tap_creatures_total_power(state, &eligible);
                    if !aggregate.satisfied_by(total_positive_power) {
                        return Ok(payment_failed(
                            "eligible creatures do not satisfy tap-creatures aggregate cost",
                        ));
                    }
                    // CR 208.1 + CR 601.2f (Crew CR 702.122a / Saddle CR 702.171a /
                    // Teamwork CR 702.194a): the aggregate form taps ANY number of
                    // creatures whose total positive power satisfies the
                    // comparator, so every subset size is admissible and the floor
                    // stays 0. `pay_tap_creatures_selection`'s `Aggregate(_)`
                    // branch validates the comparator instead of the
                    // `[min_count, count]` range.
                    (PayCostKind::TapCreatures { mode }, eligible.len(), 0)
                }
            };
            if count == 0 {
                state.last_effect_count = Some(0);
                return Ok(PaymentOutcome::Paid);
            }
            state.waiting_for = WaitingFor::PayCost {
                player,
                kind,
                choices: eligible,
                count,
                min_count,
                resume: CostResume::Resolution,
            };
            return Ok(PaymentOutcome::Paused {
                remaining_cost: None,
            });
        }
        // CR 118.3: A self-ref "exile this card" activation cost — the source
        // exiles itself from whatever zone the cost names. Covers exile-from-
        // graveyard costs (CR 702.97a Scavenge, Renew), the exile-from-hand
        // cost of CR 702.62a Suspend ("you may pay [cost] and exile it"), and
        // the exile-from-hand cost of CR 702.170a Plot ("you may exile this
        // card from your hand and pay [cost]"). The source is identified by
        // SelfRef; no player choice is needed, so this is an auto-payable cost
        // (no WaitingFor round-trip). Non-self exile costs (targeted exile from
        // any zone) are still handled by the catch-all below.
        AbilityCost::Exile {
            filter: Some(TargetFilter::SelfRef),
            zone,
            count: 1,
        } => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exile cost".to_string())
            })?;
            // CR 118.3 + CR 602.2b: an explicit zone validates the source's
            // location during cost payment; a missing zone exiles the source
            // from whatever zone it is currently in (e.g. a land's "Exile this
            // land" paid from the battlefield).
            if let Some(z) = zone {
                if obj.zone != *z {
                    return Ok(payment_failed(format!(
                        "Cannot exile self for cost: source is not in {z:?}"
                    )));
                }
            }
            let PaymentScope::Activation { .. } = scope
            else {
                unreachable!("self-referential exile costs are not payable at resolution")
            };
            if let Some(outcome) =
                move_self_activation_cost(state, player, source_id, Zone::Exile, events)
            {
                return Ok(outcome);
            }
        }
        // CR 406.6: Non-self exile cost at resolution time (e.g., The Mimeoplasm's
        // "exile two creature cards from graveyards"). The interactive choice is
        // surfaced via WaitingFor::EffectZoneChoice with is_cost_payment: true.
        AbilityCost::Exile {
            count,
            zone,
            filter,
        } if !matches!(filter, Some(TargetFilter::SelfRef))
            && matches!(scope, PaymentScope::Resolution { .. }) =>
        {
            let count = *count as usize;
            let effective_zone = zone.unwrap_or(Zone::Graveyard);
            let eligible = find_eligible_exile_targets(
                state,
                player,
                source_id,
                effective_zone,
                filter.as_ref(),
            );
            if eligible.len() < count {
                return Ok(payment_failed("not enough cards to exile"));
            }
            if count == 0 {
                // CR 118.12: record the (zero) paid count for downstream chain
                // steps that read `QuantityRef::EventContextAmount`.
                state.last_effect_count = Some(0);
                return Ok(PaymentOutcome::Paid);
            }
            // Forced-choice fast path: when the eligible set exactly
            // fills the requirement there is no choice to surface, so the
            // exile executes immediately.
            if eligible.len() == count
                && matches!(
                    scope,
                    PaymentScope::Resolution {
                        cost_move_root: ResolutionCostMoveRoot::ReplacementMayCost,
                        ..
                    }
                )
            {
                for (index, &card_id) in eligible.iter().enumerate() {
                    match zone_pipeline::move_object(
                        state,
                        ZoneMoveRequest::cost(card_id, Zone::Exile, source_id),
                        events,
                    ) {
                        ZoneMoveResult::Done => {
                            record_delivered_cost_exile(state, card_id, source_id);
                        }
                        ZoneMoveResult::NeedsChoice(choice_player) => {
                            state.pending_cost_move_resume =
                                Some(PendingCostMoveResume::ReplacementMayCost {
                                    source_id,
                                    current: card_id,
                                    remaining: eligible[index + 1..].to_vec(),
                                    paid_count: count as i32,
                                    outer_replacement: None,
                                });
                            pause_cost_payment_for_replacement_choice(state, choice_player);
                            return Ok(PaymentOutcome::Paused {
                                remaining_cost: None,
                            });
                        }
                        ZoneMoveResult::NeedsAuraAttachmentChoice => {
                            unreachable!("a cost move to Exile cannot require Aura attachment")
                        }
                    }
                }
                state.last_effect_count = Some(count as i32);
            } else {
                state.waiting_for = WaitingFor::EffectZoneChoice {
                    player,
                    cards: eligible,
                    count,
                    min_count: 0,
                    up_to: false,
                    source_id,
                    effect_kind: crate::types::ability::EffectKind::PayCost,
                    zone: effective_zone,
                    destination: Some(Zone::Exile),
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enter_transformed: false,
                    enters_under_player: None,
                    enters_attacking: false,
                    owner_library: false,
                    track_exiled_by_source: true,
                    face_down_profile: None,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![],
                    count_param: 0,
                    library_position: None,
                    is_cost_payment: true,
                    enters_modified_if: None,
                    duration: None,
                };
                return Ok(PaymentOutcome::Paused {
                    remaining_cost: None,
                });
            }
        }
        // CR 702.167a: Craft's materials are exiled by the interactive
        // `WaitingFor::PayCost { kind: ExileMaterials }` detour before this
        // resume runs, so this arm is an idempotent no-op (mirrors the non-self
        // `Sacrifice` arm above). It exists as its own arm — not folded into the
        // catch-all — so a future change to the materials payment shape forces a
        // deliberate decision here.
        AbilityCost::ExileMaterials { .. } => {}
        // Waterbend cost was already paid via ManaPayment before reaching pay_ability_cost.
        AbilityCost::Waterbend { .. } => {}
        // CR 118.1: An effect performed as a cost — "a cost is an action or
        // payment necessary to take another action … to pay a cost, a player
        // carries out the instructions specified". (NOT CR 118.3, which is the
        // resources rule and says nothing about an effect-as-cost.) Resolve the
        // effect on the source before the ability's own effect fires. The shared
        // support predicate admits only deterministic source-counter and
        // fixed-mana forms, so the effect shape itself asks the payer nothing.
        // A replacement on the resulting event can still require a player
        // choice, which parks the payment as `Paused` in the `PutCounter` arm
        // below.
        AbilityCost::EffectCost { effect } => {
            use crate::types::ability::Effect;
            match effect.as_ref() {
                Effect::PutCounter {
                    counter_type,
                    count,
                    target: TargetFilter::SelfRef,
                } => {
                    let count = resolve_cost_quantity(state, count, player, source_id, scope);
                    // CR 614.17b: "If an event can't happen, a player can't
                    // choose to pay a cost that includes that event" — a
                    // prevented counter placement pays none of this cost. The
                    // shared add primitive reports both a delivered and
                    // prevented event as complete because effect resolution
                    // needs that distinction only for continuation; payment must
                    // reject the prevented case before executing it.
                    //
                    // The gate is `replacement::mandatory_prevention_applies`
                    // (CR 614.17: "some effects state that something can't
                    // happen"): a candidate definition on the governing event
                    // whose `quantity_modification` is `Prevent` and whose mode
                    // is not optional. Only that pair can reach this refusal.
                    // CR 614.17c ("if an event can't happen, it can only be
                    // replaced by a self-replacement effect … other replacement
                    // and/or prevention effects can't modify or replace it") is
                    // implemented by `replacement::pipeline_loop`'s
                    // short-circuit, which fires ahead of any CR 616.1 ordering
                    // prompt. Its `AddCounter` replacement choice — a single
                    // optional candidate, or a CR 616.1 ordering — instead
                    // returns `CounterAdditionPreview::ChoiceRequired`, falls
                    // through, parks, and is settled as PAID by
                    // `engine_payment_choices::resume_counter_addition_unless_payment`
                    // (CR 118.12). The two legs partition the space; they do not
                    // disagree.
                    //
                    // CR 614.17b + CR 119.8 (analogue): the CHOICE is refused upstream at every
                    // RESOLUTION-scope site that consumes
                    // `resolution_cost_includes_impossible_event`, but that predicate is never
                    // consulted at `PaymentScope::Activation` — `is_payable_for_activation` admits
                    // every `EffectCost` unconditionally, and `can_pay` dry-runs this arm — so here
                    // it is the only CR 614.17b gate an activated counter cost meets.
                    let prevented = self_counter_placement_is_prohibited(
                        state,
                        player,
                        source_id,
                        counter_type.clone(),
                        counter_cost_count(count),
                    );
                    if prevented {
                        return Ok(payment_failed(
                            "Counter-placement cost prevented by a replacement effect",
                        ));
                    }
                    if !super::effects::counters::add_counter_with_replacement(
                        state,
                        player,
                        source_id,
                        counter_type.clone(),
                        counter_cost_count(count),
                        events,
                    ) {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                // CR 106.3 + CR 106.4: A Braid of Fire-style cost performs
                // fixed mana production directly into the payer's pool. This
                // uses the ordinary replacement-aware mana primitive but does
                // not resolve a separate ability or change priority mid-cost.
                Effect::Mana {
                    produced:
                        produced @ crate::types::ability::ManaProduction::Fixed { colors, .. },
                    restrictions,
                    grants,
                    expiry,
                    target: None,
                } => {
                    let restrictions = super::effects::mana::resolve_restrictions(
                        restrictions,
                        state,
                        source_id,
                    );
                    let source_could_produce_two_or_more_colors =
                        super::mana_sources::mana_production_could_produce_two_or_more_colors(
                            state, player, source_id, produced,
                        );
                    for color in colors {
                        super::mana_payment::produce_mana_with_attributes_from_source_quality(
                            state,
                            source_id,
                            super::mana_sources::mana_color_to_type(color),
                            player,
                            false,
                            source_could_produce_two_or_more_colors,
                            &restrictions,
                            grants,
                            *expiry,
                            events,
                        );
                    }
                }
                _ => {
                    return Ok(payment_failed(format!(
                        "Effect-as-cost not yet resolvable: {effect:?}"
                    )));
                }
            }
        }
        AbilityCost::Unimplemented { description } => {
            return Ok(payment_failed(format!(
                "Cost not implemented: {description}"
            )));
        }
        // CR 118.9 + CR 702.62a: a borrowed keyword cost is an alternative cost on
        // the *cast spell*, paid through the casting pipeline's
        // `ExileWithAltCost` / `ExileWithAltAbilityCost` permissions
        // (casting.rs / casting_costs.rs), never as an activation cost paid here.
        // Reaching this arm means a misrouted cost — fail loudly rather than
        // silently no-op.
        AbilityCost::KeywordCostOfCastSpell { .. } => {
            return Ok(payment_failed(
                "Keyword-cost-of-cast-spell is paid by the casting pipeline, not as an \
                 activation cost",
            ));
        }
        // CR 107.14: A player can pay {E} only if they have enough energy.
        // CR 107.3c: Resolve the `QuantityExpr` so dynamic amounts read game
        // state at payment time.
        AbilityCost::PayEnergy { amount } => {
            let amount = u32::try_from(
                resolve_cost_quantity(state, amount, player, source_id, scope).max(0),
            )
            .unwrap_or(0);
            let energy = state.players[player.0 as usize].energy;
            if energy < amount {
                return Ok(payment_failed("Not enough energy"));
            }
            if amount > 0 {
                state
                    .resolve_and_apply_player_edit(
                        player,
                        crate::types::resolved_commands::ResolvedPlayerEdit::Energy {
                            delta: -(amount as i32),
                        },
                    )
                    .expect("preflighted energy payment must apply");
            }
            events.push(GameEvent::EnergyChanged {
                player,
                delta: -(amount as i32),
            });
        }
        // CR 702.21a + CR 122.1 + CR 104.3d: Ward cost paid by giving the
        // paying player counters of a kind (The Serpent Society). No
        // affordability check (see `can_pay_resolution`) — a player may
        // always choose to accept more counters. Routes through
        // `add_player_counter_with_replacement` — not a raw
        // `resolve_and_apply_player_edit` call — so "players can't get
        // counters" replacement effects still apply, mirroring the
        // `EffectCost`/`PutCounter` arm's use of the sibling
        // `effects::counters::add_counter_with_replacement` above. A
        // replacement that PREVENTS the addition (Solemnity) is a genuinely
        // FAILED payment here, not a paused one: unlike effect resolution
        // (where "prevented" and "applied" both just mean the pending item is
        // resolved), a cost that silently gives zero counters must not be
        // mistaken for having actually been paid, or Ward's deterrent is
        // bypassed for free.
        //
        // CR 614.17b is the rule ("if an event can't happen, a player can't
        // choose to pay a cost that includes that event"). The gate that reaches
        // it is `replacement::mandatory_prevention_applies` (CR 614.17:
        // "some effects state that something can't happen"): a candidate
        // definition on the governing event whose `quantity_modification`
        // is `Prevent` and whose mode is not optional — not a semantic
        // can't-effect test. CR 614.17c is why the CR 616.1
        // ordering step never intervenes: an impossible event "can only be
        // replaced by a self-replacement effect … other replacement and/or
        // prevention effects can't modify or replace it", so
        // `replacement::pipeline_loop` short-circuits it ahead of any CR 616.1
        // prompt. Its `AddCounter` replacement choice — a single optional
        // candidate, or a CR 616.1 ordering — instead returns `NeedsChoice`,
        // parks, and is settled as PAID by
        // `engine_payment_choices::resume_counter_addition_unless_payment`
        // (CR 118.12). Same partition as the `EffectCost`/`PutCounter` cost
        // arm. `resolution_cost_includes_impossible_event` refuses the CHOICE
        // at every site that consumes it, so this refusal is defense in depth
        // for the CR 614.17a mid-window case rather than the only gate.
        AbilityCost::GetPlayerCounters {
            counter_kind,
            count,
        } => {
            match super::effects::player_counter::add_player_counter_with_replacement(
                state, player, player, *counter_kind, *count, events,
            ) {
                super::effects::player_counter::PlayerCounterAdditionOutcome::Applied => {}
                super::effects::player_counter::PlayerCounterAdditionOutcome::Prevented => {
                    return Ok(payment_failed(
                        "Player-counter cost prevented by a replacement effect",
                    ));
                }
                super::effects::player_counter::PlayerCounterAdditionOutcome::NeedsChoice => {
                    return Ok(PaymentOutcome::Paused {
                        remaining_cost: None,
                    });
                }
            }
        }
        AbilityCost::PaySpeed { amount } => {
            let amount = resolve_cost_quantity(state, amount, player, source_id, scope);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            let current_speed = effective_speed(state, player);
            if amount > current_speed {
                return Ok(payment_failed("Not enough speed"));
            }
            set_speed(state, player, Some(current_speed - amount), events);
        }
        // CR 701.3d: Explicit unattach cost. Legality is pre-gated by
        // `AbilityCost::is_payable`; payment clears both sides of the
        // attachment graph and keeps the Equipment on the battlefield.
        AbilityCost::Unattach => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for unattach cost".to_string())
            })?;
            if obj.zone != Zone::Battlefield
                || obj.controller != player
                || !obj
                    .card_types
                    .subtypes
                    .iter()
                    .any(|subtype| subtype == "Equipment")
            {
                return Ok(payment_failed(
                    "Cannot unattach: source is not a controlled battlefield Equipment",
                ));
            }
            if obj.attached_to.is_none() {
                return Ok(payment_failed("Cannot unattach: source is not attached"));
            }
            if let Some(old_target) = super::effects::attach::unattach(state, source_id) {
                events.push(GameEvent::Unattached {
                    attachment_id: source_id,
                    old_target,
                });
            }
        }
        // CR 606.4: Loyalty abilities use loyalty counter adjustment as their cost.
        // Called after target selection when the ability was initiated interactively.
        // Routes through the single-authority counter resolver so replacement
        // effects (Vorinclex, Doubling Season) can apply per CR 614.1a and
        // obj.loyalty stays in sync with counters[Loyalty] (CR 306.5b).
        AbilityCost::Loyalty { amount } => {
            let amount = *amount;
            match amount.cmp(&0) {
                std::cmp::Ordering::Greater => {
                    if !super::effects::counters::add_counter_with_replacement(
                        state,
                        player,
                        source_id,
                        crate::types::counter::CounterType::Loyalty,
                        amount as u32,
                        events,
                    ) {
                        return Ok(PaymentOutcome::Paused {
                            remaining_cost: None,
                        });
                    }
                }
                std::cmp::Ordering::Less => {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        crate::types::counter::CounterType::Loyalty,
                        (-amount) as u32,
                        events,
                    );
                }
                std::cmp::Ordering::Equal => {}
            }
        }
        // CR 118.3 + CR 122: Remove-counter cost. The SelfRef form ("Remove N
        // {type} counters from ~") is auto-payable — no player choice is needed,
        // so it lands here rather than in an interactive WaitingFor round-trip.
        // Routes through the single-authority counter resolver so replacement
        // effects (Vorinclex, Doubling Season) apply per CR 614.1a and
        // obj.loyalty/obj.defense stay in sync per CR 306.5b / CR 310.4c.
        // Legality (CR 118.3: "can't pay a cost without having the necessary
        // resources") is enforced upstream by `AbilityCost::is_payable` in
        // cost_payability.rs before activation is committed.
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            target: None,
            ..
        } => {
            if *count == REMOVE_COUNTER_COST_ALL
                && matches!(counter_type, crate::types::counter::CounterMatch::Any)
            {
                let mut counters: Vec<_> = state
                    .objects
                    .get(&source_id)
                    .map(|obj| {
                        obj.counters
                            .iter()
                            .map(|(ty, count)| (ty.clone(), *count))
                            .collect()
                    })
                    .unwrap_or_default();
                // Issue #4878: `obj.counters` is a default-RandomState HashMap;
                // sort by CounterType so the removal (and any per-type triggers
                // it emits) happen in a deterministic, process-independent order.
                counters.sort_by(|a, b| a.0.cmp(&b.0));
                for (counter_type, count) in counters {
                    super::effects::counters::remove_counter_with_replacement(
                        state,
                        source_id,
                        counter_type,
                        count,
                        events,
                    );
                }
                return Ok(PaymentOutcome::Paid);
            }
            // CR 601.2h: Resolve `CounterMatch::Any` to the concrete counter
            // type currently present on the source before the replacement
            // pipeline sees it — `remove_counter_with_replacement` operates on
            // a single concrete kind. `OfType(t)` passes through unchanged.
            if let Some(resolved) = super::effects::counters::resolve_counter_match_for_removal(
                state,
                source_id,
                counter_type,
            ) {
                let count = if *count == REMOVE_COUNTER_COST_ALL {
                    state
                        .objects
                        .get(&source_id)
                        .and_then(|obj| obj.counters.get(&resolved))
                        .copied()
                        .unwrap_or(0)
                } else {
                    *count
                };
                super::effects::counters::remove_counter_with_replacement(
                    state, source_id, resolved, count, events,
                );
            }
        }
        // Targeted remove-counter costs are paid by the interactive
        // WaitingFor::RemoveCounterForCost path before automatic cost
        // components resume here. This arm intentionally no-ops so composite
        // activation costs can still pay their remaining automatic pieces.
        AbilityCost::RemoveCounter {
            target: Some(_), ..
        } => {}
        // CR 701.43a: "To exert a permanent, its controller chooses to have it
        // not untap during its controller's next untap step." Modeled as a
        // transient continuous effect with `StaticMode::CantUntap` scoped to
        // `Duration::UntilNextStepOf { step: Untap, player: Controller }` on the source permanent,
        // identical to the "doesn't untap during its controller's next untap
        // step" pattern already handled by the layer system (see
        // `layers::prune_controller_untap_step_effects`).
        //
        // CR 701.43b: "A permanent can be exerted even if it's not tapped or
        // has already been exerted in a turn." Pushing a second identical
        // effect is harmless — both expire during the same untap step.
        //
        // CR 701.43c: "An object that isn't on the battlefield can't be
        // exerted." Enforced here so off-battlefield activations (which
        // shouldn't reach this site for Exert costs on permanents) fail
        // loudly rather than creating a dangling effect.
        AbilityCost::Exert => {
            let obj = state.objects.get(&source_id).ok_or_else(|| {
                EngineError::InvalidAction("Source object not found for exert cost".to_string())
            })?;
            if obj.zone != Zone::Battlefield {
                return Ok(payment_failed(
                    "Cannot exert: source is not on the battlefield",
                ));
            }
            let controller = obj.controller;
            state.add_transient_continuous_effect(
                source_id,
                controller,
                crate::types::ability::Duration::UntilNextStepOf {
                    step: crate::types::phase::Phase::Untap,
                    player: crate::types::ability::PlayerScope::Controller,
                },
                TargetFilter::SpecificObject { id: source_id },
                vec![
                    crate::types::ability::ContinuousModification::AddStaticMode {
                        mode: StaticMode::CantUntap,
                    },
                ],
                None,
            );
        }
        // CR 118.3 + CR 602.2b + CR 601.2h: Self-return costs such as
        // Recurring Nightmare and Maze's End are automatic once chosen;
        // non-self returns use the WaitingFor::PayCost detour before payment
        // begins.
        AbilityCost::ReturnToHand {
            count,
            filter: Some(TargetFilter::SelfRef),
            from_zone,
        } => {
            if *count != 1 {
                return Ok(payment_failed(
                    "self return-to-hand cost must return exactly one permanent",
                ));
            }
            let Some(obj) = state.objects.get(&source_id) else {
                return Ok(payment_failed("source not found for return-to-hand cost"));
            };
            let expected_zone = from_zone.unwrap_or(Zone::Battlefield);
            if obj.zone != expected_zone {
                return Ok(payment_failed(
                    "cannot return source to hand: source is not in the required zone",
                ));
            }
            let PaymentScope::Activation { .. } = scope
            else {
                unreachable!("self-referential return costs are not payable at resolution")
            };
            if let Some(outcome) =
                move_self_activation_cost(state, player, source_id, Zone::Hand, events)
            {
                return Ok(outcome);
            }
        }
        // Other cost types require interactive resolution and are intercepted
        // before reaching pay_ability_cost, or are not yet auto-payable.
        // CR 117.1 + CR 601.2b: `ExileWithAggregate` (Baron Helmut Zemo's Boast)
        // is paid by the `WaitingFor::PayCost { kind: ExileAggregate }` detour
        // before this resume runs; this arm is an idempotent no-op (mirrors the
        // `CollectEvidence`/`ExileMaterials` interactive-cost arms).
        AbilityCost::Exile { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        // CR 701.3d + CR 601.2h: A non-self unattach cost is paid by the
        // interactive `WaitingFor::PayCost { kind: UnattachFrom }` detour before
        // this resume runs, so this arm is an idempotent no-op (mirrors the
        // non-self Exile/Sacrifice interactive-cost arms).
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Behold { .. } => {}
        AbilityCost::Discard { .. } | AbilityCost::NinjutsuFamily { .. } => {
            // At Activation these shapes are intercepted by the interactive
            // WaitingFor detours before payment is invoked, so passing through
            // to `Paid` is sound. At Resolution there is no interceptor — but
            // none of these shapes is in `supported_at_resolution`, so the
            // structural guard at the top of this function has already refused
            // them with `Failed` (CR 118.3 / CR 601.2h) and this arm is only
            // ever reached at Activation scope.
        }
        // CR 118.12a: `OneOf` (disjunctive unless-cost) is intercepted at
        // `surface_unless_payment` and never reaches an auto-payment site.
        AbilityCost::OneOf { .. } => {
            return Ok(payment_failed(
                "OneOf cost is only valid as an unless-cost and must be \
                 resolved interactively via UnlessPaymentChooseCost",
            ));
        }
        // CR 702.24a: `PerCounter` is expanded into a concrete cost at the
        // unless-payment entry point (Task 6 wires resolution). It must never
        // reach an auto-payment site as-is — the multiplier has to be resolved
        // against the live game state first.
        AbilityCost::PerCounter { .. } => {
            return Ok(payment_failed(
                "PerCounter cost must be expanded against game state before \
                 reaching pay_ability_cost",
            ));
        }
    }
    Ok(PaymentOutcome::Paid)
}

/// CR 118.3 + CR 601.2h: The single payability authority. Returns whether
/// `payer` could pay `cost` right now in the active [`PaymentScope`].
///
/// Activation scope reproduces the aggregate (relocated from
/// `casting::can_pay_ability_cost_now`): the [`AbilityCost::is_payable`]
/// choice-eligibility/resource gate plus a clone-and-dry-run of
/// `pay_ability_cost_inner`, which is the affordability oracle for every
/// deterministic component (including the source's tapped state for `{T}`, and
/// the activation-window mana payment). A *bare* `Waterbend` cost skips the dry
/// run — `is_payable`'s Waterbend arm already routes through
/// `can_pay_cost_after_auto_tap`, and the dry run no-ops the Waterbend arm, so
/// it would be pure waste — but a `Composite` carrying both a Waterbend leg and
/// deterministic legs (e.g. Waterbend's own `{T}` companion cost) is dry-run for
/// those legs. The skip is gated on the bare `Waterbend` *shape*, never on the
/// folded `InteractiveMana` class: the fold returns `InteractiveMana` for any
/// Composite containing a Waterbend leg, so gating on the class would wrongly
/// suppress the dry run that checks the `{T}` leg's tapped-source state.
///
/// Resolution scope answers CR 118.12 payability per `AbilityCost` (relocated
/// from the deleted `effects::pay::can_pay_resolution_ability_cost`): a resource
/// match, plus — for the two counter-placement arms — CR 614.17b event
/// possibility. It is exhaustive with no wildcard so a new `AbilityCost` variant
/// forces a deliberate decision.
pub(crate) fn can_pay(
    state: &GameState,
    payer: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    scope: &PaymentScope,
) -> bool {
    match scope {
        PaymentScope::Activation { ability_index, .. } => {
            if !cost.is_payable_for_activation(state, payer, source_id, *ability_index) {
                return false;
            }
            // CR 118.12a: disjunctive activation costs resolve via
            // `ActivationCostOneOfChoice`, but each branch must still pass the
            // same activation affordability authority (is_payable + dry-run) as a
            // deterministic cost. `is_payable` alone does not catch tapped-source
            // `{T}` legs — shard-style `OneOf([Composite([Mana, Tap]), …])` would
            // otherwise surface as legal when every branch needs an untapped source.
            if let AbilityCost::OneOf { costs } = cost {
                return costs
                    .iter()
                    .any(|branch| can_pay(state, payer, source_id, branch, scope));
            }
            // CR 701.67a: A bare Waterbend cost has no deterministic component
            // to dry-run — its affordability is fully answered by `is_payable`'s
            // auto-tap check above. Gate on the bare `Waterbend` *shape*, not the
            // folded `InteractiveMana` class: the fold reports `InteractiveMana`
            // for any Composite that merely *contains* a Waterbend leg (e.g.
            // "Waterbend {3}, {T}"), and skipping the dry run there would leak
            // the `{T}` leg's tapped-source state — `is_payable`'s Tap arm is
            // unconditionally true. Every other shape (including such a
            // Composite) relies on the relocated A2 simulation guarantee.
            if matches!(cost, AbilityCost::Waterbend { .. }) {
                return true;
            }
            crate::game::perf_counters::record_state_clone_for_legality();
            let mut simulated = state.clone();
            // CR 601.2h: dry-run the authority on a throwaway clone. A `Failed`
            // outcome (insufficient mana, life, …) or an engine error (e.g. a
            // tapped source for a `{T}` cost) means the cost can't be paid.
            let dry_run_ok = matches!(
                pay_ability_cost_inner(
                    &mut simulated,
                    payer,
                    source_id,
                    cost,
                    &mut Vec::new(),
                    scope,
                    None,
                ),
                Ok(PaymentOutcome::Paid | PaymentOutcome::Paused { .. })
            );
            if !dry_run_ok {
                return false;
            }
            // CR 601.2g + CR 601.2f / CR 602.2b: an activated ability's activation
            // cost is the analog of a spell's mana cost, so the CR 601.2g/601.2h
            // ordering applies. The mana-leg detour in `handle_activate_ability`
            // now pays the mana leg FIRST (opening the CR 601.2g mana-ability
            // window on the INTACT board) and the non-mana battlefield-removal leg
            // LAST. The dry-run above therefore matches the live path exactly —
            // both pay mana on the intact board — so no supplemental
            // remove-then-recheck witness is needed; the former over-approximation
            // is now the correct verdict.
            true
        }
        PaymentScope::Resolution { ability, .. } => can_pay_resolution(state, payer, cost, ability),
    }
}

/// CR 118.12: The single source of truth for which `AbilityCost` shapes
/// `pay_ability_cost_inner` can actually pay at `PaymentScope::Resolution`. Both
/// the resolution affordability oracle (`can_pay_resolution`) and the
/// resolution-scope structural guard inside `pay_ability_cost_inner` derive from
/// this one predicate, so the two can never disagree and a future variant forces
/// a deliberate decision in exactly one place.
///
/// A shape outside this set has no resolution-time payment arm: at resolution
/// there is no interactive `WaitingFor` interceptor and no activation-window
/// mana detour, so executing such an arm would either silently report a no-op
/// cost as `Paid` (`Waterbend`, `ExileMaterials`, non-self `Sacrifice`, targeted
/// `RemoveCounter`) or perform an effect that was never meant to fire at
/// resolution (singleton `Tap`, self-ref `Sacrifice`/`Exile`, `Loyalty`,
/// `RemoveCounter { target: None }`, `Exert`, `Unattach`, arbitrary `EffectCost`,
/// source-card `Discard`). Both outcomes violate CR 118.3 / CR 601.2h, so the
/// guard refuses them with `Failed`.
pub(crate) fn supported_at_resolution(cost: &AbilityCost) -> bool {
    use crate::types::ability::{CardSelectionMode, DiscardSelfScope};
    match cost {
        AbilityCost::Mana { .. }
        | AbilityCost::ManaDynamic { .. }
        | AbilityCost::PayLife { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::Composite { .. }
        // CR 702.21a + CR 122.1: Ward's unless-pay always resolves at
        // resolution time (never activation), so this must be true here.
        | AbilityCost::GetPlayerCounters { .. }
        | AbilityCost::OneOf { .. } => true,
        // Only the chosen-from-hand discard has a resolution arm (the
        // `WaitingFor::DiscardChoice` / forced-choice fast path). The source-card
        // discard arm is an activation-cost shape with no resolution payment.
        AbilityCost::Discard {
            selection: CardSelectionMode::Chosen,
            self_scope: DiscardSelfScope::FromHand,
            ..
        } => true,
        // CR 406.6: Non-self exile cost at resolution time (e.g., The Mimeoplasm's
        // "exile two creature cards from graveyards"). The interactive choice is
        // surfaced via WaitingFor::PayCost before this resume runs.
        AbilityCost::Exile { filter, .. } if !matches!(filter, Some(TargetFilter::SelfRef)) => true,
        // CR 118.3: The shared effect-cost predicate admits only deterministic
        // payment effects that the authority resolves directly.
        AbilityCost::EffectCost { .. } if cost.supports_effect_cost_payment() => true,
        AbilityCost::Discard { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        // CR 117.1 + CR 601.2b: `ExileWithAggregate` is an activation-only cost
        // (paid by the interactive `PayCost { ExileAggregate }` detour); it has
        // no resolution-time payment path.
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Unattach
        // CR 701.3d: an unattach-from cost is paid at activation via the
        // interactive `PayCost { UnattachFrom }` detour, never at resolution.
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::PerCounter { .. }
        // CR 118.9: borrowed keyword cost — paid by the casting pipeline, never as
        // a resolution-time activation cost.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => false,
    }
}

/// CR 614.17b: would a mandatory can't-effect stop this player-counter gain?
fn player_counter_gain_is_prohibited(
    state: &GameState,
    payer: PlayerId,
    counter_kind: crate::types::player::PlayerCounterKind,
    count: u32,
) -> bool {
    super::effects::player_counter::preview_player_counter_addition(
        state,
        payer,
        payer,
        counter_kind,
        count,
    )
    .is_prohibited()
}

/// CR 614.17b: object-counter sibling, for the `EffectCost`/`PutCounter{SelfRef}` shape.
fn self_counter_placement_is_prohibited(
    state: &GameState,
    payer: PlayerId,
    source_id: ObjectId,
    counter_type: crate::types::counter::CounterType,
    count: u32,
) -> bool {
    state.objects.get(&source_id).is_some_and(|object| {
        super::effects::counters::preview_counter_addition(
            state,
            payer,
            ObjectIncarnationRef::from_object(object),
            counter_type,
            count,
        )
        .is_some_and(super::effects::counters::CounterAdditionPreview::is_prohibited)
    })
}

/// CR 118.5 + CR 702.24a: how many counters a resolution-time counter cost
/// places, from its resolved `QuantityExpr`.
///
/// CR 107.1b: "If a calculation that would determine the result of an effect
/// yields a negative number, zero is used instead, unless that effect doubles,
/// triples, or sets to a specific value a player's life total or the power
/// and/or toughness of a creature or creature card." A counter count is in
/// none of those exception classes, so a negative resolved quantity places
/// ZERO counters — never its magnitude, which would turn a cost that performs
/// no event into one that places counters the rules never asked for.
///
/// The resolver really can hand this function a negative value: `fold_compose`
/// evaluates `QuantityExpr::Offset` as an unfloored `inner + offset` and
/// `QuantityExpr::Multiply` with a signed factor, so any cost quantity whose
/// dynamic inner falls below its offset arrives here negative. `ClampMin` is
/// the *expression-level* opt-in to the same rule; a cost consumer cannot
/// assume its quantity was built with one.
///
/// `.max(0)` is the clamp every other resolved-quantity consumer in this file
/// uses (`Discard`, `PayLife`, `PayEnergy`, the dynamic generic mana cost).
///
/// Single authority on purpose. The choice-time predicate
/// (`resolution_cost_includes_impossible_event`) and the payment path
/// (`pay_ability_cost_inner`) must preview the SAME count: if they disagree, a
/// count the predicate reads as 0 short-circuits both previews to
/// `Applied { count: 0 }`, the pay branch is offered, and the payment then
/// refuses it — the exact offered-then-rejected defect CR 614.17b forbids.
fn counter_cost_count(resolved: i32) -> u32 {
    u32::try_from(resolved.max(0)).unwrap_or(0)
}

/// CR 614.17b: "If an event can't happen, a player can't choose to pay a cost
/// that includes that event." Answers exactly that, for a resolution-time cost.
///
/// NOT an affordability test. CR 118.3 (resources) is answered by `can_pay`; an
/// unaffordable cost may still legally be CHOSEN, because the unless-payment
/// window exists so the payer can produce the resources (CR 118.2: "the player
/// paying the cost has a chance to activate mana abilities"; CR 117.1d: mana
/// abilities may be activated "whenever a rule or effect asks for a mana
/// payment"). An impossible event admits no such window.
///
/// The verdict comes from the live replacement pipeline
/// (`replacement::pipeline_loop`'s CR 614.17c short-circuit via
/// `mandatory_prevention_applies`), reached through the read-only
/// `preview_*_counter_addition` primitives — never from a per-card test.
pub(crate) fn resolution_cost_includes_impossible_event(
    state: &GameState,
    payer: PlayerId,
    cost: &AbilityCost,
    ability: &ResolvedAbility,
) -> bool {
    use crate::types::ability::Effect;
    match cost {
        // CR 614.17b + CR 702.21a + CR 122.1: Ward's player-counter cost places the event.
        AbilityCost::GetPlayerCounters {
            counter_kind,
            count,
        } => player_counter_gain_is_prohibited(state, payer, *counter_kind, *count),
        AbilityCost::EffectCost { effect } => match effect.as_ref() {
            // CR 614.17b + CR 702.24a: cumulative upkeep's source-counter effect-cost shape.
            Effect::PutCounter {
                counter_type,
                count,
                target: TargetFilter::SelfRef,
            } => {
                let resolved = resolve_quantity_with_targets(state, count, ability);
                self_counter_placement_is_prohibited(
                    state,
                    payer,
                    ability.source_id,
                    counter_type.clone(),
                    counter_cost_count(resolved),
                )
            }
            // CR 118.1: producing mana performs no counter placement, so nothing to prohibit.
            Effect::Mana {
                produced: crate::types::ability::ManaProduction::Fixed { .. },
                ..
            } => false,
            // `supports_effect_cost_payment` refuses every other effect-cost shape upstream.
            // CR 614.17b: this arm swallows ANY widening of that predicate, not just a further
            // counter-placing one, so admitting any new effect-cost shape owes a matching arm
            // here in the same change — otherwise the new shape answers "no impossible event"
            // and silently loses this refusal. An `EffectCost { LoseLife }` shape is the nearest
            // example: CR 119.8 ("a cost that involves having that player pay life can't be
            // paid") is the direct analogue, and nothing else catches it, because
            // `can_pay_resolution` reaches that refusal only through `can_pay_life_cost` from its
            // `AbilityCost::PayLife` arm, which an effect-cost never matches.
            _ => false,
        },
        // Prohibition is the De Morgan dual of payability: `can_pay_resolution` answers
        // `Composite` with `.all()` and `OneOf` with `.any()`; this predicate answers them
        // with `.any()` and `.all()`. Copying one arm from the other is a rules bug in
        // whichever direction it is copied.
        // CR 614.17b: every component must be paid, so a cost that INCLUDES an impossible
        // component is itself unchoosable.
        AbilityCost::Composite { costs } => costs
            .iter()
            .any(|c| resolution_cost_includes_impossible_event(state, payer, c, ability)),
        // CR 614.17b + CR 118.12a: exactly one option is paid, so the cost is unchoosable
        // only if EVERY option includes an impossible event.
        // Only the offending index is refused at the pick; whether the WHOLE disjunctive
        // cost is unchoosable is this arm's question — `.all()`, not `.any()`.
        AbilityCost::OneOf { costs } => costs
            .iter()
            .all(|c| resolution_cost_includes_impossible_event(state, payer, c, ability)),
        // CR 702.24a: `expand_per_counter` resolves this into a concrete cost before the
        // predicate is consulted.
        AbilityCost::PerCounter { .. } => false,
        // No counter-placement event is performed while paying any remaining variant.
        AbilityCost::Mana { .. }
        | AbilityCost::ManaDynamic { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => false,
    }
}

/// CR 118.3 + CR 118.12: resolution-time payability. A player can't pay a cost
/// without the resources to pay it fully; used as the `Composite` pre-flight so
/// the resolver never commits a sub-cost before discovering a later sub-cost is
/// unpayable. Exhaustive over `AbilityCost`.
///
/// CR 614.17b: the counter-placement arms additionally refuse a cost whose
/// payment includes an event a mandatory can't-effect forbids — impossibility,
/// not affordability. `resolution_cost_includes_impossible_event` owns that
/// question; this function only folds its answer into those two leaves.
fn can_pay_resolution(
    state: &GameState,
    payer: PlayerId,
    cost: &AbilityCost,
    ability: &ResolvedAbility,
) -> bool {
    use crate::types::ability::{CardSelectionMode, DiscardSelfScope};
    match cost {
        AbilityCost::Mana { cost: mana_cost } => {
            can_pay_effect_mana_cost_after_auto_tap(state, payer, ability.source_id, mana_cost)
        }
        // CR 118.4 + CR 107.3c: Resolve the dynamic generic to a concrete
        // amount, then check mana payability. Dynamic-generic ability costs
        // appear primarily in unless-pay contexts; activation paths normally
        // pre-resolve to `Mana { cost }` upstream.
        AbilityCost::ManaDynamic { quantity } => {
            let amount = resolve_quantity_with_targets(state, quantity, ability);
            let mana = crate::types::mana::ManaCost::generic(amount.max(0) as u32);
            can_pay_effect_mana_cost_after_auto_tap(state, payer, ability.source_id, &mana)
        }
        // CR 119.4: Pay life requires the player's life total to be at least the
        // payment amount (and no CantLoseLife lock).
        AbilityCost::PayLife { amount } => {
            let amount = resolve_quantity_with_targets(state, amount, ability);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            can_pay_life_cost(state, payer, amount)
        }
        // CR 107.14: Pay {E} requires that many energy counters.
        AbilityCost::PayEnergy { amount } => {
            let amount =
                u32::try_from(resolve_quantity_with_targets(state, amount, ability).max(0))
                    .unwrap_or(0);
            state
                .players
                .iter()
                .find(|p| p.id == payer)
                .is_some_and(|p| p.energy >= amount)
        }
        // CR 702.179f: Pay speed requires that much current speed.
        AbilityCost::PaySpeed { amount } => {
            let amount = resolve_quantity_with_targets(state, amount, ability);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            effective_speed(state, payer) >= amount
        }
        // CR 701.9: A chosen-from-hand discard requires `count` eligible cards
        // in the payer's hand (matching `filter` if present). This is the only
        // discard shape with a resolution payment arm (`supported_at_resolution`);
        // the source-card discard is an activation-cost shape and falls to the
        // unsupported list below. `random` does not affect affordability — random
        // discard still needs the card count — so it is not constrained here.
        AbilityCost::Discard {
            count,
            filter,
            selection: CardSelectionMode::Chosen,
            self_scope: DiscardSelfScope::FromHand,
        } => {
            let count = u32::try_from(resolve_quantity_with_targets(state, count, ability).max(0))
                .unwrap_or(0) as usize;
            let eligible =
                find_eligible_discard_targets(state, payer, ability.source_id, filter.as_ref());
            eligible.len() >= count
        }
        // CR 406.6: Non-self exile cost at resolution time (e.g., The Mimeoplasm's
        // "exile two creature cards from graveyards"). The interactive choice is
        // surfaced via WaitingFor::EffectZoneChoice.
        AbilityCost::Exile {
            count,
            zone,
            filter,
            ..
        } if !matches!(filter, Some(TargetFilter::SelfRef)) => {
            let count = *count as usize;
            let effective_zone = zone.unwrap_or(Zone::Graveyard);
            let eligible = find_eligible_exile_targets(
                state,
                payer,
                ability.source_id,
                effective_zone,
                filter.as_ref(),
            );
            eligible.len() >= count
        }
        // CR 118.12 + CR 701.26a: Resolution-time optional tap-creatures costs
        // are payable when enough currently untapped matching creatures can be
        // selected. The concrete selection is surfaced through WaitingFor::PayCost.
        AbilityCost::TapCreatures {
            requirement,
            filter,
        } => {
            let eligible = find_eligible_tap_creatures_targets(state, payer, ability, filter);
            match requirement {
                crate::types::ability::TapCreaturesRequirement::Count { count } => {
                    // CR 107.3a: route the floor through the single bounds
                    // authority so the `u32::MAX` X-sentinel is not compared as a
                    // literal minimum (which is unsatisfiable for any real board).
                    // A fixed (non-X) count degrades to `(count, count)`, leaving
                    // every existing card's payability verdict unchanged.
                    let (min_count, _) =
                        super::casting::sacrifice_cost_bounds(*count, eligible.len());
                    eligible.len() >= min_count
                }
                crate::types::ability::TapCreaturesRequirement::Aggregate {
                    stat,
                    comparator,
                    value,
                } => {
                    let aggregate = crate::types::ability::TapCreaturesAggregate {
                        stat: *stat,
                        comparator: *comparator,
                        value: *value,
                    };
                    let total_positive_power =
                        super::casting_costs::tap_creatures_total_power(state, &eligible);
                    aggregate.satisfied_by(total_positive_power)
                }
            }
        }
        // CR 117 + CR 118.3: Composite is payable iff every sub-cost is payable.
        AbilityCost::Composite { costs } => costs
            .iter()
            .all(|cost| can_pay_resolution(state, payer, cost, ability)),
        // CR 118.12a: Disjunctive — payable iff any sub-cost is payable. The
        // choice is made interactively via `UnlessPaymentChooseCost`; the
        // unconditional pre-flight check only needs at least one branch.
        AbilityCost::OneOf { costs } => costs
            .iter()
            .any(|cost| can_pay_resolution(state, payer, cost, ability)),
        // CR 118.3 + CR 104.3d: no RESOURCE limit on giving yourself counters — the
        // ten-or-more poison loss condition is a state-based action, not a
        // payment-time affordability gate.
        // CR 614.17b: the one bar is a mandatory can't-effect on the counter
        // placement, which `resolution_cost_includes_impossible_event` answers.
        AbilityCost::GetPlayerCounters {
            counter_kind,
            count,
        } => !player_counter_gain_is_prohibited(state, payer, *counter_kind, *count),
        // CR 118.3: every deterministic effect-cost payment admitted by the shared
        // support predicate has the resources to be paid; its resolver handles any
        // replacement effects while paying it.
        // CR 614.17b: it is offerable only if paying it does not require an event a
        // mandatory can't-effect forbids.
        AbilityCost::EffectCost { .. } if cost.supports_effect_cost_payment() => {
            !resolution_cost_includes_impossible_event(state, payer, cost, ability)
        }
        // Variants below have no resolution-time payment arm
        // (`supported_at_resolution` is the shared membership authority).
        // Refusing here is the conservative affordability answer (treat as
        // "can't pay" → `cost_payment_failed_flag` → the effect's didn't-pay
        // branch, per CR 118.12). The structural guard at the top of
        // `pay_ability_cost_inner` backs this up: a shape that slips past this
        // pre-gate returns `Failed`, never a silent `Paid` and never an
        // unintended execution.
        //
        // The source-card / non-chosen `Discard` shapes land here (only the
        // chosen-from-hand discard above has a resolution arm).
        //
        // CR 702.24a: `PerCounter` is expanded into a concrete cost at the
        // unless-payment entry point; the resolved base is what gets
        // payability-checked. The wrapper itself is not a direct resolution-time
        // cost, so refusing here keeps the effect proceeding pre-expansion.
        AbilityCost::Discard { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Unattach
        // CR 701.3d: an unattach-from cost is an activation cost, not a
        // resolution-time cost; refuse here like the unit `Unattach`.
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        // CR 117.1: `ExileWithAggregate` is paid at activation, not resolution.
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::PerCounter { .. }
        // CR 118.9: borrowed keyword cost — paid by the casting pipeline, not a
        // direct resolution-time cost.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{
        BeholdCostAction, CardSelectionMode, CostObjectCount, DiscardSelfScope, Effect,
        NinjutsuVariant, QuantityExpr, SacrificeCost, TapCreaturesRequirement,
        TapCreaturesSelectionMode,
    };
    use crate::types::counter::{CounterMatch, CounterType};
    use crate::types::mana::ManaCost;

    const P0: PlayerId = PlayerId(0);

    /// Build one representative value for EVERY `AbilityCost` variant via an
    /// exhaustive `match` over a tag enum. The `match` has no wildcard, so a new
    /// `AbilityCost` variant forces a compile error here — the lockstep gate
    /// (plan §5 / risk R5): a new payable resource must be given a
    /// `supported_at_resolution` answer and payable through `pay_cost` before
    /// this test compiles.
    fn sample_for(tag: &AbilityCost) -> AbilityCost {
        let life = QuantityExpr::Fixed { value: 1 };
        match tag {
            AbilityCost::Mana { .. } => AbilityCost::Mana {
                cost: ManaCost::NoCost,
            },
            AbilityCost::ManaDynamic { .. } => AbilityCost::ManaDynamic {
                quantity: life.clone(),
            },
            AbilityCost::Tap => AbilityCost::Tap,
            AbilityCost::Untap => AbilityCost::Untap,
            AbilityCost::Loyalty { .. } => AbilityCost::Loyalty { amount: 1 },
            AbilityCost::Sacrifice(_) => {
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1))
            }
            AbilityCost::PayLife { .. } => AbilityCost::PayLife {
                amount: life.clone(),
            },
            AbilityCost::Discard { .. } => AbilityCost::Discard {
                count: life.clone(),
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand,
            },
            AbilityCost::Exile { .. } => AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::SelfRef),
            },
            AbilityCost::ExileMaterials { .. } => AbilityCost::ExileMaterials {
                materials: TargetFilter::Any,
                count: CostObjectCount::default(),
            },
            AbilityCost::CollectEvidence { .. } => AbilityCost::CollectEvidence { amount: 1 },
            AbilityCost::ExileWithAggregate { .. } => AbilityCost::ExileWithAggregate {
                filter: TargetFilter::Any,
                function: crate::types::ability::AggregateFunction::Sum,
                property: crate::types::ability::ObjectProperty::ManaSymbolCount(
                    crate::types::mana::ManaColor::Black,
                ),
                comparator: crate::types::ability::Comparator::GE,
                value: 1,
                zone: Zone::Graveyard,
            },
            AbilityCost::TapCreatures { .. } => AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::count(1),
                filter: TargetFilter::Any,
            },
            AbilityCost::RemoveCounter { .. } => AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                target: None,
                selection: Default::default(),
            },
            AbilityCost::PayEnergy { .. } => AbilityCost::PayEnergy {
                amount: life.clone(),
            },
            AbilityCost::PaySpeed { .. } => AbilityCost::PaySpeed {
                amount: life.clone(),
            },
            AbilityCost::ReturnToHand { .. } => AbilityCost::ReturnToHand {
                count: 1,
                filter: None,
                from_zone: None,
            },
            AbilityCost::Unattach => AbilityCost::Unattach,
            AbilityCost::UnattachFrom { .. } => AbilityCost::UnattachFrom {
                filter: TargetFilter::Any,
                count: 1,
            },
            AbilityCost::Mill { .. } => AbilityCost::Mill { count: 1 },
            AbilityCost::Exert => AbilityCost::Exert,
            AbilityCost::Blight { .. } => AbilityCost::Blight { count: 1 },
            AbilityCost::Reveal { .. } => AbilityCost::Reveal {
                count: 1,
                filter: None,
            },
            AbilityCost::Behold { .. } => AbilityCost::Behold {
                count: 1,
                filter: TargetFilter::Any,
                action: BeholdCostAction::ChooseOrReveal,
                type_choice: None,
            },
            AbilityCost::Composite { .. } => AbilityCost::Composite {
                costs: vec![AbilityCost::Tap, AbilityCost::PayLife { amount: life }],
            },
            AbilityCost::OneOf { .. } => AbilityCost::OneOf {
                costs: vec![AbilityCost::Tap],
            },
            AbilityCost::Waterbend { .. } => AbilityCost::Waterbend {
                cost: ManaCost::generic(1),
            },
            AbilityCost::NinjutsuFamily { .. } => AbilityCost::NinjutsuFamily {
                variant: NinjutsuVariant::Ninjutsu,
                mana_cost: ManaCost::generic(1),
            },
            AbilityCost::EffectCost { .. } => AbilityCost::EffectCost {
                effect: Box::new(Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }),
            },
            AbilityCost::PerCounter { .. } => AbilityCost::PerCounter {
                counter: CounterType::Age,
                target: TargetFilter::SelfRef,
                base: Box::new(AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                }),
            },
            AbilityCost::KeywordCostOfCastSpell { .. } => AbilityCost::KeywordCostOfCastSpell {
                keyword: crate::types::keywords::KeywordKind::Suspend,
            },
            AbilityCost::GetPlayerCounters { .. } => AbilityCost::GetPlayerCounters {
                counter_kind: crate::types::player::PlayerCounterKind::Poison,
                count: 1,
            },
            AbilityCost::Unimplemented { .. } => AbilityCost::Unimplemented {
                description: "test".to_string(),
            },
        }
    }

    /// One zero-data instance of every variant — `sample_for` is exhaustive, so
    /// this list is guaranteed to cover the full enum.
    fn all_variants() -> Vec<AbilityCost> {
        // The tag values only select the `match` arm; their inner data is ignored.
        let tags = [
            AbilityCost::Mana {
                cost: ManaCost::NoCost,
            },
            AbilityCost::ManaDynamic {
                quantity: QuantityExpr::Fixed { value: 0 },
            },
            AbilityCost::Tap,
            AbilityCost::Untap,
            AbilityCost::Loyalty { amount: 0 },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 0 },
            },
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 0 },
                filter: None,
                selection: CardSelectionMode::Chosen,
                self_scope: DiscardSelfScope::FromHand,
            },
            AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: None,
            },
            AbilityCost::ExileMaterials {
                materials: TargetFilter::Any,
                count: CostObjectCount::default(),
            },
            AbilityCost::CollectEvidence { amount: 0 },
            AbilityCost::ExileWithAggregate {
                filter: TargetFilter::Any,
                function: crate::types::ability::AggregateFunction::Sum,
                property: crate::types::ability::ObjectProperty::ManaSymbolCount(
                    crate::types::mana::ManaColor::Black,
                ),
                comparator: crate::types::ability::Comparator::GE,
                value: 0,
                zone: Zone::Graveyard,
            },
            AbilityCost::TapCreatures {
                requirement: TapCreaturesRequirement::count(0),
                filter: TargetFilter::Any,
            },
            AbilityCost::RemoveCounter {
                count: 0,
                counter_type: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                target: None,
                selection: Default::default(),
            },
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 0 },
            },
            AbilityCost::PaySpeed {
                amount: QuantityExpr::Fixed { value: 0 },
            },
            AbilityCost::ReturnToHand {
                count: 0,
                filter: None,
                from_zone: None,
            },
            AbilityCost::Unattach,
            AbilityCost::UnattachFrom {
                filter: TargetFilter::Any,
                count: 1,
            },
            AbilityCost::Mill { count: 0 },
            AbilityCost::Exert,
            AbilityCost::Blight { count: 0 },
            AbilityCost::Reveal {
                count: 0,
                filter: None,
            },
            AbilityCost::Behold {
                count: 0,
                filter: TargetFilter::Any,
                action: BeholdCostAction::ChooseOrReveal,
                type_choice: None,
            },
            AbilityCost::Composite { costs: vec![] },
            AbilityCost::OneOf { costs: vec![] },
            AbilityCost::Waterbend {
                cost: ManaCost::NoCost,
            },
            AbilityCost::NinjutsuFamily {
                variant: NinjutsuVariant::Ninjutsu,
                mana_cost: ManaCost::NoCost,
            },
            AbilityCost::EffectCost {
                effect: Box::new(Effect::PutCounter {
                    counter_type: CounterType::Plus1Plus1,
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::SelfRef,
                }),
            },
            AbilityCost::PerCounter {
                counter: CounterType::Age,
                target: TargetFilter::SelfRef,
                base: Box::new(AbilityCost::Tap),
            },
            AbilityCost::KeywordCostOfCastSpell {
                keyword: crate::types::keywords::KeywordKind::Suspend,
            },
            AbilityCost::GetPlayerCounters {
                counter_kind: crate::types::player::PlayerCounterKind::Poison,
                count: 1,
            },
            AbilityCost::Unimplemented {
                description: String::new(),
            },
        ];
        tags.iter().map(sample_for).collect()
    }

    /// Plan §5 lockstep: every `AbilityCost` variant has a resolution-support
    /// answer from `supported_at_resolution` (the exhaustive `match` makes a
    /// missing arm a compile error), so a new variant is forced through a
    /// deliberate "is this payable at resolution?" decision — the single
    /// authority shared by `can_pay_resolution` and the `pay_ability_cost_inner`
    /// structural guard.
    #[test]
    fn every_ability_cost_variant_has_resolution_support_answer() {
        for cost in all_variants() {
            // `supported_at_resolution` is exhaustive; calling it on every
            // variant proves the membership predicate is total.
            let _supported = supported_at_resolution(&cost);
        }
    }

    /// Plan §5 lockstep (risk R5): for the deterministic costs that are payable
    /// in a fixture, `can_pay(Activation) == true` implies `pay_cost` does not
    /// return `Failed`. This keeps the affordability authority and the payment
    /// authority in agreement so AI legality never desyncs from the submit path.
    #[test]
    fn can_pay_implies_pay_cost_not_failed_for_payable_deterministic_costs() {
        let mut scenario = GameScenario::new();
        // A creature with loyalty + counters so loyalty/remove-counter/exert pay.
        let src = scenario.add_creature(P0, "Test Source", 2, 2).id();
        {
            let obj = scenario.state.objects.get_mut(&src).unwrap();
            obj.loyalty = Some(3);
            obj.counters
                .insert(CounterType::Generic("charge".to_string()), 2);
        }
        scenario.state.players[P0.0 as usize].life = 20;
        scenario.state.players[P0.0 as usize].energy = 5;

        let payable_samples = [
            AbilityCost::Tap,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            AbilityCost::Loyalty { amount: 1 },
            AbilityCost::RemoveCounter {
                count: 1,
                counter_type: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                target: None,
                selection: Default::default(),
            },
            AbilityCost::Exert,
        ];

        for cost in payable_samples {
            let excluded = ability_mana_payment_excluded_sources(&cost, src);
            let scope = PaymentScope::Activation {
                excluded_sources: &excluded,
                ability_index: Some(0),
            };
            assert!(
                can_pay(&scenario.state, P0, src, &cost, &scope),
                "expected can_pay == true for {cost:?}"
            );
            // Dry-run on a clone (each iteration independent): can_pay == true
            // must mean the authority does not report Failed.
            let mut sim = scenario.state.clone();
            let outcome =
                pay_ability_cost_inner(&mut sim, P0, src, &cost, &mut Vec::new(), &scope, None)
                    .unwrap();
            assert!(
                !matches!(outcome, PaymentOutcome::Failed { .. }),
                "can_pay==true but pay_cost returned Failed for {cost:?}"
            );
        }
    }

    #[test]
    fn untap_cost_untaps_tapped_source_and_rejects_when_already_untapped() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Untap Source", 2, 2).id();
        scenario.state.objects.get_mut(&src).unwrap().tapped = true;

        let cost = AbilityCost::Untap;
        let excluded = ability_mana_payment_excluded_sources(&cost, src);
        let scope = PaymentScope::Activation {
            excluded_sources: &excluded,
            ability_index: Some(0),
        };
        let mut events = Vec::new();
        let outcome = pay_ability_cost_inner(
            &mut scenario.state,
            P0,
            src,
            &cost,
            &mut events,
            &scope,
            None,
        )
        .unwrap();
        assert!(matches!(outcome, PaymentOutcome::Paid));
        assert!(!scenario.state.objects.get(&src).unwrap().tapped);
        assert!(events.iter().any(
            |event| matches!(event, GameEvent::PermanentUntapped { object_id } if *object_id == src)
        ));

        // CR 107.6: a permanent that's already untapped can't be untapped again
        // to pay the cost — the second payment must FAIL, not silently no-op.
        let result = pay_ability_cost_inner(
            &mut scenario.state,
            P0,
            src,
            &cost,
            &mut events,
            &scope,
            None,
        );
        assert!(
            result.is_err(),
            "paying {{Q}} on an already-untapped permanent must be rejected (CR 107.6)"
        );
    }

    #[test]
    fn self_return_to_hand_cost_honors_explicit_from_zone() {
        let mut scenario = GameScenario::new();
        let src = scenario
            .add_creature(P0, "Self Returning Source", 2, 2)
            .id();
        let graveyard_cost = AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: Some(Zone::Graveyard),
        };

        let rejected = pay_ability_cost_for_activation(
            &mut scenario.state,
            P0,
            src,
            &graveyard_cost,
            Some(0),
            &mut Vec::new(),
        );
        assert!(matches!(rejected, Err(EngineError::ActionNotAllowed(_))));
        assert_eq!(scenario.state.objects[&src].zone, Zone::Battlefield);

        let battlefield_cost = AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: Some(Zone::Battlefield),
        };
        pay_ability_cost_for_activation(
            &mut scenario.state,
            P0,
            src,
            &battlefield_cost,
            Some(0),
            &mut Vec::new(),
        )
        .expect("battlefield self-return cost should be payable");
        assert_eq!(scenario.state.objects[&src].zone, Zone::Hand);
    }

    /// Activation-scope `can_pay` against `state` for `source`.
    fn can_pay_activation(state: &GameState, source: ObjectId, cost: &AbilityCost) -> bool {
        let excluded = ability_mana_payment_excluded_sources(cost, source);
        can_pay(
            state,
            P0,
            source,
            cost,
            &PaymentScope::Activation {
                excluded_sources: &excluded,
                ability_index: Some(0),
            },
        )
    }

    /// Phase 5 discriminating test for the DELETED non-self-Sacrifice A2
    /// pre-check (`find_non_self_sacrifice_cost`): `can_pay` alone (is_payable +
    /// dry-run, no bespoke walk) must still reject "Sacrifice a creature" when no
    /// eligible permanent exists, and accept it once one does. CR 601.2b /
    /// CR 118.3.
    #[test]
    fn can_pay_rejects_non_self_sacrifice_without_eligible_permanent() {
        use crate::types::ability::TypedFilter;
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Altar", 0, 1).id();
        // The source is a 0/1 creature; "another creature" filter excludes it.
        let cost = AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(
                TypedFilter::creature()
                    .properties(vec![crate::types::ability::FilterProp::Another]),
            ),
            1,
        ));
        assert!(
            !can_pay_activation(&scenario.state, src, &cost),
            "no other creature to sacrifice → unpayable"
        );
        scenario.add_creature(P0, "Fodder", 1, 1);
        assert!(
            can_pay_activation(&scenario.state, src, &cost),
            "another creature now exists → payable"
        );
    }

    /// Phase 5 discriminating test for the DELETED PayLife A2 pre-check
    /// (`find_pay_life_cost`): `can_pay` alone must reject a life cost exceeding
    /// the player's life total (CR 118.3) and accept one within it (CR 119.4).
    #[test]
    fn can_pay_rejects_unaffordable_pay_life() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Source", 0, 1).id();
        scenario.state.players[P0.0 as usize].life = 3;
        let too_much = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 4 },
        };
        let affordable = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 3 },
        };
        assert!(!can_pay_activation(&scenario.state, src, &too_much));
        assert!(can_pay_activation(&scenario.state, src, &affordable));
    }

    /// Phase 5 discriminating test for the DELETED TapCreatures A2 pre-check
    /// (`find_tap_creatures_cost`): `can_pay` alone must reject "tap N creatures"
    /// when fewer than N untapped controlled creatures exist (CR 601.2b) and
    /// accept it once enough do.
    #[test]
    fn can_pay_rejects_tap_creatures_without_enough_untapped() {
        use crate::types::ability::TypedFilter;
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Lord", 2, 2).id();
        let cost = AbilityCost::TapCreatures {
            requirement: TapCreaturesRequirement::count(2),
            filter: TargetFilter::Typed(TypedFilter::creature()),
        };
        // Only the source creature is present (1 < 2).
        assert!(
            !can_pay_activation(&scenario.state, src, &cost),
            "only 1 untapped creature < 2 → unpayable"
        );
        scenario.add_creature(P0, "Helper", 1, 1);
        assert!(
            can_pay_activation(&scenario.state, src, &cost),
            "2 untapped creatures → payable"
        );
    }

    /// HIGH-1 regression (CR 118.12a + CR 118.3): shard-style
    /// `OneOf([Composite([Mana, Tap]), …])` must route each branch through the
    /// activation dry-run, not `is_payable` alone. The Tap arm is unconditionally
    /// true in `is_payable`, so a tapped source must be `can_pay == false`.
    #[test]
    fn one_of_tap_branches_respects_tapped_source() {
        use crate::parser::oracle_cost::parse_oracle_cost;
        use crate::types::mana::{ManaType, ManaUnit};

        let mut scenario = GameScenario::new();
        let src = scenario
            .add_creature(P0, "Granite Shard", 0, 0)
            .as_artifact()
            .id();
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Colorless, src, false, vec![]),
                ManaUnit::new(ManaType::Colorless, src, false, vec![]),
                ManaUnit::new(ManaType::Colorless, src, false, vec![]),
                ManaUnit::new(ManaType::Red, src, false, vec![]),
            ],
        );
        let cost = parse_oracle_cost("{3}, {T} or {R}, {T}");

        assert!(
            can_pay_activation(&scenario.state, src, &cost),
            "untapped source with mana → OneOf tap branches payable"
        );
        scenario.state.objects.get_mut(&src).unwrap().tapped = true;
        assert!(
            !can_pay_activation(&scenario.state, src, &cost),
            "tapped source → OneOf tap branches must be unpayable"
        );
    }
    /// HIGH-1 regression (CR 701.67a + CR 118.3): a `Composite[Waterbend, {T}]`
    /// (Avatar TLA "Waterbend [cost], {T}: …") must NOT skip the dry run just
    /// because the `payment_class` fold reports `InteractiveMana` for the
    /// Waterbend leg. The `{T}` leg's tapped-source state is only checked by the
    /// dry run (`is_payable`'s Tap arm is unconditionally true), so a TAPPED
    /// source must be `can_pay == false` and an UNTAPPED source `true`. Before
    /// the bare-shape gate fix this asserted `true` for the tapped source
    /// (leaking an unactivatable ability into legal actions).
    #[test]
    fn composite_waterbend_tap_respects_tapped_source() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Waterbender", 1, 1).id();
        // NoCost Waterbend leg isolates the {T} leg as the only differentiator
        // (the mana auto-tap check is trivially satisfied).
        let cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Waterbend {
                    cost: ManaCost::NoCost,
                },
                AbilityCost::Tap,
            ],
        };
        // Untapped source: the {T} leg can be paid → payable.
        assert!(
            can_pay_activation(&scenario.state, src, &cost),
            "untapped source → Composite[Waterbend, {{T}}] payable"
        );
        // Tap the source: the {T} leg can no longer be paid → unpayable.
        scenario.state.objects.get_mut(&src).unwrap().tapped = true;
        assert!(
            !can_pay_activation(&scenario.state, src, &cost),
            "tapped source → Composite[Waterbend, {{T}}] must be unpayable"
        );
    }

    // -----------------------------------------------------------------------
    // Composite "{N}, <battlefield-removal>" mana-first affordability (CR 601.2g
    // / CR 601.2h ordering: the mana-leg detour pays mana FIRST on the intact
    // board, the removal LAST). The helpers below build the Claws-of-Gix /
    // Mox-Opal-Metalcraft minimal board.
    // -----------------------------------------------------------------------

    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ActivationRestriction, Comparator, ContinuousModification,
        ControllerRef, ParsedCondition, QuantityRef, StaticCondition, StaticDefinition, TypeFilter,
        TypedFilter,
    };
    use crate::types::statics::StaticMode;
    use crate::types::ManaProduction;

    /// `QuantityExpr::Ref(ObjectCount(artifacts you control))`.
    fn artifacts_you_control() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                ),
            },
        }
    }

    /// A `{T}: Add {1}` mana ability gated by Metalcraft-style *live-eval*
    /// "control 3+ artifacts" via an `ActivationRestriction::RequiresCondition`
    /// (`ParsedCondition::QuantityComparison`). This is the Mox-Opal model: the
    /// gate reads the live battlefield, NOT the layer system.
    fn metalcraft_mox(scenario: &mut GameScenario) -> ObjectId {
        let mut def = AbilityDefinition::new(
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
        .cost(AbilityCost::Tap);
        def.activation_restrictions
            .push(ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: artifacts_you_control(),
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }),
            });
        let mut b = scenario.add_creature(P0, "Mox Opal", 0, 0);
        b.as_artifact();
        b.with_ability_definition(def);
        b.id()
    }

    /// Add a plain artifact (sacrifice fodder / artifact-count filler) with no
    /// mana ability.
    fn plain_artifact(scenario: &mut GameScenario, name: &str) -> ObjectId {
        let mut b = scenario.add_creature(P0, name, 0, 1);
        b.as_artifact();
        b.id()
    }

    /// The Claws-of-Gix cost: `{1}, Sacrifice a permanent`.
    fn claws_cost() -> AbilityCost {
        AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::permanent()),
                    1,
                )),
            ],
        }
    }

    /// V1 (mana-first → payable): Metalcraft-only board — exactly 3 artifacts
    /// including Mox Opal (the only {1} source) + Claws. CR 601.2g / CR 601.2h:
    /// the mana-leg detour pays {1} FIRST while all 3 artifacts are intact
    /// (Metalcraft holds → Mox produces {1}); the sacrifice is paid LAST. So the
    /// composite IS payable even though sacrificing afterwards drops below
    /// Metalcraft — the mana was already in the pool. Reverting the mana-first
    /// detour restores the sacrifice-first ordering, which dead-ends here.
    #[test]
    fn claws_metalcraft_only_board_is_payable_mana_first() {
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Artifact A");
        let claws = plain_artifact(&mut scenario, "Claws of Gix");
        // 3 artifacts on board; Mox makes {1} while Metalcraft holds, and the
        // mana-first window pays {1} before the sacrifice shrinks the board.
        assert!(
            can_pay_activation(&scenario.state, claws, &claws_cost()),
            "mana paid first on the intact 3-artifact board → payable"
        );
    }

    /// V2 (no over-reject): Claws plus an untapped basic land (a non-conditional
    /// `{1}` source) plus the Metalcraft Mox plus fodder. A sacrifice can leave
    /// the `{1}` payable from the land, so `can_pay` is `true`. The full live
    /// activation and life+1 assertion is covered through the real pipeline by
    /// the phase-ai `choose_action` scenario
    /// `scenario_claws_of_gix_witness_board_does_not_dead_end`; this layer asserts
    /// only the affordability oracle's verdict.
    #[test]
    fn claws_with_unconditional_land_is_payable() {
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Artifact A");
        // A Forest produces one mana usable for the generic {1} — a
        // non-conditional source that survives any sacrifice.
        scenario.add_basic_land(P0, crate::types::mana::ManaColor::Green);
        let claws = plain_artifact(&mut scenario, "Claws of Gix");
        assert!(
            can_pay_activation(&scenario.state, claws, &claws_cost()),
            "land provides a non-conditional {{1}} → payable regardless of sacrifice"
        );
    }

    /// V5 (mana-first → payable): even though EVERY eligible sacrifice would break
    /// the sole {1} producer, CR 601.2g pays {1} from the Mox on the intact
    /// 3-artifact board BEFORE the sacrifice, so the composite is payable. The
    /// post-sacrifice board state is irrelevant once the mana is in the pool.
    #[test]
    fn claws_every_sacrifice_breaks_producer_is_payable_mana_first() {
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Filler 1");
        let claws = plain_artifact(&mut scenario, "Claws of Gix");
        assert!(
            can_pay_activation(&scenario.state, claws, &claws_cost()),
            "{{1}} paid from the Mox before the sacrifice → payable"
        );
    }

    /// V6 (payable, redundant mana): with FOUR artifacts (Mox + 3 fodder), any
    /// single sacrifice leaves 3 → Metalcraft still holds → `{1}` payable from
    /// the Mox itself → `true`.
    #[test]
    fn claws_redundant_artifact_count_keeps_metalcraft_payable() {
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Filler 1");
        plain_artifact(&mut scenario, "Filler 2");
        let claws = plain_artifact(&mut scenario, "Claws of Gix");
        // 4 artifacts: sacrificing any one leaves 3 → Metalcraft holds.
        assert!(
            can_pay_activation(&scenario.state, claws, &claws_cost()),
            "4 artifacts → a witness leaves Metalcraft on → payable"
        );
    }

    /// V7 (payable, disjoint producer): dedicated NON-artifact fodder distinct
    /// from the producer. Sacrificing the fodder doesn't change artifact count,
    /// so Metalcraft holds. With 3 artifacts (Mox + 2 fillers) + a creature
    /// fodder, sacrificing the creature keeps 3 artifacts → payable.
    #[test]
    fn claws_disjoint_fodder_preserves_producer() {
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Filler 1");
        plain_artifact(&mut scenario, "Filler 2");
        // Non-artifact creature fodder — sacrificing it leaves artifact count = 3.
        scenario.add_creature(P0, "Bear", 2, 2);
        let claws = plain_artifact(&mut scenario, "Claws of Gix");
        assert!(
            can_pay_activation(&scenario.state, claws, &claws_cost()),
            "sacrificing non-artifact fodder preserves Metalcraft → payable"
        );
    }

    /// V10 (count>1 still payable): a `Composite[{1}, Sacrifice TWO permanents]`
    /// is payable on a board where sacrificing would break the conditional mana
    /// source, because CR 601.2g pays {1} on the intact board before either
    /// sacrifice. The mana-first detour is count-agnostic — it pays the mana leg
    /// regardless of how many permanents the removal leg sacrifices.
    #[test]
    fn claws_sacrifice_two_count_gt_one_not_rejected() {
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Filler 1");
        let claws = plain_artifact(&mut scenario, "Claws of Gix");
        let cost_two = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::permanent()),
                    2,
                )),
            ],
        };
        assert!(
            can_pay_activation(&scenario.state, claws, &cost_two),
            "count > 1 sacrifice composite falls through to today's over-approximation (true)"
        );
    }

    /// B1 (layer-granted mana source, mana-first → payable): the Mox grants its
    /// own `{T}: Add {1}` mana ability via a continuous `StaticDefinition`
    /// (`ContinuousModification::GrantAbility`) gated by
    /// `StaticCondition::QuantityComparison(artifacts >= 3)`. Unlike
    /// `metalcraft_mox` (live-eval `activation_restrictions`), the granted ability
    /// only appears in `obj.abilities` after `flush_layers` re-derives layer 6.
    ///
    /// CR 601.2g / CR 601.2h: the mana-leg detour pays {1} from the granted
    /// ability while all 3 artifacts are intact (the grant is live), and the
    /// sacrifice is paid LAST. So the composite is payable even though sacrificing
    /// afterwards would drop below Metalcraft and the layer reflush would remove
    /// the grant — that post-removal board is irrelevant once the mana is paid.
    /// Reverting the mana-first detour restores sacrifice-first ordering, which
    /// dead-ends here (the granted {1} is gone before mana payment).
    #[test]
    fn claws_layer_granted_mana_requires_layer_reflush() {
        let mut scenario = GameScenario::new();
        // The mana ability the Mox GRANTS to itself while controlling 3+ artifacts.
        let granted = AbilityDefinition::new(
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
        .cost(AbilityCost::Tap);
        let mut grant_static = StaticDefinition::new(StaticMode::Continuous);
        grant_static.affected = Some(TargetFilter::SelfRef);
        grant_static.modifications = vec![ContinuousModification::GrantAbility {
            definition: Box::new(granted),
        }];
        grant_static.condition = Some(StaticCondition::QuantityComparison {
            lhs: artifacts_you_control(),
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: 3 },
        });

        let mox = {
            let mut b = scenario.add_creature(P0, "Layer Mox", 0, 0);
            b.as_artifact();
            b.with_static_definition(grant_static);
            b.id()
        };
        plain_artifact(&mut scenario, "Filler 1");
        let claws = plain_artifact(&mut scenario, "Claws of Gix");

        // Flush layers so the grant is live on the base 3-artifact board, proving
        // the granted ability really is the sole {1} source before any sacrifice.
        crate::game::layers::flush_layers(&mut scenario.state);
        assert!(
            scenario.state.objects[&mox]
                .abilities
                .iter()
                .any(|a| matches!(&*a.effect, Effect::Mana { .. })),
            "precondition: Mox has the granted mana ability at 3 artifacts"
        );

        assert!(
            can_pay_activation(&scenario.state, claws, &claws_cost()),
            "granted {{1}} paid on the intact 3-artifact board before the sacrifice → payable"
        );
    }

    /// BLOCKER-1 regression (CR 117.1 + CR 701.13a): a `Composite[{N}, Exile a
    /// CARD]` whose exile leg has `zone: None` and a NON-permanent filter must
    /// classify to `Zone::Hand`, so the battlefield-removal walker returns `None`
    /// and the mana-leg detour is NOT triggered — the composite keeps its
    /// payable dry-run verdict. This pins the walker's hand-vs-battlefield
    /// classification: if `find_battlefield_exile_cost` wrongly routed a
    /// hand-exile here, the detour would mis-fire on a non-battlefield cost.
    #[test]
    fn hand_exile_composite_not_routed_to_battlefield_removal() {
        use crate::types::ability::TypeFilter;
        let card_filter = TargetFilter::Typed(TypedFilter::new(TypeFilter::Card));
        // Classifier: a `zone: None` + non-permanent (Card) filter is Hand, not
        // Battlefield — the exact false-reject guard documented at the walker.
        assert_eq!(
            crate::game::cost_payability::exile_cost_effective_zone(None, Some(&card_filter)),
            Zone::Hand,
            "zone:None + Card filter must classify to Hand"
        );
        let cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::NoCost,
                },
                AbilityCost::Exile {
                    count: 1,
                    zone: None,
                    filter: Some(card_filter),
                },
            ],
        };
        // The battlefield-removal walker must NOT match a hand-exile leg.
        assert!(
            crate::game::casting::find_non_self_battlefield_removal_cost(&cost).is_none(),
            "hand-exile leg must not be treated as a battlefield removal"
        );
        // can_pay keeps the dry-run verdict: NoCost mana + no-op activation-scope
        // exile → payable; the hand-exile leg never triggers the mana-leg detour.
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Jhoira", 0, 1).id();
        scenario.add_card_to_hand(P0, "Some Card");
        assert!(
            can_pay_activation(&scenario.state, src, &cost),
            "hand-exile composite must keep its unchanged (payable) dry-run verdict"
        );
    }

    /// Row 4 (count > 1 exile still payable): a `Composite[{1}, Exile TWO
    /// artifacts from the battlefield]` on a board where the only `{1}` source is
    /// Metalcraft-gated is payable, because CR 601.2g pays {1} on the intact board
    /// before either exile. The mana-first detour is count-agnostic. Mirrors the
    /// count==2 sacrifice case (`claws_sacrifice_two_count_gt_one_not_rejected`).
    #[test]
    fn exile_two_count_gt_one_not_rejected() {
        use crate::types::ability::TypeFilter;
        let mut scenario = GameScenario::new();
        metalcraft_mox(&mut scenario);
        plain_artifact(&mut scenario, "Filler 1");
        let src = plain_artifact(&mut scenario, "Exiler");
        let cost_two = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Exile {
                    count: 2,
                    zone: Some(Zone::Battlefield),
                    filter: Some(TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact))),
                },
            ],
        };
        assert!(
            can_pay_activation(&scenario.state, src, &cost_two),
            "{{1}} paid on the intact board before the exiles → payable"
        );
    }

    /// Row 5 (Discard excluded): a `Composite[{1}, Discard a card]` is NOT a
    /// battlefield removal — discard shrinks the hand, never the board, so it can
    /// never change board-derived mana. The walker must return `None` (proven
    /// no-op, deliberately out of scope).
    #[test]
    fn discard_leg_is_not_battlefield_removal() {
        let cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Discard {
                    count: QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    selection: CardSelectionMode::Chosen,
                    self_scope: DiscardSelfScope::FromHand,
                },
            ],
        };
        assert!(
            crate::game::casting::find_non_self_battlefield_removal_cost(&cost).is_none(),
            "Discard must not be treated as a battlefield removal"
        );
    }

    /// Row 6 (self-ref excluded): a self-referential Exile or Sacrifice leg
    /// (Scavenge/Suspend-style self-exile, "Sacrifice this") is the source's own
    /// removal, not a board-shrinking non-mana leg in the CR 601.2h ordering
    /// sense — the walker must return `None` for both. The SelfRef-first arm in
    /// `find_battlefield_exile_cost` exists precisely because a SelfRef filter can
    /// be permanent-implying and would otherwise pass the battlefield gate.
    #[test]
    fn self_ref_removal_legs_are_out_of_scope() {
        let self_exile = AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: Some(TargetFilter::SelfRef),
        };
        assert!(
            crate::game::casting::find_non_self_battlefield_removal_cost(&self_exile).is_none(),
            "self-exile leg must not be treated as a battlefield removal"
        );
        let self_sacrifice = AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1));
        assert!(
            crate::game::casting::find_non_self_battlefield_removal_cost(&self_sacrifice).is_none(),
            "self-sacrifice leg must not be treated as a battlefield removal"
        );
        let self_return = AbilityCost::ReturnToHand {
            count: 1,
            filter: Some(TargetFilter::SelfRef),
            from_zone: None,
        };
        assert!(
            crate::game::casting::find_non_self_battlefield_removal_cost(&self_return).is_none(),
            "self-bounce leg must not be treated as a battlefield removal"
        );
    }

    /// MED-2 regression (CR 118.3 / CR 601.2h): at `PaymentScope::Resolution` a
    /// shape with no resolution payment arm must yield `Failed` via the single
    /// structural guard — never a silent fake-`Paid` no-op, never an unintended
    /// execution. A bare `Waterbend` (whose `pay_ability_cost_inner` arm is a
    /// no-op that previously returned `Paid`) and a singleton `Tap` (which
    /// previously executed, tapping the source) are the two discriminating
    /// shapes. Before the guard the Waterbend arm returned `Paid` and the Tap
    /// arm tapped the source.
    #[test]
    fn unsupported_shapes_fail_at_resolution_without_mutation() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Source", 1, 1).id();
        // The effect body is irrelevant — the structural guard fires before any
        // arm reads the ability; use a trivial self-counter effect as the stub.
        let ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            src,
            P0,
        );
        let scope = PaymentScope::Resolution {
            ability: &ability,
            cost_move_root: ResolutionCostMoveRoot::EffectPayCost,
        };

        // (i) Waterbend at Resolution → Failed (was a silent no-op `Paid`).
        let waterbend = AbilityCost::Waterbend {
            cost: ManaCost::generic(1),
        };
        let outcome = pay_ability_cost_inner(
            &mut scenario.state,
            P0,
            src,
            &waterbend,
            &mut Vec::new(),
            &scope,
            None,
        )
        .unwrap();
        assert!(
            matches!(outcome, PaymentOutcome::Failed { .. }),
            "Waterbend at Resolution must Failed, got {outcome:?}"
        );

        // (ii) Singleton Tap at Resolution → Failed, and the source stays
        // untapped (was: executed, tapping the source).
        let outcome = pay_ability_cost_inner(
            &mut scenario.state,
            P0,
            src,
            &AbilityCost::Tap,
            &mut Vec::new(),
            &scope,
            None,
        )
        .unwrap();
        assert!(
            matches!(outcome, PaymentOutcome::Failed { .. }),
            "singleton Tap at Resolution must Failed, got {outcome:?}"
        );
        assert!(
            !scenario.state.objects.get(&src).unwrap().tapped,
            "Tap at Resolution must not tap the source"
        );
    }

    /// Installs a synthetic MANDATORY `Prevent`-on-`AddCounter` replacement
    /// scoped `AnyPlayer` on a fresh P0 permanent — the unit-side sibling of
    /// `serpent_society_ward_poison_cost.rs`'s installers. No printed card
    /// produces an OPTIONAL `AddCounter` replacement, and CR 614.17c
    /// short-circuits every MANDATORY one ahead of the CR 616.1 prompt.
    fn install_any_player_counter_prohibition(scenario: &mut GameScenario) {
        let source = scenario.add_creature(P0, "Poison Warden", 1, 1).id();
        let mut def = crate::types::ability::ReplacementDefinition::new(
            crate::types::replacements::ReplacementEvent::AddCounter,
        );
        def.mode = crate::types::ability::ReplacementMode::Mandatory;
        def.quantity_modification = Some(crate::types::ability::QuantityModification::Prevent);
        def.valid_player = Some(crate::types::ability::ReplacementPlayerScope::AnyPlayer);
        let reps = vec![def];
        let obj = scenario.state.objects.get_mut(&source).unwrap();
        obj.replacement_definitions = reps.clone().into();
        obj.base_replacement_definitions = std::sync::Arc::new(reps);
    }

    /// CR 614.17b: the aggregate arms of
    /// `resolution_cost_includes_impossible_event`, pinned from both sides.
    ///
    /// `Composite` is `.any()` — CR 614.17b's "a cost that INCLUDES that event"
    /// — and `OneOf` is `.all()`, because CR 118.12a pays exactly one option, so
    /// a disjunctive cost is unchoosable only when EVERY option is. The two are
    /// the De Morgan dual of `can_pay_resolution`'s `.all()` / `.any()`.
    ///
    /// Affordability is held CONSTANT across the pair: every leg is affordable
    /// at life 20, and the row asserts the life totals so that stays true.
    ///
    /// Revert probe: writing the `OneOf` arm as `.any()` makes `mixed_oneof`
    /// answer `true` and fails assertion (iii) on the same board that keeps
    /// `counter_composite` answering `true`.
    #[test]
    fn resolution_cost_prohibition_covers_composite_and_disjunctive_shapes() {
        use crate::types::player::PlayerCounterKind;

        let poison5 = AbilityCost::GetPlayerCounters {
            counter_kind: PlayerCounterKind::Poison,
            count: 5,
        };
        let poison3 = AbilityCost::GetPlayerCounters {
            counter_kind: PlayerCounterKind::Poison,
            count: 3,
        };
        let pay_life_1 = AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 1 },
        };
        let counter_composite = AbilityCost::Composite {
            costs: vec![poison5.clone(), pay_life_1.clone()],
        };
        let control_composite = AbilityCost::Composite {
            costs: vec![pay_life_1.clone(), pay_life_1.clone()],
        };
        let mixed_oneof = AbilityCost::OneOf {
            costs: vec![poison5.clone(), pay_life_1.clone()],
        };
        let all_impossible_oneof = AbilityCost::OneOf {
            costs: vec![poison5, poison3],
        };

        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Source", 1, 1).id();
        let ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            src,
            P0,
        );
        let scope = PaymentScope::Resolution {
            ability: &ability,
            cost_move_root: ResolutionCostMoveRoot::EffectPayCost,
        };

        // (v) + (vi) CLEAN board: the instrument fires only on the prohibition,
        // and every leg is affordable at life 20.
        assert_eq!(
            scenario.state.players[P0.0 as usize].life, 20,
            "affordability is held constant across the pair"
        );
        for (name, cost) in [
            ("counter_composite", &counter_composite),
            ("control_composite", &control_composite),
            ("mixed_oneof", &mixed_oneof),
            ("all_impossible_oneof", &all_impossible_oneof),
        ] {
            assert!(
                !resolution_cost_includes_impossible_event(&scenario.state, P0, cost, &ability),
                "{name} must not be prohibited on a clean board"
            );
            assert!(
                can_pay(&scenario.state, P0, src, cost, &scope),
                "{name} must be payable on a clean board"
            );
        }

        install_any_player_counter_prohibition(&mut scenario);
        assert_eq!(
            scenario.state.players[P0.0 as usize].life, 20,
            "installing the prohibition must not change affordability"
        );

        // (i) `Composite` INCLUDES an impossible component ⇒ prohibited.
        assert!(
            resolution_cost_includes_impossible_event(
                &scenario.state,
                P0,
                &counter_composite,
                &ability
            ),
            "a Composite including a prohibited counter gain must be prohibited"
        );
        // Control cost: same board, same prohibition, same affordability.
        assert!(
            !resolution_cost_includes_impossible_event(
                &scenario.state,
                P0,
                &control_composite,
                &ability
            ),
            "a Composite with no counter component must not be prohibited"
        );

        // (ii) The payability oracle follows the leaves.
        assert!(
            !can_pay(&scenario.state, P0, src, &counter_composite, &scope),
            "can_pay must refuse a Composite whose payment includes an impossible event"
        );
        assert!(
            can_pay(&scenario.state, P0, src, &control_composite, &scope),
            "can_pay must still accept the control Composite"
        );

        // (iii) The `.any()`-leak guard: one payable option keeps the whole
        // disjunctive cost choosable (CR 118.12a).
        assert!(
            !resolution_cost_includes_impossible_event(&scenario.state, P0, &mixed_oneof, &ability),
            "a OneOf with one payable option must not be prohibited"
        );
        assert!(
            can_pay(&scenario.state, P0, src, &mixed_oneof, &scope),
            "a OneOf with one payable option must stay payable"
        );

        // (iv) Its paired opposite: every option impossible ⇒ prohibited.
        assert!(
            resolution_cost_includes_impossible_event(
                &scenario.state,
                P0,
                &all_impossible_oneof,
                &ability
            ),
            "a OneOf whose every option is impossible must be prohibited"
        );
        assert!(
            !can_pay(&scenario.state, P0, src, &all_impossible_oneof, &scope),
            "a OneOf whose every option is impossible must not be payable"
        );
    }

    /// CR 614.17b: "If an event can't happen, a player can't choose to pay a
    /// cost that includes that event" — at `PaymentScope::Activation`.
    ///
    /// Wall of Roots' mana ability costs `EffectCost { PutCounter { SelfRef } }`;
    /// Solemnity's second sentence is a CR 614.17 can't-effect on exactly that
    /// placement. Activation never consults
    /// `resolution_cost_includes_impossible_event` and
    /// `is_payable_for_activation` admits every `EffectCost` unconditionally, so
    /// the refusal inside the payment arm is the only answer available here.
    ///
    /// Revert probe: rewriting that arm's `let prevented =
    /// self_counter_placement_is_prohibited(…)` as `let prevented = false` makes
    /// the payer answer `Paid` on a board where the counter is prevented, failing
    /// (ii) and (iii).
    #[test]
    fn activation_self_counter_cost_under_solemnity_is_refused() {
        let mut scenario = GameScenario::new();
        let wall = scenario.add_creature(P0, "Wall of Roots", 0, 5).id();
        let cost = AbilityCost::EffectCost {
            effect: Box::new(Effect::PutCounter {
                counter_type: CounterType::PowerToughness {
                    power: 0,
                    toughness: -1,
                },
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            }),
        };

        // (i) Reach guard: the same cost on the same board is payable before the
        // prohibition exists, so (ii) and (iii) cannot pass upstream of the arm.
        assert!(
            can_pay_activation(&scenario.state, wall, &cost),
            "an unprohibited self-counter activation cost must be payable"
        );

        scenario.add_enchantment_from_oracle(
            P0,
            "Solemnity",
            "Players can't get counters.\n\
             Counters can't be put on artifacts, creatures, enchantments, or lands.",
        );

        // (ii) The ability is no longer activatable.
        assert!(
            !can_pay_activation(&scenario.state, wall, &cost),
            "a prevented counter placement must make the activation cost unpayable"
        );

        // (iii) CR 601.2h: the payer refuses rather than reporting a cost whose
        // event the replacement swallowed.
        let refused = pay_ability_cost_for_activation(
            &mut scenario.state,
            P0,
            wall,
            &cost,
            Some(0),
            &mut Vec::new(),
        );
        assert!(
            matches!(refused, Err(EngineError::ActionNotAllowed(_))),
            "a prevented counter-placement cost must refuse activation, got {refused:?}"
        );
    }

    // ---------------------------------------------------------------------
    // Resolution-scope `AbilityCost::TapCreatures` payment bounds.
    //
    // The resolution registration site below (`pay_ability_cost_inner`'s
    // `TapCreatures` @ `PaymentScope::Resolution` arm) emits the
    // `WaitingFor::PayCost { count, min_count }` window that
    // `casting_costs::pay_tap_creatures_selection` later validates against.
    // That validator is SHARED with the activation/casting path, which moved
    // from an exact-match check to a `[min_count, count]` range check to
    // support the CR 107.3a X-sentinel ("Tap X untapped …"). The hardcoded
    // `min_count: 0` here was harmless under exact-match and load-bearing
    // (and wrong — CR 601.2h forbids partial payment) under the range check.
    // ---------------------------------------------------------------------

    /// Stub resolution ability for the tap-cost arm. The effect body is never
    /// read by the `TapCreatures` arm (it only uses the ability for
    /// `FilterContext`), so a trivial self-counter effect suffices.
    fn tap_cost_stub_ability(src: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::SelfRef,
            },
            Vec::new(),
            src,
            P0,
        )
    }

    /// Drive the real resolution-time payment entry point
    /// (`pay_ability_cost_inner` @ `PaymentScope::Resolution`, the path
    /// `effects::pay` takes for a reflexive "you may tap N creatures" cost).
    fn pay_tap_cost_at_resolution(
        state: &mut GameState,
        src: ObjectId,
        ability: &ResolvedAbility,
        requirement: TapCreaturesRequirement,
    ) -> PaymentOutcome {
        let cost = AbilityCost::TapCreatures {
            requirement,
            filter: TargetFilter::Typed(TypedFilter::creature()),
        };
        pay_ability_cost_inner(
            state,
            P0,
            src,
            &cost,
            &mut Vec::new(),
            &PaymentScope::Resolution {
                ability,
                cost_move_root: ResolutionCostMoveRoot::EffectPayCost,
            },
            None,
        )
        .expect("resolution-time tap-creatures payment must not error")
    }

    /// Read the emitted `[min_count, count]` payment window plus the offered
    /// choices out of `state.waiting_for`, failing loudly if the arm did not
    /// surface a `TapCreatures` PayCost prompt at all.
    fn emitted_tap_cost_window(state: &GameState) -> (usize, usize, Vec<ObjectId>) {
        match &state.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { .. },
                choices,
                count,
                min_count,
                resume: CostResume::Resolution,
                ..
            } => (*min_count, *count, choices.clone()),
            other => panic!("expected a resolution TapCreatures PayCost prompt, got {other:?}"),
        }
    }

    /// CR 601.2h ("Partial payments are not allowed") regression discriminator
    /// for the resolution-scope registration site. Kitt-Kanto-shaped: a fixed
    /// `count: 2` reflexive tap cost with exactly two eligible untapped
    /// creatures.
    ///
    /// Reverting the `min_count` fix (back to the hardcoded `min_count: 0`)
    /// flips BOTH assertions below: the emitted floor becomes `0`, and the
    /// shared range validator then returns `Ok(())` for a one-creature
    /// selection — silently letting a "tap two creatures" cost be paid by
    /// tapping one.
    #[test]
    fn resolution_fixed_tap_cost_rejects_partial_payment() {
        let mut scenario = GameScenario::new();
        let src = scenario
            .add_creature(P0, "Kitt Kanto, Mayhem Diva", 2, 3)
            .id();
        scenario.add_creature(P0, "Helper", 1, 1);
        let ability = tap_cost_stub_ability(src);

        let outcome = pay_tap_cost_at_resolution(
            &mut scenario.state,
            src,
            &ability,
            TapCreaturesRequirement::count(2),
        );
        assert!(
            matches!(outcome, PaymentOutcome::Paused { .. }),
            "a payable fixed tap cost must pause for the player's selection, got {outcome:?}"
        );

        let (min_count, count, choices) = emitted_tap_cost_window(&scenario.state);
        assert_eq!(choices.len(), 2, "both untapped creatures must be offered");

        // Positive reach guard: the one-creature selection really is drawn from
        // the offered choice set, so the rejection below is the range check
        // firing and not the eligibility pre-check.
        let partial = vec![choices[0]];
        assert!(
            choices.contains(&partial[0]),
            "reach guard: the partial selection must be an eligible choice"
        );

        // BEHAVIORAL discriminator, asserted BEFORE the shape assertions so a
        // revert fails here on the real consequence rather than on the window
        // shape: the emitted `[min_count, count]` window is threaded verbatim
        // into the shared validator exactly as `engine.rs`'s
        // `CostResume::Resolution` handler threads it. With the hardcoded
        // `min_count: 0` this call returns `Ok(())` and TAPS ONE CREATURE to
        // pay a "tap two creatures" cost.
        let err = crate::game::casting_costs::pay_tap_creatures_selection(
            &mut scenario.state,
            min_count,
            count,
            TapCreaturesSelectionMode::Fixed,
            &choices,
            &partial,
            &mut Vec::new(),
        )
        .expect_err("CR 601.2h: tapping 1 of a required 2 creatures is a partial payment");
        assert!(
            matches!(err, EngineError::InvalidAction(_)),
            "partial payment must be an InvalidAction, got {err:?}"
        );
        assert!(
            !scenario.state.objects[&partial[0]].tapped,
            "a rejected partial payment must not tap anything"
        );

        // Secondary shape pin on the emitted window itself.
        assert_eq!(
            count, 2,
            "the fixed requirement's upper bound is the printed count"
        );
        assert_eq!(
            min_count, 2,
            "CR 601.2h: a fixed `count: 2` cost must advertise a floor of 2, not 0"
        );
    }

    /// Sibling of the discriminator: full payment of the same fixed cost is
    /// still accepted and actually taps both creatures (the fix narrows the
    /// window, it must not break the legal payment).
    #[test]
    fn resolution_fixed_tap_cost_accepts_full_payment() {
        let mut scenario = GameScenario::new();
        let src = scenario
            .add_creature(P0, "Kitt Kanto, Mayhem Diva", 2, 3)
            .id();
        scenario.add_creature(P0, "Helper", 1, 1);
        let ability = tap_cost_stub_ability(src);

        pay_tap_cost_at_resolution(
            &mut scenario.state,
            src,
            &ability,
            TapCreaturesRequirement::count(2),
        );
        let (min_count, count, choices) = emitted_tap_cost_window(&scenario.state);

        crate::game::casting_costs::pay_tap_creatures_selection(
            &mut scenario.state,
            min_count,
            count,
            TapCreaturesSelectionMode::Fixed,
            &choices,
            &choices,
            &mut Vec::new(),
        )
        .expect("CR 601.2h: tapping exactly the required 2 creatures is a legal full payment");
        assert!(
            choices.iter().all(|id| scenario.state.objects[id].tapped),
            "a full payment must tap every chosen creature"
        );
    }

    /// `count: 1` boundary, the other real resolution-scope card shape today —
    /// Meanders Guide ("you may tap another untapped Merfolk you control").
    /// Only the COUNT axis is reproduced here; the card's `Merfolk`/`Another`
    /// filter is orthogonal to the payment window under test, and the wider
    /// `creature` filter deliberately offers TWO eligible creatures so the
    /// `(1, 1)` window is proven to come from the requirement rather than from
    /// coinciding with the eligible-set size.
    ///
    /// Reverting the fix makes the empty selection legal — a "tap an untapped
    /// creature you control" cost paid by tapping nothing.
    #[test]
    fn resolution_single_tap_cost_rejects_empty_selection() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Meanders Guide", 1, 2).id();
        scenario.add_creature(P0, "Helper", 1, 1);
        let ability = tap_cost_stub_ability(src);

        pay_tap_cost_at_resolution(
            &mut scenario.state,
            src,
            &ability,
            TapCreaturesRequirement::count(1),
        );
        let (min_count, count, choices) = emitted_tap_cost_window(&scenario.state);

        // BEHAVIORAL discriminator first: with the hardcoded `min_count: 0`
        // this returns `Ok(())`, paying a "tap an untapped creature you
        // control" cost by tapping nothing at all.
        let err = crate::game::casting_costs::pay_tap_creatures_selection(
            &mut scenario.state,
            min_count,
            count,
            TapCreaturesSelectionMode::Fixed,
            &choices,
            &[],
            &mut Vec::new(),
        )
        .expect_err(
            "CR 601.2h: paying a `count: 1` tap cost with zero creatures is a partial payment",
        );
        assert!(
            matches!(err, EngineError::InvalidAction(_)),
            "empty selection must be an InvalidAction, got {err:?}"
        );
        assert!(
            choices.iter().all(|id| !scenario.state.objects[id].tapped),
            "a rejected empty payment must not tap anything"
        );

        // Secondary shape pin on the emitted window.
        assert_eq!(
            (min_count, count),
            (1, 1),
            "CR 601.2h: a `count: 1` cost is an exact-1 window even with 2 eligible creatures"
        );

        // The legal one-creature payment still works.
        crate::game::casting_costs::pay_tap_creatures_selection(
            &mut scenario.state,
            min_count,
            count,
            TapCreaturesSelectionMode::Fixed,
            &choices,
            &choices[..1],
            &mut Vec::new(),
        )
        .expect("tapping exactly 1 creature satisfies a `count: 1` cost");
        assert!(
            scenario.state.objects[&choices[0]].tapped,
            "the chosen creature must be tapped by the accepted payment"
        );
    }

    /// CR 208.1 + CR 601.2f (Crew CR 702.122a / Saddle CR 702.171a / Teamwork):
    /// the aggregate (Crew/Saddle/Teamwork) shape taps ANY
    /// number of creatures whose total positive power satisfies the comparator,
    /// so its floor stays 0 — unchanged by this fix. This pins that the widened
    /// `(kind, count, min_count)` binding did not leak the fixed-count floor
    /// into the aggregate arm.
    #[test]
    fn resolution_aggregate_tap_cost_keeps_zero_floor() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Crewed Vehicle", 1, 1).id();
        scenario.add_creature(P0, "Helper", 1, 1);
        let ability = tap_cost_stub_ability(src);

        let outcome = pay_tap_cost_at_resolution(
            &mut scenario.state,
            src,
            &ability,
            TapCreaturesRequirement::total_power_at_least(2),
        );
        assert!(
            matches!(outcome, PaymentOutcome::Paused { .. }),
            "two 1-power creatures satisfy total power >= 2, got {outcome:?}"
        );

        match &scenario.state.waiting_for {
            WaitingFor::PayCost {
                kind:
                    PayCostKind::TapCreatures {
                        mode: TapCreaturesSelectionMode::Aggregate(aggregate),
                    },
                count,
                min_count,
                ..
            } => {
                assert_eq!(
                    *min_count, 0,
                    "CR 601.2f: the aggregate form admits any subset size, so the floor is 0"
                );
                assert_eq!(*count, 2, "the aggregate ceiling is the eligible count");
                assert_eq!(
                    aggregate.value, 2,
                    "the advertised comparator value is carried through"
                );
            }
            other => panic!("expected an aggregate TapCreatures PayCost prompt, got {other:?}"),
        }
    }

    /// CR 118.3 + CR 601.2h: the aggregate arm is the case the dedup guard
    /// actually protects. `tap_creatures_total_power` sums `chosen` with NO
    /// dedup, so a repeated id double-counts its power: `[c0, c0]` on a
    /// 1-power creature sums to 2 and spuriously satisfies "total power >= 2"
    /// with only ONE real creature. Without the guard in
    /// `pay_tap_creatures_selection` this returns `Ok(())` and taps a single
    /// 1-power creature to pay a 2-power crew-shaped cost.
    #[test]
    fn resolution_aggregate_tap_cost_rejects_duplicate_creature() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Crewed Vehicle", 1, 1).id();
        scenario.add_creature(P0, "Helper", 1, 1);
        let ability = tap_cost_stub_ability(src);

        pay_tap_cost_at_resolution(
            &mut scenario.state,
            src,
            &ability,
            TapCreaturesRequirement::total_power_at_least(2),
        );

        let (min_count, count, choices) = emitted_tap_cost_window(&scenario.state);
        let mode = match &scenario.state.waiting_for {
            WaitingFor::PayCost {
                kind: PayCostKind::TapCreatures { mode },
                ..
            } => *mode,
            other => panic!("expected a TapCreatures PayCost prompt, got {other:?}"),
        };
        assert!(
            matches!(mode, TapCreaturesSelectionMode::Aggregate(_)),
            "reach guard: this test must exercise the Aggregate arm, got {mode:?}"
        );

        // Positive reach guard: ONE creature's power alone does not satisfy the
        // threshold, but the duplicated pair sums to exactly the threshold — so
        // the aggregate check PASSES on this submission and the rejection below
        // can only be the dedup guard firing, not "does not satisfy".
        let single = [choices[0]];
        assert_eq!(
            crate::game::casting_costs::tap_creatures_total_power(&scenario.state, &single),
            1,
            "reach guard: one eligible creature contributes only 1 power"
        );
        let duplicated = vec![choices[0], choices[0]];
        assert_eq!(
            crate::game::casting_costs::tap_creatures_total_power(&scenario.state, &duplicated),
            2,
            "reach guard: the duplicate double-counts to exactly the threshold, so the aggregate \
             check cannot be what rejects this submission"
        );
        assert!(
            duplicated.iter().all(|id| choices.contains(id)),
            "reach guard: the submitted id must be an eligible choice"
        );

        let err = crate::game::casting_costs::pay_tap_creatures_selection(
            &mut scenario.state,
            min_count,
            count,
            mode,
            &choices,
            &duplicated,
            &mut Vec::new(),
        )
        .expect_err("CR 601.2h: one creature cannot pay an aggregate tap cost twice");
        let EngineError::InvalidAction(message) = &err else {
            panic!("a duplicate selection must be an InvalidAction, got {err:?}");
        };
        assert!(
            message.contains("Cannot tap the same creature twice"),
            "the dedup guard must reject this, not the aggregate comparator, got {message:?}"
        );
        assert!(
            choices.iter().all(|id| !scenario.state.objects[id].tapped),
            "a rejected duplicate payment must not tap anything"
        );
    }

    /// Hostile fixture: fewer eligible creatures than the fixed requirement.
    /// The `eligible.len() < min_count` pre-check (CR 118.3) must fail the
    /// payment outright — no `WaitingFor::PayCost` prompt may be surfaced at
    /// all, or the player would be handed an unsatisfiable selection window.
    #[test]
    fn resolution_fixed_tap_cost_fails_without_enough_eligible() {
        let mut scenario = GameScenario::new();
        let src = scenario
            .add_creature(P0, "Kitt Kanto, Mayhem Diva", 2, 3)
            .id();
        // A creature controlled by the OPPONENT and a TAPPED one of P0's own are
        // both ineligible, so only the source itself is a legal choice (1 < 2).
        scenario.add_creature(PlayerId(1), "Opposing Bear", 2, 2);
        let dozing = scenario.add_creature(P0, "Already Tapped", 1, 1).id();
        scenario.state.objects.get_mut(&dozing).unwrap().tapped = true;
        let ability = tap_cost_stub_ability(src);
        let before = scenario.state.waiting_for.clone();

        let outcome = pay_tap_cost_at_resolution(
            &mut scenario.state,
            src,
            &ability,
            TapCreaturesRequirement::count(2),
        );
        assert!(
            matches!(outcome, PaymentOutcome::Failed { .. }),
            "CR 118.3: 1 eligible creature cannot pay a `count: 2` tap cost, got {outcome:?}"
        );
        assert_eq!(
            scenario.state.waiting_for, before,
            "a failed tap cost must not surface an unsatisfiable PayCost prompt"
        );
    }

    /// CR 107.3a: the resolution-scope payability ORACLE (`can_pay_resolution`,
    /// reached in production through the `can_pay_cost` scope dispatcher) must
    /// route the `u32::MAX` X-sentinel through `sacrifice_cost_bounds` like every
    /// other checkpoint. X=0 is a legal announcement, so the cost is payable even
    /// with ZERO eligible creatures on the battlefield.
    ///
    /// Reverting the `can_pay_resolution` fix restores
    /// `eligible.len() >= *count as usize`, i.e. `0 >= u32::MAX as usize`, and
    /// the first assertion below flips to `false`.
    #[test]
    fn resolution_x_sentinel_tap_cost_is_payable_with_zero_eligible() {
        let mut scenario = GameScenario::new();
        // The only permanent is a non-creature, so the creature-typed tap cost
        // has an empty eligible set — the exact hostile shape the sentinel
        // comparison used to fail on.
        let src = scenario.add_artifact_from_oracle(P0, "Powerstone", "").id();
        let ability = tap_cost_stub_ability(src);
        let x_sentinel = AbilityCost::TapCreatures {
            requirement: TapCreaturesRequirement::Count { count: u32::MAX },
            filter: TargetFilter::Typed(TypedFilter::creature()),
        };

        // Positive reach guard: prove the eligible set really is empty, so the
        // verdict below is the sentinel bound and not an accidental hit.
        assert!(
            find_eligible_tap_creatures_targets(
                &scenario.state,
                P0,
                &ability,
                &TargetFilter::Typed(TypedFilter::creature()),
            )
            .is_empty(),
            "reach guard: the fixture must have zero eligible creatures"
        );

        assert!(
            can_pay_resolution(&scenario.state, P0, &x_sentinel, &ability),
            "CR 107.3a: X=0 is a legal announcement, so an X-sentinel resolution \
             tap cost is payable with no eligible creatures"
        );

        // Sibling/negative: a FIXED count is still gated by the eligible set, so
        // the fix widens only the sentinel case.
        assert!(
            !can_pay_resolution(
                &scenario.state,
                P0,
                &AbilityCost::TapCreatures {
                    requirement: TapCreaturesRequirement::count(1),
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                },
                &ability,
            ),
            "CR 601.2h: a fixed `count: 1` tap cost is NOT payable with zero eligible creatures"
        );
    }
}
