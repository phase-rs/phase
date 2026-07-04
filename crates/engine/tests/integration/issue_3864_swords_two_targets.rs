//! Regression for GitHub issue #3864 — Swords to Plowshares asks for two targets.
//!
//! Oracle: "Exile target creature. Its controller gains life equal to its power."

use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, TargetRef};

const SWORDS_ORACLE: &str = "Exile target creature. Its controller gains life equal to its power.";

#[test]
fn swords_spell_ability_is_single_target_chain() {
    let mut scenario = GameScenario::new();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_ORACLE)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&spell].abilities[0];
    assert!(
        matches!(*ability.effect, Effect::ChangeZone { .. }),
        "primary effect should exile a creature"
    );
    assert!(
        ability.sub_ability.is_some(),
        "life gain should chain as sub_ability"
    );
    assert!(
        ability.multi_target.is_none(),
        "Swords must not declare multi-target, got {:?}",
        ability.multi_target
    );
}

#[test]
fn swords_builds_one_target_slot() {
    let mut scenario = GameScenario::new();
    let creature = scenario.add_creature(P1, "Victim", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_ORACLE)
        .id();

    let runner = scenario.build();
    let ability = runner.state().objects[&spell].abilities[0].clone();
    let resolved = build_resolved_from_def(&ability, spell, P0);
    let slots = build_target_slots(runner.state(), &resolved).expect("target slots");
    assert_eq!(
        slots.len(),
        1,
        "Swords must build exactly one target slot (issue #3864), got {}",
        slots.len()
    );
    assert!(
        slots[0]
            .legal_targets
            .contains(&TargetRef::Object(creature)),
        "the opposing creature must be legal"
    );
}

#[test]
fn swords_cast_requires_only_one_target() {
    let mut scenario = GameScenario::new();
    let creature = scenario.add_creature(P1, "Victim", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_ORACLE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(spell).target_object(creature).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.battlefield_count(P1), 0, "creature should be exiled");
}
