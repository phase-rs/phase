use engine::game::players;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde::{Deserialize, Serialize};

use crate::planner::ValueEstimate;
use crate::projection::Projection;
use crate::zone_eval;

/// Weights for board evaluation heuristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalWeights {
    pub life: f64,
    pub aggression: f64,
    pub board_presence: f64,
    pub board_power: f64,
    pub board_toughness: f64,
    pub hand_size: f64,
    /// Weight for zone-quality strategic dimension (hand quality + graveyard value).
    pub zone_quality: f64,
    /// Weight for card-advantage strategic dimension (resource differential).
    pub card_advantage: f64,
    /// Weight for synergy strategic dimension (board synergy bonus).
    pub synergy: f64,
}

impl Default for EvalWeights {
    fn default() -> Self {
        EvalWeights {
            life: 1.0,
            aggression: 0.5,
            board_presence: 2.0,
            board_power: 1.5,
            board_toughness: 1.0,
            hand_size: 0.5,
            zone_quality: 0.3,
            card_advantage: 0.3,
            synergy: 0.5,
        }
    }
}

impl EvalWeights {
    /// Weights learned from 17Lands Premier Draft replay data (late-game phase).
    /// Used as a single-phase fallback; prefer `EvalWeightSet::learned()` for
    /// phase-aware evaluation.
    pub fn learned() -> Self {
        EvalWeightSet::learned().late
    }
}

/// Turn-phase-aware weight sets: early (T1-3), mid (T4-7), late (T8+).
/// Learned from 90.4M 17Lands game-turn samples split by turn number.
/// Each phase has different weight profiles reflecting how the importance
/// of board state features shifts across a game of Magic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalWeightSet {
    pub early: EvalWeights,
    pub mid: EvalWeights,
    pub late: EvalWeights,
}

impl Default for EvalWeightSet {
    fn default() -> Self {
        Self::uniform(EvalWeights::default())
    }
}

impl EvalWeightSet {
    /// All three phases use the same weights.
    pub fn uniform(weights: EvalWeights) -> Self {
        EvalWeightSet {
            early: weights.clone(),
            mid: weights.clone(),
            late: weights,
        }
    }

    /// Select weights for the current turn number.
    pub fn for_turn(&self, turn: u32) -> &EvalWeights {
        match turn {
            0..=3 => &self.early,
            4..=7 => &self.mid,
            _ => &self.late,
        }
    }

    /// Phase-aware weights learned from 17Lands Premier Draft replay data.
    /// Trained on 90.4M samples across 6 sets (DFT, EOE, FDN, FIN, PIO, TDM)
    /// from skilled players (win_rate >= 0.55, games >= 50).
    /// Five fields per phase are data-driven; four retain hand-tuned defaults.
    /// See scripts/train_eval_weights.py and data/learned-weights.json.
    pub fn learned() -> Self {
        EvalWeightSet {
            early: EvalWeights {
                life: 0.4636,
                aggression: 0.5,
                board_presence: 2.0636,
                board_power: 1.0174,
                board_toughness: 1.0,
                hand_size: 1.3716,
                zone_quality: 0.3,
                card_advantage: 2.5,
                synergy: 0.5,
            },
            mid: EvalWeights {
                life: 0.5838,
                aggression: 0.5,
                board_presence: 1.9888,
                board_power: 0.8031,
                board_toughness: 1.0,
                hand_size: 2.396,
                zone_quality: 0.3,
                card_advantage: 2.5,
                synergy: 0.5,
            },
            late: EvalWeights {
                life: 0.4912,
                aggression: 0.5,
                board_presence: 1.7317,
                board_power: 0.6686,
                board_toughness: 1.0,
                hand_size: 2.5,
                zone_quality: 0.3,
                card_advantage: 1.945,
                synergy: 0.5,
            },
        }
    }
}

const WIN_SCORE: f64 = 10000.0;
const LOSS_SCORE: f64 = -10000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategicIntent {
    PushLethal,
    Stabilize,
    PreserveAdvantage,
    Develop,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvaluationBreakdown {
    pub life: f64,
    pub board_presence: f64,
    pub board_power: f64,
    pub board_toughness: f64,
    pub hand_size: f64,
    pub aggression: f64,
    pub card_advantage: f64,
    /// Fixed-coefficient mana-development offset, carried in its own field rather
    /// than folded onto `hand_size` as the energy offset historically is — it
    /// reaches 90.0 at the source target, which would make `hand_size`
    /// uninterpretable.
    pub mana_development: f64,
}

impl EvaluationBreakdown {
    /// Sum of every component.
    ///
    /// EXHAUSTIVE DESTRUCTURING — deliberately no `..`, and it must stay that way.
    /// A hand-written `self.a + self.b + …` drops a newly added field from the
    /// **production score** with no error and no warning; this makes the same
    /// mistake an **E0027**. Struct analogue of CLAUDE.md's "exhaustive `match`
    /// without wildcard fallbacks — let the compiler catch missing arms".
    pub fn total(&self) -> f64 {
        let Self {
            life,
            board_presence,
            board_power,
            board_toughness,
            hand_size,
            aggression,
            card_advantage,
            mana_development,
        } = self;
        life + board_presence
            + board_power
            + board_toughness
            + hand_size
            + aggression
            + card_advantage
            + mana_development
    }
}

/// Single-authority **unweighted** feature vector for the tactical board eval —
/// the Texel train/serve invariant. `evaluate_state_breakdown` is defined as
/// `evaluate_features(..)? × weights`, so a feature harvested for offline weight
/// fitting is byte-for-byte the value that multiplies the corresponding weight at
/// serve time (see `crate::duel_suite::harvest::FeatureRow`, which extends this
/// with the three strategic dimensions from `evaluate_with_strategy`).
///
/// Every field except the **two fixed offsets** is a raw (self − opponent)
/// differential that pairs with one `EvalWeights` field. `energy_offset`
/// (`energy × 0.1`, CR 107.14) and `mana_development_offset`
/// (`7.5 × min(sources, 12)`, CR 106.1) are fixed-coefficient serve-time offsets
/// added **after** weighting, so both are excluded from
/// [`EvalFeatures::weighted_total`] — see the offset contract in
/// `evaluate_state_breakdown`.
///
/// The full fitted serve vector is `EvalFeatures` (minus both fixed offsets) plus
/// `zone_bonus` (`zone_quality`), `SynergyGraph::board_synergy_bonus` (`synergy`),
/// and `card_advantage::differential` (folded into `card_advantage` alongside
/// `card_advantage_breakdown`). The three unfitted serve-time terms are
/// `energy_offset` and `mana_development_offset` (the fixed offsets) and
/// `threat_adjustment` (a heuristic with no `EvalWeights` field).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EvalFeatures {
    pub life: f64,
    pub board_presence: f64,
    pub board_power: f64,
    pub board_toughness: f64,
    pub hand_size: f64,
    pub aggression: f64,
    /// Unweighted non-creature-permanent differential — the `nc_diff` term that
    /// the `card_advantage` weight multiplies in the tactical breakdown. The serve
    /// `card_advantage` feature also folds in `card_advantage::differential`;
    /// see `FeatureRow::extract`.
    pub card_advantage_breakdown: f64,
    /// Fixed-coefficient energy offset (`energy × 0.1`). Added after weighting, so
    /// excluded from `weighted_total`.
    pub energy_offset: f64,
    /// Fixed-coefficient mana-development offset
    /// (`MANA_DEVELOPMENT_COEFF × min(sources, MANA_SOURCE_TARGET)`). Added after
    /// weighting, so excluded from `weighted_total`.
    pub mana_development_offset: f64,
}

impl EvalFeatures {
    /// Weighted sum of every tactical feature **excluding both fixed offsets**
    /// (`energy_offset` and `mana_development_offset`, added after weighting).
    /// Holds `breakdown.total() == features.weighted_total(&w) +
    /// features.energy_offset + features.mana_development_offset` by construction.
    pub fn weighted_total(&self, w: &EvalWeights) -> f64 {
        self.life * w.life
            + self.board_presence * w.board_presence
            + self.board_power * w.board_power
            + self.board_toughness * w.board_toughness
            + self.hand_size * w.hand_size
            + self.aggression * w.aggression
            + self.card_advantage_breakdown * w.card_advantage
    }
}

