//! Integration test: Lightning Dart — disjunctive color "instead" condition.
//!
//! CR 105.2 + CR 608.2c: "Lightning Dart deals 1 damage to target creature. If
//! that creature is white or blue, Lightning Dart deals 4 damage to it instead."
//!
//! The parser must recognise the disjunctive "white or blue" condition and emit
//! `TargetFilter::Or { [HasColor(White), HasColor(Blue)] }` wrapped in
//! `ConditionInstead`. At runtime, the override fires when the target is either
//! color, and the base damage (1) applies otherwise.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const LIGHTNING_DART_ORACLE: &str =
    "Lightning Dart deals 1 damage to target creature. If that creature is white or blue, \
     Lightning Dart deals 4 damage to it instead.";

/// A white creature targeted by Lightning Dart takes 4 damage (override fires).
#[test]
fn lightning_dart_deals_4_to_white_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario
        .add_creature(P1, "White Bear", 2, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 1,
        })
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Dart", true, LIGHTNING_DART_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.damage_marked(target),
        4,
        "white creature should take 4 damage (instead override)"
    );
}

/// A blue creature targeted by Lightning Dart takes 4 damage (override fires).
#[test]
fn lightning_dart_deals_4_to_blue_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario
        .add_creature(P1, "Blue Bird", 1, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Dart", true, LIGHTNING_DART_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.damage_marked(target),
        4,
        "blue creature should take 4 damage (instead override)"
    );
}

/// A red creature targeted by Lightning Dart takes only 1 damage (base effect).
#[test]
fn lightning_dart_deals_1_to_non_matching_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario
        .add_creature(P1, "Red Goblin", 2, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        })
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lightning Dart", true, LIGHTNING_DART_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    assert_eq!(
        outcome.damage_marked(target),
        1,
        "red creature should take only 1 damage (base effect, no override)"
    );
}
