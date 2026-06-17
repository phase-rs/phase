//! Issue #541: Endurance-style "target player puts all the cards from their
//! graveyard on the bottom of their library" effects preserve library order.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn zone_names(
    runner: &engine::game::scenario::GameRunner,
    player: PlayerId,
    zone: Zone,
) -> Vec<String> {
    let ids = match zone {
        Zone::Library => &runner.state().players[player.0 as usize].library,
        Zone::Graveyard => &runner.state().players[player.0 as usize].graveyard,
        _ => panic!("zone_names only supports library/graveyard in this test"),
    };
    ids.iter()
        .map(|id| runner.state().objects[id].name.clone())
        .collect()
}

#[test]
fn target_player_bottoms_graveyard_without_shuffling_library() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Endurance Regression",
            false,
            "Target player puts all the cards from their graveyard on the bottom of their \
             library in a random order.",
        )
        .id();

    scenario.with_library_top(P1, &["Library Top", "Library Middle", "Library Bottom"]);
    scenario.add_creature_to_graveyard(P1, "Graveyard One", 1, 1);
    scenario.add_creature_to_graveyard(P1, "Graveyard Two", 1, 1);

    let mut runner = scenario.build();

    runner.cast(spell).target_player(P1).resolve();

    let library = zone_names(&runner, P1, Zone::Library);
    assert_eq!(
        &library[..3],
        ["Library Top", "Library Middle", "Library Bottom"],
        "Endurance must not shuffle the target player's existing library"
    );

    let mut bottom = library[3..].to_vec();
    bottom.sort();
    assert_eq!(
        bottom,
        ["Graveyard One", "Graveyard Two"],
        "Endurance must put every targeted graveyard card on the library bottom"
    );
    assert!(
        zone_names(&runner, P1, Zone::Graveyard).is_empty(),
        "targeted graveyard should be empty after Endurance resolves"
    );
}