pub fn strategic_intent(state: &GameState, player: PlayerId) -> StrategicIntent {
    let opponents = players::opponents(state, player);
    if opponents.is_empty() {
        return StrategicIntent::PreserveAdvantage;
    }

    let (_, my_power, _, _) = board_stats(state, player);
    let total_opp_power: i32 = opponents.iter().map(|&opp| board_stats(state, opp).1).sum();
    let min_opp_life = opponents
        .iter()
        .map(|&opp| state.players[opp.0 as usize].life)
        .min()
        .unwrap_or(i32::MAX);
    let my_life = state.players[player.0 as usize].life;
    let avg_opp_life = opponents
        .iter()
        .map(|&opp| state.players[opp.0 as usize].life)
        .sum::<i32>() as f64
        / opponents.len() as f64;

    if min_opp_life > 0 && my_power >= min_opp_life {
        StrategicIntent::PushLethal
    } else if my_life <= total_opp_power.max(1) {
        StrategicIntent::Stabilize
    } else if my_power >= total_opp_power && my_life as f64 >= avg_opp_life {
        StrategicIntent::PreserveAdvantage
    } else {
        StrategicIntent::Develop
    }
}

/// Compute threat level of `target` from `evaluator`'s perspective.
/// Returns 0.0-1.0 where higher means more threatening.
/// Factors: board presence (creature count/total power), life ratio, hand size,
/// commander damage dealt to evaluator.
pub fn threat_level(state: &GameState, evaluator: PlayerId, target: PlayerId) -> f64 {
    threat_level_projected(state, evaluator, target, None)
}

/// Card-equivalent value of a living opponent's battlefield creature, weighted
/// by how threatening that creature's controller is to `evaluator`.
///
/// Keeping the relationship and zone checks here gives removal timing, target
/// selection, and play-order hints one authoritative multiplayer valuation.
pub(crate) fn opponent_battlefield_creature_threat_value(
    state: &GameState,
    evaluator: PlayerId,
    object_id: ObjectId,
) -> Option<f64> {
    let object = state.objects.get(&object_id)?;
    if object.zone != Zone::Battlefield
        || !object.card_types.core_types.contains(&CoreType::Creature)
        || !players::is_alive(state, object.controller)
        || !players::is_opponent(state, evaluator, object.controller)
    {
        return None;
    }

    Some(
        evaluate_creature(state, object_id)
            * (threat_level(state, evaluator, object.controller) + 0.5),
    )
}

/// Projection-aware variant of `threat_level`. When `projection` is provided,
/// the target's board power is read from the projected state — capturing
/// scaling threats like Ouroboroid before they actually swing. The rest of
/// the score (life ratio, hand size, commander damage) uses the current
/// state because those are orthogonal to combat-trigger projection.
pub fn threat_level_projected(
    state: &GameState,
    evaluator: PlayerId,
    target: PlayerId,
    projection: Option<&Projection>,
) -> f64 {
    let target_player = &state.players[target.0 as usize];
    let starting_life = state.format_config.starting_life.max(1) as f64;

    // Board presence: creature count from current state; power from projected
    // state when available (catches growth velocity in the strategic signal).
    let (creatures, base_power, _toughness, _nc) = board_stats(state, target);
    let power = projection
        .map(|p| projected_power(&p.state, target))
        .unwrap_or(base_power);
    let board_score = (creatures as f64 * 0.3 + power as f64 * 0.7).min(10.0) / 10.0;

    // Life ratio: higher life = more threatening
    let life_ratio = (target_player.life as f64 / starting_life).clamp(0.0, 2.0) / 2.0;

    // Hand size: more cards = more options
    let hand_score = (target_player.hand.len() as f64).min(7.0) / 7.0;

    // CR 903.10a: Loss only fires when a SINGLE commander reaches the threshold —
    // accumulated damage across multiple commanders does not. Use the max progress
    // ratio of any one of `target`'s commanders against `evaluator` so the threat
    // signal tracks "closest single commander to the loss condition." Delegates to
    // `commander_lethal_headroom` for the headroom math (single source of truth).
    let cmd_threat = state
        .format_config
        .commander_damage_threshold
        .map_or(0.0, |threshold| {
            let threshold_f = f64::from(threshold);
            state
                .objects
                .values()
                .filter(|o| o.is_commander && o.owner == target)
                .filter_map(|cmd_obj| {
                    let headroom = engine::game::commander::commander_lethal_headroom(
                        state, evaluator, cmd_obj.id,
                    )?;
                    let dealt = f64::from(u32::from(threshold).saturating_sub(headroom));
                    Some((dealt / threshold_f).min(1.0))
                })
                .fold(0.0f64, f64::max)
        });

    // Weighted combination
    board_score * 0.4 + life_ratio * 0.2 + hand_score * 0.15 + cmd_threat * 0.25
}

/// Evaluate the board state from `player`'s perspective.
/// Returns a score where higher is better for `player`.
/// In multiplayer, weights opponent scores by threat level (focus fire on highest threat).
pub fn evaluate_state(state: &GameState, player: PlayerId, weights: &EvalWeights) -> f64 {
    evaluate_state_breakdown(state, player, weights)
        .map(|breakdown| breakdown.total())
        .unwrap_or_else(|terminal| terminal)
}

pub fn evaluate_for_planner(
    state: &GameState,
    player: PlayerId,
    weights: &EvalWeights,
) -> ValueEstimate {
    let value = evaluate_state(state, player, weights);
    ValueEstimate {
        value,
        intent: strategic_intent(state, player),
    }
}

/// Extract the **unweighted** tactical feature vector from `player`'s
/// perspective. Single authority for the feature math shared by the serve-time
/// weighting ([`evaluate_state_breakdown`]) and offline Texel harvesting
/// (`crate::duel_suite::harvest::FeatureRow::extract`).
///
/// Terminal short-circuits are identical to [`evaluate_state_breakdown`]: a
/// game-over / lethal / all-opponents-dead position returns `Err(terminal_score)`
/// rather than a feature vector, so harvesting skips label-leaking terminal
/// positions by construction. Both the 2-player and multiplayer (threat-weighted)
/// aggregations are covered — the harvested value is whatever multiplies the
/// weight, so one extractor is path-agnostic.
pub fn evaluate_features(state: &GameState, player: PlayerId) -> Result<EvalFeatures, f64> {
    // Check for game over
    if let WaitingFor::GameOver { winner } = &state.waiting_for {
        return Err(match winner {
            Some(w) if *w == player => WIN_SCORE,
            Some(_) => LOSS_SCORE,
            None => 0.0, // draw
        });
    }

    let opponents = players::opponents(state, player);
    let p = &state.players[player.0 as usize];

    // Check for lethal life totals
    if p.life <= 0 {
        return Err(LOSS_SCORE);
    }
    // If any opponent is dead, that's good (but not an outright win unless all are)
    let all_opponents_dead = !opponents.is_empty()
        && opponents
            .iter()
            .all(|&opp| state.players[opp.0 as usize].life <= 0);
    if all_opponents_dead {
        return Err(WIN_SCORE);
    }

    let mut features = EvalFeatures::default();
    let opp_count = opponents.len().max(1) as f64;

    // For multiplayer (3+), use threat-weighted opponent scoring
    if opponents.len() >= 2 {
        // Compute threat levels and use them as weights
        let threats: Vec<(PlayerId, f64)> = opponents
            .iter()
            .map(|&opp| (opp, threat_level(state, player, opp)))
            .collect();
        let total_threat: f64 = threats.iter().map(|(_, t)| t).sum::<f64>().max(0.01);

        let mut weighted_opp_life = 0.0;
        let mut weighted_opp_creatures = 0.0;
        let mut weighted_opp_power = 0.0;
        let mut weighted_opp_toughness = 0.0;
        let mut weighted_opp_hand = 0.0;
        let mut weighted_opp_nc = 0.0;

        for &(opp, threat) in &threats {
            let w = threat / total_threat;
            let o = &state.players[opp.0 as usize];
            let (opp_creatures, opp_power, opp_toughness, opp_nc) = board_stats(state, opp);
            weighted_opp_life += o.life as f64 * w;
            weighted_opp_creatures += opp_creatures as f64 * w;
            weighted_opp_power += opp_power as f64 * w;
            weighted_opp_toughness += opp_toughness as f64 * w;
            weighted_opp_hand += o.hand.len() as f64 * w;
            weighted_opp_nc += opp_nc as f64 * w;
        }

        // Life differential (against threat-weighted opponent)
        features.life = p.life as f64 - weighted_opp_life;

        let (my_creatures, my_power, my_toughness, my_nc) = board_stats(state, player);
        features.board_presence = my_creatures as f64 - weighted_opp_creatures;
        features.board_power = my_power as f64 - weighted_opp_power;
        features.board_toughness = my_toughness as f64 - weighted_opp_toughness;
        features.hand_size = p.hand.len() as f64 - weighted_opp_hand;
        features.card_advantage_breakdown = my_nc as f64 - weighted_opp_nc;

        if p.life as f64 > weighted_opp_life && my_power > 0 {
            features.aggression = my_power as f64;
        }
    } else {
        // 2-player path: original logic (no threat weighting overhead)
        let mut total_opp_life = 0;
        let mut total_opp_creatures = 0;
        let mut total_opp_power = 0;
        let mut total_opp_toughness = 0;
        let mut total_opp_hand_size = 0;
        let mut total_opp_nc = 0;
        for &opp in &opponents {
            let o = &state.players[opp.0 as usize];
            total_opp_life += o.life;
            let (opp_creatures, opp_power, opp_toughness, opp_nc) = board_stats(state, opp);
            total_opp_creatures += opp_creatures;
            total_opp_power += opp_power;
            total_opp_toughness += opp_toughness;
            total_opp_hand_size += o.hand.len();
            total_opp_nc += opp_nc;
        }

        let avg_opp_life = total_opp_life as f64 / opp_count;
        features.life = p.life as f64 - avg_opp_life;

        let (my_creatures, my_power, my_toughness, my_nc) = board_stats(state, player);
        features.board_presence = (my_creatures - total_opp_creatures) as f64;
        features.board_power = (my_power - total_opp_power) as f64;
        features.board_toughness = (my_toughness - total_opp_toughness) as f64;

        let avg_opp_hand = total_opp_hand_size as f64 / opp_count;
        features.hand_size = p.hand.len() as f64 - avg_opp_hand;

        let avg_opp_nc = total_opp_nc as f64 / opp_count;
        features.card_advantage_breakdown = my_nc as f64 - avg_opp_nc;

        if p.life as f64 > avg_opp_life && my_power > 0 {
            features.aggression = my_power as f64;
        }
    }

    // CR 107.14: Energy counters are a minor resource — value each energy point
    // as a small fraction of a card (comparable to scry). Fixed-coefficient
    // offset applied AFTER weighting (see `evaluate_state_breakdown`), so it lives
    // on `EvalFeatures` separately rather than folded into a weighted feature.
    features.energy_offset = p.energy as f64 * 0.1;

    // CR 106.1: mana is the primary resource. `board_stats` credits a land in
    // neither bucket while the same card in hand is worth `w.hand_size`, so
    // without this term the weighted delta of the controller's own land drop is
    // strictly negative. Fixed-coefficient offset applied AFTER weighting, exactly
    // like the energy offset above.
    features.mana_development_offset = mana_development_offset(mana_source_count(state, player));

    Ok(features)
}

