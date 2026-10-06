use super::*;
use crate::game::zones::create_object;
use crate::types::game_state::{ExileLink, ExileLinkKind, ZoneChangeRecord};
use crate::types::identifiers::CardId;

#[test]
fn exile_return_source_leaves_battlefield_returns_exiled_card() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Create source permanent (e.g., Banishing Light) on battlefield
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Banishing Light".to_string(),
        Zone::Battlefield,
    );

    // Create exiled card -- directly in exile
    let exiled_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Exiled Creature".to_string(),
        Zone::Exile,
    );

    // Set up the exile link (exiled from battlefield)
    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    // Simulate events where source leaves the battlefield
    let events = vec![GameEvent::ZoneChanged {
        object_id: source_id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            name: "Banishing Light".to_string(),
            ..ZoneChangeRecord::test_minimal(source_id, Some(Zone::Battlefield), Zone::Graveyard)
        }),
    }];

    // Call check_exile_returns
    check_exile_returns(&mut state, &mut events.clone());

    // CR 610.3a: Exiled card should return to its previous zone (battlefield)
    assert!(
        state.battlefield.contains(&exiled_id),
        "Exiled card should return to battlefield"
    );
    assert!(
        !state.exile.contains(&exiled_id),
        "Exiled card should no longer be in exile"
    );

    // ExileLink should be removed
    assert!(
        state.exile_links.is_empty(),
        "ExileLink should be cleaned up"
    );
}

// #783: end-to-end integration. Component tests cover link creation and the
// return in isolation; this drives the WHOLE flow — exile via the real
// change_zone resolver (which must create the UntilSourceLeaves link), then
// the host actually leaves the battlefield via move_to_zone, then
// check_exile_returns runs on that event batch. The exiled permanent must
// return. CR 610.3a.
#[test]
fn exile_until_host_leaves_returns_card_through_full_pipeline() {
    use crate::game::effects::change_zone;
    use crate::game::zones::move_to_zone;
    use crate::types::ability::{Duration, Effect, ResolvedAbility, TargetFilter, TargetRef};
    use crate::types::card_type::CoreType;

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.active_player = PlayerId(0);

    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Banishing Light".to_string(),
        Zone::Battlefield,
    );
    let victim_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Opponent's Bear".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&victim_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    // "exile target nonland permanent ... until this enchantment leaves the
    // battlefield" — exile resolves and must register the return link.
    let mut exile = ResolvedAbility::new(
        Effect::ChangeZone {
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
            enters_modified_if: None,
        },
        vec![TargetRef::Object(victim_id)],
        source_id,
        PlayerId(0),
    );
    exile.duration = Some(Duration::UntilHostLeavesPlay);

    let mut events = Vec::new();
    change_zone::resolve(&mut state, &exile, &mut events).unwrap();
    assert!(state.exile.contains(&victim_id), "victim should be exiled");
    assert_eq!(state.exile_links.len(), 1, "exile link must be created");

    // Host leaves the battlefield (e.g. destroyed or sacrificed).
    let mut leave_events = Vec::new();
    move_to_zone(&mut state, source_id, Zone::Graveyard, &mut leave_events);
    check_exile_returns(&mut state, &mut leave_events);

    assert!(
        state.battlefield.contains(&victim_id),
        "#783: exiled permanent must return when the host leaves the battlefield"
    );
    assert!(
        !state.exile.contains(&victim_id),
        "returned permanent must no longer be in exile"
    );
}

/// CR 610.3a: When a card exiled from hand (e.g., Deep-Cavern Bat) is returned,
/// it goes back to hand, not to the battlefield.
#[test]
fn exile_return_to_hand_when_exiled_from_hand() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Deep-Cavern Bat".to_string(),
        Zone::Battlefield,
    );

    let exiled_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Exiled From Hand".to_string(),
        Zone::Exile,
    );

    // Exiled from hand → should return to hand
    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Hand,
        },
    });

    let events = vec![GameEvent::ZoneChanged {
        object_id: source_id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            name: "Deep-Cavern Bat".to_string(),
            ..ZoneChangeRecord::test_minimal(source_id, Some(Zone::Battlefield), Zone::Graveyard)
        }),
    }];

    check_exile_returns(&mut state, &mut events.clone());

    // CR 610.3a: Card returns to hand, NOT battlefield
    assert!(
        state.players[1].hand.contains(&exiled_id),
        "Card exiled from hand should return to hand"
    );
    assert!(
        !state.battlefield.contains(&exiled_id),
        "Card exiled from hand should NOT go to battlefield"
    );
    assert!(
        !state.exile.contains(&exiled_id),
        "Card should no longer be in exile"
    );
    assert!(state.exile_links.is_empty());
}

#[test]
fn exile_return_card_already_gone_no_error() {
    let mut state = GameState::new_two_player(42);

    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );

    // Exiled card that has already left exile (moved to hand by another effect)
    let exiled_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Already Moved".to_string(),
        Zone::Hand,
    );

    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let events = vec![GameEvent::ZoneChanged {
        object_id: source_id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            name: "Source".to_string(),
            ..ZoneChangeRecord::test_minimal(source_id, Some(Zone::Battlefield), Zone::Graveyard)
        }),
    }];

    // Should not panic -- gracefully handle already-moved card
    check_exile_returns(&mut state, &mut events.clone());

    // Card stays in hand (not moved)
    assert!(state.players[1].hand.contains(&exiled_id));
    // Link is still cleaned up
    assert!(state.exile_links.is_empty());
}

#[test]
fn exile_return_link_removed_after_return() {
    let mut state = GameState::new_two_player(42);

    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Source".to_string(),
        Zone::Battlefield,
    );

    let exiled_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Exiled".to_string(),
        Zone::Exile,
    );

    // Another unrelated exile link that should NOT be removed
    let other_source = create_object(
        &mut state,
        CardId(3),
        PlayerId(0),
        "Other Source".to_string(),
        Zone::Battlefield,
    );
    let other_exiled = create_object(
        &mut state,
        CardId(4),
        PlayerId(1),
        "Other Exiled".to_string(),
        Zone::Exile,
    );

    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });
    state.exile_links.push(ExileLink {
        exiled_id: other_exiled,
        source_id: other_source,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let events = vec![GameEvent::ZoneChanged {
        object_id: source_id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            name: "Source".to_string(),
            ..ZoneChangeRecord::test_minimal(source_id, Some(Zone::Battlefield), Zone::Graveyard)
        }),
    }];

    check_exile_returns(&mut state, &mut events.clone());

    // First link's exiled card should return, second should stay in exile
    assert!(state.battlefield.contains(&exiled_id));
    assert!(state.exile.contains(&other_exiled));

    // Only the triggered link should be removed
    assert_eq!(state.exile_links.len(), 1);
    assert_eq!(state.exile_links[0].exiled_id, other_exiled);
}

/// CR 400.7 + CR 610.3a: End-to-end — when the source permanent of an
/// `UntilHostLeavesPlay` exile leaves the battlefield through the real
/// reducer pipeline (move_to_zone → post-action pipeline), the exiled
/// card must return to its previous zone. Regression test for White
/// Auracite / Oblivion Ring / Banishing Light class.
#[test]
fn exile_return_end_to_end_through_pipeline() {
    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Source permanent (e.g., White Auracite) on P0's battlefield
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "White Auracite".to_string(),
        Zone::Battlefield,
    );

    // Opponent's enchantment on battlefield, then exiled by the source
    let exiled_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Opponent Enchantment".to_string(),
        Zone::Exile,
    );

    // Register the UntilSourceLeaves link as if the trigger had resolved
    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    // Destroy the source via move_to_zone, then run the post-action pipeline
    // (mirrors what happens when an SBA or destroy effect runs during apply).
    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(&mut state, source_id, Zone::Graveyard, &mut events);

    let default_wf = WaitingFor::Priority {
        player: PlayerId(0),
    };
    crate::game::engine_priority::run_post_action_pipeline(
        &mut state,
        &mut events,
        &default_wf,
        true,
        false,
    )
    .unwrap();

    // Exiled card must have returned to battlefield
    assert!(
        state.battlefield.contains(&exiled_id),
        "Exiled card should return to battlefield when source leaves; battlefield={:?}, exile={:?}",
        state.battlefield,
        state.exile,
    );
    assert!(!state.exile.contains(&exiled_id));
    assert!(
        state.exile_links.is_empty(),
        "ExileLink should be consumed after return"
    );
}

