//! A "target player's ..." zone count with a non-player primary must announce
//! any player — including the controller — as its count source (issue #6856
//! follow-up, PR #9280 review).
//!
//! NOTE ON /card-test's verbatim-Oracle-text rule: this test uses a SYNTHETIC
//! card. A Scryfall Oracle-text search finds exactly one printed card with a
//! "target player's <zone>" zone count — Tibalt, the Fiend-Blooded — and its
//! primary already declares the player (recipient==counted, no second slot),
//! so verbatim Oracle text cannot cover the divergent-recipient branch. The
//! sentence below is genuine Oracle grammar exercising the production
//! `parse_target_player_possessive_zone` path, the same way Recurring
//! Insight's sentence exercises the opponent branch.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

// Synthetic Oracle sentence: "target player's hand" with a controller-draw
// primary, so the count needs its own any-player slot.
const TARGET_PLAYERS_INSIGHT_ORACLE: &str =
    "Draw cards equal to the number of cards in target player's hand.";

/// CR 115.1 + CR 601.2c: the count-source slot for "target player's hand"
/// admits any player — the caster announces themselves and draws their own
/// hand size. Under the old opponent-only slot this announcement is illegal
/// and the cast fails, so the assertion flips when the scope mapping is
/// reverted. Three unequal hands prove the count follows the announced
/// player.
#[test]
fn target_players_hand_count_draws_announced_players_hand_size() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Target Player's Insight",
            false,
            TARGET_PLAYERS_INSIGHT_ORACLE,
        )
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_cards_in_hand(P0, &["Self One", "Self Two", "Self Three", "Self Four"]);
    scenario.with_cards_in_hand(P1, &["Lone"]);
    scenario.with_cards_in_hand(P2, &["Other One", "Other Two"]);
    scenario.with_library_top(P0, &["One", "Two", "Three", "Four", "Five"]);
    let mut runner = scenario.build();

    // Self-announcement: legal only for an any-player slot.
    let outcome = runner.cast(spell).target_player(P0).resolve();

    // CR 121.1: draws the announced (own) hand size — 4, not P1's 1 or P2's 2.
    outcome.assert_hand_drawn(P0, 4);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}
