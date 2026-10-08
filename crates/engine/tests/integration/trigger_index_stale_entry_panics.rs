//! CR 113.6 — falsification probe for the `TriggerIndex` stale-entry detector.
//!
//! This file exists for one reason: to prove the `debug_assert!` in
//! `trigger_index::candidates_for_event` actually fires. It lives in the
//! integration crate because that is the ONLY test build where the assertion is
//! live.
//!
//! The gate is `#[cfg(all(debug_assertions, not(test)))]`. Rust passes
//! `--cfg test` only when a crate is compiled as its own test harness, so:
//!
//! - the engine's in-crate `#[cfg(test)] mod tests` compiles with `cfg(test)`
//!   ON, and the assertion is compiled OUT — which is what lets the hostile
//!   unit fixtures in `trigger_index.rs` construct a stale entry at all;
//! - these integration binaries link `phase_engine` as an ordinary lib
//!   dependency, WITHOUT `cfg(test)`, so the assertion is LIVE here.
//!
//! That asymmetry is deliberate: it makes the whole integration suite a
//! recurrence detector at zero extra cost. A detector nobody has watched go red
//! is not evidence, so this probe watches it.

use engine::game::scenario::GameScenario;
use engine::game::trigger_index::candidates_for_event;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// CR 113.6: induce the `state.battlefield` / `obj.zone` desync the guard
/// exists for — an indexed battlefield permanent whose live zone has moved to
/// the hand while `state.battlefield` and the index are left stale — then
/// consult the index and require the detector to panic.
///
/// The desync is written directly through the public `state.objects` /
/// `obj.zone` fields. Deliberately NOT via `zones::move_to_zone`: its CR 603.6c
/// hook removes the object from the index whenever it leaves the battlefield,
/// so the stale entry this probe needs would never exist.
#[test]
#[should_panic(expected = "TriggerIndex holds off-battlefield candidates")]
fn stale_off_battlefield_index_entry_panics_in_integration_builds() {
    let mut scenario = GameScenario::new();
    let source = scenario
        .add_creature_from_oracle(
            PlayerId(0),
            "The Locust God",
            4,
            4,
            "Whenever you draw a card, create a 1/1 blue and red Insect creature \
             token with flying and haste.",
        )
        .id();
    let mut runner = scenario.build();

    // Reach-guard: the object must actually be indexed and reachable as a
    // candidate BEFORE the desync, or a panic-free run would prove nothing and
    // a panic could come from somewhere else entirely.
    let event = GameEvent::CardDrawn {
        player_id: PlayerId(0),
        object_id: ObjectId(1),
        nth_in_turn: 1,
        nth_in_step: 1,
    };
    assert!(
        !candidates_for_event(runner.state(), &event).is_empty(),
        "reach-guard: the consult must return candidates before the desync is induced"
    );

    // Induce the desync, leaving `state.battlefield` and the index untouched.
    runner.state_mut().objects.get_mut(&source).unwrap().zone = Zone::Hand;
    assert!(
        runner.state().battlefield.contains(&source),
        "reach-guard: the stale id must still be in `state.battlefield`"
    );

    // The consult is what trips the detector.
    let _ = candidates_for_event(runner.state(), &event);
}
