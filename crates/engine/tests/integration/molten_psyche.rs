//! Molten Psyche must bind "that player" to each live `DamageEachPlayer`
//! recipient after every player has shuffled and drawn their hand.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const MOLTEN_PSYCHE_ORACLE: &str = "Each player shuffles the cards from their hand into their library, then draws that many cards.\nMetalcraft — If you control three or more artifacts, Molten Psyche deals damage to each opponent equal to the number of cards that player has drawn this turn.";

fn molten_psyche_scenario(artifact_count: usize) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for index in 0..artifact_count {
        scenario
            .add_creature(P0, &format!("Artifact {index}"), 0, 1)
            .as_artifact();
    }

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Molten Psyche", false, MOLTEN_PSYCHE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    // The cast moves Molten Psyche off P0's hand before resolution, leaving
    // hands of 9, 3, and 7 cards to shuffle and redraw respectively.
    for index in 0..9 {
        scenario.add_card_to_hand(P0, &format!("P0 hand {index}"));
    }
    for index in 0..3 {
        scenario.add_card_to_hand(P1, &format!("P1 hand {index}"));
    }
    for index in 0..7 {
        scenario.add_card_to_hand(P2, &format!("P2 hand {index}"));
    }

    for player in [P0, P1, P2] {
        for index in 0..10 {
            scenario.add_card_to_library_top(player, &format!("P{} library {index}", player.0));
        }
    }

    (scenario.build(), spell)
}

/// CR 207.2c + CR 608.2c + CR 121.1 + CR 120.3: Metalcraft labels the
/// conditional second instruction, which runs after the written shuffle/draw
/// instruction and deals each opponent damage equal to that recipient's live
/// draw count.
#[test]
fn molten_psyche_metalcraft_damages_each_opponent_by_their_own_draw_count() {
    let (mut runner, spell) = molten_psyche_scenario(3);

    let outcome = runner.cast(spell).resolve();

    assert_eq!(
        outcome.state().players[P0.0 as usize].cards_drawn_this_turn,
        9,
        "P0's distinct post-resolution draw count is the controller-scope guard"
    );
    assert_eq!(
        outcome.state().players[P1.0 as usize].cards_drawn_this_turn,
        3,
        "P1 must draw its three-card hand before damage is calculated"
    );
    assert_eq!(
        outcome.state().players[P2.0 as usize].cards_drawn_this_turn,
        7,
        "P2 must draw its seven-card hand before damage is calculated"
    );
    outcome.assert_life_delta(P1, -3);
    outcome.assert_life_delta(P2, -7);
}

/// CR 207.2c + CR 121.1: Fewer than three artifacts skips Metalcraft damage,
/// while the preceding shuffle/draw instruction still executes for every player.
#[test]
fn molten_psyche_without_metalcraft_draws_everyone_but_deals_no_damage() {
    let (mut runner, spell) = molten_psyche_scenario(2);

    let outcome = runner.cast(spell).resolve();

    assert_eq!(
        outcome.state().players[P0.0 as usize].cards_drawn_this_turn,
        9,
        "the controller draw reach guard proves the spell resolved"
    );
    assert_eq!(
        outcome.state().players[P1.0 as usize].cards_drawn_this_turn,
        3,
        "P1's draw reach guard proves the first instruction resolved"
    );
    assert_eq!(
        outcome.state().players[P2.0 as usize].cards_drawn_this_turn,
        7,
        "P2's draw reach guard proves the first instruction resolved"
    );
    outcome.assert_life_delta(P1, 0);
    outcome.assert_life_delta(P2, 0);
}
