//! A dies-trigger LKI restoration must leave only serializable trigger entries.

use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::{TriggerDefinitionOccurrenceRef, TriggerEntry};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::resolution::ResolutionStateWire;
use engine::types::resolved_commands::{ResolvedRulesCommand, ResolvedZoneChangeCommand};

const DIES_TRIGGER: &str = "When this creature dies, create a 1/1 green Squirrel creature token.";

fn drain_to_priority(runner: &mut GameRunner) {
    for _ in 0..256 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty()
        {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
    panic!(
        "dies trigger did not drain; waiting_for={:?}, stack={}",
        runner.state().waiting_for,
        runner.state().stack.len()
    );
}

fn assert_materialized(label: &str, entries: impl IntoIterator<Item = TriggerEntry>) {
    for entry in entries {
        assert!(
            !matches!(
                entry.occurrence,
                TriggerDefinitionOccurrenceRef::Unmaterialized
            ),
            "{label} retained an Unmaterialized trigger entry"
        );
    }
}

fn erase_trigger_occurrences(record: &mut serde_json::Value) {
    let source = record["trigger_definitions"]
        .as_array()
        .expect("fixture zone-change record has trigger entries");
    assert!(
        !source.is_empty(),
        "fixture record must carry trigger entries for the erase to emulate a legacy payload"
    );
    let entries = source
        .iter()
        .map(|entry| entry["definition"].clone())
        .collect();
    record["trigger_definitions"] = serde_json::Value::Array(entries);
}

fn journal_zone_change_record_mut(
    wire: &mut serde_json::Value,
    object_id: u64,
) -> &mut serde_json::Value {
    wire["resolved_rules_journal"]["entries"]
        .as_array_mut()
        .expect("journal entries serialize as an array")
        .iter_mut()
        .find_map(|entry| {
            let record = entry
                .get_mut("command")?
                .get_mut("ZoneChange")?
                .get_mut("zone_change_record");
            record.filter(|record| record["object_id"] == object_id)
        })
        .expect("fixture journal retains a zone-change command")
}

fn triggered_stack_event_record_mut(wire: &mut serde_json::Value) -> &mut serde_json::Value {
    wire["stack"]
        .as_array_mut()
        .expect("stack serializes as an array")
        .iter_mut()
        .find_map(|entry| {
            entry
                .get_mut("kind")?
                .get_mut("data")?
                .get_mut("trigger_event")?
                .get_mut("data")?
                .get_mut("record")
        })
        .expect("fixture places the dies trigger on the stack")
}

#[test]
fn dies_lki_trigger_restoration_keeps_game_state_serializable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let dying = scenario
        .add_creature_from_oracle(P0, "LKI Trigger Bear", 1, 1, DIES_TRIGGER)
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&dying)
        .expect("dying source exists")
        .damage_marked = 99;
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(&mut runner);

    let state = runner.state();
    let dies_record = state
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == dying)
        .expect("dies event retains its zone-change LKI record");
    assert!(
        !dies_record.trigger_definitions.is_empty(),
        "the source's dies trigger must survive in the LKI record used for off-zone restoration"
    );

    for (object_id, object) in &state.objects {
        assert_materialized(
            &format!("object {object_id:?}"),
            object.trigger_definitions.iter_unchecked().cloned(),
        );
    }
    for (ledger, records) in [
        ("created_tokens_this_turn", &state.created_tokens_this_turn),
        (
            "sacrificed_permanents_this_turn",
            &state.sacrificed_permanents_this_turn,
        ),
        ("zone_changes_this_turn", &state.zone_changes_this_turn),
    ] {
        for (index, record) in records.iter().enumerate() {
            assert_materialized(
                &format!("{ledger}[{index}]"),
                record.trigger_definitions.clone(),
            );
        }
    }

    serde_json::to_string(state)
        .expect("a game state after dies-trigger LKI restoration must serialize");
}

