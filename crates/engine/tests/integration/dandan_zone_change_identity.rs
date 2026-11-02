//! Dandân shared pile: a zone-change record keeps the departure snapshot (owner, controller,
//! source context) apart from the identity the move installs on the destination object.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::visibility::filter_events_for_viewer;
use engine::game::zones::apply_resolved_zone_change;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, ZoneChangeRecord};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{ResolvedRulesCommand, ResolvedZoneChangeCommand};
use engine::types::zones::Zone;

use super::dandan_filter_owner_axis::{dandan, scenario, stage, start};
use crate::support::shared_card_db;

fn other(seat: PlayerId) -> PlayerId {
    if seat == P0 {
        P1
    } else {
        P0
    }
}

fn record_to(state: &GameState, id: ObjectId, to: Zone) -> ZoneChangeRecord {
    state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|record| record.object_id == id && record.to_zone == to)
        .cloned()
        .unwrap_or_else(|| panic!("reach: a record of the move to {to:?} exists"))
}

fn departure(record: &ZoneChangeRecord) -> (PlayerId, PlayerId) {
    (record.owner, record.controller)
}

fn arrival(record: &ZoneChangeRecord) -> (PlayerId, PlayerId) {
    (record.arrival.owner, record.arrival.controller)
}

fn source_context_lki(record: &ZoneChangeRecord) -> (PlayerId, PlayerId) {
    let context = record
        .trigger_source_context()
        .expect("a live move carries its source context");
    (context.lki.owner, context.lki.controller)
}

fn command_to(state: &GameState, id: ObjectId, to: Zone) -> ResolvedZoneChangeCommand {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ZoneChange(command)
                if command.object.object_id == id && command.to == to =>
            {
                Some((**command).clone())
            }
            _ => None,
        })
        .expect("reach: the move is journaled")
}

fn state_round_trip(state: &GameState) -> GameState {
    let wire = serde_json::to_string(state).expect("the state serializes");
    serde_json::from_str(&wire).expect("the journaled state validates and deserializes")
}

fn reveal_top(runner: &mut GameRunner) {
    engine::game::derived::sync_continuous_reveals(runner.state_mut());
}

fn future_sight_board(
    db: &CardDatabase,
    format: FormatConfig,
    zone_cards: &[(PlayerId, &str)],
    caster: PlayerId,
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = scenario(format);
    let staged = stage(&mut scenario, db, Zone::Library, zone_cards);
    scenario.add_real_card(caster, "Future Sight", Zone::Battlefield, db);
    let mut runner = start(scenario, caster);
    reveal_top(&mut runner);
    (runner, staged)
}

#[test]
fn a_stack_cast_by_the_non_holder_keeps_the_departure_snapshot() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let (mut runner, staged) = future_sight_board(
            db,
            dandan(),
            &[(pile, "Lightning Bolt"), (pile, "Island")],
            caster,
        );
        let bolt = staged[0];
        assert_eq!(
            runner.state().objects[&bolt].owner,
            pile,
            "reach: staged owner"
        );
        runner
            .cast(bolt)
            .target_player(pile)
            .try_resolve()
            .expect("reach: the cast resolves");

        let record = record_to(runner.state(), bolt, Zone::Stack);
        assert_eq!(
            record.from_zone,
            Some(Zone::Library),
            "reach: cast from the pile"
        );
        assert_eq!(departure(&record), (pile, pile), "departure snapshot");
        assert_eq!(source_context_lki(&record), (pile, pile), "source context");
        assert_eq!(arrival(&record), (caster, caster), "arrival identity");
        assert_eq!(
            runner.state().objects[&bolt].owner,
            caster,
            "installed owner"
        );
    }
}

#[test]
fn a_cast_by_the_stamp_holder_has_arrival_equal_departure() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, staged) =
        future_sight_board(db, dandan(), &[(P0, "Lightning Bolt"), (P0, "Island")], P0);
    let bolt = staged[0];
    runner
        .cast(bolt)
        .target_player(P1)
        .try_resolve()
        .expect("reach: the cast resolves");
    let record = record_to(runner.state(), bolt, Zone::Stack);
    assert_eq!(record.from_zone, Some(Zone::Library), "reach");
    assert_eq!(departure(&record), (P0, P0));
    assert_eq!(arrival(&record), departure(&record));
}

