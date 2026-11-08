//! Regression for issue #7899: Blaster Hulk's cost reduction counts only the
//! energy its caster has paid or lost this turn.
//! CR 107.14: To pay {E}, a player removes one energy counter from themselves.
//! CR 601.2f: Cost reductions apply while the total cost is determined, so
//! energy held (but not spent) reduces nothing.

use engine::game::casting::effective_spell_cost;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{QuantityExpr, QuantityRef};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BLASTER_HULK: &str = "This spell costs {1} less to cast for each {E} (energy counter) you've paid or lost this turn.\nHaste\nWhenever this creature attacks, you get {E}{E}, then you may pay eight {E}. When you do, this creature deals 8 damage divided as you choose among up to eight targets.";
const LONGTUSK_CUB: &str = "Whenever this creature deals combat damage to a player, you get {E}{E} (two energy counters).\nPay {E}{E}: Put a +1/+1 counter on this creature.";

fn hulk_cost(generic: u32) -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic,
    }
}

fn pool(red: usize, colorless: usize) -> Vec<ManaUnit> {
    std::iter::repeat_n(ManaType::Red, red)
        .chain(std::iter::repeat_n(ManaType::Colorless, colorless))
        .map(|color| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn add_hulk(scenario: &mut GameScenario) -> ObjectId {
    let mut builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Blaster Hulk", 8, 8, BLASTER_HULK);
    builder
        .as_artifact_creature()
        .with_subtypes(vec!["Pirate"])
        .with_mana_cost(hulk_cost(6));
    builder.id()
}

#[test]
fn blaster_hulk_without_spent_energy_costs_full_price() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(2, 0));
    let hulk = add_hulk(&mut scenario);

    let mut runner = scenario.build();
    // Energy held but not spent this turn reduces nothing (Blaster Hulk ruling).
    runner.state_mut().players[P0.0 as usize].energy = 8;

    assert_eq!(
        effective_spell_cost(runner.state(), P0, hulk),
        Some(hulk_cost(6)),
        "no energy paid or lost this turn: no reduction"
    );
    assert!(
        runner.cast(hulk).try_resolve().is_err(),
        "Blaster Hulk must not be castable for {{R}}{{R}} without spending energy"
    );
    assert_eq!(runner.state().objects[&hulk].zone, Zone::Hand);
    assert_eq!(runner.state().players[P0.0 as usize].energy, 8);
}

#[test]
fn blaster_hulk_counts_energy_paid_earlier_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, pool(2, 4));
    let cub = {
        let mut builder = scenario.add_creature_from_oracle(P0, "Longtusk Cub", 2, 2, LONGTUSK_CUB);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        builder.id()
    };
    let hulk = add_hulk(&mut scenario);

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].energy = 2;
    assert_eq!(
        effective_spell_cost(runner.state(), P0, hulk),
        Some(hulk_cost(6)),
        "before paying, the held energy does not reduce the cost"
    );

    // CR 107.14: paying {E}{E} removes two energy counters.
    runner.activate(cub, 0).resolve();
    assert_eq!(runner.state().players[P0.0 as usize].energy, 0);
    assert_eq!(
        runner.state().players[P0.0 as usize].energy_paid_or_lost_this_turn,
        2
    );
    assert_eq!(
        effective_spell_cost(runner.state(), P0, hulk),
        Some(hulk_cost(4)),
        "two energy paid this turn reduce {{6}}{{R}}{{R}} to {{4}}{{R}}{{R}}"
    );

    let outcome = runner.cast(hulk).resolve();
    outcome.assert_zone(&[hulk], Zone::Battlefield);
    assert_eq!(
        outcome.mana_pool_total(P0),
        0,
        "the reduced cost spends exactly the six available mana"
    );
}

#[test]
fn energy_paid_last_turn_no_longer_reduces_blaster_hulk() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cub = {
        let mut builder = scenario.add_creature_from_oracle(P0, "Longtusk Cub", 2, 2, LONGTUSK_CUB);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        });
        builder.id()
    };
    let hulk = add_hulk(&mut scenario);

    let energy_paid = QuantityExpr::Ref {
        qty: QuantityRef::EnergyPaidOrLostThisTurn,
    };
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].energy = 2;
    runner.activate(cub, 0).resolve();
    assert_eq!(
        effective_spell_cost(runner.state(), P0, hulk),
        Some(hulk_cost(4)),
        "reach guard: the payment reduced the cost this turn"
    );
    assert_eq!(resolve_quantity(runner.state(), &energy_paid, P0, hulk), 2);

    let turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("declaring no attackers ends combat");
    runner.advance_to_upkeep();
    assert_eq!(
        runner.state().turn_number,
        turn + 1,
        "the game must reach the next turn"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].energy_paid_or_lost_this_turn,
        0
    );
    // CR 601.2f: the reduction reads the per-turn tracker; it is now an
    // opponent's turn, so read the quantity directly rather than a cast cost.
    assert_eq!(
        resolve_quantity(runner.state(), &energy_paid, P0, hulk),
        0,
        "\"this turn\": energy paid during an earlier turn no longer counts"
    );
}
