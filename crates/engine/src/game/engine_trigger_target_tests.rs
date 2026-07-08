use super::*;
use crate::game::zones::create_object;
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, ModalChoice, ModalSelectionConstraint,
    QuantityExpr, ResolvedAbility, StaticCondition, TargetFilter, TargetRef, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::game_state::TargetSelectionConstraint;
use crate::types::identifiers::CardId;

#[test]
fn trigger_target_selection_select_targets_pushes_to_stack() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Create two opponent creatures as legal targets
    let target1 = create_object(
        &mut state,
        CardId(10),
        PlayerId(1),
        "Opp Creature 1".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&target1)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    state.objects.get_mut(&target1).unwrap().controller = PlayerId(1);

    let target2 = create_object(
        &mut state,
        CardId(11),
        PlayerId(1),
        "Opp Creature 2".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&target2)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    state.objects.get_mut(&target2).unwrap().controller = PlayerId(1);

    // Create trigger creature (Banishing Light)
    let trigger_creature = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Banishing Light".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&trigger_creature).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.entered_battlefield_turn = Some(1);
    }

    // Manually set up the pending trigger state (as process_triggers would)
    let ability = crate::types::ability::ResolvedAbility::new(
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
        Vec::new(),
        trigger_creature,
        PlayerId(0),
    )
    .duration(crate::types::ability::Duration::UntilHostLeavesPlay);

    // CR 603.3c + CR 603.3d "Push first": match what production does —
    // push the trigger entry to the stack and stash both the pending
    // trigger and the cursor BEFORE entering target selection.
    let pending = crate::game::triggers::PendingTrigger {
        source_id: trigger_creature,
        controller: PlayerId(0),
        condition: None,
        ability,
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);

    let legal_targets = vec![TargetRef::Object(target1), TargetRef::Object(target2)];

    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: PlayerId(0),
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![crate::types::game_state::TargetSelectionSlot {
            legal_targets: legal_targets.clone(),
            optional: false,
        }],
        target_constraints: Vec::new(),
        selection: crate::game::ability_utils::begin_target_selection(
            &[crate::types::game_state::TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
            }],
            &[],
        )
        .unwrap(),
        mode_labels: Vec::new(),
        source_id: None,
        description: None,
    };

    // Player selects target1
    let result = apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(target1)],
        },
    )
    .unwrap();

    // Should return Priority
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { .. }),
        "Expected Priority, got {:?}",
        result.waiting_for
    );

    // Trigger should be on the stack with the selected target
    assert_eq!(state.stack.len(), 1, "Trigger should be on stack");
    let entry = &state.stack[0];
    assert_eq!(entry.source_id, trigger_creature);
    match &entry.kind {
        crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
            assert_eq!(ability.targets, vec![TargetRef::Object(target1)]);
        }
        _ => panic!("Expected TriggeredAbility on stack"),
    }

    // Pending trigger should be consumed
    assert!(state.pending_trigger.is_none());
}

#[test]
fn trigger_target_selection_rejects_illegal_target() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);

    let legal_target = ObjectId(10);
    let illegal_target = ObjectId(99);

    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id: ObjectId(1),
        controller: PlayerId(0),
        condition: None,
        ability: crate::types::ability::ResolvedAbility::new(
            Effect::ChangeZone {
                enters_modified_if: None,
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
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
            vec![],
            ObjectId(1),
            PlayerId(0),
        ),
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);

    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: PlayerId(0),
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![crate::types::game_state::TargetSelectionSlot {
            legal_targets: vec![TargetRef::Object(legal_target)],
            optional: false,
        }],
        mode_labels: Vec::new(),
        target_constraints: Vec::new(),
        selection: crate::types::game_state::TargetSelectionProgress::default(),
        source_id: None,
        description: None,
    };

    // Try to select an illegal target
    let result = apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![TargetRef::Object(illegal_target)],
        },
    );

    assert!(result.is_err(), "Should reject illegal target");
}

