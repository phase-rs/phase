//! Phase-1 protocol coverage for explicit Resolve All consent.

use std::collections::{BTreeMap, BTreeSet};

use engine::ai_support::{
    candidate_actions, legal_actions_for_viewer, stuck_decision_diagnostic, AiDecisionContract,
};
use engine::game::elimination::eliminate_player;
use engine::game::engine::{
    apply, apply_verified_ai_priority_pass, classify_restored_stack_automation,
    pending_resolve_all_ready_requester, resolve_all_ready_access, resolve_all_ready_prefix,
    resolve_all_ready_prefix_with, resume_restored_stack_automation, ResolveAllContinuation,
    ResolveAllReadyAccess, RestoredStackAutomation, RestoredStackAutomationOutcome,
};
use engine::game::game_object::AttachTarget;
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, resolve_interaction_response,
};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::create_object;
use engine::types::ability::{
    ChoiceType, ControllerRef, CopyRetargetPermission, Effect, ResolvedAbility, TargetFilter,
    TargetRef, TargetSelectionMode, TypedFilter,
};
use engine::types::actions::{GameAction, ResolveAllConsentDecision, ResolveAllScope};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AutoPassMode, GameState, PersistedGameState, PriorityPassingMode, StackEntry, StackEntryKind,
    StackResolutionAutoPassOverlay, StackResolutionBudget, StackResolutionEntryFence,
    StackResolutionPolicy, StackResolutionSession, TurnBoundary, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponse, InteractionSessionId,
    InteractionSubmission,
};
use engine::types::phase::{Phase, PhaseStop, PhaseStopScope};
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedInformationAudience, ResolvedInformationEdit, ResolvedRulesCommand,
};
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn begin(state: &mut GameState) -> u64 {
    apply(
        state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 7,
            scope: ResolveAllScope::Shared,
        },
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
    ability_entry(id, controller, Effect::NoOp, vec![])
}

fn ability_entry(
    id: u64,
    controller: PlayerId,
    effect: Effect,
    targets: Vec<TargetRef>,
) -> StackEntry {
    StackEntry {
        id: ObjectId(id),
        source_id: ObjectId(id),
        controller,
        kind: StackEntryKind::ActivatedAbility {
            source_id: ObjectId(id),
            ability: Box::new(ResolvedAbility::new(
                effect,
                targets,
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
            scope: ResolveAllScope::Shared,
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
    let result = apply(
        &mut state,
        P2,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the final AI grants consent");
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        1,
        "a granted browser Resolve All must use the ordinary runner and resolve the Equip"
    );
    assert!(state.stack.is_empty());
    assert!(state.stack_resolution_session.is_none());
    assert!(state.resolve_all_consent_run.is_none());
    assert_eq!(
        state.objects[&equipment].attached_to,
        Some(AttachTarget::Object(creature)),
        "the resolved Equip attaches to its already-selected creature target"
    );
}

#[test]
fn browser_partial_priority_equip_uses_the_shared_session_instead_of_a_prefix_proof() {
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
            scope: ResolveAllScope::Shared,
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
    assert!(
        !matches!(state.waiting_for, WaitingFor::ResolveAllReady { .. }),
        "fresh consent never hands off to the legacy Ready-prefix resolver"
    );
    assert!(
        state.stack.is_empty(),
        "the ordinary session runner resolves the Equip without another manual P0 action"
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
    let restored = persisted
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");

    assert!(
        restored.pending_trigger_event_batch.is_empty(),
        "a saved orphan carrier is not active stack work and must not poison Resolve All after reload"
    );
    assert!(restored.pending_trigger.is_none());
}

/// CR 117.3d: `ResolveAllScope::Own` binds the requester alone, so a table with
/// AI seats can never park the requester on a consent prompt nobody answers.
#[test]
fn own_scope_resolves_without_asking_anyone() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 0x5E55_1017);
    state.active_player = P2;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.stack.push_back(no_op_entry(1, P2));
    state.stack.push_back(no_op_entry(2, P2));

    let result = apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 1,
            scope: ResolveAllScope::Own,
        },
    )
    .expect("the priority holder begins Resolve All without anyone else's consent");

    assert!(
        !matches!(
            state.waiting_for,
            WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. }
        ),
        "Own scope raises no consent prompt, got {:?}",
        state.waiting_for
    );
    assert!(state.resolve_all_consent_run.is_none());
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        1,
        "the requester's own session resolves under its cap with no second party"
    );
    assert_eq!(state.stack.len(), 1);
    for viewer in [P0, P1, P2] {
        assert!(
            !legal_actions_for_viewer(&state, viewer)
                .0
                .iter()
                .any(|action| matches!(
                    action,
                    GameAction::RespondResolveAllConsent { .. }
                        | GameAction::RevokeResolveAllConsent { .. }
                )),
            "{viewer:?} must not be offered a consent response"
        );
    }
}

