use super::anti_self_harm::AntiSelfHarmPolicy;
use super::context::PolicyContext;
use super::effect_timing::EffectTimingPolicy;
use crate::config::AiConfig;
use crate::planner::PolicyPrior;
use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

pub trait TacticalPolicy: Send + Sync {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64;
}

pub struct PolicyRegistry {
    policies: Vec<Box<dyn TacticalPolicy>>,
}

impl Default for PolicyRegistry {
    fn default() -> Self {
        Self {
            policies: vec![Box::new(AntiSelfHarmPolicy), Box::new(EffectTimingPolicy)],
        }
    }
}

impl PolicyRegistry {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        self.policies.iter().map(|policy| policy.score(ctx)).sum()
    }

    pub fn priors(
        &self,
        state: &GameState,
        decision: &AiDecisionContext,
        candidates: &[CandidateAction],
        ai_player: PlayerId,
        config: &AiConfig,
    ) -> Vec<PolicyPrior> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let raw_scores: Vec<f64> = candidates
            .iter()
            .map(|candidate| {
                self.score(&PolicyContext {
                    state,
                    decision,
                    candidate,
                    ai_player,
                    config,
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
