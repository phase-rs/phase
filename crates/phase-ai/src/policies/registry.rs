use std::collections::HashMap;

use super::aggro_pressure::AggroPressurePolicy;
use super::anthem_priority::AnthemPriorityPolicy;
use super::anti_self_harm::AntiSelfHarmPolicy;
use super::blight_value::BlightValuePolicy;
use super::blink_payoff::BlinkPayoffPolicy;
use super::board_development::BoardDevelopmentPolicy;
use super::board_wipe_telegraph::BoardWipeTelegraphPolicy;
use super::card_advantage::CardAdvantagePolicy;
use super::chalice_avoidance::ChaliceAvoidancePolicy;
use super::context::PolicyContext;
use super::copy_value::CopyValuePolicy;
use super::effect_timing::EffectTimingPolicy;
use super::enchantments_payoff::EnchantmentsPayoffPolicy;
use super::equipment_payoff::EquipmentPayoffPolicy;
use super::etb_value::EtbValuePolicy;
use super::evasion_removal_priority::EvasionRemovalPriorityPolicy;
use super::fetch_land_patience::FetchLandPatiencePolicy;
use super::free_outlet_activation::FreeOutletActivationPolicy;
use super::hand_disruption::HandDisruptionPolicy;
use super::hold_mana_up::HoldManaUpForInteractionPolicy;
use super::interaction_reservation::InteractionReservationPolicy;
use super::landfall_timing::LandfallTimingPolicy;
use super::lethality_awareness::LethalityAwarenessPolicy;
use super::life_total_resource::LifeTotalResourcePolicy;
use super::lifegain_payoff::LifegainPayoffPolicy;
use super::payment_selection::PaymentSelectionPolicy;
use super::plus_one_counters::PlusOneCountersPolicy;
use super::ramp_timing::RampTimingPolicy;
use super::reactive_self_protection::ReactiveSelfProtectionPolicy;
use super::reanimator_payoff::ReanimatorPayoffPolicy;
use super::recursion_awareness::RecursionAwarenessPolicy;
use super::redundancy_avoidance::RedundancyAvoidancePolicy;
use super::sacrifice_land_protection::SacrificeLandProtectionPolicy;
use super::sacrifice_value::SacrificeValuePolicy;
use super::separate_piles_timing::SeparatePilesTimingPolicy;
use super::spellslinger_casting::SpellslingerCastingPolicy;
use super::sweeper_timing::SweeperTimingPolicy;
use super::tokens_wide::TokensWidePolicy;
use super::tribal_lord_priority::TribalLordPriorityPolicy;
use super::tutor::TutorPolicy;
use super::x_value::XValuePolicy;
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
    ArtifactSynergyTactical,
    BoardDevelopment,
    EtbValue,
    EnchantmentsPayoff,
    EquipmentPayoff,
    BlinkPayoff,
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
    BlightValue,
    EvasionRemovalPriority,
    RecursionAwareness,
    ReanimatorPayoff,
    BoardWipeTelegraph,
    LifeTotalResource,
    LifegainPayoff,
    CardAdvantage,
    LandfallTiming,
    RampTiming,
    RedundancyAvoidance,
    KeepablesByLandCount,
    LandfallKeepablesMulligan,
    RampKeepablesMulligan,
    TribalLordPriority,
    TribalDensityMulligan,
    HoldManaUpForInteraction,
    SweeperTiming,
    FreeOutletActivation,
    FetchLandPatience,
    AristocratsKeepablesMulligan,
    AggroPressure,
    AggroKeepablesMulligan,
    TokensWide,
    AnthemPriority,
    TokensWideMulligan,
    PlusOneCountersTactical,
    PlusOneCountersMulligan,
    SpellslingerCasting,
    SpellslingerKeepablesMulligan,
    CombatTaxPayment,
    ReactiveSelfProtection,
    SacrificeLandProtection,
    ComboLineProgress,
    CedhKeepablesMulligan,
    FixedDeckKeepMulligan,
    PlaneswalkerLoyalty,
    EquipmentPriority,
    SpellskitePriority,
    LandSequencing,
    ConditionGatedActivation,
    ControlChangeAwareness,
    XValue,
    LandAnimation,
    MillTargeting,
    ChaliceAvoidance,
    PaymentSelection,
    SeparatePilesTiming,
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

/// Score unit contract: `delta = 1.0` is one card of expected value.
/// Policy helpers expose the declared band at each call site; Phase 3 lints
/// direct `Score` literals so new policies route through this contract.
pub const NUDGE_MAX: f64 = 0.3;
pub const PREFERENCE_MAX: f64 = 1.5;
pub const STRONG_MAX: f64 = 5.0;
pub const CRITICAL_MAX: f64 = 15.0;

