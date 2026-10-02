//! CR 706.2 — a results-table row is selected by the result AFTER modifiers, and
//! that result can legitimately be ZERO.
//!
//! `roll_die.rs`'s `apply_modifier` clamps the modified result to `0..=u8::MAX`,
//! so a downward modifier that exceeds the natural roll produces 0. A printed
//! `"N or less"` row plainly covers 0, so it must be parsed as `0..=N`.
//!
//! This is the runtime half of that fix. The parser half lives in
//! `try_parse_die_result_line`'s own tests; this file proves the zero-result
//! branch is actually reachable and actually selected through the real cast
//! pipeline, which a parser-level tuple assertion cannot show.
//!
//! REVERT-FAILING: with the row parsed as `1..=9` (the previous lowering) a
//! clamped result of 0 matches NO branch, nothing resolves, and the life delta
//! below is 0 instead of +3.

use engine::game::scenario::{GameScenario, P0};
use engine::types::events::GameEvent;
use engine::types::phase::Phase;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// The modifier wording is The Deck of Many Things'; the two rows are chosen so
/// the bands are distinguishable by life delta alone.
const ZERO_RESULT_TABLE: &str = "Roll a d20 and subtract the number of cards in your hand.\n9 or less | You gain 3 life.\n10\u{2014}20 | You lose 3 life.";

#[test]
fn a_modifier_clamped_zero_result_selects_the_or_less_row() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // One card in hand at resolution, so the subtraction is exactly -1.
    scenario.add_card_to_hand(P0, "Hand Filler");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Zero Result Probe", false, ZERO_RESULT_TABLE)
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    // Seed 6 pins the natural face to 1 (the same mapping
    // `name_sticker_goblin.rs` publishes). Reset AFTER commit and immediately
    // before resolution so the mapping is a property of the seed.
    let state = committed.state_mut();
    state.rng_seed = 6;
    state.rng_word_pos = 0;
    state.rng = ChaCha20Rng::seed_from_u64(6);
    let outcome = committed.resolve();

    // REACH-GUARD, and the direct proof of the premise: the emitted result is
    // the POST-modifier number (CR 706.2), and it is exactly ZERO here — natural
    // face 1 minus one card in hand, clamped by `apply_modifier`. This is the
    // value the row lookup consumes.
    let result = outcome.events().iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 20,
            result: Some(result),
            ..
        } => Some(*result),
        _ => None,
    });
    assert_eq!(
        result,
        Some(0),
        "the post-modifier result must be exactly 0, or this fixture is not \
         exercising the zero-result branch at all"
    );

    // THE DISCRIMINATOR. A clamped result of 0 is "9 or less", so the gain-3 row
    // fires. Under the old 1..=9 lowering no branch matches and this is 0.
    outcome.assert_life_delta(P0, 3);
}

/// PAIRED POSITIVE REACH-GUARD: with no modifier reduction reaching zero, the
/// OTHER row is still selectable — so the assertion above is discriminating
/// rather than passing because the table always picks its first row.
#[test]
fn an_unclamped_high_result_still_selects_the_other_row() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_hand(P0, "Hand Filler");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Zero Result Probe", false, ZERO_RESULT_TABLE)
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    // Seed 15 pins the natural face to 16; minus one card in hand leaves a
    // post-modifier result of 15, inside the printed 10-20 band.
    let state = committed.state_mut();
    state.rng_seed = 15;
    state.rng_word_pos = 0;
    state.rng = ChaCha20Rng::seed_from_u64(15);
    let outcome = committed.resolve();

    let result = outcome.events().iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 20,
            result: Some(result),
            ..
        } => Some(*result),
        _ => None,
    });
    assert_eq!(
        result,
        Some(15),
        "the post-modifier result must land in the printed 10-20 band"
    );

    outcome.assert_life_delta(P0, -3);
}
