//! Issue #841 — Selvala, Explorer Returned parley must add {G} only for nonland reveals.
//!
//! https://github.com/phase-rs/phase/issues/841

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const SELVALA_ORACLE: &str = "Parley — {T}: Each player reveals the top card of their library. For each nonland card revealed this way, add {G} and you gain 1 life. Then each player draws a card.";

fn stamp_library_top(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
    core_type: CoreType,
) {
    let top = runner.state().players[player.0 as usize].library[0];
    let obj = runner
        .state_mut()
        .objects
        .get_mut(&top)
        .expect("library top");
    obj.card_types.core_types.push(core_type);
    obj.base_card_types = obj.card_types.clone();
}

#[test]
fn selvala_parley_adds_green_only_for_nonland_reveals() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Grizzly Bears", "Library Bottom"]);
    scenario.with_library_top(P1, &["Forest", "Library Bottom"]);
    scenario.add_creature_from_oracle(P0, "Selvala, Explorer Returned", 2, 3, SELVALA_ORACLE);

    let mut runner = scenario.build();
    stamp_library_top(&mut runner, P0, CoreType::Creature);
    stamp_library_top(&mut runner, P1, CoreType::Land);

    let selvala_id = runner
        .state()
        .battlefield
        .iter()
        .find(|id| runner.state().objects[id].name == "Selvala, Explorer Returned")
        .copied()
        .expect("Selvala on battlefield");
    let life_before = runner.state().players[P0.0 as usize].life;

    runner.activate(selvala_id, 0).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "one nonland reveal across both players must produce exactly one green mana"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before + 1,
        "Selvala must gain 1 life per nonland card revealed"
    );
}
