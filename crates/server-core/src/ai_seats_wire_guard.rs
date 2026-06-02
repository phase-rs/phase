//! Wire validation for `CreateGameWithSettings::ai_seats` on Full-mode hosts.
//!
//! The lobby projection drops `ai_seats` when mapping to `LobbyClientMessage`, so
//! `lobby_broker::guard_inbound` never bounds AI seat payloads. Full-mode create
//! then clones deck names and nested deck lists for every AI seat entry.

use lobby_broker::validate_deck_payload;
use lobby_broker::validation::validate_token;

use crate::protocol::{AiSeatRequest, DeckChoice};

/// Full-mode game sessions support at most six seats.
pub const MAX_FULL_GAME_PLAYER_COUNT: u8 = 6;
/// Max AI seat entries per create request (host occupies seat 0).
pub const MAX_AI_SEATS: usize = (MAX_FULL_GAME_PLAYER_COUNT - 1) as usize;
/// Max starter-deck name length accepted on the wire for AI seats.
pub const MAX_AI_DECK_NAME_LEN: usize = 128;

fn validate_optional_deck_name(field: &str, value: &Option<String>) -> Result<(), String> {
    match value {
        Some(name) => validate_token(field, name, MAX_AI_DECK_NAME_LEN),
        None => Ok(()),
    }
}

fn validate_deck_choice(field: &str, choice: &DeckChoice) -> Result<(), String> {
    match choice {
        DeckChoice::Random => Ok(()),
        DeckChoice::Named(name) => {
            validate_token(&format!("{field}.name"), name, MAX_AI_DECK_NAME_LEN)
        }
        DeckChoice::DeckList(deck) => validate_deck_payload(&format!("{field}.deck"), deck),
    }
}

/// Validate AI seat wire payloads before deck resolve and session setup.
pub fn guard_create_ai_seats(ai_seats: &[AiSeatRequest], player_count: u8) -> Result<(), String> {
    if ai_seats.len() > MAX_AI_SEATS {
        return Err(format!(
            "ai_seats must contain at most {MAX_AI_SEATS} entries"
        ));
    }

    let max_seat = player_count.clamp(2, MAX_FULL_GAME_PLAYER_COUNT);
    let mut seen_seats = 0u8;
    for (index, seat) in ai_seats.iter().enumerate() {
        let field = format!("ai_seats[{index}]");
        if seat.seat_index == 0 {
            return Err(format!("{field}.seat_index must not be 0 (host seat)"));
        }
        if seat.seat_index >= max_seat {
            return Err(format!(
                "{field}.seat_index must be less than player_count ({max_seat})"
            ));
        }
        let seat_mask = 1u8 << seat.seat_index;
        if (seen_seats & seat_mask) != 0 {
            return Err(format!(
                "{field}.seat_index ({}) is duplicated",
                seat.seat_index
            ));
        }
        seen_seats |= seat_mask;
        validate_optional_deck_name(&format!("{field}.deck_name"), &seat.deck_name)?;
        if let Some(choice) = &seat.deck {
            validate_deck_choice(&format!("{field}.deck"), choice)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use phase_ai::config::AiDifficulty;

    use super::{guard_create_ai_seats, MAX_AI_DECK_NAME_LEN, MAX_AI_SEATS};
    use crate::protocol::{AiSeatRequest, DeckChoice};

    fn ai_seat(seat_index: u8) -> AiSeatRequest {
        AiSeatRequest {
            seat_index,
            difficulty: AiDifficulty::Medium,
            deck_name: None,
            deck: None,
        }
    }

    #[test]
    fn ai_seats_accepts_valid_entry() {
        assert!(guard_create_ai_seats(&[ai_seat(1)], 4).is_ok());
    }

    #[test]
    fn ai_seats_rejects_too_many_entries() {
        let seats: Vec<AiSeatRequest> = (0..=MAX_AI_SEATS).map(|_| ai_seat(1)).collect();
        let err = guard_create_ai_seats(&seats, 6).unwrap_err();
        assert!(err.contains("ai_seats"));
    }

    #[test]
    fn ai_seats_rejects_duplicate_seats() {
        let seats = vec![ai_seat(1), ai_seat(1)];
        let err = guard_create_ai_seats(&seats, 4).unwrap_err();
        assert!(err.contains("duplicated"));
    }

    #[test]
    fn ai_seats_rejects_seat_outside_effective_player_count() {
        let err = guard_create_ai_seats(&[ai_seat(6)], 8).unwrap_err();
        assert!(err.contains("less than player_count (6)"));
    }

    #[test]
    fn ai_seats_rejects_oversized_named_deck() {
        let seats = vec![AiSeatRequest {
            seat_index: 1,
            difficulty: AiDifficulty::Medium,
            deck_name: None,
            deck: Some(DeckChoice::Named("x".repeat(MAX_AI_DECK_NAME_LEN + 1))),
        }];
        let err = guard_create_ai_seats(&seats, 4).unwrap_err();
        assert!(err.contains("name"));
    }
}
