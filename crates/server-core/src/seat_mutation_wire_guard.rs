//! Wire validation for `SeatMutate` frames in `phase-server`.
//!
//! Seat mutation supports nested AI `DeckChoice` payloads (including
//! client-supplied full deck lists). These fields must be bounded before
//! reducer/deck-resolution work to avoid clone-heavy abuse.

use lobby_broker::validate_deck_payload;
use lobby_broker::validation::{validate_token, MAX_PLAYER_COUNT};

use crate::protocol::{DeckChoice, SeatKind, SeatMutation};

/// Max AI starter deck name length in seat mutations.
pub const MAX_AI_DECK_NAME_LEN: usize = 128;

fn validate_deck_choice(field: &str, deck: &DeckChoice) -> Result<(), String> {
    match deck {
        DeckChoice::Random => Ok(()),
        DeckChoice::Named(name) => {
            validate_token(&format!("{field}.name"), name, MAX_AI_DECK_NAME_LEN)
        }
        DeckChoice::DeckList(deck) => validate_deck_payload(&format!("{field}.deck"), deck),
    }
}

/// Validate seat mutation payloads before reducer apply/deck resolution.
pub fn guard_seat_mutation(mutation: &SeatMutation) -> Result<(), String> {
    match mutation {
        SeatMutation::SetKind { seat_index, kind } => {
            if *seat_index >= MAX_PLAYER_COUNT {
                return Err(format!("seat_index must be less than {MAX_PLAYER_COUNT}"));
            }
            if let SeatKind::Ai { deck, .. } = kind {
                validate_deck_choice("mutation.kind.ai.deck", deck)?;
            }
        }
        SeatMutation::Remove { seat_index } => {
            if *seat_index >= MAX_PLAYER_COUNT {
                return Err(format!("seat_index must be less than {MAX_PLAYER_COUNT}"));
            }
        }
        SeatMutation::Start => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use phase_ai::config::AiDifficulty;

    use super::{guard_seat_mutation, MAX_AI_DECK_NAME_LEN};
    use crate::protocol::{DeckChoice, SeatKind, SeatMutation};

    #[test]
    fn seat_mutation_guard_accepts_valid_ai_named_deck() {
        let mutation = SeatMutation::SetKind {
            seat_index: 1,
            kind: SeatKind::Ai {
                difficulty: AiDifficulty::Medium,
                deck: DeckChoice::Named("Mono Green".to_string()),
            },
        };
        assert!(guard_seat_mutation(&mutation).is_ok());
    }

    #[test]
    fn seat_mutation_guard_rejects_oversized_named_deck() {
        let mutation = SeatMutation::SetKind {
            seat_index: 1,
            kind: SeatKind::Ai {
                difficulty: AiDifficulty::Medium,
                deck: DeckChoice::Named("x".repeat(MAX_AI_DECK_NAME_LEN + 1)),
            },
        };
        let err = guard_seat_mutation(&mutation).unwrap_err();
        assert!(err.contains("name"));
    }

    #[test]
    fn seat_mutation_guard_rejects_oversized_seat_index() {
        let mutation = SeatMutation::Remove { seat_index: 200 };
        let err = guard_seat_mutation(&mutation).unwrap_err();
        assert!(err.contains("seat_index"));
    }
}