/// CR 730.3c: An "exile until this leaves" effect (Banisher Priest, Banishing
/// Light, Oblivion Ring) that exiles a MERGED Mutate permanent must, when it
/// leaves and its `UntilSourceLeaves` return fires, bring back ALL of the
/// component cards the merged permanent split into — not just the tracked
/// survivor. Regression test for the implicit-return path (companion to the
/// flicker/`ChangeZone` path covered in `merge_tests`).
#[test]
fn until_source_leaves_return_brings_back_all_merge_components() {
    use crate::game::merge::{merge_object_onto, MergeSide};

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // The "O-Ring" source on P0's battlefield.
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Banishing Light".to_string(),
        Zone::Battlefield,
    );
    // A merged Mutate permanent: host (survivor) + rider (absorbed component).
    let host = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Host".to_string(),
        Zone::Battlefield,
    );
    let rider = create_object(
        &mut state,
        CardId(3),
        PlayerId(1),
        "Rider".to_string(),
        Zone::Battlefield,
    );
    let mut events: Vec<GameEvent> = Vec::new();
    merge_object_onto(&mut state, rider, host, MergeSide::Top, &mut events);
    // Runtime invariant: the mutating spell resolved off the stack, so the
    // absorbed component is not an independent member of the battlefield list.
    state.battlefield.retain(|&id| id != rider);

    // The source exiles the merged permanent; the survivor is the tracked,
    // exile-linked object (the component is split out alongside it).
    crate::game::zones::move_to_zone(&mut state, host, Zone::Exile, &mut events);
    assert_eq!(state.objects.get(&host).unwrap().zone, Zone::Exile);
    assert_eq!(state.objects.get(&rider).unwrap().zone, Zone::Exile);
    state.exile_links.push(ExileLink {
        exiled_id: host,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    // The source leaves the battlefield → the implicit return fires.
    events.clear();
    crate::game::zones::move_to_zone(&mut state, source_id, Zone::Graveyard, &mut events);
    let default_wf = WaitingFor::Priority {
        player: PlayerId(0),
    };
    crate::game::engine_priority::run_post_action_pipeline(
        &mut state,
        &mut events,
        &default_wf,
        true,
        false,
    )
    .unwrap();

    // CR 730.3c: BOTH the survivor and the component card return — as separate,
    // non-merged objects — not just the survivor.
    for id in [host, rider] {
        assert!(
                state.battlefield.contains(&id),
                "component {id:?} must return to the battlefield (CR 730.3c); battlefield={:?}, exile={:?}",
                state.battlefield,
                state.exile,
            );
        let o = state.objects.get(&id).unwrap();
        assert!(
            o.merged_components.is_empty(),
            "returns un-merged (CR 730.3)"
        );
        assert_eq!(
            o.split_from_merge_survivor, None,
            "the survivor back-link clears on battlefield entry"
        );
    }
    assert!(!state.exile.contains(&host) && !state.exile.contains(&rider));
}

/// CR 400.7 + CR 610.3a: End-to-end through full apply path — cast a
/// Destroy spell targeting the source, resolve it, verify the exiled
/// card returns. Regression test for the White Auracite user report.
#[test]
fn exile_return_after_destroy_resolution_via_apply() {
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(1);
    state.priority_player = PlayerId(1);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };

    // P0 controls White Auracite (source)
    let source_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "White Auracite".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&source_id)
        .unwrap()
        .card_types
        .core_types
        .push(crate::types::card_type::CoreType::Artifact);

    // The opponent's enchantment that WA exiled
    let exiled_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Opponent Enchantment".to_string(),
        Zone::Exile,
    );

    // Link: UntilSourceLeaves → Battlefield
    state.exile_links.push(ExileLink {
        exiled_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    // P1 casts a Destroy ability targeting WA: push ResolvedAbility with
    // Effect::Destroy onto the stack and resolve it via resolve_top.
    let _ = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        },
    );
    let destroy_ability = crate::types::ability::ResolvedAbility::new(
        Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        },
        vec![crate::types::ability::TargetRef::Object(source_id)],
        ObjectId(999),
        PlayerId(1),
    )
    .kind(AbilityKind::Spell);

    let spell_obj = create_object(
        &mut state,
        CardId(99),
        PlayerId(1),
        "Disenchant".to_string(),
        Zone::Stack,
    );

    state.stack.push_back(crate::types::game_state::StackEntry {
        id: spell_obj,
        source_id: spell_obj,
        controller: PlayerId(1),
        kind: crate::types::game_state::StackEntryKind::Spell {
            ability: Some(Box::new(destroy_ability)),
            card_id: CardId(99),
            casting_variant: crate::types::game_state::CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });

    // Resolve the top stack entry
    let mut events = Vec::new();
    crate::game::stack::resolve_top(&mut state, &mut events);

    // Run the post-action pipeline exactly as apply() would
    let default_wf = WaitingFor::Priority {
        player: PlayerId(1),
    };
    crate::game::engine_priority::run_post_action_pipeline(
        &mut state,
        &mut events,
        &default_wf,
        false,
        false,
    )
    .unwrap();

    // White Auracite should be destroyed
    assert!(
        state.players[0].graveyard.contains(&source_id),
        "White Auracite should be in graveyard"
    );
    // Exiled enchantment should have returned to battlefield
    assert!(
        state.battlefield.contains(&exiled_id),
        "Exiled enchantment should return to battlefield; battlefield={:?}, exile={:?}",
        state.battlefield,
        state.exile,
    );
    assert!(!state.exile.contains(&exiled_id));
    assert!(state.exile_links.is_empty());
}

/// CR 610.3a: an exile-until-source-leaves return happens immediately even if
/// the effect that removed the source pauses on a later resolution choice.
#[test]
fn exile_return_occurs_before_a_pending_resolution_choice() {
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::game::zones::move_to_zone;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source_id = scenario
        .add_creature(P0, "Leyline Binding", 0, 1)
        .as_enchantment()
        .id();
    let returned_id = scenario
        .add_creature(P1, "Resolute Reinforcements", 1, 1)
        .from_oracle_text("When this creature enters, create a 1/1 white Soldier creature token.")
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    let mut setup_events = Vec::new();
    move_to_zone(state, returned_id, Zone::Exile, &mut setup_events);
    state.exile_links.push(ExileLink {
        exiled_id: returned_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let mut events = Vec::new();
    move_to_zone(state, source_id, Zone::Graveyard, &mut events);
    state.waiting_for = WaitingFor::OptionalEffectChoice {
        player: PlayerId(0),
        decision_subject_id: None,
        source_id,
        description: Some("Search your library for a land card".to_string()),
        may_trigger_key: None,
        same_card_may_trigger_choice_available: false,
    };
    let default_wf = WaitingFor::Priority {
        player: PlayerId(1),
    };
    let waiting_for = crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut events,
        &default_wf,
        false,
        false,
    )
    .unwrap();

    assert!(matches!(
        waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    assert!(state.battlefield.contains(&returned_id));
    assert!(!state.exile.contains(&returned_id));
    assert!(state.exile_links.is_empty());
    assert_eq!(state.deferred_triggers.len(), 1);

    state.waiting_for = WaitingFor::SearchChoice {
        player: PlayerId(0),
        library_owner: None,
        cards: Vec::new(),
        count: 1,
        reveal: false,
        up_to: true,
        allows_partial_find: true,
        constraint: Default::default(),
        ordering_hint: Default::default(),
        split: None,
    };
    let mut search_events = Vec::new();
    let waiting_for = crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut search_events,
        &default_wf,
        false,
        false,
    )
    .unwrap();
    assert!(matches!(waiting_for, WaitingFor::SearchChoice { .. }));
    assert_eq!(state.deferred_triggers.len(), 1);

    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(1),
    };
    let mut settle_events = Vec::new();
    crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut settle_events,
        &default_wf,
        false,
        false,
    )
    .unwrap();
    assert!(state
        .stack
        .iter()
        .any(|entry| entry.source_id == returned_id));
    let mut trigger_events = Vec::new();
    crate::game::stack::resolve_top(state, &mut trigger_events);
    assert!(state
        .battlefield
        .iter()
        .any(|id| state.objects[id].name == "Soldier"));
}

