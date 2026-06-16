//! Regression for issue #3316: Stinging Study must correctly parse the
//! "mana value of a commander you own on the battlefield or in the command zone" pattern.
//!
//! https://github.com/phase-rs/phase/issues/3316
//!
//! NOTE: The parser now correctly produces `QuantityRef::ChosenObject` for the
//! non-greatest pattern (CR 608.2d choice semantics). However, full runtime
//! choice semantics require additional effect-chain work:
//! - An `Effect::ChooseFromZone` or similar to prompt the player for object choice
//! - Storage of the chosen object ID in the source's `ChosenAttribute::Object`
//! - The resolver can then read this choice and return the property
//!
//! This integration test verifies parsing correctness. Full runtime choice
//! prompt implementation is tracked as a follow-up item.

use engine::game::scenario::{GameScenario, P0};

const STINGING_STUDY: &str = "You draw X cards and you lose X life, where X is the mana value of a commander you own on the battlefield or in the command zone.";

const GREATEST_VARIANT: &str = "Flashback {8}{G}{G}. This spell costs {X} less to cast this way, where X is the greatest mana value of a commander you own on the battlefield or in the command zone.";

#[test]
fn stinging_study_parses_without_swallowing_dynamic_qty() {
    // This test verifies that Stinging Study's commander mana value pattern
    // parses correctly without triggering the DynamicQty swallow detector.
    // The actual parsing verification is in the unit test
    // parser::oracle_nom::quantity::tests::test_parse_chosen_commander_mana_value_ref.
    let mut scenario = GameScenario::new();
    let _stinging_study = scenario
        .add_spell_to_hand_from_oracle(P0, "Stinging Study", false, STINGING_STUDY)
        .id();
    let _runner = scenario.build();

    // If we reach here without panicking, the card parsed successfully.
    // The unit test verifies the internal structure is correct (ChosenObject).
}

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