#[test]
fn a_land_play_by_the_non_holder_keeps_the_departure_snapshot_and_journals_the_rebind() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let (mut runner, staged) =
            future_sight_board(db, dandan(), &[(pile, "Island"), (pile, "Island")], caster);
        let land = staged[0];
        let pre = runner.state().clone();
        let card_id = runner.state().objects[&land].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .expect("the land play is legal");
        let state = runner.state();

        let record = record_to(state, land, Zone::Battlefield);
        assert_eq!(record.from_zone, Some(Zone::Library), "reach");
        assert_eq!(departure(&record), (pile, pile), "departure snapshot");
        assert_eq!(source_context_lki(&record), (pile, pile), "source context");
        assert_eq!(arrival(&record), (caster, caster), "arrival identity");
        assert_eq!(
            (state.objects[&land].owner, state.objects[&land].controller),
            (caster, caster),
            "installed identity"
        );

        let command = command_to(state, land, Zone::Battlefield);
        assert_eq!(command.owner, caster);
        assert_eq!(command.rebound_from, Some(pile), "reach: the move rebound");
        assert_eq!(departure(&command.zone_change_record), (pile, pile));
        assert_eq!(arrival(&command.zone_change_record), (caster, caster));
        state_round_trip(state);

        let mut replay = pre;
        apply_resolved_zone_change(&mut replay, &command).expect("replays on its pre-state");
        let replayed = record_to(&replay, land, Zone::Battlefield);
        assert_eq!(departure(&replayed), departure(&record));
        assert_eq!(arrival(&replayed), arrival(&record));
    }
}

#[test]
fn a_draw_by_the_non_holder_keeps_the_departure_snapshot() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let mut scenario = scenario(dandan());
        let drawn = stage(
            &mut scenario,
            db,
            Zone::Library,
            &[
                (pile, "Island"),
                (pile, "Island"),
                (pile, "Island"),
                (pile, "Island"),
            ],
        );
        let brainstorm = scenario.add_real_card(caster, "Brainstorm", Zone::Hand, db);
        let mut runner = start(scenario, caster);
        let _ = runner.cast(brainstorm).try_resolve();
        for id in drawn.iter().take(3) {
            let record = record_to(runner.state(), *id, Zone::Hand);
            assert_eq!(
                record.from_zone,
                Some(Zone::Library),
                "reach: drawn from the pile"
            );
            assert_eq!(departure(&record), (pile, pile), "departure snapshot");
            assert_eq!(source_context_lki(&record), (pile, pile), "source context");
            assert_eq!(arrival(&record), (caster, caster), "arrival identity");
            let command = command_to(runner.state(), *id, Zone::Hand);
            assert_eq!(command.rebound_from, Some(pile), "reach: the draw rebound");
        }
        state_round_trip(runner.state());
    }
}

#[test]
fn a_per_seat_draw_has_arrival_equal_departure() {
    let Some(db) = shared_card_db() else { return };
    let mut scenario = scenario(FormatConfig::standard());
    let drawn = stage(
        &mut scenario,
        db,
        Zone::Library,
        &[
            (P1, "Island"),
            (P1, "Island"),
            (P1, "Island"),
            (P1, "Island"),
        ],
    );
    let brainstorm = scenario.add_real_card(P1, "Brainstorm", Zone::Hand, db);
    let mut runner = start(scenario, P1);
    let _ = runner.cast(brainstorm).try_resolve();
    let record = record_to(runner.state(), drawn[0], Zone::Hand);
    assert_eq!(record.from_zone, Some(Zone::Library), "reach");
    assert_eq!(departure(&record), (P1, P1));
    assert_eq!(arrival(&record), departure(&record));
}

