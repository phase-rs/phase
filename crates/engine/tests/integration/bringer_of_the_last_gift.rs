use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Flying\nWhen this creature enters, if you cast it, each player sacrifices all other creatures they control. Then each player returns all creature cards from their graveyard that weren't put there this way to the battlefield.";

struct Setup {
    runner: GameRunner,
    bringer: ObjectId,
    prior_graveyard_creatures: [ObjectId; 2],
    sacrificed_creatures: [ObjectId; 2],
}

fn setup() -> Setup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let prior_graveyard_creatures = [
        scenario
            .add_creature_to_graveyard(P0, "Prior P0 Creature", 2, 2)
            .id(),
        scenario
            .add_creature_to_graveyard(P1, "Prior P1 Creature", 2, 2)
            .id(),
    ];
    let sacrificed_creatures = [
        scenario.add_creature(P0, "P0 Creature", 2, 2).id(),
        scenario.add_creature(P1, "P1 Creature", 2, 2).id(),
    ];
    Setup {
        runner: scenario.build(),
        bringer,
        prior_graveyard_creatures,
        sacrificed_creatures,
    }
}

fn cast_and_resolve_bringer(setup: &mut Setup) {
    setup.runner.cast(setup.bringer).resolve();
    setup.runner.advance_until_stack_empty();
    assert!(
        matches!(
            setup.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ),
        "Bringer's cast trigger should resolve without prompting"
    );
}

#[test]
fn returns_creatures_that_were_already_in_each_graveyard() {
    let mut setup = setup();
    cast_and_resolve_bringer(&mut setup);

    for creature in setup.prior_graveyard_creatures {
        assert_eq!(
            setup.runner.state().objects[&creature].zone,
            Zone::Battlefield,
            "creatures already in graveyards should return"
        );
    }
}

#[test]
fn does_not_return_creatures_put_into_graveyards_by_its_sacrifice() {
    let mut setup = setup();
    cast_and_resolve_bringer(&mut setup);

    for creature in setup.sacrificed_creatures {
        assert_eq!(
            setup.runner.state().objects[&creature].zone,
            Zone::Graveyard,
            "creatures sacrificed by this trigger must be excluded from its return"
        );
    }
}