/// CR 117.1: Full Control is a standing refusal to give up any window, and it
/// must survive a session ANOTHER player installed — those passes are taken
/// inside `run_auto_pass_loop` and never reach a frontend, so a client-only
/// toggle could not have stopped them.
#[test]
fn full_control_holds_a_window_against_another_players_resolve_all() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 0xFC0_0DED);
    state.active_player = P2;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.stack.push_back(no_op_entry(1, P2));
    state.stack.push_back(no_op_entry(2, P2));
    // P1 has nothing meaningful to do, so ONLY Full Control can hold their
    // window — the meaningful-action gate would let the session pass them.
    state
        .priority_passing_modes
        .insert(P1, PriorityPassingMode::FullControl);

    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 0,
            scope: ResolveAllScope::Own,
        },
    )
    .expect("P0 begins their own Resolve All");

    assert!(
        !state.stack.is_empty(),
        "P1's Full Control must stop the session before it drains the stack"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P1),
        "the window belongs to the Full Control player, got {:?}",
        state.waiting_for
    );
}

#[test]
fn final_grant_materializes_the_ordinary_session_without_a_ready_latch() {
    let mut state = GameState::new_two_player(42);
    state.stack.push_back(no_op_entry(1, P0));
    let epoch = begin(&mut state);

    let result = apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("queued representative may grant");
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
    assert!(state.stack.is_empty());
    assert!(state.stack_resolution_session.is_none());
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
}

#[test]
fn final_grant_transfers_the_requested_cap_zero_as_unlimited() {
    let run = |max_resolutions| {
        let mut state = GameState::new_two_player(0xCA9);
        state.stack.push_back(no_op_entry(1, P0));
        state.stack.push_back(no_op_entry(2, P0));
        let epoch = apply(
            &mut state,
            P0,
            GameAction::BeginResolveAll {
                max_resolutions,
                scope: ResolveAllScope::Shared,
            },
        )
        .expect("priority holder begins")
        .waiting_for;
        let WaitingFor::ResolveAllConsent {
            epoch,
            representative: P1,
        } = epoch
        else {
            panic!("two-seat Begin must queue P1")
        };
        let result = apply(
            &mut state,
            P1,
            GameAction::RespondResolveAllConsent {
                epoch,
                decision: ResolveAllConsentDecision::Grant,
            },
        )
        .expect("final representative grants");
        (state, result)
    };

    let (capped, capped_result) = run(1);
    assert_eq!(
        capped_result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        1
    );
    assert_eq!(capped.stack.len(), 1);
    assert!(matches!(capped.waiting_for, WaitingFor::Priority { .. }));

    let (unlimited, unlimited_result) = run(0);
    assert_eq!(
        unlimited_result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count(),
        2,
        "zero keeps StackResolutionBudget's legacy unlimited meaning"
    );
    assert!(unlimited.stack.is_empty());
    assert!(!matches!(
        unlimited.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
}

#[test]
fn final_grant_restores_the_baseline_when_a_materialized_copy_changes_the_next_top_fence() {
    let mut state = GameState::new_two_player(0xF3EC3);
    state.stack.push_back(no_op_entry(1, P0));
    state.stack.push_back(ability_entry(
        2,
        P0,
        Effect::CopySpell {
            target: TargetFilter::SelfRef,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: vec![],
            starting_loyalty_from_casualty_sacrifice: false,
        },
        vec![],
    ));
    state.stack.push_back(no_op_entry(3, P0));
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: Default::default(),
        },
    );
    let baseline = state.auto_pass.clone();
    let epoch = begin(&mut state);

    let result = apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("CopySpell changes the next top only after final Grant materializes the session");

    assert!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::EffectResolved { .. }))
            .count()
            >= 2,
        "the normal runner resolves the captured top and CopySpell before its copy breaks the next fence"
    );
    assert!(
        state.stack.iter().any(|entry| entry.id == ObjectId(1)),
        "the lower captured entry remains once CopySpell's fresh top no longer matches its fence"
    );
    assert_eq!(state.auto_pass, baseline);
    assert!(state.stack_resolution_session.is_none());
    assert!(state.resolve_all_consent_run.is_none());
}

#[test]
fn final_grant_prompt_restores_the_preconsent_overlay_before_waiting_for_choice() {
    let mut state = GameState::new_two_player(0xC401CE);
    state.stack.push_back(ability_entry(
        1,
        P0,
        Effect::Choose {
            choice_type: ChoiceType::BasicLandType,
            persist: false,
            selection: TargetSelectionMode::Chosen,
        },
        vec![],
    ));
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: Default::default(),
        },
    );
    let baseline = state.auto_pass.clone();
    let epoch = begin(&mut state);

    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the final grant resolves through the normal interactive choice path");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::NamedChoice { player: P0, .. }
    ));
    assert_eq!(state.auto_pass, baseline);
    assert!(state.stack_resolution_session.is_none());
    assert!(state.resolve_all_consent_run.is_none());
}

