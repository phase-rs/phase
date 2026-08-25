use engine::analysis::decision_template::{
    AnnouncementSubject, DecisionGroupKey, DecisionKind, DecisionSlot, DecisionTemplate,
    IterationCount, MayChoiceOption, PinnedDecision, Ranking, ReplayMode, TargetPin,
    TargetSchedule, UnlessPaymentOption,
};
use engine::game::combat::AttackTarget;
use engine::game::engine::{
    apply, apply_with_rejection, preflight_debug_action, preflight_debug_action_with_rejection,
    resolve_all_ready_prefix_with_rejection,
};
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, preview_interaction,
    preview_interaction_with_rejection, submit_interaction_with_rejection,
};
use engine::game::preview::{
    preview_action_with_rejection, preview_auto_payment_sources,
    preview_auto_payment_sources_with_rejection,
};
use engine::game::scenario::{P0, P1};
use engine::game::visibility::filter_action_rejection_for_viewer;
use engine::game::zones::create_object;
use engine::types::action_rejection::{
    ActionRejection, ActionRejectionCode, ActionRejectionDisposition,
};
use engine::types::actions::{DebugAction, GameAction, MayTriggerAutoChoiceOp, PriorityYieldOp};
use engine::types::game_state::{
    GameState, MayTriggerAutoChoiceSelector, MayTriggerOrigin, WaitingFor, YieldTarget,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    InteractionAvailability, InteractionChoiceId, InteractionId, InteractionPreviewRequest,
    InteractionResponse, InteractionSessionId, InteractionSubmission, PreviewRequestId,
};
use engine::types::mana::ManaColor;
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
        targets: vec![ObjectId(9), ObjectId(3), ObjectId(9)],
        payment_mode: Default::default(),
    };

    assert_eq!(action.related_object_ids(), vec![ObjectId(3), ObjectId(9)]);
}

