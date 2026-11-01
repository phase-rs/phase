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

fn bound_seat_index(seat_index: u8) -> Result<(), String> {
    if seat_index >= MAX_PLAYER_COUNT {
        return Err(format!("seat_index must be less than {MAX_PLAYER_COUNT}"));
    }
    Ok(())
}

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
        SeatMutation::SetKind {
            seat_index,
            kind,
            expected_kind,
        } => {
            bound_seat_index(*seat_index)?;
            if let SeatKind::Ai { deck, .. } = kind {
                validate_deck_choice("mutation.kind.ai.deck", deck)?;
            }
            if let Some(SeatKind::Ai { deck, .. }) = expected_kind {
                validate_deck_choice("mutation.expectedKind.ai.deck", deck)?;
            }
        }
        SeatMutation::Remove { seat_index } => {
            bound_seat_index(*seat_index)?;
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
            expected_kind: None,
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
            expected_kind: None,
            kind: SeatKind::Ai {
                difficulty: AiDifficulty::Medium,
                deck: DeckChoice::Named("x".repeat(MAX_AI_DECK_NAME_LEN + 1)),
            },
        };
        let err = guard_seat_mutation(&mutation).unwrap_err();
        assert!(err.contains("name"));
    }

    #[test]
    fn seat_mutation_guard_bounds_expected_ai_decks() {
        use engine::starter_decks::DeckData;
        use lobby_broker::inbound_guard::MAX_MAIN_DECK_ENTRIES;

        for deck in [
            DeckChoice::Named("x".repeat(MAX_AI_DECK_NAME_LEN)),
            DeckChoice::DeckList(Box::new(DeckData {
                main_deck: vec!["Forest".to_string(); MAX_MAIN_DECK_ENTRIES],
                ..Default::default()
            })),
        ] {
            let mutation = SeatMutation::SetKind {
                seat_index: 1,
                kind: SeatKind::WaitingHuman,
                expected_kind: Some(SeatKind::Ai {
                    difficulty: AiDifficulty::Medium,
                    deck,
                }),
            };
            assert!(guard_seat_mutation(&mutation).is_ok());
        }
        for deck in [
            DeckChoice::Named("x".repeat(MAX_AI_DECK_NAME_LEN + 1)),
            DeckChoice::DeckList(Box::new(DeckData {
                main_deck: vec!["Forest".to_string(); MAX_MAIN_DECK_ENTRIES + 1],
                ..Default::default()
            })),
        ] {
            let mutation = SeatMutation::SetKind {
                seat_index: 1,
                kind: SeatKind::Ai {
                    difficulty: AiDifficulty::Easy,
                    deck: DeckChoice::Named("Mono Green".to_string()),
                },
                expected_kind: Some(SeatKind::Ai {
                    difficulty: AiDifficulty::Medium,
                    deck,
                }),
            };
            assert!(guard_seat_mutation(&mutation)
                .unwrap_err()
                .contains("mutation.expectedKind.ai.deck"));
        }
    }

    #[test]
    fn seat_mutation_guard_rejects_oversized_seat_index() {
        let mutation = SeatMutation::Remove { seat_index: 200 };
        let err = guard_seat_mutation(&mutation).unwrap_err();
        assert!(err.contains("seat_index"));
    }
}