pub fn evaluate_state_breakdown(
    state: &GameState,
    player: PlayerId,
    weights: &EvalWeights,
) -> Result<EvaluationBreakdown, f64> {
    let features = evaluate_features(state, player)?;
    let mut breakdown = EvaluationBreakdown {
        life: features.life * weights.life,
        board_presence: features.board_presence * weights.board_presence,
        board_power: features.board_power * weights.board_power,
        board_toughness: features.board_toughness * weights.board_toughness,
        hand_size: features.hand_size * weights.hand_size,
        aggression: features.aggression * weights.aggression,
        card_advantage: features.card_advantage_breakdown * weights.card_advantage,
        mana_development: 0.0,
    };

    // CR 107.14: energy is a fixed-coefficient offset added AFTER weighting, so
    // `EvalFeatures::weighted_total` excludes it and it lands on `hand_size` here
    // exactly as the historical `breakdown.hand_size += p.energy * 0.1` did.
    breakdown.hand_size += features.energy_offset;

    // CR 106.1: mana development is the second fixed-coefficient offset, also
    // excluded from `weighted_total`. It gets its own breakdown field rather than
    // riding on `hand_size` (as energy does for historical reasons) because it
    // reaches 90.0 and would make `hand_size` unreadable.
    breakdown.mana_development = features.mana_development_offset;

    Ok(breakdown)
}

/// Board statistics: (creature_count, total_power, total_toughness, non_creature_permanents).
/// Total creature power controlled by `player` in `state`. Unlike
/// `board_stats`, this only computes the power dimension — used by
/// `threat_level_projected` to read power from a projected state without
/// recomputing creature counts that are frame-invariant.
fn projected_power(state: &GameState, player: PlayerId) -> i32 {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            obj.controller == player && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|obj| obj.power.unwrap_or(0))
        .sum()
}

pub fn board_stats(state: &GameState, player: PlayerId) -> (i32, i32, i32, i32) {
    let mut creatures = 0;
    let mut total_power = 0;
    let mut total_toughness = 0;
    let mut non_creatures = 0;

    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player {
                if obj.card_types.core_types.contains(&CoreType::Creature) {
                    creatures += 1;
                    total_power += obj.power.unwrap_or(0);
                    total_toughness += obj.toughness.unwrap_or(0);
                } else if !obj.card_types.core_types.contains(&CoreType::Land) {
                    // Non-creature, non-land permanents (enchantments, artifacts, planeswalkers)
                    non_creatures += 1;
                }
            }
        }
    }

    (creatures, total_power, total_toughness, non_creatures)
}

