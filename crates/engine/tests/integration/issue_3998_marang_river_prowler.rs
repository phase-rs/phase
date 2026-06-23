//! Regression for issue #3998: Marang River Prowler's graveyard-cast permission
//! must require the controller (not any player) to control a black or green
//! permanent.
//!
//! https://github.com/phase-rs/phase/issues/3998

use engine::game::casting::spell_objects_available_to_cast;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

const MARANG_ORACLE: &str = "This creature can't block and can't be blocked.\n\
You may cast this card from your graveyard as long as you control a black or green permanent.";

fn add_black_creature(
    scenario: &mut GameScenario,
    player: engine::types::player::PlayerId,
) -> engine::types::identifiers::ObjectId {
    let id = scenario.add_creature(player, "Black Permanent", 2, 2).id();
    id
}

fn paint_black(
    state: &mut engine::types::game_state::GameState,
    id: engine::types::identifiers::ObjectId,
) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.color = vec![ManaColor::Black];
}

#[test]
fn marang_river_prowler_not_castable_when_only_opponent_controls_black() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marang = scenario
        .add_creature_to_graveyard(P0, "Marang River Prowler", 2, 1)
        .from_oracle_text(MARANG_ORACLE)
        .id();
    let opponent_black = add_black_creature(&mut scenario, P1);

    let mut runner = scenario.build();
    paint_black(runner.state_mut(), opponent_black);

    assert!(
        !spell_objects_available_to_cast(runner.state(), P0).contains(&marang),
        "Marang must not be castable from the graveyard when only an opponent controls black"
    );
}

#[test]
fn marang_river_prowler_castable_when_you_control_black() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let marang = scenario
        .add_creature_to_graveyard(P0, "Marang River Prowler", 2, 1)
        .from_oracle_text(MARANG_ORACLE)
        .id();
    let own_black = add_black_creature(&mut scenario, P0);

    let mut runner = scenario.build();
    paint_black(runner.state_mut(), own_black);

    assert!(
        spell_objects_available_to_cast(runner.state(), P0).contains(&marang),
        "Marang must be castable from the graveyard when you control a black permanent"
    );
}
