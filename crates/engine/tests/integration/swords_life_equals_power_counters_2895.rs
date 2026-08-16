//! Runtime regression for #2895 — Swords to Plowshares on a counter'd creature.
//!
//! "Exile target creature. Its controller gains life equal to its power." must
//! gain life equal to the exiled creature's power as last known immediately
//! before it left the battlefield (CR 608.2h) — i.e. INCLUDING +1/+1 counters.
//! Previously the `Target` power scope read the live object after exile, where
//! counters have been stripped, so a 3/3 with eight +1/+1 counters gained only
//! 3 life instead of 11.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const SWORDS: &str = "Exile target creature. Its controller gains life equal to its power.";

#[test]
fn life_equals_power_includes_counters_via_lki() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // P0's creature: base 3/3 with eight +1/+1 counters → current power 11.
    let beast = scenario
        .add_creature(P0, "Counter Beast", 3, 3)
        .with_plus_counters(8)
        .id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    runner.cast(spell).target_object(beast).resolve();
    runner.advance_until_stack_empty();

    // The targeted creature is exiled, and its controller gains 11 (3 base + 8
    // counters), not 3.
    assert_eq!(
        runner.battlefield_count(P0),
        0,
        "the targeted creature must be exiled"
    );
    assert_eq!(
        runner.life(P0) - life_before,
        11,
        "controller gains life equal to the exiled creature's power WITH +1/+1 \
         counters (11), not its base power (3)"
    );
}
