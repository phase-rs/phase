//! Bounded, reducer-backed prospective mana routes for AI consumers.
//!
//! This module intentionally proves only the fetch-then-cast grammar.  It is
//! not a hidden-zone searcher or a second payment implementation: every route
//! edge is a frozen engine candidate and every transition uses the ordinary
//! interaction reducer with the candidate's authenticated actor and semantic
//! owner.

use std::cmp::Ordering;

use crate::ai_support::{
    validated_candidate_actions_for_semantic_owner, CandidateAction, TacticalClass,
};
use crate::game::engine::{
    apply_actionless_priority_pass_for_prospective, apply_interaction_for_prospective_simulation,
    apply_interaction_for_simulation, ProspectiveSimulationOutcome,
};
use crate::game::turn_control::authorized_submitter_for_player;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect,
    QuantityExpr, SacrificeRequirement, SearchSelectionConstraint, TargetFilter,
};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{
    DelayedTriggerOrigin, ObjectId, ObjectIdentityBinding, ObjectIncarnationRef, TriggerFiring,
};
use crate::types::interaction::{ActiveInteractionSlot, InteractionSessionId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// The one supported prospective route request. `cast` is a pre-existing hand
/// card binding; the route never learns a card identity from a library before
/// the real search prompt is reached.
#[derive(Debug, Clone)]
struct FetchThenCastRequest {
    pub fetch: CandidateAction,
    pub cast: ObjectIdentityBinding,
}
/// A completed route, an opaque real search prompt, or a fail-closed result.
#[derive(Debug, Clone)]
enum ProspectiveManaResult {
    Contingent(Box<SearchChoiceAtPrompt>),
    Indeterminate,
}

/// Opaque continuation for an actual live `SearchChoice` prompt.  Its state,
/// authority, and remaining private budget cannot be reset by consumers.
#[derive(Debug, Clone)]
struct SearchChoiceAtPrompt {
    state: GameState,
    capability: StateCapabilityBinding,
    search: SearchBinding,
    owner: PlayerId,
}

/// A reducer-certified choice for one real fetch search prompt. This retains
/// no simulated state or terminal evaluation; it can only be redeemed against
/// the identical live prompt that produced its authoritative fingerprint.
#[derive(Debug, Clone)]
pub struct CertifiedFetchPrompt {
    capability: StateCapabilityBinding,
    search: SearchBinding,
    owner: PlayerId,
    selection: GameAction,
    follow_up: CertifiedFetchFollowUp,
}

impl CertifiedFetchPrompt {
    /// Return the certified selection only while the real interaction is the
    /// exact prompt that was reducer-certified. A stale or altered prompt
    /// deliberately falls through to the caller's ordinary choice policy.
    pub fn action_for(&self, state: &GameState, semantic_owner: PlayerId) -> Option<GameAction> {
        if self.owner != semantic_owner
            || !self.capability.matches(state)
            || !self.search.matches(state)
        {
            return None;
        }
        validated_candidate_actions_for_semantic_owner(state, semantic_owner)
            .into_iter()
            .any(|candidate| {
                candidate.action.cmp_stable(&self.selection) == Ordering::Equal
                    && candidate.metadata.tactical_class == TacticalClass::Selection
            })
            .then(|| self.selection.clone())
    }

    /// Retain the exact post-search cast proof without exposing the simulated
    /// state that produced it. The follow-up accepts only the resulting live
    /// state revision and one currently validated spell candidate.
    pub fn follow_up(&self) -> CertifiedFetchFollowUp {
        self.follow_up.clone()
    }
}

/// Opaque post-search action from a reducer-certified fetch route.
#[derive(Debug, Clone)]
pub struct CertifiedFetchFollowUp {
    capability: StateCapabilityBinding,
    owner: PlayerId,
    cast: ObjectIdentityBinding,
}

/// Opaque reducer-certified Pact route. The delayed-trigger provenance remains
/// engine-private and is used only to retain a live cast continuation.
#[derive(Debug, Clone)]
pub struct CertifiedPactPlan {
    root: FrozenCandidate,
    receipt: PactReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PactReceipt {
    provenance: DelayedTriggerOrigin,
    payer: PlayerId,
    source_id: ObjectId,
}

/// The plan deliberately exposes only its lifecycle, never the receipt or a
/// simulated state. A dormant plan stays bound to its exact installed delayed
/// trigger until the engine consumes it or a conflicting state invalidates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PactPlanState {
    Dormant,
    Expired,
}

impl CertifiedPactPlan {
    /// Revalidate the proposed root against the exact state and legal candidate
    /// that produced it. This is called before moving a scored draft into a
    /// durable AI session route.
    pub fn root_action_for(
        &self,
        state: &GameState,
        semantic_owner: PlayerId,
    ) -> Option<GameAction> {
        (self.root.semantic_owner == semantic_owner
            && self.root.capability.matches(state)
            && frozen_candidates(state, semantic_owner)
                .iter()
                .any(|candidate| self.root.matches_candidate(candidate)))
        .then(|| self.root.action.clone())
    }

    /// Check the exact live delayed-trigger carrier without exposing its
    /// private provenance. A plan cannot attach to another obligation with the
    /// same source object id.
    pub fn state_for(&self, state: &GameState, semantic_owner: PlayerId) -> PactPlanState {
        if semantic_owner != self.receipt.payer
            || matches!(state.waiting_for, WaitingFor::GameOver { .. })
        {
            return PactPlanState::Expired;
        }
        if state
            .pending_cast
            .as_ref()
            .is_some_and(|pending| pending.object_id == self.receipt.source_id)
            || state.stack.iter().any(|entry| {
                entry.id == self.receipt.source_id && entry.controller == self.receipt.payer
            })
            || (!matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.resolving_stack_entry.as_ref().is_some_and(|entry| {
                    entry.id == self.receipt.source_id && entry.controller == self.receipt.payer
                }))
        {
            return PactPlanState::Dormant;
        }
        if state.delayed_triggers.iter().any(|trigger| {
            trigger.provenance.origin() == Some(self.receipt.provenance)
                && trigger.controller == self.receipt.payer
                && trigger.source_id == self.receipt.source_id
        }) || state.deferred_triggers.iter().any(|trigger| {
            trigger.firing() == TriggerFiring::ReceiptEligible(self.receipt.provenance)
        }) || state.pending_trigger_order.as_ref().is_some_and(|order| {
            order.groups.iter().any(|group| {
                group.triggers.iter().any(|trigger| {
                    trigger.firing == TriggerFiring::ReceiptEligible(self.receipt.provenance)
                })
            })
        }) || state.pending_trigger_firing
            == Some(TriggerFiring::ReceiptEligible(self.receipt.provenance))
            || (!matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.resolving_trigger_firing
                    == Some(TriggerFiring::ReceiptEligible(self.receipt.provenance)))
            || state
                .stack_trigger_firings
                .values()
                .any(|firing| *firing == TriggerFiring::ReceiptEligible(self.receipt.provenance))
        {
            PactPlanState::Dormant
        } else {
            PactPlanState::Expired
        }
    }
}

/// Derive a Pact certificate from the exact delayed-install observation emitted
/// by each successful frozen reducer boundary, then confirm the matching live
/// delayed trigger still carries that private provenance.
pub fn certify_pact_plan(state: &GameState, root: &CandidateAction) -> Option<CertifiedPactPlan> {
    let GameAction::CastSpell { object_id, .. } = root.action else {
        return None;
    };
    if !is_pact_payment_cast(state, &root.action) {
        return None;
    }
    let frozen = FrozenCandidate::capture(state, root)?;
    if frozen.tactical_class != TacticalClass::Spell {
        return None;
    }
    let mut projected = state.clone();
    let outcome = frozen.clone().apply_with_lifecycle(&mut projected).ok()?;
    if draws_are_opaque(&projected, &outcome.action.events) {
        return None;
    }
    let mut budget = ProspectiveBudget::default();
    let receipt = receipt_installed_by(&projected, object_id, frozen.semantic_owner, &outcome)
        .or_else(|| {
            advance_pact_to_install(
                &mut projected,
                frozen.semantic_owner,
                object_id,
                &mut budget,
            )
        })?;
    pact_payment_survives(&mut projected, frozen.semantic_owner, receipt, &mut budget).then(|| {
        CertifiedPactPlan {
            root: frozen,
            receipt,
        }
    })
}

/// CR 603.7a + CR 118.1 + CR 104.3e: Cheap, typed prefilter for the Pact-class
/// prospective route. This performs
/// no candidate enumeration or reducer projection, so ordinary casts never
/// pay the bounded prospective-simulation cost.
pub fn is_pact_payment_cast(state: &GameState, action: &GameAction) -> bool {
    let GameAction::CastSpell { object_id, .. } = action else {
        return false;
    };
    state.objects.get(object_id).is_some_and(|source| {
        source
            .abilities
            .iter()
            .filter(|ability| ability.kind == AbilityKind::Spell)
            .any(is_pact_payment_ability)
    })
}

/// Recognize a next-upkeep mandatory mana payment whose failed-payment branch
/// loses the delayed trigger's controller. The shape is card-name agnostic and
/// intentionally excludes optional, modal, and unrelated delayed effects.
pub fn is_pact_payment_ability(ability: &AbilityDefinition) -> bool {
    !ability.optional
        && ability.condition.is_none()
        && (matches!(
            ability.effect.as_ref(),
            Effect::CreateDelayedTrigger {
                condition:
                    DelayedTriggerCondition::AtNextPhaseForPlayer {
                        phase: crate::types::phase::Phase::Upkeep,
                        ..
                    },
                effect,
                ..
            } if delayed_trigger_is_mandatory_mana_payment_or_loss_definition(effect)
        ) || ability
            .sub_ability
            .as_deref()
            .filter(|sub| !sub.optional && sub.condition.is_none())
            .is_some_and(is_pact_payment_ability))
}

fn delayed_trigger_is_mandatory_mana_payment_or_loss_definition(
    ability: &AbilityDefinition,
) -> bool {
    let Effect::PayCost {
        cost: AbilityCost::Mana { cost },
        scale: None,
        payer: TargetFilter::Controller,
    } = ability.effect.as_ref()
    else {
        return false;
    };
    cost.mana_value() > 0
        && !ability.optional
        && ability.sub_ability.as_deref().is_some_and(|failure| {
            !failure.optional
                && failure
                    .condition
                    .as_ref()
                    .is_some_and(AbilityCondition::is_not_optional_effect_performed)
                && matches!(
                    failure.effect.as_ref(),
                    Effect::LoseTheGame {
                        target: None | Some(TargetFilter::Controller)
                    }
                )
        })
}

/// Drive only deterministic turn progression after the exact delayed trigger
/// is installed. The engine owns Pact's resolution-time mana payment: it
/// auto-taps legal sources and either completes the mandatory payment or takes
/// the loss branch without exposing a `ManaPayment` interaction to the AI.
fn pact_payment_survives(
    state: &mut GameState,
    owner: PlayerId,
    receipt: PactReceipt,
    budget: &mut ProspectiveBudget,
) -> bool {
    for _ in 0..PROSPECTIVE_MAX_PACT_TRANSITIONS {
        if let WaitingFor::GameOver { winner } = &state.waiting_for {
            // CR 104.2: a completed game is a successful prospective terminal
            // only when the Pact controller won; opponent wins and draws do
            // not prove the obligation's safe progression.
            return *winner == Some(owner);
        }
        if state.pending_trigger_order.as_ref().is_some_and(|order| {
            order
                .groups
                .iter()
                .any(|group| group.controller != owner && group.triggers.len() != 1)
        }) {
            // CR 603.3b: preserve the whole scheduler-owned ordering batch,
            // not merely the currently displayed group. A competing opponent
            // group is a strategic choice even while another group is prompted.
            return false;
        }
        match &state.waiting_for {
            WaitingFor::Priority { player } => {
                let Some(outcome) = advance_priority_for_route(state, owner, *player) else {
                    return false;
                };
                if outcome.receipt_finished_normally(receipt.provenance)
                    && pact_payment_completed(state, owner, receipt)
                {
                    return true;
                }
                if outcome.receipt_terminalized(receipt.provenance) {
                    return false;
                }
            }
            WaitingFor::OrderTriggers { player, .. } => {
                // CR 603.3b: a prospective certificate may advance an
                // ordering prompt only when the reducer exposes one forced
                // ordering. This applies to the Pact controller as well as
                // opponents: choosing either order would otherwise make the
                // certificate responsible for a strategic decision.
                let order_candidates: Vec<_> = frozen_candidates(state, *player)
                    .into_iter()
                    .filter(|candidate| {
                        matches!(candidate.action, GameAction::OrderTriggers { .. })
                    })
                    .collect();
                if order_candidates.len() != 1 {
                    return false;
                }
                let Some(action) = FrozenCandidate::capture(state, &order_candidates[0]) else {
                    return false;
                };
                let Some(permit) = budget.issue_forced_progress(action) else {
                    return false;
                };
                let Ok(outcome) = permit.apply(state) else {
                    return false;
                };
                if outcome.receipt_finished_normally(receipt.provenance)
                    && pact_payment_completed(state, owner, receipt)
                {
                    return true;
                }
                if outcome.receipt_terminalized(receipt.provenance) {
                    return false;
                }
            }
            WaitingFor::DeclareAttackers { .. } | WaitingFor::DeclareBlockers { .. } => {
                let Some(player) = state.waiting_for.acting_player() else {
                    return false;
                };
                let candidates = frozen_candidates(state, player);
                let candidate_count = candidates.len();
                let neutral_actions: Vec<_> = candidates
                    .into_iter()
                    .filter(|candidate| match &candidate.action {
                        GameAction::OrderTriggers { .. } => true,
                        GameAction::DeclareAttackers { attacks, .. } => attacks.is_empty(),
                        GameAction::DeclareBlockers { assignments } => assignments.is_empty(),
                        _ => false,
                    })
                    .collect();
                // An opponent's trigger order, attack, or block is never a
                // prospective-route choice. Advance only when its one legal
                // candidate is the neutral empty/identity action.
                if player != owner
                    && (neutral_actions.len() != 1 || neutral_actions.len() != candidate_count)
                {
                    return false;
                }
                let Some(action) = neutral_actions
                    .into_iter()
                    .next()
                    .and_then(|candidate| FrozenCandidate::capture(state, &candidate))
                else {
                    return false;
                };
                let Ok(outcome) = action.apply_with_lifecycle(state) else {
                    return false;
                };
                if outcome.receipt_finished_normally(receipt.provenance)
                    && pact_payment_completed(state, owner, receipt)
                {
                    return true;
                }
                if outcome.receipt_terminalized(receipt.provenance) {
                    return false;
                }
            }
            _ => {
                return false;
            }
        }
    }
    false
}

/// CR 603.3b + CR 608.2c: The receipt may temporarily leave every provenance
/// carrier while trigger ordering is being assembled. Treat it as paid only
/// once its upkeep trigger has left the stack and reducer control has returned
/// to priority.
fn pact_payment_completed(state: &GameState, owner: PlayerId, receipt: PactReceipt) -> bool {
    state.active_player == owner
        && state.phase == crate::types::phase::Phase::Upkeep
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
        && !state
            .stack
            .iter()
            .any(|entry| entry.source_id == receipt.source_id)
}

/// Advance a real priority pass during a reducer-certified prospective route.
/// The route owner may deliberately choose their own ordinary pass through the
/// existing candidate/boundary path. A non-owner's pass is never candidate
/// forced: it must be admitted by the engine's complete actionless preflight.
fn advance_priority_for_route(
    state: &mut GameState,
    route_owner: PlayerId,
    holder: PlayerId,
) -> Option<ProspectiveSimulationOutcome> {
    if holder != route_owner {
        return apply_actionless_priority_pass_for_prospective(state).ok();
    }
    let pass = frozen_candidates(state, holder)
        .into_iter()
        .find(|candidate| matches!(candidate.action, GameAction::PassPriority))?;
    FrozenCandidate::capture(state, &pass)?
        .apply_with_lifecycle(state)
        .ok()
}

fn advance_pact_to_install(
    state: &mut GameState,
    owner: PlayerId,
    source_id: ObjectId,
    budget: &mut ProspectiveBudget,
) -> Option<PactReceipt> {
    let mut priority_beats = 0;
    while priority_beats < PROSPECTIVE_MAX_PRIORITY_BEATS {
        match &state.waiting_for {
            WaitingFor::Priority { player } => {
                let outcome = advance_priority_for_route(state, owner, *player)?;
                if draws_are_opaque(state, &outcome.action.events) {
                    return None;
                }
                if let Some(receipt) = receipt_installed_by(state, source_id, owner, &outcome) {
                    return Some(receipt);
                }
                priority_beats += 1;
            }
            waiting if waiting.acting_player() == Some(owner) => {
                let mut continuations =
                    frozen_candidates(state, owner)
                        .into_iter()
                        .filter(|candidate| {
                            matches!(
                                candidate.metadata.tactical_class,
                                TacticalClass::Target | TacticalClass::Selection
                            )
                        });
                let continuation = continuations.next()?;
                if continuations.next().is_some() {
                    return None;
                }
                let continuation = FrozenCandidate::capture(state, &continuation)?;
                let permit = budget.issue_forced_progress(continuation)?;
                let outcome = permit.apply(state).ok()?;
                if draws_are_opaque(state, &outcome.action.events) {
                    return None;
                }
                if let Some(receipt) = receipt_installed_by(state, source_id, owner, &outcome) {
                    return Some(receipt);
                }
            }
            _ => return None,
        }
    }
    None
}

fn receipt_installed_by(
    state: &GameState,
    source_id: ObjectId,
    payer: PlayerId,
    outcome: &ProspectiveSimulationOutcome,
) -> Option<PactReceipt> {
    if !outcome.has_outer_lifecycle_facts() {
        return None;
    }
    outcome.delayed_installations().find_map(
        |(provenance, installed_source, installed_controller)| {
            state
                .delayed_triggers
                .iter()
                .find(|trigger| {
                    trigger.provenance.origin() == Some(provenance)
                        && trigger.source_id == source_id
                        && trigger.controller == payer
                        && trigger.one_shot
                        && matches!(
                            &trigger.condition,
                            DelayedTriggerCondition::AtNextPhaseForPlayer {
                                phase: crate::types::phase::Phase::Upkeep,
                                player,
                                ..
                            } if *player == payer
                        )
                        && delayed_trigger_is_mandatory_mana_payment_or_loss(&trigger.ability)
                })
                .filter(|_| installed_source == source_id && installed_controller == payer)
                .map(|_| PactReceipt {
                    provenance,
                    payer,
                    source_id,
                })
        },
    )
}

fn delayed_trigger_is_mandatory_mana_payment_or_loss(
    ability: &crate::types::ability::ResolvedAbility,
) -> bool {
    let Effect::PayCost {
        cost: AbilityCost::Mana { cost },
        scale: None,
        payer: TargetFilter::Controller,
    } = &ability.effect
    else {
        return false;
    };
    cost.mana_value() > 0
        && !ability.optional
        && ability.sub_ability.as_deref().is_some_and(|failure| {
            !failure.optional
                && failure
                    .condition
                    .as_ref()
                    .is_some_and(AbilityCondition::is_not_optional_effect_performed)
                && matches!(
                    &failure.effect,
                    Effect::LoseTheGame {
                        target: None | Some(TargetFilter::Controller)
                    }
                )
        })
}

impl CertifiedFetchFollowUp {
    /// Return the certified spell only on the exact state produced by the
    /// certified real search selection. A stale or modified state falls back
    /// to ordinary action selection.
    pub fn action_for(&self, state: &GameState, semantic_owner: PlayerId) -> Option<GameAction> {
        if self.owner != semantic_owner || !self.capability.matches(state) {
            return None;
        }
        validated_candidate_actions_for_semantic_owner(state, semantic_owner)
            .into_iter()
            .find(|candidate| {
                matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == self.cast.reference.object_id)
                    && candidate.metadata.tactical_class == TacticalClass::Spell
            })
            .map(|candidate| candidate.action)
    }
}

