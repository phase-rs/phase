//! Runtime regression for issue #6208 — Punishing Punch.
//!
//! Oracle: "Target creature you control deals damage equal to twice its power to
//! target creature an opponent controls."
//!
//! "Its power" names the first target (the creature you control that deals the
//! damage). The multiplier form lowered it to `Multiply{2, Power{Source}}`, and
//! `Source` reads the SPELL (power 0), so the clause dealt 0 damage. The parser
//! now retargets the source-scoped power to the target subject, mirroring the
//! singular "its power" form. This drives the real cast pipeline (live-parsed,
//! no card-data dependency) and asserts the damage equals twice the SUBJECT's
//! power — not 0 (dropped referent) and not the recipient's own power.
//!
//! Distinct from Duggan (`duggan_private_detective_punch`): there the subject is
//! the source itself, so `Power{Source}` is correct and stays. Here the subject
//! is a separate target, so the source-scoped power must follow it.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const PUNISHING_PUNCH: &str = "Target creature you control deals damage equal to twice its power \
to target creature an opponent controls.";

#[test]
fn punishing_punch_deals_twice_the_subject_creatures_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Subject: P0's creature, power 3 → deals 2 x 3 = 6.
    let subject = scenario.add_vanilla(P0, 3, 3);
    // Recipient: P1's creature, toughness 10 (survives 6 so `damage_marked` stays
    // observable) and power 4 (differs from 6, so a wrong referent cannot pass).
    let recipient = scenario.add_vanilla(P1, 4, 10);
    let punch = scenario
        .add_spell_to_hand(P0, "Punishing Punch", true)
        .from_oracle_text(PUNISHING_PUNCH)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    let outcome = runner
        .cast(punch)
        .target_object(subject)
        .target_object(recipient)
        .resolve();

    assert_eq!(
        outcome.state().objects[&recipient].damage_marked,
        6,
        "Punishing Punch deals twice the SUBJECT's power (2 x 3 = 6), not 0 (the \
         dropped Source referent reads the spell) and not the recipient's own power (4)"
    );
}