#[test]
fn final_grant_terminal_resolution_restores_a_survivors_preconsent_overlay() {
    let mut state = GameState::new_two_player(0x007E_21A1);
    state.stack.push_back(ability_entry(
        1,
        P0,
        Effect::LoseTheGame { target: None },
        vec![],
    ));
    state.auto_pass.insert(
        P1,
        AutoPassMode::UntilTurnBoundary {
            until: Default::default(),
        },
    );
    let survivor_mode = state.auto_pass.get(&P1).copied();
    let epoch = begin(&mut state);

    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the final grant resolves the terminal entry through the shared session");

    assert!(matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    assert_eq!(state.auto_pass.get(&P1).copied(), survivor_mode);
    assert!(state.stack_resolution_session.is_none());
    assert!(state.resolve_all_consent_run.is_none());
}

#[test]
fn pending_non_auto_pass_preference_does_not_mutate_the_consent_baseline() {
    let mut state = GameState::new_two_player(0xA9EF);
    state.stack.push_back(no_op_entry(1, P0));
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    let expected_auto_pass = state.auto_pass.clone();
    let baseline: BTreeMap<_, _> = state
        .auto_pass
        .iter()
        .map(|(&player, &mode)| (player, mode))
        .collect();
    let epoch = begin(&mut state);
    let stops = vec![PhaseStop {
        phase: Phase::End,
        scope: PhaseStopScope::AllTurns,
    }];

    apply(
        &mut state,
        P1,
        GameAction::SetPhaseStops {
            stops: stops.clone(),
        },
    )
    .expect("an actor-scoped phase-stop preference remains legal during consent");

    assert_eq!(state.auto_pass, expected_auto_pass);
    assert_eq!(
        state
            .resolve_all_consent_run
            .as_ref()
            .expect("fresh consent is still pending")
            .auto_pass_baseline
            .as_ref(),
        Some(&baseline)
    );
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("the queued representative may decline");

    assert_eq!(state.auto_pass, expected_auto_pass);
    assert_eq!(state.phase_stops.get(&P1), Some(&stops));
}

#[test]
fn pending_consent_preserves_modes_and_cancellation_is_not_resurrected_on_revoke() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 0x0C0A_5E17);
    state.stack.push_back(no_op_entry(1, P0));
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    state.auto_pass.insert(
        P2,
        AutoPassMode::UntilTurnBoundary {
            until: Default::default(),
        },
    );
    let expected_before_cancel = state.auto_pass.clone();
    let epoch = begin(&mut state);
    assert_eq!(
        state.auto_pass, expected_before_cancel,
        "Begin is non-destructive"
    );

    apply(&mut state, P0, GameAction::CancelAutoPass)
        .expect("a grantor may cancel its pending preference");
    assert!(!state.auto_pass.contains_key(&P0));
    apply(
        &mut state,
        P0,
        GameAction::RevokeResolveAllConsent {
            epoch,
            representative: P0,
        },
    )
    .expect("the original grantor may revoke while consent is pending");

    assert!(!state.auto_pass.contains_key(&P0));
    assert_eq!(
        state.auto_pass.get(&P2),
        expected_before_cancel.get(&P2),
        "a cancellation changes only its canonical representative's baseline entry"
    );
    assert!(state.resolve_all_consent_run.is_none());
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn begin_resolve_all_refuses_to_replace_an_existing_resolution_session() {
    let mut state = GameState::new_two_player(0x005E_5510_u64);
    state.stack.push_back(no_op_entry(1, P0));
    let fence =
        StackResolutionEntryFence::capture(state.stack.back().expect("fixture stack entry exists"));
    state.stack_resolution_session = Some(StackResolutionSession {
        entries: vec![fence],
        cursor: 0,
        representatives: BTreeSet::from([P0, P1]),
        verified_pass_representatives: BTreeSet::new(),
        budget: StackResolutionBudget::Unlimited,
        policy: StackResolutionPolicy::Committed,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: state
                .auto_pass
                .iter()
                .map(|(&player, &mode)| (player, mode))
                .collect(),
        },
    });
    let before = state.clone();

    assert!(apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 1,
            scope: ResolveAllScope::Shared
        },
    )
    .is_err());
    assert_eq!(
        state, before,
        "a rejected Begin leaves the active session untouched"
    );
}

#[test]
fn stale_epoch_and_decline_restore_the_exact_preconsent_priority_checkpoint() {
    let mut state = GameState::new_two_player(43);
    state.stack.push_back(no_op_entry(1, P0));
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    let expected_auto_pass = state.auto_pass.clone();
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

    assert_eq!(state.stack.len(), 1, "decline resolves no stack entry");
    assert_eq!(state.auto_pass, expected_auto_pass);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
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
fn decline_preserves_the_requesters_retained_end_of_turn_auto_pass() {
    let mut state = GameState::new_two_player(44);
    state.stack.push_back(no_op_entry(1, P1));
    state.auto_pass.insert(
        P0,
        AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        },
    );
    let epoch = begin(&mut state);

    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("a decline restores the retained end-of-turn preference");

    assert_eq!(
        state.auto_pass.get(&P0),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        }),
        "declining Resolve All must not overwrite a live end-of-turn preference"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "the opponent's stack object still pauses it"
    );
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn persisted_pending_consent_decline_restores_the_captured_baseline() {
    let mut state = GameState::new_two_player(430);
    state.stack.push_back(no_op_entry(1, P0));
    let epoch = begin(&mut state);

    let persisted = PersistedGameState::capture(state);
    let encoded = serde_json::to_string(&persisted).expect("pending consent serializes");
    let persisted: PersistedGameState =
        serde_json::from_str(&encoded).expect("pending consent deserializes");
    let mut restored = persisted
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::ResolveAllConsent { epoch: restored_epoch, representative } if restored_epoch == epoch && representative == P1
    ));

    apply(
        &mut restored,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Decline,
        },
    )
    .expect("restored responder may decline");
    assert_eq!(restored.stack.len(), 1);
    assert!(restored.auto_pass.is_empty());
}

