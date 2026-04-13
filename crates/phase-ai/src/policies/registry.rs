use std::collections::HashMap;

use super::aggro_pressure::AggroPressurePolicy;
use super::anthem_priority::AnthemPriorityPolicy;
use super::anti_self_harm::AntiSelfHarmPolicy;
use super::board_development::BoardDevelopmentPolicy;
use super::board_wipe_telegraph::BoardWipeTelegraphPolicy;
use super::card_advantage::CardAdvantagePolicy;
use super::context::PolicyContext;
use super::copy_value::CopyValuePolicy;
use super::effect_timing::EffectTimingPolicy;
use super::etb_value::EtbValuePolicy;
use super::evasion_removal_priority::EvasionRemovalPriorityPolicy;
use super::free_outlet_activation::FreeOutletActivationPolicy;
use super::hand_disruption::HandDisruptionPolicy;
use super::hold_mana_up::HoldManaUpForInteractionPolicy;
use super::interaction_reservation::InteractionReservationPolicy;
use super::landfall_timing::LandfallTimingPolicy;
use super::lethality_awareness::LethalityAwarenessPolicy;
use super::life_total_resource::LifeTotalResourcePolicy;
use super::plus_one_counters::PlusOneCountersPolicy;
use super::ramp_timing::RampTimingPolicy;
use super::recursion_awareness::RecursionAwarenessPolicy;
use super::sacrifice_value::SacrificeValuePolicy;
use super::sweeper_timing::SweeperTimingPolicy;
use super::tokens_wide::TokensWidePolicy;
use super::tribal_lord_priority::TribalLordPriorityPolicy;
use super::tutor::TutorPolicy;
use crate::cast_facts::cast_facts_for_action;
use crate::config::AiConfig;
use crate::decision_kind::classify as classify_decision;
use crate::features::DeckFeatures;
use crate::planner::PolicyPrior;
use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

/// Stable identity for a `TacticalPolicy` implementation. One variant per
/// implementation — no `Legacy` catch-all, no string IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicyId {
    AntiSelfHarm,
    BoardDevelopment,
    EtbValue,
    CopyValue,
    Tutor,
    HandDisruption,
    InteractionReservation,
    EffectTiming,
    ManaEfficiency,
    StackAwareness,
    DownsideAwareness,
    TempoCurve,
    SynergyCasting,
    LethalityAwareness,
    SacrificeValue,
    EvasionRemovalPriority,
    RecursionAwareness,
    BoardWipeTelegraph,
    LifeTotalResource,
    CardAdvantage,
    LandfallTiming,
    RampTiming,
    KeepablesByLandCount,
    LandfallKeepablesMulligan,
    RampKeepablesMulligan,
    TribalLordPriority,
    TribalDensityMulligan,
    HoldManaUpForInteraction,
    SweeperTiming,
    FreeOutletActivation,
    AristocratsKeepablesMulligan,
    AggroPressure,
    AggroKeepablesMulligan,
    TokensWide,
    AnthemPriority,
    TokensWideMulligan,
    PlusOneCountersTactical,
    PlusOneCountersMulligan,
}

/// Coarse routing kind for a candidate decision. Each policy declares which
/// kinds it fires for; the registry pre-builds a `HashMap<DecisionKind,
/// Vec<usize>>` and only invokes the relevant policies per candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionKind {
    Mulligan,
    PlayLand,
    CastSpell,
    ActivateAbility,
    ActivateManaAbility,
    SelectTarget,
    DeclareAttackers,
    DeclareBlockers,
    ManaPayment,
    ChooseX,
}

/// Structured reason emitted alongside every policy verdict — no freeform
/// strings. `kind` is a stable category identifier owned by each policy;
/// `facts` carries typed numeric context for observability.
#[derive(Debug, Clone)]
pub struct PolicyReason {
    pub kind: &'static str,
    pub facts: Vec<(&'static str, i64)>,
}

impl PolicyReason {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            facts: Vec::new(),
        }
    }

    pub fn with_fact(mut self, key: &'static str, value: i64) -> Self {
        self.facts.push((key, value));
        self
    }
}

/// A policy's verdict on a single candidate.
#[derive(Debug, Clone)]
pub enum PolicyVerdict {
    /// Hard veto — propagated to `tactical_gate::GateDecision::Reject`.
    Reject { reason: PolicyReason },
    /// Additive scalar contribution to the candidate's prior.
    Score { delta: f64, reason: PolicyReason },
}

/// The clean `TacticalPolicy` trait — four required methods, zero defaults.
///
/// Scaling discipline (CR-equivalent invariant for the AI layer):
/// 1. `decision_kinds()` filters which candidates this policy ever sees.
/// 2. `activation()` returns the single multiplicative knob.
///    `None` = opt out; `Some(x)` multiplies the verdict's `delta` by `x`.
/// 3. `verdict()` returns the policy's judgment on the current candidate.
///
/// The registry multiplies `delta * activation` exactly once. There is no
/// `score()` and no `archetype_scale()` — policies that need archetype- or
/// turn-sensitive weight compute it inside `activation()` from the inputs.
pub trait TacticalPolicy: Send + Sync {
    fn id(&self) -> PolicyId;

    fn decision_kinds(&self) -> &'static [DecisionKind];

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        player: PlayerId,
    ) -> Option<f32>;

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict;
}