#[test]
fn a_controller_override_moves_the_arrival_controller_only() {
    let Some(db) = shared_card_db() else { return };
    for format in [dandan(), FormatConfig::standard()] {
        let mut scenario = scenario(format);
        let bears = stage(&mut scenario, db, Zone::Graveyard, &[(P0, "Grizzly Bears")])[0];
        let reanimate = scenario.add_real_card(P1, "Reanimate", Zone::Hand, db);
        let mut runner = start(scenario, P1);
        runner.cast(reanimate).target_object(bears).resolve();
        assert_eq!(
            runner.state().objects[&bears].zone,
            Zone::Battlefield,
            "reach"
        );

        let record = record_to(runner.state(), bears, Zone::Battlefield);
        assert_eq!(departure(&record), (P0, P0), "departure snapshot");
        assert_eq!(source_context_lki(&record), (P0, P0), "source context");
        assert_eq!(
            arrival(&record),
            (P0, P1),
            "owner kept, controller overridden"
        );
        assert_eq!(
            runner.state().objects[&bears].controller,
            P1,
            "reach: override fired"
        );
    }
}

#[test]
fn a_record_saved_without_an_arrival_loads_with_arrival_equal_departure() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let (mut runner, staged) =
            future_sight_board(db, dandan(), &[(pile, "Island"), (pile, "Island")], caster);
        let land = staged[0];
        let card_id = runner.state().objects[&land].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .expect("the land play is legal");
        let record = record_to(runner.state(), land, Zone::Battlefield);
        assert_ne!(
            arrival(&record),
            departure(&record),
            "reach: a rebound record"
        );

        let mut wire = serde_json::to_value(&record).expect("serializes");
        let round_trip: ZoneChangeRecord = serde_json::from_value(wire.clone()).expect("reads");
        assert_eq!(round_trip, record);

        wire.as_object_mut()
            .unwrap()
            .remove("arrival")
            .expect("the key was written");
        let legacy: ZoneChangeRecord = serde_json::from_value(wire).expect("the old shape reads");
        assert_eq!(arrival(&legacy), departure(&legacy));
        assert_eq!(departure(&legacy), departure(&record));
    }
}

#[test]
fn a_graveyard_land_play_by_the_non_holder_keeps_the_departure_snapshot() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let mut scenario = scenario(dandan());
        let land = stage(&mut scenario, db, Zone::Graveyard, &[(pile, "Island")])[0];
        scenario.add_real_card(caster, "Crucible of Worlds", Zone::Battlefield, db);
        let mut runner = start(scenario, caster);
        let card_id = runner.state().objects[&land].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: land,
                card_id,
            })
            .expect("reach: Crucible lets the seat play the pile's land");

        let record = record_to(runner.state(), land, Zone::Battlefield);
        assert_eq!(record.from_zone, Some(Zone::Graveyard), "reach");
        assert_eq!(departure(&record), (pile, pile), "departure snapshot");
        assert_eq!(source_context_lki(&record), (pile, pile), "source context");
        assert_eq!(arrival(&record), (caster, caster), "arrival identity");
        let command = command_to(runner.state(), land, Zone::Battlefield);
        assert_eq!(command.rebound_from, Some(pile), "reach: the move rebound");
        state_round_trip(runner.state());
    }
}

#[test]
fn a_pile_draw_is_visible_to_the_drawer_only() {
    let Some(db) = shared_card_db() else { return };
    for caster in [P0, P1] {
        let pile = other(caster);
        let mut scenario = scenario(dandan());
        let drawn = stage(
            &mut scenario,
            db,
            Zone::Library,
            &[
                (pile, "Island"),
                (pile, "Island"),
                (pile, "Island"),
                (pile, "Island"),
            ],
        );
        let brainstorm = scenario.add_real_card(caster, "Brainstorm", Zone::Hand, db);
        let mut runner = start(scenario, caster);
        let _ = runner.cast(brainstorm).try_resolve();
        let record = record_to(runner.state(), drawn[0], Zone::Hand);
        assert_ne!(
            departure(&record),
            arrival(&record),
            "reach: a rebound draw"
        );
        let events = [GameEvent::ZoneChanged {
            object_id: drawn[0],
            from: Some(Zone::Library),
            to: Zone::Hand,
            record: Box::new(record),
        }];
        assert_eq!(
            filter_events_for_viewer(&events, runner.state(), caster).len(),
            1,
            "the drawer sees the draw"
        );
        assert_eq!(
            filter_events_for_viewer(&events, runner.state(), pile).len(),
            0,
            "the pile's stamp holder does not"
        );
    }
}
