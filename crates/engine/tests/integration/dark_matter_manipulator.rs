//! Runtime regression for Dark Matter Manipulator (Reality Fracture #52).
//!
//! Oracle text:
//! > When this creature enters, mill three cards.
//! > This creature gets +2/+0 for every seven cards in your graveyard.
//!
//! The printed text counts complete groups of seven. CR 109.5 binds "your" to
//! this creature's current controller. The power modifier applies in layer 7c
//! (CR 613.4c) and updates as the graveyard changes while this creature remains
//! on the battlefield and has the ability (CR 604.2 + CR 611.3a + CR 611.3b).
//! The public-action regression resolves the ETB mill and a later Cremate
//! exile so the static ability is checked across real graveyard changes.

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const DARK_MATTER_MANIPULATOR_ORACLE: &str = "When this creature enters, mill three cards.\n\
This creature gets +2/+0 for every seven cards in your graveyard.";

const CREMATE_ORACLE: &str = "Exile target card from a graveyard. Draw a card.";

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

fn graveyard_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|state| state.id == player)
        .expect("player exists")
        .graveyard
        .len()
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
fn dark_matter_manipulator_updates_after_public_mill_and_cremate_actions() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let manipulator = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Dark Matter Manipulator",
            1,
            2,
            DARK_MATTER_MANIPULATOR_ORACLE,
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let cremate = scenario
        .add_spell_to_hand_from_oracle(P1, "Cremate", true, CREMATE_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 0,
        })
        .id();
    add_mixed_graveyard(&mut scenario, P0, "Your", 4);
    let milled_cards = [
        scenario.add_card_to_library_top(P0, "Milled One"),
        scenario.add_card_to_library_top(P0, "Milled Two"),
        scenario.add_card_to_library_top(P0, "Milled Three"),
    ];
    scenario.add_card_to_library_top(P1, "Cremate Draw");
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![])],
    );

    let mut runner = scenario.build();
    runner.cast(manipulator).resolve();
    for card in milled_cards {
        assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
    }
    assert_eq!(graveyard_len(&runner, P0), 7, "the ETB mill reaches seven");
    assert_eq!(effective_pt(&runner, manipulator), (3, 2));
    assert!(runner.state().stack.is_empty(), "the ETB trigger resolved");
    assert_eq!(runner.state().priority_player, P0);

    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes priority to P1");
    assert_eq!(runner.state().priority_player, P1);

    let cremate_outcome = runner
        .cast(cremate)
        .target_object(milled_cards[0])
        .resolve();
    assert_eq!(runner.state().objects[&milled_cards[0]].zone, Zone::Exile);
    assert_eq!(graveyard_len(&runner, P0), 6);
    assert_eq!(effective_pt(&runner, manipulator), (1, 2));
    cremate_outcome.assert_hand_drawn(P1, 1);
    assert!(runner.state().stack.is_empty());
    assert_eq!(runner.state().priority_player, P0);
}
