use serde::{Deserialize, Serialize};

use crate::deck_profile::ArchetypeMultipliers;
use crate::eval::{EvalWeightSet, KeywordBonuses};
use crate::strategy_profile::StrategyProfile;

/// Wall-clock budget for AI search across ALL difficulties and platforms.
///
/// When `Some(ms)`, search terminates at the deadline even if `max_depth` /
/// `max_nodes` hasn't been reached, capping user-visible AI latency at the
/// cost of search quality on slow hardware. The same deadline gates expensive
/// tactical projections so optional lookahead cannot dominate a move.
///
/// Measurement test and duel-suite runs call [`AiConfig::into_measurement`]
/// to disable this wall-clock cap and remain bounded solely by node/depth
/// budgets.
///
/// **Single source of truth** — every `SearchConfig::time_budget_ms` in this
/// crate references this constant.
pub const AI_SEARCH_TIME_BUDGET_MS: Option<u32> = Some(1500);

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
    /// Bracket-5 competitive Commander. Bypasses 4-player paranoid scaling;
    /// activates combo-recognition policies via `DeckFeatures::is_cedh`.
    CEDH,
}

impl AiDifficulty {
    /// Parse a difficulty label supplied by a transport boundary (WASM bridge,
    /// Tauri IPC, CLI). Case-insensitive; unknown labels fall back to `Medium`.
    ///
    /// This is the single authority for the label → enum mapping. The frontend
    /// sends one label per AI seat and uses `"CEDH"` for competitive Commander
    /// games, so this MUST include the `cedh` arm — every transport that maps a
    /// difficulty string routes through here precisely so a missing arm can't
    /// silently downgrade a preset (cEDH previously fell through to `Medium`).
    pub fn from_label(label: &str) -> AiDifficulty {
        // Trim first: transport boundaries (config files, CLI args via ai_duel)
        // may carry surrounding whitespace.
        match label.trim().to_lowercase().as_str() {
            "veryeasy" => AiDifficulty::VeryEasy,
            "easy" => AiDifficulty::Easy,
            "medium" => AiDifficulty::Medium,
            "hard" => AiDifficulty::Hard,
            "veryhard" => AiDifficulty::VeryHard,
            "cedh" => AiDifficulty::CEDH,
            _ => AiDifficulty::Medium,
        }
    }
}

/// Platform the AI runs on (affects budget constraints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Native,
    Wasm,
}

/// Runtime mode for AI execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Production and interactive callers use latency-bounded search and
    /// caller-supplied entropy.
    Interactive,
    /// Regression measurement is a pure function of `(binary, config, seed)`.
    Measurement { seed: u64 },
}

