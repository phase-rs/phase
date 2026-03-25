use engine::game::players;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

/// Compute the card-advantage differential between `player` and their strongest opponent.
///
/// Resources counted: real permanents (1.0 each), tokens (0.5 each), and hand cards (1.0 each).
/// Returns a positive value when the player is ahead, negative when behind.
pub fn differential(state: &GameState, player: PlayerId) -> f64 {
    let my_resources = count_resources(state, player);
    let opponents = players::opponents(state, player);
    if opponents.is_empty() {
        return 0.0;
    }
    // Compare against the best-resourced opponent (most relevant threat).
    let max_opp = opponents
        .iter()
        .map(|&opp| count_resources(state, opp))
        .fold(f64::NEG_INFINITY, f64::max);
    my_resources - max_opp
}

/// Count a player's total resources.
///
/// Real permanents cost a card each (1.0). Tokens are "free" (0.5 each).
/// Hand cards count as 1.0 each (they represent future plays).
fn count_resources(state: &GameState, player: PlayerId) -> f64 {
    let mut real_permanents = 0u32;
    let mut tokens = 0u32;
    for &id in &state.battlefield {
        if let Some(obj) = state.objects.get(&id) {
            if obj.controller == player {
                if obj.is_token {
                    tokens += 1;
                } else {
                    real_permanents += 1;
                }
            }
        }
    }
    let hand = state.players[player.0 as usize].hand.len() as f64;
    real_permanents as f64 + (tokens as f64 * 0.5) + hand
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

    fn add_permanent(state: &mut GameState, owner: PlayerId, is_token: bool) {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Permanent".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.controller = owner;
        obj.is_token = is_token;
    }

    #[test]
    fn more_permanents_means_ahead() {
        let mut state = make_state();
        // Player 0: 5 permanents, Player 1: 3 permanents
        for _ in 0..5 {
            add_permanent(&mut state, PlayerId(0), false);
        }
        for _ in 0..3 {
            add_permanent(&mut state, PlayerId(1), false);
        }
        let diff = differential(&state, PlayerId(0));
        assert!(diff > 0.0, "5 vs 3 permanents should be positive: {diff}");
    }

    #[test]
    fn tokens_count_at_half() {
        let mut state = make_state();
        // Player 0: 2 tokens + 2 hand cards
        add_permanent(&mut state, PlayerId(0), true);
        add_permanent(&mut state, PlayerId(0), true);
        // Hand already has initial cards from new_two_player; clear and add exactly 2
        state.players[0].hand.clear();
        let h1 = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let h2 = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Card".to_string(),
            Zone::Hand,
        );
        let _ = (h1, h2);

        let resources = count_resources(&state, PlayerId(0));
        // 2 tokens * 0.5 + 2 hand = 3.0
        assert!(
            (resources - 3.0).abs() < f64::EPSILON,
            "Expected 3.0 resources, got {resources}"
        );
    }

    #[test]
    fn symmetrical_board_is_zero() {
        let mut state = make_state();
        // Equal permanents and hand sizes
        for _ in 0..3 {
            add_permanent(&mut state, PlayerId(0), false);
            add_permanent(&mut state, PlayerId(1), false);
        }
        // Equalize hands
        state.players[0].hand.clear();
        state.players[1].hand.clear();

        let diff = differential(&state, PlayerId(0));
        assert!(
            diff.abs() < f64::EPSILON,
            "Equal resources should be ~0.0: {diff}"
        );
    }
}