/// CR 117.4: a save written before `ResolveAllScope` existed carries no `scope`
/// key at all, and `#[serde(default)]` must resolve that to `Own` -- the scope
/// that binds only the requester. Defaulting a legacy run to `Shared` would
/// silently hand it table-wide authority over seats that never consented, which
/// is precisely what the field's default exists to prevent.
///
/// `begin` opens a SHARED run, so the captured payload really does carry a
/// `scope`. That matters twice over: removing the key is what reconstructs a
/// genuine pre-migration payload, and starting from `Shared` is what makes the
/// assertion discriminating -- the default has to FLIP the value, not merely
/// reproduce it. The `remove` is asserted because a fixture that never carried
/// a `scope` would otherwise pass this test while proving nothing.
#[test]
fn a_pre_scope_save_restores_as_an_own_run() {
    let mut state = GameState::new_two_player(431);
    state.stack.push_back(no_op_entry(1, P0));
    begin(&mut state);

    let encoded = serde_json::to_string(&PersistedGameState::capture(state))
        .expect("pending consent serializes");
    let mut doc: serde_json::Value =
        serde_json::from_str(&encoded).expect("the captured payload is valid JSON");
    // A persisted payload is either a trusted envelope wrapping `state` or a
    // bare raw state. `reject_legacy_raw_prompt_authority` reads it the same way.
    let root = if doc.get("state").is_some() {
        doc.get_mut("state").expect("just observed")
    } else {
        &mut doc
    };
    let run = root
        .get_mut("resolve_all_consent_run")
        .and_then(serde_json::Value::as_object_mut)
        .expect("the captured payload carries the consent run");
    assert!(
        run.remove("scope").is_some(),
        "the fixture must carry a scope for its removal to reconstruct a legacy save"
    );

    let persisted: PersistedGameState =
        serde_json::from_str(&doc.to_string()).expect("a pre-scope save still deserializes");
    let restored = persisted
        .into_game_state()
        .expect("a pre-scope save satisfies the checked restore contract");
    assert_eq!(
        restored
            .resolve_all_consent_run
            .as_ref()
            .expect("the restored snapshot keeps its consent run")
            .scope,
        ResolveAllScope::Own,
        "a legacy run must not acquire table-wide authority"
    );
}

#[test]
fn serialized_legacy_pending_grant_removes_its_mode_before_entering_ready() {
    let mut state = GameState::new_two_player(0x001E_6AC0);
    state.stack.push_back(no_op_entry(1, P0));
    state.auto_pass.insert(
        P1,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    let epoch = begin(&mut state);
    state
        .resolve_all_consent_run
        .as_mut()
        .expect("fresh fixture has a pending run")
        .auto_pass_baseline = None;
    let encoded = serde_json::to_string(&PersistedGameState::capture(state))
        .expect("legacy pending run serializes without a baseline");
    let mut restored = serde_json::from_str::<PersistedGameState>(&encoded)
        .expect("legacy pending run deserializes")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    assert!(restored
        .resolve_all_consent_run
        .as_ref()
        .expect("restored pending run exists")
        .auto_pass_baseline
        .is_none());
    assert!(restored.auto_pass.contains_key(&P1));

    apply(
        &mut restored,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("legacy final grant reaches its Ready reader");

    assert!(!restored.auto_pass.contains_key(&P1));
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
    let result = resolve_all_ready_prefix(&mut restored, P0);
    assert_eq!(result.items_resolved, 1);
    assert!(restored.stack.is_empty());
}

#[test]
fn restored_mid_stack_priority_can_start_a_new_resolve_all_consent_run() {
    let mut state = GameState::new_two_player(431);
    state.stack.push_back(no_op_entry(1, P0));
    let persisted = PersistedGameState::capture(state);
    let encoded = serde_json::to_string(&persisted).expect("mid-stack priority serializes");
    let persisted: PersistedGameState =
        serde_json::from_str(&encoded).expect("mid-stack priority deserializes");
    let mut restored = persisted
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");

    let epoch = begin(&mut restored);
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::ResolveAllConsent { epoch: restored_epoch, representative } if restored_epoch == epoch && representative == P1
    ));
}