#[test]
fn triggered_modal_modes_with_targets_wait_for_target_selection() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id: ObjectId(20),
        controller: PlayerId(0),
        condition: None,
        ability: ResolvedAbility::new(
            Effect::Unimplemented {
                name: "modal_placeholder".to_string(),
                description: None,
            },
            vec![],
            ObjectId(20),
            PlayerId(0),
        ),
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: Some(GameEvent::SpellCast {
            controller: PlayerId(0),
            object_id: ObjectId(98),
            card_id: CardId(98),
        }),
        modal: Some(ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 1,
            mode_descriptions: vec!["Deal 1 damage to target player.".to_string()],
            allow_repeat_modes: true,
            constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
            ..Default::default()
        }),
        mode_abilities: vec![AbilityDefinition::new(
            AbilityKind::Database,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
        )],
        description: Some("Choose two target players".to_string()),
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);
    state.waiting_for = WaitingFor::AbilityModeChoice {
        player: PlayerId(0),
        modal: ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 1,
            mode_descriptions: vec!["Deal 1 damage to target player.".to_string()],
            allow_repeat_modes: true,
            constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
            ..Default::default()
        },
        source_id: ObjectId(20),
        mode_abilities: vec![AbilityDefinition::new(
            AbilityKind::Database,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
        )],
        is_activated: false,
        ability_index: None,
        ability_cost: None,
        unavailable_modes: vec![],
    };

    let result = apply_as_current(
        &mut state,
        GameAction::SelectModes {
            indices: vec![0, 0],
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::TriggerTargetSelection {
            target_slots,
            target_constraints,
            ..
        } => {
            assert_eq!(target_slots.len(), 2);
            assert_eq!(
                target_constraints,
                vec![TargetSelectionConstraint::DifferentTargetPlayers]
            );
        }
        other => panic!("Expected TriggerTargetSelection, got {other:?}"),
    }
    // CR 603.3c + CR 603.3d "Push first": after mode chosen, the trigger
    // entry remains on the stack in mid-construction (target prompt
    // pending). `pending_trigger_entry` still identifies it.
    assert_eq!(state.stack.len(), 1);
    assert!(state.pending_trigger.is_some());
    assert!(state.pending_trigger_entry.is_some());
}

#[test]
fn triggered_modal_modes_without_targets_consume_pending_trigger() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let source_id = ObjectId(21);
    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id,
        controller: PlayerId(0),
        condition: None,
        ability: ResolvedAbility::new(
            Effect::Unimplemented {
                name: "modal_placeholder".to_string(),
                description: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        ),
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: Some(GameEvent::SpellCast {
            controller: PlayerId(0),
            object_id: ObjectId(99),
            card_id: CardId(99),
        }),
        modal: Some(ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["Gain 2 life.".to_string(), "Draw a card.".to_string()],
            allow_repeat_modes: false,
            ..Default::default()
        }),
        mode_abilities: vec![
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ),
        ],
        description: Some("Whenever you cast your second spell each turn".to_string()),
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);
    state.waiting_for = WaitingFor::AbilityModeChoice {
        player: PlayerId(0),
        modal: ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["Gain 2 life.".to_string(), "Draw a card.".to_string()],
            allow_repeat_modes: false,
            ..Default::default()
        },
        source_id,
        mode_abilities: vec![
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ),
        ],
        is_activated: false,
        ability_index: None,
        ability_cost: None,
        unavailable_modes: vec![],
    };

    let result =
        apply_as_current(&mut state, GameAction::SelectModes { indices: vec![0] }).unwrap();

    assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.pending_trigger.is_none());
    assert_eq!(state.stack.len(), 1);
    match &state.stack[0].kind {
        crate::types::game_state::StackEntryKind::TriggeredAbility {
            ability,
            trigger_event,
            description,
            ..
        } => {
            assert!(matches!(ability.effect, Effect::GainLife { .. }));
            assert!(matches!(trigger_event, Some(GameEvent::SpellCast { .. })));
            assert_eq!(
                description.as_deref(),
                Some("Whenever you cast your second spell each turn")
            );
        }
        other => panic!("expected triggered ability on stack, got {other:?}"),
    }
}

#[test]
fn triggered_commander_modal_cap_uses_controller_board_state() {
    let mut state = GameState::new_two_player(42);
    let source_id = ObjectId(22);
    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id,
        controller: PlayerId(0),
        condition: None,
        ability: ResolvedAbility::new(
            Effect::Unimplemented {
                name: "modal_placeholder".to_string(),
                description: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        ),
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: Some(ModalChoice {
            min_choices: 1,
            max_choices: 2,
            mode_count: 2,
            mode_descriptions: vec!["Create a token.".to_string(), "Put a counter.".to_string()],
            constraints: vec![ModalSelectionConstraint::ConditionalMaxChoices {
                condition: crate::types::ability::ModalSelectionCondition::Static {
                    condition: StaticCondition::ControlsCommander {
                        ownership: crate::types::ability::CommanderOwnership::Any,
                    },
                },
                max_choices: 2,
                otherwise_max_choices: 1,
            }],
            ..Default::default()
        }),
        mode_abilities: vec![
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ),
        ],
        description: Some("Choose one or both with commander".to_string()),
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);

    let waiting = begin_pending_trigger_target_selection(&mut state)
        .unwrap()
        .expect("modal choice should be required");
    match waiting {
        WaitingFor::AbilityModeChoice { modal, .. } => {
            assert_eq!(modal.max_choices, 1);
        }
        other => panic!("expected AbilityModeChoice, got {other:?}"),
    }

    let commander_id = create_object(
        &mut state,
        CardId(99),
        PlayerId(0),
        "Commander".to_string(),
        Zone::Battlefield,
    );
    state.objects.get_mut(&commander_id).unwrap().is_commander = true;
    let waiting = begin_pending_trigger_target_selection(&mut state)
        .unwrap()
        .expect("modal choice should still be required");
    match waiting {
        WaitingFor::AbilityModeChoice { modal, .. } => {
            assert_eq!(modal.max_choices, 2);
        }
        other => panic!("expected AbilityModeChoice, got {other:?}"),
    }
}

