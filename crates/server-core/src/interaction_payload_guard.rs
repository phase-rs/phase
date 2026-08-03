//! Wire-payload bounds for inbound `InteractionSubmission` bodies on the native
//! WebSocket path.
//!
//! The engine owns these bounds
//! (`engine::game::interaction::bound_interaction_submission`) and re-runs them
//! inside `submit_interaction`. This module exists so
//! `guard_client_message_before_dispatch` can *declare* the variant's wire
//! policy in its exhaustive match, the way every other arm declares one, rather
//! than declaring `Ok(())` — "unbounded at the wire" — for a variant that
//! carries a client-controlled payload.
//!
//! It deliberately restates no limit of its own: the engine owns the bounds, and
//! a second copy here would drift the first time a response variant changes.
//!
//! The rejection string is byte-identical to `engine-wasm`'s
//! (`format!("Engine error: {:?}", code)`) and to
//! `SessionManager::handle_interaction`'s, so the same engine reason code reads
//! the same on the WASM and WebSocket transports and at every server layer that
//! reports one.

use engine::game::interaction::bound_interaction_submission;
use engine::types::interaction::InteractionSubmission;

/// Validate a client-supplied interaction submission before session dispatch.
pub fn guard_interaction_submission_payload(
    submission: &InteractionSubmission,
) -> Result<(), String> {
    bound_interaction_submission(submission)
        .map_err(|error| format!("Engine error: {:?}", error.code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::interaction::MAX_INTERACTION_STRING_LEN;
    use engine::types::interaction::{
        InteractionChoiceId, InteractionId, InteractionReasonCode, InteractionResponse,
        InteractionShortcutDecision, InteractionShortcutPin, MAX_INTERACTION_LIST_LEN,
    };

    fn choice(id: &str) -> InteractionChoiceId {
        InteractionChoiceId(id.to_string())
    }

    fn submission(response: InteractionResponse) -> InteractionSubmission {
        InteractionSubmission {
            interaction_id: InteractionId("interaction-1".to_string()),
            response,
        }
    }

    /// Reach guard for every negative below: the guard is not rejecting
    /// everything it is handed.
    #[test]
    fn accepts_a_realistic_submission() {
        let msg = submission(InteractionResponse::Select {
            choice_ids: vec![choice("a"), choice("b")],
        });

        assert_eq!(guard_interaction_submission_payload(&msg), Ok(()));
    }

    #[test]
    fn rejects_an_oversized_select_list() {
        let msg = submission(InteractionResponse::Select {
            choice_ids: vec![choice("a"); MAX_INTERACTION_LIST_LEN + 1],
        });

        assert!(guard_interaction_submission_payload(&msg).is_err());

        // Boundary sibling: the bound sits at the constant, not at "any large
        // list".
        let at_limit = submission(InteractionResponse::Select {
            choice_ids: vec![choice("a"); MAX_INTERACTION_LIST_LEN],
        });
        assert_eq!(guard_interaction_submission_payload(&at_limit), Ok(()));
    }

    /// The routine-reachable case: a `TextChoiceProjection` with
    /// `allow_arbitrary` accepts free-form text, so an ordinary paste can
    /// exceed the bound. This is why the rejection must not travel on
    /// `ServerMessage::Error`.
    #[test]
    fn rejects_an_oversized_text_value() {
        let msg = submission(InteractionResponse::Text {
            value: "x".repeat(MAX_INTERACTION_STRING_LEN + 1),
        });

        assert!(guard_interaction_submission_payload(&msg).is_err());

        let at_limit = submission(InteractionResponse::Text {
            value: "x".repeat(MAX_INTERACTION_STRING_LEN),
        });
        assert_eq!(guard_interaction_submission_payload(&at_limit), Ok(()));
    }

    #[test]
    fn rejects_an_oversized_interaction_id() {
        let msg = InteractionSubmission {
            interaction_id: InteractionId("x".repeat(MAX_INTERACTION_STRING_LEN + 1)),
            response: InteractionResponse::Choose {
                choice_id: choice("a"),
            },
        };

        assert!(guard_interaction_submission_payload(&msg).is_err());
    }

    /// The `OutboundBudget` cumulative path — the only nested branch in the
    /// engine validator, and precisely what a naive per-field guard would miss:
    /// no single pin is oversized, only their sum.
    #[test]
    fn rejects_an_oversized_nested_shortcut_pin_budget() {
        let per_pin = MAX_INTERACTION_LIST_LEN / 2;
        let pins: Vec<InteractionShortcutPin> = (0..3)
            .map(|group| InteractionShortcutPin {
                group,
                choice_ids: vec![choice("a"); per_pin],
            })
            .collect();

        for pin in &pins {
            assert!(pin.choice_ids.len() <= MAX_INTERACTION_LIST_LEN);
        }

        let msg = submission(InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::AcceptSuggested,
            pins,
        });

        assert!(guard_interaction_submission_payload(&msg).is_err());
    }

    /// Pins the byte-identity claim at this layer so it cannot silently drift
    /// from `engine-wasm/src/lib.rs`'s `format!("Engine error: {:?}", code)`.
    #[test]
    fn rejection_string_matches_the_wasm_transport() {
        let msg = submission(InteractionResponse::Select {
            choice_ids: vec![choice("a"); MAX_INTERACTION_LIST_LEN + 1],
        });

        assert_eq!(
            guard_interaction_submission_payload(&msg),
            Err(format!(
                "Engine error: {:?}",
                InteractionReasonCode::PayloadTooLarge
            ))
        );
    }
}
