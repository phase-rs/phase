use super::process_triggers;
use crate::game::effects::resolve_ability_chain;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect,
    QuantityExpr, ResolvedAbility, TargetFilter, TargetRef, TriggerDefinition, TypedFilter,
};
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntryKind, WaitingFor, ZoneChangeRecord};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

fn setup() -> GameState {
    GameState::new_two_player(42)
}

fn make_creature(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    obj.power = Some(2);
    obj.toughness = Some(2);
    id
}

fn zone_changed_event(object_id: ObjectId, from: Zone, to: Zone) -> GameEvent {
    GameEvent::ZoneChanged {
        object_id,
        from: Some(from),
        to,
        record: Box::new(ZoneChangeRecord {
            name: "Test Source".to_string(),
            core_types: vec![CoreType::Enchantment],
            subtypes: vec![],
            ..ZoneChangeRecord::test_minimal(object_id, Some(from), to)
        }),
    }
}

fn build_exile_target_opponent_creature_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .execute(AbilityDefinition::new(
            AbilityKind::Database,
            Effect::ChangeZone {
                enters_modified_if: None,
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        ))
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield)
}

fn make_source_with_trigger(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        PlayerId(0),
        "Test Source".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.entered_battlefield_turn = Some(1);
    obj.trigger_definitions
        .push(build_exile_target_opponent_creature_trigger());
    id
}

/// Test #1 (Lulu / blocker-validating): a target-requiring trigger pushes
/// to the stack BEFORE prompting the controller. This test must FAIL on
/// the pre-refactor codebase and PASS on the post-refactor codebase.
#[test]
fn push_first_target_trigger_appears_on_stack_during_prompt() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Two legal opponent creatures so target choice cannot auto-resolve.
    let target1 = make_creature(&mut state, PlayerId(1), "Opp 1");
    let _target2 = make_creature(&mut state, PlayerId(1), "Opp 2");
    let source = make_source_with_trigger(&mut state);

    process_triggers(
        &mut state,
        &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
    );

    // CR 603.3c + CR 603.3d "Push first": the trigger entry MUST be on the
    // stack already, identified by `pending_trigger_entry`. This is the
    // structural change that fails on the pre-refactor codebase, where
    // `process_triggers` would set `pending_trigger` without pushing.
    assert_eq!(
        state.stack.len(),
        1,
        "trigger entry must be on the stack while target prompt is pending",
    );
    let entry_id = state
        .pending_trigger_entry
        .expect("pending_trigger_entry must mark the in-construction entry");
    assert_eq!(state.stack.back().map(|e| e.id), Some(entry_id));
    let entry = state.stack.back().unwrap();
    assert_eq!(entry.source_id, source);
    assert!(matches!(
        entry.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert!(state.pending_trigger.is_some());

    // Drive the engine pipeline forward — `begin_pending_trigger_target_selection`
    // translates the pending state into `WaitingFor::TriggerTargetSelection`,
    // matching what the action dispatcher does in production.
    let wf = crate::game::engine::begin_pending_trigger_target_selection(&mut state)
        .expect("begin target selection")
        .expect("target prompt required (two legal targets)");
    state.waiting_for = wf;
    assert!(matches!(
        state.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));

    // Complete the choice: entry stays on the stack, fully constructed,
    // with targets populated. Cursor cleared.
    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target1)),
        },
    )
    .expect("choose target succeeds");
    assert_eq!(state.stack.len(), 1, "entry remains on stack post-choice");
    assert!(
        state.pending_trigger_entry.is_none(),
        "construction complete -> cursor cleared",
    );
    let entry = state.stack.back().unwrap();
    if let StackEntryKind::TriggeredAbility { ability, .. } = &entry.kind {
        assert_eq!(ability.targets, vec![TargetRef::Object(target1)]);
    } else {
        panic!("expected TriggeredAbility on stack");
    }
}

