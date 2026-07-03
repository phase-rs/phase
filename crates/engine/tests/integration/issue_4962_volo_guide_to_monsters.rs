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
    // A controlled creature of a DIFFERENT type must not block the copy — this
    // makes the positive case discriminating rather than trivially true.
    scenario
        .add_creature(P0, "Resident Beast", 2, 2)
        .with_subtypes(vec!["Beast"]);
    let goblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Goblin", 1, 1, "")
        .with_mana_cost(ManaCost::generic(0))
        .with_subtypes(vec!["Goblin"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Beast".to_string()];
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(1),
            false,
            vec![],
        ));

    let before = battlefield_creature_count(&runner);
    runner.cast(goblin).resolve();
    runner.advance_until_stack_empty();
    let after = battlefield_creature_count(&runner);

    // Volo(1) + Beast(1) = 2 before; casting the Goblin (no shared type with the
    // Beast) adds the spell (1) AND Volo's copy token (1) = before + 2.
    assert_eq!(
        after,
        before + 2,
        "Volo must copy a creature spell that shares no creature type with a creature you \
         control or in your graveyard (before={before}, after={after})"
    );
}

fn battlefield_creature_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter(|id| {
            let obj = &runner.state().objects[id];
            obj.zone == Zone::Battlefield && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count()
}

/// CR 205.3m (issue #4962 review — negative): Volo must NOT copy a cast
/// creature spell that shares a creature type with a creature you already
/// control. Here a Goblin is already on the battlefield, so casting another
/// Goblin fails the disjunctive "creature you control" blocker → no token.
#[test]
fn volo_does_not_copy_when_sharing_type_with_controlled_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Volo, Guide to Monsters", 3, 3, VOLO);
    scenario
        .add_creature(P0, "Resident Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"]);
    let goblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Goblin", 1, 1, "")
        .with_mana_cost(ManaCost::generic(0))
        .with_subtypes(vec!["Goblin"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Beast".to_string()];
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(1),
            false,
            vec![],
        ));

    let before = battlefield_creature_count(&runner);
    runner.cast(goblin).resolve();
    runner.advance_until_stack_empty();
    let after = battlefield_creature_count(&runner);

    // Volo(1) + Resident Goblin(1) already on battlefield = 2 before; casting the
    // Goblin adds exactly 1 (the spell itself), with NO Volo copy token.
    assert_eq!(
        after,
        before + 1,
        "Volo must not copy a creature spell sharing a type with a creature you control \
         (before={before}, after={after})"
    );
}

/// CR 205.3m (issue #4962 review — negative): the graveyard leg of the
/// disjunction. A Goblin card in your graveyard blocks the copy just as a
/// controlled Goblin does — proving BOTH `Or` legs are enforced at runtime.
#[test]
fn volo_does_not_copy_when_sharing_type_with_graveyard_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario.add_creature_from_oracle(P0, "Volo, Guide to Monsters", 3, 3, VOLO);
    scenario
        .add_creature_to_graveyard(P0, "Dead Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"]);
    let goblin = scenario
        .add_creature_to_hand_from_oracle(P0, "Test Goblin", 1, 1, "")
        .with_mana_cost(ManaCost::generic(0))
        .with_subtypes(vec!["Goblin"])
        .id();

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string()];
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(1),
            false,
            vec![],
        ));

    let before = battlefield_creature_count(&runner);
    runner.cast(goblin).resolve();
    runner.advance_until_stack_empty();
    let after = battlefield_creature_count(&runner);

    // Volo(1) on battlefield = 1 before; casting the Goblin adds exactly 1, with
    // NO Volo copy token (the graveyard Goblin shares the "Goblin" type).
    assert_eq!(
        after,
        before + 1,
        "Volo must not copy a creature spell sharing a type with a creature card in your \
         graveyard (before={before}, after={after})"
    );
}
