//! Festival of Embers — graveyard cast permission with an ADDITIONAL pay-life
//! cost (CR 601.2f).
//!
//! "During your turn, you may cast instant and sorcery spells from your
//! graveyard by paying 1 life in addition to their other costs."
//!
//! The permission keeps the spell's printed mana cost (CR 601.2f: an ADDITIONAL
//! cost, NOT an alternative one — distinct from Valgavoth's alt cost) and adds a
//! 1-life payment on top. These tests drive the real cast pipeline through the
//! scenario `GameRunner` / `SpellCast` driver and assert the 1 life is actually
//! paid.

use engine::game::casting::{
    can_cast_object_now, effective_spell_cost, spell_objects_available_to_cast,
};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const FESTIVAL_ORACLE: &str = "During your turn, you may cast instant and sorcery spells from your graveyard by paying 1 life in addition to their other costs.";

fn pool_units(colors: &[ManaType]) -> Vec<ManaUnit> {
    let dummy = engine::types::identifiers::ObjectId(0);
    colors
        .iter()
        .map(|&color| ManaUnit::new(color, dummy, false, vec![]))
        .collect()
}

/// CR 601.2f: the graveyard spell keeps its printed mana cost — the rider is an
/// ADDITIONAL cost, not an alternative one (the mana cost must NOT be zeroed).
#[test]
fn festival_keeps_printed_mana_cost() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Festival of Embers", 0, 0)
        .as_enchantment()
        .from_oracle_text(FESTIVAL_ORACLE);
    let sorcery_id = scenario
        .add_spell_to_graveyard(P0, "Searing Bolt", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Searing Bolt deals 3 damage to any target.")
        .id();
    let runner = scenario.build();

    let effective = effective_spell_cost(runner.state(), P0, sorcery_id).expect("effective cost");
    assert!(
        !effective.is_without_paying_mana(),
        "Festival is an additional cost — the spell's printed mana cost must remain, got {effective:?}"
    );
    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&sorcery_id),
        "Festival must surface the graveyard sorcery as castable"
    );
}

/// CR 601.2f + CR 119.4: end-to-end — casting the graveyard sorcery via Festival
/// pays its {R} AND 1 life. DISCRIMINATING: reverting the static's `extra_cost`
/// to `None` removes the 1-life payment, so P0's life delta would be 0, not -1.
#[test]
fn festival_cast_pays_one_life_in_addition() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Festival of Embers", 0, 0)
        .as_enchantment()
        .from_oracle_text(FESTIVAL_ORACLE);
    let sorcery_id = scenario
        .add_spell_to_graveyard(P0, "Searing Bolt", false)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Searing Bolt deals 3 damage to any target.")
        .id();
    let target = scenario.add_creature(P1, "Target", 2, 2).id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Red]));
    let mut runner = scenario.build();

    let outcome = runner.cast(sorcery_id).target_object(target).resolve();

    assert_eq!(
        outcome.life_delta(P0),
        -1,
        "casting via Festival must pay 1 life in addition to the {{R}} mana cost"
    );
}

/// CR 117.1c: the "During your turn" qualifier gates the permission to P0's
/// turn — on P1's turn P0 may not cast the graveyard spell via Festival.
#[test]
fn festival_only_functions_on_controllers_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain).with_life(P0, 20);
    scenario
        .add_creature(P0, "Festival of Embers", 0, 0)
        .as_enchantment()
        .from_oracle_text(FESTIVAL_ORACLE);
    let sorcery_id = scenario
        .add_spell_to_graveyard(P0, "Searing Bolt", true)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .from_oracle_text("Searing Bolt deals 3 damage to any target.")
        .id();
    scenario.with_mana_pool(P0, pool_units(&[ManaType::Red]));
    let mut runner = scenario.build();
    // Make it the opponent's turn — the "During your turn" gate must block P0.
    runner.state_mut().active_player = PlayerId(1);

    assert!(
        !can_cast_object_now(runner.state(), P0, sorcery_id),
        "Festival's during-your-turn gate must block the cast on the opponent's turn"
    );
}
