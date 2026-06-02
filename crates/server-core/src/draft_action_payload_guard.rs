//! Payload bounds for `DraftAction` bodies on the native WebSocket path.
//!
//! `draft_wire_guard::guard_draft_action` only validates `draft_code`. Oversized
//! pick IDs, submit-deck lists, and match IDs still reach clone-heavy draft
//! reducers unless bounded here.

use draft_core::types::DraftAction;
use lobby_broker::inbound_guard::{MAX_DECK_CARD_NAME_LEN, MAX_MAIN_DECK_ENTRIES};
use lobby_broker::validation::{
    validate_optional_label, validate_token, MAX_DISPLAY_NAME_LEN, MAX_TOKEN_LEN,
};

fn has_control_char(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

fn validate_card_name(field: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if name.len() > MAX_DECK_CARD_NAME_LEN {
        return Err(format!(
            "{field} must be at most {MAX_DECK_CARD_NAME_LEN} bytes"
        ));
    }
    if has_control_char(name) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

fn validate_main_deck_list(main_deck: &[String]) -> Result<(), String> {
    if main_deck.len() > MAX_MAIN_DECK_ENTRIES {
        return Err(format!(
            "SubmitDeck.main_deck must contain at most {MAX_MAIN_DECK_ENTRIES} entries"
        ));
    }
    for (index, name) in main_deck.iter().enumerate() {
        validate_card_name(&format!("SubmitDeck.main_deck[{index}]"), name)?;
    }
    Ok(())
}

/// Validate client-supplied `DraftAction` payload fields before session dispatch.
pub fn guard_draft_action_payload(action: &DraftAction) -> Result<(), String> {
    match action {
        DraftAction::Pick {
            card_instance_id, ..
        } => {
            validate_token("Pick.card_instance_id", card_instance_id, MAX_TOKEN_LEN)?;
        }
        DraftAction::SubmitDeck { main_deck, .. } => validate_main_deck_list(main_deck)?,
        DraftAction::ReportMatchResult { match_id, .. } => {
            validate_token("ReportMatchResult.match_id", match_id, MAX_TOKEN_LEN)?;
        }
        DraftAction::ReplaceSeatWithBot { name, .. } => {
            validate_optional_label("ReplaceSeatWithBot.name", name, MAX_DISPLAY_NAME_LEN)?;
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
}