#[test]
fn decline_under_turn_control_restores_the_semantic_priority_snapshot() {
    let mut state = GameState::new_two_player(432);
    state.active_player = P0;
    state.turn_decision_controller = Some(P1);
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.stack.push_back(no_op_entry(1, P0));

    apply(
        &mut state,
        P1,
        GameAction::BeginResolveAll {
            max_resolutions: 7,
            scope: ResolveAllScope::Shared,
        },
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

    assert_eq!(
        state.stack.len(),
        1,
        "declining does not resolve a stack entry"
    );
    assert!(decline.events.is_empty());
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(state.auto_pass.is_empty());
}

#[test]
fn eliminating_a_consent_representative_drops_the_run_and_restores_living_priority() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 44);
    state.auto_pass.insert(
        P1,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    state.auto_pass.insert(
        P2,
        AutoPassMode::UntilTurnBoundary {
            until: Default::default(),
        },
    );
    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 7,
            scope: ResolveAllScope::Shared,
        },
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
    assert!(
        !state.auto_pass.contains_key(&P1),
        "the leaving seat's restored baseline is pruned by ordinary elimination cleanup"
    );
    assert!(
        state.auto_pass.contains_key(&P2),
        "the survivor's pre-consent preference survives projected recovery"
    );
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
    assert!(state.resolve_all_consent_run.is_none());
    assert!(state.stack_resolution_session.is_none());
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
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
fn rotated_three_player_consent_runs_through_the_shared_session() {
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
    let result = apply(
        &mut state,
        P2,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("second queued representative grants");
    assert!(result
        .events
        .iter()
        .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
    assert!(state.stack.is_empty());
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
}

#[test]
fn granted_representative_can_revoke_off_queue_and_private_run_is_not_visible() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 45);
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

    let view = filter_state_for_viewer(&state, P1);
    assert!(
        matches!(&view.waiting_for, WaitingFor::ResolveAllConsent { epoch: e, representative: P2 } if *e == epoch)
    );
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
    .expect("a granted representative may revoke while another representative is queued");
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
    let mut state = ready_two_seat_state();
    let WaitingFor::ResolveAllReady { epoch } = state.waiting_for else {
        panic!("legacy fixture reaches Ready");
    };

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
    state
        .resolve_all_consent_run
        .as_mut()
        .expect("pending run exists")
        .auto_pass_baseline = None;
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("second representative grants");

    let result =
        resolve_all_ready_prefix_with(&mut state, P0, ResolveAllContinuation::StopAtPriority);

    assert_eq!(
        result.items_resolved,
        2,
        "safe-prefix result={result:?}, waiting={:?}, stack_len={}",
        state.waiting_for,
        state.stack.len(),
    );
    assert_eq!(state.stack.len(), 1, "the stack-growing item remains live");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(
        state.auto_pass.is_empty(),
        "the bounded proof continuation must not install an ordinary auto-pass session"
    );
}
/// Builds an old persisted two-seat consent run that still uses the legacy
/// Ready reader. Fresh Begin/Grant states deliberately retain `Some(...)` and
/// never reach this helper's latch.
fn ready_two_seat_state() -> GameState {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE0);
    state.stack.push_back(no_op_entry(1, P1));
    apply(
        &mut state,
        P0,
        GameAction::BeginResolveAll {
            max_resolutions: 1,
            scope: ResolveAllScope::Shared,
        },
    )
    .expect("priority holder may begin Resolve All consent");
    let WaitingFor::ResolveAllConsent { epoch, .. } = state.waiting_for else {
        panic!(
            "expected a queued consent prompt, got {:?}",
            state.waiting_for
        );
    };
    state
        .resolve_all_consent_run
        .as_mut()
        .expect("fresh pending run exists")
        .auto_pass_baseline = None;
    apply(
        &mut state,
        P1,
        GameAction::RespondResolveAllConsent {
            epoch,
            decision: ResolveAllConsentDecision::Grant,
        },
    )
    .expect("the remaining representative may grant on the legacy wire path");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllReady { .. }
    ));
    state
}

fn restored_session_state(max_resolutions: u32) -> GameState {
    let mut state = GameState::new_two_player(0xA11CE);
    state.stack.push_back(no_op_entry(1, P0));
    state.stack.push_back(no_op_entry(2, P1));
    let baseline: BTreeMap<_, _> = BTreeSet::from([P0])
        .into_iter()
        .map(|player| {
            (
                player,
                AutoPassMode::UntilTurnBoundary {
                    until: Default::default(),
                },
            )
        })
        .collect();
    let representatives = BTreeSet::from([P0, P1]);
    let entries = state
        .stack
        .iter()
        .rev()
        .map(StackResolutionEntryFence::capture)
        .collect();
    state.stack_resolution_session = Some(StackResolutionSession {
        entries,
        cursor: 0,
        representatives: representatives.clone(),
        verified_pass_representatives: BTreeSet::new(),
        budget: StackResolutionBudget::from_legacy_max_resolutions(max_resolutions),
        policy: StackResolutionPolicy::Committed,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: baseline.clone(),
        },
    });
    for representative in representatives {
        state.auto_pass.insert(
            representative,
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: state.stack.len(),
                policy: StackResolutionPolicy::Committed,
            },
        );
    }
    state
}

