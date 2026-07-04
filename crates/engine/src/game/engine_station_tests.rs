use super::*;
use crate::game::zones::create_object;
use crate::types::ability::{
    ContinuousModification, StaticCondition, StaticDefinition, TargetFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::{CounterMatch, CounterType};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn setup_game_at_main_phase() -> GameState {
    let mut state = new_game(42);
    state.turn_number = 2;
    state.phase = Phase::PreCombatMain;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };
    state
}

/// Set up a Spacecraft with the Station keyword and two eligible creatures.
fn setup_station_scenario() -> (GameState, ObjectId, ObjectId, ObjectId) {
    let mut state = setup_game_at_main_phase();

    let spacecraft_id = create_object(
        &mut state,
        CardId(300),
        PlayerId(0),
        "Test Spacecraft".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&spacecraft_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Spacecraft".to_string());
        obj.keywords.push(crate::types::keywords::Keyword::Station);
    }

    let power_5 = create_object(
        &mut state,
        CardId(301),
        PlayerId(0),
        "Power 5 Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&power_5).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(5);
        obj.toughness = Some(5);
        obj.base_power = Some(5);
        obj.base_toughness = Some(5);
    }

    let power_2 = create_object(
        &mut state,
        CardId(302),
        PlayerId(0),
        "Power 2 Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&power_2).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
    }

    (state, spacecraft_id, power_5, power_2)
}

#[test]
fn station_activation_enters_station_target_state() {
    let (mut state, spacecraft_id, p5, p2) = setup_station_scenario();

    let result = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();

    match result.waiting_for {
        WaitingFor::StationTarget {
            player,
            spacecraft_id: sid,
            eligible_creatures,
        } => {
            assert_eq!(player, PlayerId(0));
            assert_eq!(sid, spacecraft_id);
            assert!(eligible_creatures.contains(&p5));
            assert!(eligible_creatures.contains(&p2));
            // Spacecraft must NOT be eligible to tap itself
            assert!(!eligible_creatures.contains(&spacecraft_id));
        }
        other => panic!("Expected StationTarget, got {other:?}"),
    }
}

#[test]
fn station_resolution_taps_creature_and_adds_counters_equal_to_power() {
    let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();

    // Announcement: cost paid (tap), stack entry pushed — but no counters yet.
    let announce = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(p5),
        },
    )
    .unwrap();
    assert!(
        state.objects.get(&p5).unwrap().tapped,
        "creature must be tapped at announcement"
    );
    assert_eq!(
        state.stack.len(),
        1,
        "Station announcement must push a stack entry (CR 113.3b)"
    );
    let charge_after_announce = state
        .objects
        .get(&spacecraft_id)
        .unwrap()
        .counters
        .get(&CounterType::Generic("charge".to_string()))
        .copied()
        .unwrap_or(0);
    assert_eq!(
        charge_after_announce, 0,
        "charge counters must not be applied before stack resolution"
    );
    assert!(
        !announce
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::Stationed { .. })),
        "Stationed event must not fire at announcement"
    );

    // Both players pass priority → stack resolves → counters added.
    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    let resolve = apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    let charge = state
        .objects
        .get(&spacecraft_id)
        .unwrap()
        .counters
        .get(&CounterType::Generic("charge".to_string()))
        .copied()
        .unwrap_or(0);
    assert_eq!(charge, 5, "charge counters applied at stack resolution");
    assert!(
            resolve
                .events
                .iter()
                .any(|e| matches!(e, GameEvent::Stationed { spacecraft_id: sid, creature_id: cid, counters_added: 5 } if *sid == spacecraft_id && *cid == p5)),
            "Stationed event fires at resolution"
        );
    assert!(state.stack.is_empty(), "stack empty after resolution");
}

#[test]
fn stationed_perpetual_grant_keywords_applies_to_stationing_creature() {
    use crate::game::trigger_index::reindex_object_triggers;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PerpetualModification, TriggerDefinition,
    };
    use crate::types::keywords::Keyword;
    use crate::types::triggers::TriggerMode;

    let (mut state, spacecraft_id, p5, _) = setup_station_scenario();
    {
        let trigger_def =
            TriggerDefinition::new(TriggerMode::Stationed).execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::ApplyPerpetual {
                    target: TargetFilter::ParentTarget,
                    modification: PerpetualModification::GrantKeywords {
                        keywords: vec![Keyword::Deathtouch, Keyword::Lifelink],
                    },
                },
            ));
        let obj = state.objects.get_mut(&spacecraft_id).unwrap();
        obj.trigger_definitions.push(trigger_def.clone());
        std::sync::Arc::make_mut(&mut obj.base_trigger_definitions).push(trigger_def);
    }
    reindex_object_triggers(&mut state, spacecraft_id);

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(p5),
        },
    )
    .unwrap();

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    assert!(
        !state.stack.is_empty(),
        "Stationed trigger must push a stack entry"
    );

    let mut guard = 0;
    while !state.stack.is_empty() {
        guard += 1;
        assert!(guard < 20, "stack failed to drain");
        apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
        apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();
    }

    crate::game::layers::flush_layers(&mut state);
    let creature = state.objects.get(&p5).unwrap();
    assert!(creature.has_keyword(&Keyword::Deathtouch));
    assert!(creature.has_keyword(&Keyword::Lifelink));
    let spacecraft = state.objects.get(&spacecraft_id).unwrap();
    assert!(!spacecraft.has_keyword(&Keyword::Deathtouch));
    assert!(!spacecraft.has_keyword(&Keyword::Lifelink));
}

