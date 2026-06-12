//! Regression for GitHub issue #2918 — Future Sight's "play with the top card
//! of your library revealed" static must keep the library top in
//! `revealed_cards` for all players (CR 400.2).

use engine::game::derived::derive_display_state;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const FUTURE_SIGHT_ORACLE: &str = "Play with the top card of your library revealed.\n\
You may play lands and cast spells from the top of your library.";
const LANTERN_ORACLE: &str = "Players play with the top card of their libraries revealed.";

fn move_to_top_of_library(
    state: &mut engine::types::game_state::GameState,
    obj_id: ObjectId,
    owner: PlayerId,
) {
    let player = state.players.iter_mut().find(|p| p.id == owner).unwrap();
    player.library.retain(|id| *id != obj_id);
    player.library.push_front(obj_id);
    let obj = state.objects.get_mut(&obj_id).unwrap();
    obj.zone = Zone::Library;
}

#[test]
fn future_sight_keeps_library_top_revealed_to_all_players() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Future Sight", 0, 0, FUTURE_SIGHT_ORACLE);

    let top_card = scenario.add_creature_to_hand(P0, "Library Top", 2, 2).id();

    let mut runner = scenario.build();
    move_to_top_of_library(runner.state_mut(), top_card, P0);
    derive_display_state(runner.state_mut());

    assert!(
        runner.state().revealed_cards.contains(&top_card),
        "Future Sight must reveal the controller's library top via revealed_cards"
    );

    let opponent_view = filter_state_for_viewer(runner.state(), P1);
    let opponent_player = opponent_view.players.iter().find(|p| p.id == P0).unwrap();
    let revealed_top = opponent_player
        .library
        .front()
        .and_then(|id| opponent_view.objects.get(id));
    assert!(
        revealed_top.is_some(),
        "opponent-filtered state must expose the revealed library top"
    );
}

#[test]
fn lantern_effect_reveals_each_players_library_top() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Lantern", 0, 0, LANTERN_ORACLE);

    let p0_top = scenario
        .add_creature_to_hand(P0, "P0 Library Top", 2, 2)
        .id();
    let p1_top = scenario
        .add_creature_to_hand(P1, "P1 Library Top", 2, 2)
        .id();

    let mut runner = scenario.build();
    move_to_top_of_library(runner.state_mut(), p0_top, P0);
    move_to_top_of_library(runner.state_mut(), p1_top, P1);
    derive_display_state(runner.state_mut());

    assert!(runner.state().revealed_cards.contains(&p0_top));
    assert!(runner.state().revealed_cards.contains(&p1_top));

    let p0_view = filter_state_for_viewer(runner.state(), P0);
    let p1_player = p0_view.players.iter().find(|p| p.id == P1).unwrap();
    let revealed_top = p1_player
        .library
        .front()
        .and_then(|id| p0_view.objects.get(id));
    assert!(
        revealed_top.is_some(),
        "all-player reveal static must expose opponents' library tops"
    );
}
