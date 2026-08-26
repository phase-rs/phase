//! Phase-1 protocol coverage for explicit Resolve All consent.

use engine::ai_support::{candidate_actions, legal_actions_for_viewer};
use engine::game::elimination::eliminate_player;
use engine::game::engine::{
    apply, pending_resolve_all_ready_requester, recover_orphaned_resolve_all,
    resolve_all_ready_access, resolve_all_ready_prefix, resolve_all_ready_prefix_with,
    ResolveAllContinuation, ResolveAllReadyAccess,
};
use engine::game::game_object::AttachTarget;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::types::ability::{
    ControllerRef, CopyRetargetPermission, Effect, ResolvedAbility, TargetFilter, TargetRef,
    TypedFilter,
};
use engine::types::actions::{GameAction, ResolveAllConsentDecision};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AutoPassMode, GameState, PersistedGameState, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponse, InteractionSessionId,
    InteractionSubmission,
};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

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

fn no_op_entry(id: u64, controller: PlayerId) -> StackEntry {
    StackEntry {
        id: ObjectId(id),
        source_id: ObjectId(id),
        controller,
        kind: StackEntryKind::ActivatedAbility {
            source_id: ObjectId(id),
            ability: Box::new(ResolvedAbility::new(
                Effect::NoOp,
                vec![],
                ObjectId(id),
                controller,
            )),
        },
    }
}

/// Mirrors the live browser failure: P2 has already passed, P0 holds priority,
/// and a fourth seat has been eliminated. The stack item is the same Equip
/// shape (Equipment -> targeted creature) from the captured game, rather than
/// a synthetic spell-only shortcut.
fn browser_partial_priority_equip_state() -> (GameState, ObjectId, ObjectId) {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0x0A11_E0A1);
    state.active_player = P2;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.priority_pass_count = 1;
    state.priority_passes.insert(P2);
    state.players[P3.0 as usize].is_eliminated = true;

    let equipment = create_object(
        &mut state,
        CardId(140),
        P2,
        "Sigiled Sword of Valeron".to_string(),
        Zone::Battlefield,
    );
    let creature = create_object(
        &mut state,
        CardId(289),
        P2,
        "Cold-Eyed Selkie".to_string(),
        Zone::Battlefield,
    );
    {
        let equipment_object = state
            .objects
            .get_mut(&equipment)
            .expect("fixture Equipment exists");
        equipment_object.card_types.core_types = vec![CoreType::Artifact];
        equipment_object.card_types.subtypes = vec!["Equipment".to_string()];
        equipment_object.base_card_types = equipment_object.card_types.clone();
    }
    {
        let creature_object = state
            .objects
            .get_mut(&creature)
            .expect("fixture creature exists");
        creature_object.card_types.core_types = vec![CoreType::Creature];
        creature_object.base_card_types = creature_object.card_types.clone();
    }
    state.stack.push_back(StackEntry {
        id: ObjectId(461),
        source_id: equipment,
        controller: P2,
        kind: StackEntryKind::ActivatedAbility {
            source_id: equipment,
            ability: Box::new(ResolvedAbility::new(
                Effect::Attach {
                    attachment: TargetFilter::SelfRef,
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                },
                vec![TargetRef::Object(creature)],
                equipment,
                P2,
            )),
        },
    });
    (state, equipment, creature)
}

#[test]
fn browser_partial_priority_equip_grants_then_resolves_at_the_public_batch_seam() {
    let (mut state, equipment, creature) = browser_partial_priority_equip_state();

    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 100,
        },
    )
    .expect("the human priority holder starts the browser Resolve All flow");
    let epoch = match state.waiting_for {
        WaitingFor::ResolveAllConsent {
            epoch,
            representative: P1,
        } => epoch,
        ref waiting_for => panic!("P1 should consent first, got {waiting_for:?}"),
    };
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the first AI grants consent");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllConsent {
            epoch: next_epoch,
            representative: P2,
        } if next_epoch == epoch
    ));
    apply(
        &mut state,
        P2,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the final AI grants consent");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { epoch: ready_epoch } if ready_epoch == epoch
    ));

    let result = resolve_all_ready_prefix(&mut state, P2);

    assert_eq!(
        result.items_resolved, 1,
        "a granted browser Resolve All must not return to manual priority with the Equip still on the stack"
    );
    assert!(state.stack.is_empty());
    assert_eq!(
        state.objects[&equipment].attached_to,
        Some(AttachTarget::Object(creature)),
        "the resolved Equip attaches to its already-selected creature target"
    );
}