/// Test #5 (resolver-refusal): `stack::resolve_top` must NOT fire the top
/// entry while `pending_trigger_entry` identifies it. This is the
/// invariant gate that prevents the in-construction entry from resolving.
#[test]
fn push_first_resolver_refuses_in_construction_entry() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Set up a pending-target trigger via the production pipeline (so the
    // entry is genuinely in-construction, not synthesized by hand).
    let _t1 = make_creature(&mut state, PlayerId(1), "Opp 1");
    let _t2 = make_creature(&mut state, PlayerId(1), "Opp 2");
    let source = make_source_with_trigger(&mut state);
    process_triggers(
        &mut state,
        &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
    );

    // Confirm pre-conditions: top is the in-construction entry.
    let in_construction_id = state.pending_trigger_entry.expect("entry set");
    let stack_len_before = state.stack.len();
    assert_eq!(state.stack.back().map(|e| e.id), Some(in_construction_id));

    // Call resolve_top directly: it must refuse to act on the entry.
    let mut events = Vec::new();
    crate::game::stack::resolve_top(&mut state, &mut events);

    assert_eq!(
        state.stack.len(),
        stack_len_before,
        "resolve_top must not pop the in-construction entry",
    );
    assert_eq!(
        state.stack.back().map(|e| e.id),
        Some(in_construction_id),
        "in-construction entry stays on top",
    );
    assert!(
        events.is_empty(),
        "no StackResolved event for refused resolution, got {events:?}",
    );
    assert_eq!(
        state.pending_trigger_entry,
        Some(in_construction_id),
        "cursor preserved on refusal",
    );
}

/// Test #7 (CR 603.3d → CR 601.2c no-legal-targets removal): a
/// target-requiring trigger with zero legal targets is dropped without
/// pushing to the stack or leaving a cursor.
#[test]
fn push_first_no_legal_targets_drops_trigger_silently() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Opponent controls NO creatures; the trigger requires a target
    // opponent creature.
    let source = make_source_with_trigger(&mut state);
    let stack_before = state.stack.len();
    process_triggers(
        &mut state,
        &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
    );

    assert_eq!(
        state.stack.len(),
        stack_before,
        "no-legal-target trigger must not be pushed to the stack",
    );
    assert!(
        state.pending_trigger_entry.is_none(),
        "no-legal-target trigger must not leave a cursor",
    );
    assert!(state.pending_trigger.is_none());
}

/// Test #10 (reflexive WhenYouDo trigger): the push-first contract holds
/// at the OTHER pause-path site, `effects/mod.rs::resolve_chain_body`
/// (line ~3654). A reflexive `WhenYouDo` sub-ability with empty `targets`
/// and a non-empty target-slot set must push the entry to the stack
/// BEFORE entering `WaitingFor::TriggerTargetSelection`. Structurally
/// identical to the main dispatch path but lives in a different function;
/// a regression here would not fail the other discriminating tests.
#[test]
fn push_first_reflexive_when_you_do_pushes_to_stack_during_prompt() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    // Cost-payment must succeed so the WhenYouDo gate fires (CR 603.12 +
    // issue #418): controller has exactly enough energy.
    state.players[0].energy = 3;

    // Two legal target candidates (own creatures) so the reflexive's
    // PutCounter target slot has multiple legal choices, forcing the
    // player-choice path through `begin_target_selection_for_ability`.
    let candidate1 = make_creature(&mut state, PlayerId(0), "Candidate 1");
    let _candidate2 = make_creature(&mut state, PlayerId(0), "Candidate 2");

    // Reflexive sub-ability: PutCounter on a chosen creature you control.
    // `targets` is empty so `effects/mod.rs:3585-3667` enters the
    // push-first path; `TargetFilter::Typed` resolves to a target slot
    // when `build_target_slots` runs.
    let source_id = ObjectId(state.next_object_id);
    let sub = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        },
        vec![],
        source_id,
        PlayerId(0),
    )
    .condition(AbilityCondition::WhenYouDo);

    // Parent: pay {E}{E}{E}. On success the reflexive `WhenYouDo` fires.
    let parent = ResolvedAbility::new(
        Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
        vec![],
        source_id,
        PlayerId(0),
    )
    .sub_ability(sub);

    let stack_before = state.stack.len();
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, &parent, &mut events, 0).expect("resolve parent chain");

    // CR 603.3c + CR 603.3d: The reflexive trigger entry MUST be on the
    // stack now (push-first contract holds at the reflexive site too).
    assert_eq!(
        state.stack.len(),
        stack_before + 1,
        "reflexive WhenYouDo trigger must be on the stack while its target prompt is open",
    );
    let entry_id = state
        .pending_trigger_entry
        .expect("pending_trigger_entry must mark the in-construction entry");
    assert_eq!(state.stack.back().map(|e| e.id), Some(entry_id));
    let entry = state.stack.back().unwrap();
    assert!(matches!(
        entry.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert!(matches!(
        state.waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));

    // Complete the target choice: entry stays on stack, fully constructed.
    crate::game::engine::apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(candidate1)),
        },
    )
    .expect("choose target succeeds");
    assert!(
        state.pending_trigger_entry.is_none(),
        "reflexive construction complete -> cursor cleared",
    );
}

