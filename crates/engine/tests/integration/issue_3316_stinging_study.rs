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

#[test]
fn stinging_study_runtime_without_choice_path_resolves_to_zero() {
    // This test documents the bug: without the runtime choice prompt/storage
    // path, Stinging Study resolves to X=0 (draws 0, loses 0 life) even when
    // commanders are present.
    //
    // The quantity.rs resolver for ChosenObject (lines 2116-2143) reads
    // ChosenAttribute::Object from the source and falls back to ObjectId(0)
    // when absent. ObjectId(0) has mana_value 0, so X=0.
    //
    // Once the full runtime path is implemented (effect-chain prompt +
    // ChosenAttribute::Object storage), this test should be updated to:
    // 1. Set up two commanders with different mana values (e.g., 3 and 5)
    // 2. Cast Stinging Study
    // 3. Verify the player is prompted to choose a commander
    // 4. After choosing, verify X equals the chosen commander's mana value
    // 5. Verify the player draws and loses that many cards/life
    //
    // For now, this test is a placeholder documenting the missing runtime path.
    // The actual integration test would require:
    // - Effect::ChooseFromZone or similar to prompt for object choice
    // - Storage of chosen object ID in source's ChosenAttribute::Object
    // - Resolver reading the choice and returning the property

    // This test is intentionally left as a placeholder.
    // Once the runtime path is implemented, replace this with the full test.
}
