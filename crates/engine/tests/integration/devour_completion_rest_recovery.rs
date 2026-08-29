//! Regression for the production capture `b152fcbf-0976-408a-a501-346237e1f8cb`:
//! a Bloodspore Thrinax Devour entry completed with an empty post-replacement
//! parent below its Devour-only ChangeZone snapshot. The stale resolution
//! carrier then let a later priority pass enter `start_next_turn` and panic.

use engine::game::engine::{
    apply, classify_restored_stack_automation, EngineError, RestoredStackAutomation,
};
use engine::game::triggers::{PendingTrigger, PendingTriggerContext};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    AutoPassMode, CastingVariant, GameState, PendingResolutionCompletion, PendingSpellResolution,
    PersistedGameState, PersistedRestoreError, PersistedRestoreFinalization,
    PostReplacementDrainStack, StackEntry, StackEntryKind, StackResolutionAutoPassOverlay,
    StackResolutionBudget, StackResolutionEntryFence, StackResolutionPolicy,
    StackResolutionSession, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use std::collections::{BTreeMap, BTreeSet};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn bare_spell_resolution_rest_state() -> GameState {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.phase = Phase::Cleanup;
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(347),
        source_id: ObjectId(347),
        controller: P3,
        kind: StackEntryKind::Spell {
            card_id: CardId(347),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 4,
        },
    });
    state.push_spell_resolution(PendingSpellResolution {
        object_id: ObjectId(347),
        controller: P3,
        casting_variant: CastingVariant::Normal,
        cast_from_zone: None,
        cast_controller: Some(P3),
        cast_timing_permission: None,
        spell_targets: vec![],
        actual_mana_spent: 4,
        kickers_paid: vec![],
        additional_cost_payment_count: 0,
        additional_cost_payments: vec![],
        convoked_creatures: vec![],
    });
    state
}

fn deferred_draw_trigger() -> PendingTriggerContext {
    PendingTriggerContext::single(PendingTrigger {
        source_id: ObjectId(901),
        controller: P3,
        condition: None,
        ability: Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(901),
            P3,
        )),
        timestamp: 0,
        target_constraints: vec![],
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
        provenance: None,
    })
}

/// CR 117.3b + CR 117.4 + CR 608.2c + CR 614.12a + CR 614.13a: an old pass
/// submitted from the captured priority window repairs the completed Devour
/// entry, grants priority to the active player, and does not advance the turn.
#[test]
fn captured_devour_rest_shape_recovers_before_a_stale_pass_can_start_the_next_turn() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.phase = Phase::Cleanup;
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
    state.priority_pass_count = 3;
    state.priority_passes.extend([P0, P1, P3]);
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(347),
        source_id: ObjectId(347),
        controller: P3,
        kind: StackEntryKind::Spell {
            card_id: CardId(347),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 4,
        },
    });
    state
        .resolution_stack
        .push_post_replacement(PostReplacementDrainStack::default());
    state.push_devour_change_zone_snapshot([ObjectId(27), ObjectId(31)].into_iter().collect());

    let result = apply(&mut state, P2, GameAction::PassPriority)
        .expect("the stale captured pass repairs the ownerless rest state");

    assert!(
        state.resolution_stack.is_empty(),
        "the empty replacement parent and its Devour-only snapshot must both retire"
    );
    assert!(
        state.resolving_stack_entry.is_none(),
        "the completed spell carrier must settle before another turn can begin"
    );
    assert_eq!(
        state.phase,
        Phase::Cleanup,
        "the stale pass must not advance phase"
    );
    assert_eq!(state.priority_player, P3);
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: P3 },
        "CR 117.3b grants the active player the recovered priority window"
    );
    assert!(state.priority_passes.is_empty());
    assert_eq!(state.priority_pass_count, 0);
    assert_eq!(result.waiting_for, state.waiting_for);
}

/// An actor unauthorized in the captured window cannot consume the recovery
/// no-op, even though the impossible persisted state is repaired.
#[test]
fn unauthorized_pass_repairs_but_cannot_spend_devour_recovery() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.phase = Phase::Cleanup;
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(347),
        source_id: ObjectId(347),
        controller: P3,
        kind: StackEntryKind::Spell {
            card_id: CardId(347),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 4,
        },
    });
    state
        .resolution_stack
        .push_post_replacement(PostReplacementDrainStack::default());
    state.push_devour_change_zone_snapshot([ObjectId(27), ObjectId(31)].into_iter().collect());

    assert!(matches!(
        apply(&mut state, P0, GameAction::PassPriority),
        Err(EngineError::WrongPlayer)
    ));
    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P3 });
}

/// The Discord turn-26 capture has no Devour frame: only a completed
/// permanent-spell epilogue remains above the resolving carrier.
#[test]
fn bare_spell_resolution_rest_recovers_before_a_captured_pass() {
    let mut state = bare_spell_resolution_rest_state();

    let result = apply(&mut state, P2, GameAction::PassPriority)
        .expect("the captured priority holder may submit the recovery no-op");

    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(state.phase, Phase::Cleanup);
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P3 });
    assert_eq!(result.waiting_for, state.waiting_for);
}

/// A terminal-looking spell frame with another completion hold is live work,
/// not persisted residue; recovery must leave it to the ordinary resumer.
#[test]
fn bare_spell_resolution_recovery_preserves_live_completion_work() {
    let mut state = bare_spell_resolution_rest_state();
    state.pending_resolution_completion = Some(PendingResolutionCompletion {
        player: P3,
        source_id: ObjectId(347),
        final_cast: None,
    });
    let before = state.resolution_stack.clone();

    apply(&mut state, P0, GameAction::SetPhaseStops { stops: vec![] })
        .expect("actor-scoped preferences are valid at every prompt");

    assert_eq!(state.resolution_stack, before);
    assert!(state.resolving_stack_entry.is_some());
}

