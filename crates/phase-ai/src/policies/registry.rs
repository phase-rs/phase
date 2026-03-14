use super::anti_self_harm::AntiSelfHarmPolicy;
use super::context::PolicyContext;
use super::effect_timing::EffectTimingPolicy;

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
}