/// Opaque, decision-scoped reducer-attempt allowance for prospective routes.
/// Consumers may retain and pass this handle across root/candidate fallback
/// work, but only this module can inspect or replenish its counters.
#[derive(Debug)]
struct ProspectiveManaDecision {
    budget: ProspectiveBudget,
    search_branches_remaining: usize,
}

const PROSPECTIVE_MAX_STRATEGIC_ACTIONS: usize = 2;
const PROSPECTIVE_MAX_PRIORITY_BEATS: usize = 2;
const PROSPECTIVE_MAX_PACT_TRANSITIONS: usize = 96;
const PROSPECTIVE_MAX_PAYMENT_APPLICATIONS: usize = 64;
const PROSPECTIVE_MAX_SEARCH_BRANCHES: usize = 12;
// Only a fully bound, deterministic continuation can consume this allowance.
const PROSPECTIVE_MAX_FORCED_TRANSITIONS: usize = 16;

impl Default for ProspectiveManaDecision {
    fn default() -> Self {
        Self {
            budget: ProspectiveBudget::default(),
            search_branches_remaining: PROSPECTIVE_MAX_SEARCH_BRANCHES,
        }
    }
}

#[derive(Debug)]
struct ProspectiveBudget {
    strategic_actions_remaining: usize,
    priority_beats_remaining: usize,
    payment_applications_remaining: usize,
    forced_transitions_remaining: usize,
}

