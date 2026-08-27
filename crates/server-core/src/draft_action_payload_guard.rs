//! Payload bounds for `DraftAction` bodies on the native WebSocket path.
//!
//! `draft_wire_guard::guard_draft_action` only validates `draft_code`. Oversized
//! pick IDs, submit-deck lists, and match IDs still reach clone-heavy draft
//! reducers unless bounded here.

use std::collections::HashSet;

use draft_core::types::{DraftAction, MAX_CARDS_PER_PICK, MAX_COMMANDER_DESIGNATIONS};
use lobby_broker::inbound_guard::{validate_deck_list, MAX_MAIN_DECK_ENTRIES};
use lobby_broker::validation::{
    validate_required_label, validate_token, MAX_DISPLAY_NAME_LEN, MAX_TOKEN_LEN,
};

/// Validate client-supplied `DraftAction` payload fields before session dispatch.
pub fn guard_draft_action_payload(action: &DraftAction) -> Result<(), String> {
    match action {
        // CR 903.13b: a pick step carries `cards_per_pick` ids. This function
        // receives only the action and can never consult the session, so the
        // EXACT count is checked in `draft_core::pick_pass::apply_pick_inner`;
        // what is bounded here is what an action alone can state.
        DraftAction::Pick {
            card_instance_ids, ..
        } => {
            if card_instance_ids.is_empty() {
                return Err("Pick requires at least one card ID".to_string());
            }
            if card_instance_ids.len() > MAX_CARDS_PER_PICK {
                return Err(format!(
                    "Pick accepts at most {MAX_CARDS_PER_PICK} card IDs"
                ));
            }
            // Pairwise-distinct over the WHOLE Vec. NOT the index form used by
            // `PickWithDraftEffect` below: that is sound only under its
            // `len() != 2` early return, and a `Pick` is legitimately length 1.
            let mut seen = HashSet::with_capacity(card_instance_ids.len());
            for card_instance_id in card_instance_ids {
                if !seen.insert(card_instance_id) {
                    return Err("Pick requires distinct card IDs".to_string());
                }
                validate_token("Pick.card_instance_id", card_instance_id, MAX_TOKEN_LEN)?;
            }
        }
        DraftAction::PickWithDraftEffect {
            effect_card_instance_id,
            card_instance_ids,
            ..
        } => {
            validate_token(
                "PickWithDraftEffect.effect_card_instance_id",
                effect_card_instance_id,
                MAX_TOKEN_LEN,
            )?;
            if card_instance_ids.len() != 2 {
                return Err("PickWithDraftEffect requires exactly two card IDs".to_string());
            }
            if card_instance_ids[0] == card_instance_ids[1] {
                return Err("PickWithDraftEffect requires distinct card IDs".to_string());
            }
            for card_instance_id in card_instance_ids {
                validate_token(
                    "PickWithDraftEffect.card_instance_id",
                    card_instance_id,
                    MAX_TOKEN_LEN,
                )?;
            }
        }
        DraftAction::SubmitDeck {
            main_deck,
            commanders,
            ..
        } => {
            validate_deck_list("SubmitDeck.main_deck", main_deck, MAX_MAIN_DECK_ENTRIES)?;
            // CR 702.124g: a client-controlled, otherwise unbounded
            // `Vec<String>` reaching the clone-heavy reducer is exactly the
            // hole this module exists to close. Bounded by draft-core's
            // CR-derived constant, imported rather than duplicated -- the same
            // shape `MAX_CARDS_PER_PICK` already uses above.
            //
            // NOT `lobby_broker::inbound_guard::MAX_COMMANDER_ENTRIES`: that is
            // a larger TRANSPORT bound on the lobby's own `DeckData.commander`
            // list, and reaching for it here would silently admit a third
            // commander.
            validate_deck_list(
                "SubmitDeck.commanders",
                commanders,
                MAX_COMMANDER_DESIGNATIONS,
            )?;
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
        | DraftAction::GeneratePairings
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
            card_instance_ids: vec!["card-0".to_string()],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn pick_rejects_oversized_instance_id() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["x".repeat(MAX_TOKEN_LEN + 1)],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("card_instance_id"));
    }

    /// CR 903.13b: a Commander Draft pick carries two ids, and the guard must
    /// admit them.
    #[test]
    fn pick_accepts_two_distinct_card_ids() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["card-0".to_string(), "card-1".to_string()],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn pick_rejects_empty_card_ids() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: Vec::new(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("at least one"));
    }

    #[test]
    fn pick_rejects_more_than_max_cards_per_pick() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: (0..=MAX_CARDS_PER_PICK)
                .map(|i| format!("card-{i}"))
                .collect(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("at most"));
    }

    /// The distinctness check must be pairwise over the whole `Vec`, not the
    /// index form `PickWithDraftEffect` uses — that form is sound only under
    /// its own `len() != 2` precondition and would panic on the length-1
    /// `Pick` that `pick_accepts_valid_instance_id` covers.
    #[test]
    fn pick_rejects_duplicate_card_ids() {
        let action = DraftAction::Pick {
            seat: 0,
            card_instance_ids: vec!["card-0".to_string(), "card-0".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("distinct"));
    }

    #[test]
    fn draft_effect_pick_accepts_two_distinct_card_ids() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string(), "card-2".to_string()],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
    }

    #[test]
    fn draft_effect_pick_rejects_wrong_card_count() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("exactly two"));
    }

    #[test]
    fn draft_effect_pick_rejects_duplicate_card_ids() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string(), "card-1".to_string()],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("distinct"));
    }

    #[test]
    fn draft_effect_pick_rejects_oversized_card_id() {
        let action = DraftAction::PickWithDraftEffect {
            seat: 0,
            effect_card_instance_id: "cogwork-1".to_string(),
            card_instance_ids: vec!["card-1".to_string(), "x".repeat(MAX_TOKEN_LEN + 1)],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("card_instance_id"));
    }

    #[test]
    fn submit_deck_rejects_oversized_main() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string(); MAX_MAIN_DECK_ENTRIES + 1],
            commanders: Vec::new(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("main_deck"));
    }

    #[test]
    fn submit_deck_rejects_invalid_card_name() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest\nIsland".to_string()],
            commanders: Vec::new(),
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(err.contains("control characters"));
    }

    /// CR 702.124g: the WIRE bound on the designation list.
    ///
    /// Written off `MAX_COMMANDER_DESIGNATIONS`, never off the literal 3, so
    /// the assertion tracks the constant rather than a number that happens to
    /// exceed it today. Keys on the FIELD NAME rather than the full sentence,
    /// so rewording `validate_deck_list`'s `format!` does not break it.
    ///
    /// The bound that must NOT be reached for here is
    /// `lobby_broker::inbound_guard::MAX_COMMANDER_ENTRIES` — a larger
    /// TRANSPORT bound on the lobby's own commander list, already imported into
    /// this file's sibling module. Under it a three-commander payload is
    /// ACCEPTED and this test passes vacuously; the two constants having
    /// different values is exactly what makes this discriminating.
    #[test]
    fn submit_deck_rejects_more_than_max_commander_designations() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string()],
            commanders: vec!["Forest".to_string(); MAX_COMMANDER_DESIGNATIONS + 1],
        };
        let err = guard_draft_action_payload(&action).unwrap_err();
        assert!(
            err.contains("SubmitDeck.commanders"),
            "expected the commanders field to be named, got {err}"
        );
    }

    /// Paired positive reach-guard for the bound above: exactly
    /// `MAX_COMMANDER_DESIGNATIONS` names is accepted, so the rejection cannot
    /// pass by `SubmitDeck` being rejected for some unrelated reason.
    #[test]
    fn submit_deck_accepts_max_commander_designations() {
        let action = DraftAction::SubmitDeck {
            seat: 0,
            main_deck: vec!["Forest".to_string()],
            commanders: vec!["Forest".to_string(); MAX_COMMANDER_DESIGNATIONS],
        };
        assert!(guard_draft_action_payload(&action).is_ok());
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
