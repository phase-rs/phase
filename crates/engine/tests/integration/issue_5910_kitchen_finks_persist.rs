//! Regression for issue #5910: Persist must use each death event's exact
//! source incarnation and last known counter state.
//!
//! https://github.com/phase-rs/phase/issues/5910

use engine::database::synthesis::KeywordTriggerInstaller;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::triggers::process_triggers;
use engine::game::zones::move_to_zone;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
    TriggerCondition, TriggerDefinition, TriggerDefinitionOccurrenceRef, TriggerDefinitionRef,
    TriggerEntry, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor, ZoneChangeRecord};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
use engine::types::CounterType;

use crate::support::shared_card_db;

fn add_two_red_mana(runner: &mut GameRunner) {
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..2 {
        pool.add(ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }
}

fn single_persist_entry(entries: &[TriggerEntry]) -> &TriggerEntry {
    let mut persist_entries = entries.iter().filter(|entry| {
        KeywordTriggerInstaller::trigger_matches_keyword_kind(entry.definition(), &Keyword::Persist)
    });
    let entry = persist_entries
        .next()
        .expect("Kitchen Finks must have one synthesized Persist trigger");
    assert!(
        persist_entries.next().is_none(),
        "Kitchen Finks must have exactly one synthesized Persist trigger"
    );
    entry
}

fn single_death_record(events: &[GameEvent], object_id: ObjectId) -> &ZoneChangeRecord {
    let mut records = events.iter().filter_map(|event| match event {
        GameEvent::ZoneChanged {
            object_id: changed,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record,
        } if *changed == object_id => Some(record.as_ref()),
        _ => None,
    });
    let record = records
        .next()
        .expect("Lightning Bolt must produce one Kitchen Finks death record");
    assert!(
        records.next().is_none(),
        "each Lightning Bolt must produce exactly one Kitchen Finks death record"
    );
    record
}

fn zone_change_count(events: &[GameEvent], object_id: ObjectId, from: Zone, to: Zone) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id: changed,
                    from: Some(actual_from),
                    to: actual_to,
                    ..
                } if *changed == object_id && *actual_from == from && *actual_to == to
            )
        })
        .count()
}

fn minus_counter_count(record: &ZoneChangeRecord) -> u32 {
    record
        .trigger_source_context()
        .expect("a real zone change must carry its exact trigger source context")
        .lki
        .counters
        .get(&CounterType::Minus1Minus1)
        .copied()
        .unwrap_or(0)
}

fn persist_definition_ref(record: &ZoneChangeRecord) -> TriggerDefinitionRef {
    let context = record
        .trigger_source_context()
        .expect("a real zone change must carry its exact trigger source context");
    context.definition_ref(single_persist_entry(&record.trigger_definitions))
}

fn observer_trigger(condition: TriggerCondition) -> TriggerDefinition {
    let gain_life = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );

    TriggerDefinition::new(TriggerMode::ChangesZone)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .valid_card(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        ))
        .condition(condition)
        .execute(gain_life)
}

fn observer_fixture(condition: TriggerCondition) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let observer = scenario
        .add_creature(P0, "Counter departure observer", 2, 2)
        .with_trigger_definition(observer_trigger(condition))
        .id();
    let subject = scenario.add_creature(P0, "Repeated incarnation", 2, 2).id();
    (scenario.build(), observer, subject)
}

fn move_and_capture_zone_change(
    runner: &mut GameRunner,
    object_id: ObjectId,
    destination: Zone,
) -> GameEvent {
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), object_id, destination, &mut events);
    let mut matching = events.into_iter().filter(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id: changed,
                to,
                ..
            } if *changed == object_id && *to == destination
        )
    });
    let event = matching
        .next()
        .expect("move_to_zone must emit the requested ZoneChanged event");
    assert!(
        matching.next().is_none(),
        "one move must emit exactly one matching ZoneChanged event"
    );
    event
}

