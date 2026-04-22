use rand::seq::SliceRandom;

use crate::game::effects::resolve_effect;
use crate::types::ability::{AbilityKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::turns;
use super::zones;

/// CR 103.4: Starting hand size is seven cards.
const STARTING_HAND_SIZE: usize = 7;
const MAX_MULLIGANS: u8 = 7;

/// CR 103.4: Start the mulligan process — shuffle libraries and draw 7 for each player.
pub fn start_mulligan(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    events.push(GameEvent::MulliganStarted);

    // Shuffle both libraries
    for player in &mut state.players {
        player.library.shuffle(&mut state.rng);
    }

    // Draw 7 for each player in seat order
    let seat_order = state.seat_order.clone();
    for &player_id in &seat_order {
        draw_n(state, player_id, STARTING_HAND_SIZE, events);
    }

    // First player in seat order gets the first mulligan decision
    let first_player = state.seat_order.first().copied().unwrap_or(PlayerId(0));
    WaitingFor::MulliganDecision {
        player: first_player,
        mulligan_count: 0,
    }
}

/// CR 103.5: London mulligan — draw 7 each time, put N cards on bottom after keeping.
pub fn handle_mulligan_decision(
    state: &mut GameState,
    player: PlayerId,
    keep: bool,
    mulligan_count: u8,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    // CR 103.5c: In multiplayer (3+ players), the first mulligan is free —
    // it doesn't count toward the number of cards put on the bottom.
    let is_multiplayer = state.seat_order.len() > 2;

    if keep {
        // CR 103.5: Bottom N cards where N = mulligans taken,
        // minus 1 in multiplayer for the free first mulligan (CR 103.5c).
        let bottom_count = if is_multiplayer {
            mulligan_count.saturating_sub(1)
        } else {
            mulligan_count
        };
        if bottom_count > 0 {
            // Need to put cards on bottom
            WaitingFor::MulliganBottomCards {
                player,
                count: bottom_count,
            }
        } else {
            // No cards to bottom, move to next player
            advance_mulligan(state, player, events)
        }
    } else {
        let new_count = mulligan_count + 1;

        // Mulligan: check if forced keep at max
        if new_count >= MAX_MULLIGANS {
            // Force keep with 0 cards (no cards to bottom since hand will be empty)
            // Shuffle hand into library, draw 7
            shuffle_hand_into_library(state, player, events);
            draw_n(state, player, STARTING_HAND_SIZE, events);
            // Must bottom 7 cards (minus 1 for free mulligan in multiplayer)
            let bottom = if is_multiplayer {
                MAX_MULLIGANS - 1
            } else {
                MAX_MULLIGANS
            };
            WaitingFor::MulliganBottomCards {
                player,
                count: bottom,
            }
        } else {
            // Shuffle hand into library, draw 7 again
            shuffle_hand_into_library(state, player, events);
            draw_n(state, player, STARTING_HAND_SIZE, events);
            WaitingFor::MulliganDecision {
                player,
                mulligan_count: new_count,
            }
        }
    }
}

/// CR 103.5: Player chooses which cards to put on the bottom of their library.
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

    advance_mulligan(state, player, events).pipe(Ok)
}

/// Move to the next player's mulligan in seat order, or finish mulligans if all done.
fn advance_mulligan(
    state: &mut GameState,
    current_player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    // Find the next player in seat_order after current_player
    let seat_order = &state.seat_order;
    let current_idx = seat_order
        .iter()
        .position(|&id| id == current_player)
        .unwrap_or(0);

    // Check if there's another player after current in seat order
    if current_idx + 1 < seat_order.len() {
        let next_player = seat_order[current_idx + 1];
        WaitingFor::MulliganDecision {
            player: next_player,
            mulligan_count: 0,
        }
    } else {
        finish_mulligans(state, events)
    }
}

/// Execute all BeginGame abilities for cards in each player's opening hand.
/// Called once after all mulligan decisions are finalized.
fn execute_begin_game_abilities(state: &mut GameState, events: &mut Vec<GameEvent>) {
    // Collect first to avoid borrow conflict during mutation
    let begin_game: Vec<(PlayerId, ObjectId, crate::types::ability::Effect)> = state
        .seat_order
        .clone()
        .into_iter()
        .flat_map(|player_id| {
            let player = state
                .players
                .iter()
                .find(|p| p.id == player_id)
                .expect("player exists");
            player
                .hand
                .iter()
                .filter_map(|&obj_id| {
                    let obj = state.objects.get(&obj_id)?;
                    let ability = obj
                        .abilities
                        .iter()
                        .find(|a| a.kind == AbilityKind::BeginGame)?;
                    Some((player_id, obj_id, *ability.effect.clone()))
                })
                .collect::<Vec<_>>()
        })
        .collect();

    for (player_id, obj_id, effect) in begin_game {
        let ability =
            ResolvedAbility::new(effect, vec![TargetRef::Object(obj_id)], obj_id, player_id);
        let _ = resolve_effect(state, &ability, events);
    }
}

/// All players have kept. Start the game properly.
fn finish_mulligans(state: &mut GameState, events: &mut Vec<GameEvent>) -> WaitingFor {
    execute_begin_game_abilities(state, events);
    turns::auto_advance(state, events)
}

