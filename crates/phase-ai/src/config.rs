use serde::{Deserialize, Serialize};

use crate::deck_profile::ArchetypeMultipliers;
use crate::eval::{EvalWeightSet, KeywordBonuses};

/// AI difficulty level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AiDifficulty {
    VeryEasy,
    Easy,
    Medium,
    Hard,
    VeryHard,
}

/// Platform the AI runs on (affects budget constraints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Native,
    Wasm,
}

/// Search algorithm configuration.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub enabled: bool,
    pub max_depth: u32,
    pub max_nodes: u32,
    pub max_branching: u32,
    pub planner_mode: PlannerMode,
    pub rollout_depth: u32,
    pub rollout_samples: u32,
    pub mcts: Option<MctsConfig>,
    pub hidden_info_mode: HiddenInfoMode,
    pub opponent_model: OpponentModel,
    /// Optional time budget in milliseconds. When set, search terminates
    /// after this duration regardless of node count. Essential for WASM
    /// where hardware performance varies widely.
    pub time_budget_ms: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerMode {
    BeamOnly,
    BeamPlusRollout,
    BeamPlusMcts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenInfoMode {
    PerfectInfo,
    Determinized,
    RevealedOnlyBias,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpponentModel {
    DeterministicBestReply,
    ThreatWeightedReply,
    SampledReply,
}

#[derive(Debug, Clone)]
pub struct MctsConfig {
    pub simulations: u32,
    pub c_puct: f64,
    pub rollout_depth: u32,
    pub exploration_fraction: f64,
    pub dirichlet_alpha: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct AiProfile {
    pub risk_tolerance: f64,
    pub interaction_patience: f64,
    pub stabilize_bias: f64,
}

impl Default for AiProfile {
    fn default() -> Self {
        Self {
            risk_tolerance: 0.6,
            interaction_patience: 0.75,
            stabilize_bias: 1.0,
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            enabled: false,
            max_depth: 0,
            max_nodes: 0,
            max_branching: 5,
            planner_mode: PlannerMode::BeamOnly,
            rollout_depth: 0,
            rollout_samples: 0,
            mcts: None,
            hidden_info_mode: HiddenInfoMode::PerfectInfo,
            opponent_model: OpponentModel::DeterministicBestReply,
            time_budget_ms: None,
        }
    }
}

/// Tunable penalty values for AI tactical policies.
/// All values are `f64` for compatibility with the CMA-ES training pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyPenalties {
    /// Penalty for targeting a creature already doomed by pending stack effects.
    pub redundant_removal_penalty: f64,
    /// Penalty for targeting a creature with pending (but non-lethal) damage.
    pub redundant_damage_penalty: f64,

    /// Penalty for casting a spell that gifts the opponent a card draw.
    pub gift_card_penalty: f64,
    /// Penalty for gifting opponent a Treasure token.
    pub gift_treasure_penalty: f64,
    /// Penalty for gifting opponent a Food token.
    pub gift_food_penalty: f64,
    /// Penalty for gifting opponent a tapped 1/1 Fish token.
    pub gift_fish_penalty: f64,
    /// Minimum creature value (from evaluate_creature) to justify gift removal.
    pub worthy_target_threshold: f64,

    /// Base penalty for massive overkill (damage > 2x remaining toughness).
    pub overkill_base_penalty: f64,
    /// Penalty for using premium removal on cheap targets.
    pub removal_quality_mismatch: f64,

    /// Bonus for bouncing a token (ceases to exist) or tucking to library.
    pub bounce_token_bonus: f64,
    /// Discount for bouncing a cheap permanent (easily replayed).
    pub bounce_cheap_discount: f64,
    /// Per-mana-value bonus for bouncing expensive permanents.
    pub bounce_expensive_bonus_per_mv: f64,

    /// Penalty for casting Destroy at an indestructible creature.
    pub indestructible_destroy_penalty: f64,
    /// Base penalty for targeting a creature with ward (scaled by cost severity).
    pub ward_cost_penalty_base: f64,

    /// Bonus for removal targeting a creature being pumped by opponent on the stack.
    pub pump_response_bonus: f64,
    /// Bonus for burn that would be lethal to opponent.
    pub lethal_burn_bonus: f64,
    /// Multiplier for protect-own-spell counter incentive (× threatened spell value).
    pub protect_spell_bonus_mult: f64,

    /// Penalty for tapping out when opponent has lethal damage on board.
    #[serde(default = "default_lethality_tapout_penalty")]
    pub lethality_tapout_penalty: f64,
    /// Value of a land when scoring sacrifice candidates (higher = worse to sacrifice).
    #[serde(default = "default_sacrifice_land_penalty")]
    pub sacrifice_land_penalty: f64,
    /// Value of a token when scoring sacrifice candidates (lower = cheaper to sacrifice).
    #[serde(default = "default_sacrifice_token_cost")]
    pub sacrifice_token_cost: f64,
    /// Multiplier for evasion removal bonus (× target power).
    #[serde(default = "default_evasion_removal_bonus_mult")]
    pub evasion_removal_bonus_mult: f64,
    /// Penalty for using destroy/damage removal on a recursive creature.
    #[serde(default = "default_recursion_destroy_penalty")]
    pub recursion_destroy_penalty: f64,
    /// Bonus for using exile on a recursive creature.
    #[serde(default = "default_recursion_exile_bonus")]
    pub recursion_exile_bonus: f64,
    /// Penalty for destroying a creature with death triggers (value on death).
    #[serde(default = "default_death_trigger_destroy_penalty")]
    pub death_trigger_destroy_penalty: f64,
    /// Per-creature penalty when overextending into probable board wipe.
    #[serde(default = "default_wrath_overextend_penalty")]
    pub wrath_overextend_penalty: f64,
    /// Bonus for casting defensive creatures when AI life is critical.
    #[serde(default = "default_low_life_defensive_bonus")]
    pub low_life_defensive_bonus: f64,
    /// Penalty for casting pure aggro creatures when AI life is critical.
    #[serde(default = "default_low_life_aggro_penalty")]
    pub low_life_aggro_penalty: f64,
    /// Bonus for card-generating plays when behind on card advantage.
    #[serde(default = "default_card_advantage_behind_extra")]
    pub card_advantage_behind_extra: f64,
    /// Penalty for spending the last counterspell on a low-impact target.
    #[serde(default = "default_counter_last_reservation_penalty")]
    pub counter_last_reservation_penalty: f64,
}

impl Default for PolicyPenalties {
    fn default() -> Self {
        Self {
            redundant_removal_penalty: -6.0,
            redundant_damage_penalty: -4.0,
            gift_card_penalty: -3.0,
            gift_treasure_penalty: -1.5,
            gift_food_penalty: -1.0,
            gift_fish_penalty: -0.5,
            worthy_target_threshold: 3.0,
            overkill_base_penalty: -2.0,
            removal_quality_mismatch: -1.5,
            bounce_token_bonus: 3.0,
            bounce_cheap_discount: -2.0,
            bounce_expensive_bonus_per_mv: 0.3,
            indestructible_destroy_penalty: -8.0,
            ward_cost_penalty_base: -2.0,
            pump_response_bonus: 2.5,
            lethal_burn_bonus: 15.0,
            protect_spell_bonus_mult: 0.75,
            lethality_tapout_penalty: default_lethality_tapout_penalty(),
            sacrifice_land_penalty: default_sacrifice_land_penalty(),
            sacrifice_token_cost: default_sacrifice_token_cost(),
            evasion_removal_bonus_mult: default_evasion_removal_bonus_mult(),
            recursion_destroy_penalty: default_recursion_destroy_penalty(),
            recursion_exile_bonus: default_recursion_exile_bonus(),
            death_trigger_destroy_penalty: default_death_trigger_destroy_penalty(),
            wrath_overextend_penalty: default_wrath_overextend_penalty(),
            low_life_defensive_bonus: default_low_life_defensive_bonus(),
            low_life_aggro_penalty: default_low_life_aggro_penalty(),
            card_advantage_behind_extra: default_card_advantage_behind_extra(),
            counter_last_reservation_penalty: default_counter_last_reservation_penalty(),
        }
    }
}

fn default_lethality_tapout_penalty() -> f64 {
    -2.5
}
fn default_sacrifice_land_penalty() -> f64 {
    4.0
}
fn default_sacrifice_token_cost() -> f64 {
    0.5
}
fn default_evasion_removal_bonus_mult() -> f64 {
    0.4
}
fn default_recursion_destroy_penalty() -> f64 {
    -1.5
}
fn default_death_trigger_destroy_penalty() -> f64 {
    -0.5
}
fn default_recursion_exile_bonus() -> f64 {
    1.0
}
fn default_wrath_overextend_penalty() -> f64 {
    -0.4
}
fn default_low_life_defensive_bonus() -> f64 {
    0.3
}
fn default_low_life_aggro_penalty() -> f64 {
    -0.3
}
fn default_card_advantage_behind_extra() -> f64 {
    0.15
}
fn default_counter_last_reservation_penalty() -> f64 {
    -1.5
}

/// Full AI configuration combining difficulty, search, and evaluation settings.
#[derive(Debug, Clone)]
pub struct AiConfig {
    pub difficulty: AiDifficulty,
    pub temperature: f64,
    pub profile: AiProfile,
    pub play_lookahead: bool,
    pub combat_lookahead: bool,
    pub search: SearchConfig,
    pub weights: EvalWeightSet,
    pub keyword_bonuses: KeywordBonuses,
    pub archetype_multipliers: ArchetypeMultipliers,
    pub policy_penalties: PolicyPenalties,
    /// Number of players in the game (used for search budget scaling).
    pub player_count: u8,
}

impl Default for AiConfig {
    fn default() -> Self {
        create_config(AiDifficulty::Medium, Platform::Native)
    }
}

/// Create an AI configuration for the given difficulty and platform.
///
/// Five presets scale from random play (VeryEasy) to deterministic best-move (VeryHard).
/// WASM platform reduces search budgets to fit within browser constraints.
pub fn create_config(difficulty: AiDifficulty, platform: Platform) -> AiConfig {
    let (temperature, profile, play_lookahead, combat_lookahead, search) = match difficulty {
        AiDifficulty::VeryEasy => (
            4.0,
            AiProfile {
                risk_tolerance: 0.9,
                interaction_patience: 0.2,
                stabilize_bias: 0.8,
            },
            false,
            false,
            SearchConfig {
                enabled: false,
                max_depth: 0,
                max_nodes: 0,
                max_branching: 5,
                planner_mode: PlannerMode::BeamOnly,
                rollout_depth: 0,
                rollout_samples: 0,
                mcts: None,
                hidden_info_mode: HiddenInfoMode::PerfectInfo,
                opponent_model: OpponentModel::DeterministicBestReply,
                time_budget_ms: None,
            },
        ),
        AiDifficulty::Easy => (
            2.0,
            AiProfile {
                risk_tolerance: 0.8,
                interaction_patience: 0.4,
                stabilize_bias: 0.9,
            },
            true,
            false,
            SearchConfig {
                enabled: false,
                max_depth: 0,
                max_nodes: 0,
                max_branching: 5,
                planner_mode: PlannerMode::BeamOnly,
                rollout_depth: 0,
                rollout_samples: 0,
                mcts: None,
                hidden_info_mode: HiddenInfoMode::PerfectInfo,
                opponent_model: OpponentModel::DeterministicBestReply,
                time_budget_ms: None,
            },
        ),
        AiDifficulty::Medium => (
            1.0,
            AiProfile {
                risk_tolerance: 0.65,
                interaction_patience: 0.7,
                stabilize_bias: 1.0,
            },
            true,
            true,
            SearchConfig {
                enabled: true,
                max_depth: 2,
                max_nodes: 24,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 1,
                rollout_samples: 1,
                mcts: None,
                hidden_info_mode: HiddenInfoMode::PerfectInfo,
                opponent_model: OpponentModel::DeterministicBestReply,
                time_budget_ms: None,
            },
        ),
        AiDifficulty::Hard => (
            0.5,
            AiProfile {
                risk_tolerance: 0.55,
                interaction_patience: 0.9,
                stabilize_bias: 1.1,
            },
            true,
            true,
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 48,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 2,
                rollout_samples: 1,
                mcts: None,
                hidden_info_mode: HiddenInfoMode::PerfectInfo,
                opponent_model: OpponentModel::ThreatWeightedReply,
                time_budget_ms: None,
            },
        ),
        AiDifficulty::VeryHard => (
            0.01,
            AiProfile {
                risk_tolerance: 0.45,
                interaction_patience: 1.0,
                stabilize_bias: 1.2,
            },
            true,
            true,
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 64,
                max_branching: 6,
                planner_mode: PlannerMode::BeamPlusMcts,
                rollout_depth: 2,
                rollout_samples: 2,
                mcts: Some(MctsConfig {
                    simulations: 48,
                    c_puct: 1.25,
                    rollout_depth: 2,
                    exploration_fraction: 0.0,
                    dirichlet_alpha: None,
                }),
                hidden_info_mode: HiddenInfoMode::Determinized,
                opponent_model: OpponentModel::ThreatWeightedReply,
                time_budget_ms: None,
            },
        ),
    };

    let mut config = AiConfig {
        difficulty,
        temperature,
        profile,
        play_lookahead,
        combat_lookahead,
        search,
        weights: EvalWeightSet::learned(),
        keyword_bonuses: KeywordBonuses::default(),
        archetype_multipliers: ArchetypeMultipliers::default(),
        policy_penalties: PolicyPenalties::default(),
        player_count: 2,
    };

    // WASM platform constraints: reduce budgets but keep MCTS with a time cap.
    // AI computation runs in a Web Worker so it does not block the UI thread.
    if platform == Platform::Wasm {
        config.search.max_depth = config.search.max_depth.min(2);
        config.search.max_nodes = config.search.max_nodes * 2 / 3;
        config.search.rollout_depth = config.search.rollout_depth.min(2);
        if matches!(config.search.planner_mode, PlannerMode::BeamPlusMcts) {
            // Reduce simulations (48 → 20) and add 500ms time cap as safety net
            if let Some(mcts) = &mut config.search.mcts {
                mcts.simulations = mcts.simulations.min(20);
                mcts.rollout_depth = mcts.rollout_depth.min(2);
            }
            config.search.time_budget_ms = Some(500);
        }
    }

    config
}

