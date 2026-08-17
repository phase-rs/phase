//! Phase-1 protocol coverage for explicit Resolve All consent.

use engine::ai_support::{candidate_actions, legal_actions_for_viewer};
use engine::game::elimination::eliminate_player;
use engine::game::engine::{apply, resolve_all_ready_prefix};
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{CopyRetargetPermission, Effect, ResolvedAbility, TargetFilter};
use engine::types::actions::{GameAction, ResolveAllConsentDecision};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponse, InteractionSessionId,
    InteractionSubmission,
};
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

fn begin(state: &mut GameState) -> u64 {
    apply(
        state,
        P0,
        GameAction::BeginResolveAll { max_resolutions: 7 },
    )
    .expect("priority holder may begin Resolve All consent");
    match &state.waiting_for {
        WaitingFor::ResolveAllConsent {
            epoch,
            representative,
        } => {
            assert_eq!(
                *representative, P1,
                "initiator grants before the queue opens"
            );
            *epoch
        }
        ref other => panic!("expected queued consent, got {other:?}"),
    }
}

#[test]
fn consent_queue_reaches_inert_ready_only_after_every_representative_grants() {
    let mut state = GameState::new_two_player(42);
    let epoch = begin(&mut state);

    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("queued representative may grant");
    assert!(matches!(
        &state.waiting_for,
        WaitingFor::ResolveAllReady { epoch: ready_epoch } if *ready_epoch == epoch
    ));
    assert_eq!(
        state.priority_player, P0,
        "Ready preserves the saved priority cursor"
    );
    assert!(apply(&mut state, P1, GameAction::PassPriority).is_err());
    assert!(matches!(
        &state.waiting_for,
        WaitingFor::ResolveAllReady { epoch: ready_epoch } if *ready_epoch == epoch
    ));
}

#[test]
fn stale_epoch_and_decline_restore_the_exact_priority_snapshot() {
    let mut state = GameState::new_two_player(43);
    state.priority_pass_count = 3;
    state.priority_passes.insert(P0);
    let epoch = begin(&mut state);

    assert!(apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch: epoch + 1,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .is_err());
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("queued representative may decline");

    assert!(matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0));
    assert_eq!(state.priority_player, P0);
    assert_eq!(state.priority_pass_count, 3);
    assert!(state.priority_passes.contains(&P0));
    assert!(state.resolve_all_consent_run.is_none());
    assert!(apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .is_err());
}

#[test]
fn eliminating_a_consent_representative_drops_the_run_and_restores_living_priority() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 44);
    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll { max_resolutions: 7 },
    )
    .expect("priority holder may begin Resolve All consent");
    assert!(matches!(
        &state.waiting_for,
        WaitingFor::ResolveAllConsent {
            representative: P1,
            ..
        }
    ));
    state.priority_pass_count = 2;
    state.priority_passes.insert(P0);

    eliminate_player(&mut state, P1, &mut Vec::new());

    assert!(state.players[P1.0 as usize].is_eliminated);
    assert!(state.resolve_all_consent_run.is_none());
    assert!(matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0));
    assert_eq!(state.priority_player, P0);
    assert_eq!(state.priority_pass_count, 0);
    assert!(state.priority_passes.is_empty());
    assert!(!state.players[P2.0 as usize].is_eliminated);
}

#[test]
fn queued_response_and_candidate_keep_the_frozen_submitter_after_control_changes() {
    let mut state = GameState::new_two_player(44);
    let epoch = begin(&mut state);
    state.active_player = P1;
    state.turn_decision_controller = Some(P0);

    let candidates = candidate_actions(&state);
    assert!(candidates.iter().any(|candidate| {
        matches!(
            candidate.action,
            GameAction::RespondResolveAllConsent {
                epoch: candidate_epoch,
                decision: ResolveAllConsentDecision::Grant,
            } if candidate_epoch == epoch
        ) && candidate.metadata.actor == Some(P1)
    }));
    assert!(apply(
        &mut state,
        P0,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .is_err());
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("frozen submitter, not the new live controller, answers the prompt");
    assert!(apply(
        &mut state,
        P0,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .is_err());
}

#[test]
fn rotated_three_player_consent_reaches_the_ready_prefix() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 49);
    let entry = StackEntry {
        id: ObjectId(1),
        source_id: ObjectId(1),
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: ObjectId(1),
            ability: Box::new(ResolvedAbility::new(Effect::NoOp, vec![], ObjectId(1), P0)),
        },
    };
    state.stack.push_back(entry);
    let epoch = begin(&mut state);

    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("first queued representative grants");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllConsent { representative, .. } if representative == P2
    ));
    apply(
        &mut state,
        P2,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("second queued representative grants");

    let result = resolve_all_ready_prefix(&mut state, P0);
    assert_eq!(result.items_resolved, 1);
    assert!(state.stack.is_empty());
}

#[test]
fn granted_representative_can_revoke_off_queue_and_private_run_is_not_visible() {
    let mut state = GameState::new_two_player(45);
    let epoch = begin(&mut state);
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("reach ready state");

    let view = filter_state_for_viewer(&state, P1);
    assert!(matches!(&view.waiting_for, WaitingFor::ResolveAllReady { epoch: e } if *e == epoch));
    assert!(view.resolve_all_consent_run.is_none());

    let candidates = candidate_actions(&state);
    assert!(candidates.iter().any(|candidate| {
        matches!(
            candidate.action,
            GameAction::RevokeResolveAllConsent {
                epoch: candidate_epoch,
                representative: P0,
            } if candidate_epoch == epoch
        ) && candidate.metadata.actor == Some(P0)
    }));
    apply(
        &mut state,
        P0,
        GameAction::RevokeResolveAllConsent {
            epoch,
            representative: P0,
        },
    )
    .expect("a granted representative may revoke from Ready");
    assert!(matches!(&state.waiting_for, WaitingFor::Priority { player } if *player == P0));
    assert!(state.resolve_all_consent_run.is_none());
}

