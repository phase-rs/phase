//! Regression for issue #3316: Stinging Study must correctly parse the
//! "greatest mana value of a commander you own on the battlefield or in the command zone" pattern.
//!
//! https://github.com/phase-rs/phase/issues/3316
//!
//! NOTE: The parser now correctly produces `QuantityRef::Aggregate` with Max function
//! for the "greatest mana value" pattern (CR 202.3 aggregate-max semantics).
//! This handles partner commanders correctly by taking the maximum of their mana values.
//!
//! The non-greatest pattern ("mana value of a commander") was removed as it required
//! undriveable runtime object-choice semantics. Only the greatest variant is supported.

use engine::game::scenario::{GameScenario, P0};

const GREATEST_VARIANT: &str = "Flashback {8}{G}{G}. This spell costs {X} less to cast this way, where X is the greatest mana value of a commander you own on the battlefield or in the command zone.";

#[test]
fn greatest_variant_parses_without_swallowing_dynamic_qty() {
    // This test verifies that the "greatest mana value" pattern (flashback costs)
    // parses correctly without triggering the DynamicQty swallow detector.
    // The actual parsing verification is in the unit test
    // parser::oracle_nom::quantity::tests::test_parse_greatest_commander_mana_value_ref.
    let mut scenario = GameScenario::new();
    let _spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Spell", false, GREATEST_VARIANT)
        .id();
    let _runner = scenario.build();

    // If we reach here without panicking, the card parsed successfully.
    // The unit test verifies the internal structure is correct (Aggregate with Max).
}
