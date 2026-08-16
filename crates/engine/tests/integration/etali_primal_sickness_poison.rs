//! Regression test for Etali, Primal Sickness (back face of Etali, Primal
//! Conqueror): "Whenever Etali deals combat damage to a player, they get that
//! many poison counters."
//!
//! Before the fix, `try_parse_player_counter` had no "that many"/"that much"
//! quantity arm, so "get that many poison counters" matched neither the article
//! nor the number arm and the effect lowered to `Effect::Unimplemented`. The
//! trigger fired but gave zero poison counters.
//!
//! The fix adds the `parse_that_much_or_many` count-prefix arm, binding the
//! count to `QuantityRef::EventContextAmount` (CR 608.2h: the amount is the
//! triggering combat-damage event's total, determined once when the effect
//! applies). The "they" recipient (`resolve_they_pronoun` →
//! `TriggeringPlayer`) and the amount resolution
//! (`extract_amount_from_event` for `CombatDamageDealtToPlayer`) already work.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

use super::rules::run_combat;

/// Verbatim Oracle text of Etali, Primal Sickness
/// (`jq '.["etali, primal sickness"].oracle_text'` in `card-data.json`).
const ETALI_ORACLE: &str = "Trample, indestructible\n\
Whenever Etali deals combat damage to a player, they get that many poison \
counters. (A player with ten or more poison counters loses the game.)";

/// CR 608.2h + CR 104.3d: an unblocked Etali deals combat damage to a player,
/// and the event-bound trigger gives that player exactly that many poison
/// counters. Revert-failing: without the "that many" quantity arm the trigger
/// lowers to `Effect::Unimplemented` and P1 ends with 0 poison counters.
#[test]
fn etali_combat_damage_gives_that_many_poison_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A 7/7 Etali deals 7 combat damage to P1 → P1 gets 7 poison counters.
    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Sickness", 7, 7, ETALI_ORACLE)
        .id();

    let mut runner = scenario.build();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        0,
        "precondition: P1 starts with no poison counters"
    );

    // 7/7 Etali attacks P1 unblocked (CR 510.1b: 7 combat damage to P1).
    run_combat(&mut runner, vec![etali], vec![]);
    // CR 510.3a: the combat-damage trigger goes on the stack — drain it so the
    // poison counters land before asserting.
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        7,
        "CR 608.2h: P1 takes 7 combat damage and gets that many (7) poison \
         counters. A regression to the pre-fix parse lowers the trigger to \
         Unimplemented and leaves P1 at 0 poison."
    );
}

/// The count tracks the event amount, not a constant: a 3-power Etali gives 3
/// poison counters (not 7). Pins `EventContextAmount` against a hard-coded
/// value — a Fixed-count regression would give the same number in both tests.
#[test]
fn etali_poison_count_tracks_combat_damage_amount() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let etali = scenario
        .add_creature_from_oracle(P0, "Etali, Primal Sickness", 3, 3, ETALI_ORACLE)
        .id();

    let mut runner = scenario.build();

    run_combat(&mut runner, vec![etali], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].poison_counters,
        3,
        "CR 608.2h: the poison count equals the combat damage dealt (3), \
         proving the count is the event amount, not a constant"
    );
}
