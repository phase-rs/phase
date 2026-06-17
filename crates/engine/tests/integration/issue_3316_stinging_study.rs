//! Regression for issue #3316: Stinging Study must correctly parse the
//! "mana value of a commander you own on the battlefield or in the command zone" pattern.
//!
//! https://github.com/phase-rs/phase/issues/3316
//!
//! This test verifies that the non-greatest "mana value of a commander" pattern
//! (Stinging Study's actual text) parses correctly to `QuantityRef::CommanderManaValue`.
//! The actual parsing verification is in the unit test
//! parser::oracle_nom::quantity::tests::test_parse_commander_mana_value_ref.

use engine::game::scenario::{GameScenario, P0};

const STINGING_STUDY: &str = "Flashback {8}{G}{G}. This spell costs {X} less to cast this way, where X is the mana value of a commander you own on the battlefield or in the command zone.";

#[test]
fn stinging_study_non_greatest_variant_parses_without_swallowing_dynamic_qty() {
    // This test verifies that the non-greatest "mana value" pattern (flashback costs)
    // parses correctly without triggering the DynamicQty swallow detector.
    let mut scenario = GameScenario::new();
    let _spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Stinging Study", false, STINGING_STUDY)
        .id();
    let _runner = scenario.build();

    // If we reach here without panicking, the card parsed successfully.
    // The unit test verifies the internal structure is correct (CommanderManaValue).
}
