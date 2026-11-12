//! CR 400.7: "for each permanent you control that you've put a counter on this
//! turn" counts current objects. A permanent that got a counter and then left
//! and re-entered the battlefield is a new object and no longer counts.
//! La Forge, Thoughtful as Ever (Scryfall oracle text) is driven through the
//! real cast/activation pipeline.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LA_FORGE: &str = "{T}: For each permanent you control that you've put a counter on this turn, create a 1/1 blue Faerie creature token with flying.";
const BATTLEGROWTH: &str = "Put a +1/+1 counter on target creature.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";

struct Board {
    runner: GameRunner,
    la_forge: ObjectId,
    bear: ObjectId,
    growth: ObjectId,
    shift: ObjectId,
}

fn mana(ty: ManaType) -> ManaUnit {
    ManaUnit::new(ty, ObjectId(0), false, vec![])
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let la_forge = scenario
        .add_creature_from_oracle(P0, "La Forge, Thoughtful as Ever", 2, 4, LA_FORGE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        })
        .id();
    let shift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(P0, vec![mana(ManaType::Green), mana(ManaType::White)]);
    Board {
        runner: scenario.build(),
        la_forge,
        bear,
        growth,
        shift,
    }
}

fn faeries(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == "Faerie" && o.zone == Zone::Battlefield && o.controller == P0)
        .count()
}

fn put_counter_on_bear(b: &mut Board) {
    b.runner.cast(b.growth).target_object(b.bear).resolve();
    assert_eq!(
        b.runner.state().objects[&b.bear]
            .counters
            .values()
            .sum::<u32>(),
        1,
        "reach guard: the real spell put the counter"
    );
}

fn activate_la_forge(b: &mut Board) {
    b.runner.activate(b.la_forge, 0).resolve();
}

#[test]
fn la_forge_makes_a_faerie_for_a_permanent_countered_this_turn() {
    let mut b = board();
    put_counter_on_bear(&mut b);
    activate_la_forge(&mut b);
    assert_eq!(faeries(&b.runner), 1);
}

#[test]
fn la_forge_ignores_a_permanent_that_was_blinked_after_getting_the_counter() {
    let mut b = board();
    put_counter_on_bear(&mut b);
    b.runner.cast(b.shift).target_object(b.bear).resolve();
    let bear = &b.runner.state().objects[&b.bear];
    assert_eq!(
        bear.zone,
        Zone::Battlefield,
        "reach guard: the Bear returned"
    );
    assert!(
        bear.counters.values().all(|n| *n == 0),
        "reach guard: the returned Bear is a new object without the counter"
    );
    activate_la_forge(&mut b);
    assert_eq!(faeries(&b.runner), 0);
}
