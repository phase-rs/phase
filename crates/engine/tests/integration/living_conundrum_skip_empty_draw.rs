//! Discriminating runtime regression for **Living Conundrum** (std long-tail
//! batch):
//!
//! > If you would draw a card while your library has no cards in it, skip that
//! > draw instead.
//!
//! The residual gap was the replacement body "skip that draw [instead]": the
//! parser lowered it to `Effect::Unimplemented("skip that draw")`, a silent
//! runtime passthrough — the draw still happened (and, from an empty library,
//! flagged the controller for the CR 704.5b draw-from-empty loss).
//!
//! The fix recognizes the draw-suppression body and emits the structured
//! `QuantityModification::Prevent` (the same negation surface the
//! lifegain-negation arm uses). The draw pipeline honors it via
//! `ReplacementResult::Prevented`, so no draw happens.
//!
//! This drives the real draw pipeline through `DebugAction::DrawCards`.
//!
//! DISCRIMINATORS (both flip on revert):
//!   1. With an EMPTY library and the replacement active, drawing a card does
//!      NOT set `drew_from_empty_library` (no draw occurred). Pre-fix the
//!      `Unimplemented` passthrough let the empty-library draw through and set
//!      the flag.
//!   2. The "while your library has no cards" antecedent gate is preserved: with
//!      a NON-empty library, the replacement does NOT fire — a normal draw moves
//!      the top card to hand.
//!
//! CR 614.6: a replaced event never happens. CR 121.6 / CR 614.11: draw
//! replacements apply even from an empty library. CR 704.5b: drawing from an
//! empty library is a loss.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LIVING_CONUNDRUM: &str =
    "If you would draw a card while your library has no cards in it, skip that draw instead.";

/// Build a scenario with Living Conundrum's replacement on P0's battlefield.
/// `p0_library` seats that many anonymous cards on P0's library.
fn scenario_with_replacement(
    p0_library: usize,
) -> (GameRunner, Vec<engine::types::identifiers::ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Living Conundrum", 0, 0)
        .from_oracle_text(LIVING_CONUNDRUM);
    let mut lib = Vec::new();
    for i in 0..p0_library {
        lib.push(scenario.add_card_to_library_top(P0, &format!("Lib {i}")));
    }
    // P1 needs a library so SBAs don't end the game spuriously.
    for i in 0..5 {
        scenario.add_card_to_library_top(P1, &format!("P1 Lib {i}"));
    }
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    (runner, lib)
}

/// DISCRIMINATOR 1: with an empty library, the skip-that-draw replacement
/// prevents the draw — no card is drawn and the draw-from-empty flag is NOT set.
#[test]
fn empty_library_draw_is_skipped_no_card_no_loss_flag() {
    let (mut runner, _lib) = scenario_with_replacement(0);
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "the empty-library draw must be skipped — no card enters the hand"
    );
    assert!(
        !runner.state().players[P0.0 as usize].drew_from_empty_library,
        "the skipped draw must NOT flag a draw-from-empty (no draw happened); \
         pre-fix the Unimplemented passthrough drew from empty and set the flag"
    );
}

/// DISCRIMINATOR 2: the "while your library has no cards" antecedent is enforced
/// — with a non-empty library the replacement does NOT fire and the draw works.
#[test]
fn nonempty_library_draw_is_not_skipped() {
    let (mut runner, lib) = scenario_with_replacement(2);
    // `add_card_to_library_top` inserts at the front, so the LAST card added is
    // the top of the library.
    let top = *lib.last().unwrap();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "a draw from a non-empty library must NOT be skipped (antecedent gate)"
    );
    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Hand,
        "the drawn card must move from library to hand"
    );
}