#[test]
fn transport_surfaces_only_each_grantors_own_revoke_and_uses_exact_consent_choices() {
    let mut state = GameState::new_two_player(46);
    let epoch = begin(&mut state);

    let p0_actions = legal_actions_for_viewer(&state, P0).0;
    assert_eq!(
        p0_actions,
        vec![GameAction::RevokeResolveAllConsent {
            epoch,
            representative: P0,
        }],
        "an off-prompt grantor receives only its own frozen revoke"
    );
    let p1_actions = legal_actions_for_viewer(&state, P1).0;
    assert!(p1_actions
        .iter()
        .all(|action| { !matches!(action, GameAction::RevokeResolveAllConsent { .. }) }));
    assert!(p1_actions.iter().any(|action| {
        matches!(
            action,
            GameAction::RespondResolveAllConsent {
                epoch: action_epoch,
                decision: ResolveAllConsentDecision::Grant,
            } if *action_epoch == epoch
        )
    }));

    bind_interaction_authority(&mut state, InteractionSessionId("resolve-all".to_string()))
        .expect("consent slots bind for each authorized owner");
    let p0_view = derive_viewer_interaction(&state, &filter_state_for_viewer(&state, P0), P0);
    let p1_view = derive_viewer_interaction(&state, &filter_state_for_viewer(&state, P1), P1);
    assert!(p0_view.can_submit);
    assert!(p1_view.can_submit);
    assert_eq!(p0_view.opportunities.len(), 1);
    assert_eq!(p1_view.opportunities.len(), 1);

    let InteractionOpportunityResponse::ExactChoices { choices } =
        &p0_view.opportunities[0].response
    else {
        panic!("off-prompt revoke must use an exact choice, not the CR 732 reply schema");
    };
    let choice_id = choices
        .first()
        .expect("grantor has one revoke choice")
        .id
        .clone();
    let action = resolve_interaction_response(
        &state,
        P0,
        &InteractionSubmission {
            interaction_id: p0_view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Choose { choice_id },
        },
    )
    .expect("transport may materialize the off-prompt revoke");
    assert_eq!(
        action,
        GameAction::RevokeResolveAllConsent {
            epoch,
            representative: P0,
        }
    );

    let InteractionOpportunityResponse::ExactChoices { choices } =
        &p1_view.opportunities[0].response
    else {
        panic!("queued consent must use bounded exact grant/decline choices");
    };
    assert_eq!(choices.len(), 2);
}

#[test]
fn ready_state_transport_materializes_each_grantors_frozen_revoke() {
    let mut state = GameState::new_two_player(47);
    let epoch = begin(&mut state);
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the final grant reaches Ready");

    bind_interaction_authority(
        &mut state,
        InteractionSessionId("resolve-all-ready".to_string()),
    )
    .expect("Ready binds one slot per frozen grantor");
    let p0_view = derive_viewer_interaction(&state, &filter_state_for_viewer(&state, P0), P0);
    assert_eq!(p0_view.opportunities.len(), 1);
    let InteractionOpportunityResponse::ExactChoices { choices } =
        &p0_view.opportunities[0].response
    else {
        panic!("Ready revoke must remain an exact choice");
    };
    assert_eq!(choices.len(), 1);
    let action = resolve_interaction_response(
        &state,
        P0,
        &InteractionSubmission {
            interaction_id: p0_view.opportunities[0].interaction_id.clone(),
            response: InteractionResponse::Choose {
                choice_id: choices[0].id.clone(),
            },
        },
    )
    .expect("Ready has no acting player, but its frozen grantor may still revoke");
    assert_eq!(
        action,
        GameAction::RevokeResolveAllConsent {
            epoch,
            representative: P0,
        }
    );
}

#[test]
fn ready_consent_collapses_the_safe_prefix_before_a_stack_growing_resolution() {
    let entry = |id, effect| StackEntry {
        id: ObjectId(id),
        source_id: ObjectId(id),
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: ObjectId(id),
            ability: Box::new(ResolvedAbility::new(effect, vec![], ObjectId(id), P0)),
        },
    };
    let mut state = GameState::new_two_player(48);
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.priority_player = P0;
    state.stack = vec![
        entry(
            1,
            Effect::CopySpell {
                target: TargetFilter::SelfRef,
                retarget: CopyRetargetPermission::KeepOriginalTargets,
                copier: None,
                additional_modifications: vec![],
                starting_loyalty_from_casualty_sacrifice: false,
            },
        ),
        entry(2, Effect::NoOp),
        entry(3, Effect::NoOp),
    ]
    .into_iter()
    .collect();
    let epoch = begin(&mut state);
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("second representative grants");

    let result = resolve_all_ready_prefix(&mut state, P0);

    assert_eq!(
        result.items_resolved,
        2,
        "safe-prefix result={result:?}, waiting={:?}, stack_len={}",
        state.waiting_for,
        state.stack.len(),
    );
    assert_eq!(state.stack.len(), 1, "the stack-growing item remains live");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}
