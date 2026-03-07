use rand::seq::SliceRandom;

use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::turns;
use super::zones;

const STARTING_HAND_SIZE: usize = 7;
const MAX_MULLIGANS: u8 = 7;

/// Start the mulligan process: shuffle libraries and draw 7 for each player.
pub fn start_mulligan(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    events.push(GameEvent::MulliganStarted);

    // Shuffle both libraries
    for player in &mut state.players {
        player.library.shuffle(&mut state.rng);
    }

    // Draw 7 for each player
    for player_idx in 0..state.players.len() {
        let player_id = state.players[player_idx].id;
        draw_n(state, player_id, STARTING_HAND_SIZE, events);
    }

    WaitingFor::MulliganDecision {
        player: PlayerId(0),
        mulligan_count: 0,
    }
}

/// Handle a mulligan keep/mull decision.
pub fn handle_mulligan_decision(
    state: &mut GameState,
    player: PlayerId,
    keep: bool,
    mulligan_count: u8,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    if keep {
        if mulligan_count > 0 {
            // Need to put cards on bottom
            WaitingFor::MulliganBottomCards {
                player,
                count: mulligan_count,
            }
        } else {
            // No cards to bottom, move to next player
            advance_mulligan(state, player, events)
        }
    } else {
        // Mulligan: check if forced keep at max
        if mulligan_count + 1 >= MAX_MULLIGANS {
            // Force keep with 0 cards (no cards to bottom since hand will be empty)
            // Shuffle hand into library, draw 7
            shuffle_hand_into_library(state, player, events);
            draw_n(state, player, STARTING_HAND_SIZE, events);
            // Must bottom 7 cards
            WaitingFor::MulliganBottomCards {
                player,
                count: MAX_MULLIGANS,
            }
        } else {
            // Shuffle hand into library, draw 7 again
            shuffle_hand_into_library(state, player, events);
            draw_n(state, player, STARTING_HAND_SIZE, events);
            WaitingFor::MulliganDecision {
                player,
                mulligan_count: mulligan_count + 1,
            }
        }
    }
}

/// Handle selecting cards to put on bottom of library after keeping a mulligan hand.
pub fn handle_mulligan_bottom(
    state: &mut GameState,
    player: PlayerId,
    cards: Vec<ObjectId>,
    expected_count: u8,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    if cards.len() != expected_count as usize {
        return Err(format!(
            "Expected {} cards to bottom, got {}",
            expected_count,
            cards.len()
        ));
    }

    // Validate all cards are in player's hand
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");
    for &card_id in &cards {
        if !player_data.hand.contains(&card_id) {
            return Err(format!("Card {:?} is not in player's hand", card_id));
        }
    }

    // Move each card to bottom of library
    for card_id in cards {
        zones::move_to_library_position(state, card_id, false, events);
    }

    advance_mulligan(state, player, events)
        .pipe(Ok)
}

/// Move to the next player's mulligan, or finish mulligans if all done.
fn advance_mulligan(
    state: &mut GameState,
    current_player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let next_player = PlayerId(current_player.0 + 1);
    if (next_player.0 as usize) < state.players.len() {
        WaitingFor::MulliganDecision {
            player: next_player,
            mulligan_count: 0,
        }
    } else {
        finish_mulligans(state, events)
    }
}

/// Both players have kept. Start the game properly.
fn finish_mulligans(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    turns::auto_advance(state, events)
}

fn shuffle_hand_into_library(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let hand_ids: Vec<ObjectId> = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .clone();

    for card_id in hand_ids {
        zones::move_to_zone(state, card_id, Zone::Library, events);
    }

    // Shuffle library
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    player_data.library.shuffle(&mut state.rng);
}

fn draw_n(state: &mut GameState, player_id: PlayerId, count: usize, events: &mut Vec<GameEvent>) {
    for _ in 0..count {
        let player = state
            .players
            .iter()
            .find(|p| p.id == player_id)
            .expect("player exists");

        if player.library.is_empty() {
            break;
        }

        let top_card = player.library[0];
        zones::move_to_zone(state, top_card, Zone::Hand, events);
    }

    events.push(GameEvent::CardsDrawn {
        player_id,
        count: count as u32,
    });
}

/// Extension trait to pipe values (like Rust nightly's pipe)
trait Pipe: Sized {
    fn pipe<F, R>(self, f: F) -> R
    where
        F: FnOnce(Self) -> R,
    {
        f(self)
    }
}

impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;

    fn setup_with_libraries(cards_per_player: usize) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;

        for player_idx in 0..2u8 {
            for i in 0..cards_per_player {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }

        state
    }

    #[test]
    fn start_mulligan_draws_seven_for_each_player() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();

        let waiting = start_mulligan(&mut state, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);
        assert_eq!(state.players[0].library.len(), 13);
        assert_eq!(state.players[1].library.len(), 13);
        assert!(matches!(
            waiting,
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 0,
            }
        ));
    }

    #[test]
    fn start_mulligan_emits_event() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();

        start_mulligan(&mut state, &mut events);

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::MulliganStarted)));
    }

    #[test]
    fn keep_with_zero_mulligans_advances_to_next_player() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        let waiting = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            true,
            0,
            &mut events,
        );

        assert!(matches!(
            waiting,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                mulligan_count: 0,
            }
        ));
    }

    #[test]
    fn keep_after_mulligan_requests_bottom_cards() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        // Mulligan once
        let waiting = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            false,
            0,
            &mut events,
        );
        assert!(matches!(
            waiting,
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 1,
            }
        ));

        // Keep after 1 mulligan
        let waiting = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            true,
            1,
            &mut events,
        );
        assert!(matches!(
            waiting,
            WaitingFor::MulliganBottomCards {
                player: PlayerId(0),
                count: 1,
            }
        ));
    }

    #[test]
    fn mulligan_redraws_seven() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        assert_eq!(state.players[0].hand.len(), 7);

        // Mulligan
        handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            false,
            0,
            &mut events,
        );

        // Should still have 7 in hand after redraw
        assert_eq!(state.players[0].hand.len(), 7);
    }

    #[test]
    fn handle_bottom_cards_puts_on_bottom() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        // Mulligan once, then keep
        handle_mulligan_decision(&mut state, PlayerId(0), false, 0, &mut events);
        handle_mulligan_decision(&mut state, PlayerId(0), true, 1, &mut events);

        // Put 1 card on bottom
        let card_to_bottom = state.players[0].hand[0];
        let result = handle_mulligan_bottom(
            &mut state,
            PlayerId(0),
            vec![card_to_bottom],
            1,
            &mut events,
        );

        assert!(result.is_ok());
        assert_eq!(state.players[0].hand.len(), 6); // 7 - 1
        // Card should be at bottom of library
        assert_eq!(
            *state.players[0].library.last().unwrap(),
            card_to_bottom,
        );
    }

    #[test]
    fn handle_bottom_cards_wrong_count_errors() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        let result = handle_mulligan_bottom(
            &mut state,
            PlayerId(0),
            vec![],
            1,
            &mut events,
        );

        assert!(result.is_err());
    }

    #[test]
    fn both_players_keep_starts_game() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        // Player 0 keeps
        let waiting = handle_mulligan_decision(
            &mut state,
            PlayerId(0),
            true,
            0,
            &mut events,
        );
        assert!(matches!(
            waiting,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                ..
            }
        ));

        // Player 1 keeps
        let waiting = handle_mulligan_decision(
            &mut state,
            PlayerId(1),
            true,
            0,
            &mut events,
        );

        // Should auto-advance to PreCombatMain
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }
}