/// Marginal value of one controlled mana source, in the same units as the
/// weighted feature sum.
///
/// Sized to strictly exceed the worst-case weighted cost of a land drop across
/// every shipped archetype and turn phase. That cost is dominated by
/// `hand_size`, whose *serve-time* value is the learned weight scaled by the
/// archetype multiplier (`DeckProfile::adjust_weights_with`), peaking at
/// `2.5 × 2.5 = 6.25` for Combo in the late phase; the `zone_quality`
/// hand-castability term and the `synergy` dilution term add at most 0.31 more.
/// Binding cost 6.5545, so 7.5 carries 14.42% headroom **against the maximum**.
///
/// Because this is one archetype-invariant constant sized against the *largest*
/// land-drop cost, headroom is NOT uniform: it ranges from 14.4% (Combo) to
/// ~771% (Aggro), because serve-time `hand_size` spans 8.3x across archetypes
/// (0.75 Aggro .. 6.25 Combo). In card-equivalent terms (`policies::registry`:
/// `delta = 1.0` is one card) one mana source is worth ~1.2 cards to Combo and
/// ~10 cards to Aggro. This is an accepted consequence of the fixed-coefficient
/// design; the archetype-relative alternative was considered and rejected.
///
/// `mana_development_floor_holds_for_every_archetype_and_phase` recomputes this
/// from the real weights and fails loudly if a retrain or a multiplier change
/// invalidates it. The floor is proven for the SHIPPED DEFAULTS ONLY —
/// `bin/ai_tune.rs` can write arbitrary weights and is outside the guarantee.
///
/// # The disclosed inversion is ARCHETYPE-INVARIANT, not a Control phenomenon
///
/// This constant carries no archetype term and is applied *after* weighting
/// (`evaluate_state_breakdown`), so no `ArchetypeMultipliers` entry can scale it:
/// **every archetype receives exactly +7.5 per source.** Only the counterweight
/// differs, and it differs enormously, so read Control and Aggro as the measured
/// *bounds* of one uniform effect rather than as "the archetypes that invert".
/// Margins for a 2-mana renewable rock versus a comparable 2-mana 3/3 body, late
/// phase, at the eval layer (`rock − body`; positive = the rock is preferred).
/// The "with it" column is a real measurement from `tests/ai_quality.rs`, not a
/// prediction; the "without" column is that value less the coefficient.
///
/// | Archetype | Without the offset | With it | Standing coverage |
/// |---|---|---|---|
/// | Control | −3.23 | **+4.27** (INVERTS) | `control_prefers_mana_rock_over_comparable_creature_as_disclosed` (banded 2.0..7.0) |
/// | Midrange (`#[default]`) | −7.82651 | **−0.32651** (does NOT invert — but is 0.33 from it) | `midrange_still_ranks_creature_above_mana_rock_but_barely` |
/// | Aggro | −7.91465 | **−0.41465** (does NOT invert) | `aggro_still_ranks_creature_above_mana_rock` |
/// | Combo, Ramp | — | **UNMEASURED** | none |
///
/// Note the ordering that the "Control inverts" framing hides: **Midrange is the
/// archetype closest to inverting after Control**, at 0.33, and it is the
/// `#[default]` — the classification every unplaceable deck falls back to.
///
/// Combo and Ramp have no rock-vs-body row. Combo is the archetype with the
/// largest `hand_size` counterweight (2.5×) and is therefore the *least* likely
/// to invert; Ramp sits near Midrange. Neither is asserted — treat them as
/// unknown, not as safe.
///
/// # The premium applies symmetrically to LOSING a source
///
/// `mana_source_count` is recomputed live from the battlefield, so a source that
/// leaves takes its +7.5 with it. Where that can actually change a decision is
/// narrower than "any board loss", so name the mechanism rather than the
/// situation. The offset has exactly two consumers: `evaluate_state_breakdown`
/// (`:553`, whence `EvaluationBreakdown::total` feeds
/// `PlannerServices::evaluate_with_strategy`) and `FeatureRow::extract` (a
/// harvested control column the trainer discards). It can therefore move a
/// choice ONLY through the search leaf eval, i.e. only when
/// `config.search.enabled`; the heuristic-only branch of `score_candidates_core`
/// ranks with `PlannerServices::tactical_score` (`should_play_now_with_facts`
/// plus the policy registry), and no file under `policies/` references
/// `evaluate_state`.
///
/// Resolve that bound rather than leaving it as a condition, because "only when
/// search is enabled" reads as a narrow restriction and is not one. In
/// `config::create_config`, `search.enabled` is `false` for **`VeryEasy` and
/// `Easy`** and `true` for **`Medium`, `Hard`, `VeryHard`, and `CEDH`** — so the
/// *preset* bound means *Medium and above*, and since `AiConfig::default` is
/// `create_config(AiDifficulty::Medium, Platform::Native)`, the **default
/// difficulty does see this offset**.
///
/// The presets are not the whole set, and the remainder inverts that answer for a
/// 5- or 6-player pod at `<= Medium`. Read the regime the way `policies::loop_shortcut`
/// already enumerates it — search is off for VeryEasy / Easy, **for large pods at
/// `<= Medium`**, for `SearchConfig::default()`, and for a pre-expired deadline:
///
/// - `config::create_config_for_players` forces `search.enabled = false` when
///   `player_count` is **5 or more** and `config.difficulty <= AiDifficulty::Medium`
///   — `AiDifficulty` derives `Ord` in declaration order, so that set is exactly
///   `VeryEasy`, `Easy`, `Medium`. This is the constructor the production entry
///   points use on **every platform, the browser included**: `engine-wasm`'s AI
///   entry points call `create_config_for_players(ai_difficulty, Platform::Wasm,
///   state.players.len() as u8)`, and `phase_server` / `server_core::session` pass the
///   live lobby count. So on a **5- or 6-player pod at `<= Medium`, the default
///   difficulty included, this offset is inert.** The boundary falls *above* the
///   ordinary 4-player Commander table: the `3..=4` arm rescales
///   `max_depth`, `max_nodes`, `max_branching`, and `rollout_depth` only and never
///   writes `enabled`, and `create_config`'s `Platform::Wasm` block clamps the same
///   budget knobs and no more — so **4 players at Medium on any platform keeps
///   `enabled: true` and does see the offset.** The match arm is unbounded above
///   (`_`), but `FormatConfig::commander` is `min_players: 2, max_players: 6`, so
///   for Commander the inert band is exactly `{5, 6}` of `{2..=6}` — the two
///   largest pods the format admits.
/// - `SearchConfig::default()` is `enabled: false`, but no shipped difficulty
///   preset uses it.
/// - A **pre-expired deadline** breaks `score_candidates_core`'s iterative
///   deepening at the rung-0 entry guard and returns the tactical-only floor, so
///   no leaf eval runs and the offset is absent there too — at *any* difficulty.
///
/// The practical consequence for triage: the first question on a report of this
/// behaviour is **which difficulty AND how many players**. Difficulty alone
/// under-determines it, and so does a Commander label. A **4-player report does
/// not rule this seam out** — `client/src/services/presets.ts` ships
/// `default-commander-ffa` ("Quick Commander (4-player FFA)": Commander, Medium,
/// `playerCount: 4`), which sits on the live side of the boundary and sees the
/// full offset.
///
/// That boundary is a routing fact, and the search-off regime keeps its own
/// coverage — name what supplies it. `search::prefer_land_drop` still runs there:
/// `deterministic_choice` calls it *ahead* of the `config.search.enabled` branch,
/// and `prefer_land_drop` accepts no `AiConfig` at all (`state`, `ai_player`,
/// `actions`), so no difficulty or player count can gate it. But it deliberately
/// fires only when **exactly one** `PlayLand` action exists, so that
/// `LandSequencingPolicy` can compare ambiguous land choices through scoring.
/// One land in hand is therefore
/// still played; **two or more** falls through to `PlannerServices::tactical_score`
/// (`should_play_now_with_facts` plus the policy registry), which is where
/// `policies::land_sequencing`, `policies::board_development`, and
/// `policies::landfall_timing` live. Those policies hold that regime; this offset
/// does not reach it.
///
/// - **REACHED — ward-cost sacrifice, and only that.**
///   `WaitingFor::WardSacrificeChoice` (CR 702.21a, "choose a permanent to
///   sacrifice as ward cost payment") emits one candidate per permanent in its
///   single-permanent form (`engine::ai_support::candidates`).
///   `deterministic_choice` has no arm for the variant, so those candidates
///   reach scoring, and each gives up exactly one permanent — so a pair of them
///   differs by one `mana_source_count` whenever exactly one of the two is a
///   source, which is where this offset discriminates.
///   `WardSacrificeChoice`'s other two branches are excluded, and the exclusion
///   is structural rather than a matter of degree: the aggregate-power
///   (`min_total_power`) form maps a single deterministic
///   `power_threshold_witness` to exactly one selection — or to none when no
///   witness exists — and the empty-`permanents` form emits only a decline.
///   Neither expresses a choice, so no coefficient can move them.
/// - **NOT REACHED — the ordinary "sacrifice a permanent" choice**, which is the
///   one a reader of this section will picture. It arrives as
///   `WaitingFor::EffectZoneChoice` (`effects::sacrifice`), and
///   `deterministic_choice` **does** have an arm for it: when `effect_kind` is
///   `EffectKind::Sacrifice` with a non-empty `cards`, `!up_to`, and `count > 0`
///   it returns `pick_lowest_value_sacrifices` immediately, before any scoring.
///   That helper orders by `policies::strategy_helpers::sacrifice_key` —
///   documented there as "**the single battlefield give-up authority**", with
///   its own land axis and tiering. So the mandatory give-up decision is priced
///   by `sacrifice_key`, not by this offset, and changing this coefficient
///   cannot move it. Only the residue that the arm's guard excludes —
///   `up_to` (optional) sacrifices and `count == 0` — falls through to scoring.
/// - **NOT REACHED — `WaitingFor::ChooseObjectsSelection`.** Recorded explicitly
///   because an earlier revision of this very block cited it as the primary
///   reached class, and it is not one. Per its own variant doc in
///   `types::game_state`, it is the `ChooseObjectsIntoTrackedSet` selection: it
///   *references* battlefield permanents into a tracked set for a downstream
///   `PayCost { ScaledMana }` / `IfYouDo` / `Untap`.
///   **The permanents are not given up.** Its candidates do reach
///   scoring, but every one of them leaves the board population identical, so
///   `mana_source_count` is equal across them and this offset cannot
///   discriminate. (`Untap` downstream does not change that: the count is a
///   development measure that ignores tapped state — see
///   `zone_eval::is_intrinsic_mana_source`.)
/// - **NOT REACHED — block assignment.** `score_candidates_core` intercepts
///   `WaitingFor::DeclareAttackers | DeclareBlockers` *after* `build_decision_context`
///   has already generated candidates, but *before* validation, gating, and
///   scoring, and routes to `deterministic_combat_choice` →
///   `combat_ai::choose_blockers_with_profile`, which ranks with
///   `evaluate_creature` / `threat_level` — the only two `eval` items
///   `combat_ai` imports. The block path cannot see this offset, so its
///   chump-block behaviour is **UNMEASURED** in exactly the sense Combo and Ramp
///   are above; do not read one into this disclosure.
///
/// Every reachability verdict above is established by code read, not by an
/// executed decision: no test drives a sacrifice or block prompt end to end.
///
/// Midrange, late, with the shipped tables, the eval-layer cost of losing one
/// permanent is:
///
/// - a **1/1 mana dork**: `2.598 (presence) + 0.802 (power) + 1.200 (toughness)
///   + 0.778 (card_advantage) + 7.500 (this offset)` = **12.878**
/// - a **vanilla 4/4**: `2.598 + 3.209 + 4.800 + 0.778` = **11.385**
///
/// Break-even is a vanilla **4.7/4.7** body (a 4.0/4.0 body when the
/// `aggression` term is live, i.e. when the AI is ahead on life). Pinned by
/// `mana_dork_outvalues_a_bigger_body_when_trading`, which compares the two
/// post-loss states at the eval layer — it measures this arithmetic, not the
/// routing that delivers a state to it.
///
/// Read that arithmetic for what the routing above supports and no further. It
/// is a property of the **evaluation function**: of two states differing by
/// which permanent left, the one that kept the dork scores higher. It becomes an
/// observable *decision* only where a scored candidate pair actually differs by
/// one source — per the routing, the ward-cost sacrifice and the optional
/// (`up_to`) residue. It is **not** a claim about blocks, and **not** a claim
/// about the ordinary sacrifice prompt; both are decided elsewhere, by
/// `combat_ai::choose_blockers_with_profile` and by `sacrifice_key` respectively.
/// An earlier revision of this block asserted the block case outright ("the AI
/// will chump-block a 4/4 to save a Llanowar Elves") — that was false, and the
/// consumer list at the top of this section was sufficient to refute it at the
/// time it was written. This is the same accepted consequence as
/// the acquisition inversion, disclosed separately because it is a different
/// decision class with a different counterweight.
const MANA_DEVELOPMENT_COEFF: f64 = 7.5;

