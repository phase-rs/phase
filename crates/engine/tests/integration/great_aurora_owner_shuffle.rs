//! Great Aurora's owner-routed shuffle and local draw sequence.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const GREAT_AURORA_ORACLE: &str = "Each player shuffles all cards from their hand and all permanents they own into their library, then draws that many cards.";

/// CR 608.2c + CR 701.24a + CR 701.24c/d: each player's move, terminal
/// shuffle, and EventContextAmount draw resolve together before the next
/// player's iteration. Empty and nonempty prospective populations use the
/// same path.
#[test]
fn three_players_draw_their_local_zero_two_four_moved_counts() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let great_aurora = scenario
        .add_spell_to_hand_from_oracle(P0, "The Great Aurora", false, GREAT_AURORA_ORACLE)
        .id();

    for (player, hand_count) in [(P0, 0), (P1, 2), (P2, 4)] {
        for card in 0..hand_count {
            scenario.add_card_to_hand(player, &format!("P{} hand {card}", player.0));
        }
        for card in 0..10 {
            scenario.add_card_to_library_top(player, &format!("P{} library {card}", player.0));
        }
    }

    let mut runner = scenario.build();
    let outcome = runner.cast(great_aurora).resolve();

    for (player, expected_draws) in [(P0, 0), (P1, 2), (P2, 4)] {
        assert_eq!(
            outcome.state().players[player.0 as usize].cards_drawn_this_turn,
            expected_draws,
            "P{} must draw exactly the number of cards they moved",
            player.0
        );
        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    player_id,
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                } if *player_id == player
            )),
            "P{} must shuffle their own library",
            player.0
        );
    }

    let last_shuffle = outcome
        .events()
        .iter()
        .rposition(|event| {
            matches!(
                event,
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::ShuffledLibrary,
                    ..
                }
            )
        })
        .expect("the local shuffle chain emitted shuffle actions");
    let first_draw = outcome
        .events()
        .iter()
        .position(|event| matches!(event, GameEvent::CardDrawn { .. }))
        .expect("the local draw emitted card-draw events");
    assert!(
        first_draw < last_shuffle,
        "the first player's local draw must precede the last player's shuffle"
    );
}
