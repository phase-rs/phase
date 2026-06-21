//! Regression for issue #3873: Teferi's Ageless Insight must double multi-card draws.
//!
//! https://github.com/phase-rs/phase/issues/3873

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const TEFERI_ORACLE: &str =
    "If you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";

const DRAW_FOUR_ORACLE: &str = "Draw four cards.";

#[test]
fn teferi_ageless_insight_doubles_multi_card_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..12 {
        scenario.add_spell_to_library_top(P0, &format!("Filler {i}"), true);
    }
    scenario
        .add_creature_from_oracle(P0, "Teferi's Ageless Insight", 0, 0, TEFERI_ORACLE)
        .as_enchantment();
    let draw_four = scenario
        .add_spell_to_hand_from_oracle(P0, "Inspiration", false, DRAW_FOUR_ORACLE)
        .id();
    let mana: Vec<ManaUnit> = (0..8)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect();
    scenario.with_mana_pool(P0, mana);

    let mut runner = scenario.build();
    runner.cast(draw_four).resolve();

    assert_eq!(
        runner.state().players[0].hand.len(),
        8,
        "Teferi must double a draw-four spell into eight cards"
    );
}