#[test]
fn trigger_target_selection_enforces_different_player_constraint() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id: ObjectId(30),
        controller: PlayerId(0),
        condition: None,
        ability: crate::types::ability::ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(30),
            PlayerId(0),
        )
        .sub_ability(crate::types::ability::ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(30),
            PlayerId(0),
        )),
        timestamp: 1,
        target_constraints: vec![TargetSelectionConstraint::DifferentTargetPlayers],
        distribute: None,
        trigger_event: None,
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);
    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: PlayerId(0),
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![
            crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ],
        mode_labels: Vec::new(),
        target_constraints: vec![TargetSelectionConstraint::DifferentTargetPlayers],
        selection: crate::types::game_state::TargetSelectionProgress::default(),
        source_id: None,
        description: None,
    };

    let invalid = apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Player(PlayerId(1)),
            ],
        },
    );
    assert!(invalid.is_err(), "same player should be rejected");

    let valid = apply_as_current(
        &mut state,
        GameAction::SelectTargets {
            targets: vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
        },
    )
    .unwrap();

    assert!(matches!(valid.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.len(), 1);
    match &state.stack[0].kind {
        crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
            assert_eq!(
                crate::game::ability_utils::flatten_targets_in_chain(ability),
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ]
            );
        }
        other => panic!("expected triggered ability on stack, got {other:?}"),
    }
}

#[test]
fn choose_target_action_advances_trigger_selection_from_engine_state() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let target_slots = vec![
        crate::types::game_state::TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            optional: false,
        },
        crate::types::game_state::TargetSelectionSlot {
            legal_targets: vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            optional: false,
        },
    ];
    let target_constraints = vec![TargetSelectionConstraint::DifferentTargetPlayers];
    let trigger_event = GameEvent::DamageDealt {
        source_id: ObjectId(31),
        target: TargetRef::Object(ObjectId(99)),
        amount: 3,
        is_combat: true,
        excess: 0,
    };
    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id: ObjectId(31),
        controller: PlayerId(0),
        condition: None,
        ability: crate::types::ability::ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(31),
            PlayerId(0),
        )
        .sub_ability(crate::types::ability::ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
            vec![],
            ObjectId(31),
            PlayerId(0),
        )),
        timestamp: 1,
        target_constraints: target_constraints.clone(),
        distribute: None,
        trigger_event: Some(trigger_event.clone()),
        modal: None,
        mode_abilities: vec![],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);
    state.waiting_for = WaitingFor::TriggerTargetSelection {
        player: PlayerId(0),
        trigger_controller: Some(PlayerId(0)),
        trigger_event: Some(trigger_event.clone()),
        trigger_events: vec![trigger_event.clone()],
        target_slots: target_slots.clone(),
        mode_labels: Vec::new(),
        target_constraints: target_constraints.clone(),
        selection: crate::game::ability_utils::begin_target_selection(
            &target_slots,
            &target_constraints,
        )
        .unwrap(),
        source_id: None,
        description: None,
    };

    let intermediate = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Player(PlayerId(0))),
        },
    )
    .unwrap();

    match intermediate.waiting_for {
        WaitingFor::TriggerTargetSelection {
            trigger_controller,
            trigger_event: prompt_event,
            trigger_events,
            selection,
            ..
        } => {
            assert_eq!(trigger_controller, Some(PlayerId(0)));
            assert_eq!(prompt_event, Some(trigger_event.clone()));
            assert_eq!(trigger_events, vec![trigger_event]);
            assert_eq!(selection.current_slot, 1);
            assert_eq!(
                selection.current_legal_targets,
                vec![TargetRef::Player(PlayerId(1))]
            );
        }
        other => panic!("expected trigger target selection, got {other:?}"),
    }

    let completed = apply_as_current(
        &mut state,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Player(PlayerId(1))),
        },
    )
    .unwrap();

    assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(state.stack.len(), 1);
}