fn observer_trigger_events(state: &GameState, observer: ObjectId) -> Vec<GameEvent> {
    state
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                trigger_event: Some(event),
                ..
            } if entry.source_id == observer => Some(event.clone()),
            _ => None,
        })
        .collect()
}

fn clear_zone_change_context(event: &mut GameEvent) {
    let GameEvent::ZoneChanged { record, .. } = event else {
        panic!("test event must be a ZoneChanged event");
    };
    record.trigger_source_context = None;
}

#[derive(Debug, Clone, Copy)]
enum ProvenanceMismatch {
    RecordObject,
    RecordFrom,
    RecordTo,
    ContextObject,
    ContextExpectedZone,
}

fn introduce_provenance_mismatch(
    event: &mut GameEvent,
    mismatch: ProvenanceMismatch,
    different_object: ObjectId,
) {
    let GameEvent::ZoneChanged { record, .. } = event else {
        panic!("test event must be a ZoneChanged event");
    };
    match mismatch {
        ProvenanceMismatch::RecordObject => record.object_id = different_object,
        ProvenanceMismatch::RecordFrom => record.from_zone = Some(Zone::Hand),
        ProvenanceMismatch::RecordTo => record.to_zone = Zone::Exile,
        ProvenanceMismatch::ContextObject => {
            record
                .trigger_source_context
                .as_mut()
                .expect("real zone change must carry context")
                .identity
                .reference
                .object_id = different_object;
        }
        ProvenanceMismatch::ContextExpectedZone => {
            record
                .trigger_source_context
                .as_mut()
                .expect("real zone change must carry context")
                .identity
                .expected_zone = Zone::Hand;
        }
    }
}