#[test]
fn legacy_zone_change_trigger_records_restore_before_client_serialization() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let dying = scenario
        .add_creature_from_oracle(P0, "LKI Trigger Bear", 1, 1, DIES_TRIGGER)
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&dying)
        .expect("dying source exists")
        .damage_marked = 99;
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_to_priority(&mut runner);

    let record = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == dying)
        .expect("dies record exists")
        .clone();
    let source = record
        .trigger_source_context
        .as_ref()
        .expect("dies record retains exact source context")
        .identity
        .reference;
    let cause = runner
        .state_mut()
        .resolved_rules_journal
        .begin_proposal()
        .expect("fixture opens a journal proposal");
    runner
        .state_mut()
        .resolved_rules_journal
        .record_zone_change(ResolvedZoneChangeCommand {
            object: source,
            resulting_incarnation: source.incarnation + 1,
            from: record.from_zone.expect("dies source left a zone"),
            to: record.to_zone,
            destination_position: 0,
            owner: record.owner,
            entry_timestamp: None,
            turn_zone_change_index: record.turn_zone_change_index,
            zone_change_record: record.clone(),
            cause,
        })
        .expect("fixture journals the dies zone change");
    let pending_event = GameEvent::ZoneChanged {
        object_id: dying,
        from: record.from_zone,
        to: record.to_zone,
        record: Box::new(record),
    };
    runner.state_mut().current_trigger_event = Some(pending_event.clone());
    runner.state_mut().pending_trigger_event_batch = vec![pending_event];

    let mut wire =
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
            .expect("materialized state serializes as a resolution wire");
    let state = wire.as_object_mut().expect("wire is a state object");
    let mut legacy_record = state["zone_changes_this_turn"]
        .as_array()
        .expect("zone-change ledger serializes as an array")[0]
        .clone();
    erase_trigger_occurrences(&mut legacy_record);
    state["zone_changes_this_turn"] = serde_json::Value::Array(vec![legacy_record.clone()]);
    state.insert(
        "created_tokens_this_turn".to_string(),
        serde_json::Value::Array(vec![legacy_record.clone()]),
    );
    state.insert(
        "sacrificed_permanents_this_turn".to_string(),
        serde_json::Value::Array(vec![legacy_record]),
    );
    erase_trigger_occurrences(&mut state["current_trigger_event"]["data"]["record"]);
    erase_trigger_occurrences(&mut state["pending_trigger_event_batch"][0]["data"]["record"]);
    erase_trigger_occurrences(journal_zone_change_record_mut(&mut wire, dying.0));

    let mut context_free_journal = wire.clone();
    journal_zone_change_record_mut(&mut context_free_journal, dying.0)
        .as_object_mut()
        .expect("journal record is an object")
        .remove("trigger_source_context");
    let error = serde_json::from_value::<ResolutionStateWire>(context_free_journal)
        .expect_err("a legacy journal record must not borrow a live object's trigger base");
    assert!(
        error.to_string().contains(
            "legacy journal zone-change record has no record-owned trigger source context"
        ),
        "journal migration rejects context-free legacy trigger payloads"
    );

    let mut noninitial_base = wire.clone();
    noninitial_base["zone_changes_this_turn"][0]
        .as_object_mut()
        .expect("ledger record is an object")
        .remove("trigger_source_context");
    noninitial_base["objects"][dying.0.to_string()]["trigger_base_set_instance"] =
        serde_json::Value::from(2);
    let error = serde_json::from_value::<ResolutionStateWire>(noninitial_base)
        .expect_err("context-free legacy records require the initial printed base set");
    assert!(
        error
            .to_string()
            .contains("legacy zone-change record requires the initial printed trigger base set"),
        "live fallback rejects noninitial trigger base generations"
    );

    let mut malformed_context = wire.clone();
    malformed_context["zone_changes_this_turn"][0]["trigger_source_context"]["identity"]
        ["reference"]["object_id"] = serde_json::Value::from(dying.0 + 1);
    let error = serde_json::from_value::<ResolutionStateWire>(malformed_context)
        .expect_err("legacy records require a source context for the exact record object");
    assert!(
        error
            .to_string()
            .contains("zone-change trigger source context object id does not match its record"),
        "context identity mismatches fail closed"
    );

    let restored = serde_json::from_value::<ResolutionStateWire>(wire)
        .expect("legacy LKI trigger records restore through their exact source context")
        .into_game_state();
    let client = serde_json::to_value(ClientGameStateRef::wrap(&restored, Some(P0)))
        .expect("restored state serializes for the browser");
    assert!(
        client["state"].get("resolved_rules_journal").is_none(),
        "client serialization continues to omit the private resolved-rules journal"
    );
    for records in [
        &restored.created_tokens_this_turn,
        &restored.sacrificed_permanents_this_turn,
        &restored.zone_changes_this_turn,
    ] {
        for record in records {
            assert_materialized("restored ledger", record.trigger_definitions.clone());
        }
    }
    let mut restored_live_zone_change_events = 0;
    for event in restored
        .current_trigger_event
        .iter()
        .chain(restored.pending_trigger_event_batch.iter())
    {
        if let GameEvent::ZoneChanged { record, .. } = event {
            assert_materialized("restored live event", record.trigger_definitions.clone());
            restored_live_zone_change_events += 1;
        }
    }
    assert_eq!(
        restored_live_zone_change_events, 2,
        "both live zone-change event roots must survive restoration"
    );
}

