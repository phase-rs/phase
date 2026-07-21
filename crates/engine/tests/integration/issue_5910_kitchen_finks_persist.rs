//! Regression for issue #5910: Persist must use each death event's exact
//! source incarnation and last known counter state.
//!
//! https://github.com/phase-rs/phase/issues/5910

use engine::database::synthesis::KeywordTriggerInstaller;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{TriggerDefinitionOccurrenceRef, TriggerDefinitionRef, TriggerEntry};
use engine::types::events::GameEvent;
use engine::types::game_state::{WaitingFor, ZoneChangeRecord};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
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

    // CR 603.4: Persist's intervening-if is false at the second death, so it
    // does not trigger. Only the second Lightning Bolt may be pushed.
    let stack_pushes = second
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::StackPushed { object_id } => Some(*object_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(stack_pushes, vec![bolt_2]);

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
