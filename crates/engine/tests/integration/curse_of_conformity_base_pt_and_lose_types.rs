//! Curse of Conformity — "Nonlegendary creatures enchanted player controls have
//! base power and toughness 3/3 and lose all creature types."
//!
//! Runtime regression coverage for the continuous static that combines:
//!   - **base P/T set** (layer 7b, CR 613.4b) — overrides printed P/T to 3/3,
//!   - **remove all creature types** (layer 4, CR 205.1a + CR 613.1d),
//!   - **nonlegendary filter** — legendary creatures are excluded,
//!   - **enchanted-player scope** — only the enchanted player's creatures are
//!     affected (CR 303.4b).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` state — a runtime test, not an AST-shape
//! test.

use engine::game::effects::attach::attach_to_player;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

/// Oracle text for Curse of Conformity (Innistrad: Crimson Vow).
const CURSE_OF_CONFORMITY: &str =
    "Nonlegendary creatures enchanted player controls have base power and toughness 3/3 \
     and lose all creature types.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

fn has_subtype(runner: &GameRunner, id: ObjectId, subtype: &str) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object present")
        .card_types
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(subtype))
}

/// CR 613.4b + CR 205.1a: Nonlegendary creature under the enchanted player
/// gets base 3/3 and loses all creature types.
#[test]
fn curse_of_conformity_sets_base_pt_and_removes_creature_types() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Create the curse as an enchantment on the battlefield under P0's control.
    let curse = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Curse of Conformity", 0, 0, CURSE_OF_CONFORMITY);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    // Enchanted player's nonlegendary creature — a 1/1 Elf.
    let elf = {
        let mut builder = scenario.add_creature(P1, "Llanowar Elves", 1, 1);
        builder.with_subtypes(vec!["Elf", "Druid"]);
        builder.id()
    };

    // Enchanted player's legendary creature — should NOT be affected.
    let legend = {
        let mut builder = scenario.add_creature(P1, "Thalia, Guardian of Thraben", 2, 1);
        builder.as_legendary();
        builder.with_subtypes(vec!["Human", "Soldier"]);
        builder.id()
    };

    // Controller's own nonlegendary creature — outside the enchanted player filter.
    let ally = {
        let mut builder = scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        builder.with_subtypes(vec!["Bear"]);
        builder.id()
    };

    let mut runner = scenario.build();

    // Seed all_creature_types so RemoveAllSubtypes has a set to check against.
    runner.state_mut().all_creature_types = vec![
        "Bear".to_string(),
        "Druid".to_string(),
        "Elf".to_string(),
        "Human".to_string(),
        "Soldier".to_string(),
    ];

    // Attach the curse to P1 (the enchanted player).
    attach_to_player(runner.state_mut(), curse, P1);

    // CR 613.4b: nonlegendary creature under enchanted player → base 3/3.
    assert_eq!(
        effective_pt(&mut runner, elf),
        (3, 3),
        "nonlegendary creature under enchanted player must become 3/3"
    );

    // CR 205.1a: creature types removed.
    assert!(
        !has_subtype(&runner, elf, "Elf"),
        "Elf subtype must be removed"
    );
    assert!(
        !has_subtype(&runner, elf, "Druid"),
        "Druid subtype must be removed"
    );

    // Legendary creature is excluded from the filter.
    assert_eq!(
        effective_pt(&mut runner, legend),
        (2, 1),
        "legendary creature must NOT be affected by Curse of Conformity"
    );
    assert!(
        has_subtype(&runner, legend, "Human"),
        "legendary creature must retain its creature types"
    );

    // Controller's own creature is excluded (wrong controller).
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "curse controller's own creature must NOT be affected"
    );
    assert!(
        has_subtype(&runner, ally, "Bear"),
        "curse controller's creature must retain its creature types"
    );
}

/// CR 611.3: The continuous effect ends when its source leaves the battlefield.
#[test]
fn curse_of_conformity_effect_ends_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let curse = {
        let mut builder =
            scenario.add_creature_from_oracle(P0, "Curse of Conformity", 0, 0, CURSE_OF_CONFORMITY);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura", "Curse"]);
        builder.id()
    };

    let elf = {
        let mut builder = scenario.add_creature(P1, "Llanowar Elves", 1, 1);
        builder.with_subtypes(vec!["Elf", "Druid"]);
        builder.id()
    };

    let mut runner = scenario.build();

    runner.state_mut().all_creature_types = vec!["Druid".to_string(), "Elf".to_string()];

    attach_to_player(runner.state_mut(), curse, P1);

    // Baseline: affected.
    assert_eq!(
        effective_pt(&mut runner, elf),
        (3, 3),
        "baseline: nonlegendary creature is 3/3 while curse is present"
    );
    assert!(
        !has_subtype(&runner, elf, "Elf"),
        "baseline: Elf subtype removed while curse is present"
    );

    // Remove the curse from the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != curse);
        state.objects.remove(&curse);
    }

    // CR 611.3: effect ends.
    assert_eq!(
        effective_pt(&mut runner, elf),
        (1, 1),
        "creature reverts to base 1/1 once the curse is gone"
    );
    assert!(
        has_subtype(&runner, elf, "Elf"),
        "creature regains Elf subtype once the curse is gone"
    );
    assert!(
        has_subtype(&runner, elf, "Druid"),
        "creature regains Druid subtype once the curse is gone"
    );
}