pub struct PolicyRegistry {
    policies: Vec<Box<dyn TacticalPolicy>>,
    /// Per-`DecisionKind` index list — pre-built so candidate scoring iterates
    /// only the relevant policies.
    by_kind: HashMap<DecisionKind, Vec<usize>>,
}

impl Default for PolicyRegistry {
    fn default() -> Self {
        let policies: Vec<Box<dyn TacticalPolicy>> = vec![
            Box::new(AntiSelfHarmPolicy),
            Box::new(BoardDevelopmentPolicy),
            Box::new(EtbValuePolicy),
            Box::new(CopyValuePolicy),
            Box::new(TutorPolicy),
            Box::new(HandDisruptionPolicy),
            Box::new(InteractionReservationPolicy),
            Box::new(EffectTimingPolicy),
            Box::new(super::mana_efficiency::ManaEfficiencyPolicy),
            Box::new(super::stack_awareness::StackAwarenessPolicy),
            Box::new(super::downside_awareness::DownsideAwarenessPolicy),
            Box::new(super::tempo_curve::TempoCurvePolicy),
            Box::new(super::synergy_casting::SynergyCastingPolicy),
            Box::new(LethalityAwarenessPolicy),
            Box::new(SacrificeValuePolicy),
            Box::new(EvasionRemovalPriorityPolicy),
            Box::new(RecursionAwarenessPolicy),
            Box::new(BoardWipeTelegraphPolicy),
            Box::new(LifeTotalResourcePolicy),
            Box::new(CardAdvantagePolicy),
            Box::new(LandfallTimingPolicy),
            Box::new(RampTimingPolicy),
            Box::new(TribalLordPriorityPolicy),
            Box::new(HoldManaUpForInteractionPolicy),
            Box::new(SweeperTimingPolicy),
            Box::new(FreeOutletActivationPolicy),
            Box::new(AggroPressurePolicy),
            Box::new(TokensWidePolicy),
            Box::new(AnthemPriorityPolicy),
            Box::new(PlusOneCountersPolicy),
        ];
        let mut by_kind: HashMap<DecisionKind, Vec<usize>> = HashMap::new();
        for (idx, policy) in policies.iter().enumerate() {
            for kind in policy.decision_kinds() {
                by_kind.entry(*kind).or_default().push(idx);
            }
        }
        Self { policies, by_kind }
    }
}

impl PolicyRegistry {
    /// Run every policy whose `decision_kinds()` matches the classified kind
    /// for `ctx.candidate`, returning each policy's structured verdict.
    /// Used by `priors()` and (when tracing is enabled) for trace aggregation.
    pub fn verdicts(&self, ctx: &PolicyContext<'_>) -> Vec<(PolicyId, PolicyVerdict)> {
        let kind = classify_decision(&ctx.decision.waiting_for, &ctx.candidate.action);
        let Some(indices) = self.by_kind.get(&kind) else {
            return Vec::new();
        };
        let session_features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(indices.len());
        for &idx in indices {
            let policy = &self.policies[idx];
            let Some(activation) = policy.activation(&session_features, ctx.state, ctx.ai_player)
            else {
                continue;
            };
            let verdict = policy.verdict(ctx);
            let scaled = match verdict {
                PolicyVerdict::Reject { reason } => PolicyVerdict::Reject { reason },
                PolicyVerdict::Score { delta, reason } => PolicyVerdict::Score {
                    delta: delta * activation as f64,
                    reason,
                },
            };
            out.push((policy.id(), scaled));
        }
        out
    }

    /// Aggregate scaled verdicts into a single scalar — sum of all
    /// `Score { delta }` contributions. `Reject` verdicts surface as
    /// `f64::NEG_INFINITY` so the candidate is excluded by downstream
    /// softmax/argmax.
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let verdicts = self.verdicts(ctx);
        let mut total = 0.0;
        for (_id, verdict) in verdicts {
            match verdict {
                PolicyVerdict::Reject { .. } => return f64::NEG_INFINITY,
                PolicyVerdict::Score { delta, .. } => total += delta,
            }
        }
        total
    }

    pub fn priors(
        &self,
        state: &GameState,
        decision: &AiDecisionContext,
        candidates: &[CandidateAction],
        ai_player: PlayerId,
        config: &AiConfig,
        context: &crate::context::AiContext,
    ) -> Vec<PolicyPrior> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let raw_scores: Vec<f64> = candidates
            .iter()
            .map(|candidate| {
                let cast_facts = cast_facts_for_action(state, &candidate.action, ai_player);
                self.score(&PolicyContext {
                    state,
                    decision,
                    candidate,
                    ai_player,
                    config,
                    context,
                    cast_facts,
                })
            })
            .collect();
        let min_score = raw_scores
            .iter()
            .copied()
            .filter(|s| s.is_finite())
            .fold(f64::INFINITY, f64::min);
        let shifted: Vec<f64> = raw_scores
            .iter()
            .map(|score| {
                if score.is_finite() {
                    ((score - min_score) + 0.01).max(0.01)
                } else {
                    0.01
                }
            })
            .collect();
        let total = shifted.iter().sum::<f64>().max(0.01);

        candidates
            .iter()
            .cloned()
            .zip(shifted)
            .map(|(candidate, prior)| PolicyPrior {
                candidate,
                prior: prior / total,
            })
            .collect()
    }
}
