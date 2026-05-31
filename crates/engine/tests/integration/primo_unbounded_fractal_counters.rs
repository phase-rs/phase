//! Runtime regression for Primo, the Unbounded (#1361), driven through the real
//! `apply` pipeline (combat → trigger → token creation).
//!
//! Primo Oracle: "Whenever one or more creatures you control with base power 0
//! deal combat damage to a player, create a 0/0 green and blue Fractal creature
//! token. Put a number of +1/+1 counters on it equal to the damage dealt."
//!
//! The bug (#1361): the Fractal entered with ZERO counters because (a) the parser
//! dropped the `equal to [qty]` counter quantity (→ `Variable("X")` = 0), and
//! (b) `CombatDamageDealtToPlayer` carried no damage total for
//! `EventContextAmount` to read. The unit tests in `parser/`, `trigger_matchers`,
//! and `targeting` pin each hop; THIS test composes them end-to-end through
//! `apply` and asserts the resulting token's counters equal the combat damage —
//! the assertion that fails pre-fix (counters == 0) and passes post-fix.
//!
//! CR 120.1 + CR 510.2 + CR 603.7c: the triggering combat-damage step's total is
//! available to the resolving ability via the event context.

use super::rules::{run_combat, GameScenario, Phase, P0, P1};
use engine::types::counter::CounterType;

const PRIMO_ORACLE: &str = "Trample\nPrimo enters with twice X +1/+1 counters on it.\nWhenever one or more creatures you control with base power 0 deal combat damage to a player, create a 0/0 green and blue Fractal creature token. Put a number of +1/+1 counters on it equal to the damage dealt.";

#[test]
fn primo_fractal_token_enters_with_counters_equal_to_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Primo on the battlefield as the trigger source. Its own P/T is irrelevant
    // (it does not attack); use 4/4 so a 0-toughness body doesn't die to SBA.
    // Built from real Oracle text so the parser produces the
    // DamageDoneOnceByController trigger + base-power-0 source filter + Token body.
    scenario.add_creature_from_oracle(P0, "Primo, the Unbounded", 4, 4, PRIMO_ORACLE);

    // The attacker: a Fractal-shaped creature with BASE power 0 (matches Primo's
    // "base power 0" source filter, CR 208.4b) but CURRENT power 3 via three
    // +1/+1 counters (CR 613.7c). It deals 3 combat damage.
    let attacker = scenario.add_creature(P0, "Zero Striker", 3, 3).id();

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&attacker).unwrap();
        obj.base_power = Some(0);
        obj.base_toughness = Some(0);
        obj.counters.insert(CounterType::Plus1Plus1, 3);
    }

    let p1_life_before = runner.life(P1);

    // Zero Striker attacks P1 unblocked → 3 combat damage → Primo's trigger fires.
    run_combat(&mut runner, vec![attacker], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "attacker must deal 3 combat damage to P1 (proves combat actually happened)"
    );

    // Primo must have created a Fractal token, and it must enter with +1/+1
    // counters EQUAL TO the 3 combat damage dealt. Pre-fix this was 0.
    let fractal = runner
        .state()
        .objects
        .values()
        .find(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Fractal"))
        .expect("Primo must create a Fractal token after combat damage");
    assert_eq!(
        fractal
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        3,
        "Fractal token must enter with +1/+1 counters equal to the combat damage dealt (#1361)"
    );
}
