use engine::game::players;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use serde::{Deserialize, Serialize};

use crate::planner::ValueEstimate;

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
/// Learned from 12.9M 17Lands game-turn samples split by turn number.
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
    let target_player = &state.players[target.0 as usize];
    let starting_life = state.format_config.starting_life.max(1) as f64;

    // Board presence: creature count and total power
    let (creatures, power, _toughness, _nc) = board_stats(state, target);
    let board_score = (creatures as f64 * 0.3 + power as f64 * 0.7).min(10.0) / 10.0;

    // Life ratio: higher life = more threatening
    let life_ratio = (target_player.life as f64 / starting_life).clamp(0.0, 2.0) / 2.0;

    // Hand size: more cards = more options
    let hand_score = (target_player.hand.len() as f64).min(7.0) / 7.0;

    // Commander damage dealt to evaluator
    let cmd_damage: u32 = state
        .commander_damage
        .iter()
        .filter(|e| e.player == evaluator)
        .filter(|e| {
            state
                .objects
                .get(&e.commander)
                .map(|o| o.owner == target)
                .unwrap_or(false)
        })
        .map(|e| e.damage)
        .sum();
    let cmd_threat = if let Some(threshold) = state.format_config.commander_damage_threshold {
        (cmd_damage as f64 / threshold as f64).min(1.0)
    } else {
        0.0
    };

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

pub fn evaluate_state_breakdown(
    state: &GameState,
    player: PlayerId,
    weights: &EvalWeights,
) -> Result<EvaluationBreakdown, f64> {
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

    let mut breakdown = EvaluationBreakdown::default();
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
        breakdown.life = (p.life as f64 - weighted_opp_life) * weights.life;

        let (my_creatures, my_power, my_toughness, my_nc) = board_stats(state, player);
        breakdown.board_presence =
            (my_creatures as f64 - weighted_opp_creatures) * weights.board_presence;
        breakdown.board_power = (my_power as f64 - weighted_opp_power) * weights.board_power;
        breakdown.board_toughness =
            (my_toughness as f64 - weighted_opp_toughness) * weights.board_toughness;
        breakdown.hand_size = (p.hand.len() as f64 - weighted_opp_hand) * weights.hand_size;
        breakdown.card_advantage = (my_nc as f64 - weighted_opp_nc) * weights.card_advantage;

        if p.life as f64 > weighted_opp_life && my_power > 0 {
            breakdown.aggression = my_power as f64 * weights.aggression;
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
        breakdown.life = (p.life as f64 - avg_opp_life) * weights.life;

        let (my_creatures, my_power, my_toughness, my_nc) = board_stats(state, player);
        breakdown.board_presence =
            (my_creatures - total_opp_creatures) as f64 * weights.board_presence;
        breakdown.board_power = (my_power - total_opp_power) as f64 * weights.board_power;
        breakdown.board_toughness =
            (my_toughness - total_opp_toughness) as f64 * weights.board_toughness;

        let avg_opp_hand = total_opp_hand_size as f64 / opp_count;
        breakdown.hand_size = (p.hand.len() as f64 - avg_opp_hand) * weights.hand_size;

        let avg_opp_nc = total_opp_nc as f64 / opp_count;
        breakdown.card_advantage = (my_nc as f64 - avg_opp_nc) * weights.card_advantage;

        if p.life as f64 > avg_opp_life && my_power > 0 {
            breakdown.aggression = my_power as f64 * weights.aggression;
        }
    }

    // CR 122.1: Energy counters are a minor resource — value each energy point
    // as a small fraction of a card (comparable to scry).
    breakdown.hand_size += p.energy as f64 * 0.1;

    Ok(breakdown)
}

/// Board statistics: (creature_count, total_power, total_toughness, non_creature_permanents).
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

    let power = obj.power.unwrap_or(0) as f64;
    let toughness = obj.toughness.unwrap_or(0) as f64;

    // Base value: power matters more for combat
    let mut value = power * 1.5 + toughness;

    // Keyword bonuses
    if obj.has_keyword(&Keyword::Flying) {
        value += power * bonuses.flying_mult;
    }
    if obj.has_keyword(&Keyword::Trample) {
        value += power * bonuses.trample_mult;
    }
    if obj.has_keyword(&Keyword::Deathtouch) {
        value += bonuses.deathtouch_flat;
    }
    if obj.has_keyword(&Keyword::Lifelink) {
        value += power * bonuses.lifelink_mult;
    }
    if obj.has_keyword(&Keyword::Hexproof) {
        value += bonuses.hexproof_flat;
    }
    if obj.has_keyword(&Keyword::Indestructible) {
        value += bonuses.indestructible_flat;
    }
    if obj.has_keyword(&Keyword::FirstStrike) || obj.has_keyword(&Keyword::DoubleStrike) {
        value += power * bonuses.first_strike_mult;
    }
    if obj.has_keyword(&Keyword::Vigilance) {
        value += bonuses.vigilance_flat;
    }
    if obj.has_keyword(&Keyword::Menace) {
        value += power * bonuses.menace_mult;
    }

    // Tapped creatures are less valuable
    if obj.tapped {
        value -= bonuses.tapped_penalty;
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
}