/// A move-only grant to apply one already captured deterministic continuation.
/// It is issued only after candidate selection has proved that no player choice
/// remains; the grant owns that exact capability and burns its budget on issue.
#[derive(Debug)]
struct ForcedProgressPermit {
    continuation: FrozenCandidate,
}

impl Default for ProspectiveBudget {
    fn default() -> Self {
        Self {
            strategic_actions_remaining: PROSPECTIVE_MAX_STRATEGIC_ACTIONS,
            priority_beats_remaining: PROSPECTIVE_MAX_PRIORITY_BEATS,
            payment_applications_remaining: PROSPECTIVE_MAX_PAYMENT_APPLICATIONS,
            forced_transitions_remaining: PROSPECTIVE_MAX_FORCED_TRANSITIONS,
        }
    }
}

impl ProspectiveBudget {
    fn issue_forced_progress(
        &mut self,
        continuation: FrozenCandidate,
    ) -> Option<ForcedProgressPermit> {
        if self.forced_transitions_remaining == 0 {
            return None;
        }
        self.forced_transitions_remaining -= 1;
        Some(ForcedProgressPermit { continuation })
    }
}

impl ForcedProgressPermit {
    fn apply(self, state: &mut GameState) -> Result<ProspectiveSimulationOutcome, ()> {
        self.continuation.apply_with_lifecycle(state)
    }
}