#[test]
fn browser_partial_priority_equip_keeps_the_requesters_no_manual_resolution_intent_when_a_pending_event_blocks_batch_proof(
) {
    let (mut state, equipment, creature) = browser_partial_priority_equip_state();
    // This is the latent combat-damage event carrier present in the live game.
    // It makes the proof checkpoint intentionally fail closed, but it must not
    // erase the human request to continue through ordinary engine auto-pass.
    state.pending_trigger_event_batch = vec![GameEvent::DamageDealt {
        source_id: ObjectId(398),
        target: TargetRef::Player(P0),
        amount: 4,
        is_combat: true,
        excess: 0,
    }];

    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 100,
        },
    )
    .expect("the human priority holder starts Resolve All");
    let epoch = match state.waiting_for {
        WaitingFor::ResolveAllConsent {
            epoch,
            representative: P1,
        } => epoch,
        ref waiting_for => panic!("P1 should consent first, got {waiting_for:?}"),
    };
    for representative in [P1, P2] {
        apply(
            &mut state,
            representative,
            GameAction::RespondResolveAllConsent {
                epoch,
                decision: ResolveAllConsentDecision::Grant,
            },
        )
        .expect("each AI representative grants the live Resolve All prompt");
    }
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { epoch: ready_epoch } if ready_epoch == epoch
    ));

    let result = resolve_all_ready_prefix(&mut state, P2);

    assert_eq!(
        result.items_resolved, 0,
        "the conservative batch proof must not consume an unsettled checkpoint"
    );
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P1 },
    ));
    assert_eq!(
        state.auto_pass.get(&P0),
        Some(&AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
        }),
        "the failed proof becomes the requester's ordinary standing auto-pass"
    );
    apply(&mut state, P1, GameAction::PassPriority)
        .expect("the next AI's ordinary pass continues the requester's auto-pass");
    assert!(
        state.stack.is_empty(),
        "the Equip resolves without another manual P0 action"
    );
    assert_eq!(
        state.objects[&equipment].attached_to,
        Some(AttachTarget::Object(creature)),
    );
    assert!(state.auto_pass.is_empty());
}

#[test]
fn restored_mid_stack_priority_discards_an_orphaned_trigger_event_carrier() {
    let (mut state, _, _) = browser_partial_priority_equip_state();
    state.pending_trigger_event_batch = vec![GameEvent::DamageDealt {
        source_id: ObjectId(398),
        target: TargetRef::Player(P0),
        amount: 4,
        is_combat: true,
        excess: 0,
    }];
    assert!(state.pending_trigger.is_none());

    let persisted = PersistedGameState::capture(state);
    let encoded = serde_json::to_string(&persisted).expect("mid-stack state serializes");
    let persisted: PersistedGameState =
        serde_json::from_str(&encoded).expect("mid-stack state deserializes");
    let restored = persisted.into_game_state();

    assert!(
        restored.pending_trigger_event_batch.is_empty(),
        "a saved orphan carrier is not active stack work and must not poison Resolve All after reload"
    );
    assert!(restored.pending_trigger.is_none());
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
fn stale_epoch_and_decline_continue_through_the_requesters_engine_auto_pass() {
    let mut state = GameState::new_two_player(43);
    state.stack.push_back(no_op_entry(1, P0));
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
    let decline = apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("queued representative may decline");

    assert!(
        state.stack.is_empty(),
        "the two-player auto-pass resolves the stack immediately"
    );
    assert!(decline.events.iter().any(|event| matches!(
        event,
        engine::types::events::GameEvent::EffectResolved { .. }
    )));
    assert!(state.auto_pass.is_empty());
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
fn persisted_pending_consent_keeps_the_initiators_auto_pass_fallback() {
    let mut state = GameState::new_two_player(430);
    state.stack.push_back(no_op_entry(1, P0));
    let epoch = begin(&mut state);

    let persisted = PersistedGameState::capture(state);
    let encoded = serde_json::to_string(&persisted).expect("pending consent serializes");
    let persisted: PersistedGameState =
        serde_json::from_str(&encoded).expect("pending consent deserializes");
    let mut restored = persisted.into_game_state();
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::ResolveAllConsent { epoch: restored_epoch, representative } if restored_epoch == epoch && representative == P1
    ));

    let decline = apply(
        &mut restored,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("restored responder may decline");
    assert!(
        decline.events.iter().any(|event| matches!(
            event,
            engine::types::events::GameEvent::EffectResolved { .. }
        )),
        "declining after a save resumes the normal auto-pass pipeline"
    );
    assert!(restored.stack.is_empty());
    assert!(restored.auto_pass.is_empty());
}

#[test]
fn restored_mid_stack_priority_can_start_a_new_resolve_all_consent_run() {
    let mut state = GameState::new_two_player(431);
    state.stack.push_back(no_op_entry(1, P0));
    let persisted = PersistedGameState::capture(state);
    let encoded = serde_json::to_string(&persisted).expect("mid-stack priority serializes");
    let persisted: PersistedGameState =
        serde_json::from_str(&encoded).expect("mid-stack priority deserializes");
    let mut restored = persisted.into_game_state();

    let epoch = begin(&mut restored);
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::ResolveAllConsent { epoch: restored_epoch, representative } if restored_epoch == epoch && representative == P1
    ));
}