/// CR 400.7 + CR 610.3a + CR 611.2: Full integration test using the real
/// parsed Oracle text for White Auracite. Exercises the complete pipeline:
/// parser → trigger.execute (with Duration::UntilHostLeavesPlay) →
/// build_resolved_from_def → stack resolution → execute_zone_move
/// (which must register the ExileLink) → destroy source → post-action
/// pipeline → check_exile_returns → return to battlefield.
///
/// Regression test for the L4-18 user report: White Auracite's exiled
/// enchantment was not returning when White Auracite itself was destroyed.
#[test]
fn white_auracite_real_oracle_text_returns_exiled_card() {
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::types::ability::TargetRef;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::StackEntry;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // White Auracite on P0's battlefield, with its real parsed triggers.
    let wa_id = scenario
        .add_creature(P0, "White Auracite", 0, 0)
        .as_artifact()
        .from_oracle_text(
            "When this artifact enters, exile target nonland permanent an opponent \
                 controls until this artifact leaves the battlefield.\n{T}: Add {W}.",
        )
        .id();

    // Opponent's enchantment on battlefield (the one WA will exile).
    let ench_id = scenario
        .add_creature(P1, "Opponent Enchantment", 0, 0)
        .as_enchantment()
        .id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Sanity-check the parser: WA must have an ETB trigger whose execute
    // ability carries Duration::UntilHostLeavesPlay on a ChangeZone to
    // Exile. If this fails, the parser regressed, not the engine.
    let wa = state.objects.get(&wa_id).expect("WA on battlefield");
    let etb_trigger = wa
        .trigger_definitions
        .iter_all()
        .find(|t| {
            matches!(t.definition.mode, crate::types::TriggerMode::ChangesZone)
                && t.definition.destination == Some(Zone::Battlefield)
        })
        .expect("WA must have an ETB (ChangesZone to Battlefield) trigger");
    let execute_def = etb_trigger
        .definition
        .execute
        .as_deref()
        .expect("trigger.execute");
    assert_eq!(
        execute_def.duration,
        Some(crate::types::ability::Duration::UntilHostLeavesPlay),
        "parser regression: WA's exile trigger must carry UntilHostLeavesPlay"
    );
    assert!(
        matches!(
            &*execute_def.effect,
            crate::types::ability::Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ),
        "parser regression: WA's trigger effect must be ChangeZone→Exile"
    );

    // Build a ResolvedAbility from the real parsed execute and pre-populate
    // its target with the opponent's enchantment. This bypasses the target
    // selection UX but exercises every downstream code path (ability
    // duration threading, execute_zone_move, exile link creation,
    // check_exile_returns). The parser / targeting is tested separately.
    let mut resolved = build_resolved_from_def(execute_def, wa_id, PlayerId(0));
    resolved.targets = vec![TargetRef::Object(ench_id)];

    // Push a TriggeredAbility stack entry that mirrors what
    // push_pending_trigger_to_stack would create.
    let stack_id = ObjectId(9_000_000);
    state.stack.push_back(StackEntry {
        id: stack_id,
        source_id: wa_id,
        controller: PlayerId(0),
        kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
            source_id: wa_id,
            ability: Box::new(resolved),
            description: Some("When WA enters...".to_string()),
            condition: None,
            trigger_event: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    });

    // Resolve the trigger: WA's target enchantment moves to exile and the
    // ExileLink for UntilSourceLeaves must be created.
    let mut events = Vec::new();
    crate::game::stack::resolve_top(state, &mut events);

    assert!(
        state.exile.contains(&ench_id),
        "opponent enchantment must be in exile after trigger resolves"
    );
    let has_link = state.exile_links.iter().any(|link| {
        link.exiled_id == ench_id
            && link.source_id == wa_id
            && matches!(
                link.kind,
                crate::types::game_state::ExileLinkKind::UntilSourceLeaves {
                    return_zone: Zone::Battlefield
                }
            )
    });
    assert!(
        has_link,
        "execute_zone_move must register an UntilSourceLeaves link; exile_links={:?}",
        state.exile_links
    );

    // Now destroy White Auracite via move_to_zone and run the full
    // post-action pipeline exactly as apply() would.
    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(state, wa_id, Zone::Graveyard, &mut events);

    let default_wf = WaitingFor::Priority {
        player: PlayerId(0),
    };
    crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut events,
        &default_wf,
        false,
        false,
    )
    .unwrap();

    // Confirm WA is in graveyard and the exiled enchantment has returned.
    assert!(
        state.players[0].graveyard.contains(&wa_id),
        "White Auracite should be in graveyard"
    );
    // The returned enchantment must be on the battlefield under its owner's
    // control (CR 400.7a).
    assert!(
        state.battlefield.contains(&ench_id),
        "exiled enchantment should return to battlefield; battlefield={:?}, exile={:?}",
        state.battlefield,
        state.exile,
    );
    assert!(!state.exile.contains(&ench_id));
    assert!(
        state.exile_links.is_empty(),
        "ExileLink should be consumed after return; remaining={:?}",
        state.exile_links
    );
    let returned = state.objects.get(&ench_id).unwrap();
    assert!(
        returned
            .card_types
            .core_types
            .contains(&CoreType::Enchantment),
        "returned object must still be an enchantment"
    );
}

/// CR 607.1 + CR 610.3 + #881: Haytham Kenway — per-opponent multi-target exile
/// with Duration::UntilHostLeavesPlay. Exiles one creature per opponent using
/// the per-opponent fanout targeting mechanism; ExileLinks are created for each;
/// all return when the source leaves the battlefield.
#[test]
fn haytham_kenway_per_opponent_exile_returns_when_source_leaves() {
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::types::ability::TargetRef;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Haytham Kenway on P0's battlefield with his real parsed oracle text.
    let haytham_id = scenario
        .add_creature(P0, "Haytham Kenway", 3, 3)
        .from_oracle_text(
            "When this creature enters, for each opponent, exile up to one target \
                 creature that player controls until this creature leaves the battlefield.",
        )
        .id();

    // Opponent's creature to be exiled.
    let victim_id = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Verify parser: ETB trigger must have UntilHostLeavesPlay on the exile execute.
    let haytham = state
        .objects
        .get(&haytham_id)
        .expect("Haytham on battlefield");
    let etb = haytham
        .trigger_definitions
        .iter_all()
        .find(|t| {
            matches!(t.definition.mode, crate::types::TriggerMode::ChangesZone)
                && t.definition.destination == Some(Zone::Battlefield)
        })
        .expect("Haytham must have ETB trigger");
    let execute_def = etb
        .definition
        .execute
        .as_deref()
        .expect("ETB must have execute");
    assert_eq!(
        execute_def.duration,
        Some(crate::types::ability::Duration::UntilHostLeavesPlay),
        "Haytham ETB exile must carry UntilHostLeavesPlay"
    );

    // Build the resolved exile effect with the opponent's creature as a target.
    // The per-opponent fanout produces [Player(P1), Object(victim)] target pairs;
    // we simulate the post-selection ability.targets state.
    let mut resolved = build_resolved_from_def(execute_def, haytham_id, PlayerId(0));
    resolved.targets = vec![TargetRef::Player(PlayerId(1)), TargetRef::Object(victim_id)];

    // Push and resolve the trigger.
    let stack_id = ObjectId(9_000_001);
    state.stack.push_back(crate::types::game_state::StackEntry {
        id: stack_id,
        source_id: haytham_id,
        controller: PlayerId(0),
        kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
            source_id: haytham_id,
            ability: Box::new(resolved),
            description: Some("When Haytham enters...".to_string()),
            condition: None,
            trigger_event: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    });

    let mut events = Vec::new();
    crate::game::stack::resolve_top(state, &mut events);

    assert!(
        state.exile.contains(&victim_id),
        "creature must be in exile"
    );
    let has_link = state.exile_links.iter().any(|link| {
        link.exiled_id == victim_id
            && link.source_id == haytham_id
            && matches!(
                link.kind,
                crate::types::game_state::ExileLinkKind::UntilSourceLeaves {
                    return_zone: Zone::Battlefield
                }
            )
    });
    assert!(
        has_link,
        "UntilSourceLeaves exile link must be created; exile_links={:?}",
        state.exile_links
    );

    // Haytham Kenway leaves the battlefield (dies, bounced, etc.).
    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(state, haytham_id, Zone::Graveyard, &mut events);
    crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut events,
        &WaitingFor::Priority {
            player: PlayerId(0),
        },
        true,
        false,
    )
    .unwrap();

    assert!(
        state.battlefield.contains(&victim_id),
        "exiled creature must return when Haytham Kenway leaves the battlefield"
    );
    assert!(!state.exile.contains(&victim_id));
    assert!(state.exile_links.is_empty(), "exile link must be consumed");
}

