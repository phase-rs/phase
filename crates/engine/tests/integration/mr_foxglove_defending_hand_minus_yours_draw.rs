//! Runtime pipeline regression — Mr. Foxglove.
//!
//! Oracle: "Whenever Mr. Foxglove attacks, draw cards equal to the number of
//! cards in defending player's hand minus the number of cards in your hand. If
//! you didn't draw cards this way, you may put a creature card from your hand
//! onto the battlefield."
//!
//! This exercises the full B-cluster fix:
//!   - B1: the `defending player's hand` quantity arm.
//!   - B2: routing the Draw count through the arithmetic-aware
//!     `parse_cda_quantity` so the binary `minus` resolves to
//!     `Sum[HandSize{DefendingPlayer}, Multiply{-1, HandSize{Controller}}]`.
//!   - B3: clamping a negative draw count to 0 (CR 107.1b).
//!
//! Both cases drive the real combat + attack-trigger pipeline (`run_combat`),
//! resolving `DefendingPlayer` against live combat state.
//!
//! DISCRIMINATING:
//!   - Case A (defender 5, controller 2 → +3): fails to draw if the parser
//!     fix (B1/B2) is reverted (clause drops to `Unimplemented`, 0 drawn).
//!   - Case B (defender 1, controller 4 → 0, NOT ~4 billion): the load-bearing
//!     B3 clamp test. Without `.max(0)`, `(-3) as u32` wraps to ~4.29e9 and the
//!     draw either panics (library exhausted / SBA) or empties the library. The
//!     assertion `drew == 0` fails if the clamp is reverted.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Build a scenario with Mr. Foxglove on P0's battlefield (ready to attack),
/// `controller_hand` filler cards in P0's hand, `defender_hand` cards in P1's
/// hand, and ample libraries for both players.
fn setup(controller_hand: usize, defender_hand: usize) -> (GameRunner, ObjectId) {
    let db = load_db().expect("card database must be available for this test");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let foxglove = scenario.add_real_card(P0, "Mr. Foxglove", Zone::Battlefield, db);

    // Generic filler cards — only the COUNT matters for the hand-size quantity.
    for i in 0..controller_hand {
        scenario.add_card_to_hand(P0, &format!("Controller Card {i}"));
    }
    for i in 0..defender_hand {
        scenario.add_card_to_hand(P1, &format!("Defender Card {i}"));
    }

    // Plenty of library so a (clamped) draw never exhausts it.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
    }
    for _ in 0..20 {
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let runner = scenario.build();
    (runner, foxglove)
}

/// Drive combat and drain Mr. Foxglove's attack trigger, declining the optional
/// "put a creature card onto the battlefield" follow-up when it surfaces.
fn attack_and_resolve(runner: &mut GameRunner, foxglove: ObjectId) {
    crate::rules::run_combat(runner, vec![foxglove], vec![]);
    for _ in 0..16 {
        runner.advance_until_stack_empty();
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the optional put-creature follow-up");
            }
            _ => break,
        }
    }
}

#[test]
fn foxglove_draws_defender_hand_minus_controller_hand() {
    let (mut runner, foxglove) = setup(2, 5);
    // P0 attacks P1; defender hand = 5, controller hand = 2 → draw 3.
    assert_eq!(
        runner.state().objects[&foxglove].zone,
        Zone::Battlefield,
        "Foxglove must be on the battlefield to attack"
    );
    let p0_hand_before = runner.state().players[0].hand.len();
    attack_and_resolve(&mut runner, foxglove);
    let p0_hand_after = runner.state().players[0].hand.len();
    assert_eq!(
        p0_hand_after as i64 - p0_hand_before as i64,
        3,
        "defender hand (5) minus controller hand (2) = 3 cards drawn"
    );
}

/// B3 clamp: when the controller's hand is LARGER than the defender's, the raw
/// count is negative; CR 107.1b forces it to 0. Without `.max(0)` the `as u32`
/// cast wraps to ~4 billion and the draw empties the library / panics.
#[test]
fn foxglove_negative_draw_count_clamps_to_zero() {
    let (mut runner, foxglove) = setup(4, 1);
    let p0_hand_before = runner.state().players[0].hand.len();
    let p0_lib_before = runner.state().players[0].library.len();
    attack_and_resolve(&mut runner, foxglove);
    let p0_hand_after = runner.state().players[0].hand.len();
    let p0_lib_after = runner.state().players[0].library.len();
    assert_eq!(
        p0_hand_after, p0_hand_before,
        "defender hand (1) minus controller hand (4) = -3 → clamped to 0 cards drawn (CR 107.1b)"
    );
    assert_eq!(
        p0_lib_after, p0_lib_before,
        "no cards may leave the library when the clamped draw count is 0"
    );
}
