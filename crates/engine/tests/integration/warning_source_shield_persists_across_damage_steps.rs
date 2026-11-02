//! Warning — continuous source-scoped combat-damage prevention, driven through
//! the real cast + combat pipeline.
//!
//! Oracle text under test (Scryfall):
//!   "Prevent all combat damage that would be dealt by target attacking
//!    creature this turn."
//!
//! A qualified "dealt by target <creature>" source is a continuous shield, not
//! the one-shot "the next time target creature would deal damage" (Awe Strike).
//! A double-strike attacker deals combat damage in two steps (CR 702.4b), so the
//! shield must prevent both, and keep preventing for the rest of the turn.
//!
//! CR 615.1a + CR 609.7a + CR 702.4b + CR 510.4.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

const WARNING: &str =
    "Prevent all combat damage that would be dealt by target attacking creature this turn.";

/// CR 615.1a + CR 702.4b: a double-strike attacker chosen by Warning deals no
/// damage in the first-strike step or the regular step. A second, unshielded
/// attacker in the same combat still connects, so the life delta proves the
/// combat-damage steps ran and the shield is scoped to the chosen creature.
#[test]
fn warning_prevents_a_double_strikers_damage_in_both_steps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shielded = scenario
        .add_creature(P0, "Shielded Double Striker", 3, 3)
        .double_strike()
        .id();
    let unshielded = scenario.add_creature(P0, "Free Attacker", 2, 2).id();
    let warning = scenario
        .add_spell_to_hand_from_oracle(P0, "Warning", true, WARNING)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (shielded, AttackTarget::Player(P1)),
            (unshielded, AttackTarget::Player(P1)),
        ])
        .expect("declaring both attackers must be accepted");

    runner.cast(warning).target_objects(&[shielded]).resolve();

    let outcome = runner.combat_damage();

    // Unshielded 2 damage lands in the regular step. Any 3 from the shielded
    // double striker (first-strike step, or regular step once the shield had
    // lapsed) would show up here as -5 or -8.
    outcome.assert_life_delta(P1, -2);
}
