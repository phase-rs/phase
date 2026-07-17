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
/// Search runs iterative deepening (rung `0 -> max_depth-1`): this budget now
/// bounds the *rungs* — the deepest fully-completed rung's scores are returned
/// on expiry (rather than a single fixed-depth pass collapsing to a
/// tactical-only score). Measurement mode pins the iteration ceiling and never
/// consults the wall clock, preserving byte-determinism.
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
    /// Number of determinized opponent-hidden-zone samples to average the
    /// `score_candidates` ensemble over. `0` disables determinization entirely
    /// (perfect-information search, byte-identical to the pre-feature path) — the
    /// disabled sentinel, matching the `max_nodes`/`rollout_samples` numeric-knob
    /// convention rather than a bool flag. `K > 0` replaces the opponent's real
    /// hidden hand/library with K resampled plausible worlds and means the
    /// per-action scores across them (§7 of the determinization plan). Higher
    /// tiers set larger K; Medium keeps `0` to preserve the default-tier strength
    /// floor.
    pub determinization_samples: u32,
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
            determinization_samples: 0,
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
    /// CR 702.41a / CR 702.126a: Bonus for casting an affinity-for-artifacts or
    /// improvise spell in an artifacts-matter deck — the cost payoff gets
    /// cheaper/easier the wider the artifact board. Consumed by
    /// `ArtifactSynergyPolicy`.
    #[serde(default = "default_artifact_cost_payoff_bonus")]
    pub artifact_cost_payoff_bonus: f64,
    /// CR 301.1: Nudge for deploying an artifact in an artifacts-matter deck,
    /// growing the count that affinity/improvise/metalcraft payoffs scale on.
    /// Consumed by `ArtifactSynergyPolicy`.
    #[serde(default = "default_deploy_artifact_bonus")]
    pub deploy_artifact_bonus: f64,
    /// CR 119.3 / CR 702.15a: Bonus for casting a lifegain *source* (lifelink or
    /// "you gain N life") in a deck that has lifegain payoffs — each life-gain
    /// event feeds those payoffs. Consumed by `LifegainPayoffPolicy`, which is
    /// payoff-gated so this never applies to incidental lifegain in non-lifegain
    /// decks.
    #[serde(default = "default_lifegain_source_bonus")]
    pub lifegain_source_bonus: f64,
    /// CR 601.2i / CR 603.6a: Bonus for casting an enchantment in a deck that has
    /// enchantment payoffs (enchantress / constellation) — each enchantment feeds
    /// those payoffs. Consumed by `EnchantmentsPayoffPolicy`, which is
    /// payoff-gated so this never applies to decks with no enchantment payoff.
    #[serde(default = "default_enchantment_cast_bonus")]
    pub enchantment_cast_bonus: f64,
    /// CR 404.1 + CR 110.1: Bonus for casting a reanimation spell (graveyard →
    /// battlefield) in a reanimator deck that has a worthwhile target — cheating
    /// a fat body into play ahead of curve. Consumed by `ReanimatorPayoffPolicy`,
    /// which is payoff-gated so this never applies to non-reanimator decks.
    #[serde(default = "default_reanimation_cast_bonus")]
    pub reanimation_cast_bonus: f64,
    /// CR 701.17a / CR 701.9a: Bonus for casting a graveyard enabler (self-mill /
    /// discard outlet) in a reanimator deck — loading the graveyard so a
    /// reanimation has fuel. Consumed by `ReanimatorPayoffPolicy`; smaller than
    /// the reanimation bonus because it is setup, not the payoff.
    #[serde(default = "default_graveyard_enabler_bonus")]
    pub graveyard_enabler_bonus: f64,
    /// CR 301.5: Bonus for deploying an Equipment in an equipment-committed deck
    /// (one with both Equipment density and payoffs) — growing the voltron
    /// package. Consumed by `EquipmentPayoffPolicy`, which is payoff-gated so
    /// this never applies to decks running incidental Equipment.
    #[serde(default = "default_deploy_equipment_bonus")]
    pub deploy_equipment_bonus: f64,
    /// CR 701.23 / CR 702.6: Bonus for casting an equipment-matters support card
    /// (tutor / auto-attacher / equip-cost grant / equipment-cast payoff) in an
    /// equipment-committed deck. Consumed by `EquipmentPayoffPolicy`.
    #[serde(default = "default_equipment_payoff_cast_bonus")]
    pub equipment_payoff_cast_bonus: f64,
    /// CR 603.7: Bonus for deploying a flicker enabler in a blink-committed deck
    /// (one with both flicker density and ETB payoffs) — the engine that
    /// re-triggers ETBs. Consumed by `BlinkPayoffPolicy`, which is payoff-gated so
    /// this never applies to decks running incidental flicker.
    #[serde(default = "default_deploy_flicker_engine_bonus")]
    pub deploy_flicker_engine_bonus: f64,
    /// CR 603.6a: Bonus for casting a value-ETB creature in a blink-committed
    /// deck — a re-triggerable payoff, worth a premium on top of its one-shot ETB
    /// value because the deck can flicker it. Consumed by `BlinkPayoffPolicy`.
    #[serde(default = "default_etb_payoff_cast_bonus")]
    pub etb_payoff_cast_bonus: f64,
    /// Bonus for casting an opponent-mill spell in a mill-committed deck.
    /// Scales with library-size urgency (×2 below 15 cards, ×3 below 5 cards).
    /// Consumed by `MillPayoffPolicy`.
    #[serde(default = "default_mill_cast_bonus")]
    pub mill_cast_bonus: f64,
    /// Bonus for casting an energy-relevant spell (producer or sink body) in an
    /// energy-committed deck. Scales with the casting player's reserve momentum
    /// (×2 at 2–4 {E}, ×3 at ≥5 {E}).
    /// Consumed by `EnergyPayoffPolicy`.
    #[serde(default = "default_energy_cast_bonus")]
    pub energy_cast_bonus: f64,
    /// Penalty for a "wasted cast" the AI should avoid — a spell that whiffs or
    /// backfires: a legendary duplicate the legend rule will immediately kill, an
    /// ETB whose only target is illegal, or a creature-targeting spell with no
    /// legal creature target (beneficial with no own creature, harmful
    /// creature-only with no opponent creature, or bounce with no opponent
    /// permanent). Consumed by `AntiSelfHarmPolicy`.
    #[serde(default = "default_wasted_cast_penalty")]
    pub wasted_cast_penalty: f64,
    /// Bonus for untapping the AI's own tapped creature (frees a blocker /
    /// re-enables a tapped attacker). Consumed by `AntiSelfHarmPolicy`.
    #[serde(default = "default_untap_own_tapped_bonus")]
    pub untap_own_tapped_bonus: f64,
    /// Penalty for an untap effect that would untap an opponent's tapped creature
    /// (hands them back a blocker/attacker). Consumed by `AntiSelfHarmPolicy`.
    #[serde(default = "default_untap_opponent_tapped_penalty")]
    pub untap_opponent_tapped_penalty: f64,
    /// Penalty for targeting an already-untapped creature with an untap effect —
    /// no state change, so the effect is wasted. Consumed by `AntiSelfHarmPolicy`.
    #[serde(default = "default_untap_untapped_penalty")]
    pub untap_untapped_penalty: f64,
    /// Penalty for non-lethal removal aimed at a tapped opponent creature during
    /// the pre-combat main phase — a tapped creature can't block, so there is no
    /// urgency advantage over waiting. Consumed by `AntiSelfHarmPolicy`.
    #[serde(default = "default_tapped_removal_no_urgency_penalty")]
    pub tapped_removal_no_urgency_penalty: f64,
    /// CR 119.4: Per-point cost of a self-inflicted pay-life activation cost,
    /// before runtime life-pressure scaling. Mirrors the `player_impact`
    /// GainLife/LoseLife weight (0.15). Consumed by `SelfCostValuePolicy`.
    #[serde(default = "default_self_cost_pay_life_per_point")]
    pub self_cost_pay_life_per_point: f64,
    /// CR 701.9a: Per-card cost of a self-inflicted discard activation cost (one
    /// card ≈ one unit of expected value). Consumed by `SelfCostValuePolicy`.
    #[serde(default = "default_self_cost_discard_per_card")]
    pub self_cost_discard_per_card: f64,
    /// CR 701.13a: Per-card cost of exiling a card from the AI's own graveyard
    /// as an activation cost — cheap unless the deck is graveyard-committed.
    /// Consumed by `SelfCostValuePolicy`.
    #[serde(default = "default_self_cost_exile_graveyard_per_card")]
    pub self_cost_exile_graveyard_per_card: f64,
    /// One card-equivalent of patience that cancels Cycling's generic activation
    /// edge while leaving tactical payoffs free to justify cycling.
    /// Consumed by `CyclingDisciplinePolicy`.
    #[serde(default = "default_cycling_patience_penalty")]
    pub cycling_patience_penalty: f64,
    /// Stronger finite penalty for cycling away the sole land still needed by
    /// the current deck plan. Consumed by `CyclingDisciplinePolicy`.
    #[serde(default = "default_cycling_needed_land_penalty")]
    pub cycling_needed_land_penalty: f64,
    /// CR 732.2a / CR 104.2a: bonus for proposing an `UntilLethal` loop shortcut whose latched
    /// `predicted_winner` IS the proposer — the crown ends the game in their favor, and the only
    /// other outcome (`until_lethal_fallback`) restores the board a decline would have produced.
    /// Game-deciding ⇒ the default lands in the `critical` band (5.0, 15.0]. Consumed by
    /// `LoopShortcutPolicy`; fed through `PolicyVerdict::score`, which auto-bands and clamps to
    /// `CRITICAL_MAX`. (The losing / no-crown cases are `PolicyVerdict::reject`s and take no
    /// scalar.)
    #[serde(default = "default_loop_shortcut_winning_declare_bonus")]
    pub loop_shortcut_winning_declare_bonus: f64,
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
            artifact_cost_payoff_bonus: default_artifact_cost_payoff_bonus(),
            deploy_artifact_bonus: default_deploy_artifact_bonus(),
            lifegain_source_bonus: default_lifegain_source_bonus(),
            enchantment_cast_bonus: default_enchantment_cast_bonus(),
            reanimation_cast_bonus: default_reanimation_cast_bonus(),
            graveyard_enabler_bonus: default_graveyard_enabler_bonus(),
            deploy_equipment_bonus: default_deploy_equipment_bonus(),
            equipment_payoff_cast_bonus: default_equipment_payoff_cast_bonus(),
            deploy_flicker_engine_bonus: default_deploy_flicker_engine_bonus(),
            etb_payoff_cast_bonus: default_etb_payoff_cast_bonus(),
            mill_cast_bonus: default_mill_cast_bonus(),
            energy_cast_bonus: default_energy_cast_bonus(),
            wasted_cast_penalty: default_wasted_cast_penalty(),
            untap_own_tapped_bonus: default_untap_own_tapped_bonus(),
            untap_opponent_tapped_penalty: default_untap_opponent_tapped_penalty(),
            untap_untapped_penalty: default_untap_untapped_penalty(),
            tapped_removal_no_urgency_penalty: default_tapped_removal_no_urgency_penalty(),
            self_cost_pay_life_per_point: default_self_cost_pay_life_per_point(),
            self_cost_discard_per_card: default_self_cost_discard_per_card(),
            self_cost_exile_graveyard_per_card: default_self_cost_exile_graveyard_per_card(),
            cycling_patience_penalty: default_cycling_patience_penalty(),
            cycling_needed_land_penalty: default_cycling_needed_land_penalty(),
            loop_shortcut_winning_declare_bonus: default_loop_shortcut_winning_declare_bonus(),
        }
    }
}

