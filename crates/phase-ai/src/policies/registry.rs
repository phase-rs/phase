use super::anti_self_harm::AntiSelfHarmPolicy;
use super::board_development::BoardDevelopmentPolicy;
use super::board_wipe_telegraph::BoardWipeTelegraphPolicy;
use super::card_advantage::CardAdvantagePolicy;
use super::context::PolicyContext;
use super::copy_value::CopyValuePolicy;
use super::effect_timing::EffectTimingPolicy;
use super::etb_value::EtbValuePolicy;
use super::evasion_removal_priority::EvasionRemovalPriorityPolicy;
use super::hand_disruption::HandDisruptionPolicy;
use super::interaction_reservation::InteractionReservationPolicy;
use super::lethality_awareness::LethalityAwarenessPolicy;
use super::life_total_resource::LifeTotalResourcePolicy;
use super::recursion_awareness::RecursionAwarenessPolicy;
use super::sacrifice_value::SacrificeValuePolicy;
use super::tutor::TutorPolicy;
use crate::cast_facts::cast_facts_for_action;
use crate::config::AiConfig;
use crate::deck_profile::DeckArchetype;
use crate::planner::PolicyPrior;
use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

pub trait TacticalPolicy: Send + Sync {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64;

    /// Archetype scaling factor for this policy. Default is 1.0 (no scaling).
    /// Policies override this to return their archetype-specific multiplier.
    fn archetype_scale(&self, archetype: DeckArchetype) -> f64 {
        let _ = archetype;
        1.0
    }
}

pub struct PolicyRegistry {
    policies: Vec<Box<dyn TacticalPolicy>>,
}

impl Default for PolicyRegistry {
    fn default() -> Self {
        Self {
            policies: vec![
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
            ],
        }
    }
}

impl PolicyRegistry {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let archetype = ctx.context.deck_profile.archetype;
        let turn = ctx.state.turn_number;
        let turn_mult = ctx.context.strategy.turn_phase_mult(turn);

        self.policies
            .iter()
            .map(|policy| {
                let raw = policy.score(ctx);
                let arch_scale = policy.archetype_scale(archetype);
                raw * arch_scale * turn_mult
            })
            .sum()
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
        let min_score = raw_scores.iter().copied().fold(f64::INFINITY, f64::min);
        let shifted: Vec<f64> = raw_scores
            .iter()
            .map(|score| ((score - min_score) + 0.01).max(0.01))
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