#[test]
fn persisted_restore_classifies_without_advancing_stack_automation() {
    let state = ready_two_seat_state();
    let persisted = PersistedGameState::capture(state.clone());
    let encoded = serde_json::to_string(&persisted).expect("Ready state serializes");
    let restored = serde_json::from_str::<PersistedGameState>(&encoded)
        .expect("Ready state deserializes")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");

    assert_eq!(
        classify_restored_stack_automation(&restored),
        RestoredStackAutomation::LegacyResolveAllReady
    );
    assert_eq!(
        restored.stack, state.stack,
        "decode resolves no stack entry"
    );
    assert_eq!(
        restored.auto_pass, state.auto_pass,
        "decode preserves overlays"
    );
    assert_eq!(
        restored.waiting_for, state.waiting_for,
        "decode preserves Ready"
    );
}

#[test]
fn explicit_restore_resume_drives_a_coherent_session_through_the_ordinary_runner() {
    let mut state = restored_session_state(1);
    state
        .stack_resolution_session
        .as_mut()
        .expect("fixture has a session")
        .auto_pass_overlay
        .baseline
        .clear();
    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::ActiveSession
    );

    let resumed = resume_restored_stack_automation(&mut state);
    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::Progressed,
        "a coherent session must enter the ordinary runner"
    );
    let result = resumed.action_result();
    assert_eq!(
        result
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        1,
        "the saved budget limits ordinary runner resolution"
    );
    assert_eq!(state.stack.len(), 1);
    assert!(
        state.stack_resolution_session.is_none(),
        "cap tears down the session"
    );
    assert!(
        state.auto_pass.is_empty(),
        "teardown restores the empty baseline"
    );
}

#[test]
fn restored_rechecking_session_waits_for_a_fresh_ai_contract() {
    let mut state = restored_session_state(1);
    {
        let session = state
            .stack_resolution_session
            .as_mut()
            .expect("fixture has a session");
        session.policy = StackResolutionPolicy::RecheckNoMeaningfulPriorityAction;
        session.verified_pass_representatives.insert(P0);
    }
    let stack_len = state.stack.len();
    for mode in state.auto_pass.values_mut() {
        *mode = AutoPassMode::UntilStackEmpty {
            initial_stack_len: stack_len,
            policy: StackResolutionPolicy::RecheckNoMeaningfulPriorityAction,
        };
    }
    let encoded = serde_json::to_string(&PersistedGameState::capture(state))
        .expect("the active rechecking session serializes");
    let mut state = serde_json::from_str::<PersistedGameState>(&encoded)
        .expect("the active rechecking session deserializes")
        .into_game_state()
        .expect("persisted test snapshot satisfies the checked restore contract");
    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::ActiveSession
    );
    let before_stack = state.stack.clone();

    let resumed = resume_restored_stack_automation(&mut state);

    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::Progressed,
        "the restore owner did invoke the ordinary session runner"
    );
    assert_eq!(
        resumed.presentation.automated_resolution_count, 0,
        "the restored cached P0 pass advances only to P1's unverified priority window"
    );
    assert_eq!(
        state.stack, before_stack,
        "the fenced entries remain intact"
    );
    assert!(state.stack_resolution_session.is_some());
    assert!(matches!(
        state.waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    assert!(state
        .stack_resolution_session
        .as_ref()
        .expect("the paused session is retained")
        .verified_pass_representatives
        .contains(&P0));

    let contract = AiDecisionContract::issue(&state, P1);
    apply_verified_ai_priority_pass(&mut state, P1, &contract, GameAction::PassPriority)
        .expect("P1's first verified pass completes the cached cohort");

    assert_eq!(
        state.stack.len(),
        1,
        "the saved session budget remains in force"
    );
}

#[test]
fn explicit_restore_resume_publishes_revealed_cards_through_the_boundary_journal() {
    let mut state = restored_session_state(1);
    let revealed = create_object(
        &mut state,
        CardId(77),
        P0,
        "Restored Reveal".to_string(),
        Zone::Library,
    );
    state.stack.clear();
    state.stack.push_back(ability_entry(
        1,
        P0,
        Effect::RevealTop {
            player: TargetFilter::Controller,
            count: 1,
        },
        vec![],
    ));
    let entries = state
        .stack
        .iter()
        .rev()
        .map(StackResolutionEntryFence::capture)
        .collect();
    let session = state
        .stack_resolution_session
        .as_mut()
        .expect("fixture has a session");
    session.entries = entries;
    session.auto_pass_overlay.baseline.clear();
    for mode in state.auto_pass.values_mut() {
        *mode = AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        };
    }

    let resumed = resume_restored_stack_automation(&mut state);
    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::Progressed,
        "a coherent reveal session must enter the runner"
    );
    let result = resumed.action_result();
    assert!(result.events.iter().any(|event| {
        matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids == &vec![revealed])
    }));
    assert!(state.viewer_knows_card_identity(P1, revealed));
    assert!(state.resolved_rules_journal.entries().iter().any(|entry| {
        matches!(
            entry.command.as_ref(),
            Some(ResolvedRulesCommand::Information(information))
                if information.audience == ResolvedInformationAudience::Public
                    && information.edit == ResolvedInformationEdit::Reveal
                    && information.occurrences.iter().any(|occurrence| occurrence.object_id == revealed)
        )
    }));
}

