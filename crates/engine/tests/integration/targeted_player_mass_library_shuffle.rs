//! Runtime regression for targeted-player mass graveyard shuffles.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const REMINISCE_ORACLE: &str = "Target player shuffles their graveyard into their library.";

/// CR 400.3 + CR 608.2c + CR 701.24a: Reminisce moves every card from the
/// chosen player's graveyard into that player's library and shuffles that
/// library. The caster's zones and shuffle history are independent.
#[test]
fn reminisce_moves_and_shuffles_only_the_target_players_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let reminisce = scenario
        .add_spell_to_hand_from_oracle(P0, "Reminisce", false, REMINISCE_ORACLE)
        .id();

    let p0_library = scenario.add_card_to_library_top(P0, "Caster Library Card");
    let p1_library = scenario.add_card_to_library_top(P1, "Target Library Card");
    let p0_graveyard = scenario
        .add_creature_to_graveyard(P0, "Caster Graveyard Card", 1, 1)
        .id();
    let p1_graveyard_a = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card A", 1, 1)
        .id();
    let p1_graveyard_b = scenario
        .add_creature_to_graveyard(P1, "Target Graveyard Card B", 1, 1)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(reminisce).target_player(P1).resolve();

    outcome.assert_zone(&[p1_graveyard_a, p1_graveyard_b], Zone::Library);
    outcome.assert_zone(&[p0_graveyard], Zone::Graveyard);
    assert_eq!(outcome.zone_of(p0_library), Zone::Library);
    assert_eq!(outcome.zone_of(p1_library), Zone::Library);

    let p0_state = &outcome.state().players[P0.0 as usize];
    let p1_state = &outcome.state().players[P1.0 as usize];
    assert!(p0_state.graveyard.contains(&p0_graveyard));
    assert!(!p0_state.library.contains(&p0_graveyard));
    assert!(p0_state.library.contains(&p0_library));
    assert!(p1_state.graveyard.is_empty());
    assert!(p1_state.library.contains(&p1_library));
    assert!(p1_state.library.contains(&p1_graveyard_a));
    assert!(p1_state.library.contains(&p1_graveyard_b));

    let shuffled_players: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    assert_eq!(shuffled_players, vec![P1]);
}
