pub mod ability_chain;
pub mod auto_play;
pub mod card_advantage;
pub mod card_hints;
pub mod cast_facts;
pub mod combat_ai;
pub mod combo;
pub mod config;
pub mod context;
pub mod damage_reflection;
pub mod decision_kind;
pub mod deck_knowledge;
pub mod deck_profile;
pub mod draft_eval;
pub mod duel_suite;
pub mod eval;
pub mod features;
pub mod mana_colors;
pub mod plan;
pub mod planner;
pub mod policies;
pub mod projection;
pub mod search;
pub mod session;
pub mod strategy_profile;
pub mod synergy;
pub mod tactical_gate;
pub mod threat_profile;
pub mod tribute_eval;
pub mod zone_eval;

pub use card_hints::should_play_now;
pub use combat_ai::{choose_attackers, choose_attackers_with_targets, choose_blockers};
pub use config::{
    create_config, create_config_for_players, AiConfig, AiDifficulty, AiProfile, OpponentModel,
    PlannerMode, Platform, SearchConfig,
};
pub use deck_profile::ArchetypeMultipliers;
pub use draft_eval::{
    evaluate_draft_card, evaluate_draft_card_default, rarity_prior, DraftWeights,
};
pub use eval::{
    creature_combat_value, evaluate_creature, evaluate_creature_with_bonuses, evaluate_for_planner,
    evaluate_state, evaluate_state_breakdown, strategic_intent, threat_level,
    threat_level_projected, EvalWeightSet, EvalWeights, EvaluationBreakdown, KeywordBonuses,
    StrategicIntent,
};
pub use search::{choose_action, score_candidates, softmax_select_pairs};
