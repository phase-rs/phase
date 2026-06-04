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
use engine::game::triggers::process_triggers;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

/// Full Oracle text of Unstoppable Slasher (Duskmourn). The dies trigger is the
/// load-bearing piece for the regression below; Deathtouch and the
/// combat-damage trigger are included so the parser sees the canonical card
/// shape (keyword line + two trigger lines).
const SLASHER_FULL_ORACLE: &str = "Deathtouch\n\
Whenever Unstoppable Slasher deals combat damage to a player, that player loses \
half their life, rounded up.\n\
When Unstoppable Slasher dies, if it had no counters on it, return it to the \
battlefield tapped and with two stun counters under its owner's control.";

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

/// CR 603.4 + CR 614.1c + issue #1498: Unstoppable Slasher's dies trigger
/// ("When ~ dies, if it had no counters on it, return it to the battlefield
/// tapped and with two stun counters under its owner's control") must (1) fire
/// when the creature dies with no counters and (2) return it tapped and bearing
/// two stun counters.
///
/// Before the fix, the "if it had no counters on it" intervening-if was dropped
/// (so the gate was absent) and the "with two stun counters" clause parsed as
/// an `Unimplemented` follow-up, so the creature returned untapped with zero
/// counters — the user-visible "it's not triggering its ability" symptom.
#[test]
fn unstoppable_slasher_dies_with_no_counters_returns_tapped_with_two_stun() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let slasher = scenario
        .add_creature_from_oracle(P0, "Unstoppable Slasher", 3, 3, SLASHER_FULL_ORACLE)
        .id();

    let mut runner = scenario.build();

    // Precondition: the dies trigger is installed and the creature has no counters.
    assert!(
        !runner.state().objects[&slasher]
            .trigger_definitions
            .is_empty(),
        "Unstoppable Slasher must have triggers installed by the Oracle parser"
    );
    assert!(
        runner.state().objects[&slasher].counters.is_empty(),
        "precondition: the Slasher has no counters before dying"
    );

    // Kill it via the real zone-move pipeline (CR 603.6c emits the dies event).
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), slasher, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().objects[&slasher].zone,
        Zone::Graveyard,
        "precondition: the Slasher moved to the graveyard"
    );

    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the dies trigger should be on the stack (its intervening-if 'if it had \
         no counters on it' is satisfied — the Slasher had none)"
    );

    runner.advance_until_stack_empty();

    let returned = &runner.state().objects[&slasher];
    assert_eq!(
        returned.zone,
        Zone::Battlefield,
        "CR 603.4: the dies trigger returns the Slasher to the battlefield"
    );
    assert!(returned.tapped, "CR 614.1c: the Slasher returns tapped");
    assert_eq!(
        returned.counters.get(&CounterType::Stun).copied(),
        Some(2),
        "CR 122.1d + CR 614.1c: the Slasher returns with two stun counters"
    );
}

/// CR 603.4 + issue #1498 (negative path): a Slasher that dies WHILE it has a
/// counter on it must NOT return — the "if it had no counters on it"
/// intervening-if (`Not(HadCounters)`) gates the trigger out. This is what
/// stops the recursion: after the first return it bears two stun counters, so a
/// second death leaves it in the graveyard.
#[test]
fn unstoppable_slasher_dies_with_counter_does_not_return() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let slasher = scenario
        .add_creature_from_oracle(P0, "Unstoppable Slasher", 3, 3, SLASHER_FULL_ORACLE)
        .id();
    // Give it a counter (a stun counter, as it would carry after a prior return).
    scenario.with_counter(slasher, CounterType::Stun, 2);

    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), slasher, Zone::Graveyard, &mut events);

    process_triggers(runner.state_mut(), &events);
    assert_eq!(
        runner.state().stack.len(),
        0,
        "CR 603.4: the dies trigger's 'if it had no counters on it' intervening-if \
         fails (the Slasher had stun counters), so nothing is put on the stack"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&slasher].zone,
        Zone::Graveyard,
        "the Slasher stays in the graveyard — it had counters, so it does not return"
    );
}