#[test]
fn legacy_ceased_token_trigger_payloads_restore_after_trigger_is_stacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let dying = scenario
        .add_creature_from_oracle(P0, "Ephemeral Trigger Token", 1, 1, DIES_TRIGGER)
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&dying)
        .expect("token source exists")
        .is_token = true;
    runner
        .state_mut()
        .objects
        .get_mut(&dying)
        .expect("token source exists")
        .damage_marked = 99;
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    assert!(
        !runner.state().objects.contains_key(&dying),
        "a token source ceases to exist after dying"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "the dies trigger is already an independent ability on the stack"
    );
    let record = runner
        .state()
        .zone_changes_this_turn
        .iter()
        .find(|record| record.object_id == dying)
        .expect("token death remains in the current-turn ledger")
        .clone();
    let source = record
        .trigger_source_context
        .as_ref()
        .expect("token death retains its exact source context")
        .identity
        .reference;
    let cause = runner
        .state_mut()
        .resolved_rules_journal
        .begin_proposal()
        .expect("fixture opens a journal proposal");
    runner
        .state_mut()
        .resolved_rules_journal
        .record_zone_change(ResolvedZoneChangeCommand {
            object: source,
            resulting_incarnation: source.incarnation + 1,
            from: record.from_zone.expect("dies source left a zone"),
            to: record.to_zone,
            destination_position: 0,
            owner: record.owner,
            entry_timestamp: None,
            turn_zone_change_index: record.turn_zone_change_index,
            zone_change_record: record,
            cause,
        })
        .expect("fixture journals the token death");

    let mut wire =
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
            .expect("token fixture serializes as a resolution wire");
    let state = wire.as_object_mut().expect("wire is a state object");
    let mut legacy_record = state["zone_changes_this_turn"]
        .as_array()
        .expect("zone-change ledger serializes as an array")
        .iter()
        .find(|record| record["object_id"] == dying.0)
        .expect("token death remains in the current-turn ledger")
        .clone();
    legacy_record
        .as_object_mut()
        .expect("ledger record is an object")
        .remove("trigger_source_context");
    erase_trigger_occurrences(&mut legacy_record);
    state["zone_changes_this_turn"] = serde_json::Value::Array(vec![legacy_record.clone()]);
    state.insert(
        "created_tokens_this_turn".to_string(),
        serde_json::Value::Array(vec![legacy_record.clone()]),
    );
    state.insert(
        "sacrificed_permanents_this_turn".to_string(),
        serde_json::Value::Array(vec![legacy_record]),
    );
    let stacked_record = triggered_stack_event_record_mut(&mut wire);
    stacked_record
        .as_object_mut()
        .expect("stacked trigger event record is an object")
        .remove("trigger_source_context");
    erase_trigger_occurrences(stacked_record);
    let journal_record = journal_zone_change_record_mut(&mut wire, dying.0);
    journal_record
        .as_object_mut()
        .expect("journal record is an object")
        .remove("trigger_source_context");
    erase_trigger_occurrences(journal_record);

    let mut active_event = wire.clone();
    active_event["current_trigger_event"] =
        active_event["stack"][0]["kind"]["data"]["trigger_event"].clone();
    let error = serde_json::from_value::<ResolutionStateWire>(active_event)
        .expect_err("an active legacy event must retain strict source provenance");
    assert!(
        error
            .to_string()
            .contains("legacy zone-change record has no same-id persisted object base set"),
        "only historical ledgers and stacked triggers may prune a ceased source payload"
    );

    let restored = serde_json::from_value::<ResolutionStateWire>(wire)
        .expect("ceased-source historical payloads no longer block reload")
        .into_game_state();
    for records in [
        &restored.created_tokens_this_turn,
        &restored.sacrificed_permanents_this_turn,
        &restored.zone_changes_this_turn,
    ] {
        assert!(
            records
                .iter()
                .all(|record| record.trigger_definitions.is_empty()),
            "a ceased token source keeps no fabricated trigger occurrence"
        );
    }
    let stacked_event = restored
        .stack
        .iter()
        .find_map(|entry| match &entry.kind {
            engine::types::game_state::StackEntryKind::TriggeredAbility {
                trigger_event: Some(GameEvent::ZoneChanged { record, .. }),
                ..
            } => Some(record),
            _ => None,
        })
        .expect("the already-stacked token trigger survives restoration");
    assert!(
        stacked_event.trigger_definitions.is_empty(),
        "the stacked trigger retains its ability, not an invented source occurrence"
    );
    let journal_records = restored
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| match entry.command.as_ref() {
            Some(ResolvedRulesCommand::ZoneChange(command)) => Some(&command.zone_change_record),
            _ => None,
        })
        .filter(|record| record.object_id == dying)
        .collect::<Vec<_>>();
    assert!(
        !journal_records.is_empty(),
        "fixture retains the historical token-death journal record"
    );
    assert!(
        journal_records
            .iter()
            .all(|record| record.trigger_definitions.is_empty()),
        "the historical journal keeps no fabricated token trigger occurrence"
    );
    serde_json::to_value(ClientGameStateRef::wrap(&restored, Some(P0)))
        .expect("the restored state remains serializable for the browser");
}