impl ProspectiveManaDecision {
    /// Reserve one mutually exclusive terminal-evaluation branch. The route
    /// allowance is copied only after the shared root has been precharged, so
    /// a failed counterfactual cannot consume a sibling route's cast action.
    /// The separate enumeration allowance remains decision-scoped.
    fn terminal_branch_budget(&mut self) -> Option<ProspectiveBudget> {
        if self.search_branches_remaining == 0 {
            return None;
        }
        self.search_branches_remaining -= 1;
        Some(ProspectiveBudget {
            strategic_actions_remaining: self.budget.strategic_actions_remaining,
            priority_beats_remaining: self.budget.priority_beats_remaining,
            payment_applications_remaining: self.budget.payment_applications_remaining,
            forced_transitions_remaining: self.budget.forced_transitions_remaining,
        })
    }
}

/// Full trusted interaction capability, not a redacted display projection.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StateCapabilityBinding {
    state_revision: u64,
    interaction_session_id: Option<InteractionSessionId>,
    interaction_generation: u64,
    active_interaction_slots: Vec<ActiveInteractionSlot>,
    waiting_for: WaitingFor,
    semantic_owner: PlayerId,
}

impl StateCapabilityBinding {
    fn capture(state: &GameState, semantic_owner: PlayerId) -> Option<Self> {
        Some(Self {
            state_revision: state.state_revision,
            interaction_session_id: state.interaction_session_id.clone(),
            interaction_generation: state.interaction_generation,
            active_interaction_slots: state.active_interaction_slots.clone(),
            waiting_for: state.waiting_for.clone(),
            semantic_owner,
        })
    }

