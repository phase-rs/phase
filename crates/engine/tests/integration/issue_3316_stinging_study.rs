//! Regression for issue #3316: Stinging Study must correctly parse the
//! "mana value of a commander you own on the battlefield or in the command zone" pattern.
//!
//! https://github.com/phase-rs/phase/issues/3316

use engine::game::scenario::{GameScenario, P0};

const STINGING_STUDY: &str = "You draw X cards and you lose X life, where X is the mana value of a commander you own on the battlefield or in the command zone.";

#[test]
fn stinging_study_parses_without_swallowing_dynamic_qty() {
    // This test verifies that Stinging Study's commander mana value pattern
    // parses correctly without triggering the DynamicQty swallow detector.
    // The actual parsing verification is in the unit test
    // parser::oracle_nom::quantity::tests::test_parse_commander_mana_value_ref.
    let mut scenario = GameScenario::new();
    let _stinging_study = scenario
        .add_spell_to_hand_from_oracle(P0, "Stinging Study", false, STINGING_STUDY)
        .id();
    let _runner = scenario.build();

    // If we reach here without panicking, the card parsed successfully.
    // The unit test verifies the internal structure is correct.
}
