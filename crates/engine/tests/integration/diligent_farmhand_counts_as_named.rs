//! Diligent Farmhand — "counts as a card named Muscle Burst" graveyard static.
//!
//! Oracle text (second line):
//!   "If Diligent Farmhand is in a graveyard, effects from spells named Muscle
//!   Burst count it as a card named Muscle Burst."
//!
//! Muscle Burst reads:
//!   "Target creature gets +X/+X until end of turn, where X is 3 plus the
//!   number of cards named Muscle Burst in all graveyards."
//!
//! No general CR governs this Odyssey-specific templating; name-matching
//! semantics per CR 201.2a. While in a graveyard, Diligent Farmhand is counted
//! by any effect that counts "cards named Muscle Burst" in graveyards.
//!
//! This test verifies that the CountsAsNamed static (active only in graveyard)
//! causes the Muscle Burst ObjectCount to include Diligent Farmhand copies.

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

/// Diligent Farmhand — uses "this card" (the production pipeline form; `"this card"`
/// is in `SELF_REF_PARSE_ONLY_PHRASES` and is NOT normalized to `~`).
const FARMHAND_ORACLE: &str = "{1}{G}, Sacrifice ~: Search your library for a basic \
land card, put that card onto the battlefield tapped, then shuffle.\n\
If this card is in a graveyard, effects from spells named Muscle Burst count it as a card named Muscle Burst.";

/// Muscle Burst — verbatim Oracle text.
const MUSCLE_BURST_ORACLE: &str = "Target creature gets +X/+X until end of turn, \
where X is 3 plus the number of cards named Muscle Burst in all graveyards.";

/// Two Diligent Farmhands in the graveyard should each count as "Muscle Burst"
/// for the Muscle Burst spell's X calculation.
///
/// X = 3 (base) + 2 (Farmhands counting as Muscle Burst) = 5.
/// Target creature: 1/1 → 6/6.
#[test]
fn muscle_burst_counts_diligent_farmhands_in_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two Diligent Farmhands in the graveyard — their CountsAsNamed static is
    // active only in graveyard (CR 201.2a name-matching semantics).
    scenario
        .add_creature_to_graveyard(P0, "Diligent Farmhand", 1, 1)
        .from_oracle_text(FARMHAND_ORACLE);
    scenario
        .add_creature_to_graveyard(P0, "Diligent Farmhand", 1, 1)
        .from_oracle_text(FARMHAND_ORACLE);

    // Target creature on the battlefield.
    let target = scenario.add_creature(P0, "Bear Cub", 1, 1).id();

    // Muscle Burst in hand, ready to cast.
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Muscle Burst", true, MUSCLE_BURST_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_object(target).resolve();

    // X = 3 + 2 = 5; creature goes from 1/1 to 6/6.
    outcome.assert_power_toughness(target, 6, 6);
}