/// Source count past which an additional mana source is worth nothing. Below it
/// the marginal is flat; at and above it the offset is constant, so the AI
/// cannot farm value by flooding.
///
/// 12 covers modern Tron (7 lands) and reaches the **top** of a typical Commander
/// board of 8–12 sources including rocks — it is the upper end of that band, not
/// its middle. Ceiling is `MANA_DEVELOPMENT_COEFF * 12 = 90.0` against
/// `WIN_SCORE = 10000.0`, so terminal scores are never approached.
///
/// # The reported bug RETURNS IN FULL at and above this cap
///
/// State plainly what the cap does, because "flooding earns nothing" reads as
/// neutrality and it is not neutral. Past the target the marginal is exactly
/// `0.0`, but **the land-drop cost is unchanged** — the card still leaves hand
/// (`w.hand_size`, up to 6.25 for Combo/late), still loses its `zone_quality`
/// hand-castability contribution, and still dilutes `synergy`, for a binding total
/// of **6.5545**. So at `sources >= 12` the weighted delta of the controller's own
/// land drop is strictly negative again and `PlayLand` once more scores below
/// `PassPriority`: the exact symptom this unit exists to fix, restored.
///
/// This is reachable, not theoretical: Commander from roughly turn 10, with 12
/// lands/rocks on the battlefield and **two or more** lands in hand (two, so
/// `search::prefer_land_drop`'s single-land shortcut declines and the decision
/// actually reaches scoring). Closing it is the root fix — this coefficient
/// deliberately does not attempt it, because raising or removing the cap trades
/// the flooding defect back in.
///
/// `mana_development_floor_holds_for_every_archetype_and_phase` pins **both**
/// poles: `marginal > cost` below the cap, and `marginal < cost` at and above it.
/// The above-cap assertion is deliberately an assertion rather than prose so the
/// successor unit inherits a failing record when it fixes this, instead of having
/// to rediscover it.
const MANA_SOURCE_TARGET: i32 = 12;

/// Count of *renewable* mana sources `player` controls on the battlefield.
///
/// CR 106.1: mana is the primary resource. Counts permanents, not pips — a
/// two-mana rock counts once. Never called for an opponent **on any production
/// path** (every non-test caller passes the evaluating player), so this adds
/// exactly one battlefield pass per evaluation. Note this is a property of the
/// call sites, not an invariant the compiler enforces: existing unit tests do
/// evaluate for `PlayerId(1)`, and that is fine — it simply costs a second pass.
///
/// CR 701.21: one-shot self-sacrificing sources (Treasure, Gold, Lotus Petal)
/// do NOT count — see `zone_eval::is_intrinsic_mana_source`. Cracking one must
/// not read as losing a mana source.
fn mana_source_count(state: &GameState, player: PlayerId) -> i32 {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player && zone_eval::is_intrinsic_mana_source(obj))
        .count() as i32
}

/// Fixed-coefficient value of controlling `sources` mana sources.
///
/// CR 305.1 + CR 305.2: playing a land is a once-per-turn special action, so a
/// missed drop is a permanently lost increment — every source up to the target
/// must be worth strictly more than the card it came from. Beyond the target the
/// marginal is exactly zero, so flooding earns nothing — and, stated without
/// euphemism, the land drop therefore scores strictly *negative* again above the
/// cap and the reported bug returns. See `MANA_SOURCE_TARGET` for the full
/// disclosure and the assertion that pins it.
///
/// `f(n) = C · min(n, S)`. Marginal of the k-th source is `C` for `k <= S` and
/// exactly `0.0` for `k > S`. With `C = 7.5`, `S = 12`: `f(0) = 0`,
/// `f(12) = f(20) = 90.0`.
///
/// A strictly-diminishing curve was considered and rejected: to keep the
/// marginal above the 6.55 floor at `k = S` it would have to start at roughly
/// twice the floor and accumulate roughly twice the ceiling, for a fidelity gain
/// no requirement asks for. If the AI is later measured over-valuing sources
/// 8–12, lower `MANA_SOURCE_TARGET` — do not reintroduce curvature.
///
/// Known mild double-credit on a land drop: raising available mana can flip hand
/// cards castable in `zone_bonus`, worth at most 0.045 per flipped card (Combo,
/// the per-archetype maximum of `castable_bonus × 0.3 × m[6]`). Directionally
/// correct and accepted uncompensated; compensating would require this term to
/// read tapped state, which is the defect it exists to avoid.
fn mana_development_offset(sources: i32) -> f64 {
    MANA_DEVELOPMENT_COEFF * f64::from(sources.clamp(0, MANA_SOURCE_TARGET))
}

/// Configurable keyword bonuses for creature evaluation.
/// Multiplicative bonuses scale with power; flat bonuses are constant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordBonuses {
    pub flying_mult: f64,
    pub trample_mult: f64,
    pub deathtouch_flat: f64,
    pub lifelink_mult: f64,
    pub hexproof_flat: f64,
    pub indestructible_flat: f64,
    pub first_strike_mult: f64,
    pub vigilance_flat: f64,
    pub menace_mult: f64,
    pub tapped_penalty: f64,
}

impl Default for KeywordBonuses {
    fn default() -> Self {
        Self {
            flying_mult: 1.0,
            trample_mult: 0.5,
            deathtouch_flat: 3.0,
            lifelink_mult: 0.5,
            hexproof_flat: 2.0,
            indestructible_flat: 4.0,
            first_strike_mult: 0.8,
            vigilance_flat: 1.0,
            menace_mult: 0.5,
            tapped_penalty: 1.5,
        }
    }
}

/// Evaluate a single creature's combat value.
/// Higher scores indicate more valuable creatures.
pub fn evaluate_creature(state: &GameState, obj_id: ObjectId) -> f64 {
    evaluate_creature_with_bonuses(state, obj_id, &KeywordBonuses::default())
}

/// Evaluate a creature using configurable keyword bonuses.
pub fn evaluate_creature_with_bonuses(
    state: &GameState,
    obj_id: ObjectId,
    bonuses: &KeywordBonuses,
) -> f64 {
    let obj = match state.objects.get(&obj_id) {
        Some(o) => o,
        None => return 0.0,
    };

    let mut value = creature_combat_value(
        obj.power.unwrap_or(0),
        obj.toughness.unwrap_or(0),
        |kw| obj.has_keyword(kw),
        bonuses,
    );

    // Tapped creatures are less valuable (board state, not an intrinsic trait).
    if obj.tapped {
        value -= bonuses.tapped_penalty;
    }

    value
}

