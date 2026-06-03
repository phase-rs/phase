//! Payload bounds for `DraftAction` bodies on the native WebSocket path.
//!
//! `draft_wire_guard::guard_draft_action` only validates `draft_code`. Oversized
//! pick IDs, submit-deck lists, and match IDs still reach clone-heavy draft
//! reducers unless bounded here.

use draft_core::types::DraftAction;
use lobby_broker::inbound_guard::{validate_deck_list, MAX_MAIN_DECK_ENTRIES};
use lobby_broker::validation::{
    validate_required_label, validate_token, MAX_DISPLAY_NAME_LEN, MAX_TOKEN_LEN,
};

/// Validate client-supplied `DraftAction` payload fields before session dispatch.
pub fn guard_draft_action_payload(action: &DraftAction) -> Result<(), String> {
    match action {
        DraftAction::Pick {
            card_instance_id, ..
        } => {
            validate_token("Pick.card_instance_id", card_instance_id, MAX_TOKEN_LEN)?;
        }
        DraftAction::SubmitDeck { main_deck, .. } => {
            validate_deck_list("SubmitDeck.main_deck", main_deck, MAX_MAIN_DECK_ENTRIES)?;
        }
        DraftAction::ReportMatchResult { match_id, .. } => {
            validate_token("ReportMatchResult.match_id", match_id, MAX_TOKEN_LEN)?;
        }
        DraftAction::ReplaceSeatWithBot { name, .. } => {
            if let Some(n) = name {
                if !n.trim().is_empty() {
                    validate_required_label("ReplaceSeatWithBot.name", n, MAX_DISPLAY_NAME_LEN)?;
                }
            }
        }
        DraftAction::StartDraft
        | DraftAction::AdvanceRound
        | DraftAction::GeneratePairings { .. }
        | DraftAction::SetSeatConnected { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::inbound_guard::MAX_MAIN_DECK_ENTRIES;

    #[test]
    fn pick_accepts_valid_instance_id() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_id: "card-0".to_string(),
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn pick_rejects_oversized_instance_id() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_id: "x".repeat(MAX_TOKEN_LEN + 1),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("card_instance_id"));
    }

    #[test]
    fn submit_deck_rejects_oversized_main() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string(); MAX_MAIN_DECK_ENTRIES + 1],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("main_deck"));
    }

    #[test]
    fn submit_deck_rejects_invalid_card_name() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest\nIsland".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("control characters"));
    }

    #[test]
    fn replace_seat_with_bot_rejects_oversized_name() {
        let action = DraftAction::ReplaceSeatWithBot {
            seat: 0,
            name: Some("x".repeat(MAX_DISPLAY_NAME_LEN + 1)),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("ReplaceSeatWithBot.name"));
    }
}
