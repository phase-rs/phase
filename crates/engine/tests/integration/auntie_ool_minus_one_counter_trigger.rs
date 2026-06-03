//! Regression for issue #1510 — Auntie Ool, Cursewretch's "-1/-1 counters put
//! on a creature" trigger, and the CR 704.5q +1/+1 / -1/-1 annihilation that
//! surrounds it.
//!
//! Oracle (relevant line):
//!   "Whenever one or more -1/-1 counters are put on a creature, draw a card if
//!    you control that creature. If you don't control it, its controller loses
//!    1 life."
//!
//! Bug as reported: putting -1/-1 counters on a creature that already has +1/+1
//! counters "removes the -1/-1 counters". The removal is correct per CR 704.5q
//! (pairs of +1/+1 and -1/-1 counters cancel). The genuine defect surfaced while
//! reproducing it: the trigger resolved BOTH branches unconditionally — the
//! controller always drew a card AND "that creature's controller" always lost 1
//! life — instead of choosing exactly one branch on whether the controller
//! controls the creature the counters were put on.
//!
//! These tests drive the real cast → counter-placement → SBA → trigger pipeline
//! from parsed Oracle text (no hand-built AST), so they fail on the pre-fix
//! engine and pass once the control branch is gated correctly.

use engine::game::scenario::{CastOutcome, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const AUNTIE: &str = "Whenever one or more -1/-1 counters are put on a creature, draw a card if you control that creature. If you don't control it, its controller loses 1 life.";
const SHRINK: &str = "Put a -1/-1 counter on target creature.";

/// P0's library is stocked with this many cards before the cast (so the
/// conditional draw never decks them out); the post-resolution `zone_count`
/// reads against this baseline.
const LIBRARY_BASELINE: usize = 5;

/// Cast a one-shot "-1/-1 counter on target creature" sorcery at `target` while
/// the controller (P0) has Auntie Ool on the battlefield, driving the cast →
/// counter-placement → SBA → trigger pipeline through `resolve()`. Returns the
/// resolution outcome and the targeted object for counter assertions.
fn put_minus_counter_with_auntie(
    target_opponent: bool,
    target_plus_counters: u32,
) -> (CastOutcome, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);
    scenario.with_life(P1, 20);

    scenario
        .add_creature_from_oracle(P0, "Auntie Ool, Cursewretch", 4, 4, AUNTIE)
        .id();
    let mut my_creature = scenario.add_creature(P0, "Hill Giant", 3, 3);
    if target_plus_counters > 0 {
        my_creature.with_plus_counters(target_plus_counters);
    }
    let my_creature = my_creature.id();
    let mut opp_creature = scenario.add_creature(P1, "Ogre", 3, 3);
    if target_plus_counters > 0 {
        opp_creature.with_plus_counters(target_plus_counters);
    }
    let opp_creature = opp_creature.id();

    let shrink = scenario
        .add_spell_to_hand_from_oracle(P0, "Shrink", false, SHRINK)
        .id();
    // Stock P0's library so the conditional draw doesn't deck them out.
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5"]);

    let mut runner = scenario.build();
    let target = if target_opponent {
        opp_creature
    } else {
        my_creature
    };

    let outcome = runner.cast(shrink).target_object(target).resolve();

    // CR 704.5q: with N +1/+1 counters present, one -1/-1 cancels one +1/+1.
    if target_plus_counters > 0 {
        assert_eq!(
            outcome.counters(target, CounterType::Plus1Plus1),
            target_plus_counters - 1,
            "CR 704.5q: one +1/+1 counter should be cancelled"
        );
        assert_eq!(
            outcome.counters(target, CounterType::Minus1Minus1),
            0,
            "CR 704.5q: the lone -1/-1 counter should be cancelled"
        );
    }

    (outcome, target)
}

/// CR 122.1 + control branch: you control the creature the -1/-1 counter is put
/// on → you draw exactly one card and nobody loses life.
#[test]
fn counters_on_creature_you_control_draws_only() {
    let (out, _target) = put_minus_counter_with_auntie(false, 0);
    assert_eq!(
        out.zone_count(P0, Zone::Library),
        LIBRARY_BASELINE - 1,
        "you control it → draw exactly one card"
    );
    out.assert_life_delta(P0, 0); // you control it → you do NOT lose life
    out.assert_life_delta(P1, 0); // opponent is untouched
}

/// CR 122.1 + control branch: you don't control the creature → its controller
/// loses 1 life and you draw nothing.
#[test]
fn counters_on_creature_you_dont_control_drains_only() {
    let (out, _target) = put_minus_counter_with_auntie(true, 0);
    assert_eq!(
        out.zone_count(P0, Zone::Library),
        LIBRARY_BASELINE,
        "you don't control it → NO draw"
    );
    out.assert_life_delta(P1, -1); // you don't control it → its controller loses 1 life
    out.assert_life_delta(P0, 0); // you do not lose life
}

/// CR 704.5q: the -1/-1 counter cancels a pre-existing +1/+1 counter, but the
/// trigger still fires from the counter-placement event (you control it → draw).
#[test]
fn annihilation_still_fires_the_trigger_when_you_control_it() {
    let (out, _target) = put_minus_counter_with_auntie(false, 1);
    assert_eq!(
        out.zone_count(P0, Zone::Library),
        LIBRARY_BASELINE - 1,
        "trigger still fires → draw one card"
    );
    out.assert_life_delta(P0, 0);
    out.assert_life_delta(P1, 0);
}

/// CR 704.5q + drain branch: annihilation on an opponent's +1/+1 creature still
/// drives the life-loss branch.
#[test]
fn annihilation_still_fires_the_trigger_when_you_dont_control_it() {
    let (out, _target) = put_minus_counter_with_auntie(true, 1);
    assert_eq!(
        out.zone_count(P0, Zone::Library),
        LIBRARY_BASELINE,
        "no draw"
    );
    out.assert_life_delta(P1, -1); // controller loses 1 life
    out.assert_life_delta(P0, 0);
}
