//! Regression for GitHub issue #3288 — Electrolyze second target selection.
//!
//! Oracle: "Electrolyze deals 2 damage divided as you choose among one or two
//! target creatures and/or players. Draw a card."

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::ability_utils::build_target_slots;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AbilityKind, MultiTargetSpec};
use engine::types::phase::Phase;

const ELECTROLYZE_ORACLE: &str = "Electrolyze deals 2 damage divided as you choose among one or two target creatures and/or players.\nDraw a card.";

#[test]
fn electrolyze_parses_one_or_two_target_quantifier() {
    let mut scenario = GameScenario::new();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrolyze", true, ELECTROLYZE_ORACLE)
        .id();
    let runner = scenario.build();
    let ability = &runner.state().objects[&spell].abilities[0];
    assert_eq!(ability.kind, AbilityKind::Spell);
    assert_eq!(
        ability.multi_target,
        Some(MultiTargetSpec::fixed(1, 2)),
        "Electrolyze must offer one required and one optional target slot"
    );
}

#[test]
fn electrolyze_builds_two_target_slots_at_cast_time() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let _elf = scenario.add_creature(P1, "Llanowar Elves", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Electrolyze", true, ELECTROLYZE_ORACLE)
        .id();

    let runner = scenario.build();
    let ability = runner.state().objects[&spell].abilities[0].clone();
    let resolved = build_resolved_from_def(&ability, spell, P0);
    let slots = build_target_slots(runner.state(), &resolved).expect("target slots");
    assert_eq!(
        slots.len(),
        2,
        "Electrolyze must build two target slots so a second target can be chosen (issue #3288)"
    );
    assert!(!slots[0].optional, "first target is required");
    assert!(slots[1].optional, "second target is optional");
}