#[test]
fn related_object_ids_cover_battle_and_nested_source_carriers() {
    let battle = GameAction::DeclareAttackers {
        attacks: vec![(ObjectId(1), AttackTarget::Battle(ObjectId(2)))],
        bands: vec![],
    };
    assert_eq!(battle.related_object_ids(), vec![ObjectId(1), ObjectId(2)]);

    let yield_remove = GameAction::SetPriorityYield {
        op: PriorityYieldOp::Remove {
            target: YieldTarget::ThisObject {
                source_id: ObjectId(3),
                incarnation: Some(1),
                trigger_description: None,
            },
        },
    };
    assert_eq!(yield_remove.related_object_ids(), vec![ObjectId(3)]);

    let may_remove = GameAction::SetMayTriggerAutoChoice {
        op: MayTriggerAutoChoiceOp::Remove {
            selector: MayTriggerAutoChoiceSelector::ExactInstance {
                player: P0,
                source_id: ObjectId(4),
                origin: MayTriggerOrigin::Printed { trigger_index: 0 },
            },
        },
    };
    assert_eq!(may_remove.related_object_ids(), vec![ObjectId(4)]);

    let source = |object_id| YieldTarget::ThisObject {
        source_id: Object_id,
        incarnation: Some(1),
        trigger_description: None,
    };
    let slot = DecisionSlot {
        source: source(ObjectId(6)),
        index: 0,
    };
    let template = DecisionTemplate {
        owner: P0,
        decisions: vec![
            PinnedDecision::Order {
                source: source(ObjectId(6)),
                pos: 0,
            },
            PinnedDecision::Targets {
                slot,
                targets: vec![
                    TargetPin::ByIdentity(source(ObjectId(7))),
                    TargetPin::Scheduled(TargetSchedule::Constant(Ranking::one(
                        AnnouncementSubject::Object(source(ObjectId(8))),
                    ))),
                    TargetPin::Scheduled(TargetSchedule::RoundRobin(vec![Ranking::one(
                        AnnouncementSubject::Object(source(ObjectId(9))),
                    )])),
                    TargetPin::Scheduled(TargetSchedule::Piecewise(vec![(
                        0,
                        Ranking::one(AnnouncementSubject::Object(source(ObjectId(10)))),
                    )])),
                ],
            },
            PinnedDecision::Mode {
                slot: DecisionSlot {
                    source: source(ObjectId(11)),
                    index: 0,
                },
                indices: vec![0],
            },
            PinnedDecision::MayChoice {
                slot: DecisionSlot {
                    source: source(ObjectId(12)),
                    index: 0,
                },
                take: MayChoiceOption::Take,
            },
            PinnedDecision::UnlessBreak {
                slot: DecisionSlot {
                    source: source(ObjectId(13)),
                    index: 0,
                },
                pay: UnlessPaymentOption::Pay,
            },
            PinnedDecision::ConvokeTaps {
                slot: DecisionSlot {
                    source: source(ObjectId(14)),
                    index: 0,
                },
            },
            PinnedDecision::ManaColor {
                slot: DecisionSlot {
                    source: source(ObjectId(15)),
                    index: 0,
                },
                color: ManaColor::Blue,
            },
        ],
        replay: ReplayMode::Static,
        key: DecisionGroupKey::from_sources(&[source(ObjectId(5))], DecisionKind::LoopChoice),
    };
    let shortcut = GameAction::DeclareShortcut {
        count: IterationCount::Fixed(1),
        template: Some(template),
    };
    assert_eq!(
        shortcut.related_object_ids(),
        vec![
            ObjectId(5),
            ObjectId(6),
            ObjectId(7),
            ObjectId(8),
            ObjectId(9),
            ObjectId(10),
            ObjectId(11),
            ObjectId(12),
            ObjectId(13),
            ObjectId(14),
            ObjectId(15),
        ]
    );
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

#[test]
fn rich_auto_payment_preview_preserves_legacy_noop_behavior() {
    let state = GameState::new_two_player(1);
    let before = state.clone();
    let action = GameAction::PassPriority;

    assert_eq!(
        preview_auto_payment_sources_with_rejection(&state, P0, &action)
            .expect("non-cast preview is an empty legacy result"),
        preview_auto_payment_sources(&state, P0, &action)
            .expect("legacy non-cast preview is empty")
    );
    assert_eq!(state, before);
}

#[test]
fn rich_interaction_preview_matches_legacy_after_materialization() {
    let mut state = GameState::new_two_player(1);
    bind_interaction_authority(
        &mut state,
        InteractionSessionId("action-rejection".to_string()),
    )
    .expect("interaction authority binds");
    let filtered = engine::game::visibility::filter_state_for_viewer(&state, P0);
    let view = derive_viewer_interaction(&state, &filtered, P0);
    let InteractionAvailability::ProgressAvailable { witness } = view.availability else {
        panic!("priority interaction has a materializable progress witness");
    };
    let request = InteractionPreviewRequest {
        request_id: PreviewRequestId("preview".to_string()),
        interaction_id: witness.interaction_id,
        response: witness.response,
    };
    let before = state.clone();

    let legacy = preview_interaction(&state, P0, &request);
    let rich = preview_interaction_with_rejection(&state, P0, &request)
        .expect("materialized priority response previews");

    assert_eq!(rich, legacy);
    assert_eq!(state, before);
}

#[test]
fn rich_debug_preflight_is_safe_and_does_not_mutate_state() {
    let mut state = GameState::new_two_player(1);
    let object_id = create_object(
        &mut state,
        CardId(3),
        P0,
        "Visible debug object".to_string(),
        Zone::Battlefield,
    );
    let before = state.clone();
    let action = DebugAction::RemoveObject { object_id };

    assert!(preflight_debug_action(&state, P0, &action).is_err());
    let rejection = preflight_debug_action_with_rejection(&state, P0, &action)
        .expect_err("disabled debug mode rejects the action");

    assert_eq!(rejection.code, ActionRejectionCode::InvalidAction);
    assert_eq!(
        rejection.message,
        "That action is not valid in the current game state."
    );
    assert_eq!(rejection.related_object_ids, vec![object_id]);
    assert_eq!(state, before);
}

#[test]
fn rich_resolve_all_rejection_is_safe_and_does_not_mutate_state() {
    let mut state = GameState::new_two_player(1);
    let before = state.clone();

    let rejection = resolve_all_ready_prefix_with_rejection(&mut state, P0)
        .expect_err("a non-Ready state cannot run Resolve All");

    assert_eq!(rejection.code, ActionRejectionCode::ResolveAllNotReady);
    assert_eq!(rejection.message, "Resolve All is not ready to run.");
    assert!(rejection.related_object_ids.is_empty());
    assert_eq!(state, before);
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
}
