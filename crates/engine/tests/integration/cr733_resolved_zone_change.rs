//! CR733 P2 coverage for exact single-object zone-transition commands.

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::{apply_resolved_zone_change, move_to_zone};
use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedRulesJournal, ResolvedZoneChangeReplayInvariantError,
};
use engine::types::zones::Zone;

fn recorded_zone_change(
    state: &GameState,
    from: Zone,
    to: Zone,
) -> engine::types::resolved_commands::ResolvedZoneChangeCommand {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.from == from && command.to == to =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("ordinary move must journal the expected zone-change command")
}

fn battlefield_reentry_states() -> (GameState, GameState) {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    let creature = scenario
        .add_creature_from_oracle(P0, "Journal Entrant", 2, 2, "")
        .id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    let mut events = Vec::new();
    move_to_zone(&mut state, creature, Zone::Graveyard, &mut events);
    let pre_state = state.clone();
    move_to_zone(&mut state, creature, Zone::Battlefield, &mut events);
    (pre_state, state)
}

#[test]
fn zone_change_command_round_trips_and_replays_the_exact_transition_core() {
    let (pre_state, ordinary_state) = battlefield_reentry_states();
    let command = recorded_zone_change(&ordinary_state, Zone::Graveyard, Zone::Battlefield);

    let wire = serde_json::to_value(ResolvedRulesCommand::ZoneChange(Box::new(command.clone())))
        .expect("zone-change command serializes");
    assert_eq!(
        serde_json::from_value::<ResolvedRulesCommand>(wire)
            .expect("zone-change command deserializes"),
        ResolvedRulesCommand::ZoneChange(Box::new(command.clone()))
    );

    let journal_wire = serde_json::to_value(&ordinary_state.resolved_rules_journal)
        .expect("journal serializes with a zone-change command");
    assert_eq!(
        serde_json::from_value::<ResolvedRulesJournal>(journal_wire)
            .expect("journal validates and deserializes"),
        ordinary_state.resolved_rules_journal
    );

    let mut replay = pre_state;
    apply_resolved_zone_change(&mut replay, &command).expect("exact transition replays");

    let object = &replay.objects[&command.object.object_id];
    assert_eq!(object.zone, command.to);
    assert_eq!(object.incarnation, command.resulting_incarnation);
    assert_eq!(object.timestamp, command.entry_timestamp.unwrap());
    assert_eq!(
        replay.battlefield[command.destination_position], command.object.object_id,
        "replay installs the recorded destination position"
    );
    assert_eq!(
        replay.zone_changes_this_turn[command.turn_zone_change_index], command.zone_change_record,
        "replay appends the exact recorded per-turn zone-change entry"
    );

    // CR 613.7: installing the recorded entry timestamp is only half the
    // contract — the allocator must also be carried past it. `next_timestamp`
    // is the draw counter, so an applier that installs a value while leaving
    // the counter behind hands that same timestamp to a second object later in
    // the replay, and CR 613.7 orders effects within a layer by timestamp
    // alone. Asserted by DRAWING, so this pins the consequence, not the field.
    let installed = command.entry_timestamp.expect("a battlefield entry draws");
    let next_drawn = replay.next_timestamp();
    assert!(
        next_drawn > installed,
        "CR 613.7d: replay installed entry timestamp {installed} but the next draw handed \
         out {next_drawn}; two objects sharing a timestamp are unordered within their layer"
    );
}

/// The other side of the CR 613.7d allocator contract: a move to a zone that
/// grants no timestamp records none, so replaying it must advance no allocator.
/// This is what keeps the advance bound to a recorded draw rather than applied
/// blanket to every zone change, which would silently inflate the counter.
#[test]
fn a_zone_change_that_drew_no_timestamp_advances_no_allocator_on_replay() {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    let creature = scenario
        .add_creature_from_oracle(P0, "Journal Entrant", 2, 2, "")
        .id();
    let runner = scenario.build();

    // The predecessor is the state the graveyard move was recorded FROM, so the
    // command's own incarnation and occurrence guards are satisfied on replay.
    let pre_state = runner.state().clone();
    let mut state = pre_state.clone();
    let mut events = Vec::new();
    move_to_zone(&mut state, creature, Zone::Graveyard, &mut events);

    let to_graveyard = recorded_zone_change(&state, Zone::Battlefield, Zone::Graveyard);
    assert_eq!(
        to_graveyard.entry_timestamp, None,
        "CR 613.7d grants a timestamp on entering the battlefield; a graveyard move draws none"
    );

    let mut replay = pre_state;
    let before = replay.next_timestamp;
    apply_resolved_zone_change(&mut replay, &to_graveyard)
        .expect("the recorded graveyard move replays against the state it was recorded from");
    assert_eq!(
        replay.objects[&creature].zone,
        Zone::Graveyard,
        "reach guard: the replay actually performed the move being asserted about"
    );
    assert_eq!(
        replay.next_timestamp, before,
        "a zone change that drew no timestamp must advance no allocator on replay"
    );
}

#[test]
fn zone_change_rejects_occurrence_and_destination_position_mismatches() {
    let (pre_state, ordinary_state) = battlefield_reentry_states();
    let command = recorded_zone_change(&ordinary_state, Zone::Graveyard, Zone::Battlefield);

    let mut replay = pre_state.clone();
    apply_resolved_zone_change(&mut replay, &command).expect("first exact application succeeds");
    assert!(matches!(
        apply_resolved_zone_change(&mut replay, &command),
        Err(ResolvedZoneChangeReplayInvariantError::OccurrenceMismatch { .. })
    ));

    let mut wrong_position = command.clone();
    wrong_position.destination_position += 1;
    assert!(matches!(
        apply_resolved_zone_change(&mut pre_state.clone(), &wrong_position),
        Err(ResolvedZoneChangeReplayInvariantError::DestinationPositionMismatch { .. })
    ));
}

#[test]
fn zone_change_journal_rejects_an_unrelated_cause() {
    let (_, ordinary_state) = battlefield_reentry_states();
    let mut wire = serde_json::to_value(&ordinary_state.resolved_rules_journal)
        .expect("journal serializes before malformed-cause test");
    let entries = wire["entries"]
        .as_array_mut()
        .expect("journal stores command entries");
    let command = entries
        .iter_mut()
        .find_map(|entry| entry["command"]["ZoneChange"].as_object_mut())
        .expect("journal contains a zone-change command");
    command.insert("cause".to_string(), serde_json::json!({ "Proposal": 99 }));

    assert!(serde_json::from_value::<ResolvedRulesJournal>(wire).is_err());
}

#[test]
fn production_change_zone_pipeline_records_the_transition_command() {
    let mut scenario = GameScenario::new_n_player(2, 0x733);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature_from_oracle(P0, "Target Creature", 2, 2, "")
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Exile Test", true, "Exile target creature.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(spell).target_object(target).resolve();

    let command = recorded_zone_change(runner.state(), Zone::Battlefield, Zone::Exile);
    assert_eq!(command.object.object_id, target);
    assert_eq!(
        command.resulting_incarnation,
        command.object.incarnation + 1
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Exile);
}