impl PolicyVerdict {
    pub fn score(delta: f64, reason: PolicyReason) -> Self {
        let magnitude = delta.abs();
        if magnitude == 0.0 {
            Self::neutral(reason)
        } else if magnitude <= NUDGE_MAX {
            Self::nudge(delta, reason)
        } else if magnitude <= PREFERENCE_MAX {
            Self::preference(delta, reason)
        } else if magnitude <= STRONG_MAX {
            Self::strong(delta, reason)
        } else {
            Self::critical(delta.signum() * magnitude.min(CRITICAL_MAX), reason)
        }
    }

    pub fn neutral(reason: PolicyReason) -> Self {
        Self::Score { delta: 0.0, reason }
    }

    pub fn reject(reason: PolicyReason) -> Self {
        Self::Reject { reason }
    }

    pub fn nudge(delta: f64, reason: PolicyReason) -> Self {
        Self::score_in_band(delta, 0.0, NUDGE_MAX, reason)
    }

    pub fn preference(delta: f64, reason: PolicyReason) -> Self {
        Self::score_in_band(delta, NUDGE_MAX, PREFERENCE_MAX, reason)
    }

    pub fn strong(delta: f64, reason: PolicyReason) -> Self {
        Self::score_in_band(delta, PREFERENCE_MAX, STRONG_MAX, reason)
    }

    pub fn critical(delta: f64, reason: PolicyReason) -> Self {
        Self::score_in_band(delta, STRONG_MAX, CRITICAL_MAX, reason)
    }

