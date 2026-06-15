//! Regression coverage for issue #3351: "its owner" on a life-change effect must
//! resolve to the destroyed object's OWNER, not the spell's controller.
//!
//! CARD TEXT: Misfortune's Gain and Path of Peace are both
//! "Destroy target creature. Its owner gains 4 life." (verified identical in the
//! engine's authoritative MTGJSON `AtomicCards.json`). The "Its owner gains 4
//! life" rider is the card's intentional drawback — when you destroy an
//! opponent's creature, the OPPONENT (the creature's owner) gains the 4 life.
//!
//! CR 108.3: an object's owner is the player who started the game with it in
//!   their deck — distinct from its controller (control can change).
//! CR 109.4: a reference to a player's possession resolves at the moment the
//!   effect applies (LKI for an object that has left the battlefield).
//! CR 608.2c: instructions resolve in written order — the destroy fires first,
//!   then "Its owner gains 4 life" reads the (now-destroyed) creature's owner.
//!
//! DISCRIMINATING: P0 casts the spell at a creature OWNED + CONTROLLED by P1.
//! After resolution P1 (the owner) must have gained 4 life and P0 must have
//! gained 0. On pre-fix code the bare "its owner" subject was unhandled, so the
//! GainLife clause was DROPPED and P1 gained 0 — this test fails there.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// "Destroy target creature. Its owner gains 4 life." — shared verbatim text of
/// Misfortune's Gain and Path of Peace.
const OWNER_LIFE_ORACLE: &str = "Destroy target creature. Its owner gains 4 life.";

#[test]
fn misfortunes_gain_destroyed_creatures_owner_gains_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The victim is OWNED and CONTROLLED by P1 (the opponent). P0 destroys it.
    let victim = scenario.add_creature(P1, "Owned Bear", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Misfortune's Gain", false, OWNER_LIFE_ORACLE)
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).target_object(victim).resolve();

    // CR 608.2c: the destroy resolves first.
    outcome.assert_zone(&[victim], Zone::Graveyard);

    // CR 108.3 + CR 109.4: the destroyed creature's OWNER (P1) gains 4 life;
    // the spell's controller (P0) does NOT. Pre-fix the GainLife clause was
    // dropped, so P1's delta was 0 — the make-or-break correctness gate.
    outcome.assert_life_delta(P1, 4);
    outcome.assert_life_delta(P0, 0);
}
