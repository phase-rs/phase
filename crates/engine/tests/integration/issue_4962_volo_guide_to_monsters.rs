//! Issue #4962 — Volo, Guide to Monsters copies creature spells that don't
//! share a creature type with a creature you control or a creature card in
//! your graveyard.

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const VOLO: &str = "Whenever you cast a creature spell that doesn't share a creature type with a creature you control or a creature card in your graveyard, copy that spell. (A copy of a creature spell becomes a token.)";

#[test]
fn volo_copies_creature_spell_with_no_shared_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Volo, Guide to Monsters", 3, 3, VOLO);
    let goblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Goblin", 1, 1, "")
        .with_mana_cost(ManaCost::generic(0))
        .with_subtypes(vec!["Goblin"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(1),
            false,
            vec![],
        ));

    runner.cast(goblin).resolve();
    runner.advance_until_stack_empty();

    let creature_count = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &runner.state().objects[id];
            obj.zone == Zone::Battlefield && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();

    assert!(
        creature_count >= 3,
        "Volo + cast Goblin + token copy expected at least 3 creatures, got {creature_count}"
    );
}

#[test]
fn volo_does_not_copy_creature_spell_with_shared_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Volo, Guide to Monsters", 3, 3, VOLO);
    scenario
        .add_creature_from_oracle(P0, "Existing Goblin", 1, 1, "")
        .with_subtypes(vec!["Goblin"]);
    let goblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Goblin", 1, 1, "")
        .with_mana_cost(ManaCost::generic(0))
        .with_subtypes(vec!["Goblin"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(1),
            false,
            vec![],
        ));

    runner.cast(goblin).resolve();
    runner.advance_until_stack_empty();

    let creature_count = runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &runner.state().objects[id];
            obj.zone == Zone::Battlefield && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();

    assert_eq!(
        creature_count, 3,
        "Volo + existing Goblin + cast Goblin should not copy (shared type), got {creature_count}"
    );
}