    fn score_in_band(
        delta: f64,
        min_exclusive: f64,
        max_inclusive: f64,
        reason: PolicyReason,
    ) -> Self {
        let magnitude = delta.abs();
        debug_assert!(
            magnitude > min_exclusive && magnitude <= max_inclusive,
            "policy delta {delta} outside declared band ({min_exclusive}, {max_inclusive}]"
        );
        let clamped = if magnitude == 0.0 {
            0.0
        } else {
            delta.signum() * magnitude.clamp(min_exclusive.min(f64::EPSILON), max_inclusive)
        };
        Self::Score {
            delta: clamped,
            reason,
        }
    }
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
            Box::new(super::artifact_synergy::ArtifactSynergyPolicy),
            Box::new(BoardDevelopmentPolicy),
            Box::new(EtbValuePolicy),
            Box::new(EnchantmentsPayoffPolicy),
            Box::new(EquipmentPayoffPolicy),
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
            Box::new(BlightValuePolicy),
            Box::new(EvasionRemovalPriorityPolicy),
            Box::new(RecursionAwarenessPolicy),
            Box::new(BoardWipeTelegraphPolicy),
            Box::new(LifeTotalResourcePolicy),
            Box::new(LifegainPayoffPolicy),
            Box::new(CardAdvantagePolicy),
            Box::new(LandfallTimingPolicy),
            Box::new(RampTimingPolicy),
            Box::new(RedundancyAvoidancePolicy),
            Box::new(TribalLordPriorityPolicy),
            Box::new(HoldManaUpForInteractionPolicy),
            Box::new(SweeperTimingPolicy),
            Box::new(FreeOutletActivationPolicy),
            Box::new(FetchLandPatiencePolicy),
            Box::new(AggroPressurePolicy),
            Box::new(TokensWidePolicy),
            Box::new(AnthemPriorityPolicy),
            Box::new(PlusOneCountersPolicy),
            Box::new(SpellslingerCastingPolicy),
            Box::new(super::combat_tax::CombatTaxPaymentPolicy),
            Box::new(ReactiveSelfProtectionPolicy),
            Box::new(SacrificeLandProtectionPolicy),
            Box::new(super::combo_line::ComboLinePolicy::new()),
            Box::new(super::planeswalker_loyalty::PlaneswalkerLoyaltyPolicy),
            Box::new(super::equipment_priority::EquipmentPriorityPolicy),
            Box::new(super::spellskite_priority::SpellskitePriorityPolicy),
            Box::new(super::land_sequencing::LandSequencingPolicy),
            Box::new(super::condition_gated_activation::ConditionGatedActivationPolicy),
            Box::new(XValuePolicy),
            Box::new(super::control_change_awareness::ControlChangeAwarenessPolicy),
            Box::new(super::land_animation::LandAnimationPolicy),
            Box::new(super::mill_targeting::MillTargetingPolicy),
            Box::new(ChaliceAvoidancePolicy),
            Box::new(PaymentSelectionPolicy),
            Box::new(SeparatePilesTimingPolicy),
            Box::new(ReanimatorPayoffPolicy),
            Box::new(BlinkPayoffPolicy),
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
    /// Return a process-wide shared `PolicyRegistry`, constructed once on first
    /// access. Policies are stateless (`TacticalPolicy: Send + Sync`, no
    /// interior mutability by construction), so a single instance safely
    /// serves every thread and every decision without cross-game bleed.
    ///
    /// Prefer this over `PolicyRegistry::default()` in hot paths: `default()`
    /// allocates ~20 `Box<dyn TacticalPolicy>` per call, which the scorer and
    /// decision tracer previously ran on every candidate evaluation.
    pub fn shared() -> &'static Self {
        static REGISTRY: std::sync::OnceLock<PolicyRegistry> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(PolicyRegistry::default)
    }

    /// Run every policy whose `decision_kinds()` matches the classified kind
    /// for `ctx.candidate`, returning each policy's structured verdict.
    /// Used by `priors()` and (when tracing is enabled) for trace aggregation.
    pub fn verdicts(&self, ctx: &PolicyContext<'_>) -> Vec<(PolicyId, PolicyVerdict)> {
        let kind = classify_decision(&ctx.decision.waiting_for, &ctx.candidate.action);
        let Some(indices) = self.by_kind.get(&kind) else {
            return Vec::new();
        };
        // Borrow the cached DeckFeatures instead of cloning. Cloning a
        // DeckFeatures (9 Feature sub-structs, most carrying Vec<CardId>)
        // per candidate is a ~hundred-microsecond hit × hundreds of
        // `verdicts()` calls per decision — a measurable fraction of the
        // pre-search tactical pass on large states. `AiSession::features`
        // stays `cached-per-decision` so the borrow is safe for the scope.
        let default_features;
        let session_features: &crate::features::DeckFeatures =
            match ctx.context.session.features.get(&ctx.ai_player) {
                Some(f) => f,
                None => {
                    default_features = crate::features::DeckFeatures::default();
                    &default_features
                }
            };
        let mut out = Vec::with_capacity(indices.len());
        for &idx in indices {
            let policy = &self.policies[idx];
            let policy_id = policy.id();
            let Some(activation) = policy.activation(session_features, ctx.state, ctx.ai_player)
            else {
                continue;
            };
            let verdict = policy.verdict(ctx);
            let scaled = match verdict {
                PolicyVerdict::Reject { reason } => PolicyVerdict::Reject { reason },
                PolicyVerdict::Score { delta, reason } => {
                    let scaled_delta = delta * activation as f64;
                    debug_assert!(
                        scaled_delta.abs() <= CRITICAL_MAX,
                        "policy {:?} scaled delta {} exceeds critical band ceiling {}",
                        policy_id,
                        scaled_delta,
                        CRITICAL_MAX
                    );
                    if scaled_delta.abs() > CRITICAL_MAX {
                        tracing::warn!(
                            target: "phase_ai::decision_trace",
                            ?policy_id,
                            scaled_delta,
                            activation,
                            "policy scaled delta exceeds critical band ceiling"
                        );
                    }
                    PolicyVerdict::Score {
                        delta: scaled_delta,
                        reason,
                    }
                }
            };
            out.push((policy_id, scaled));
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

    /// Returns `true` if any registered policy has the given `PolicyId`.
    /// Intended for integration tests and diagnostics — not for hot paths.
    pub fn has_policy(&self, id: PolicyId) -> bool {
        self.policies.iter().any(|p| p.id() == id)
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
            .filter(|score| score.is_finite())
            .fold(f64::INFINITY, f64::min);
        if !min_score.is_finite() {
            tracing::warn!(
                target: "phase_ai::decision_trace",
                candidate_count = candidates.len(),
                "all policy candidates were rejected; using uniform forced-action priors"
            );
            let prior = 1.0 / candidates.len() as f64;
            return candidates
                .iter()
                .cloned()
                .map(|candidate| PolicyPrior { candidate, prior })
                .collect();
        }
        let shifted: Vec<f64> = raw_scores
            .iter()
            .map(|score| {
                if score.is_finite() {
                    ((score - min_score) + 0.01).max(0.01)
                } else {
                    0.0
                }
            })
            .collect();
        let total = shifted.iter().sum::<f64>();
        if total <= 0.0 {
            tracing::warn!(
                target: "phase_ai::decision_trace",
                candidate_count = candidates.len(),
                "policy priors summed to zero; using uniform forced-action priors"
            );
            let prior = 1.0 / candidates.len() as f64;
            return candidates
                .iter()
                .cloned()
                .map(|candidate| PolicyPrior { candidate, prior })
                .collect();
        }

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

#[cfg(test)]
mod shared_invariant_tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::types::actions::GameAction;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};

    #[test]
    fn default_registry_contains_combo_line_progress() {
        let reg = PolicyRegistry::default();
        let has = reg
            .policies
            .iter()
            .any(|p| p.id() == PolicyId::ComboLineProgress);
        assert!(
            has,
            "PolicyRegistry::default() must register ComboLinePolicy"
        );
    }

    /// `PolicyRegistry::shared()` returns a stable process-wide instance.
    /// Two calls must hand back the same pointer and the same policy count —
    /// if a future `TacticalPolicy` impl adds interior mutability, the shape
    /// may still match but cross-game bleed becomes possible. This test is
    /// the minimum check that the sharing contract is wired correctly.
    #[test]
    fn shared_returns_same_instance() {
        let a = PolicyRegistry::shared();
        let b = PolicyRegistry::shared();
        assert!(
            std::ptr::eq(a, b),
            "PolicyRegistry::shared() must return the same OnceLock-backed \
             instance across calls — interior mutability in any policy \
             would then bleed state across games"
        );
        assert_eq!(
            a.policies.len(),
            PolicyRegistry::default().policies.len(),
            "shared instance must contain the same policy set as a fresh default()"
        );
    }

    struct PriorTestPolicy;

    impl TacticalPolicy for PriorTestPolicy {
        fn id(&self) -> PolicyId {
            PolicyId::AntiSelfHarm
        }

        fn decision_kinds(&self) -> &'static [DecisionKind] {
            &[DecisionKind::ActivateAbility, DecisionKind::PlayLand]
        }

        fn activation(
            &self,
            _features: &DeckFeatures,
            _state: &GameState,
            _player: PlayerId,
        ) -> Option<f32> {
            Some(1.0)
        }

        fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
            match ctx.candidate.action {
                GameAction::PassPriority => PolicyVerdict::reject(PolicyReason::new("test_reject")),
                _ => PolicyVerdict::nudge(0.1, PolicyReason::new("test_score")),
            }
        }
    }

    fn prior_test_registry() -> PolicyRegistry {
        let policies: Vec<Box<dyn TacticalPolicy>> = vec![Box::new(PriorTestPolicy)];
        let mut by_kind: HashMap<DecisionKind, Vec<usize>> = HashMap::new();
        by_kind.insert(DecisionKind::ActivateAbility, vec![0]);
        by_kind.insert(DecisionKind::PlayLand, vec![0]);
        PolicyRegistry { policies, by_kind }
    }

    fn candidate(action: GameAction, tactical_class: TacticalClass) -> CandidateAction {
        CandidateAction {
            action,
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class,
            },
        }
    }

    fn priority_decision(candidates: Vec<CandidateAction>) -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates,
        }
    }

    #[test]
    fn rejected_candidate_gets_zero_prior_when_any_candidate_is_allowed() {
        let rejected = candidate(GameAction::PassPriority, TacticalClass::Pass);
        let allowed = candidate(
            GameAction::PlayLand {
                object_id: ObjectId(1),
                card_id: CardId(1),
            },
            TacticalClass::Land,
        );
        let candidates = vec![rejected.clone(), allowed.clone()];
        let decision = priority_decision(candidates.clone());
        let state = GameState::new_two_player(7);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let priors = prior_test_registry().priors(
            &state,
            &decision,
            &candidates,
            PlayerId(0),
            &config,
            &context,
        );

        assert_eq!(priors.len(), 2);
        assert_eq!(priors[0].prior, 0.0);
        assert!(priors[1].prior > 0.99);
    }

    #[test]
    fn all_rejected_candidates_fall_back_to_uniform_priors() {
        let candidates = vec![
            candidate(GameAction::PassPriority, TacticalClass::Pass),
            candidate(GameAction::PassPriority, TacticalClass::Pass),
        ];
        let decision = priority_decision(candidates.clone());
        let state = GameState::new_two_player(7);
        let config = AiConfig::default();
        let context = crate::context::AiContext::empty(&config.weights);

        let priors = prior_test_registry().priors(
            &state,
            &decision,
            &candidates,
            PlayerId(0),
            &config,
            &context,
        );

        assert_eq!(priors.len(), 2);
        assert!(priors
            .iter()
            .all(|prior| (prior.prior - 0.5).abs() < f64::EPSILON));
    }
}