    fn matches(&self, state: &GameState) -> bool {
        state.state_revision == self.state_revision
            && state.interaction_session_id == self.interaction_session_id
            && state.interaction_generation == self.interaction_generation
            && state.active_interaction_slots == self.active_interaction_slots
            && state.waiting_for == self.waiting_for
    }
}

#[derive(Debug, Clone)]
struct FrozenCandidate {
    action: GameAction,
    semantic_owner: PlayerId,
    authenticated_actor: PlayerId,
    tactical_class: TacticalClass,
    capability: StateCapabilityBinding,
}

impl FrozenCandidate {
    fn capture(state: &GameState, candidate: &CandidateAction) -> Option<Self> {
        let semantic_owner = candidate.metadata.semantic_owner?;
        let authenticated_actor = candidate.metadata.actor?;
        let capability = StateCapabilityBinding::capture(state, semantic_owner)?;
        Some(Self {
            action: candidate.action.clone(),
            semantic_owner,
            authenticated_actor,
            tactical_class: candidate.metadata.tactical_class,
            capability,
        })
    }

    fn matches_candidate(&self, candidate: &CandidateAction) -> bool {
        candidate.action.cmp_stable(&self.action) == Ordering::Equal
            && candidate.metadata.semantic_owner == Some(self.semantic_owner)
            && candidate.metadata.actor == Some(self.authenticated_actor)
            && candidate.metadata.tactical_class == self.tactical_class
    }

    fn apply(self, state: &mut GameState) -> Result<Vec<GameEvent>, ()> {
        if !self.capability.matches(state) {
            return Err(());
        }
        let current = frozen_candidates(state, self.semantic_owner);
        if !current
            .iter()
            .any(|candidate| self.matches_candidate(candidate))
        {
            return Err(());
        }
        apply_interaction_for_simulation(
            state,
            self.authenticated_actor,
            self.semantic_owner,
            self.action,
        )
        .map(|result| result.events)
        .map_err(|_| ())
    }

    fn apply_with_lifecycle(
        self,
        state: &mut GameState,
    ) -> Result<ProspectiveSimulationOutcome, ()> {
        if !self.capability.matches(state) {
            return Err(());
        }
        let current = frozen_candidates(state, self.semantic_owner);
        if !current
            .iter()
            .any(|candidate| self.matches_candidate(candidate))
        {
            return Err(());
        }
        apply_interaction_for_prospective_simulation(
            state,
            self.authenticated_actor,
            self.semantic_owner,
            self.action,
        )
        .map_err(|_| ())
    }
}

#[derive(Debug, Clone)]
struct SearchBinding {
    player: PlayerId,
    library_owner: Option<PlayerId>,
    cards: Vec<ObjectIncarnationRef>,
    count: usize,
    reveal: bool,
    up_to: bool,
    allows_partial_find: bool,
    constraint: SearchSelectionConstraint,
    split: Option<crate::types::ability::SearchDestinationSplit>,
}

impl SearchBinding {
    fn capture(state: &GameState) -> Option<Self> {
        let WaitingFor::SearchChoice {
            player,
            library_owner,
            cards,
            count,
            reveal,
            up_to,
            allows_partial_find,
            constraint,
            split,
            ..
        } = &state.waiting_for
        else {
            return None;
        };
        Some(Self {
            player: *player,
            library_owner: *library_owner,
            cards: cards
                .iter()
                .filter_map(|id| state.objects.get(id).map(ObjectIncarnationRef::from_object))
                .collect(),
            count: *count,
            reveal: *reveal,
            up_to: *up_to,
            allows_partial_find: *allows_partial_find,
            constraint: constraint.clone(),
            split: split.clone(),
        })
    }

    fn matches(&self, state: &GameState) -> bool {
        let Some(other) = Self::capture(state) else {
            return false;
        };
        self.player == other.player
            && self.library_owner == other.library_owner
            && self.cards == other.cards
            && self.count == other.count
            && self.reveal == other.reveal
            && self.up_to == other.up_to
            && self.allows_partial_find == other.allows_partial_find
            && self.constraint == other.constraint
            && self.split == other.split
    }
}

