use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "Flying\nWhen this creature enters, if you cast it, each player sacrifices all other creatures they control. Then each player returns all creature cards from their graveyard that weren't put there this way to the battlefield.";
const MURDER_ORACLE: &str = "Destroy target creature.";

/// Printed cost {6}{B}{B}.
fn bringer_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 6,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    }
}

/// Printed cost {1}{B}{B}.
fn murder_cost() -> ManaCost {
    ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
    }
}

fn black_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
        .collect()
}

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
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    scenario.with_mana_pool(P0, black_mana(8));
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

fn resolve_cast_and_trigger(runner: &mut GameRunner, spell: ObjectId) {
    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Bringer's cast trigger should resolve without prompting"
    );
}

#[test]
fn returns_prior_graveyard_creatures_but_not_those_its_sacrifice_put_there() {
    let mut setup = setup();
    resolve_cast_and_trigger(&mut setup.runner, setup.bringer);

    // Reach guard: the trigger ran and returned the creatures that were
    // already in graveyards, so the negative assertion below is not vacuous.
    for creature in setup.prior_graveyard_creatures {
        assert_eq!(
            setup.runner.state().objects[&creature].zone,
            Zone::Battlefield,
            "creatures already in graveyards should return"
        );
    }
    for creature in setup.sacrificed_creatures {
        assert_eq!(
            setup.runner.state().objects[&creature].zone,
            Zone::Graveyard,
            "creatures sacrificed by this trigger must be excluded from its return"
        );
    }
}

/// An empty sacrifice publishes an empty chain set, so "this way" excludes
/// nothing on the second Bringer even though an earlier Bringer's sacrifice
/// published a non-empty set. Falling back to the stale earlier set would keep
/// the first sacrificed creature in the graveyard.
#[test]
fn empty_sacrifice_excludes_nothing_despite_an_earlier_bringers_set() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bringer_a = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let bringer_b = scenario
        .add_creature_to_hand_from_oracle(P0, "Bringer of the Last Gift", 6, 6, ORACLE)
        .with_mana_cost(bringer_cost())
        .with_subtypes(vec!["Vampire", "Demon"])
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, MURDER_ORACLE)
        .with_mana_cost(murder_cost())
        .id();
    scenario.with_mana_pool(P0, black_mana(8 + 3 + 8));
    let creature_x = scenario.add_creature(P0, "Creature X", 2, 2).id();
    let mut runner = scenario.build();

    // First Bringer sacrifices X, publishing a non-empty tracked set.
    resolve_cast_and_trigger(&mut runner, bringer_a);
    assert_eq!(
        runner.state().objects[&bringer_a].zone,
        Zone::Battlefield,
        "Bringer A stays (it sacrifices only *other* creatures)"
    );
    assert_eq!(
        runner.state().objects[&creature_x].zone,
        Zone::Graveyard,
        "X was sacrificed by Bringer A and excluded from its return"
    );

    // Murder destroys Bringer A; it publishes no tracked set.
    runner.cast(murder).target_object(bringer_a).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&bringer_a].zone,
        Zone::Graveyard,
        "Murder should destroy Bringer A"
    );

    // Second Bringer finds no other creatures: nothing is put into a graveyard
    // "this way", so both X and Bringer A return.
    resolve_cast_and_trigger(&mut runner, bringer_b);
    assert_eq!(runner.state().objects[&bringer_b].zone, Zone::Battlefield);
    for (name, id) in [("X", creature_x), ("Bringer A", bringer_a)] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Battlefield,
            "{name} was not put into a graveyard by the second sacrifice and must return"
        );
    }
}