#[test]
fn station_activation_rejects_outside_sorcery_window() {
    let (mut state, spacecraft_id, _, _) = setup_station_scenario();
    // Move to declare attackers — no longer sorcery speed.
    state.phase = Phase::DeclareAttackers;

    let err = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::ActionNotAllowed(_)));
}

#[test]
fn station_activation_rejects_on_opponents_turn() {
    let (mut state, spacecraft_id, _, _) = setup_station_scenario();
    state.active_player = PlayerId(1);

    let err = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::ActionNotAllowed(_)));
}

#[test]
fn station_cannot_tap_the_spacecraft_itself() {
    let (mut state, spacecraft_id, _, _) = setup_station_scenario();

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();

    // Attempt to select the spacecraft itself — rejected because it's not
    // in the eligible list.
    let err = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(spacecraft_id),
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::InvalidAction(_)));
}

#[test]
fn station_resolution_uses_snapshot_power_when_tapped_creature_leaves_battlefield() {
    // CR 113.7a: Station's counter count is snapshot at announcement. If the
    // tapped creature leaves the battlefield between announcement and
    // resolution (e.g. bounced by an instant-speed response), the snapshot
    // value is still applied.
    let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(p5),
        },
    )
    .unwrap();

    // Remove the tapped creature from the battlefield before resolution.
    let p5_obj = state.objects.get_mut(&p5).unwrap();
    p5_obj.zone = Zone::Graveyard;
    state.battlefield.retain(|id| *id != p5);

    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    // Counters still applied at snapshot value despite creature leaving.
    let charge = state
        .objects
        .get(&spacecraft_id)
        .unwrap()
        .counters
        .get(&CounterType::Generic("charge".to_string()))
        .copied()
        .unwrap_or(0);
    assert_eq!(
        charge, 5,
        "CR 113.7a: snapshot_power applied even when tapped creature left battlefield"
    );
}

#[test]
fn station_threshold_static_reapplies_and_spacecraft_becomes_creature() {
    let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

    {
        let spacecraft = state.objects.get_mut(&spacecraft_id).unwrap();
        spacecraft.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .condition(StaticCondition::HasCounters {
                    counters: CounterMatch::OfType(CounterType::Generic("charge".to_string())),
                    minimum: 8,
                    maximum: None,
                })
                .modifications(vec![
                    ContinuousModification::AddType {
                        core_type: CoreType::Creature,
                    },
                    ContinuousModification::SetPower { value: 5 },
                    ContinuousModification::SetToughness { value: 5 },
                ])
                .description("CR 721.2b: Spacecraft is an artifact creature at 8+".to_string()),
        );
    }

    // First station activation: 5 charge counters, below threshold.
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(p5),
        },
    )
    .unwrap();
    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        !state
            .objects
            .get(&spacecraft_id)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "spacecraft should still be noncreature below threshold"
    );

    // Simulate a later main phase where the same creature can station again.
    state.objects.get_mut(&p5).unwrap().tapped = false;
    state.phase = Phase::PreCombatMain;
    state.priority_player = PlayerId(0);
    state.waiting_for = WaitingFor::Priority {
        player: PlayerId(0),
    };

    // Second station activation: another 5 counters, crossing threshold.
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();
    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(p5),
        },
    )
    .unwrap();
    apply(&mut state, PlayerId(0), GameAction::PassPriority).unwrap();
    apply(&mut state, PlayerId(1), GameAction::PassPriority).unwrap();

    assert!(
        state
            .objects
            .get(&spacecraft_id)
            .unwrap()
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "spacecraft should become a creature at 8+ charge counters"
    );
}

#[test]
fn station_rejects_tapped_creature_after_gap() {
    let (mut state, spacecraft_id, p5, _) = setup_station_scenario();

    apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap();

    // Simulate an intervening effect that tapped p5 between activation
    // and resolution (the HarmonizeTap-idiom revalidation scenario).
    state.objects.get_mut(&p5).unwrap().tapped = true;

    let err = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: Some(p5),
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::InvalidAction(_)));
}

#[test]
fn station_without_eligible_creature_rejected() {
    let mut state = setup_game_at_main_phase();
    let spacecraft_id = create_object(
        &mut state,
        CardId(400),
        PlayerId(0),
        "Lone Spacecraft".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&spacecraft_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Spacecraft".to_string());
        obj.keywords.push(crate::types::keywords::Keyword::Station);
    }

    let err = apply_as_current(
        &mut state,
        GameAction::ActivateStation {
            spacecraft_id,
            creature_id: None,
        },
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::ActionNotAllowed(_)));
}