/// Start the fetch half of the route.  A successful fetch resolution produces
/// a real `SearchChoiceAtPrompt`; callers must submit the actual selection via
/// [`CertifiedFetchPrompt::action_for`].
fn prospect_fetch_then_cast(
    decision: &mut ProspectiveManaDecision,
    state: &GameState,
    request: &FetchThenCastRequest,
) -> ProspectiveManaResult {
    if !is_exact_fetchland_candidate(state, &request.fetch) || !binding_matches(state, request.cast)
    {
        return ProspectiveManaResult::Indeterminate;
    }
    let Some(root) = FrozenCandidate::capture(state, &request.fetch) else {
        return ProspectiveManaResult::Indeterminate;
    };
    if decision.budget.strategic_actions_remaining == 0 {
        return ProspectiveManaResult::Indeterminate;
    }
    decision.budget.strategic_actions_remaining -= 1;
    let owner = root.semantic_owner;
    let mut clone = state.clone();
    let Ok(events) = root.apply(&mut clone) else {
        return ProspectiveManaResult::Indeterminate;
    };
    if draws_are_opaque(&clone, &events) {
        return ProspectiveManaResult::Indeterminate;
    }
    advance_fetch_to_search(&mut clone, owner, &mut decision.budget)
}

/// Evaluate every legal one-card choice at a certified fetch prompt without
/// exposing the clone that proves the route.  The scorer receives only the
/// ephemeral terminal state and the exact selection candidate that produced
/// it; neither can be retained or mutated by this engine-owned traversal.
///
/// The result is intentionally a draft rather than a continuation: callers
/// may use its action and score to rank the current public decision, but they
/// cannot resume the simulated route or reset its private budget.
pub fn certify_fetch_then_cast(
    state: &GameState,
    fetch: &CandidateAction,
    casts: &[ObjectIdentityBinding],
    mut score_terminal: impl FnMut(&GameState, &ObjectIdentityBinding) -> f64,
) -> Option<(CertifiedFetchPrompt, f64)> {
    let mut decision = ProspectiveManaDecision::default();
    let seed_cast = casts
        .iter()
        .copied()
        .find(|cast| binding_matches(state, *cast))?;
    let ProspectiveManaResult::Contingent(continuation) = prospect_fetch_then_cast(
        &mut decision,
        state,
        &FetchThenCastRequest {
            fetch: fetch.clone(),
            cast: seed_cast,
        },
    ) else {
        return None;
    };
    let continuation = *continuation;
    let actor = authorized_submitter_for_player(&continuation.state, continuation.owner);
    let mut best: Option<(CertifiedFetchPrompt, f64)> = None;

    for binding in &continuation.search.cards {
        let action = GameAction::SelectCards {
            cards: vec![binding.object_id],
        };
        for &cast in casts {
            let Some(mut branch_budget) = decision.terminal_branch_budget() else {
                return best;
            };
            let mut clone = continuation.state.clone();
            let Ok(result) = apply_interaction_for_simulation(
                &mut clone,
                actor,
                continuation.owner,
                action.clone(),
            ) else {
                continue;
            };
            if draws_are_opaque(&clone, &result.events) {
                continue;
            }
            let Some(fetched) = clone.objects.get(&binding.object_id) else {
                continue;
            };
            if fetched.zone != Zone::Battlefield
                || ObjectIncarnationRef::from_object(fetched) == *binding
            {
                continue;
            }
            let Some(follow_up_capability) =
                StateCapabilityBinding::capture(&clone, continuation.owner)
            else {
                continue;
            };
            let Some(terminal) =
                cast_bound_card_terminal(&mut clone, continuation.owner, cast, &mut branch_budget)
            else {
                continue;
            };
            let score = score_terminal(&terminal, &cast);
            if best
                .as_ref()
                .is_none_or(|(_, best_score)| score > *best_score)
            {
                best = Some((
                    CertifiedFetchPrompt {
                        capability: continuation.capability.clone(),
                        search: continuation.search.clone(),
                        owner: continuation.owner,
                        selection: action.clone(),
                        follow_up: CertifiedFetchFollowUp {
                            capability: follow_up_capability,
                            owner: continuation.owner,
                            cast,
                        },
                    },
                    score,
                ));
            }
        }
    }
    best
}

fn is_exact_fetchland_candidate(state: &GameState, candidate: &CandidateAction) -> bool {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = candidate.action
    else {
        return false;
    };
    let Some(owner) = candidate.metadata.semantic_owner else {
        return false;
    };
    let Some(actor) = candidate.metadata.actor else {
        return false;
    };
    if actor != authorized_submitter_for_player(state, owner) {
        return false;
    }
    let Some(source) = state.objects.get(&source_id) else {
        return false;
    };
    if source.zone != Zone::Battlefield
        || source.owner != owner
        || source.controller != owner
        || !source
            .card_types
            .core_types
            .contains(&crate::types::card_type::CoreType::Land)
    {
        return false;
    }
    let Some(ability) = source.abilities.get(ability_index) else {
        return false;
    };
    ability.kind == AbilityKind::Activated
        && has_exact_self_sacrifice_cost(ability.cost.as_ref())
        && has_exact_fetch_effect_chain(ability)
}

fn has_exact_self_sacrifice_cost(cost: Option<&AbilityCost>) -> bool {
    fn visit(cost: &AbilityCost, sacrifices: &mut usize) -> bool {
        match cost {
            AbilityCost::Composite { costs } => costs.iter().all(|cost| visit(cost, sacrifices)),
            AbilityCost::Tap | AbilityCost::PayLife { .. } => true,
            AbilityCost::Sacrifice(sacrifice) => {
                if sacrifice.target != TargetFilter::SelfRef
                    || sacrifice.requirement != SacrificeRequirement::count(1)
                {
                    return false;
                }
                *sacrifices += 1;
                true
            }
            _ => false,
        }
    }

    let Some(cost) = cost else {
        return false;
    };
    let mut sacrifices = 0;
    visit(cost, &mut sacrifices) && sacrifices == 1
}