#[test]
fn stale_restored_session_repairs_without_resolving_and_restores_its_baseline() {
    let mut state = restored_session_state(0);
    state
        .stack
        .back_mut()
        .expect("fixture has a top stack entry")
        .source_id = ObjectId(99);

    let resumed = resume_restored_stack_automation(&mut state);
    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::ZeroResolutionRepair,
        "a stale entry fence must repair, never advance"
    );
    let result = resumed.action_result();
    assert!(
        !result
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { .. })),
        "repair emits no stack resolution"
    );
    assert_eq!(state.stack.len(), 2, "repair leaves every entry intact");
    assert!(state.stack_resolution_session.is_none());
    assert_eq!(
        state.auto_pass.get(&P0),
        Some(&AutoPassMode::UntilTurnBoundary {
            until: Default::default(),
        })
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

#[test]
fn restored_session_rejects_a_cached_nonrepresentative_without_resolving() {
    let mut state = restored_session_state(0);
    state
        .stack_resolution_session
        .as_mut()
        .expect("fixture has a session")
        .verified_pass_representatives
        .insert(P1);
    state
        .stack_resolution_session
        .as_mut()
        .expect("fixture has a session")
        .representatives = BTreeSet::from([P0]);
    state.auto_pass.remove(&P1);
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };

    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::Repair
    );
    let resumed = resume_restored_stack_automation(&mut state);

    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::ZeroResolutionRepair
    );
    assert_eq!(
        resumed.presentation.automated_resolution_count, 0,
        "an untrusted cache entry must not pass a nonrepresentative"
    );
    assert_eq!(state.stack.len(), 2);
    assert!(state.stack_resolution_session.is_none());
}

#[test]
fn stale_restored_session_cursor_or_overlay_repairs_without_resolving() {
    let mut stale_cursor = restored_session_state(0);
    let entries_len = stale_cursor
        .stack_resolution_session
        .as_ref()
        .expect("fixture has a session")
        .entries
        .len();
    stale_cursor
        .stack_resolution_session
        .as_mut()
        .expect("fixture has a session")
        .cursor = entries_len;
    let cursor_resumed = resume_restored_stack_automation(&mut stale_cursor);
    assert_eq!(
        cursor_resumed.presentation.outcome,
        RestoredStackAutomationOutcome::ZeroResolutionRepair,
        "an exhausted cursor must repair"
    );
    let cursor_result = cursor_resumed.action_result();
    assert!(cursor_result.events.is_empty());

    let mut bad_overlay = restored_session_state(0);
    bad_overlay.auto_pass.remove(&P1);
    let overlay_resumed = resume_restored_stack_automation(&mut bad_overlay);
    assert_eq!(
        overlay_resumed.presentation.outcome,
        RestoredStackAutomationOutcome::ZeroResolutionRepair,
        "a malformed overlay must repair"
    );
    let overlay_result = overlay_resumed.action_result();
    assert!(overlay_result.events.is_empty());
    assert_eq!(bad_overlay.stack.len(), 2);
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

    // A retained auto-pass keeps P0 entitled to access the latch, but makes the
    // Ready run incoherent because the consent overlay is no longer complete.
    state.auto_pass.insert(
        P1,
        AutoPassMode::UntilStackEmpty {
            initial_stack_len: 1,
            policy: StackResolutionPolicy::Committed,
        },
    );
    assert_eq!(
        resolve_all_ready_access(&state, P0),
        ResolveAllReadyAccess::Admitted,
        "coherence is not this gate's axis; P0 remains an entitled participant"
    );
    assert_eq!(
        pending_resolve_all_ready_requester(&state),
        None,
        "a retained auto-pass makes a Ready run incoherent"
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

/// The row-8 negative sibling for `viewer_projection_ingest_gate`: the same
/// prompt as [`pending_consent_without_its_run`], but with its run still LIVE
/// and built by the real reducer. An authoritative state in this shape must
/// keep decoding — it is what proves the projection gate keys on the marker
/// rather than on the prompt.
pub(crate) fn pending_consent_with_live_run() -> GameState {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE2);
    state.stack.push_back(no_op_entry(1, P0));
    begin(&mut state);
    state
}

/// The reporter's shape: P0 proposes Resolve All, P1's consent is queued, and
/// the private run is gone while the public prompt still stands.
pub(crate) fn pending_consent_without_its_run() -> GameState {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE2);
    state.stack.push_back(no_op_entry(1, P0));
    let epoch = begin(&mut state);
    // Not an arbitrary mutation: this is exactly what a viewer projection of a
    // pending consent carries, so any state reconstructed from one has it.
    let projected = filter_state_for_viewer(&state, P0);
    assert_eq!(projected.waiting_for, state.waiting_for);
    assert!(projected.resolve_all_consent_run.is_none());
    state.resolve_all_consent_run = None;
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ResolveAllConsent { epoch: e, representative: P1 } if e == epoch
    ));
    state
}