#[test]
fn decline_auto_pass_is_owned_by_the_semantic_priority_seat_under_turn_control() {
    let mut state = GameState::new_two_player(432);
    state.active_player = P0;
    state.turn_decision_controller = Some(P1);
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.stack.push_back(no_op_entry(1, P0));

    apply(
        &mut state,
        P1,
        GameAction::BeginResolveAll { max_resolutions: 7 },
    )
    .expect("the controller may begin Resolve All for the controlled priority seat");
    let epoch = match state.waiting_for {
        WaitingFor::ResolveAllConsent {
            epoch,
            representative,
        } => {
            assert_eq!(representative, P1);
            epoch
        }
        ref waiting_for => panic!("expected queued consent, got {waiting_for:?}"),
    };

    let decline = apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("the responder may decline");

    assert!(
        state.stack.is_empty(),
        "the semantic priority seat P0 was passed immediately; using submitter P1 would leave the stack intact"
    );
    assert!(decline.events.iter().any(|event| matches!(
        event,
        engine::types::events::GameEvent::EffectResolved { .. }
    )));
    assert!(state.auto_pass.is_empty());
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
    assert!(state.auto_pass.is_empty());
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
    assert!(state.auto_pass.is_empty());
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
    assert_eq!(
        state.auto_pass.get(&P0),
        Some(&AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
        }),
        "a partial proof keeps the original requester as the durable auto-pass owner"
    );

    let WaitingFor::Priority { player } = state.waiting_for else {
        panic!("the unproved entry must return to the ordinary priority pipeline");
    };
    assert_ne!(
        player, P0,
        "the requester has already passed; another seat now owns the live priority window"
    );
    apply(&mut state, player, GameAction::PassPriority)
        .expect("the ordinary priority action resumes the requester's stored auto-pass");
    assert!(
        state.auto_pass.is_empty(),
        "a stack-growing resolution interrupts UntilStackEmpty instead of inheriting stale consent"
    );
}
/// Drives a two-seat run to unanimous consent and returns the latched state.
fn ready_two_seat_state() -> GameState {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE0);
    state.stack.push_back(no_op_entry(1, P1));
    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll { max_resolutions: 1 },
    )
    .expect("priority holder may begin Resolve All consent");
    let WaitingFor::ResolveAllConsent { epoch, .. } = state.waiting_for else {
        panic!(
            "expected a queued consent prompt, got {:?}",
            state.waiting_for
        );
    };
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the remaining representative may grant");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
    state
}

/// The gate answers ONE question — entitlement — and coherence is answered
/// elsewhere, by the resolver. This pins that separation from both sides: the
/// gate's verdict does not move when coherence changes, and
/// `pending_resolve_all_ready_requester` is what moves instead.
#[test]
fn ready_access_refuses_an_unentitled_seat_and_admits_an_incoherent_run() {
    let mut state = ready_two_seat_state();
    assert_eq!(
        resolve_all_ready_access(&state, P0),
        ResolveAllReadyAccess::Admitted
    );
    assert_eq!(
        pending_resolve_all_ready_requester(&state),
        Some(P0),
        "the run's own first participant is the frozen requester"
    );

    // P0 is a seat at this two-player table; P2 is not, which is exactly the
    // shape a forged or stale wire request takes: an id the run never froze.
    assert_eq!(
        resolve_all_ready_access(&state, P2),
        ResolveAllReadyAccess::Refused
    );

    // An installed auto-pass makes the frozen priority snapshot no longer
    // describe the live game. Entitlement is untouched by that — P0 is still a
    // frozen submitter — so the gate must not move.
    state.auto_pass.insert(
        P1,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
        },
    );
    assert_eq!(
        resolve_all_ready_access(&state, P0),
        ResolveAllReadyAccess::Admitted,
        "coherence is not this gate's axis; refusing here would strand the latch"
    );
    assert_eq!(
        pending_resolve_all_ready_requester(&state),
        None,
        "an incoherent run authorizes no unattended consumption"
    );

    // With no run at all there is no frozen submitter list, so there is nobody
    // to check anyone against — including a seat that never was a participant.
    // Refusing here is what would make the latch permanent.
    state.resolve_all_consent_run = None;
    assert_eq!(
        resolve_all_ready_access(&state, P2),
        ResolveAllReadyAccess::Admitted,
        "a run-less latch has no owner to prove; the repair is its only exit"
    );
}