fn default_wasted_cast_penalty() -> f64 {
    -8.0
}
fn default_untap_own_tapped_bonus() -> f64 {
    8.0
}
fn default_untap_opponent_tapped_penalty() -> f64 {
    -20.0
}
fn default_untap_untapped_penalty() -> f64 {
    -6.0
}
fn default_tapped_removal_no_urgency_penalty() -> f64 {
    -5.0
}
fn default_self_cost_pay_life_per_point() -> f64 {
    0.15
}
fn default_self_cost_discard_per_card() -> f64 {
    1.0
}
fn default_self_cost_exile_graveyard_per_card() -> f64 {
    0.15
}

fn default_cycling_patience_penalty() -> f64 {
    -1.0
}
fn default_cycling_needed_land_penalty() -> f64 {
    -2.0
}

/// 8.0 = mid-`critical` band. Sized for the HEURISTIC branch, which adds the tactical score RAW:
/// it turns the measured 0.5-vs-0.4 coinflip on a GUARANTEED win into ~88% declare at VeryEasy
/// (T = 4.0) and ~98% at Easy (T = 2.0). That is the branch where nothing else can differentiate
/// the two candidates.
///
/// On the SEARCH branch (Medium and up) the score is multiplied by `tactical_weight`: 0.1 at a
/// quiesced `LoopShortcut` node, or 0.35 if an opponent's object is on the stack (the offer is
/// raised at a priority window, which does not imply an empty stack). So this becomes a
/// +0.8..+2.8 move-ordering / tie-break nudge on top of the beam's own continuation value, which
/// already sees the CR 104.2a crown. It is deliberately NOT sized to dominate that value, and
/// deliberately NOT saturated to `CRITICAL_MAX`: a VeryEasy AI is allowed to miss a free win.
///
/// The symmetric losing case is a `Reject` (`-inf`), which is temperature- AND weight-IMMUNE
/// (`-inf * 0.1 == -inf * 0.35 == -inf`; `exp(-inf / T) == 0` for every T > 0) — no difficulty
/// ever throws the game away.
fn default_loop_shortcut_winning_declare_bonus() -> f64 {
    8.0
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
fn default_artifact_cost_payoff_bonus() -> f64 {
    0.5
}
fn default_deploy_artifact_bonus() -> f64 {
    0.2
}
fn default_lifegain_source_bonus() -> f64 {
    0.4
}
fn default_enchantment_cast_bonus() -> f64 {
    0.4
}
fn default_reanimation_cast_bonus() -> f64 {
    0.5
}
fn default_graveyard_enabler_bonus() -> f64 {
    0.3
}
fn default_deploy_equipment_bonus() -> f64 {
    0.3
}
fn default_equipment_payoff_cast_bonus() -> f64 {
    0.4
}
fn default_deploy_flicker_engine_bonus() -> f64 {
    0.4
}
fn default_etb_payoff_cast_bonus() -> f64 {
    0.3
}
fn default_mill_cast_bonus() -> f64 {
    0.5
}
fn default_energy_cast_bonus() -> f64 {
    0.5
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
pub const UNTUNED_POLICY_PENALTY_FIELDS: &[(&str, &str)] = &[
    (
        "artifact_cost_payoff_bonus",
        "new ArtifactSynergyPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "deploy_artifact_bonus",
        "new ArtifactSynergyPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "enchantment_cast_bonus",
        "new EnchantmentsPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "lifegain_source_bonus",
        "new LifegainPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "reanimation_cast_bonus",
        "new ReanimatorPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "graveyard_enabler_bonus",
        "new ReanimatorPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "deploy_equipment_bonus",
        "new EquipmentPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "equipment_payoff_cast_bonus",
        "new EquipmentPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "deploy_flicker_engine_bonus",
        "new BlinkPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "etb_payoff_cast_bonus",
        "new BlinkPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "mill_cast_bonus",
        "new MillPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "energy_cast_bonus",
        "new EnergyPayoffPolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "wasted_cast_penalty",
        "AntiSelfHarmPolicy magnitude lifted from a raw literal (value-preserving); awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "untap_own_tapped_bonus",
        "AntiSelfHarmPolicy magnitude lifted from a raw literal (value-preserving); awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "untap_opponent_tapped_penalty",
        "AntiSelfHarmPolicy magnitude lifted from a raw literal (value-preserving); awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "untap_untapped_penalty",
        "AntiSelfHarmPolicy magnitude lifted from a raw literal (value-preserving); awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "tapped_removal_no_urgency_penalty",
        "AntiSelfHarmPolicy magnitude lifted from a raw literal (value-preserving); awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "self_cost_pay_life_per_point",
        "new SelfCostValuePolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "self_cost_discard_per_card",
        "new SelfCostValuePolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "self_cost_exile_graveyard_per_card",
        "new SelfCostValuePolicy knob; awaiting a paired-seed ai-gate calibration before joining the CMA-ES vector",
    ),
    (
        "cycling_patience_penalty",
        "CyclingDisciplinePolicy one-card-equivalent value cancels the generic +1 activation prior; explicitly untuned pending broader paired-seed calibration",
    ),
    (
        "cycling_needed_land_penalty",
        "CyclingDisciplinePolicy sole-needed-land value occupies the finite strong band; explicitly untuned pending broader paired-seed calibration",
    ),
    (
        "loop_shortcut_winning_declare_bonus",
        "LoopShortcutPolicy band selector for a game-deciding CR 104.2a crown; deliberately kept OUT of the CMA-ES penalties vector — win-rate gradients from games that never reach a WaitingFor::LoopShortcut node would tune a win-detector into noise",
    ),
];

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
                determinization_samples: 0,
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
                determinization_samples: 0,
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
                // Medium keeps perfect-information search (K=0): the default
                // tier's strength floor (§7c/F1) — determinization is Hard+.
                determinization_samples: 0,
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
                // K=2: halves single-sample variance at 2x base cost; node cap
                // 48 keeps each search short. Exercised by the quick ai-gate.
                determinization_samples: 2,
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
                // K=3: materially de-biases without runaway cost; node cap 64.
                determinization_samples: 3,
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
                // K=3: same as VeryHard; multiplayer + node cap 96 dominates cost.
                determinization_samples: 3,
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
        // The frontend worker pool already provides cross-sample root
        // parallelism (ai-worker-pool.ts merges N workers), so cap per-worker K
        // at 2 — effective samples = N_workers x K without per-worker latency
        // blow-up (§7c).
        config.search.determinization_samples = config.search.determinization_samples.min(2);
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
                // Determinizing 3+ opponents per sample multiplies pool work;
                // keep K modest beyond 2 players (§7c). cEDH keeps its tier K.
                config.search.determinization_samples =
                    config.search.determinization_samples.min(1);
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
                // 5-6+ players: one determinized sample at most (pool work scales
                // with opponent count).
                config.search.determinization_samples =
                    config.search.determinization_samples.min(1);
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
        // Medium stays at perfect-information search (K=0) — the default-tier
        // strength floor (§7c/F1).
        assert_eq!(config.search.determinization_samples, 0);
    }

    #[test]
    fn hard_increases_depth() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        assert_eq!(config.temperature, 0.5);
        assert!(config.profile.stabilize_bias > 1.0);
        assert_eq!(config.search.max_depth, 3);
        assert_eq!(config.search.max_nodes, 48);
        assert_eq!(config.search.rollout_depth, 2);
        // Hard is the first tier to determinize opponent hidden zones (K=2) —
        // the tier the quick ai-gate exercises (§7c/§11).
        assert_eq!(config.search.determinization_samples, 2);
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
        assert_eq!(config.search.determinization_samples, 3);
    }

    #[test]
    fn wasm_reduces_budgets() {
        let native = create_config(AiDifficulty::Hard, Platform::Native);
        let wasm = create_config(AiDifficulty::Hard, Platform::Wasm);

        assert!(wasm.search.max_depth <= 2);
        assert!(wasm.search.max_nodes < native.search.max_nodes);
        assert!(wasm.search.rollout_depth <= native.search.rollout_depth);
        // WASM caps per-worker K at 2 (Hard native K=2 -> still 2 here).
        assert!(wasm.search.determinization_samples <= 2);
        assert_eq!(wasm.search.determinization_samples, 2);
    }

    #[test]
    fn wasm_caps_determinization_samples_at_two() {
        // VeryHard native K=3 must be capped to 2 on WASM (§7c min(2,tier)).
        let native = create_config(AiDifficulty::VeryHard, Platform::Native);
        let wasm = create_config(AiDifficulty::VeryHard, Platform::Wasm);
        assert_eq!(native.search.determinization_samples, 3);
        assert_eq!(wasm.search.determinization_samples, 2);
    }

    #[test]
    fn multiplayer_caps_determinization_samples() {
        // Hard at 4 players: paranoid scaling caps K at 1 (§7c).
        let four = create_config_for_players(AiDifficulty::Hard, Platform::Native, 4);
        assert_eq!(four.search.determinization_samples, 1);
        // cEDH skips paranoid scaling entirely, so it keeps its tier K=3 at 4p.
        let cedh4 = create_config_for_players(AiDifficulty::CEDH, Platform::Native, 4);
        assert_eq!(cedh4.search.determinization_samples, 3);
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
        assert_eq!(config.search.determinization_samples, 3);
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

    /// Value-identity guard for the `AntiSelfHarmPolicy` magnitudes migrated from
    /// raw literals into config. Each default MUST equal the exact literal the
    /// bespoke code used before the lift, so a mistyped port is caught here.
    #[test]
    fn policy_penalties_default_anti_self_harm_migrated_magnitudes() {
        let p = PolicyPenalties::default();
        assert_eq!(p.wasted_cast_penalty, -8.0);
        assert_eq!(p.untap_own_tapped_bonus, 8.0);
        assert_eq!(p.untap_opponent_tapped_penalty, -20.0);
        assert_eq!(p.untap_untapped_penalty, -6.0);
        assert_eq!(p.tapped_removal_no_urgency_penalty, -5.0);
    }

    #[test]
    fn policy_penalties_default_cycling_discipline_magnitudes() {
        let p = PolicyPenalties::default();
        assert_eq!(p.cycling_patience_penalty, -1.0);
        assert_eq!(p.cycling_needed_land_penalty, -2.0);
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