/// Test #6 (CR 603.3c no-legal-modes modal early-drop): a modal trigger
/// where every mode's target is illegal must be dropped at the modal
/// pre-filter (`triggers.rs::dispatch_pending_trigger_context` line ~2235)
/// BEFORE any `StackPushed` event is emitted. Exercises the new pre-push
/// logic (`compute_unavailable_modes` + `filter_modes_by_target_legality`)
/// that is otherwise structurally unverified by the non-modal Err-branch
/// tests.
#[test]
fn push_first_no_legal_modes_modal_trigger_dropped_silently() {
    use crate::types::ability::{ModalChoice, PlayerFilter};

    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // No opponent creatures exist. Both modes target an opponent
    // creature, so every mode pre-filters as illegal at modal-pause time.
    let source_id = ObjectId(state.next_object_id);
    let opponent_creature_target =
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
    let mode_a = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::Destroy {
            target: opponent_creature_target.clone(),
            cant_regenerate: false,
        },
    );
    let mode_b = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::Destroy {
            target: opponent_creature_target.clone(),
            cant_regenerate: false,
        },
    );
    let modal = ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        mode_descriptions: vec!["A".to_string(), "B".to_string()],
        allow_repeat_modes: false,
        constraints: vec![],
        mode_costs: vec![],
        mode_pawprints: vec![],
        entwine_cost: None,
        chooser: PlayerFilter::Controller,
        selection: crate::types::ability::TargetSelectionMode::Chosen,
        dynamic_max_choices: None,
    };
    let modal_ability = AbilityDefinition::new(
        AbilityKind::Database,
        // Inner effect is not actually executed (a mode replaces it); pick
        // a placeholder that resolves cleanly if it were ever to fire.
        Effect::Destroy {
            target: TargetFilter::None,
            cant_regenerate: false,
        },
    )
    .with_modal(modal, vec![mode_a, mode_b]);

    // Construct a PendingTrigger directly and dispatch it through the
    // public pipeline. Modal trigger context with no legal mode reaches
    // the early-drop branch at `triggers.rs::dispatch_pending_trigger_context`.
    let trigger = super::PendingTrigger {
        source_id,
        controller: PlayerId(0),
        condition: None,
        ability: super::super::ability_utils::build_resolved_from_def(
            &modal_ability,
            source_id,
            PlayerId(0),
        ),
        timestamp: state.turn_number,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: modal_ability.modal.clone(),
        mode_abilities: modal_ability.mode_abilities.clone(),
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };

    let stack_before = state.stack.len();
    let mut events = Vec::new();
    let paused = super::dispatch_pending_trigger_context(
        &mut state,
        super::PendingTriggerContext::single(trigger),
        &mut events,
    )
    .paused();

    // CR 603.3c "If no mode can be chosen, the ability is removed from
    // the stack": dispatcher reports no pause; nothing pushed; no cursor.
    assert!(
        !paused,
        "modal trigger with no legal mode must not pause on player input",
    );
    assert_eq!(
        state.stack.len(),
        stack_before,
        "no-legal-mode modal trigger must NOT be pushed to the stack",
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, GameEvent::StackPushed { .. })),
        "no StackPushed event must be emitted for a dropped no-legal-mode modal trigger",
    );
    assert!(
        state.pending_trigger_entry.is_none(),
        "no-legal-mode modal trigger must not leave a cursor",
    );
    assert!(
        state.pending_trigger.is_none(),
        "no-legal-mode modal trigger must not leave a stashed pending_trigger",
    );
}