/// The persistence boundary repairs only the exact terminal carrier before
/// runtime rehydration, then exposes one settled priority window. It does not
/// need (or manufacture) a priority pass to do so.
#[test]
fn persisted_bare_spell_rest_is_settled_before_rehydrated_publication() {
    let state = PersistedGameState::Raw(Box::new(bare_spell_resolution_rest_state()))
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the exact completed spell carrier is engine-owned residue")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("the rehydrated state is publishable");

    assert!(state.resolution_stack.is_empty());
    assert!(state.resolving_stack_entry.is_none());
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P3 });
    assert_eq!(state.priority_player, P3);
}

/// A priority window with resolution ownership that is neither a coherent
/// stack session nor an exact terminal rest is rejected at persistence decode,
/// rather than being exposed for a later turn-advance panic.
#[test]
fn persisted_unsettled_priority_resolution_fails_closed() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.waiting_for = WaitingFor::Priority { player: P2 };
    state.resolving_stack_entry = Some(StackEntry {
        id: ObjectId(347),
        source_id: ObjectId(347),
        controller: P3,
        kind: StackEntryKind::Spell {
            card_id: CardId(347),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 4,
        },
    });

    assert_eq!(
        PersistedGameState::Raw(Box::new(state))
            .into_game_state()
            .expect_err("an unclassified priority-time carrier must not be published"),
        PersistedRestoreError::UnsettledPriorityResolution,
    );
}

/// Deferred triggers captured beside a terminal carrier are an engine-owned
/// settlement obligation. Restore must construct them before it can expose a
/// priority window, just as the ordinary post-action pipeline does.
#[test]
fn persisted_terminal_rest_settles_deferred_triggers_before_priority() {
    let mut state = bare_spell_resolution_rest_state();
    state.deferred_triggers.push(deferred_draw_trigger());

    let state = PersistedGameState::Raw(Box::new(state))
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the exact carrier remains repairable")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("deferred trigger construction settles before publication");

    assert!(state.deferred_triggers.is_empty());
    assert!(
        !matches!(state.waiting_for, WaitingFor::Priority { .. }) || !state.stack.is_empty(),
        "a restored queued trigger must order or reach the stack before a priority window"
    );
}

/// A raw snapshot can forge the serialized recipient that normally carries a
/// settled-priority construction batch. It must not unlock that exceptional
/// drain policy above a passive spell; the restore boundary drops the recipient
/// and leaves ordinary trigger construction to its normal action boundary.
#[test]
fn forged_settled_priority_recipient_cannot_bypass_resolution_safe_restore() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 0xB152_FCBF);
    state.active_player = P3;
    state.priority_player = P2;
    state.waiting_for = WaitingFor::Priority { player: P2 };
    state.stack.push_back(StackEntry {
        id: ObjectId(347),
        source_id: ObjectId(347),
        controller: P3,
        kind: StackEntryKind::Spell {
            card_id: CardId(347),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 4,
        },
    });
    state.deferred_triggers.push(deferred_draw_trigger());

    let mut raw = serde_json::to_value(state).expect("raw state serializes");
    raw["pending_trigger_construction_priority_recipient"] = serde_json::json!(P2.0);
    let persisted: PersistedGameState =
        serde_json::from_value(raw).expect("forged raw state still decodes as a raw snapshot");
    assert_eq!(
        serde_json::to_value(&persisted)
            .expect("the decoded snapshot reserializes")
            .get("pending_trigger_construction_priority_recipient"),
        Some(&serde_json::json!(P2.0)),
        "reach guard: the forged recipient must survive decode before restore removes it"
    );

    let restored = persisted
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the passive spell is a valid unresolved stack state")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("the unchanged stack remains a valid restore state");

    let restored_wire = serde_json::to_value(&restored).expect("the restored state serializes");
    assert!(
        restored_wire
            .get("pending_trigger_construction_priority_recipient")
            .is_none(),
        "a serialized recipient alone must not authorize settled-priority construction"
    );
    assert_eq!(restored.deferred_triggers.len(), 1);
    assert_eq!(restored.stack.len(), 1);
}

#[test]
fn persisted_coherent_stack_session_defers_queued_triggers_to_session_resume() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 9);
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state.stack.push_back(StackEntry {
        id: ObjectId(77),
        source_id: ObjectId(77),
        controller: P0,
        kind: StackEntryKind::Spell {
            card_id: CardId(77),
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 1,
        },
    });
    let representatives = BTreeSet::from([P0, P1, P2, P3]);
    for representative in &representatives {
        state.auto_pass.insert(
            *representative,
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: state.stack.len(),
                policy: StackResolutionPolicy::Committed,
            },
        );
    }
    state.stack_resolution_session = Some(StackResolutionSession {
        entries: vec![StackResolutionEntryFence::capture(
            state.stack.back().unwrap(),
        )],
        cursor: 0,
        representatives,
        verified_pass_representatives: BTreeSet::new(),
        budget: StackResolutionBudget::Unlimited,
        policy: StackResolutionPolicy::Committed,
        auto_pass_overlay: StackResolutionAutoPassOverlay {
            baseline: BTreeMap::new(),
        },
    });
    state.deferred_triggers.push(deferred_draw_trigger());
    let state = PersistedGameState::Raw(Box::new(state))
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("the coherent session is valid persisted automation")
        .finalize_after_rehydration(|_| Ok(()))
        .expect("restore leaves queued triggers to the coherent session runner");
    assert_eq!(
        classify_restored_stack_automation(&state),
        RestoredStackAutomation::ActiveSession,
        "the restored authorization remains coherent for its explicit resumer"
    );
    assert_eq!(state.deferred_triggers.len(), 1);
}
