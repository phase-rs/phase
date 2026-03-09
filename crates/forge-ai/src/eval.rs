use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Weights for board evaluation heuristics.
#[derive(Debug, Clone)]
pub struct EvalWeights {
    pub life: f64,
    pub aggression: f64,
    pub board_presence: f64,
    pub board_power: f64,
    pub board_toughness: f64,
    pub hand_size: f64,
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
        }
    }
}

const WIN_SCORE: f64 = 10000.0;
const LOSS_SCORE: f64 = -10000.0;

/// Evaluate the board state from `player`'s perspective.
/// Returns a score where higher is better for `player`.
pub fn evaluate_state(state: &GameState, player: PlayerId, weights: &EvalWeights) -> f64 {
    // Check for game over
    if let WaitingFor::GameOver { winner } = &state.waiting_for {
        return match winner {
            Some(w) if *w == player => WIN_SCORE,
            Some(_) => LOSS_SCORE,
            None => 0.0, // draw
        };
    }

    let opponent = PlayerId(1 - player.0);
    let p = &state.players[player.0 as usize];
    let o = &state.players[opponent.0 as usize];

    // Check for lethal life totals
    if p.life <= 0 {
        return LOSS_SCORE;
    }
    if o.life <= 0 {
        return WIN_SCORE;
    }

    let mut score = 0.0;

    // Life differential
    score += (p.life - o.life) as f64 * weights.life;

    // Board stats
    let (my_creatures, my_power, my_toughness) = board_stats(state, player);
    let (opp_creatures, opp_power, opp_toughness) = board_stats(state, opponent);

    score += (my_creatures - opp_creatures) as f64 * weights.board_presence;
    score += (my_power - opp_power) as f64 * weights.board_power;
    score += (my_toughness - opp_toughness) as f64 * weights.board_toughness;

    // Hand size advantage
    score += (p.hand.len() as f64 - o.hand.len() as f64) * weights.hand_size;

    // Aggression bonus: reward attacking potential when ahead
    if p.life > o.life && my_power > 0 {
        score += my_power as f64 * weights.aggression;
    }

    score
}

fn board_stats(state: &GameState, player: PlayerId) -> (i32, i32, i32) {
    let mut creatures = 0;
    let mut total_power = 0;
    let mut total_toughness = 0;

    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            if obj.controller == player && obj.card_types.core_types.contains(&CoreType::Creature) {
                creatures += 1;
                total_power += obj.power.unwrap_or(0);
                total_toughness += obj.toughness.unwrap_or(0);
            }
        }
    }

    (creatures, total_power, total_toughness)
}

/// Evaluate a single creature's combat value.
/// Higher scores indicate more valuable creatures.
pub fn evaluate_creature(state: &GameState, obj_id: ObjectId) -> f64 {
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
        value += power;
    }
    if obj.has_keyword(&Keyword::Trample) {
        value += power * 0.5;
    }
    if obj.has_keyword(&Keyword::Deathtouch) {
        value += 3.0;
    }
    if obj.has_keyword(&Keyword::Lifelink) {
        value += power * 0.5;
    }
    if obj.has_keyword(&Keyword::Hexproof) {
        value += 2.0;
    }
    if obj.has_keyword(&Keyword::Indestructible) {
        value += 4.0;
    }
    if obj.has_keyword(&Keyword::FirstStrike) || obj.has_keyword(&Keyword::DoubleStrike) {
        value += power * 0.8;
    }
    if obj.has_keyword(&Keyword::Vigilance) {
        value += 1.0;
    }
    if obj.has_keyword(&Keyword::Menace) {
        value += power * 0.5;
    }

    // Tapped creatures are less valuable
    if obj.tapped {
        value -= 1.5;
    }

    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::card_type::CoreType;
    use engine::types::identifiers::CardId;

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
}
