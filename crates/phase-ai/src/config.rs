use serde::{Deserialize, Serialize};

use crate::deck_profile::ArchetypeMultipliers;
use crate::eval::{EvalWeightSet, KeywordBonuses};
use crate::strategy_profile::StrategyProfile;

/// Wall-clock budget for AI search across ALL difficulties and platforms.
///
/// When `Some(ms)`, search terminates at the deadline even if `max_depth` /
/// `max_nodes` hasn't been reached — capping user-visible AI latency at the
/// cost of search quality on slow hardware. When `None`, search runs to its
/// node/depth budget, keeping quality consistent regardless of host speed.
///
/// Historically set to 1500/2500/4000 ms for Medium/Hard/VeryHard to mask a
/// deep-clone perf regression on AI search nodes. The Arc-share migration
/// (Commits 1/2/3 of perf(engine) on 2026-04-24) eliminated that cost, so
/// the deadline is currently disabled.
///
/// **Single source of truth** — every `SearchConfig::time_budget_ms` in this
/// crate references this constant. Change the value here to re-enable or
/// re-tune wall-clock capping globally.
pub const AI_SEARCH_TIME_BUDGET_MS: Option<u32> = None;

/// How much the AI reasons about what the opponent might hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThreatAwareness {
    /// VeryEasy, Easy: no threat reasoning.
    #[default]
    None,
    /// Medium: fixed probabilities from opponent archetype.
    ArchetypeOnly,
    /// Hard, VeryHard: per-card hypergeometric analysis.
    Full,
}

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
    pub opponent_model: OpponentModel,
    /// Optional time budget in milliseconds. When set, search terminates
    /// after this duration regardless of node count. See
    /// `AI_SEARCH_TIME_BUDGET_MS` (top of module) for the single source of
    /// truth — every call-site should reference that constant rather than
    /// writing a literal.
    pub time_budget_ms: Option<u32>,
    /// When `true`, wall-clock deadlines are disabled — search is bounded only
    /// by `max_nodes` / `max_depth`. Integration tests and `ai-duel` regression
    /// runs pin this to `true` so they don't observe wall-clock flake.
    /// Benchmarks and production code leave this `false` to measure the real
    /// deadline-bounded regime users experience.
    pub deterministic: bool,
    /// How much the AI reasons about opponent hand threats.
    pub threat_awareness: ThreatAwareness,
    /// Minimum remaining wall-clock budget (ms) required before running an
    /// uncached multi-turn projection (e.g., `velocity_score`'s opponent-turn
    /// simulation). When `time_budget_ms.remaining < this`, policies fall back
    /// to cache-only lookups and a heuristic score — preserves the tactical
    /// signal without blowing the user-visible turn-time budget.
    ///
    /// Scaled per difficulty: Medium needs tighter gating than VeryHard because
    /// Medium's shorter budget (1500ms native) leaves less headroom for a
    /// ~1.5s opponent-turn simulation. Set to 0 to always run projections.
    pub projection_min_budget_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerMode {
    BeamOnly,
    BeamPlusRollout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpponentModel {
    DeterministicBestReply,
    ThreatWeightedReply,
    SampledReply,
}

#[derive(Debug, Clone)]
pub struct AiProfile {
    pub risk_tolerance: f64,
    pub interaction_patience: f64,
    pub stabilize_bias: f64,
}

impl AiProfile {
    /// Apply archetype strategy modulation to this difficulty-based profile.
    /// Clamps results to valid ranges to prevent extreme combinations.
    ///
    /// Key principle: archetype modulates what the AI values, difficulty modulates
    /// how well it executes.
    pub fn with_strategy(&self, strategy: &StrategyProfile) -> AiProfile {
        AiProfile {
            risk_tolerance: (self.risk_tolerance * strategy.risk_tolerance_mult).clamp(0.2, 1.0),
            interaction_patience: (self.interaction_patience * strategy.interaction_patience_mult)
                .clamp(0.1, 1.0),
            stabilize_bias: (self.stabilize_bias * strategy.stabilize_bias_mult).clamp(0.5, 2.0),
        }
    }
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
            opponent_model: OpponentModel::DeterministicBestReply,
            time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
            deterministic: false,
            threat_awareness: ThreatAwareness::None,
            projection_min_budget_ms: 500,
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
    /// Bonus for casting spells on-curve (mana value matches available mana),
    /// weighted toward early game turns.
    #[serde(default = "default_tempo_curve_bonus")]
    pub tempo_curve_bonus: f64,
    /// Bonus for casting spells that synergize with existing board presence
    /// (tribal overlap, deck synergy graph).
    #[serde(default = "default_synergy_casting_bonus")]
    pub synergy_casting_bonus: f64,
    /// Penalty multiplier for tapping out when opponent likely has countermagic.
    #[serde(default = "default_threat_counter_tapout_penalty")]
    pub threat_counter_tapout_penalty: f64,
    /// Penalty multiplier for overextending when opponent likely has board wipe.
    #[serde(default = "default_threat_wipe_overextend_penalty")]
    pub threat_wipe_overextend_penalty: f64,
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
            tempo_curve_bonus: default_tempo_curve_bonus(),
            synergy_casting_bonus: default_synergy_casting_bonus(),
            threat_counter_tapout_penalty: default_threat_counter_tapout_penalty(),
            threat_wipe_overextend_penalty: default_threat_wipe_overextend_penalty(),
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
fn default_tempo_curve_bonus() -> f64 {
    0.3
}
fn default_synergy_casting_bonus() -> f64 {
    0.25
}
fn default_threat_counter_tapout_penalty() -> f64 {
    -1.5
}
fn default_threat_wipe_overextend_penalty() -> f64 {
    -0.6
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
                opponent_model: OpponentModel::DeterministicBestReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                deterministic: false,
                threat_awareness: ThreatAwareness::None,
                projection_min_budget_ms: 0,
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
                opponent_model: OpponentModel::DeterministicBestReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                deterministic: false,
                threat_awareness: ThreatAwareness::None,
                projection_min_budget_ms: 0,
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
            false,
            SearchConfig {
                enabled: true,
                max_depth: 2,
                max_nodes: 24,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 1,
                rollout_samples: 1,
                opponent_model: OpponentModel::DeterministicBestReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                deterministic: false,
                threat_awareness: ThreatAwareness::ArchetypeOnly,
                projection_min_budget_ms: 500,
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
            false,
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 48,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 2,
                rollout_samples: 1,
                opponent_model: OpponentModel::ThreatWeightedReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                deterministic: false,
                threat_awareness: ThreatAwareness::Full,
                projection_min_budget_ms: 300,
            },
        ),
        AiDifficulty::VeryHard => (
            0.3,
            AiProfile {
                risk_tolerance: 0.45,
                interaction_patience: 1.0,
                stabilize_bias: 1.2,
            },
            true,
            false,
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 64,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 2,
                rollout_samples: 2,
                opponent_model: OpponentModel::ThreatWeightedReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                deterministic: false,
                threat_awareness: ThreatAwareness::Full,
                projection_min_budget_ms: 300,
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

    // WASM platform constraints: reduce search budgets. AI computation runs in
    // a Web Worker so it does not block the UI thread. Wall-clock deadlines are
    // intentionally absent — bounds are set by `max_depth` / `max_nodes` /
    // `rollout_depth` instead, so AI quality is consistent regardless of host
    // speed. Wall-clock capping was previously needed to hide a deep-clone
    // perf regression; the Arc-share migration removed that cost.
    if platform == Platform::Wasm {
        config.search.max_depth = config.search.max_depth.min(2);
        config.search.max_nodes = config.search.max_nodes * 2 / 3;
        config.search.rollout_depth = config.search.rollout_depth.min(2);
    }

    config
}

impl AiConfig {
    /// Return a copy of this config with deterministic mode enabled: wall-clock
    /// deadlines are disabled and search is bounded solely by `max_nodes` /
    /// `max_depth`. Used by integration tests and `ai-duel` regression runs to
    /// eliminate wall-clock flake. Production and benchmarks leave this off.
    pub fn into_deterministic(mut self) -> Self {
        self.search.deterministic = true;
        self
    }
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
    fn very_hard_is_deeper_and_more_deterministic() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        assert!(config.temperature < 0.5);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusRollout);
        assert_eq!(config.search.max_depth, 3);
        assert_eq!(config.search.max_nodes, 64);
        assert_eq!(config.search.max_branching, 5);
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
    fn wasm_very_hard_reduces_depth() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        assert_eq!(config.search.max_depth, 2);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusRollout);
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
    fn four_player_very_hard_reduces_budget() {
        let base = create_config(AiDifficulty::VeryHard, Platform::Native);
        let config = create_config_for_players(AiDifficulty::VeryHard, Platform::Native, 4);
        assert_eq!(config.search.planner_mode, PlannerMode::BeamPlusRollout);
        assert!(config.search.max_nodes < base.search.max_nodes);
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
