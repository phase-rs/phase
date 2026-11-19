//! Issue #10468: a board wipe must not resolve a source context for every
//! (departure, co-departed creature) pair.
//!
//! CR 603.10a: a leaves-the-battlefield observer destroyed by the same board
//! wipe as the creatures it counts still triggers for each of them. Every
//! departure of an N-creature wipe therefore carries an (N - 1)-member
//! `co_departed` group, and trigger collection used to resolve each member's
//! last-known source context by scanning the whole event batch and the
//! per-turn departure ledger: O(N³) for a Wrath of God over a token board.
//! Only a co-departed creature that left with a trigger can observe anything,
//! so the work must scale with the observers that exist, not with N².

use engine::game::perf_counters;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::events::GameEvent;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WRATH_OF_GOD: &str = "Destroy all creatures. They can't be regenerated.";
const ZULAPORT_CUTTHROAT: &str = "Whenever this creature or another creature you control dies, each opponent loses 1 life and you gain 1 life.";

/// Vanilla creatures per player alongside Zulaport Cutthroat.
const VANILLA_PER_PLAYER: usize = 6;

/// CR 603.10a: Zulaport Cutthroat, destroyed by the same Wrath of God as the
/// creatures it watches, triggers once for itself and once for each co-dying
/// creature its controller controlled, and the co-departure arm resolves a
/// source context only for that one observer, once per other departure.
///
/// Discriminating: without the observer gate, every co-departed vanilla
/// creature is resolved for every departure, so `observer_resolutions` is
/// N·(N − 1) = 156 instead of N − 1 = 12 for this 13-creature board.
#[test]
fn wrath_resolves_only_observers_that_left_with_a_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let zulaport = scenario
        .add_creature_from_oracle(P0, "Zulaport Cutthroat", 1, 1, ZULAPORT_CUTTHROAT)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Black],
            generic: 1,
        })
        .id();
    let mut creatures = vec![zulaport];
    for i in 0..VANILLA_PER_PLAYER {
        creatures.push(scenario.add_creature(P0, &format!("Bear {i}"), 2, 2).id());
        creatures.push(scenario.add_creature(P1, &format!("Ogre {i}"), 3, 3).id());
    }
    let wrath = scenario
        .add_spell_to_hand_from_oracle(P0, "Wrath of God", false, WRATH_OF_GOD)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::White],
            generic: 2,
        })
        .id();
    let plains: Vec<_> = (0..4)
        .map(|_| scenario.add_basic_land(P0, ManaColor::White))
        .collect();

    let mut runner = scenario.build();
    let commit = runner.cast(wrath).commit();
    // The Wrath's {2}{W}{W} was paid from the four Plains.
    for land in &plains {
        assert!(
            commit.state().objects[land].tapped,
            "every Plains must be tapped to pay for Wrath of God"
        );
    }

    perf_counters::reset();
    let outcome = commit.resolve();
    let counters = perf_counters::co_departure_observer_snapshot();

    outcome.assert_zone(&creatures, Zone::Graveyard);
    // Reach guard: the co-departed observer really observed the wipe. Zulaport
    // triggers for itself and each of its controller's six co-dying creatures.
    let observed = 1 + VANILLA_PER_PLAYER as i32;
    outcome.assert_life_delta(P0, observed);
    outcome.assert_life_delta(P1, -observed);
    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { record, .. } if record.co_departed.len() == creatures.len() - 1
        )),
        "the wipe must stamp each departure with its full co-departure group"
    );

    assert_eq!(
        counters.observer_resolutions,
        (creatures.len() - 1) as u64,
        "only Zulaport Cutthroat left with a trigger, so it is the only co-departed \
         observer resolved, once for each other departure"
    );
    assert_eq!(
        counters.index_builds, 1,
        "the co-departure index must be built once per collection pass"
    );
}
