use engine::game::engine::{apply, apply_with_rejection};
use engine::game::interaction::submit_interaction_with_rejection;
use engine::game::preview::preview_action_with_rejection;
use engine::game::scenario::{P0, P1};
use engine::game::visibility::filter_action_rejection_for_viewer;
use engine::game::zones::create_object;
use engine::types::action_rejection::{
    ActionRejection, ActionRejectionCode, ActionRejectionDisposition,
};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    InteractionChoiceId, InteractionId, InteractionResponse, InteractionSubmission,
};
use engine::types::zones::Zone;

#[test]
fn rejection_dto_serializes_only_stable_safe_fields() {
    let rejection = ActionRejection {
        code: ActionRejectionCode::InvalidAction,
        disposition: ActionRejectionDisposition::Invalid,
        message: "That action is not valid in the current game state.".to_string(),
        related_object_ids: vec![ObjectId(7)],
    };

    assert_eq!(
        serde_json::to_value(rejection).expect("rejection serializes"),
        serde_json::json!({
            "code": "invalid_action",
            "disposition": "invalid",
            "message": "That action is not valid in the current game state.",
            "related_object_ids": [7],
        })
    );
}

#[test]
fn every_rejection_code_has_a_closed_disposition_and_safe_message() {
    let cases = [
        (
            ActionRejectionCode::InvalidAction,
            ActionRejectionDisposition::Invalid,
        ),
        (
            ActionRejectionCode::WrongPlayer,
            ActionRejectionDisposition::Unauthorized,
        ),
        (
            ActionRejectionCode::NotYourPriority,
            ActionRejectionDisposition::Unavailable,
        ),
        (
            ActionRejectionCode::ActionNotAllowed,
            ActionRejectionDisposition::Unavailable,
        ),
        (
            ActionRejectionCode::InteractionUnavailable,
            ActionRejectionDisposition::Unavailable,
        ),
        (
            ActionRejectionCode::InteractionNotAuthorized,
            ActionRejectionDisposition::Unauthorized,
        ),
        (
            ActionRejectionCode::StaleInteraction,
            ActionRejectionDisposition::Stale,
        ),
        (
            ActionRejectionCode::InvalidInteractionResponse,
            ActionRejectionDisposition::Invalid,
        ),
        (
            ActionRejectionCode::InteractionPayloadTooLarge,
            ActionRejectionDisposition::Invalid,
        ),
        (
            ActionRejectionCode::InteractionConstraintUnsatisfied,
            ActionRejectionDisposition::Invalid,
        ),
        (
            ActionRejectionCode::InteractionCancelOnly,
            ActionRejectionDisposition::Unavailable,
        ),
        (
            ActionRejectionCode::InteractionReducerRejected,
            ActionRejectionDisposition::Invalid,
        ),
        (
            ActionRejectionCode::UnsupportedInteractionResponse,
            ActionRejectionDisposition::Unsupported,
        ),
        (
            ActionRejectionCode::ResolveAllNotReady,
            ActionRejectionDisposition::Unavailable,
        ),
    ];

    for (code, disposition) in cases {
        assert_eq!(code.disposition(), disposition);
        assert!(!code.message().is_empty());
    }
}

#[test]
fn related_object_ids_are_first_seen_and_deduplicated() {
    let action = GameAction::CastSpell {
        object_id: ObjectId(3),
        card_id: CardId(1),
        targets: vec![
            engine::types::TargetRef::Object(ObjectId(9)),
            engine::types::TargetRef::Object(ObjectId(3)),
            engine::types::TargetRef::Object(ObjectId(9)),
        ],
        payment_mode: Default::default(),
    };

    assert_eq!(action.related_object_ids(), vec![ObjectId(3), ObjectId(9)]);
}

#[test]
fn rejection_projection_removes_hidden_object_ids() {
    let mut state = GameState::new_two_player(1);
    let hidden = create_object(
        &mut state,
        CardId(2),
        P1,
        "Private card".to_string(),
        Zone::Hand,
    );
    state
        .players
        .iter_mut()
        .find(|player| player.id == P1)
        .expect("opponent exists")
        .hand
        .push_back(hidden);
    let rejection = ActionRejection {
        code: ActionRejectionCode::InvalidAction,
        disposition: ActionRejectionDisposition::Invalid,
        message: "That action is not valid in the current game state.".to_string(),
        related_object_ids: vec![hidden],
    };

    assert!(filter_action_rejection_for_viewer(&state, P0, &rejection)
        .related_object_ids
        .is_empty());
    assert_eq!(
        filter_action_rejection_for_viewer(&state, P1, &rejection).related_object_ids,
        vec![hidden]
    );
}

#[test]
fn rich_apply_preserves_legacy_rejection_and_state() {
    let action = GameAction::PlayLand {
        object_id: ObjectId(999),
        card_id: CardId(1),
    };
    let mut legacy = GameState::new_two_player(1);
    let mut rich = legacy.clone();

    assert!(apply(&mut legacy, P0, action.clone()).is_err());
    let rejection = apply_with_rejection(&mut rich, P0, action).expect_err("action rejects");

    assert_eq!(rejection.code, ActionRejectionCode::InvalidAction);
    assert_eq!(
        legacy, rich,
        "rich wrapper must preserve legacy mutation semantics"
    );
}

#[test]
fn opaque_interaction_rejection_before_materialization_has_no_object_ids() {
    let mut state = GameState::new_two_player(1);
    let rejection = submit_interaction_with_rejection(
        &mut state,
        P0,
        InteractionSubmission {
            interaction_id: InteractionId("missing".to_string()),
            response: InteractionResponse::Choose {
                choice_id: InteractionChoiceId("choice".to_string()),
            },
        },
    )
    .expect_err("unknown opaque interaction rejects");

    assert!(rejection.related_object_ids.is_empty());
}

#[test]
fn rich_action_preview_does_not_mutate_state() {
    let state = GameState::new_two_player(1);
    let before = state.clone();
    let _ = preview_action_with_rejection(
        &state,
        P0,
        &GameAction::PlayLand {
            object_id: ObjectId(999),
            card_id: CardId(1),
        },
    );

    assert_eq!(state, before);
}