impl ExecutionMode {
    pub fn is_measurement(self) -> bool {
        matches!(self, ExecutionMode::Measurement { .. })
    }
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
    /// How much the AI reasons about opponent hand threats.
    pub threat_awareness: ThreatAwareness,
    /// Minimum remaining wall-clock budget (ms) required before running an
    /// uncached multi-turn projection (e.g., `velocity_score`'s opponent-turn
    /// simulation). When `time_budget_ms.remaining < this`, policies fall back
    /// to cache-only lookups and a heuristic score — preserves the tactical
    /// signal without blowing the user-visible turn-time budget.
    ///
    /// Production configs set this above the move budget so uncached projections
    /// are skipped unless a prior node already populated the cache. Deterministic
    /// runs still allow projections because they have no wall-clock deadline.
    /// Set to 0 to always run projections.
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
            threat_awareness: ThreatAwareness::None,
            projection_min_budget_ms: 2000,
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
    /// Bonus prior when a candidate action progresses a combo line that is
    /// reachable this turn. Consumed by `ComboLinePolicy`.
    #[serde(default = "default_combo_progress_this_turn_bonus")]
    pub combo_progress_this_turn_bonus: f64,
    /// Bonus prior when a candidate action (tutor / draw / ramp) progresses a
    /// combo line that is reachable next turn. Consumed by `ComboLinePolicy`.
    #[serde(default = "default_combo_progress_next_turn_bonus")]
    pub combo_progress_next_turn_bonus: f64,
    /// CR 701.6a: Penalty for casting a spell whose mana value matches the
    /// charge-counter count on a Chalice-of-the-Void-class permanent the AI
    /// controls — the spell is countered for free, pure tempo and card loss.
    /// Consumed by `ChaliceAvoidancePolicy`.
    #[serde(default = "default_own_chalice_counter_penalty")]
    pub own_chalice_counter_penalty: f64,
    /// CR 701.6a: Penalty for casting a spell that an opponent's Chalice-class
    /// permanent would counter. Lighter than the own-Chalice penalty: the AI
    /// may still want the spell on the stack (e.g. to bait, or when the spell's
    /// value clears the loss), so this demotes rather than vetoes.
    #[serde(default = "default_opponent_chalice_counter_penalty")]
    pub opponent_chalice_counter_penalty: f64,
    /// CR 119.3 / CR 702.15a: Bonus for casting a lifegain *source* (lifelink or
    /// "you gain N life") in a deck that has lifegain payoffs — each life-gain
    /// event feeds those payoffs. Consumed by `LifegainPayoffPolicy`, which is
    /// payoff-gated so this never applies to incidental lifegain in non-lifegain
    /// decks.
    #[serde(default = "default_lifegain_source_bonus")]
    pub lifegain_source_bonus: f64,
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
            combo_progress_this_turn_bonus: default_combo_progress_this_turn_bonus(),
            combo_progress_next_turn_bonus: default_combo_progress_next_turn_bonus(),
            own_chalice_counter_penalty: default_own_chalice_counter_penalty(),
            opponent_chalice_counter_penalty: default_opponent_chalice_counter_penalty(),
            lifegain_source_bonus: default_lifegain_source_bonus(),
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
fn default_combo_progress_this_turn_bonus() -> f64 {
    15.0
}
fn default_combo_progress_next_turn_bonus() -> f64 {
    5.0
}
fn default_own_chalice_counter_penalty() -> f64 {
    -12.0
}
fn default_opponent_chalice_counter_penalty() -> f64 {
    -4.0
}
fn default_lifegain_source_bonus() -> f64 {
    0.4
}

/// Policy penalty fields present in the active CMA-ES `--group penalties`
/// vector. Adding a `PolicyPenalties` field requires listing it here or in
/// `UNTUNED_POLICY_PENALTY_FIELDS` with a reason.
pub const ACTIVE_POLICY_PENALTY_FIELDS: &[&str] = &[
    "redundant_removal_penalty",
    "redundant_damage_penalty",
    "gift_card_penalty",
    "gift_treasure_penalty",
    "gift_food_penalty",
    "gift_fish_penalty",
    "worthy_target_threshold",
    "overkill_base_penalty",
    "removal_quality_mismatch",
    "bounce_token_bonus",
    "bounce_cheap_discount",
    "bounce_expensive_bonus_per_mv",
    "ward_cost_penalty_base",
    "pump_response_bonus",
    "lethal_burn_bonus",
    "protect_spell_bonus_mult",
    "lethality_tapout_penalty",
    "sacrifice_land_penalty",
    "sacrifice_token_cost",
    "evasion_removal_bonus_mult",
    "recursion_destroy_penalty",
    "recursion_exile_bonus",
    "death_trigger_destroy_penalty",
    "wrath_overextend_penalty",
    "low_life_defensive_bonus",
    "low_life_aggro_penalty",
    "card_advantage_behind_extra",
    "counter_last_reservation_penalty",
    "tempo_curve_bonus",
    "synergy_casting_bonus",
    "threat_counter_tapout_penalty",
    "threat_wipe_overextend_penalty",
    "combo_progress_this_turn_bonus",
    "combo_progress_next_turn_bonus",
    "own_chalice_counter_penalty",
    "opponent_chalice_counter_penalty",
];

/// Policy penalties intentionally not present in an active CMA-ES parameter
/// vector yet.
pub const UNTUNED_POLICY_PENALTY_FIELDS: &[(&str, &str)] = &[(
    "lifegain_source_bonus",
    "new LifegainPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
)];

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
    pub execution_mode: ExecutionMode,
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
/// Six presets scale from random play (VeryEasy) to competitive Commander (CEDH).
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
                threat_awareness: ThreatAwareness::ArchetypeOnly,
                projection_min_budget_ms: 2000,
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
                threat_awareness: ThreatAwareness::Full,
                projection_min_budget_ms: 2000,
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
                threat_awareness: ThreatAwareness::Full,
                projection_min_budget_ms: 2000,
            },
        ),
        AiDifficulty::CEDH => (
            0.2,
            AiProfile {
                risk_tolerance: 0.4,
                interaction_patience: 1.0,
                stabilize_bias: 1.2,
            },
            true, // play_lookahead
            true, // combat_lookahead — cEDH is the first tier to enable this
            SearchConfig {
                enabled: true,
                max_depth: 3,
                max_nodes: 96,
                max_branching: 5,
                planner_mode: PlannerMode::BeamPlusRollout,
                rollout_depth: 2,
                rollout_samples: 2,
                opponent_model: OpponentModel::ThreatWeightedReply,
                time_budget_ms: AI_SEARCH_TIME_BUDGET_MS,
                threat_awareness: ThreatAwareness::Full,
                // == AI_SEARCH_TIME_BUDGET_MS: projections only at turn start,
                // before nodes consume the budget
                projection_min_budget_ms: 1500,
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
        execution_mode: ExecutionMode::Interactive,
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
    /// Return a copy of this config with measurement mode enabled: wall-clock
    /// deadlines are disabled and search is bounded solely by `max_nodes` /
    /// `max_depth`. Used by integration tests and `ai-duel` regression runs to
    /// eliminate wall-clock flake. Production and benchmarks leave this off.
    pub fn into_measurement(mut self, seed: u64) -> Self {
        self.execution_mode = ExecutionMode::Measurement { seed };
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
            // cEDH: no scaling needed — the preset is calibrated for 4-player tables.
            // All other difficulties get the paranoid cap.
            if difficulty != AiDifficulty::CEDH {
                // Paranoid search: cap depth at 2, reduce budget
                config.search.max_depth = config.search.max_depth.min(2);
                config.search.max_nodes = config.search.max_nodes * 2 / 3;
                config.search.max_branching = config.search.max_branching.min(4);
                config.search.rollout_depth = config.search.rollout_depth.min(1);
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
            AiDifficulty::CEDH,
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
        for diff in [
            AiDifficulty::VeryEasy,
            AiDifficulty::Easy,
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
            AiDifficulty::CEDH,
        ] {
            let json = serde_json::to_string(&diff).unwrap();
            let parsed: AiDifficulty = serde_json::from_str(&json).unwrap();
            assert_eq!(diff, parsed);
        }
    }

    #[test]
    fn from_label_maps_every_difficulty_including_cedh() {
        // The transport layers (WASM, Tauri, CLI) all route difficulty strings
        // through this one mapping; a missing arm silently downgrades a preset.
        assert_eq!(AiDifficulty::from_label("VeryEasy"), AiDifficulty::VeryEasy);
        assert_eq!(AiDifficulty::from_label("Easy"), AiDifficulty::Easy);
        assert_eq!(AiDifficulty::from_label("Medium"), AiDifficulty::Medium);
        assert_eq!(AiDifficulty::from_label("Hard"), AiDifficulty::Hard);
        assert_eq!(AiDifficulty::from_label("VeryHard"), AiDifficulty::VeryHard);
        // The cEDH bug: "CEDH" must not fall through to Medium.
        assert_eq!(AiDifficulty::from_label("CEDH"), AiDifficulty::CEDH);
        // Case-insensitive (matches the lobby's case-insensitive "cedh" checks).
        assert_eq!(AiDifficulty::from_label("cedh"), AiDifficulty::CEDH);
        assert_eq!(AiDifficulty::from_label("cEDH"), AiDifficulty::CEDH);
        // Surrounding whitespace from transport/config boundaries is trimmed.
        assert_eq!(AiDifficulty::from_label("  CEDH  "), AiDifficulty::CEDH);
        // The CEDH preset actually engages, not the Medium fallback.
        assert_eq!(
            create_config(AiDifficulty::from_label("CEDH"), Platform::Native)
                .search
                .max_nodes,
            96
        );
        // Unknown labels fall back to Medium.
        assert_eq!(AiDifficulty::from_label("nonsense"), AiDifficulty::Medium);
    }

    #[test]
    fn cedh_preset_values() {
        let config = create_config(AiDifficulty::CEDH, Platform::Native);
        assert_eq!(config.difficulty, AiDifficulty::CEDH);
        assert_eq!(config.temperature, 0.2);
        assert_eq!(config.profile.risk_tolerance, 0.4);
        assert_eq!(config.profile.interaction_patience, 1.0);
        assert_eq!(config.profile.stabilize_bias, 1.2);
        assert!(config.play_lookahead);
        assert!(config.combat_lookahead);
        assert!(config.search.enabled);
        assert_eq!(config.search.max_depth, 3);
        assert_eq!(config.search.max_nodes, 96);
        assert_eq!(config.search.max_branching, 5);
        assert_eq!(config.search.rollout_depth, 2);
        assert_eq!(config.search.rollout_samples, 2);
        assert!(matches!(
            config.search.opponent_model,
            OpponentModel::ThreatWeightedReply
        ));
        assert!(matches!(
            config.search.threat_awareness,
            ThreatAwareness::Full
        ));
        assert_eq!(config.search.projection_min_budget_ms, 1500);
        assert_eq!(config.search.time_budget_ms, AI_SEARCH_TIME_BUDGET_MS);
    }

    #[test]
    fn cedh_preset_wasm_caps_apply() {
        let config = create_config(AiDifficulty::CEDH, Platform::Wasm);
        assert_eq!(config.search.max_depth, 2); // capped from 3
        assert_eq!(config.search.max_nodes, 64); // 96 * 2/3
        assert_eq!(config.search.rollout_depth, 2);
    }

    #[test]
    fn cedh_skips_paranoid_scaling_at_4p() {
        let cfg = create_config_for_players(AiDifficulty::CEDH, Platform::Native, 4);
        assert_eq!(
            cfg.search.max_depth, 3,
            "cEDH must not be downgraded to depth 2 by paranoid scaling at 4p"
        );
        assert_eq!(
            cfg.search.max_nodes, 96,
            "cEDH must keep its native node budget at 4p"
        );
        assert_eq!(cfg.search.max_branching, 5);
        assert_eq!(cfg.search.rollout_depth, 2);
    }

    #[test]
    fn cedh_skips_paranoid_scaling_at_3p() {
        let cfg = create_config_for_players(AiDifficulty::CEDH, Platform::Native, 3);
        assert_eq!(
            cfg.search.max_depth, 3,
            "cEDH must not be downgraded at 3p any more than at 4p"
        );
        assert_eq!(cfg.search.max_nodes, 96);
        assert_eq!(cfg.search.max_branching, 5);
        assert_eq!(cfg.search.rollout_depth, 2);
    }

    #[test]
    fn veryhard_still_gets_paranoid_scaling_at_4p() {
        // Sanity: the scaling skip is cEDH-specific and doesn't affect VeryHard.
        let cfg = create_config_for_players(AiDifficulty::VeryHard, Platform::Native, 4);
        assert_eq!(
            cfg.search.max_depth, 2,
            "VeryHard should still be capped at 4p"
        );
    }

    #[test]
    fn policy_penalties_default_combo_progress_bonuses() {
        let p = PolicyPenalties::default();
        assert_eq!(p.combo_progress_this_turn_bonus, 15.0);
        assert_eq!(p.combo_progress_next_turn_bonus, 5.0);
    }

    #[test]
    fn every_policy_penalty_is_tuning_registered_or_explicitly_untuned() {
        let value = serde_json::to_value(PolicyPenalties::default()).unwrap();
        let fields: std::collections::BTreeSet<_> = value
            .as_object()
            .expect("PolicyPenalties serializes as object")
            .keys()
            .map(String::as_str)
            .collect();
        let untuned: std::collections::BTreeSet<_> = UNTUNED_POLICY_PENALTY_FIELDS
            .iter()
            .map(|(field, _reason)| *field)
            .collect();
        let active: std::collections::BTreeSet<_> =
            ACTIVE_POLICY_PENALTY_FIELDS.iter().copied().collect();
        let registered: std::collections::BTreeSet<_> = active.union(&untuned).copied().collect();

        assert_eq!(
            fields, registered,
            "PolicyPenalties fields must be present in an active CMA-ES group or UNTUNED_POLICY_PENALTY_FIELDS"
        );
        assert!(
            UNTUNED_POLICY_PENALTY_FIELDS
                .iter()
                .all(|(_field, reason)| !reason.trim().is_empty()),
            "every untuned policy penalty entry needs a reason"
        );
    }
}
