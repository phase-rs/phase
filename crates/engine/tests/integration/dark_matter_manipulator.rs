//! Runtime regression for Dark Matter Manipulator (Reality Fracture #52).
//!
//! Oracle text:
//! > When this creature enters, mill three cards.
//! > This creature gets +2/+0 for every seven cards in your graveyard.
//!
//! CR 107.1a + CR 608.2c: only complete groups of seven count. CR 109.5 binds
//! "your" in this static ability to its current controller. CR 611.3a-b and
//! CR 613.4c: the +2 power effect updates continuously as the graveyard changes.
//! The ETB mill sentence remains in the fixture so it carries the card's full
//! Oracle text while this test isolates the static ability.

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DARK_MATTER_MANIPULATOR_ORACLE: &str = "When this creature enters, mill three cards.\n\
This creature gets +2/+0 for every seven cards in your graveyard.";

/// Add actual card objects of several card types to one player's graveyard.
fn add_mixed_graveyard(scenario: &mut GameScenario, player: PlayerId, label: &str, count: usize) {
    for index in 0..count {
        let name = format!("{label} graveyard card {index}");
        match index % 4 {
            0 => {
                scenario.add_creature_to_graveyard(player, &name, 1, 1);
            }
            1 => {
                scenario.add_land_to_graveyard(player, &name);
            }
            2 => {
                scenario.add_artifact_to_graveyard(player, &name);
            }
            _ => {
                scenario.add_spell_to_graveyard(player, &name, (index / 4) % 2 == 0);
            }
        }
    }
}

fn setup_with_graveyards(own_count: usize) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let manipulator = scenario
        .add_creature_from_oracle(
            P0,
            "Dark Matter Manipulator",
            1,
            2,
            DARK_MATTER_MANIPULATOR_ORACLE,
        )
        .id();
    let ordinary_creature = scenario.add_creature(P0, "Ordinary Creature", 2, 2).id();

    add_mixed_graveyard(&mut scenario, P0, "Your", own_count);
    add_mixed_graveyard(&mut scenario, P1, "Opponent", 14);

    let mut runner = scenario.build();
    flush_layers(runner.state_mut());
    (runner, manipulator, ordinary_creature)
}

fn effective_pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let object = &runner.state().objects[&id];
    (
        object.power.expect("creature has power"),
        object.toughness.expect("creature has toughness"),
    )
}

#[test]
fn dark_matter_manipulator_counts_its_controller_graveyard_thresholds() {
    // The opponent always has 14 cards. The 0/6 rows distinguish the
    // controller-scoped "your graveyard" count from an opponent-scoped count;
    // mixed object types verify that this counts cards without a type filter.
    for (own_count, expected_power) in [(0, 1), (6, 1), (7, 3), (13, 3), (14, 5)] {
        let (runner, manipulator, ordinary_creature) = setup_with_graveyards(own_count);
        assert_eq!(
            effective_pt(&runner, manipulator),
            (expected_power, 2),
            "{own_count} own graveyard cards and 14 opposing cards"
        );
        assert_eq!(
            effective_pt(&runner, ordinary_creature),
            (2, 2),
            "the static modifies only Dark Matter Manipulator"
        );
    }
}

#[test]
fn dark_matter_manipulator_updates_after_graveyard_zone_moves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let manipulator = scenario
        .add_creature_from_oracle(
            P0,
            "Dark Matter Manipulator",
            1,
            2,
            DARK_MATTER_MANIPULATOR_ORACLE,
        )
        .id();
    let _ordinary_creature = scenario.add_creature(P0, "Ordinary Creature", 2, 2).id();
    add_mixed_graveyard(&mut scenario, P0, "Your", 6);
    add_mixed_graveyard(&mut scenario, P1, "Opponent", 14);
    let seventh_card = scenario
        .add_land_to_exile(P0, "Seventh Graveyard Card")
        .id();

    let mut runner = scenario.build();
    flush_layers(runner.state_mut());
    assert_eq!(effective_pt(&runner, manipulator), (1, 2));

    // CR 611.3a-b: the layer pass sees the seventh card enter the graveyard
    // through the normal zone pipeline and reapplies the continuous effect.
    let mut events = Vec::new();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(seventh_card, Zone::Graveyard, manipulator),
        &mut events,
    ));
    assert_eq!(runner.state().objects[&seventh_card].zone, Zone::Graveyard);
    assert!(
        runner.state().layers_dirty.is_dirty(),
        "the zone pipeline must invalidate layers"
    );
    flush_layers(runner.state_mut());
    assert_eq!(effective_pt(&runner, manipulator), (3, 2));

    // The same real object leaves the graveyard through the zone pipeline, so
    // the next normal layer flush returns the source to its base 1/2.
    events.clear();
    assert!(!move_object_for_test(
        runner.state_mut(),
        ZoneMoveRequest::effect(seventh_card, Zone::Exile, manipulator),
        &mut events,
    ));
    assert_eq!(runner.state().objects[&seventh_card].zone, Zone::Exile);
    assert!(runner.state().layers_dirty.is_dirty());
    flush_layers(runner.state_mut());
    assert_eq!(effective_pt(&runner, manipulator), (1, 2));
}