/// CR 700.2b (override) + CR 701.9b (analogous): a modal triggered ability
/// declared "choose one at random" (Cult of Skaro) must NOT prompt the
/// controller with `AbilityModeChoice` — the game picks the mode via
/// `state.rng` and the ability reaches the stack with a resolved,
/// non-modal mode. Regression test for the unconsumed-random-axis defect.
#[test]
fn random_modal_trigger_resolves_without_prompting() {
    use crate::types::ability::{ModalChoice, PlayerFilter, QuantityExpr, TargetSelectionMode};

    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let source_id = ObjectId(state.next_object_id);
    // Two non-targeting modes so the ONLY possible prompt is mode choice.
    let mode_a = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let mode_b = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    );
    let modal = ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        mode_descriptions: vec!["A".to_string(), "B".to_string()],
        allow_repeat_modes: false,
        constraints: vec![],
        mode_costs: vec![],
        mode_pawprints: vec![],
        entwine_cost: None,
        chooser: PlayerFilter::Controller,
        // The axis under test: the game selects the mode at random.
        selection: TargetSelectionMode::Random,
        dynamic_max_choices: None,
    };
    let modal_ability = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 0 },
            player: TargetFilter::Controller,
        },
    )
    .with_modal(modal, vec![mode_a, mode_b]);

    let trigger = super::PendingTrigger {
        source_id,
        controller: PlayerId(0),
        condition: None,
        ability: super::super::ability_utils::build_resolved_from_def(
            &modal_ability,
            source_id,
            PlayerId(0),
        ),
        timestamp: state.turn_number,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: modal_ability.modal.clone(),
        mode_abilities: modal_ability.mode_abilities.clone(),
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };

    let stack_before = state.stack.len();
    let mut events = Vec::new();
    let paused = super::dispatch_pending_trigger_context(
        &mut state,
        super::PendingTriggerContext::single(trigger),
        &mut events,
    )
    .paused();

    // The game chose the mode — neither modes need targets, so dispatch
    // reports "not paused" and never raises an interactive prompt.
    assert!(
        !paused,
        "random modal trigger with non-targeting modes must not pause on input",
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::AbilityModeChoice { .. }),
        "random modal trigger must NOT prompt the controller for the mode",
    );
    assert_eq!(
        state.stack.len(),
        stack_before + 1,
        "random modal trigger must be pushed to the stack with a chosen mode",
    );
    // Construction is complete — the resolved mode replaced the modal data.
    assert!(
        state.pending_trigger.is_none(),
        "random modal trigger must finish construction (no stashed pending_trigger)",
    );
    assert!(
        state.pending_trigger_entry.is_none(),
        "random modal trigger entry must be resolver-eligible (cursor cleared)",
    );
}

