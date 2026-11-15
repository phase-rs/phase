//! Recurring Insight must prompt for a target opponent and draw cards equal
//! to that opponent's hand size (issue #6856).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

// Verbatim Oracle text (Scryfall, Recurring Insight).
const RECURRING_INSIGHT_ORACLE: &str = "Draw cards equal to the number of cards in target opponent's hand.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";

/// CR 115.1 + CR 601.2c + CR 121.1: the spell declares a target opponent at
/// announcement; on resolution the controller draws cards equal to THAT
/// opponent's hand size. Three players with unequal hands prove the count
/// follows the announced target instead of defaulting to zero or to the
/// first opponent.
#[test]
fn recurring_insight_draws_targeted_opponents_hand_size() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Recurring Insight", false, RECURRING_INSIGHT_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_cards_in_hand(P1, &["Alpha", "Beta", "Gamma"]);
    scenario.with_cards_in_hand(P2, &["Solo"]);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five"]);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_player(P1).resolve();

    // CR 121.1: draws exactly the targeted opponent's hand size (3, not P2's 1).
    outcome.assert_hand_drawn(P0, 3);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}
