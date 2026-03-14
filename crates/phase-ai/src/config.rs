use crate::eval::EvalWeights;

/// AI difficulty level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    pub rollout_depth: u32,
    pub rollout_samples: u32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        SearchConfig {
            enabled: false,
            max_depth: 0,
            max_nodes: 0,
            max_branching: 5,
            rollout_depth: 0,
            rollout_samples: 0,
        }
    }
}

/// Full AI configuration combining difficulty, search, and evaluation settings.
#[derive(Debug, Clone)]
pub struct AiConfig {
    pub difficulty: AiDifficulty,
    pub temperature: f64,
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
    let (temperature, play_lookahead, combat_lookahead, search) = match difficulty {
        AiDifficulty::VeryEasy => (
            4.0,
            false,
            false,
            SearchConfig {
                enabled: false,
                max_depth: 0,
                max_nodes: 0,
                max_branching: 5,
                rollout_depth: 0,
                rollout_samples: 0,
            },
        ),
        AiDifficulty::Easy => (
            2.0,
            true,
            false,
            SearchConfig {
                enabled: false,
                max_depth: 0,
                max_nodes: 0,
                max_branching: 5,
                rollout_depth: 0,
                rollout_samples: 0,
            },
        ),
        AiDifficulty::Medium => (
            1.0,
            true,
            true,
            SearchConfig {
                enabled: true,
                max_depth: 2,
                max_nodes: 24,
                max_branching: 5,
                rollout_depth: 1,
                rollout_samples: 1,
            },
        ),
        AiDifficulty::Hard => (
            0.5,
            true,
            true,
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 48,
                max_branching: 5,
                rollout_depth: 2,
                rollout_samples: 1,
            },
        ),
        AiDifficulty::VeryHard => (
            0.01,
            true,
            true,
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 64,
                max_branching: 6,
                rollout_depth: 2,
                rollout_samples: 2,
            },
        ),
    };

    let mut config = AiConfig {
        difficulty,
        temperature,
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
        assert!(!config.search.enabled);
        assert!(!config.play_lookahead);
    }

    #[test]
    fn easy_has_play_lookahead() {
        let config = create_config(AiDifficulty::Easy, Platform::Native);
        assert_eq!(config.temperature, 2.0);
        assert!(config.play_lookahead);
        assert!(!config.search.enabled);
    }

    #[test]
    fn medium_enables_search() {
        let config = create_config(AiDifficulty::Medium, Platform::Native);
        assert_eq!(config.temperature, 1.0);
        assert!(config.search.enabled);
        assert_eq!(config.search.max_depth, 2);
        assert_eq!(config.search.max_nodes, 24);
        assert_eq!(config.search.rollout_depth, 1);
    }

    #[test]
    fn hard_increases_depth() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        assert_eq!(config.temperature, 0.5);
        assert_eq!(config.search.max_depth, 3);
        assert_eq!(config.search.max_nodes, 48);
        assert_eq!(config.search.rollout_depth, 2);
    }

    #[test]
    fn very_hard_is_near_deterministic() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        assert!(config.temperature < 0.1);
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
}