/// SHAPE-level regression (manual surgery): drives the real dispatch pipeline to
/// a genuine mid-construction pause, then manually removes the in-construction
/// entry to model an unexpected dangling `pending_trigger_entry`. The real
/// upstream producer of that dangling cursor is UNIDENTIFIED — the two known
/// rules-legal causes were both proven unreachable (a controller leaving is
/// handled upstream by `elimination::do_eliminate`; countering/removal
/// mid-construction is impossible because modes/targets are chosen before anyone
/// has priority, CR 603.3d) — so this test surgically injects the corrupt state
/// rather than reproducing the producer. See the follow-up issue for the hunt.
///
/// The recovery (`triggers::abandon_ceased_pending_trigger`, CR 608.2b / CR
/// 800.4a recovery semantics) must NOT panic in
/// `assign_pending_trigger_entry_ability`; it must record a distinguishable
/// diagnostic, clean the vanished entry's side tables, clear all pending-trigger
/// state, and hand control back to the game via `Priority`.
///
/// Reverting the recovery path makes this test panic with
/// `pending_trigger_entry must reference a stack entry` — the exact live
/// production crash (`submitAction-panic`, WASM-poisoning).
#[test]
fn ceased_in_construction_trigger_recovers_without_panic_on_completion() {
    let mut state = setup();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Two legal opponent creatures so target choice cannot auto-resolve — the
    // trigger genuinely pauses mid-construction via the real dispatch pipeline.
    let target1 = make_creature(&mut state, PlayerId(1), "Opp 1");
    let _target2 = make_creature(&mut state, PlayerId(1), "Opp 2");
    let source = make_source_with_trigger(&mut state);

    process_triggers(
        &mut state,
        &[zone_changed_event(source, Zone::Hand, Zone::Battlefield)],
    );

    let entry_id = state
        .pending_trigger_entry
        .expect("push-first: trigger entry is in construction on the stack");
    let wf = crate::game::engine::begin_pending_trigger_target_selection(&mut state)
        .expect("begin target selection")
        .expect("target prompt required (two legal targets)");
    state.waiting_for = wf;

    // Manual surgery: drop ONLY the entry from the stack, modelling an unexpected
    // dangling cursor. Seed BOTH per-entry side tables so the assertions that the
    // recovery owns their cleanup (not the test) are non-vacuous — the recovery,
    // not this test, must prune them.
    state.stack.retain(|e| e.id != entry_id);
    state
        .stack_trigger_event_batches
        .insert(entry_id, Vec::new());
    state.stack_paid_facts.insert(
        entry_id,
        crate::types::game_state::StackPaidSnapshot::default(),
    );
    assert!(
        state.stack_trigger_event_batches.contains_key(&entry_id)
            && state.stack_paid_facts.contains_key(&entry_id),
        "reach-guard: both side-table rows seeded before recovery runs",
    );

    // Submitting the choice for the now-dead trigger must recover, not panic.
    let result = crate::game::engine::apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target1)),
        },
    )
    .expect("completion of a ceased trigger recovers");

    assert!(
        state.pending_trigger_entry.is_none(),
        "ceased trigger must clear the construction cursor",
    );
    assert!(
        state.pending_trigger.is_none(),
        "ceased trigger must clear the stashed pending_trigger",
    );
    // (c) the trigger event batch is cleared as part of the coherent cleanup.
    assert!(
        state.pending_trigger_event_batch.is_empty(),
        "ceased trigger must clear pending_trigger_event_batch",
    );
    // (b) the completion hands control back to the game via Priority.
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "ceased trigger recovery must return Priority, got {:?}",
        result.waiting_for,
    );
    // (a) the abandon is recorded distinguishably for telemetry (count matters).
    assert_eq!(
        state.pending_trigger_abandons.len(),
        1,
        "the abandon must be recorded exactly once for telemetry",
    );
    // Descriptor must carry BOTH the source name and the dead stack-entry id.
    assert!(
        state.pending_trigger_abandons[0].contains("Test Source"),
        "diagnostic must name the source, got {:?}",
        state.pending_trigger_abandons,
    );
    assert!(
        state.pending_trigger_abandons[0].contains(&format!("(stack entry {})", entry_id.0)),
        "diagnostic must carry the dead stack-entry id, got {:?}",
        state.pending_trigger_abandons,
    );
    // Item 5 hygiene: the recovery — not the test — owns per-entry side-table
    // cleanup for the vanished entry.
    assert!(
        !state.stack_trigger_event_batches.contains_key(&entry_id),
        "recovery must prune the dead entry's stack_trigger_event_batches row",
    );
    assert!(
        !state.stack_paid_facts.contains_key(&entry_id),
        "recovery must prune the dead entry's stack_paid_facts row",
    );
}