fn shuffle_hand_into_library(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
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

        let waiting = handle_mulligan_decision(&mut state, PlayerId(0), true, 0, &mut events);

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
        let waiting = handle_mulligan_decision(&mut state, PlayerId(0), false, 0, &mut events);
        assert!(matches!(
            waiting,
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 1,
            }
        ));

        // Keep after 1 mulligan
        let waiting = handle_mulligan_decision(&mut state, PlayerId(0), true, 1, &mut events);
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
        handle_mulligan_decision(&mut state, PlayerId(0), false, 0, &mut events);

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
        assert_eq!(*state.players[0].library.last().unwrap(), card_to_bottom,);
    }

    #[test]
    fn handle_bottom_cards_wrong_count_errors() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        let result = handle_mulligan_bottom(&mut state, PlayerId(0), vec![], 1, &mut events);

        assert!(result.is_err());
    }

    #[test]
    fn both_players_keep_starts_game() {
        let mut state = setup_with_libraries(20);
        let mut events = Vec::new();
        start_mulligan(&mut state, &mut events);

        // Player 0 keeps
        let waiting = handle_mulligan_decision(&mut state, PlayerId(0), true, 0, &mut events);
        assert!(matches!(
            waiting,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                ..
            }
        ));

        // Player 1 keeps
        let waiting = handle_mulligan_decision(&mut state, PlayerId(1), true, 0, &mut events);

        // Should auto-advance to PreCombatMain
        assert!(matches!(waiting, WaitingFor::Priority { .. }));
    }

    #[test]
    fn multiplayer_first_mulligan_is_free() {
        // Create a 3-player game
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state.turn_number = 1;
        state.phase = crate::types::phase::Phase::Untap;

        // Add library cards for all 3 players
        for player_idx in 0..3u8 {
            for i in 0..20 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }

        let mut events = Vec::new();
        let _waiting = start_mulligan(&mut state, &mut events);

        // CR 103.5c: First mulligan increments count, but keeping after 1 mulligan
        // in multiplayer means 0 cards on bottom (free mulligan).
        let waiting = handle_mulligan_decision(&mut state, PlayerId(0), false, 0, &mut events);
        assert!(
            matches!(
                waiting,
                WaitingFor::MulliganDecision {
                    mulligan_count: 1,
                    ..
                }
            ),
            "Mulligan count should increment to 1"
        );

        // Keep after first mulligan — free mulligan means 0 cards on bottom
        let waiting = handle_mulligan_decision(&mut state, PlayerId(0), true, 1, &mut events);
        // In multiplayer, 1 mulligan = 0 bottom cards → advance directly (no MulliganBottomCards)
        assert!(
            !matches!(waiting, WaitingFor::MulliganBottomCards { .. }),
            "First mulligan should be free — no cards to bottom"
        );

        // Second mulligan in a fresh scenario: keep after 2 mulligans → 1 card on bottom
        let mut state2 = GameState::new(crate::types::format::FormatConfig::standard(), 3, 42);
        state2.turn_number = 1;
        state2.phase = crate::types::phase::Phase::Untap;
        for player_idx in 0..3u8 {
            for i in 0..20 {
                create_object(
                    &mut state2,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        let mut events2 = Vec::new();
        let _waiting = start_mulligan(&mut state2, &mut events2);
        // Two mulligans
        let _w = handle_mulligan_decision(&mut state2, PlayerId(0), false, 0, &mut events2);
        let _w = handle_mulligan_decision(&mut state2, PlayerId(0), false, 1, &mut events2);
        // Keep after 2 mulligans — should bottom 1 card (2 - 1 free = 1)
        let waiting = handle_mulligan_decision(&mut state2, PlayerId(0), true, 2, &mut events2);
        assert!(
            matches!(waiting, WaitingFor::MulliganBottomCards { count: 1, .. }),
            "After 2 mulligans in multiplayer, should bottom 1 card (free mulligan discount)"
        );
    }

    /// Regression: In a 1v1 Commander game where P1 (AI) is the starting player,
    /// the engine must authorize P1 to submit MulliganDecision.
    #[test]
    fn ai_starting_player_can_submit_mulligan_decision() {
        use crate::game::engine::{apply, start_game_with_starting_player};
        use crate::types::actions::GameAction;
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::commander(), 2, 42);
        // Populate libraries so mulligan flow engages
        for player_idx in 0..2u8 {
            for i in 0..10 {
                create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i as u64),
                    PlayerId(player_idx),
                    format!("Card {} P{}", i, player_idx),
                    Zone::Library,
                );
            }
        }
        // Add commanders for both players (matches recent fix)
        let c0 = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "P0 Cmd".to_string(),
            Zone::Command,
        );
        let c1 = create_object(
            &mut state,
            CardId(201),
            PlayerId(1),
            "P1 Cmd".to_string(),
            Zone::Command,
        );
        state.objects.get_mut(&c0).unwrap().is_commander = true;
        state.objects.get_mut(&c1).unwrap().is_commander = true;

        let result = start_game_with_starting_player(&mut state, PlayerId(1));

        // Engine should want P1 (AI) to mulligan first
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::MulliganDecision {
                    player: PlayerId(1),
                    ..
                }
            ),
            "expected MulliganDecision {{ player: 1 }}, got {:?}",
            result.waiting_for
        );

        let authorized = crate::game::turn_control::authorized_submitter(&state);
        assert_eq!(
            authorized,
            Some(PlayerId(1)),
            "authorized_submitter should be P1 (the mulligan player)"
        );

        // AI dispatches its mulligan decision
        let r = apply(
            &mut state,
            PlayerId(1),
            GameAction::MulliganDecision { keep: true },
        );
        assert!(
            r.is_ok(),
            "AI P1 should be authorized to submit MulliganDecision, got {:?}",
            r
        );
    }
}