#[test]
fn a_consent_prompt_with_no_run_issues_no_response_and_reports_a_wedge() {
    let mut state = pending_consent_without_its_run();
    let WaitingFor::ResolveAllConsent { epoch, .. } = state.waiting_for else {
        unreachable!("the fixture asserts its own prompt")
    };

    assert!(
        !candidate_actions(&state).iter().any(|candidate| matches!(
            candidate.action,
            GameAction::RespondResolveAllConsent { .. }
        )),
        "a response the reducer can only reject must never enter the issued domain"
    );
    assert!(
        AiDecisionContract::issue(&state, P1).candidates.is_empty(),
        "this prompt's contract is issued without reducer simulation, so an \
         unanswerable candidate would reach an AI submission unchecked"
    );
    for decision in [
        ResolveAllConsentDecision::Grant,
        ResolveAllConsentDecision::Decline,
    ] {
        assert!(
            apply(
                &mut state,
                P1,
                GameAction::RespondResolveAllConsent { epoch, decision },
            )
            .is_err(),
            "the empty domain is correct rather than over-strict: {decision:?} is refused"
        );
    }

    let diagnostic = stuck_decision_diagnostic(&state)
        .expect("a decision nobody can answer must surface as a wedge");
    assert_eq!(diagnostic.waiting_for_kind, "ResolveAllConsent");
    assert_eq!(diagnostic.stuck_players, vec![P1]);
    assert!(
        legal_actions_for_viewer(&state, P1).0.is_empty(),
        "the representative's own viewer set was already empty; the issued \
         domain is what disagreed with it"
    );
}

#[test]
fn a_consent_prompt_with_no_run_repairs_to_priority_without_resolving() {
    let mut coherent = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE3);
    coherent.stack.push_back(no_op_entry(1, P0));
    begin(&mut coherent);
    assert_eq!(
        classify_restored_stack_automation(&coherent),
        RestoredStackAutomation::None,
        "a live consent prompt is a decision to answer, not automation to resume"
    );

    let mut state = pending_consent_without_its_run();
    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::Repair,
        "an unanswerable saved authorization is a repair, like a run-less Ready latch"
    );

    let resumed = resume_restored_stack_automation(&mut state);

    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::ZeroResolutionRepair
    );
    assert_eq!(resumed.presentation.automated_resolution_count, 0);
    assert_eq!(state.stack.len(), 1, "the stack entry survives the repair");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the repair must restore ordinary priority, got {:?}",
        state.waiting_for
    );
    assert!(state.resolve_all_consent_run.is_none());
}

/// Generic restore classification is inert on the overwhelming majority of
/// states, which carry no stack automation at all.
#[test]
fn recovery_is_inert_on_a_state_carrying_no_latch() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 0x0C0F_FEE1);
    state.stack.push_back(no_op_entry(1, P1));
    let before = state.waiting_for.clone();

    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::None,
        "generic restore classification must leave ordinary states alone"
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

    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::LegacyResolveAllReady,
        "generic decode recognizes but does not consume the legacy authorization"
    );
    let resumed = resume_restored_stack_automation(&mut state);
    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::Progressed,
        "an intact Ready latch resumes through the shared session runner"
    );
    let batch = resumed.action_result();

    assert_eq!(
        batch
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        1,
        "the explicit resume resolves the consented entry"
    );
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
    // game, which is what makes the latch unconsumable. Retained auto-pass is
    // deliberately not used here because it is now coherent.
    state.priority_pass_count += 1;
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

    let resumed = resume_restored_stack_automation(&mut state);
    assert_eq!(
        resumed.presentation.outcome,
        RestoredStackAutomationOutcome::ZeroResolutionRepair,
        "an incoherent latch must repair without resolving"
    );
    let batch = resumed.action_result();

    assert_eq!(
        batch
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::StackResolved { .. }))
            .count(),
        0,
        "an incoherent run resolves nothing"
    );
    assert_eq!(state.stack.len(), 1, "the stack entry survives the repair");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "recovery must restore ordinary priority, got {:?}",
        state.waiting_for
    );
    assert!(
        state.auto_pass.is_empty(),
        "an incoherent legacy Ready latch has no valid live auto-pass baseline"
    );
    let ordinary_pass = apply(&mut state, P0, GameAction::PassPriority)
        .expect("ordinary priority resumes after repair");
    assert!(
        !ordinary_pass
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::StackResolved { .. })),
        "the first ordinary boundary must not inherit the discarded Ready auto-pass"
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
    state
        .resolve_all_consent_run
        .as_mut()
        .expect("pending run exists")
        .auto_pass_baseline = None;
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
/// A live session installs and immediately executes `UntilStackEmpty` so the
/// requester's standing intent survives a proof that stopped short. A restore
/// must not: that auto-pass resolves the rest of the stack through the ordinary
/// pipeline, which can end the game — and a restore has no socket attached and
/// no caller positioned to emit a ranked result or a terminal artifact, so the
/// game would be registered live while parked in `GameOver`.
#[test]
fn the_restore_continuation_installs_no_auto_pass_where_the_live_one_does() {
    let mut live = proof_stopping_ready_state();
    let live_batch =
        resolve_all_ready_prefix_with(&mut live, P0, ResolveAllContinuation::AutoPassRemainder);
    assert!(
        live.auto_pass.is_empty(),
        "the live continuation consumes its temporary fallback when CopySpell grows the stack"
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
        live_batch.items_resolved > restored_batch.items_resolved,
        "the live continuation runs past the bounded proof prefix"
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