fn has_exact_fetch_effect_chain(ability: &crate::types::ability::AbilityDefinition) -> bool {
    let mut chain = std::iter::successors(Some(ability), |current| current.sub_ability.as_deref());
    let Some(search) = chain.next() else {
        return false;
    };
    let Effect::SearchLibrary {
        source_zones,
        count: QuantityExpr::Fixed { value: 1 },
        target_player: None,
        selection_constraint: SearchSelectionConstraint::None,
        split: None,
        ..
    } = &*search.effect
    else {
        return false;
    };
    if source_zones.as_slice() != [Zone::Library] {
        return false;
    }
    let Some(change) = chain.next() else {
        return false;
    };
    if !matches!(
        &*change.effect,
        Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Battlefield,
            up_to: false,
            ..
        }
    ) {
        return false;
    }
    chain.all(|step| matches!(&*step.effect, Effect::Shuffle { .. }))
}

fn advance_fetch_to_search(
    state: &mut GameState,
    owner: PlayerId,
    budget: &mut ProspectiveBudget,
) -> ProspectiveManaResult {
    loop {
        if let Some(search) = SearchBinding::capture(state) {
            if search.player != owner
                || search.library_owner != Some(owner)
                || search.count != 1
                || search.up_to
                || search.constraint != SearchSelectionConstraint::None
                || search.split.is_some()
            {
                return ProspectiveManaResult::Indeterminate;
            }
            let Some(capability) = StateCapabilityBinding::capture(state, owner) else {
                return ProspectiveManaResult::Indeterminate;
            };
            return ProspectiveManaResult::Contingent(Box::new(SearchChoiceAtPrompt {
                state: state.clone(),
                capability,
                search,
                owner,
            }));
        }
        let holder = match &state.waiting_for {
            WaitingFor::Priority { player } => *player,
            _ => return ProspectiveManaResult::Indeterminate,
        };
        if budget.priority_beats_remaining == 0 {
            return ProspectiveManaResult::Indeterminate;
        }
        let Some(outcome) = advance_priority_for_route(state, owner, holder) else {
            return ProspectiveManaResult::Indeterminate;
        };
        budget.priority_beats_remaining -= 1;
        if draws_are_opaque(state, &outcome.action.events) {
            return ProspectiveManaResult::Indeterminate;
        }
    }
}

/// Terminal-only sibling of [`cast_bound_card`].  Keeping this in the engine
/// is important: phase-AI never receives a mutable simulation state and cannot
/// accidentally reuse a payment branch as a second prospective root.
fn cast_bound_card_terminal(
    state: &mut GameState,
    owner: PlayerId,
    cast: ObjectIdentityBinding,
    budget: &mut ProspectiveBudget,
) -> Option<GameState> {
    if budget.strategic_actions_remaining == 0 || !binding_matches(state, cast) {
        return None;
    }
    let candidate = frozen_candidates(state, owner).into_iter().find(|candidate| {
        matches!(candidate.action, GameAction::CastSpell { object_id, .. } if object_id == cast.reference.object_id)
            && candidate.metadata.tactical_class == TacticalClass::Spell
    })?;
    let frozen = FrozenCandidate::capture(state, &candidate)?;
    budget.strategic_actions_remaining -= 1;
    let events = frozen.apply(state).ok()?;
    if cast_committed(&events, cast, owner) {
        return Some(state.clone());
    }
    finish_payment_terminal(state, owner, cast, budget)
}

fn finish_payment_terminal(
    state: &mut GameState,
    owner: PlayerId,
    cast: ObjectIdentityBinding,
    budget: &mut ProspectiveBudget,
) -> Option<GameState> {
    if budget.payment_applications_remaining == 0 {
        return None;
    }
    let mana: Vec<_> = frozen_candidates(state, owner)
        .into_iter()
        .filter(|candidate| candidate.metadata.tactical_class == TacticalClass::Mana)
        .collect();
    for candidate in mana {
        let frozen = FrozenCandidate::capture(state, &candidate)?;
        let mut next = state.clone();
        budget.payment_applications_remaining -= 1;
        let Ok(events) = frozen.apply(&mut next) else {
            continue;
        };
        if draws_are_opaque(&next, &events) {
            continue;
        }
        if cast_committed(&events, cast, owner) {
            return Some(next);
        }
        if let Some(terminal) = finish_payment_terminal(&mut next, owner, cast, budget) {
            return Some(terminal);
        }
    }
    None
}

fn frozen_candidates(state: &GameState, owner: PlayerId) -> Vec<CandidateAction> {
    let mut candidates = validated_candidate_actions_for_semantic_owner(state, owner);
    candidates.sort_by(|left, right| {
        left.action
            .cmp_stable(&right.action)
            .then_with(|| {
                left.metadata
                    .semantic_owner
                    .cmp(&right.metadata.semantic_owner)
            })
            .then_with(|| left.metadata.actor.cmp(&right.metadata.actor))
            .then_with(|| {
                (left.metadata.tactical_class as u8).cmp(&(right.metadata.tactical_class as u8))
            })
    });
    candidates.dedup_by(|left, right| {
        left.action.cmp_stable(&right.action) == Ordering::Equal
            && left.metadata.semantic_owner == right.metadata.semantic_owner
            && left.metadata.actor == right.metadata.actor
            && left.metadata.tactical_class == right.metadata.tactical_class
    });
    candidates
}

fn binding_matches(state: &GameState, binding: ObjectIdentityBinding) -> bool {
    state
        .objects
        .get(&binding.reference.object_id)
        .is_some_and(|object| {
            ObjectIncarnationRef::from_object(object) == binding.reference
                && object.zone == binding.expected_zone
        })
}

fn cast_committed(events: &[GameEvent], binding: ObjectIdentityBinding, owner: PlayerId) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            GameEvent::SpellCast { object_id, controller, .. }
                if *object_id == binding.reference.object_id && *controller == owner
        )
    })
}