/// A Ready latch has no acting player, and once its run is gone it has no
/// Revoke either — `append_resolve_all_revocations` enumerates grantors from
/// the run — so a run-less latch leaves the game with no exit whatsoever. The
/// resolver must repair it rather than refuse, and resolve nothing doing so.
#[test]
fn a_ready_latch_with_no_run_repairs_to_priority_without_resolving() {
    let mut state = ready_two_seat_state();
    state.resolve_all_consent_run = None;
    assert!(
        state.waiting_for.acting_player().is_none(),
        "the fixture must reproduce the no-actor property that makes this fatal"
    );
    assert_eq!(
        resolve_all_ready_access(&state, P0),
        ResolveAllReadyAccess::Admitted,
        "no seat can prove ownership of a run-less latch, so none may be refused"
    );

    let result = resolve_all_ready_prefix(&mut state, P0);

    assert_eq!(result.items_resolved, 0, "a repair resolves nothing");
    assert_eq!(state.stack.len(), 1, "the stack entry survives the repair");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the repair must restore ordinary priority, got {:?}",
        state.waiting_for
    );
    assert!(state.auto_pass.is_empty());
}
/// Recovery is the single obligation every restore seam owes, so it must be
/// inert on the overwhelming majority of states, which carry no latch at all.
#[test]
fn recovery_is_inert_on_a_state_carrying_no_latch() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE1);
    state.stack.push_back(no_op_entry(1, P1));
    let before = state.waiting_for.clone();

    assert!(
        recover_orphaned_resolve_all(&mut state).is_none(),
        "no latch means nothing to discharge"
    );
    assert_eq!(state.waiting_for, before, "an inert call mutates nothing");
    assert_eq!(state.stack.len(), 1);
}

/// A snapshot written while an intact unanimous run was outstanding is
/// discharged rather than repaired: the players consented to this prefix
/// before the snapshot existed, and the consent was frozen with it.
#[test]
fn recovery_discharges_an_intact_latch() {
    let mut state = ready_two_seat_state();
    assert_eq!(
        pending_resolve_all_ready_requester(&state),
        Some(P0),
        "non-vacuity: the fixture must present a consumable latch"
    );

    let batch =
        recover_orphaned_resolve_all(&mut state).expect("an intact latch must be discharged");

    assert_eq!(batch.items_resolved, 1, "the consented prefix is collapsed");
    assert!(state.stack.is_empty(), "the stack entry resolved");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "discharging hands priority back, got {:?}",
        state.waiting_for
    );
}

/// Recovery must leave the interaction layer describing the state it produced,
/// not the one it repaired away.
///
/// A snapshot taken while the latch was live carries one Revoke slot per
/// grantor (see `ready_state_transport_materializes_each_grantors_frozen_revoke`),
/// and every restore seam binds interaction authority BEFORE recovery runs.
/// Repairing `waiting_for` alone would leave each grantor holding an
/// affordance for a prompt that no longer exists, which
/// `debug_assert_interaction_consistency` treats as a defect.
#[test]
fn recovery_of_an_incoherent_latch_re_derives_the_ready_era_slots() {
    let mut state = ready_two_seat_state();
    // The run is present and epoch-matching — so the slots below really are the
    // Ready set — but the frozen priority snapshot no longer describes the live
    // game, which is what makes the latch unconsumable.
    state.auto_pass.insert(
        P1,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
        },
    );
    bind_interaction_authority(
        &mut state,
        InteractionSessionId("resolve-all-restore".to_string()),
    )
    .expect("Ready binds one slot per frozen grantor");
    let ready_era_slots = state.active_interaction_slots.clone();
    assert!(
        !ready_era_slots.is_empty(),
        "non-vacuity: the fixture must actually carry Ready-era slots"
    );

    let batch =
        recover_orphaned_resolve_all(&mut state).expect("a latch is present, so recovery acts");

    assert_eq!(
        batch.items_resolved, 0,
        "an incoherent run resolves nothing"
    );
    assert_eq!(state.stack.len(), 1, "the stack entry survives the repair");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "recovery must restore ordinary priority, got {:?}",
        state.waiting_for
    );
    assert_ne!(
        state.active_interaction_slots, ready_era_slots,
        "the Ready-era Revoke slots must not outlive the prompt they belong to"
    );
}
/// A stack the prefix proof cannot finish, granted and ready. `CopySpell` grows
/// the stack when it resolves, which is exactly what stops the proof — see
/// `ready_consent_collapses_the_safe_prefix_before_a_stack_growing_resolution`.
fn proof_stopping_ready_state() -> GameState {
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
    state
}