#[test]
fn issue_5910_kitchen_finks_persist_uses_each_deaths_exact_lki() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    let finks = scenario.add_real_card(P0, "Kitchen Finks", Zone::Battlefield, db);
    let bolt_1 = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let bolt_2 = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_two_red_mana(&mut runner);

    let initial_life = runner.state().players[0].life;
    assert_eq!(initial_life, 20);
    assert_eq!(runner.state().objects[&finks].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&finks]
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied()
            .unwrap_or(0),
        0
    );

    let expected_occurrence = {
        let finks_object = &runner.state().objects[&finks];
        let entry = single_persist_entry(finks_object.trigger_definitions.as_slice());
        assert!(
            matches!(
                entry.occurrence,
                TriggerDefinitionOccurrenceRef::Printed { .. }
            ),
            "Kitchen Finks' synthesized Persist trigger must retain printed provenance"
        );
        entry.occurrence.clone()
    };

    let first = runner.cast(bolt_1).target_object(finks).resolve();

    // CR 702.79a: Persist returns the counter-free permanent with a -1/-1
    // counter. CR 122.6: A counter given as an object enters is put on it.
    assert_eq!(
        zone_change_count(first.events(), finks, Zone::Battlefield, Zone::Graveyard),
        1
    );
    assert_eq!(
        zone_change_count(first.events(), finks, Zone::Graveyard, Zone::Battlefield),
        1
    );
    first.assert_zone(&[finks], Zone::Battlefield);
    assert_eq!(first.counters(finks, CounterType::Minus1Minus1), 1);
    first.assert_life_delta(P0, 2);
    assert!(matches!(
        first.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(first.state().stack.is_empty());

    let first_death = single_death_record(first.events(), finks);
    let first_ref = persist_definition_ref(first_death);
    // CR 603.10a + CR 608.2h: A leaves-the-battlefield trigger reads the
    // source's event-time last known information.
    assert_eq!(minus_counter_count(first_death), 0);
    assert_eq!(first_ref.source.object_id, finks);
    assert_eq!(first_ref.occurrence, expected_occurrence);

    let second = runner.cast(bolt_2).target_object(finks).resolve();
    let second_death = single_death_record(second.events(), finks);
    let second_ref = persist_definition_ref(second_death);

    // CR 400.7: The returned permanent is a new object even though the engine
    // retains its storage ObjectId. Each trigger authority must bind its exact
    // incarnation, while the printed ability occurrence remains stable.
    assert_eq!(second_ref.source.object_id, finks);
    assert_ne!(first_ref.source.incarnation, second_ref.source.incarnation);
    assert!(first_ref.source.incarnation < second_ref.source.incarnation);
    assert_ne!(first_ref, second_ref);
    assert_eq!(second_ref.occurrence, expected_occurrence);
    // CR 603.10a + CR 608.2h: The second death's source context must retain the
    // counter that existed on that exact battlefield incarnation.
    assert_eq!(minus_counter_count(second_death), 1);

    assert_eq!(
        zone_change_count(second.events(), finks, Zone::Battlefield, Zone::Graveyard),
        1
    );
    assert_eq!(
        zone_change_count(second.events(), finks, Zone::Graveyard, Zone::Battlefield),
        0
    );
    second.assert_zone(&[finks], Zone::Graveyard);
    second.assert_life_delta(P0, 0);
    assert_eq!(second.state().players[0].life, initial_life + 2);
    // CR 122.2: Counters cease to exist when the permanent changes zones.
    assert_eq!(second.counters(finks, CounterType::Minus1Minus1), 0);

    assert!(matches!(
        second.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(second.state().stack.is_empty());
    assert!(second.state().pending_trigger.is_none());
    assert!(second.state().pending_trigger_event_batch.is_empty());
    assert!(second.state().pending_trigger_entry.is_none());
    assert!(second.state().deferred_triggers.is_empty());
    assert!(second.state().pending_trigger_order.is_none());
}

#[test]
fn issue_5910_fire_time_uses_exact_zone_change_record_lki() {
    let condition = TriggerCondition::HadCounters {
        counter_type: Some(CounterType::Plus1Plus1),
    };
    let (mut runner, observer, subject) = observer_fixture(condition);
    runner
        .state_mut()
        .objects
        .get_mut(&observer)
        .expect("observer exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    let first_departure = move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
    move_and_capture_zone_change(&mut runner, subject, Zone::Battlefield);
    runner
        .state_mut()
        .objects
        .get_mut(&subject)
        .expect("returned subject exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    let second_departure = move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
    assert_eq!(
        runner.state().lki_cache[&subject]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "hostile later incarnation must overwrite the cache with countered LKI"
    );

    // CR 603.10a + CR 608.2h: The first event retains its own counterless LKI;
    // neither the countered watcher nor the later same-id cache entry may answer.
    process_triggers(runner.state_mut(), std::slice::from_ref(&first_departure));
    assert!(
        observer_trigger_events(runner.state(), observer).is_empty(),
        "the counterless first departure must not satisfy HadCounters"
    );

    process_triggers(runner.state_mut(), std::slice::from_ref(&second_departure));
    assert_eq!(
        observer_trigger_events(runner.state(), observer),
        vec![second_departure],
        "the countered second departure is the positive reach guard and must carry its exact event"
    );
}

#[test]
fn issue_5910_resolution_recheck_keeps_original_zone_change_record_lki() {
    let condition = TriggerCondition::HadCounters {
        counter_type: Some(CounterType::Plus1Plus1),
    };
    let (mut runner, observer, subject) = observer_fixture(condition);
    runner
        .state_mut()
        .objects
        .get_mut(&subject)
        .expect("subject exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    let first_departure = move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
    process_triggers(runner.state_mut(), std::slice::from_ref(&first_departure));
    assert_eq!(
        observer_trigger_events(runner.state(), observer),
        vec![first_departure],
        "positive reach guard: the countered first departure must reach the stack"
    );

    move_and_capture_zone_change(&mut runner, subject, Zone::Battlefield);
    let later_counterless_departure =
        move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
    let later_record = match &later_counterless_departure {
        GameEvent::ZoneChanged { record, .. } => record,
        _ => unreachable!("helper returns ZoneChanged"),
    };
    assert!(
        later_record
            .trigger_source_context()
            .expect("real zone change carries context")
            .lki
            .counters
            .is_empty(),
        "hostile later incarnation must overwrite the cache with counterless LKI"
    );
    assert!(
        runner.state().lki_cache[&subject].counters.is_empty(),
        "the latest ObjectId-keyed cache entry must disagree with the stacked trigger's event"
    );

    let life_before = runner.state().players[0].life;
    runner
        .act(GameAction::PassPriority)
        .expect("first priority pass succeeds");
    runner.advance_until_stack_empty();

    // CR 603.4: Resolution rechecks the same intervening-if against the event
    // that caused this trigger, so the later counterless incarnation is irrelevant.
    assert_eq!(runner.state().players[0].life, life_before + 3);
    assert!(runner.state().stack.is_empty());
}

#[test]
fn issue_5910_invalid_zone_change_provenance_suppresses_negated_had_counters() {
    let condition = TriggerCondition::Not {
        condition: Box::new(TriggerCondition::HadCounters {
            counter_type: Some(CounterType::Plus1Plus1),
        }),
    };

    let (mut coherent_runner, coherent_observer, coherent_subject) =
        observer_fixture(condition.clone());
    let coherent =
        move_and_capture_zone_change(&mut coherent_runner, coherent_subject, Zone::Graveyard);
    process_triggers(coherent_runner.state_mut(), std::slice::from_ref(&coherent));
    assert_eq!(
        observer_trigger_events(coherent_runner.state(), coherent_observer),
        vec![coherent],
        "positive reach guard: coherent counterless provenance must satisfy Not(HadCounters)"
    );

    for mismatch in [
        ProvenanceMismatch::RecordObject,
        ProvenanceMismatch::RecordFrom,
        ProvenanceMismatch::RecordTo,
        ProvenanceMismatch::ContextObject,
        ProvenanceMismatch::ContextExpectedZone,
    ] {
        let (mut runner, observer, subject) = observer_fixture(condition.clone());
        let mut event = move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
        introduce_provenance_mismatch(&mut event, mismatch, observer);
        process_triggers(runner.state_mut(), std::slice::from_ref(&event));
        assert!(
            observer_trigger_events(runner.state(), observer).is_empty(),
            "{mismatch:?} must fail the outer provenance gate before Not can invert HadCounters"
        );
    }
}

#[test]
fn issue_5910_legacy_contextless_countered_cache_matches_had_counters() {
    let condition = TriggerCondition::HadCounters {
        counter_type: Some(CounterType::Plus1Plus1),
    };
    let (mut runner, observer, subject) = observer_fixture(condition);
    runner
        .state_mut()
        .objects
        .get_mut(&subject)
        .expect("subject exists")
        .counters
        .insert(CounterType::Plus1Plus1, 1);
    let mut event = move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
    clear_zone_change_context(&mut event);

    process_triggers(runner.state_mut(), std::slice::from_ref(&event));
    assert_eq!(
        observer_trigger_events(runner.state(), observer),
        vec![event],
        "a legacy contextless record retains the lower-fidelity cache fallback"
    );
}

#[test]
fn issue_5910_legacy_contextless_counterless_cache_rejects_had_counters() {
    let condition = TriggerCondition::HadCounters {
        counter_type: Some(CounterType::Plus1Plus1),
    };
    let (mut runner, observer, subject) = observer_fixture(condition);
    let mut event = move_and_capture_zone_change(&mut runner, subject, Zone::Graveyard);
    clear_zone_change_context(&mut event);

    process_triggers(runner.state_mut(), std::slice::from_ref(&event));
    assert!(
        observer_trigger_events(runner.state(), observer).is_empty(),
        "the paired countered test proves this negative reaches the legacy cache fallback"
    );
}
