//! Sheriff of Safe Passage — RUNTIME witness for enters-with base-plus-additional
//! counters.
//!
//! Oracle (verbatim, card-data.json): "This creature enters with a +1/+1 counter
//! on it plus an additional +1/+1 counter on it for each other creature you
//! control." (The separate Plot ability is out of scope and untouched.)
//!
//! Sheriff is a 0/0 printed creature whose entire body is +1/+1 counters. On
//! main the " plus an additional … for each …" bridge defeated the suffix
//! parser, so it entered with only the base `Fixed(1)` counter — a 1/1
//! regardless of the board. The new base-plus-additional combinator makes the
//! count `Offset { ObjectCount(other creatures you control), 1 }`, so it enters
//! with 1 + (other creatures) counters.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1c: "[this permanent] enters with …" is a replacement effect.
//!   - CR 122.1a: a +1/+1 counter adds 1 to power and 1 to toughness.
//!   - CR 613.4c: +1/+1 counters apply in layer 7c.
//!
//! Discrimination: with K other creatures, the entrant carries 1 + K counters
//! and (base 0/0) is (1+K)/(1+K). K=3 → 4 counters / 4/4; K=0 → 1 counter / 1/1.
//! Reverting the combinator makes the count `Fixed(1)` → always 1 counter / 1/1,
//! so the K=3 assertion flips to fail.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHERIFF: &str = "This creature enters with a +1/+1 counter on it plus an \
additional +1/+1 counter on it for each other creature you control.";

/// Cast Sheriff with `others` other creatures on P0's battlefield. Returns
/// `(plus1plus1 counters on the entrant, effective (power, toughness))`.
fn cast_sheriff_with_others(others: usize) -> (u32, (i32, i32)) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for i in 0..others {
        scenario.add_creature(P0, &format!("Deputy {i}"), 1, 1);
    }

    // Sheriff's printed body is 0/0 — it survives only via the counters it
    // enters with (CR 614.12 applies the replacement before the SBA check).
    let sheriff = scenario
        .add_creature_to_hand_from_oracle(P0, "Sheriff of Safe Passage", 0, 0, SHERIFF)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(sheriff).resolve();

    // The cast object may be reissued a new id as it enters; find the entered
    // permanent by name on the battlefield.
    let entered = outcome
        .find_object(|o| o.name == "Sheriff of Safe Passage" && o.zone == Zone::Battlefield)
        .expect(
            "Sheriff must have entered the battlefield (if absent it died as a 0/0 — the \
             enters-with counters never applied)",
        );
    (
        outcome.counters(entered, CounterType::Plus1Plus1),
        outcome.power_toughness(entered),
    )
}

#[test]
fn sheriff_enters_with_base_plus_one_per_other_creature() {
    // K = 3 other creatures → 1 base + 3 = 4 counters → 4/4.
    assert_eq!(
        cast_sheriff_with_others(3),
        (4, (4, 4)),
        "1 base + 3 other creatures = 4 +1/+1 counters (4/4); (1, (1,1)) means the \
         base-plus-additional combinator was reverted (count stuck at Fixed(1))"
    );

    // K = 0 → only the base counter applies → 1 counter → 1/1.
    assert_eq!(
        cast_sheriff_with_others(0),
        (1, (1, 1)),
        "with no other creatures only the base +1/+1 counter applies"
    );
}
