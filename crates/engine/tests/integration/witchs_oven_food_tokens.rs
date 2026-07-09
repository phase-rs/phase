//! Issue #5340 — Witch's Oven must create 1 Food token by default, or 2 (not 3)
//! when the sacrificed creature's toughness was 4 or greater.
//!
//! CR 608.2c: "Create a Food token. If … create two Food tokens instead."
//! CR 400.7j: the sacrificed creature's LKI toughness is checked at resolution.
//! CR 608.2k: the sacrifice cost object is the referent for "the sacrificed creature".

use engine::game::scenario::{GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const WITCHS_OVEN_ORACLE: &str = "{T}, Sacrifice a creature: Create a Food token. If the sacrificed creature's toughness was 4 or greater, create two Food tokens instead.";

fn food_tokens(state: &engine::types::game_state::GameState) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Food"))
        })
        .count()
}

fn make_artifact(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![CoreType::Artifact];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
}

fn oven_ability_index(state: &engine::types::game_state::GameState, oven: ObjectId) -> usize {
    state
        .objects
        .get(&oven)
        .expect("oven on battlefield")
        .abilities
        .iter()
        .position(|a| a.cost.is_some())
        .expect("Witch's Oven has a costed activated ability")
}

#[test]
fn witchs_oven_creates_one_food_for_small_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oven = scenario
        .add_creature_from_oracle(P0, "Witch's Oven", 0, 0, WITCHS_OVEN_ORACLE)
        .id();
    let fodder = scenario.add_creature(P0, "Small Goat", 2, 2).id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, oven);
    let idx = oven_ability_index(runner.state(), oven);

    runner.activate(oven, idx).pay_with(&[fodder]).resolve();

    assert_eq!(
        food_tokens(runner.state()),
        1,
        "toughness 2 sacrifice must yield exactly one Food token (issue #5340)"
    );
}

#[test]
fn witchs_oven_creates_two_food_for_tough_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let oven = scenario
        .add_creature_from_oracle(P0, "Witch's Oven", 0, 0, WITCHS_OVEN_ORACLE)
        .id();
    let fodder = scenario.add_creature(P0, "Large Ox", 4, 4).id();

    let mut runner = scenario.build();
    make_artifact(&mut runner, oven);
    let idx = oven_ability_index(runner.state(), oven);

    runner.activate(oven, idx).pay_with(&[fodder]).resolve();

    assert_eq!(
        food_tokens(runner.state()),
        2,
        "toughness 4+ sacrifice must yield exactly two Food tokens, not three (issue #5340)"
    );
}
