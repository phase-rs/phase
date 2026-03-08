pub mod legal_actions;
pub mod eval;
pub mod combat_ai;
pub mod config;
pub mod card_hints;
pub mod search;

pub use legal_actions::get_legal_actions;
pub use eval::{evaluate_state, evaluate_creature, EvalWeights};
pub use combat_ai::{choose_attackers, choose_blockers};
pub use config::{AiConfig, AiDifficulty, Platform, SearchConfig, create_config};
pub use card_hints::should_play_now;
pub use search::choose_action;
