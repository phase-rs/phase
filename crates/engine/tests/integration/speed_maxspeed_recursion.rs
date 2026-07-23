//! Regression: `speed::has_max_speed` re-entrancy / stack-overflow guard.
//!
//! ## The bug (pre-guard)
//!
//! `has_max_speed` -> `can_increase_speed_beyond_4` scans
//! `active_static_definitions`, which evaluates each static's CR 604.1
//! functioning condition. A `StaticCondition::HasMaxSpeed` condition maps
//! (layers.rs) back to `has_max_speed`, re-entering `can_increase_speed_beyond_4`
//! -> the scan -> the same condition -> infinite recursion -> stack overflow.
//! This fires on any board where the controller has a HasMaxSpeed-gated static
//! (e.g. Racers' Scoreboard) and was hit driving a saved 4-player Commander game
//! through `apply(PassPriority)` (`resolve_bench` repro: "thread 'main' has
//! overflowed its stack / fatal runtime error: stack overflow").
//!
//! Every assertion in this file drives the REAL layer-derivation path
//! (`evaluate_layers`), which evaluates the HasMaxSpeed-gated static's condition
//! via `active_static_definitions` — exactly the recursing path. Each `#[test]`
//! here STACK-OVERFLOWS on the pre-guard code; with the thread-local re-entrancy
//! guard in `speed.rs` they terminate. These are pipeline/derivation tests, not
//! shape tests: the modified power read after `evaluate_layers` is computed by
//! the engine layer system, not asserted into existence.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 702.179e: A player has max speed if their speed is 4.
//!   - CR 613.8b: a dependency loop is broken; values are taken without circular
//!     contribution (rules basis for the base-cap re-entry answer).
//!   - CR 604.1: a static's functioning condition is re-evaluated continuously.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::game::speed::{has_max_speed, increase_speed};
use engine::types::ability::{
    ContinuousModification, StaticCondition, StaticDefinition, TargetFilter, TypeFilter,
    TypedFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

/// The power a Racers'-Scoreboard-shaped HasMaxSpeed anthem grants to creatures
/// the controller owns while that player has max speed. Observable, so the test
/// can confirm the gated static is ACTIVE (or not) after layer derivation.
const ANTHEM_BONUS: i32 = 1;

/// Build a board for `P0` and return `(runner, gomif_id, beater_id)`.
///
/// `beater` is a vanilla 2/2 creature `P0` controls (the anthem's subject).
/// A Racers'-Scoreboard-shaped enchantment `P0` controls carries a `Continuous`
/// `AddPower`/`AddToughness` anthem gated by `StaticCondition::HasMaxSpeed` —
/// the static whose condition re-enters `has_max_speed`.
/// When `with_gomif` is true, `P0` also controls a Gomif-shaped permanent: a
/// `StaticDefinition { mode: SpeedCanIncreaseBeyondFour, condition: None }`.
fn build(
    with_gomif: bool,
) -> (
    engine::game::scenario::GameRunner,
    Option<ObjectId>,
    ObjectId,
) {
    let mut scenario = GameScenario::new();

    let beater = scenario.add_vanilla(P0, 2, 2);

    // Racers'-Scoreboard-shaped HasMaxSpeed-gated anthem.
    let scoreboard_static = StaticDefinition::new(StaticMode::Continuous)
        .affected(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
        .modifications(vec![
            ContinuousModification::AddPower {
                value: ANTHEM_BONUS,
            },
            ContinuousModification::AddToughness {
                value: ANTHEM_BONUS,
            },
        ])
        .condition(StaticCondition::HasMaxSpeed);
    scenario
        .add_creature(P0, "Scoreboard", 0, 0)
        .with_static_definition(scoreboard_static);

    let gomif = with_gomif.then(|| {
        // Gomif-shaped: unconditionally allows speed to exceed 4.
        let gomif_static = StaticDefinition::new(StaticMode::SpeedCanIncreaseBeyondFour);
        scenario
            .add_creature(P0, "Gomif", 0, 0)
            .with_static_definition(gomif_static)
            .id()
    });

    let runner = scenario.build();
    (runner, gomif, beater)
}

fn set_speed(runner: &mut engine::game::scenario::GameRunner, player: PlayerId, speed: Option<u8>) {
    for p in runner.state_mut().players.iter_mut() {
        if p.id == player {
            p.speed = speed;
        }
    }
}

/// Drive the layer system and read `beater`'s effective power. This is the path
/// that evaluates the HasMaxSpeed-gated anthem's condition via
/// `active_static_definitions` — the recursing path. Returns the power computed
/// by the engine after derivation.
fn derived_power(runner: &mut engine::game::scenario::GameRunner, beater: ObjectId) -> i32 {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    runner
        .state()
        .objects
        .get(&beater)
        .expect("beater exists")
        .power
        .expect("creature has power")
}

/// TERMINATION: with the HasMaxSpeed-gated static present and `P0` at speed 4,
/// driving `evaluate_layers` (which evaluates the static's condition through the
/// recursing path) COMPLETES instead of overflowing the stack, `has_max_speed`
/// returns true, and the gated anthem applies (2/2 -> 3/3).
#[test]
fn layer_derivation_terminates_with_hasmaxspeed_gated_static() {
    let (mut runner, _gomif, beater) = build(false);
    set_speed(&mut runner, P0, Some(4));

    // Drives active_static_definitions condition-eval -> has_max_speed -> ...
    // This overflows the stack on the pre-guard code.
    let power = derived_power(&mut runner, beater);

    assert!(
        has_max_speed(runner.state(), P0),
        "speed 4 with no beyond-4 static is exactly max speed"
    );
    assert_eq!(
        power,
        2 + ANTHEM_BONUS,
        "HasMaxSpeed-gated anthem must apply at speed 4"
    );
}

/// SEMANTICS: speed 4 -> max speed true; speed 3 -> false (cap is 4 with no
/// beyond-4 static). At speed 3 the gated anthem is inactive, so the beater
/// stays 2/2; at speed 4 it becomes 3/3.
#[test]
fn max_speed_semantics_without_beyond_four() {
    let (mut runner, _gomif, beater) = build(false);

    set_speed(&mut runner, P0, Some(3));
    assert!(
        !has_max_speed(runner.state(), P0),
        "speed 3 is below max speed"
    );
    assert_eq!(
        derived_power(&mut runner, beater),
        2,
        "below max speed the HasMaxSpeed anthem must be inactive"
    );

    set_speed(&mut runner, P0, Some(4));
    assert!(has_max_speed(runner.state(), P0), "speed 4 is max speed");
    assert_eq!(
        derived_power(&mut runner, beater),
        2 + ANTHEM_BONUS,
        "at max speed the HasMaxSpeed anthem must be active"
    );
}

/// INTERACTION (discriminating case): `P0` at speed 5 WITH Gomif's
/// `SpeedCanIncreaseBeyondFour` static AND the HasMaxSpeed-gated anthem.
/// `has_max_speed` must be true (speed >= 4 with a beyond-4 static), AND the
/// gated anthem must be observably ACTIVE (beater 2/2 -> 3/3) — proving the
/// guard's base-cap re-entry answer did NOT corrupt the consumed result. The
/// real layer pass re-evaluates HasMaxSpeed through the unguarded outer call.
#[test]
fn beyond_four_speed_keeps_hasmaxspeed_static_active() {
    let (mut runner, gomif, beater) = build(true);
    assert!(gomif.is_some(), "Gomif static present for this case");
    set_speed(&mut runner, P0, Some(5));

    assert!(
        has_max_speed(runner.state(), P0),
        "speed 5 WITH a SpeedCanIncreaseBeyondFour static is max speed (>= 4)"
    );
    assert_eq!(
        derived_power(&mut runner, beater),
        2 + ANTHEM_BONUS,
        "the HasMaxSpeed-gated anthem must be active at speed 5 when a beyond-4 \
         static is present — the guard must not corrupt the consumed result"
    );
}

/// WITHOUT Gomif: `increase_speed` from 4 cannot exceed the default cap of 4,
/// and a speed of 5 set directly (no beyond-4 static) is NOT max speed (the cap
/// is exactly 4). Both paths run through `can_increase_speed_beyond_4` and must
/// terminate.
#[test]
fn no_beyond_four_static_caps_increase_and_max_speed_at_four() {
    let (mut runner, _gomif, _beater) = build(false);

    // increase_speed runs can_increase_speed_beyond_4; from 4 it stays 4.
    set_speed(&mut runner, P0, Some(4));
    let mut events = Vec::new();
    increase_speed(runner.state_mut(), P0, 1, &mut events);
    let speed_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .and_then(|p| p.speed);
    assert_eq!(
        speed_after,
        Some(4),
        "without a beyond-4 static, increase_speed is capped at 4"
    );

    // Speed 5 set directly with no beyond-4 static: cap is exactly 4, so 5 is
    // NOT max speed.
    set_speed(&mut runner, P0, Some(5));
    assert!(
        !has_max_speed(runner.state(), P0),
        "speed 5 with no beyond-4 static is not max speed (cap is exactly 4)"
    );
}