/// CR 607.2a + CR 610.3: Two-trigger exile-return cards link the ETB
/// exile to the LTB return text. Journey to Nowhere has no explicit
/// "until" text on the ETB trigger, so the parser synthesis must still
/// create an `UntilSourceLeaves` exile link for the runtime return path.
#[test]
fn journey_to_nowhere_two_trigger_oracle_returns_exiled_creature() {
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::types::ability::TargetRef;
    use crate::types::game_state::StackEntry;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let journey_id = scenario
        .add_creature(P0, "Journey to Nowhere", 0, 0)
        .as_enchantment()
        .from_oracle_text(
            "When this enchantment enters, exile target creature.\n\
                 When this enchantment leaves the battlefield, return the exiled card \
                 to the battlefield under its owner's control.",
        )
        .id();
    let creature_id = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let journey = state
        .objects
        .get(&journey_id)
        .expect("Journey to Nowhere on battlefield");
    let etb_trigger = journey
        .trigger_definitions
        .iter_all()
        .find(|t| {
            matches!(t.definition.mode, crate::types::TriggerMode::ChangesZone)
                && t.definition.destination == Some(Zone::Battlefield)
        })
        .expect("Journey must have ETB trigger");
    let execute_def = etb_trigger
        .definition
        .execute
        .as_deref()
        .expect("trigger.execute");
    assert_eq!(
        execute_def.duration,
        Some(crate::types::ability::Duration::UntilHostLeavesPlay),
        "parser synthesis must make the ETB exile create an exile link"
    );

    let mut resolved = build_resolved_from_def(execute_def, journey_id, PlayerId(0));
    resolved.targets = vec![TargetRef::Object(creature_id)];

    state.stack.push_back(StackEntry {
        id: ObjectId(9_000_001),
        source_id: journey_id,
        controller: PlayerId(0),
        kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
            source_id: journey_id,
            ability: Box::new(resolved),
            description: Some("When Journey to Nowhere enters...".to_string()),
            condition: None,
            trigger_event: None,
            source_name: String::new(),
            subject_match_count: None,
            die_result: None,
            provenance: None,
        },
    });

    let mut events = Vec::new();
    crate::game::stack::resolve_top(state, &mut events);

    assert!(state.exile.contains(&creature_id));
    assert!(state.exile_links.iter().any(|link| {
        link.exiled_id == creature_id
            && link.source_id == journey_id
            && matches!(
                link.kind,
                crate::types::game_state::ExileLinkKind::UntilSourceLeaves {
                    return_zone: Zone::Battlefield
                }
            )
    }));

    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(state, journey_id, Zone::Graveyard, &mut events);
    let default_wf = WaitingFor::Priority {
        player: PlayerId(0),
    };
    crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut events,
        &default_wf,
        false,
        false,
    )
    .unwrap();

    assert!(state.players[0].graveyard.contains(&journey_id));
    assert!(state.battlefield.contains(&creature_id));
    assert!(!state.exile.contains(&creature_id));
    assert!(state.exile_links.is_empty());
}

