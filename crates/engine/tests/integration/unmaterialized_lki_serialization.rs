//! A dies-trigger LKI restoration must leave only serializable trigger entries.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::ability::{TriggerDefinitionOccurrenceRef, TriggerEntry};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;

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
