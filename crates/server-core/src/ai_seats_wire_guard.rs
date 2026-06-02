//! Wire validation for `CreateGameWithSettings::ai_seats` on Full-mode hosts.
//!
//! The lobby projection drops `ai_seats` when mapping to `LobbyClientMessage`, so
//! `lobby_broker::guard_inbound` never bounds AI seat payloads. Full-mode create
//! then clones deck names and nested deck lists for every AI seat entry.

use lobby_broker::validate_deck_payload;
use lobby_broker::validation::{validate_token, MAX_PLAYER_COUNT, MAX_TOKEN_LEN};

use crate::protocol::{AiSeatRequest, DeckChoice};

/// Max AI seat entries per create request (host occupies seat 0).
pub const MAX_AI_SEATS: usize = 5;
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

    let max_seat = player_count.clamp(2, MAX_PLAYER_COUNT);
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
        validate_optional_deck_name(&format!("{field}.deck_name"), &seat.deck_name)?;
        if let Some(choice) = &seat.deck {
            validate_deck_choice(&format!("{field}.deck"), choice)?;
        }
    }
    Ok(())
}
