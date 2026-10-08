//! CR 611.3a + CR 122.1 — Tom Bombadil's lore-counter gate.
//!
//! Oracle: `As long as there are four or more lore counters among Sagas you
//! control, Tom Bombadil has hexproof and indestructible.`
//!
//! Before the `[kind] counters among [filter]` quantity phrase existed, this
//! condition parsed to `StaticCondition::Unrecognized`, which the layer
//! evaluator treats as satisfied. That is not a missing feature but an active
//! over-application: Tom Bombadil was hexproof and indestructible unconditionally,
//! strictly better than printed. So the load-bearing assertion here is the
//! NEGATIVE one — below the threshold he must have neither keyword.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const BOMBADIL_ORACLE: &str = "As long as there are four or more lore counters among Sagas you control, ~ has hexproof and indestructible.";

/// A Saga bearing `lore` lore counters. Chapter text is irrelevant to the gate —
/// only the counters are counted (CR 122.1) — but a Saga with chapter abilities
/// is the honest shape.
const SAGA_ORACLE: &str = "I — Create a 1/1 white Soldier creature token.\n\
II — Create a 1/1 white Soldier creature token.\n\
III — Create a 1/1 white Soldier creature token.\n\
IV — Create a 1/1 white Soldier creature token.\n\
V — Create a 1/1 white Soldier creature token.";

/// Build a board with Tom Bombadil and `lore_counters` lore spread over one Saga,
/// then report whether he has the granted keywords.
fn bombadil_has_keywords(lore_counters: u32) -> (bool, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bombadil = scenario
        .add_creature(P0, "Tom Bombadil", 4, 4)
        .as_legendary()
        .from_oracle_text(BOMBADIL_ORACLE)
        .id();

    let saga = scenario
        .add_creature(P0, "Lore Test Saga", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(SAGA_ORACLE)
        .id();
    scenario.with_counter(saga, CounterType::Lore, lore_counters);

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&bombadil];
    (
        obj.has_keyword(&Keyword::Hexproof),
        obj.has_keyword(&Keyword::Indestructible),
    )
}

/// The regression that matters: three lore counters is below the threshold, so
/// neither keyword may be granted. A permissive `Unrecognized` condition fails
/// exactly here.
#[test]
fn below_four_lore_counters_grants_nothing() {
    assert_eq!(
        bombadil_has_keywords(3),
        (false, false),
        "CR 611.3a: an unmet 'as long as' gate must grant neither keyword"
    );
}

/// CR 122.1: at the threshold the gate is satisfied and both keywords apply.
#[test]
fn four_lore_counters_grants_both_keywords() {
    assert_eq!(
        bombadil_has_keywords(4),
        (true, true),
        "four lore counters among Sagas you control satisfies the gate"
    );
}

/// The comparison is "four or more", not "exactly four".
#[test]
fn above_the_threshold_still_grants() {
    assert_eq!(bombadil_has_keywords(5), (true, true));
}

/// CR 122.1: the count is summed ACROSS every Saga you control, not read from a
/// single permanent — two Sagas at two counters each reach the threshold.
#[test]
fn lore_counters_are_summed_across_sagas() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bombadil = scenario
        .add_creature(P0, "Tom Bombadil", 4, 4)
        .as_legendary()
        .from_oracle_text(BOMBADIL_ORACLE)
        .id();

    for name in ["Lore Test Saga A", "Lore Test Saga B"] {
        let saga = scenario
            .add_creature(P0, name, 0, 0)
            .as_enchantment()
            .with_subtypes(vec!["Saga"])
            .from_oracle_text(SAGA_ORACLE)
            .id();
        scenario.with_counter(saga, CounterType::Lore, 2);
    }

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&bombadil];
    assert!(
        obj.has_keyword(&Keyword::Hexproof) && obj.has_keyword(&Keyword::Indestructible),
        "2 + 2 lore counters across two Sagas must satisfy the four-counter gate"
    );
}