#[test]
fn triggered_modal_modes_reject_unsatisfiable_target_constraints() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id: ObjectId(40),
        controller: PlayerId(0),
        condition: None,
        ability: ResolvedAbility::new(
            Effect::Unimplemented {
                name: "modal_placeholder".to_string(),
                description: None,
            },
            vec![],
            ObjectId(40),
            PlayerId(0),
        ),
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: Some(GameEvent::SpellCast {
            controller: PlayerId(0),
            object_id: ObjectId(97),
            card_id: CardId(97),
        }),
        modal: Some(ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 1,
            mode_descriptions: vec!["Target opponent reveals their hand.".to_string()],
            allow_repeat_modes: true,
            constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
            ..Default::default()
        }),
        mode_abilities: vec![AbilityDefinition::new(
            AbilityKind::Database,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                damage_source: None,
                excess: None,
            },
        )],
        description: Some("Choose different target players".to_string()),
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);
    state.waiting_for = WaitingFor::AbilityModeChoice {
        player: PlayerId(0),
        modal: ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 1,
            mode_descriptions: vec!["Target opponent reveals their hand.".to_string()],
            allow_repeat_modes: true,
            constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
            ..Default::default()
        },
        source_id: ObjectId(40),
        mode_abilities: vec![AbilityDefinition::new(
            AbilityKind::Database,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
                damage_source: None,
                excess: None,
            },
        )],
        is_activated: false,
        ability_index: None,
        ability_cost: None,
        unavailable_modes: vec![],
    };

    let result = apply_as_current(
        &mut state,
        GameAction::SelectModes {
            indices: vec![0, 0],
        },
    );

    assert!(
        result.is_err(),
        "unsatisfiable target constraints should be rejected"
    );
}

#[test]
fn all_modes_exhausted_clears_pending_trigger() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let source_id = ObjectId(50);
    let modal = ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        mode_descriptions: vec!["Mode A".to_string(), "Mode B".to_string()],
        constraints: vec![ModalSelectionConstraint::NoRepeatThisTurn],
        ..Default::default()
    };

    // Mark both modes as already chosen this turn.
    state.modal_modes_chosen_this_turn.insert((source_id, 0));
    state.modal_modes_chosen_this_turn.insert((source_id, 1));

    // CR 603.3c + CR 603.3d "Push first" contract migration.
    let pending = crate::game::triggers::PendingTrigger {
        source_id,
        controller: PlayerId(0),
        condition: None,
        ability: ResolvedAbility::new(
            Effect::Unimplemented {
                name: "placeholder".to_string(),
                description: None,
            },
            vec![],
            source_id,
            PlayerId(0),
        ),
        timestamp: 1,
        target_constraints: Vec::new(),
        distribute: None,
        trigger_event: None,
        modal: Some(modal),
        mode_abilities: vec![
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 4 },
                    player: crate::types::ability::TargetFilter::Controller,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Database,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: crate::types::ability::TargetFilter::Controller,
                },
            ),
        ],
        description: None,
        may_trigger_origin: None,
        subject_match_count: None,
        die_result: None,
    };
    let pending_for_state = pending.clone();
    let stack_before = state.stack.len();
    let mut setup_events = Vec::new();
    let entry_id = crate::game::triggers::push_pending_trigger_to_stack(
        &mut state,
        pending,
        &mut setup_events,
    );
    state.pending_trigger = Some(pending_for_state);
    state.pending_trigger_entry = Some(entry_id);

    // Call the private function via the engine path.
    let result = begin_pending_trigger_target_selection(&mut state).unwrap();

    // CR 700.2b + CR 603.3c: All modes exhausted — no AbilityModeChoice
    // produced, defensive cleanup pops the in-construction entry and
    // clears both `pending_trigger` and `pending_trigger_entry`.
    assert!(result.is_none());
    assert!(state.pending_trigger.is_none());
    assert!(state.pending_trigger_entry.is_none());
    assert_eq!(
        state.stack.len(),
        stack_before,
        "defensive cleanup must pop the in-construction entry",
    );
}

#[test]
fn modal_mode_tracking_resets_on_new_turn() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 1;
    state.phase = Phase::PreCombatMain;

    let source_id = ObjectId(50);
    state.modal_modes_chosen_this_turn.insert((source_id, 0));
    state.modal_modes_chosen_this_turn.insert((source_id, 1));
    state.modal_modes_chosen_this_game.insert((source_id, 0));

    // Simulate new turn.
    let mut events = Vec::new();
    super::turns::start_next_turn(&mut state, &mut events);

    // Turn-scoped should be cleared.
    assert!(state.modal_modes_chosen_this_turn.is_empty());
    // Game-scoped should persist.
    assert!(state.modal_modes_chosen_this_game.contains(&(source_id, 0)));
}
