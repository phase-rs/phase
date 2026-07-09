//! Regression coverage for the "cards named X in your graveyard" count misparse.
//!
//! Frantic Inventory reads "Draw a card, then draw cards equal to the number of
//! cards named Frantic Inventory in your graveyard." The locative zone phrase
//! ("in your graveyard") was previously SWALLOWED into the card-name filter,
//! producing `Named { name: "Frantic Inventory in your graveyard" }` — a name no
//! object ever has — with no `InZone` constraint, so the `ObjectCount` fell back
//! to the battlefield and always resolved to 0. The second draw therefore never
//! fired, regardless of how many copies sat in the graveyard.
//!
//! The parser fix terminates the card name at the "in <zone>" locative (CR 201.2)
//! and re-attaches it as `InZone { Graveyard }` + controller You (CR 400.1), so
//! the count is over graveyard copies named "Frantic Inventory".
//!
//! This is a whole-class fix (Accumulated Knowledge "in all graveyards", Plague
//! Rats "on the battlefield", Undead Servant, Goblin Gathering, Galvanic
//! Bombardment, Ancestral Anger, Compound Fracture, Growth Cycle, Take
//! Inventory); Frantic Inventory is the runtime exemplar because the misparse
//! directly changes how many cards it draws.

use engine::game::scenario::GameScenario;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Frantic Inventory — verbatim Oracle text.
const ORACLE: &str =
    "Draw a card, then draw cards equal to the number of cards named Frantic Inventory \
in your graveyard.";

/// Two copies of Frantic Inventory sit in the graveyard alongside an unrelated
/// card. CR 121.1: the first draw always happens; the second draw is
/// `number of cards named Frantic Inventory in your graveyard` = 2 (the decoy
/// "Scrap Note" is excluded by the name filter). Total = 1 + 2 = 3.
///
/// REVERT-FAILING: before the fix the count resolved to 0 (name never matched,
/// no zone constraint), so only the first card was drawn — this assertion flips
/// to `assert_hand_drawn(P0, 1)`-vs-3 failure when the parser fix is reverted.
#[test]
fn frantic_inventory_draws_one_plus_named_graveyard_copies() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 121.1: seed the library so all three draws have something to pull.
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C", "Filler D"]);

    // Two graveyard copies named "Frantic Inventory" (counted) + one decoy with a
    // different name (excluded by the Named filter — proves the count is by name,
    // not "every card in the graveyard").
    scenario.add_spell_to_graveyard(P0, "Frantic Inventory", false);
    scenario.add_spell_to_graveyard(P0, "Frantic Inventory", false);
    scenario.add_spell_to_graveyard(P0, "Scrap Note", false);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Frantic Inventory", false, ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();

    // CR 121.1: 1 (first draw) + 2 (graveyard copies named "Frantic Inventory").
    outcome.assert_hand_drawn(P0, 3);
}
