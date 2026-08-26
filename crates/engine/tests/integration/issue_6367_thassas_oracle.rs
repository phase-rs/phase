//! Thassa's Oracle regression (#6367): its ETB Dig keeps at most one looked-at
//! card on top and randomizes every unchosen looked-at card at the bottom.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::seq::SliceRandom;

const THASSAS_ORACLE: &str = "When Thassa's Oracle enters, look at the top X cards of your library, where X is your devotion to blue. Put up to one of them on top of your library and the rest on the bottom of your library in a random order. If X is greater than or equal to the number of cards in your library, you win the game.";

fn thassa_runner() -> (GameRunner, ObjectId, [ObjectId; 3], ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 0x6367);
    scenario.at_phase(Phase::PreCombatMain);

    // Three blue pips make X=3 independently of the Oracle's own printed
    // characteristics, so the test observes one chosen and two unchosen cards.
    for name in [
        "Blue Devotion One",
        "Blue Devotion Two",
        "Blue Devotion Three",
    ] {
        scenario
            .add_creature(P0, name, 1, 1)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 0,
            });
    }

    // `add_card_to_library_top` inserts at index zero. Build bottom-first so
    // the three-card look window is `[look_one, look_two, look_three]`.
    let below = scenario.add_card_to_library_top(P0, "Below Look Window");
    let look_three = scenario.add_card_to_library_top(P0, "Look Three");
    let look_two = scenario.add_card_to_library_top(P0, "Look Two");
    let look_one = scenario.add_card_to_library_top(P0, "Look One");
    let thassa = scenario
        .add_creature_to_hand_from_oracle(P0, "Thassa's Oracle", 1, 3, THASSAS_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    (
        scenario.build(),
        thassa,
        [look_one, look_two, look_three],
        below,
    )
}

#[test]
fn thassas_oracle_keeps_selected_card_on_top_and_randomizes_rest() {
    let (mut runner, thassa, looked_at, below) = thassa_runner();
    let outcome = runner.cast(thassa).resolve();
    let WaitingFor::DigChoice { cards, .. } = outcome.final_waiting_for() else {
        panic!(
            "Thassa's Oracle ETB must reach a DigChoice, got {:?}",
            outcome.final_waiting_for()
        );
    };
    assert_eq!(
        cards, &looked_at,
        "X=3 must look at exactly the top three cards"
    );

    let chosen = looked_at[1];
    let mut expected_rest = vec![looked_at[0], looked_at[2]];
    let mut expected_rng = runner.state().rng.clone();
    expected_rest.shuffle(&mut expected_rng);
    runner
        .act(GameAction::SelectCards {
            cards: vec![chosen],
        })
        .expect("Thassa's Oracle selection resolves");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&thassa].zone, Zone::Battlefield);
    assert_eq!(
        state.players[0].library.iter().copied().collect::<Vec<_>>(),
        [vec![chosen, below], expected_rest].concat(),
        "selected card goes to the top and the unchosen cards occupy the bottom in seeded random order"
    );
    assert_eq!(
        state.rng.get_word_pos(),
        expected_rng.get_word_pos(),
        "the random bottom rider consumes the seeded RNG exactly for its rest pile"
    );
}

#[test]
fn thassas_oracle_allows_no_top_card_and_randomizes_every_looked_at_card() {
    let (mut runner, thassa, looked_at, below) = thassa_runner();
    let outcome = runner.cast(thassa).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::DigChoice { .. }),
        "reach guard: the real ETB must expose its optional up-to-one choice"
    );

    let mut expected_rest = looked_at.to_vec();
    let mut expected_rng = runner.state().rng.clone();
    expected_rest.shuffle(&mut expected_rng);
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("declining Thassa's optional top card resolves");
    runner.advance_until_stack_empty();

    let state = runner.state();
    assert_eq!(state.objects[&thassa].zone, Zone::Battlefield);
    assert_eq!(
        state.players[0].library.iter().copied().collect::<Vec<_>>(),
        [vec![below], expected_rest].concat(),
        "declining the optional choice sends every looked-at card to the random bottom pile"
    );
    assert_eq!(state.rng.get_word_pos(), expected_rng.get_word_pos());
}