/// The two continuations must actually differ, and only on the remainder.
///
/// A live session installs `UntilStackEmpty` so the requester's standing intent
/// survives a proof that stopped short. A restore must not: that auto-pass
/// resolves the rest of the stack through the ordinary pipeline, which can end
/// the game — and a restore has no socket attached and no caller positioned to
/// emit a ranked result or a terminal artifact, so the game would be registered
/// live while parked in `GameOver`.
#[test]
fn the_restore_continuation_installs_no_auto_pass_where_the_live_one_does() {
    let mut live = proof_stopping_ready_state();
    let live_batch =
        resolve_all_ready_prefix_with(&mut live, P0, ResolveAllContinuation::AutoPassRemainder);
    assert!(
        !live.auto_pass.is_empty(),
        "non-vacuity: this fixture must reach the proof-stopped fallback at all"
    );

    let mut restored = proof_stopping_ready_state();
    let restored_batch =
        resolve_all_ready_prefix_with(&mut restored, P0, ResolveAllContinuation::StopAtPriority);

    assert!(
        restored.auto_pass.is_empty(),
        "a restore must hand priority back, not run the remainder unattended"
    );
    assert!(
        matches!(restored.waiting_for, WaitingFor::Priority { .. }),
        "the restore continuation still yields an actionable state, got {:?}",
        restored.waiting_for
    );
    assert_eq!(
        restored_batch.items_resolved, 2,
        "the consented prefix is collapsed either way; only the remainder differs"
    );
    assert!(
        live_batch.items_resolved >= restored_batch.items_resolved,
        "the live continuation may resolve more, never less"
    );
    assert!(
        restored.resolve_all_consent_run.is_none(),
        "the consent is discarded on both paths"
    );
}
/// Revocation is per-grantor at the ENGINE boundary, not merely in what the
/// transport offers.
///
/// `transport_surfaces_only_each_grantors_own_revoke_and_uses_exact_consent_choices`
/// pins the surface — which actions a viewer is handed. It does not pin what
/// `apply` accepts, and those are different contracts: a forged or replayed
/// wire frame never passes through the transport's action list. The engine's
/// authorization for this action is a per-TARGET check
/// (`resolve_all_granted_submitter(state, epoch, representative) ==
/// Some(actor)`), which no set-membership test over authorized submitters can
/// express — a set says "you may act here", never "you may act on THIS
/// representative's consent".
#[test]
fn a_grantor_may_revoke_only_its_own_consent_at_the_engine_boundary() {
    let mut state = ready_two_seat_state();
    let WaitingFor::ResolveAllReady { epoch } = state.waiting_for else {
        panic!("fixture must be latched, got {:?}", state.waiting_for);
    };

    // Positive control first, so the negative below cannot pass because the
    // action is simply unroutable at Ready: P1's own revoke is accepted.
    let mut own = state.clone();
    apply(
        &mut own,
        P1,
        GameAction::RevokeResolveAllConsent {
            epoch,
            representative: P1,
        },
    )
    .expect("a grantor may withdraw its own consent while the latch stands");

    // The contract: P1 may not withdraw P0's consent, even though P1 is itself
    // a frozen submitter of this very run.
    assert!(
        apply(
            &mut state,
            P1,
            GameAction::RevokeResolveAllConsent {
                epoch,
                representative: P0,
            },
        )
        .is_err(),
        "one grantor must not be able to revoke another's consent"
    );
}
