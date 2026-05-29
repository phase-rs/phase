//! Regression test for #1245 — Unstoppable Slasher's combat-damage trigger
//! ("Whenever this creature deals combat damage to a player, they lose half
//! their life, rounded up.") silently resolved as "lose 0 life".
//!
//! The trigger is event-bound (CR 603.6f): "they"/"their" refers to the
//! damaged player carried on the triggering event, not a chosen target.
//! Before the fix, "they" parsed to `ParentTarget` and the half-life amount
//! to `LifeTotal { Target }` — both reading an absent player target, so the
//! lose-life resolved to 0 (a visible trigger with no life change).
//!
//! The fix:
//!   * `resolve_they_pronoun` binds "they" to `TriggeringPlayer` for
//!     damage-/attack-to-player triggers.
//!   * `lower_trigger_ir` rebinds the body's `PlayerScope::Target` possessives
//!     to `PlayerScope::ScopedPlayer`.
//!   * stack resolution stamps `scoped_player` from the triggering event so
//!     the `ScopedPlayer` amount resolves to the damaged player.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

use super::rules::run_combat;

/// Verified Oracle clause from `client/public/card-data.json`
/// (`jq '.["unstoppable slasher"]'`).
const SLASHER_ORACLE: &str = "Whenever this creature deals combat damage to a \
    player, they lose half their life, rounded up.";

/// CR 603.7c + CR 119.3 + CR 107.1a: an unblocked Unstoppable Slasher deals its
/// combat damage and then the trigger makes the damaged player lose
/// `ceil(life_after_damage / 2)` more life.
#[test]
fn unstoppable_slasher_combat_damage_halves_damaged_player_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P1 at 20: a 3/3 Slasher deals 3 → 17, then the trigger removes
    // ceil(17/2) = 9 → 8. The odd intermediate total (17) pins "rounded up".
    scenario.with_life(P1, 20);

    let slasher = scenario
        .add_creature_from_oracle(P0, "Unstoppable Slasher", 3, 3, SLASHER_ORACLE)
        .id();

    let mut runner = scenario.build();

    let life_before = runner.life(P1);
    assert_eq!(life_before, 20, "precondition: P1 starts at 20 life");

    // 3/3 Slasher attacks P1 unblocked (CR 510.1b: 3 combat damage to P1).
    run_combat(&mut runner, vec![slasher], vec![]);
    // CR 510.3a: the combat-damage trigger goes on the stack — drain it so the
    // half-life loss resolves before asserting.
    runner.advance_until_stack_empty();

    // 20 − 3 (combat) − ceil(17 / 2) = 20 − 3 − 9 = 8.
    assert_eq!(
        runner.life(P1),
        8,
        "CR 603.7c + CR 119.3: P1 takes 3 combat damage (→17), then the \
         event-bound trigger removes ceil(17/2) = 9 (→8). A regression to the \
         pre-fix parse resolves the trigger as 'lose 0' and leaves P1 at 17."
    );
}
