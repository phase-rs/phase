use serde::{Deserialize, Serialize};

use crate::eval::EvalWeights;

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
        }
    }
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
    pub weights: EvalWeights,
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
                hidden_info_mode: HiddenInfoMode::PerfectInfo,
                opponent_model: OpponentModel::ThreatWeightedReply,
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
        weights: EvalWeights::default(),
        player_count: 2,
    };

    // WASM platform constraints
    if platform == Platform::Wasm {
        config.search.max_depth = config.search.max_depth.min(2);
        config.search.max_nodes = config.search.max_nodes * 2 / 3;
        config.search.rollout_depth = config.search.rollout_depth.min(1);
        if matches!(config.search.planner_mode, PlannerMode::BeamPlusMcts) {
            config.search.planner_mode = PlannerMode::BeamPlusRollout;
            config.search.mcts = None;
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
    fn wasm_caps_depth_at_two() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        assert_eq!(config.search.max_depth, 2);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusRollout);
        assert!(config.search.mcts.is_none());
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
