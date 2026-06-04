//! Regression for #692 (Double strike and triggers).
//!
//! Game state: the defending player controls [[No Mercy]] — "Whenever a creature
//! deals damage to you, destroy it." A double-strike attacker connects with that
//! player in the FIRST-STRIKE combat-damage sub-step. No Mercy's trigger goes on
//! the stack and, once it resolves, destroys the attacker.
//!
//! Pre-fix bug: `resolve_combat_damage` ran the first-strike sub-step and then
//! fell straight through into the regular (second) sub-step WITHOUT granting the
//! priority window CR 510.3 requires between the two combat-damage steps. The
//! No Mercy trigger was sitting on the stack unresolved, so the still-alive
//! double striker dealt its regular-sub-step damage too — the player took TWO
//! hits from a creature that should have been destroyed after the first.
//!
//! Fix (`combat_damage.rs`): after the first-strike sub-step puts triggers on the
//! stack, `resolve_combat_damage` returns `WaitingFor::Priority` (CR 510.3) so the
//! stack resolves before the mandatory regular sub-step is re-entered via the
//! empty-stack completeness gate in `priority.rs`.
//!
//! CR references (verified against `docs/MagicCompRules.txt`):
//!   - CR 510.3 / CR 510.3a: after combat damage is dealt, triggered abilities are
//!     put on the stack and THEN the active player gets priority.
//!   - CR 510.4: with a first-strike/double-strike creature present, the phase has
//!     two combat-damage steps; the priority window of CR 510.3 happens after each.
//!   - CR 702.4b: a double striker deals combat damage in both the first-strike and
//!     the regular combat-damage step.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const NO_MERCY: &str = "Whenever a creature deals damage to you, destroy it.";

/// A 2/2 double striker attacks a player who controls No Mercy. The first-strike
/// sub-step deals 2; No Mercy's trigger destroys the attacker before the regular
/// sub-step — so the player takes exactly 2 (not 4), and the attacker is gone.
#[test]
fn double_striker_destroyed_by_first_strike_trigger_deals_no_regular_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1 (the defending player) controls No Mercy.
    scenario
        .add_creature_from_oracle(P1, "No Mercy", 0, 0, NO_MERCY)
        .as_enchantment();

    // P0's double-strike attacker.
    let attacker = {
        let mut b = scenario.add_creature(P0, "Double Striker", 2, 2);
        b.double_strike();
        b.id()
    };

    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    // Drive declare-attackers through the first combat-damage sub-step. This
    // pauses on the CR 510.3 priority window with No Mercy's trigger on the stack.
    run_combat(&mut runner, vec![attacker], vec![]);

    // CR 510.3a: No Mercy triggered on the first-strike damage and is on the stack;
    // the mandatory regular sub-step has NOT run yet.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "CR 510.3a: No Mercy's trigger is on the stack after the first-strike sub-step"
    );
    assert!(
        !runner
            .state()
            .combat
            .as_ref()
            .expect("combat is still in progress at the first-strike priority window")
            .regular_damage_done,
        "CR 510.4: the regular combat-damage sub-step must not have run yet"
    );
    assert_eq!(
        runner.life(P1),
        life_before - 2,
        "CR 510.4: only the first-strike sub-step has dealt damage so far (2)"
    );

    // Resolve No Mercy (destroys the attacker), then let the empty-stack gate
    // re-enter the regular sub-step.
    runner.advance_until_stack_empty();
    runner.pass_both_players();

    // CR 702.4b + CR 510.3: the attacker was destroyed by No Mercy before the
    // regular sub-step, so it deals NO regular-sub-step damage.
    assert_eq!(
        runner.life(P1),
        life_before - 2,
        "#692: a double striker destroyed in the first-strike sub-step deals no \
         regular-sub-step damage — the player takes 2, not 4"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "the attacker was destroyed by No Mercy after the first-strike sub-step"
    );
}

/// Control: with NO removal trigger, the same double striker DOES deal damage in
/// both sub-steps (2 + 2 = 4). Guards against a fix that over-suppresses the
/// regular sub-step.
#[test]
fn double_striker_without_removal_trigger_deals_damage_in_both_substeps() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = {
        let mut b = scenario.add_creature(P0, "Double Striker", 2, 2);
        b.double_strike();
        b.id()
    };

    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        life_before - 4,
        "CR 702.4b: an unobstructed double striker deals damage in both sub-steps (2 + 2)"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Battlefield,
        "the double striker survives when nothing removes it"
    );
}

/// First-strike (not double-strike) analog: a first-striker that connects with a
/// No Mercy player is destroyed by the trigger; since first strikers do not deal
/// damage in the regular sub-step anyway, this pins that the priority window does
/// not regress the single-hit first-strike path either.
#[test]
fn first_striker_destroyed_by_trigger_still_deals_its_single_hit() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P1, "No Mercy", 0, 0, NO_MERCY)
        .as_enchantment();

    let attacker = {
        let mut b = scenario.add_creature(P0, "First Striker", 3, 3);
        b.first_strike();
        b.id()
    };

    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();
    runner.pass_both_players();

    // CR 702.7b: a first striker deals its single hit in the first-strike sub-step.
    assert_eq!(
        runner.life(P1),
        life_before - 3,
        "CR 702.7b: the first striker dealt its single 3-damage hit"
    );
    assert_eq!(
        runner.state().objects[&attacker].zone,
        Zone::Graveyard,
        "the first striker was destroyed by No Mercy after dealing damage"
    );
}
