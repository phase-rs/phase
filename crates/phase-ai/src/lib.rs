pub mod card_hints;
pub mod combat_ai;
pub mod config;
pub mod eval;
pub mod legal_actions;
pub mod search;

pub use card_hints::should_play_now;
pub use combat_ai::{choose_attackers, choose_attackers_with_targets, choose_blockers};
pub use config::{create_config, create_config_for_players, AiConfig, AiDifficulty, Platform, SearchConfig};
pub use eval::{evaluate_creature, evaluate_state, threat_level, EvalWeights};
pub use legal_actions::get_legal_actions;
pub use search::choose_action;