/// Combat value of a creature from its raw stats and keyword set, independent of
/// board state. Power is weighted 1.5× toughness; keyword bonuses come from
/// `bonuses`. Shared by board evaluation ([`evaluate_creature_with_bonuses`]) and
/// draft-pick evaluation ([`crate::draft_eval`]). Does *not* apply the tapped
/// penalty — that is a board-state concern handled by the caller.
pub fn creature_combat_value(
    power: i32,
    toughness: i32,
    has_keyword: impl Fn(&Keyword) -> bool,
    bonuses: &KeywordBonuses,
) -> f64 {
    let power = power as f64;
    let toughness = toughness as f64;

    // Base value: power matters more for combat
    let mut value = power * 1.5 + toughness;

    // Keyword bonuses
    if has_keyword(&Keyword::Flying) {
        value += power * bonuses.flying_mult;
    }
    if has_keyword(&Keyword::Trample) {
        value += power * bonuses.trample_mult;
    }
    if has_keyword(&Keyword::Deathtouch) {
        value += bonuses.deathtouch_flat;
    }
    if has_keyword(&Keyword::Lifelink) {
        value += power * bonuses.lifelink_mult;
    }
    if has_keyword(&Keyword::Hexproof) {
        value += bonuses.hexproof_flat;
    }
    if has_keyword(&Keyword::Indestructible) {
        value += bonuses.indestructible_flat;
    }
    if has_keyword(&Keyword::FirstStrike) || has_keyword(&Keyword::DoubleStrike) {
        value += power * bonuses.first_strike_mult;
    }
    if has_keyword(&Keyword::Vigilance) {
        value += bonuses.vigilance_flat;
    }
    if has_keyword(&Keyword::Menace) {
        value += power * bonuses.menace_mult;
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        GameState::new_two_player(42)
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        power: i32,
        toughness: i32,
        keywords: Vec<Keyword>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.keywords = keywords;
        id
    }

    /// A bare land on the battlefield — the `CoreType::Land` short-circuit path.
    fn add_land(state: &mut GameState, owner: PlayerId, tapped: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.tapped = tapped;
        id
    }

    /// `{T}: Add {C}` on a nonland, noncreature permanent — Commander's-Sphere
    /// shaped. Reaches the ability-inspection arm, not the `Land` short-circuit.
    fn renewable_mana_ability() -> engine::types::ability::AbilityDefinition {
        let mut ability = engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Activated,
            engine::types::ability::Effect::Mana {
                produced: engine::types::ability::ManaProduction::Colorless {
                    count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(engine::types::ability::AbilityCost::Tap);
        ability
    }

    /// `{T}, Sacrifice this: Add {C}{C}` — the one-shot shape (Treasure-like).
    fn self_sac_mana_ability() -> engine::types::ability::AbilityDefinition {
        let mut ability = renewable_mana_ability();
        ability.cost = Some(engine::types::ability::AbilityCost::Composite {
            costs: vec![
                engine::types::ability::AbilityCost::Tap,
                engine::types::ability::AbilityCost::Sacrifice(
                    engine::types::ability::SacrificeCost::count(
                        engine::types::ability::TargetFilter::SelfRef,
                        1,
                    ),
                ),
            ],
        });
        ability
    }

    fn add_artifact_with(
        state: &mut GameState,
        owner: PlayerId,
        abilities: Vec<engine::types::ability::AbilityDefinition>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let list = std::sync::Arc::make_mut(&mut obj.abilities);
        list.extend(abilities);
        id
    }

    /// Row 2: the curve is monotone, flat-marginal up to the target, then exactly
    /// zero. The flat-tail equality is the discriminator an unclamped linear
    /// implementation fails while still passing monotonicity.
    #[test]
    fn mana_development_offset_is_flat_then_saturates() {
        assert_eq!(mana_development_offset(0), 0.0);
        // Negative pole: `clamp` floors at 0, so a nonsensical count is inert.
        assert_eq!(mana_development_offset(-1), 0.0);

        for k in 1..=MANA_SOURCE_TARGET {
            let marginal = mana_development_offset(k) - mana_development_offset(k - 1);
            assert!(
                (marginal - MANA_DEVELOPMENT_COEFF).abs() < 1e-9,
                "marginal of source {k} must be exactly the coefficient, got {marginal}"
            );
        }
        for k in (MANA_SOURCE_TARGET + 1)..=(MANA_SOURCE_TARGET + 8) {
            let marginal = mana_development_offset(k) - mana_development_offset(k - 1);
            assert_eq!(marginal, 0.0, "marginal past the target must be exactly 0");
        }

        // Monotone non-decreasing across the whole range.
        for k in 0..=(MANA_SOURCE_TARGET + 8) {
            assert!(mana_development_offset(k + 1) >= mana_development_offset(k));
        }

        // Flat-tail discriminator: an unclamped `COEFF * n` passes every
        // monotonicity assertion above and fails exactly here.
        assert_eq!(
            mana_development_offset(MANA_SOURCE_TARGET),
            mana_development_offset(MANA_SOURCE_TARGET + 8)
        );
    }

    /// Row 3: the floor holds at every source count, for every archetype, in every
    /// phase — computed from the LIVE weight table, never from literals, so a
    /// retrain or a multiplier edit that invalidates `MANA_DEVELOPMENT_COEFF`
    /// fails here loudly instead of silently reverting the land-drop bug.
    ///
    /// This is the coverage `ai-gate` structurally cannot provide: `MatchupSpec`
    /// has exactly two seats, so the suite never reaches the Commander regime.
    /// The floor is a pure arithmetic property of the curve versus the weight
    /// table and is therefore pod-size independent (see
    /// `threat_level_ignores_evaluator_own_board`, which keeps that true).
    #[test]
    fn mana_development_floor_holds_for_every_archetype_and_phase() {
        use crate::deck_profile::{ArchetypeMultipliers, DeckArchetype, DeckProfile};
        use crate::zone_eval::ZoneWeights;
        use strum::IntoEnumIterator;

        // Test-local. Max per-card synergy score is 0.3 (tribal) + 0.5 (sacrifice,
        // = 0.25 twice for a card that is both a sac outlet and a token producer)
        // + 0.6 (graveyard, = 0.3 twice for a card that both fills and recurs)
        // + 0.4 (spellcast) = 1.8, times the worst dilution factor
        // (1 - sqrt(1/2)) = 0.2929 at N = 1. The bound is monotone decreasing in
        // N, so this bounds every N.
        //
        // CAVEAT — this is the ONE term in `cost` that is a hand-derived literal
        // rather than a live read. `w.hand_size`, `w.zone_quality` and `w.synergy`
        // all come from `EvalWeightSet::learned()`, so a retrain moves them here
        // automatically; the 1.8 does NOT, because `synergy`'s four axis bonuses
        // are unnamed literals inside `synergy::detect_tribal` / `detect_sacrifice`
        // / `detect_graveyard` / `detect_spellcast`. If any of those rises, the
        // true land-drop cost rises while this bound does not and the floor test
        // keeps passing while the floor is actually violated. Re-derive the 1.8
        // from those four functions whenever synergy scoring changes. Impact is
        // small today (the term is <= 0.26 against a >= 0.95 margin), which is why
        // this is a documented caveat rather than a blocker.
        const SYNERGY_DILUTION_BOUND: f64 = 1.8 * 0.2929;

        let learned = EvalWeightSet::learned();
        let mut worst_above_cap_deficit = f64::NEG_INFINITY;
        for (phase_name, base) in [
            ("early", &learned.early),
            ("mid", &learned.mid),
            ("late", &learned.late),
        ] {
            // Exhaustiveness comes from `EnumIter`, not from a hand-written array:
            // a sixth `DeckArchetype` is iterated here automatically and cannot
            // silently escape the floor guarantee.
            for archetype in DeckArchetype::iter() {
                let profile = DeckProfile {
                    archetype,
                    ..Default::default()
                };
                let w = profile.adjust_weights_with(&ArchetypeMultipliers::default(), base);
                let zw = ZoneWeights::for_archetype(archetype);
                let cost = w.hand_size
                    + (zw.hand_card_base + zw.castable_bonus) * w.zone_quality
                    + SYNERGY_DILUTION_BOUND * w.synergy;
                for k in 1..=MANA_SOURCE_TARGET {
                    let marginal = mana_development_offset(k) - mana_development_offset(k - 1);
                    assert!(
                        marginal > cost,
                        "{phase_name}/{archetype:?} k={k}: marginal {marginal} <= land-drop cost {cost}"
                    );
                }
                // Upper pole: the guarantee deliberately STOPS at the target, and
                // what lies past it is pinned rather than merely implied.
                let past = mana_development_offset(MANA_SOURCE_TARGET + 1)
                    - mana_development_offset(MANA_SOURCE_TARGET);
                assert_eq!(past, 0.0);
                // ABOVE THE CAP THE GUARANTEE INVERTS. The marginal is 0 while the
                // land-drop cost is untouched, so `PlayLand` scores strictly below
                // `PassPriority` again — the reported bug, returning in full at
                // >= 12 sources (see `MANA_SOURCE_TARGET`). Asserted so the
                // successor unit that fixes it inherits a failing record here
                // instead of rediscovering the limitation.
                assert!(
                    past < cost,
                    "{phase_name}/{archetype:?}: above the cap the marginal {past} \
                     must be BELOW the land-drop cost {cost} — if this ever fails, \
                     the above-cap regression has been closed and both this \
                     assertion and `MANA_SOURCE_TARGET`'s disclosure must be updated"
                );
                worst_above_cap_deficit = worst_above_cap_deficit.max(cost - past);
            }
        }

        // The above-cap regression, pinned as a NUMBER rather than as prose.
        //
        // The `past < cost` assertion inside the loop states the direction, but it
        // is close to tautological: `past` is 0 by the clamp and `cost` is a sum of
        // positive weights, so nothing realistic falsifies it. This does the work.
        // `worst_above_cap_deficit` is how much score the controller LOSES per land
        // drop once at or above `MANA_SOURCE_TARGET`, at the binding
        // archetype/phase — the exact quantity `MANA_SOURCE_TARGET`'s disclosure
        // quotes as 6.5545.
        //
        // It is falsifiable and load-bearing in both directions:
        // - a retrain or multiplier edit that moves `hand_size` / `zone_quality` /
        //   `synergy` moves this number, and the disclosure that quotes it goes
        //   stale silently — this catches that;
        // - a successor unit that closes the above-cap gap drives it to <= 0, which
        //   is the failing record this exists to hand them.
        assert!(
            (6.55..6.56).contains(&worst_above_cap_deficit),
            "above the cap, every land drop costs the controller \
             {worst_above_cap_deficit} with NOTHING credited back — the reported \
             bug in full. `MANA_SOURCE_TARGET`'s disclosure quotes 6.5545; if this \
             has moved, the weights changed and that disclosure is now stale. If it \
             has gone <= 0 the regression is FIXED: delete this assertion and the \
             disclosure together."
        );
    }

    /// Row 11: the offset binds to `controller`, not `owner`, and is recomputed
    /// live rather than latched. Reassigning control of a land must move the whole
    /// offset across — an implementation reading `owner` fails both assertions.
    #[test]
    fn mana_development_follows_controller_not_owner() {
        let mut state = make_state();
        let land_id = add_land(&mut state, PlayerId(0), false);

        let before_p0 = evaluate_features(&state, PlayerId(0)).unwrap();
        let before_p1 = evaluate_features(&state, PlayerId(1)).unwrap();
        assert_eq!(before_p0.mana_development_offset, MANA_DEVELOPMENT_COEFF);
        assert_eq!(before_p1.mana_development_offset, 0.0);

        // Control changes; ownership does NOT.
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.controller = PlayerId(1);
        assert_eq!(state.objects[&land_id].owner, PlayerId(0));

        let after_p0 = evaluate_features(&state, PlayerId(0)).unwrap();
        let after_p1 = evaluate_features(&state, PlayerId(1)).unwrap();
        assert_eq!(after_p0.mana_development_offset, 0.0);
        assert_eq!(after_p1.mana_development_offset, MANA_DEVELOPMENT_COEFF);
    }

    /// Row 12: the offset is SELF-ONLY. Opponent sources must not perturb it — a
    /// (self − opponent) differential implementation fails this.
    #[test]
    fn mana_development_ignores_opponent_sources() {
        let mut state = make_state();
        add_land(&mut state, PlayerId(0), false);
        let baseline = evaluate_features(&state, PlayerId(0))
            .unwrap()
            .mana_development_offset;
        assert!(baseline > 0.0, "reach-guard: evaluator must have a source");

        for _ in 0..5 {
            add_land(&mut state, PlayerId(1), false);
        }
        let after = evaluate_features(&state, PlayerId(0))
            .unwrap()
            .mana_development_offset;
        assert_eq!(
            baseline, after,
            "opponent sources must leave the self-only offset byte-identical"
        );
    }

    /// Row 4d + row 5 + row 6, at the `mana_source_count` seam.
    ///
    /// - a permanent carrying BOTH a renewable and a self-sacrificing mana ability
    ///   counts ONCE (the `.any()` per-ability filter; an all-abilities filter
    ///   returns 0). Run as a NONLAND so `CoreType::Land` cannot mask it.
    /// - a renewable rock counts with zero lands on the battlefield.
    /// - a pure one-shot (Treasure-shaped) source counts ZERO.
    /// - tapped state is ignored: development must not collapse on tap-out.
    #[test]
    fn mana_source_count_classifies_by_renewability_not_tapped_state() {
        // Crystal-Vein-shaped NONLAND: one renewable + one self-sac ability.
        let mut both = make_state();
        add_artifact_with(
            &mut both,
            PlayerId(0),
            vec![renewable_mana_ability(), self_sac_mana_ability()],
        );
        assert_eq!(
            mana_source_count(&both, PlayerId(0)),
            1,
            "at least ONE renewable ability makes the permanent a source; \
             an all-abilities filter wrongly returns 0"
        );

        // Rocks count without any land present.
        let mut rock = make_state();
        add_artifact_with(&mut rock, PlayerId(0), vec![renewable_mana_ability()]);
        assert_eq!(mana_source_count(&rock, PlayerId(0)), 1);

        // A pure one-shot source is NOT development.
        let mut treasure = make_state();
        add_artifact_with(&mut treasure, PlayerId(0), vec![self_sac_mana_ability()]);
        assert_eq!(
            mana_source_count(&treasure, PlayerId(0)),
            0,
            "cracking a Treasure must not read as losing a mana source"
        );

        // A vanilla permanent with no mana ability is not a source.
        let mut vanilla = make_state();
        add_creature(&mut vanilla, PlayerId(0), 2, 2, vec![]);
        assert_eq!(mana_source_count(&vanilla, PlayerId(0)), 0);

        // Development ignores tapped state while availability does not.
        let mut tapped = make_state();
        add_land(&mut tapped, PlayerId(0), true);
        assert_eq!(
            mana_source_count(&tapped, PlayerId(0)),
            1,
            "a tapped land is still part of the manabase"
        );
        assert_eq!(
            crate::zone_eval::available_mana(&tapped, PlayerId(0)),
            0,
            "…while `available_mana` correctly reports zero available"
        );
    }

    /// Row 13: the falsifiable form of premise P4 — `threat_level_projected` never
    /// reads the EVALUATOR's own board, hand, or life.
    ///
    /// P4 is what makes the row-3 floor test valid Commander coverage: it is why
    /// `Δ features.hand_size` is exactly −1 in the multiplayer branch as well as
    /// the 2-player one. If a future strategic term makes `threat_level_projected`
    /// read `board_stats(state, evaluator)`, this row goes red instead of row 3
    /// silently ceasing to cover the multiplayer regime.
    #[test]
    fn threat_level_ignores_evaluator_own_board() {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 4, 42);
        add_creature(&mut state, PlayerId(1), 4, 4, vec![]);
        add_creature(&mut state, PlayerId(2), 2, 2, vec![]);

        let before: Vec<f64> = (1..4)
            .map(|i| threat_level(&state, PlayerId(0), PlayerId(i)))
            .collect();
        assert!(
            before.iter().any(|&t| t > 0.0),
            "reach-guard: at least one opponent must pose nonzero threat, else \
             this row passes vacuously against an all-zero vector"
        );

        // Mutate ONLY the evaluator's own board, hand, and life.
        add_land(&mut state, PlayerId(0), false);
        add_creature(&mut state, PlayerId(0), 3, 3, vec![]);
        // Bind the `CardId` before the `&mut state` borrow — an explicit `&mut` in
        // a free-function argument list is not a two-phase borrow (E0503).
        let card_id = CardId(state.next_object_id);
        let card = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        state.players[0].hand.push_back(card);
        state.players[0].life -= 5;

        let after: Vec<f64> = (1..4)
            .map(|i| threat_level(&state, PlayerId(0), PlayerId(i)))
            .collect();
        assert_eq!(
            before, after,
            "P4 violated: threat_level now reads the evaluator's own state, so the \
             mana-development floor test no longer covers the multiplayer regime"
        );
    }

    #[test]
    fn winning_state_scores_higher_than_losing() {
        let mut state = make_state();
        // Player 0 has big board, player 1 has nothing
        add_creature(&mut state, PlayerId(0), 5, 5, vec![]);
        add_creature(&mut state, PlayerId(0), 3, 3, vec![]);

        let weights = EvalWeights::default();
        let score_p0 = evaluate_state(&state, PlayerId(0), &weights);
        let score_p1 = evaluate_state(&state, PlayerId(1), &weights);

        assert!(
            score_p0 > 0.0,
            "Player with creatures should score positive"
        );
        assert!(
            score_p1 < 0.0,
            "Player without creatures should score negative"
        );
        assert!(score_p0 > score_p1);
    }

    #[test]
    fn game_over_win_is_max_score() {
        let mut state = make_state();
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        let weights = EvalWeights::default();
        assert_eq!(evaluate_state(&state, PlayerId(0), &weights), WIN_SCORE);
        assert_eq!(evaluate_state(&state, PlayerId(1), &weights), LOSS_SCORE);
    }

    #[test]
    fn creature_with_flying_scores_higher() {
        let mut state = make_state();
        let plain = add_creature(&mut state, PlayerId(0), 3, 3, vec![]);
        let flyer = add_creature(&mut state, PlayerId(0), 3, 3, vec![Keyword::Flying]);

        let plain_score = evaluate_creature(&state, plain);
        let flyer_score = evaluate_creature(&state, flyer);
        assert!(
            flyer_score > plain_score,
            "Flying creature should score higher"
        );
    }

    #[test]
    fn tapped_creature_scores_lower() {
        let mut state = make_state();
        let id = add_creature(&mut state, PlayerId(0), 3, 3, vec![]);
        let untapped_score = evaluate_creature(&state, id);

        state.objects.get_mut(&id).unwrap().tapped = true;
        let tapped_score = evaluate_creature(&state, id);

        assert!(untapped_score > tapped_score);
    }

    #[test]
    fn deathtouch_adds_value() {
        let mut state = make_state();
        let plain = add_creature(&mut state, PlayerId(0), 1, 1, vec![]);
        let dt = add_creature(&mut state, PlayerId(0), 1, 1, vec![Keyword::Deathtouch]);

        assert!(evaluate_creature(&state, dt) > evaluate_creature(&state, plain));
    }

    #[test]
    fn life_difference_affects_score() {
        let mut state = make_state();
        state.players[0].life = 20;
        state.players[1].life = 10;
        let weights = EvalWeights::default();
        let score = evaluate_state(&state, PlayerId(0), &weights);
        assert!(score > 0.0, "Ahead on life should score positive");
    }

    #[test]
    fn lethal_life_returns_game_result() {
        let mut state = make_state();
        state.players[1].life = 0;
        let weights = EvalWeights::default();
        assert_eq!(evaluate_state(&state, PlayerId(0), &weights), WIN_SCORE);
    }

    #[test]
    fn threat_level_higher_for_stronger_board() {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        // Player 1 has creatures, player 2 does not
        add_creature(&mut state, PlayerId(1), 5, 5, vec![]);
        add_creature(&mut state, PlayerId(1), 3, 3, vec![]);

        let t1 = threat_level(&state, PlayerId(0), PlayerId(1));
        let t2 = threat_level(&state, PlayerId(0), PlayerId(2));
        assert!(
            t1 > t2,
            "Player with creatures should be more threatening: {t1} vs {t2}"
        );
    }

    #[test]
    fn threat_level_ranges_zero_to_one() {
        let state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        let t = threat_level(&state, PlayerId(0), PlayerId(1));
        assert!((0.0..=1.0).contains(&t), "Threat should be 0-1, got {t}");
    }

    #[test]
    fn multiplayer_eval_focuses_on_highest_threat() {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        // Player 1 is strong (high threat), player 2 is weak
        add_creature(&mut state, PlayerId(1), 5, 5, vec![]);
        add_creature(&mut state, PlayerId(1), 4, 4, vec![]);
        // Player 0 also has a creature
        add_creature(&mut state, PlayerId(0), 3, 3, vec![]);

        let weights = EvalWeights::default();
        let score = evaluate_state(&state, PlayerId(0), &weights);
        // Score should reflect being behind the strongest opponent
        // (threat-weighted, so player 1's stats dominate)
        assert!(score.is_finite());
    }

    #[test]
    fn strategic_intent_pushes_lethal_when_board_represents_kill() {
        let mut state = make_state();
        state.players[1].life = 4;
        add_creature(&mut state, PlayerId(0), 3, 3, vec![]);
        add_creature(&mut state, PlayerId(0), 2, 2, vec![]);

        assert_eq!(
            strategic_intent(&state, PlayerId(0)),
            StrategicIntent::PushLethal
        );
    }

    #[test]
    fn strategic_intent_stabilizes_under_pressure() {
        let mut state = make_state();
        state.players[0].life = 3;
        add_creature(&mut state, PlayerId(1), 4, 4, vec![]);

        assert_eq!(
            strategic_intent(&state, PlayerId(0)),
            StrategicIntent::Stabilize
        );
    }

    #[test]
    fn strategic_intent_preserves_advantage_when_ahead() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), 5, 5, vec![]);
        add_creature(&mut state, PlayerId(1), 2, 2, vec![]);

        assert_eq!(
            strategic_intent(&state, PlayerId(0)),
            StrategicIntent::PreserveAdvantage
        );
    }

    #[test]
    fn opponent_creature_threat_value_weights_equal_bodies_by_controller_threat() {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        let frog = add_creature(&mut state, PlayerId(1), 3, 3, vec![]);
        let krenko = add_creature(&mut state, PlayerId(2), 3, 3, vec![]);
        for _ in 0..10 {
            add_creature(&mut state, PlayerId(2), 1, 1, vec![]);
        }

        let frog_value =
            opponent_battlefield_creature_threat_value(&state, PlayerId(0), frog).unwrap();
        let krenko_value =
            opponent_battlefield_creature_threat_value(&state, PlayerId(0), krenko).unwrap();

        assert!(
            krenko_value > frog_value,
            "equal bodies should inherit controller threat: Krenko={krenko_value}, Frog={frog_value}"
        );
    }

    /// Row 1: `evaluate_state_breakdown` must equal `evaluate_features × weights`
    /// with BOTH fixed offsets added exactly once, AFTER weighting. With
    /// `energy > 0` and at least one mana source, a regression that drops the
    /// refactor or double-counts either offset diverges by a detectable margin.
    #[test]
    fn breakdown_total_equals_weighted_features_plus_both_offsets() {
        let mut state = make_state();
        state.turn_number = 5; // mid phase
        state.players[0].life = 18;
        state.players[1].life = 11;
        add_creature(&mut state, PlayerId(0), 4, 4, vec![]);
        add_creature(&mut state, PlayerId(0), 2, 3, vec![]);
        add_creature(&mut state, PlayerId(1), 3, 2, vec![]);
        state.players[0].energy = 7; // non-vacuous energy_offset
        add_land(&mut state, PlayerId(0), false); // non-vacuous mana_development_offset

        let weights = EvalWeightSet::learned().mid;
        let features = evaluate_features(&state, PlayerId(0)).expect("mid-game is non-terminal");
        assert!(
            features.energy_offset > 0.0,
            "energy term must be non-vacuous"
        );
        assert!(
            features.mana_development_offset > 0.0,
            "mana-development term must be non-vacuous (p0 controls a land)"
        );

        let breakdown = evaluate_state_breakdown(&state, PlayerId(0), &weights)
            .expect("mid-game is non-terminal");

        let reconstructed = features.weighted_total(&weights)
            + features.energy_offset
            + features.mana_development_offset;
        assert!(
            (breakdown.total() - reconstructed).abs() < 1e-9,
            "breakdown.total()={} must equal weighted_total + both offsets={}",
            breakdown.total(),
            reconstructed,
        );
    }

    /// Row 1 hostile: terminal states short-circuit identically in both the
    /// feature extractor and the weighted breakdown (GameOver + lethal-life).
    #[test]
    fn features_and_breakdown_agree_on_terminal_short_circuits() {
        let weights = EvalWeights::default();

        let mut over = make_state();
        over.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        assert_eq!(
            evaluate_features(&over, PlayerId(0)).unwrap_err(),
            evaluate_state_breakdown(&over, PlayerId(0), &weights).unwrap_err(),
        );
        assert_eq!(
            evaluate_features(&over, PlayerId(1)).unwrap_err(),
            evaluate_state_breakdown(&over, PlayerId(1), &weights).unwrap_err(),
        );

        let mut lethal = make_state();
        lethal.players[0].life = 0;
        assert_eq!(
            evaluate_features(&lethal, PlayerId(0)).unwrap_err(),
            LOSS_SCORE,
        );
        assert_eq!(
            evaluate_features(&lethal, PlayerId(0)).unwrap_err(),
            evaluate_state_breakdown(&lethal, PlayerId(0), &weights).unwrap_err(),
        );
    }

    /// Row 1 hostile: the identity also holds on a 3-player threat-weighted
    /// position (the multiplayer aggregation branch), with energy non-zero.
    #[test]
    fn breakdown_identity_holds_for_threat_weighted_multiplayer() {
        let mut state = GameState::new(engine::types::format::FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 9; // late phase
        add_creature(&mut state, PlayerId(0), 3, 3, vec![]);
        add_creature(&mut state, PlayerId(1), 5, 5, vec![]);
        add_creature(&mut state, PlayerId(2), 1, 1, vec![]);
        state.players[0].energy = 3;
        add_land(&mut state, PlayerId(0), false); // non-vacuous mana_development_offset

        let weights = EvalWeightSet::learned().late;
        let features = evaluate_features(&state, PlayerId(0)).expect("non-terminal");
        let breakdown =
            evaluate_state_breakdown(&state, PlayerId(0), &weights).expect("non-terminal");

        assert!(features.energy_offset > 0.0);
        assert!(
            features.mana_development_offset > 0.0,
            "mana-development term must be non-vacuous in the multiplayer branch too"
        );

        let reconstructed = features.weighted_total(&weights)
            + features.energy_offset
            + features.mana_development_offset;
        assert!(
            (breakdown.total() - reconstructed).abs() < 1e-9,
            "multiplayer identity must hold: {} vs {}",
            breakdown.total(),
            reconstructed,
        );
    }

    #[test]
    fn opponent_creature_threat_value_rejects_wrong_relation_zone_and_type() {
        let mut state = GameState::new(
            engine::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        let own = add_creature(&mut state, PlayerId(0), 3, 3, vec![]);
        let teammate = add_creature(&mut state, PlayerId(1), 3, 3, vec![]);
        let eliminated = add_creature(&mut state, PlayerId(2), 3, 3, vec![]);
        state.players[2].is_eliminated = true;

        let noncreature_card_id = CardId(state.next_object_id);
        let noncreature = create_object(
            &mut state,
            noncreature_card_id,
            PlayerId(3),
            "Relic".to_string(),
            Zone::Battlefield,
        );
        let hand_creature_card_id = CardId(state.next_object_id);
        let hand_creature = create_object(
            &mut state,
            hand_creature_card_id,
            PlayerId(3),
            "Hidden Creature".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&hand_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        for id in [own, teammate, eliminated, noncreature, hand_creature] {
            assert_eq!(
                opponent_battlefield_creature_threat_value(&state, PlayerId(0), id),
                None,
                "{id:?} must be outside the living-opponent battlefield-creature contract"
            );
        }
    }
}