/// CR 603.2 + CR 603.3: A creature returned to the battlefield by an
/// "until this leaves" exile-return (Fiend Hunter's leaves-the-battlefield
/// trigger) enters the battlefield, so its OWN enters-the-battlefield trigger
/// must fire. Regression test for issue #3673: Wall of Omens, returned when
/// Fiend Hunter dies, was not drawing a card because `check_exile_returns`
/// appended the enter event without scanning it for triggers.
#[test]
fn exile_return_fires_returned_creatures_etb_trigger() {
    use crate::game::scenario::{GameScenario, P0};
    use crate::types::game_state::StackEntryKind;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Fiend Hunter on P0's battlefield (source of the exile). We register the
    // UntilSourceLeaves link directly rather than resolving Fiend Hunter's ETB
    // exile — the return path, not the exile path, is under test here.
    let hunter_id = scenario.add_creature(P0, "Fiend Hunter", 1, 3).id();

    // Wall of Omens, owned by P0, with its real ETB draw trigger. It is placed
    // on the battlefield so the parser installs the trigger, then relocated to
    // exile below (a returned card comes back under its owner's control).
    let wall_id = scenario
        .add_creature(P0, "Wall of Omens", 0, 4)
        .from_oracle_text("Defender\nWhen this creature enters, draw a card.")
        .id();

    // A card for the ETB draw to reveal.
    scenario.with_library_top(P0, &["Plains"]);

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Sanity-check the parser: Wall of Omens must have an ETB (ChangesZone to
    // Battlefield) trigger. If this fails, the parser regressed, not the engine.
    let wall = state.objects.get(&wall_id).expect("Wall on battlefield");
    assert!(
        wall.trigger_definitions.iter_all().any(|t| {
            matches!(t.definition.mode, crate::types::TriggerMode::ChangesZone)
                && t.definition.destination == Some(Zone::Battlefield)
        }),
        "Wall of Omens must have an ETB trigger for this regression to be meaningful"
    );

    // Move Wall to exile and register the return link, mirroring the state after
    // Fiend Hunter's ETB has resolved.
    let mut relocate_events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(state, wall_id, Zone::Exile, &mut relocate_events);
    state.exile_links.push(ExileLink {
        exiled_id: wall_id,
        source_id: hunter_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let hand_before = state.players[0].hand.len();

    // Fiend Hunter leaves the battlefield; the post-action pipeline returns Wall
    // of Omens and must fire its ETB.
    let mut events: Vec<GameEvent> = Vec::new();
    crate::game::zones::move_to_zone(state, hunter_id, Zone::Graveyard, &mut events);
    let default_wf = WaitingFor::Priority {
        player: PlayerId(0),
    };
    crate::game::engine_priority::run_post_action_pipeline(
        state,
        &mut events,
        &default_wf,
        false,
        false,
    )
    .unwrap();

    // Wall of Omens is back on the battlefield ...
    assert!(
        state.battlefield.contains(&wall_id),
        "Wall of Omens should return to the battlefield"
    );
    assert!(
        state.exile_links.is_empty(),
        "return link should be consumed"
    );

    // ... and its ETB trigger is on the stack (the bug: it never triggered).
    let etb_on_stack = state.stack.iter().any(|entry| {
        matches!(
            &entry.kind,
            StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == wall_id
        )
    });
    assert!(
        etb_on_stack,
        "returned Wall of Omens' ETB trigger must be on the stack; stack={:?}",
        state.stack,
    );

    // Resolving it draws a card for P0.
    let mut resolve_events: Vec<GameEvent> = Vec::new();
    crate::game::stack::resolve_top(state, &mut resolve_events);
    assert_eq!(
        state.players[0].hand.len(),
        hand_before + 1,
        "resolving the returned creature's ETB must draw a card"
    );
}

/// CR 603.2 + CR 603.3b + CR 603.7: If an exile-return event creates ordinary
/// ETB triggers and also satisfies a delayed trigger, all of those simultaneous
/// triggers are ordered as one batch. Regression for PR 5773 review: the return
/// path used to process normal ETBs and delayed triggers in separate batches,
/// so the same-controller ordering prompt could omit one side of the batch.
#[test]
fn exile_return_combines_normal_and_delayed_triggers_in_one_ordering_prompt() {
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, DelayedTriggerCondition, Effect, QuantityExpr,
        ResolvedAbility, TargetFilter, TriggerDefinition,
    };
    use crate::types::game_state::DelayedTrigger;

    fn gain_life_definition(description: &str) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Database,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )
        .description(description.to_string())
    }

    fn etb_observer_trigger(description: &str) -> TriggerDefinition {
        TriggerDefinition::new(crate::types::TriggerMode::ChangesZone)
            .destination(Zone::Battlefield)
            .valid_card(TargetFilter::Any)
            .execute(gain_life_definition(description))
            .description(description.to_string())
    }

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let host_id = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Fiend Hunter".to_string(),
        Zone::Battlefield,
    );
    let returned_id = create_object(
        &mut state,
        CardId(11),
        PlayerId(0),
        "Returned Bear".to_string(),
        Zone::Exile,
    );
    let observer_a = create_object(
        &mut state,
        CardId(12),
        PlayerId(0),
        "First ETB Observer".to_string(),
        Zone::Battlefield,
    );
    let observer_b = create_object(
        &mut state,
        CardId(13),
        PlayerId(0),
        "Second ETB Observer".to_string(),
        Zone::Battlefield,
    );
    let delayed_source = create_object(
        &mut state,
        CardId(14),
        PlayerId(0),
        "Delayed Return Watcher".to_string(),
        Zone::Battlefield,
    );

    state
        .objects
        .get_mut(&observer_a)
        .unwrap()
        .trigger_definitions
        .push(etb_observer_trigger("First observer gains 1 life"));
    state
        .objects
        .get_mut(&observer_b)
        .unwrap()
        .trigger_definitions
        .push(etb_observer_trigger("Second observer gains 1 life"));

    state.delayed_triggers.push(DelayedTrigger {
        condition: DelayedTriggerCondition::WhenEntersBattlefield {
            filter: TargetFilter::Any,
        },
        ability: Box::new(ResolvedAbility::new(
            *gain_life_definition("Delayed watcher gains 1 life").effect,
            vec![],
            delayed_source,
            PlayerId(0),
        )),
        controller: PlayerId(0),
        source_id: delayed_source,
        one_shot: true,
        provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
    });
    state.exile_links.push(ExileLink {
        exiled_id: returned_id,
        source_id: host_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let mut events = Vec::new();
    crate::game::zones::move_to_zone(&mut state, host_id, Zone::Graveyard, &mut events);
    let default_wf = WaitingFor::Priority {
        player: PlayerId(0),
    };
    let waiting_for = crate::game::engine_priority::run_post_action_pipeline(
        &mut state,
        &mut events,
        &default_wf,
        true,
        false,
    )
    .unwrap();

    assert!(
        state.battlefield.contains(&returned_id),
        "returned card must be back on the battlefield"
    );
    assert!(
        state.delayed_triggers.is_empty(),
        "matching one-shot delayed trigger must be consumed"
    );

    let WaitingFor::OrderTriggers { player, triggers } = waiting_for else {
        panic!("expected combined OrderTriggers prompt, got {waiting_for:?}");
    };
    assert_eq!(player, PlayerId(0));
    assert_eq!(
        triggers.len(),
        3,
        "normal ETBs plus delayed return trigger must share one ordering prompt: {triggers:?}"
    );
    assert!(triggers
        .iter()
        .any(|summary| summary.source_id == observer_a));
    assert!(triggers
        .iter()
        .any(|summary| summary.source_id == observer_b));
    assert!(triggers
        .iter()
        .any(|summary| summary.source_id == delayed_source));
}

// ---------------------------------------------------------------------------
// CR 303.4f/303.4g regression: a non-cast Aura re-entering the battlefield must
// be able to find a legal host in a NON-battlefield zone (graveyard/hand) when
// its Enchant ability points there (Animate Dead, Dance of the Dead, Spellweaver
// Volute, Don't Worry About It). Before the `legal_aura_attachment_targets`
// rewrite the candidate scan only looked at the battlefield, so such an Aura
// could never find a host through this path and was stuck in exile forever.
// ---------------------------------------------------------------------------

/// Verbatim Animate Dead Oracle text (matches the repo's canonical corpus form
/// in `crates/engine/tests/fixtures/integration_cards.json.gz`, mirroring the
/// `casting_tests.rs` reanimation fixtures).
const ANIMATE_DEAD_ORACLE_FULL: &str = "Enchant creature card in a graveyard\nWhen this Aura enters, if it's on the battlefield, it loses \"enchant creature card in a graveyard\" and gains \"enchant creature put onto the battlefield with this Aura.\" Return enchanted creature card to the battlefield under your control and attach this Aura to it. When this Aura leaves the battlefield, that creature's controller sacrifices it.\nEnchanted creature gets -1/-0.";

/// Builds a full Animate Dead-class reanimator Aura (real parser output for the
/// ETB reanimation trigger + the graveyard-scoped Enchant keyword) in `zone`.
fn build_reanimator_aura_in_zone(
    state: &mut GameState,
    card_id: CardId,
    controller: PlayerId,
    zone: Zone,
) -> ObjectId {
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::card_type::CoreType;
    use crate::types::keywords::Keyword;
    use std::str::FromStr;
    use std::sync::Arc;

    let aura_id = create_object(state, card_id, controller, "Animate Dead".to_string(), zone);
    let parsed = parse_oracle_text(
        ANIMATE_DEAD_ORACLE_FULL,
        "Animate Dead",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    // Reach-guard: the live parser MUST attach the reanimator ETB trigger, else a
    // downstream reanimation assertion could pass vacuously.
    assert!(
        !parsed.triggers.is_empty(),
        "parser must produce the reanimator ETB trigger; got none"
    );
    let obj = state.objects.get_mut(&aura_id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    obj.base_card_types = obj.card_types.clone();
    let enchant = Keyword::from_str("Enchant:creature card in a graveyard").unwrap();
    obj.base_keywords.push(enchant.clone());
    obj.keywords.push(enchant);
    obj.base_abilities = Arc::new(parsed.abilities.clone());
    obj.abilities = Arc::new(parsed.abilities.clone());
    obj.base_trigger_definitions = Arc::new(parsed.triggers.clone());
    obj.trigger_definitions = parsed.triggers.clone().into();
    obj.base_static_definitions = Arc::new(parsed.statics.clone());
    obj.static_definitions = parsed.statics.clone().into();
    aura_id
}

/// Builds a lightweight Aura (no ETB body) whose Enchant ability is `enchant_spec`
/// (e.g. `"creature"` for a Pacifism-shaped battlefield Aura). Sets both live and
/// base keywords so the ability survives any layer re-derivation during entry.
fn build_simple_aura(
    state: &mut GameState,
    card_id: CardId,
    controller: PlayerId,
    zone: Zone,
    enchant_spec: &str,
) -> ObjectId {
    use crate::types::card_type::CoreType;
    use crate::types::keywords::Keyword;
    use std::str::FromStr;

    let aura_id = create_object(state, card_id, controller, "Test Aura".to_string(), zone);
    let obj = state.objects.get_mut(&aura_id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.card_types.subtypes.push("Aura".to_string());
    obj.base_card_types = obj.card_types.clone();
    let enchant = Keyword::from_str(&format!("Enchant:{enchant_spec}")).unwrap();
    obj.base_keywords.push(enchant.clone());
    obj.keywords.push(enchant);
    aura_id
}

/// Creates a vanilla creature card in `zone` (owner `owner`) with a 2/2 body.
fn build_creature_card(
    state: &mut GameState,
    card_id: CardId,
    owner: PlayerId,
    zone: Zone,
) -> ObjectId {
    use crate::types::card_type::CoreType;

    let id = create_object(state, card_id, owner, "Grizzly Bears".to_string(), zone);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    obj.base_power = Some(2);
    obj.base_toughness = Some(2);
    id
}

fn source_leaves_battlefield_event(source_id: ObjectId) -> GameEvent {
    GameEvent::ZoneChanged {
        object_id: source_id,
        from: Some(Zone::Battlefield),
        to: Zone::Graveyard,
        record: Box::new(ZoneChangeRecord {
            name: "Aura Source".to_string(),
            ..ZoneChangeRecord::test_minimal(source_id, Some(Zone::Battlefield), Zone::Graveyard)
        }),
    }
}

/// CR 303.4f + CR 610.3a: an Animate Dead-class Aura returning from exile (its
/// source left the battlefield, firing its `UntilSourceLeaves` link) must find
/// the legal creature card sitting in a graveyard, re-enter the battlefield, and
/// attach to it — then its ETB reanimation trigger pulls that creature onto the
/// battlefield. REVERT PROOF: with the `legal_aura_attachment_targets` rewrite
/// reverted (battlefield-only scan) the graveyard host is invisible, the inline
/// consult takes the `[] => Done` (CR 303.4g) arm, and both the "Aura on
/// battlefield" and "attached_to == creature" assertions fail (Aura stays in
/// exile).
#[test]
fn reanimator_aura_reenters_from_exile_and_attaches_to_graveyard_creature() {
    use crate::game::game_object::AttachTarget;

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // The Aura currently sits in exile, linked to return when its source leaves.
    let aura_id = build_reanimator_aura_in_zone(&mut state, CardId(601), PlayerId(0), Zone::Exile);
    // Grizzly Bears in the OPPONENT's graveyard — a legal graveyard host and, once
    // reanimated, a genuine control change to the Aura's controller.
    let creature_id = build_creature_card(&mut state, CardId(602), PlayerId(1), Zone::Graveyard);

    let source_id = create_object(
        &mut state,
        CardId(600),
        PlayerId(0),
        "Aura Source".to_string(),
        Zone::Battlefield,
    );
    state.exile_links.push(ExileLink {
        exiled_id: aura_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    // The source leaves → the Aura's implicit return fires and consults the host
    // search on the way back onto the battlefield.
    let mut events = vec![source_leaves_battlefield_event(source_id)];
    check_exile_returns(&mut state, &mut events);

    // (a) CR 303.4f: the Aura found the graveyard host and reached the battlefield
    // instead of being stranded in exile by CR 303.4g.
    assert!(
        state.battlefield.contains(&aura_id),
        "Aura must re-enter the battlefield via the graveyard host search; exile={:?}",
        state.exile
    );
    assert!(
        !state.exile.contains(&aura_id),
        "Aura must no longer be in exile"
    );
    // (b) It attached to the specific graveyard creature.
    assert_eq!(
        state.objects[&aura_id].attached_to,
        Some(AttachTarget::Object(creature_id)),
        "Aura must be attached to the graveyard creature it enchanted"
    );

    // (c) The ETB reanimation chain fires through the real trigger pipeline and
    // pulls the creature onto the battlefield under the Aura controller's control.
    let mut trigger_events = Vec::new();
    let _ = crate::game::triggers::drain_deferred_trigger_queue(&mut state, &mut trigger_events);
    assert_eq!(
        state.stack.len(),
        1,
        "the reanimator ETB trigger must be on the stack after process_triggers"
    );
    let mut etb_events = Vec::new();
    crate::game::stack::resolve_top(&mut state, &mut etb_events);
    crate::game::layers::evaluate_layers(&mut state);

    assert_eq!(
        state.objects[&creature_id].zone,
        Zone::Battlefield,
        "reanimated creature must be on the battlefield, not the graveyard"
    );
    assert_eq!(
        state.objects[&creature_id].controller,
        PlayerId(0),
        "reanimated creature must be controlled by the Aura's controller"
    );
    assert_eq!(
        state.objects[&aura_id].attached_to,
        Some(AttachTarget::Object(creature_id)),
        "Aura must stay attached to the reanimated creature"
    );
}

/// CR 303.4g (negative) + CR 303.4f reach-guard: an ordinary battlefield-scoped
/// Aura (Pacifism-shaped, `Enchant creature`) returning from exile finds a host
/// ONLY when a legal creature is on the battlefield. Aura A returns to an empty
/// board and correctly stays in exile (no spurious attach, no behavior change for
/// the common case); Aura B — the positive reach-guard in the same state, proving
/// the consult path is live — finds its battlefield creature and attaches. If the
/// negative were vacuous (consult never ran), a non-Aura would simply re-enter
/// unattached, which assertion A explicitly excludes.
#[test]
fn battlefield_scoped_aura_reenters_only_when_a_legal_host_exists() {
    use crate::game::game_object::AttachTarget;

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Aura A: battlefield-scoped, empty board — no legal host.
    let aura_a = build_simple_aura(
        &mut state,
        CardId(701),
        PlayerId(0),
        Zone::Exile,
        "creature",
    );
    let source_a = create_object(
        &mut state,
        CardId(700),
        PlayerId(0),
        "Aura Source".to_string(),
        Zone::Battlefield,
    );
    state.exile_links.push(ExileLink {
        exiled_id: aura_a,
        source_id: source_a,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let mut events_a = vec![source_leaves_battlefield_event(source_a)];
    check_exile_returns(&mut state, &mut events_a);

    // CR 303.4g: no legal host on the battlefield → the Aura stays in its current
    // zone (exile). (Non-vacuous: an unrecognized object would have re-entered the
    // battlefield unattached — this assertion excludes that.)
    assert!(
        !state.battlefield.contains(&aura_a),
        "battlefield-scoped Aura must NOT re-enter with no legal host"
    );
    assert!(
        state.exile.contains(&aura_a),
        "battlefield-scoped Aura with no host must stay in exile (CR 303.4g)"
    );

    // Positive reach-guard — Aura B: battlefield-scoped, with a legal battlefield
    // creature present. Proves the same consult path DOES attach when a host
    // exists (so assertion A above is a real "no host", not a skipped consult).
    let aura_b = build_simple_aura(
        &mut state,
        CardId(711),
        PlayerId(0),
        Zone::Exile,
        "creature",
    );
    let creature_b = build_creature_card(&mut state, CardId(712), PlayerId(0), Zone::Battlefield);
    let source_b = create_object(
        &mut state,
        CardId(710),
        PlayerId(0),
        "Aura Source".to_string(),
        Zone::Battlefield,
    );
    state.exile_links.push(ExileLink {
        exiled_id: aura_b,
        source_id: source_b,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let mut events_b = vec![source_leaves_battlefield_event(source_b)];
    check_exile_returns(&mut state, &mut events_b);

    assert!(
        state.battlefield.contains(&aura_b),
        "battlefield-scoped Aura must re-enter when a legal battlefield host exists"
    );
    assert_eq!(
        state.objects[&aura_b].attached_to,
        Some(AttachTarget::Object(creature_b)),
        "Aura B must attach to the legal battlefield creature"
    );
}

/// CR 303.4g (negative for the fixed path): a graveyard-scoped Aura returning from
/// exile when the graveyard holds a card that does NOT satisfy its Enchant ability
/// (an instant card, not a creature card) must still stay in exile — the fix must
/// not spuriously attach to an illegal host. NON-VACUOUS: the graveyard is
/// non-empty, so the new graveyard scan genuinely enumerates the instant card and
/// `matches_target_filter` correctly rejects it; an unrecognized Aura would have
/// re-entered the battlefield unattached, which this test excludes.
#[test]
fn graveyard_scoped_aura_stays_in_exile_when_no_legal_graveyard_creature() {
    use crate::types::card_type::CoreType;

    let mut state = GameState::new_two_player(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    let aura_id = build_reanimator_aura_in_zone(&mut state, CardId(801), PlayerId(0), Zone::Exile);
    // A non-creature card sits in the graveyard: the scan reaches it, the filter
    // rejects it (Enchant creature CARD in a graveyard requires a creature card).
    let instant_id = create_object(
        &mut state,
        CardId(802),
        PlayerId(1),
        "Lightning Bolt".to_string(),
        Zone::Graveyard,
    );
    {
        let obj = state.objects.get_mut(&instant_id).unwrap();
        obj.card_types.core_types.push(CoreType::Instant);
        obj.base_card_types = obj.card_types.clone();
    }
    // Sanity: the graveyard genuinely holds the candidate the scan will enumerate.
    assert!(
        state.players[1].graveyard.contains(&instant_id),
        "reach-guard: graveyard must hold the non-matching candidate card"
    );

    let source_id = create_object(
        &mut state,
        CardId(800),
        PlayerId(0),
        "Aura Source".to_string(),
        Zone::Battlefield,
    );
    state.exile_links.push(ExileLink {
        exiled_id: aura_id,
        source_id,
        kind: ExileLinkKind::UntilSourceLeaves {
            return_zone: Zone::Battlefield,
        },
    });

    let mut events = vec![source_leaves_battlefield_event(source_id)];
    check_exile_returns(&mut state, &mut events);

    assert!(
        !state.battlefield.contains(&aura_id),
        "graveyard-scoped Aura must NOT re-enter when no legal graveyard creature exists"
    );
    assert!(
        state.exile.contains(&aura_id),
        "graveyard-scoped Aura with no legal host must stay in exile (CR 303.4g)"
    );
    assert!(
        state.objects[&aura_id].attached_to.is_none(),
        "Aura must not have spuriously attached to the illegal instant card"
    );
}

// ---------------------------------------------------------------------------
// CR 610.3b latch window: which "until" events the latch half of
// `check_exile_returns` records onto each kind of held triggered ability, and
// how much work it spends locating each holder's trigger event.
// ---------------------------------------------------------------------------
mod latch_window {
    use super::*;
    use crate::game::ability_utils::build_resolved_from_def;
    use crate::game::scenario::{GameScenario, P0, P1};
    use crate::game::triggers::{PendingTrigger, PendingTriggerContext};
    use crate::types::ability::{Duration, DurationEvent, ResolvedAbility, TargetRef};
    use crate::types::game_state::{
        PendingTriggerOrder, StackEntry, StackEntryKind, TriggerOrderGroup,
    };

    const BANISHER_PRIEST: &str = "When this creature enters, exile target creature an opponent controls until this creature leaves the battlefield.";
    const PALACE_JAILER: &str = "When this creature enters, you become the monarch.\nWhen this creature enters, exile target creature an opponent controls until an opponent becomes the monarch.";

    const STACK_ENTRY_ID: ObjectId = ObjectId(9_100_001);

    struct Fixture {
        state: GameState,
        source: ObjectId,
        ability: ResolvedAbility,
    }

    /// Builds `name` from its verbatim Oracle text and resolves the trigger
    /// whose exile is bounded by `duration`, targeting an opponent's creature.
    fn fixture(name: &str, oracle: &str, duration: Duration) -> Fixture {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, name, 2, 2)
            .from_oracle_text(oracle)
            .id();
        let target = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();
        let state = scenario.build().state().clone();
        let execute = state.objects[&source]
            .trigger_definitions
            .iter_all()
            .filter_map(|trigger| trigger.definition.execute.as_deref())
            .find(|execute| execute.duration.as_ref() == Some(&duration))
            .expect("the bounded exile trigger must parse")
            .clone();
        let mut ability = build_resolved_from_def(&execute, source, P0);
        ability.targets = vec![TargetRef::Object(target)];
        // Reach-guard: the parsed ability must carry the bounded duration, or
        // every "no latch" row below would pass vacuously.
        assert!(ability.contains_duration_event(
            duration
                .zone_change_event()
                .expect("an `until` duration bounds a zone change")
        ));
        Fixture {
            state,
            source,
            ability,
        }
    }

    fn banisher_priest() -> Fixture {
        fixture(
            "Banisher Priest",
            BANISHER_PRIEST,
            Duration::UntilHostLeavesPlay,
        )
    }

    fn palace_jailer() -> Fixture {
        fixture(
            "Palace Jailer",
            PALACE_JAILER,
            Duration::UntilOpponentBecomesMonarch,
        )
    }

    /// T: the source's own entry, the event its "enters" trigger fired on.
    fn enters(object_id: ObjectId) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                object_id,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        }
    }

    /// A second, distinct entry of the same object (a different trigger event).
    fn reenters_from_exile(object_id: ObjectId) -> GameEvent {
        GameEvent::ZoneChanged {
            object_id,
            from: Some(Zone::Exile),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                object_id,
                Some(Zone::Exile),
                Zone::Battlefield,
            )),
        }
    }

    fn becomes_monarch(player_id: PlayerId) -> GameEvent {
        GameEvent::MonarchChanged { player_id }
    }

    fn triggered_entry(
        fixture: &Fixture,
        id: ObjectId,
        ability: ResolvedAbility,
        trigger_event: Option<GameEvent>,
    ) -> StackEntry {
        StackEntry {
            id,
            source_id: fixture.source,
            controller: P0,
            kind: StackEntryKind::TriggeredAbility {
                source_id: fixture.source,
                ability: Box::new(ability),
                description: None,
                condition: None,
                trigger_event,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
                provenance: None,
            },
        }
    }

    fn pending_context(
        fixture: &Fixture,
        controller: PlayerId,
        trigger_event: Option<GameEvent>,
    ) -> PendingTriggerContext {
        let mut pending = PendingTrigger::ordinary(
            fixture.source,
            controller,
            None,
            Box::new(fixture.ability.clone()),
            1,
        );
        pending.trigger_event = trigger_event;
        PendingTriggerContext::single(pending)
    }

    /// The latches recorded on a resolved ability's bounded nodes.
    fn node_latches(ability: &ResolvedAbility) -> Vec<DurationEvent> {
        let mut latches = ability.context.duration_events.clone();
        for branch in [&ability.sub_ability, &ability.else_ability]
            .into_iter()
            .flatten()
        {
            latches.extend(node_latches(branch));
        }
        latches
    }

    /// Where a held triggered ability waits while the latch pass runs.
    #[derive(Clone, Copy, Debug)]
    enum Holder {
        Stack,
        Resolving,
        Deferred,
        /// CR 603.3b: awaiting its controller's ordering choice.
        Ordered,
    }

    const ALL_HOLDERS: [Holder; 4] = [
        Holder::Stack,
        Holder::Resolving,
        Holder::Deferred,
        Holder::Ordered,
    ];

    /// Holders whose recorded trigger event gates the window. A resolving
    /// entry triggered in an earlier priority pass, so its trigger event is
    /// never in the buffer this pass reads and it has no "before" rows.
    const GATED_HOLDERS: [Holder; 3] = [Holder::Stack, Holder::Deferred, Holder::Ordered];

    /// Places the fixture's ability in `holder`, runs the pass over `events`,
    /// and returns what the holder latched.
    fn latches_after_pass(
        fixture: &Fixture,
        holder: Holder,
        trigger_event: Option<GameEvent>,
        mut events: Vec<GameEvent>,
    ) -> Vec<DurationEvent> {
        let mut state = fixture.state.clone();
        match holder {
            Holder::Stack => state.stack.push_back(triggered_entry(
                fixture,
                STACK_ENTRY_ID,
                fixture.ability.clone(),
                trigger_event,
            )),
            Holder::Resolving => {
                state.resolving_stack_entry = Some(triggered_entry(
                    fixture,
                    STACK_ENTRY_ID,
                    fixture.ability.clone(),
                    trigger_event,
                ));
            }
            Holder::Deferred => {
                state
                    .deferred_triggers
                    .push(pending_context(fixture, P0, trigger_event))
            }
            Holder::Ordered => {
                state.pending_trigger_order = Some(PendingTriggerOrder {
                    groups: vec![TriggerOrderGroup {
                        controller: P0,
                        triggers: vec![pending_context(fixture, P0, trigger_event)],
                        ordered: false,
                    }],
                    resume_after_ordering: None,
                });
            }
        }
        check_exile_returns(&mut state, &mut events);
        match holder {
            Holder::Stack => node_latches(state.stack[0].ability().expect("triggered entry")),
            Holder::Resolving => node_latches(
                state
                    .resolving_stack_entry
                    .as_ref()
                    .and_then(StackEntry::ability)
                    .expect("triggered entry"),
            ),
            Holder::Deferred => state.deferred_triggers[0].duration_events.clone(),
            Holder::Ordered => state.pending_trigger_order.expect("ordering pass").groups[0]
                .triggers[0]
                .duration_events
                .clone(),
        }
    }

    /// CR 610.3b: the source left the battlefield after the ability triggered,
    /// so the exile one-shot must not move the target.
    #[test]
    fn source_leaving_after_the_trigger_latches_every_holder() {
        for holder in ALL_HOLDERS {
            let priest = banisher_priest();
            let source = priest.source;
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![enters(source), source_leaves_battlefield_event(source)],
                ),
                vec![DurationEvent::SourceLeftBattlefield],
                "{holder:?}: [T, L]"
            );
        }
    }

    /// CR 610.3b: a leave event that precedes the triggering event is not
    /// "after that ability triggered".
    #[test]
    fn source_leaving_before_the_trigger_does_not_latch() {
        for holder in GATED_HOLDERS {
            let priest = banisher_priest();
            let source = priest.source;
            // Reach-guard: the same holder latches when the order is reversed.
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![enters(source), source_leaves_battlefield_event(source)],
                ),
                vec![DurationEvent::SourceLeftBattlefield],
                "{holder:?}: [T, L]"
            );
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![source_leaves_battlefield_event(source), enters(source)],
                ),
                Vec::<DurationEvent>::new(),
                "{holder:?}: [L, T]"
            );
        }
    }

    /// CR 610.3b: a trigger event absent from this buffer happened in an
    /// earlier action, so every event in the buffer follows the trigger.
    #[test]
    fn trigger_event_from_an_earlier_action_opens_the_whole_buffer() {
        for holder in ALL_HOLDERS {
            let priest = banisher_priest();
            let source = priest.source;
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![source_leaves_battlefield_event(source)],
                ),
                vec![DurationEvent::SourceLeftBattlefield],
                "{holder:?}: [L] with T absent"
            );
        }
    }

    /// CR 603.8 + CR 610.3b: a held ability that bounds an "until" zone change
    /// and has no recorded trigger event is, in production, a state trigger
    /// (`check_state_triggers` builds it with
    /// `trigger_event: None`). The pipeline collects state triggers after
    /// `check_exile_returns`, so such an ability triggered before the buffer a
    /// later latch pass reads began, and every event in that buffer is "after
    /// that ability triggered".
    #[test]
    fn holder_without_a_trigger_event_opens_the_whole_buffer() {
        for holder in ALL_HOLDERS {
            let priest = banisher_priest();
            let source = priest.source;
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    None,
                    vec![source_leaves_battlefield_event(source)],
                ),
                vec![DurationEvent::SourceLeftBattlefield],
                "{holder:?}: [L] with no trigger event"
            );
        }
    }

    /// CR 610.3b: only "this creature" leaving is the specified event, and a
    /// monarch change is not the specified event of a leaves-the-battlefield
    /// duration.
    #[test]
    fn unrelated_events_do_not_latch_a_source_leaves_duration() {
        for holder in ALL_HOLDERS {
            let priest = banisher_priest();
            let source = priest.source;
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![enters(source), source_leaves_battlefield_event(source)],
                ),
                vec![DurationEvent::SourceLeftBattlefield],
                "{holder:?}: reach-guard [T, L]"
            );
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![
                        enters(source),
                        source_leaves_battlefield_event(ObjectId(777_777)),
                    ],
                ),
                Vec::<DurationEvent>::new(),
                "{holder:?}: another object leaves"
            );
            assert_eq!(
                latches_after_pass(
                    &priest,
                    holder,
                    Some(enters(source)),
                    vec![enters(source), becomes_monarch(P1)],
                ),
                Vec::<DurationEvent>::new(),
                "{holder:?}: monarch change against a leaves-the-battlefield duration"
            );
        }
    }

    /// CR 610.3b + CR 725.1: Palace Jailer's exile ends when an opponent of its
    /// controller becomes the monarch after the ability triggered.
    #[test]
    fn opponent_becoming_monarch_after_the_trigger_latches() {
        for holder in ALL_HOLDERS {
            let jailer = palace_jailer();
            let source = jailer.source;
            assert_eq!(
                latches_after_pass(
                    &jailer,
                    holder,
                    Some(enters(source)),
                    vec![enters(source), becomes_monarch(P1)],
                ),
                vec![DurationEvent::OpponentBecameMonarch],
                "{holder:?}: [T, M(P1)]"
            );
            assert_eq!(
                latches_after_pass(
                    &jailer,
                    holder,
                    Some(enters(source)),
                    vec![enters(source), becomes_monarch(P0)],
                ),
                Vec::<DurationEvent>::new(),
                "{holder:?}: the controller is not an opponent"
            );
        }
        for holder in GATED_HOLDERS {
            let jailer = palace_jailer();
            let source = jailer.source;
            assert_eq!(
                latches_after_pass(
                    &jailer,
                    holder,
                    Some(enters(source)),
                    vec![becomes_monarch(P1), enters(source)],
                ),
                Vec::<DurationEvent>::new(),
                "{holder:?}: [M(P1), T]"
            );
        }
    }

    /// CR 610.3b: each holder's window opens at its own trigger event. One
    /// leave event sits between two trigger events, so the holders that
    /// triggered on the first latch and those that triggered on the second do
    /// not, including ordered holders spread over several groups.
    #[test]
    fn each_holder_opens_its_own_window() {
        let priest = banisher_priest();
        let source = priest.source;
        let first = enters(source);
        let second = reenters_from_exile(source);
        let mut state = priest.state.clone();
        for (id, trigger_event) in [(9_200_001, &first), (9_200_002, &second)] {
            state.stack.push_back(triggered_entry(
                &priest,
                ObjectId(id),
                priest.ability.clone(),
                Some(trigger_event.clone()),
            ));
        }
        for trigger_event in [&first, &second] {
            state
                .deferred_triggers
                .push(pending_context(&priest, P0, Some(trigger_event.clone())));
        }
        state.pending_trigger_order = Some(PendingTriggerOrder {
            groups: [(P0, [&first, &second]), (P1, [&second, &first])]
                .into_iter()
                .map(|(controller, trigger_events)| TriggerOrderGroup {
                    controller,
                    triggers: trigger_events
                        .into_iter()
                        .map(|event| pending_context(&priest, controller, Some(event.clone())))
                        .collect(),
                    ordered: false,
                })
                .collect(),
            resume_after_ordering: None,
        });
        let mut events = vec![
            first.clone(),
            source_leaves_battlefield_event(source),
            second.clone(),
        ];

        check_exile_returns(&mut state, &mut events);

        let latched = vec![DurationEvent::SourceLeftBattlefield];
        let unlatched = Vec::<DurationEvent>::new();
        let stack: Vec<_> = state
            .stack
            .iter()
            .map(|entry| node_latches(entry.ability().expect("triggered entry")))
            .collect();
        assert_eq!(stack, [latched.clone(), unlatched.clone()], "stack");
        let deferred: Vec<_> = state
            .deferred_triggers
            .iter()
            .map(|context| context.duration_events.clone())
            .collect();
        assert_eq!(deferred, [latched.clone(), unlatched.clone()], "deferred");
        let ordered: Vec<Vec<_>> = state
            .pending_trigger_order
            .expect("ordering pass")
            .groups
            .iter()
            .map(|group| {
                group
                    .triggers
                    .iter()
                    .map(|context| context.duration_events.clone())
                    .collect()
            })
            .collect();
        assert_eq!(
            ordered,
            [[latched.clone(), unlatched.clone()], [unlatched, latched]],
            "ordered (group, trigger)"
        );
    }

    /// The pass locates each duration-bearing holder's trigger event once,
    /// however long the event buffer is; holders whose ability bounds no zone
    /// change are never searched.
    #[cfg(feature = "test-support")]
    #[test]
    fn trigger_origin_lookups_do_not_scale_with_the_event_buffer() {
        const BOUNDED_HOLDERS: u64 = 3;
        const PLAIN_HOLDERS: u64 = 5;
        const FILLER_EVENTS: [u64; 3] = [4, 40, 400];
        let priest = banisher_priest();
        let source = priest.source;
        let mut plain = priest.ability.clone();
        plain.duration = None;
        assert!(!plain.contains_duration_event(DurationEvent::SourceLeftBattlefield));

        // (latched holders, origin lookups) per buffer length.
        let measured: Vec<(u64, u64)> = FILLER_EVENTS
            .into_iter()
            .map(|filler_events| {
                let mut state = priest.state.clone();
                for index in 0..BOUNDED_HOLDERS {
                    state.stack.push_back(triggered_entry(
                        &priest,
                        ObjectId(9_300_000 + index),
                        priest.ability.clone(),
                        Some(enters(source)),
                    ));
                }
                for index in 0..PLAIN_HOLDERS {
                    state.stack.push_back(triggered_entry(
                        &priest,
                        ObjectId(9_400_000 + index),
                        plain.clone(),
                        Some(enters(source)),
                    ));
                }
                // Entries of unrelated objects: never a trigger event, never a
                // duration event.
                let mut events: Vec<GameEvent> = (0..filler_events)
                    .map(|index| enters(ObjectId(800_000 + index)))
                    .collect();
                events.push(source_leaves_battlefield_event(source));

                crate::game::perf_counters::reset();
                check_exile_returns(&mut state, &mut events);

                let latched_holders = state
                    .stack
                    .iter()
                    .filter(|entry| {
                        !node_latches(entry.ability().expect("triggered entry")).is_empty()
                    })
                    .count() as u64;
                let lookups = crate::game::perf_counters::exile_return_latch_snapshot()
                    .trigger_origin_lookups;
                (latched_holders, lookups)
            })
            .collect();

        // The first element is the reach-guard (every duration-bearing holder
        // latched, so the pass did its work); the second is one lookup per
        // duration-bearing holder whatever the buffer length.
        assert_eq!(
            measured,
            [(BOUNDED_HOLDERS, BOUNDED_HOLDERS); FILLER_EVENTS.len()],
            "(latched holders, origin lookups) for {FILLER_EVENTS:?} filler events"
        );
    }
}
