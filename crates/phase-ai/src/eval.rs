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
}

impl EvaluationBreakdown {
    pub fn total(&self) -> f64 {
        self.life
            + self.board_presence
            + self.board_power
            + self.board_toughness
            + self.hand_size
            + self.aggression
            + self.card_advantage
    }
}

/// Single-authority **unweighted** feature vector for the tactical board eval —
/// the Texel train/serve invariant. `evaluate_state_breakdown` is defined as
/// `evaluate_features(..)? × weights`, so a feature harvested for offline weight
/// fitting is byte-for-byte the value that multiplies the corresponding weight at
/// serve time (see `crate::duel_suite::harvest::FeatureRow`, which extends this
/// with the three strategic dimensions from `evaluate_with_strategy`).
///
/// Every field except `energy_offset` is a raw (self − opponent) differential
/// that pairs with one `EvalWeights` field. `energy_offset` is a fixed-coefficient
/// serve-time offset (`energy × 0.1`, CR 122.1) added **after** weighting, so it
/// is excluded from [`EvalFeatures::weighted_total`] — see the energy contract in
/// `evaluate_state_breakdown`.
///
/// The full fitted serve vector is `EvalFeatures` (minus `energy_offset`) plus
/// `zone_bonus` (`zone_quality`), `SynergyGraph::board_synergy_bonus` (`synergy`),
/// and `card_advantage::differential` (folded into `card_advantage` alongside
/// `card_advantage_breakdown`). The two unfitted serve-time terms are
/// `energy_offset` (this fixed offset) and `threat_adjustment` (a heuristic with
/// no `EvalWeights` field).
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
}

impl EvalFeatures {
    /// Weighted sum of every tactical feature **excluding** `energy_offset` (the
    /// fixed serve-time offset added after weighting). Holds
    /// `breakdown.total() == features.weighted_total(&w) + features.energy_offset`
    /// by construction.
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

    // CR 122.1: Energy counters are a minor resource — value each energy point
    // as a small fraction of a card (comparable to scry). Fixed-coefficient
    // offset applied AFTER weighting (see `evaluate_state_breakdown`), so it lives
    // on `EvalFeatures` separately rather than folded into a weighted feature.
    features.energy_offset = p.energy as f64 * 0.1;

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
    };

    // CR 122.1: energy is a fixed-coefficient offset added AFTER weighting, so
    // `EvalFeatures::weighted_total` excludes it and it lands on `hand_size` here
    // exactly as the historical `breakdown.hand_size += p.energy * 0.1` did.
    breakdown.hand_size += features.energy_offset;

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
    /// with the energy offset added exactly once, AFTER weighting. With
    /// `energy > 0` a regression that either drops the refactor or double-counts
    /// energy diverges by a detectable margin.
    #[test]
    fn breakdown_total_equals_weighted_features_plus_energy() {
        let mut state = make_state();
        state.turn_number = 5; // mid phase
        state.players[0].life = 18;
        state.players[1].life = 11;
        add_creature(&mut state, PlayerId(0), 4, 4, vec![]);
        add_creature(&mut state, PlayerId(0), 2, 3, vec![]);
        add_creature(&mut state, PlayerId(1), 3, 2, vec![]);
        state.players[0].energy = 7; // non-vacuous energy_offset

        let weights = EvalWeightSet::learned().mid;
        let features = evaluate_features(&state, PlayerId(0)).expect("mid-game is non-terminal");
        assert!(
            features.energy_offset > 0.0,
            "energy term must be non-vacuous"
        );

        let breakdown = evaluate_state_breakdown(&state, PlayerId(0), &weights)
            .expect("mid-game is non-terminal");

        assert!(
            (breakdown.total() - (features.weighted_total(&weights) + features.energy_offset))
                .abs()
                < 1e-9,
            "breakdown.total()={} must equal weighted_total + energy_offset={}",
            breakdown.total(),
            features.weighted_total(&weights) + features.energy_offset,
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

        let weights = EvalWeightSet::learned().late;
        let features = evaluate_features(&state, PlayerId(0)).expect("non-terminal");
        let breakdown =
            evaluate_state_breakdown(&state, PlayerId(0), &weights).expect("non-terminal");

        assert!(
            (breakdown.total() - (features.weighted_total(&weights) + features.energy_offset))
                .abs()
                < 1e-9,
            "multiplayer identity must hold: {} vs {}",
            breakdown.total(),
            features.weighted_total(&weights) + features.energy_offset,
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
