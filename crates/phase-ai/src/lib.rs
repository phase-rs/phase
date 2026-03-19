pub mod auto_play;
pub mod card_hints;
pub mod combat_ai;
pub mod config;
pub mod eval;
pub mod planner;
pub mod policies;
pub mod search;

pub use card_hints::should_play_now;
pub use combat_ai::{choose_attackers, choose_attackers_with_targets, choose_blockers};
pub use config::{
    create_config, create_config_for_players, AiConfig, AiDifficulty, AiProfile, HiddenInfoMode,
    MctsConfig, OpponentModel, PlannerMode, Platform, SearchConfig,
};
pub use eval::{
    evaluate_creature, evaluate_for_planner, evaluate_state, evaluate_state_breakdown,
    strategic_intent, threat_level, EvalWeights, EvaluationBreakdown, StrategicIntent,
};
pub use search::choose_action;