/// Create an AI configuration scaled for the given player count.
/// Reduces search depth and budget as player count grows:
/// - 2 players: unchanged
/// - 3-4 players: max depth 2, reduced node budget (paranoid search)
/// - 5-6 players: max depth 1, heuristic-heavy (or search disabled)
pub fn create_config_for_players(
    difficulty: AiDifficulty,
    platform: Platform,
    player_count: u8,
) -> AiConfig {
    let mut config = create_config(difficulty, platform);
    config.player_count = player_count;

    match player_count {
        0..=2 => {} // No scaling needed
        3..=4 => {
            // Paranoid search: cap depth at 2, reduce budget
            config.search.max_depth = config.search.max_depth.min(2);
            config.search.max_nodes = config.search.max_nodes * 2 / 3;
            config.search.max_branching = config.search.max_branching.min(4);
            config.search.rollout_depth = config.search.rollout_depth.min(1);
            if let Some(mcts) = &mut config.search.mcts {
                mcts.simulations = mcts.simulations.saturating_mul(2) / 3;
                mcts.rollout_depth = mcts.rollout_depth.min(1);
            }
        }
        _ => {
            // 5-6+ players: heuristic-only or minimal search
            if config.difficulty <= AiDifficulty::Medium {
                config.search.enabled = false;
            } else {
                config.search.max_depth = 1;
                config.search.max_nodes /= 3;
                config.search.max_branching = config.search.max_branching.min(3);
                config.search.rollout_depth = config.search.rollout_depth.min(1);
                if let Some(mcts) = &mut config.search.mcts {
                    mcts.simulations /= 2;
                    mcts.rollout_depth = mcts.rollout_depth.min(1);
                }
            }
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn very_easy_has_high_temperature() {
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        assert_eq!(config.temperature, 4.0);
        assert!(config.profile.risk_tolerance > 0.8);
        assert!(!config.search.enabled);
        assert!(!config.play_lookahead);
    }

    #[test]
    fn easy_has_play_lookahead() {
        let config = create_config(AiDifficulty::Easy, Platform::Native);
        assert_eq!(config.temperature, 2.0);
        assert!(config.profile.interaction_patience < 0.5);
        assert!(config.play_lookahead);
        assert!(!config.search.enabled);
    }

    #[test]
    fn medium_enables_search() {
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        assert_eq!(config.temperature, 1.0);
        assert!(config.search.enabled);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusRollout);
        assert!(config.profile.interaction_patience >= 0.7);
        assert_eq!(config.search.max_depth, 2);
        assert_eq!(config.search.max_nodes, 24);
        assert_eq!(config.search.rollout_depth, 1);
    }

    #[test]
    fn hard_increases_depth() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        assert_eq!(config.temperature, 0.5);
        assert!(config.profile.stabilize_bias > 1.0);
        assert_eq!(config.search.max_depth, 3);
        assert_eq!(config.search.max_nodes, 48);
        assert_eq!(config.search.rollout_depth, 2);
    }

    #[test]
    fn very_hard_is_near_deterministic() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        assert!(config.temperature < 0.1);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusMcts);
        assert!(config.search.mcts.is_some());
        assert_eq!(config.search.max_depth, 3);
        assert_eq!(config.search.max_nodes, 64);
        assert_eq!(config.search.max_branching, 6);
        assert_eq!(config.search.rollout_samples, 2);
    }

    #[test]
    fn wasm_reduces_budgets() {
        let native = create_config(AiDifficulty::Hard, Platform::Native);
        let wasm = create_config(AiDifficulty::Hard, Platform::Wasm);

        assert!(wasm.search.max_depth <= 2);
        assert!(wasm.search.max_nodes < native.search.max_nodes);
        assert!(wasm.search.rollout_depth <= native.search.rollout_depth);
    }

    #[test]
    fn wasm_very_hard_has_mcts_with_time_budget() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        assert_eq!(config.search.max_depth, 2);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusMcts);
        assert!(config.search.mcts.is_some());
        let mcts = config.search.mcts.unwrap();
        assert!(mcts.simulations <= 20);
        assert_eq!(mcts.rollout_depth, 2);
        assert_eq!(config.search.time_budget_ms, Some(500));
    }

    #[test]
    fn all_difficulties_have_valid_configs() {
        let difficulties = [
            AiDifficulty::VeryEasy,
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
        ];
        for diff in &difficulties {
            let config = create_config(*diff, Platform::Native);
            assert!(config.temperature > 0.0);
            assert_eq!(config.difficulty, *diff);
        }
    }

    #[test]
    fn default_config_is_medium_native() {
        let config = AiConfig::default();
        assert_eq!(config.difficulty, AiDifficulty::Medium);
    }

    #[test]
    fn four_player_caps_depth_at_two() {
        let config = create_config_for_players(AiDifficulty::Hard, Platform::Native, 4);
        assert!(config.search.max_depth <= 2);
        assert!(config.search.enabled);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusRollout);
    }

    #[test]
    fn four_player_reduces_budget() {
        let base = create_config(AiDifficulty::Hard, Platform::Native);
        let scaled = create_config_for_players(AiDifficulty::Hard, Platform::Native, 4);
        assert!(scaled.search.max_nodes < base.search.max_nodes);
    }

    #[test]
    fn six_player_medium_disables_search() {
        let config = create_config_for_players(AiDifficulty::Medium, Platform::Native, 6);
        assert!(!config.search.enabled);
    }

    #[test]
    fn six_player_hard_uses_depth_one() {
        let config = create_config_for_players(AiDifficulty::Hard, Platform::Native, 6);
        assert!(config.search.enabled);
        assert_eq!(config.search.max_depth, 1);
    }

    #[test]
    fn four_player_very_hard_keeps_mcts_with_lower_budget() {
        let config = create_config_for_players(AiDifficulty::VeryHard, Platform::Native, 4);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusMcts);
        assert!(config
            .search
            .mcts
            .as_ref()
            .is_some_and(|mcts| mcts.simulations < 48));
    }

    #[test]
    fn two_player_unchanged() {
        let base = create_config(AiDifficulty::Medium, Platform::Native);
        let scaled = create_config_for_players(AiDifficulty::Medium, Platform::Native, 2);
        assert_eq!(base.search.max_depth, scaled.search.max_depth);
        assert_eq!(base.search.max_nodes, scaled.search.max_nodes);
    }

    #[test]
    fn wasm_and_player_scaling_compound() {
        let config = create_config_for_players(AiDifficulty::Hard, Platform::Wasm, 4);
        // WASM caps at depth 2, then 4-player also caps at 2
        assert!(config.search.max_depth <= 2);
        // Both WASM and 4-player reduce nodes
        let native_2p = create_config(AiDifficulty::Hard, Platform::Native);
        assert!(config.search.max_nodes < native_2p.search.max_nodes);
    }

    #[test]
    fn player_count_stored_in_config() {
        let config = create_config_for_players(AiDifficulty::Medium, Platform::Native, 4);
        assert_eq!(config.player_count, 4);
    }

    #[test]
    fn ai_difficulty_serde_roundtrips() {
        let difficulties = [
            AiDifficulty::VeryEasy,
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
        ];
        for diff in &difficulties {
            let json = serde_json::to_string(diff).unwrap();
            let parsed: AiDifficulty = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, *diff);
        }
    }
}
