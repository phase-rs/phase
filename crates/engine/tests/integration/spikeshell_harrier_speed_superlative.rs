//! Spikeshell Harrier — player-property superlative-comparison conditional
//! speed-decrease (#341).
//!
//! Oracle: "When this creature enters, return target creature or Vehicle an
//! opponent controls to its owner's hand. If that opponent's speed is greater
//! than each other player's speed, reduce that opponent's speed by 1. This
//! effect can't reduce their speed below 1."
//!
//! Three #341 gaps proven end-to-end:
//!   1. `Effect::ChangeSpeed { direction: Decrease, floor: Some(1) }` — the
//!      speed-decrease effect, floored at 1.
//!   2. The chained sub-ability's `AbilityCondition::QuantityCheck` gate
//!      (CR 608.2c) — a resolution-time conditional second effect, NOT a
//!      trigger intervening-if.
//!   3. The "that opponent" anaphor → the bounced object's controller
//!      (`PlayerScope` / `PlayerFilter::ParentObjectTargetController`).
//!
//! Drives the real cast -> stack -> ETB trigger -> bounce -> conditional
//! sub-ability check -> `ChangeSpeed` pipeline through `apply`. The ETB
//! trigger has exactly one legal bounce target (P1's lone creature), so it
//! auto-resolves during stack advancement. This is a pipeline test, not a
//! shape test.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.3: once an ability triggers, its controller puts it on the stack.
//!   - CR 608.2c: the controller follows the ability's instructions in the
//!     order written; a condition gating a later instruction is evaluated as
//!     the ability resolves.
//!   - CR 702.179f: an effect that refers to speed treats "no speed" as 0.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::speed::effective_speed;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Spikeshell Harrier's printed Oracle text — byte-identical to
/// `client/public/card-data.json`.
const SPIKESHELL_HARRIER: &str = "When this creature enters, return target \
creature or Vehicle an opponent controls to its owner's hand. If that \
opponent's speed is greater than each other player's speed, reduce that \
opponent's speed by 1. This effect can't reduce their speed below 1.";

/// Build the Spikeshell scenario, cast the Harrier, and resolve its ETB
/// trigger end-to-end. P1 has exactly one creature (the only legal bounce
/// target), so the trigger resolves without a manual target choice. Returns
/// the final speed of P1 ("that opponent") after the conditional sub-ability.
fn run(p0_speed: Option<u8>, p1_speed: Option<u8>) -> u8 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P1's creature — the bounce target. "that opponent" anaphors to P1.
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);

    let harrier = scenario
        .add_creature_to_hand_from_oracle(P0, "Spikeshell Harrier", 4, 4, SPIKESHELL_HARRIER)
        .id();

    let mut runner = scenario.build();

    // Set per-player speed directly — `GameScenario` has no speed builder.
    for p in runner.state_mut().players.iter_mut() {
        if p.id == P0 {
            p.speed = p0_speed;
        } else if p.id == P1 {
            p.speed = p1_speed;
        }
    }

    cast_and_resolve(&mut runner, harrier);
    effective_speed(runner.state(), P1)
}

/// Cast Spikeshell Harrier from hand and resolve the full ETB chain.
fn cast_and_resolve(runner: &mut engine::game::scenario::GameRunner, hand_card: ObjectId) {
    let card_id = runner
        .state()
        .objects
        .get(&hand_card)
        .expect("hand card exists")
        .card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: hand_card,
            card_id,
            targets: vec![],
        })
        .expect("casting Spikeshell Harrier should succeed");
    runner.advance_until_stack_empty();
}

fn speed(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> u8 {
    effective_speed(runner.state(), player)
}

/// Gap 1+2+3: P1's speed is the strict unique maximum across all players —
/// the `QuantityCheck` gate passes, and `ChangeSpeed Decrease` reduces P1's
/// speed by 1.
#[test]
fn reduces_speed_when_that_opponent_is_strict_unique_maximum() {
    // P1 = 3, P0 = 1. P1 > max(other players) = P0's 1 → condition true.
    assert_eq!(
        run(Some(1), Some(3)),
        2,
        "P1's speed is the strict unique maximum — ChangeSpeed Decrease must \
         reduce 3 → 2"
    );
}

/// Gap 2: P1's speed is TIED for the maximum (not strictly greater) — GT is
/// false, so the speed is NOT reduced.
#[test]
fn does_not_reduce_speed_when_tied_for_maximum() {
    // P1 = 2, P0 = 2. P1 > max(others)=2 is false (tie).
    assert_eq!(
        run(Some(2), Some(2)),
        2,
        "P1 is tied for the maximum, not strictly greater — speed unchanged"
    );
}

/// Gap 2: P1's speed is NOT the maximum — GT is false, speed NOT reduced.
#[test]
fn does_not_reduce_speed_when_not_maximum() {
    // P1 = 1, P0 = 3. P1 > max(others)=3 is false.
    assert_eq!(
        run(Some(3), Some(1)),
        1,
        "P1 is not the maximum — speed unchanged"
    );
}

/// Gap 1: the `floor: Some(1)` clamp — P1's speed is 1 (and the strict max),
/// the condition is true, but `decrease_speed` clamps the result at 1.
#[test]
fn floors_decrease_at_one() {
    // P1 = 1, P0 = 0. P1 > max(others)=0 → condition true; 1 - 1 = 0, floored
    // back up to 1.
    assert_eq!(
        run(Some(0), Some(1)),
        1,
        "the card-text floor clamps the decrease at 1 — speed must not drop \
         to 0"
    );
}

/// Cross-check the harness: with the ETB resolved, P0's own speed is never
/// touched (the effect targets only "that opponent" = P1).
#[test]
fn controllers_own_speed_is_untouched() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Grizzly Bears", 2, 2);
    let harrier = scenario
        .add_creature_to_hand_from_oracle(P0, "Spikeshell Harrier", 4, 4, SPIKESHELL_HARRIER)
        .id();
    let mut runner = scenario.build();
    for p in runner.state_mut().players.iter_mut() {
        if p.id == P0 {
            p.speed = Some(1);
        } else if p.id == P1 {
            p.speed = Some(3);
        }
    }
    cast_and_resolve(&mut runner, harrier);
    assert_eq!(speed(&runner, P0), 1, "P0's own speed must be untouched");
    assert_eq!(speed(&runner, P1), 2, "P1's speed reduced 3 → 2");
}