fn draws_are_opaque(state: &GameState, events: &[GameEvent]) -> bool {
    let summaries: u32 = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsDrawn { count, .. } => Some(*count),
            _ => None,
        })
        .sum();
    let identities = events.iter().filter(|event| matches!(event, GameEvent::CardDrawn { object_id, .. } if state.objects.contains_key(object_id))).count() as u32;
    summaries > identities
}

#[cfg(test)]
mod tests {
    use super::{
        advance_priority_for_route, CertifiedPactPlan, FrozenCandidate, PactPlanState, PactReceipt,
        ProspectiveBudget, ProspectiveManaDecision, StateCapabilityBinding,
        PROSPECTIVE_MAX_SEARCH_BRANCHES,
    };
    use crate::ai_support::TacticalClass;
    use crate::game::engine::apply_actionless_priority_pass_for_prospective;
    use crate::game::triggers::{PendingTrigger, PendingTriggerContext};
    use crate::game::zones;
    use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::identifiers::{
        CardId, DelayedTriggerInstanceId, DelayedTriggerOrigin, DelayedTriggerToken, ObjectId,
    };
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn terminal_search_branches_share_the_precharged_route_allowance_but_not_each_other() {
        let mut decision = ProspectiveManaDecision::default();
        decision.budget.strategic_actions_remaining = 1;

        let mut failed_branch = decision
            .terminal_branch_budget()
            .expect("the first terminal branch must be admitted");
        failed_branch.strategic_actions_remaining = 0;
        assert_eq!(failed_branch.strategic_actions_remaining, 0);
        let succeeding_branch = decision
            .terminal_branch_budget()
            .expect("a failed sibling must not consume this branch's route allowance");

        assert_eq!(succeeding_branch.strategic_actions_remaining, 1);
        assert_eq!(decision.budget.strategic_actions_remaining, 1);

        for _ in 2..PROSPECTIVE_MAX_SEARCH_BRANCHES {
            assert!(decision.terminal_branch_budget().is_some());
        }
        assert!(decision.terminal_branch_budget().is_none());
    }

    #[test]
    fn forced_progress_permit_burns_its_budget_before_a_stale_apply_fails() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        let candidate = |state: &GameState| FrozenCandidate {
            action: GameAction::PassPriority,
            semantic_owner: player,
            authenticated_actor: player,
            tactical_class: TacticalClass::Selection,
            capability: StateCapabilityBinding::capture(state, player)
                .expect("a complete state yields a capability binding"),
        };
        let mut budget = ProspectiveBudget {
            forced_transitions_remaining: 1,
            ..Default::default()
        };

        let permit = budget
            .issue_forced_progress(candidate(&state))
            .expect("the final allowance issues one bound permit");
        assert_eq!(budget.forced_transitions_remaining, 0);
        state.state_revision += 1;

        assert!(
            permit.apply(&mut state).is_err(),
            "a capability change blocks the already-issued continuation"
        );
        assert!(
            budget.issue_forced_progress(candidate(&state)).is_none(),
            "a failed post-issue apply still burns the one forced-transition allowance"
        );
    }

    #[test]
    fn actionless_pact_priority_pass_uses_the_normal_interaction_boundary() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        let outcome = apply_actionless_priority_pass_for_prospective(&mut state)
            .expect("an actionless Priority window can take its normal pass route");

        assert!(matches!(
            outcome.action.waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_ne!(state.priority_player, player);
    }

    #[test]
    fn pact_plan_remains_dormant_while_matching_trigger_is_deferred() {
        let payer = PlayerId(0);
        let source = ObjectId(99);
        let provenance = DelayedTriggerOrigin {
            token: DelayedTriggerToken(7),
            instance: DelayedTriggerInstanceId(11),
            source_id: source,
        };
        let mut state = GameState::new_two_player(42);
        let plan = CertifiedPactPlan {
            root: FrozenCandidate {
                action: GameAction::PassPriority,
                semantic_owner: payer,
                authenticated_actor: payer,
                tactical_class: TacticalClass::Selection,
                capability: StateCapabilityBinding::capture(&state, payer)
                    .expect("a complete state yields a capability binding"),
            },
            receipt: PactReceipt {
                provenance,
                payer,
                source_id: source,
            },
        };
        let pending = PendingTrigger::ordinary(
            source,
            payer,
            None,
            Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                source,
                payer,
            )),
            state.turn_number,
        );
        state
            .deferred_triggers
            .push(PendingTriggerContext::delayed_for_test(pending, provenance));

        assert_eq!(plan.state_for(&state, payer), PactPlanState::Dormant);
    }

    #[test]
    fn route_owner_may_submit_its_own_normal_pass_without_forcing_an_opponent() {
        let owner = PlayerId(0);
        let opponent = PlayerId(1);
        let mut state = GameState::new_two_player(42);
        state.active_player = owner;
        state.priority_player = owner;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: owner };
        let land = zones::create_object(
            &mut state,
            CardId(1),
            owner,
            "Island".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .expect("new hand object exists")
            .card_types
            .core_types
            .push(CoreType::Land);
        state.players[owner.0 as usize].hand.push_back(land);

        let outcome = advance_priority_for_route(&mut state, owner, owner)
            .expect("the route owner may make its normal PassPriority decision");

        assert!(matches!(
            outcome.action.waiting_for,
            WaitingFor::Priority { player } if player == opponent
        ));
        assert_eq!(state.priority_player, opponent);
    }

    #[test]
    fn route_never_forces_an_opponents_actionable_priority_window() {
        let owner = PlayerId(1);
        let opponent = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.active_player = opponent;
        state.priority_player = opponent;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: opponent };
        let land = zones::create_object(
            &mut state,
            CardId(1),
            opponent,
            "Island".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land)
            .expect("new hand object exists")
            .card_types
            .core_types
            .push(CoreType::Land);
        state.players[opponent.0 as usize].hand.push_back(land);
        let before = state.clone();

        assert!(advance_priority_for_route(&mut state, owner, opponent).is_none());
        assert_eq!(state, before);
    }
}
