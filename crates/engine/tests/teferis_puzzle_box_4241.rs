//! Reproduction for issue #4241 — Teferi's Puzzle Box.
//!
//! "At the beginning of each player's draw step, that player puts the cards in
//! their hand on the bottom of their library in any order, then draws that many
//! cards." (CR 603.2 per-player beginning-of-step trigger.)
//!
//! https://github.com/phase-rs/phase/issues/4241

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const PUZZLE_BOX_ORACLE: &str = "At the beginning of each player's draw step, \
    that player puts the cards in their hand on the bottom of their library in any order, \
    then draws that many cards.";

fn at_untap(runner: &mut GameRunner, active: PlayerId) {
    let s = runner.state_mut();
    s.turn_number = 2;
    s.phase = Phase::Untap;
    s.active_player = active;
    s.priority_player = active;
    s.waiting_for = WaitingFor::Priority { player: active };
}

fn hand_names(runner: &GameRunner, p: PlayerId) -> Vec<String> {
    let state = runner.state();
    state.players[p.0 as usize]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

#[test]
fn puzzle_box_active_player_cycles_hand_and_draws_that_many() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Teferi's Puzzle Box", 0, 0, PUZZLE_BOX_ORACLE)
        .as_artifact();
    scenario.with_cards_in_hand(P0, &["Hand A", "Hand B", "Hand C"]);
    scenario.with_library_top(
        P0,
        &[
            "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8",
        ],
    );

    let mut runner = scenario.build();
    at_untap(&mut runner, P0);
    runner.advance_to_phase(Phase::Draw);
    runner.advance_until_stack_empty();

    let hand = hand_names(&runner, P0);
    eprintln!("post-draw-step hand = {hand:?}");

    // The whole hand must have been cycled to the bottom of the library and an
    // equal number of fresh cards drawn — no original hand card should remain.
    assert!(
        !hand.iter().any(|n| n.starts_with("Hand ")),
        "original hand cards must be on the bottom after Puzzle Box resolves, got {hand:?}"
    );
}

/// CR 603.2b + CR 102.1: the trigger fires on *each* player's draw step, and
/// "that player" is the active player of that step — not the Box's controller.
/// P0 controls the Box; on P1's draw step, P1's hand is cycled and P1 draws that
/// many, while P0's hand is untouched. This exercises the `ScopedPlayer` routing
/// in `ChangeZoneAll::resolve_all` (the whole-hand move keys off the scoped
/// player, not `ability.controller`).
#[test]
fn puzzle_box_opponent_draw_step_cycles_that_players_hand() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Teferi's Puzzle Box", 0, 0, PUZZLE_BOX_ORACLE)
        .as_artifact();
    scenario.with_cards_in_hand(P1, &["P1 Hand A", "P1 Hand B"]);
    scenario.with_cards_in_hand(P0, &["P0 Keep X", "P0 Keep Y"]);
    scenario.with_library_top(
        P1,
        &[
            "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8",
        ],
    );

    let mut runner = scenario.build();
    // P1 is the active player whose draw step runs.
    at_untap(&mut runner, P1);
    runner.advance_to_phase(Phase::Draw);
    runner.advance_until_stack_empty();

    let p1_hand = hand_names(&runner, P1);
    let p0_hand = hand_names(&runner, P0);

    // P1's original hand was cycled to the bottom; only fresh library cards
    // remain. The non-empty guard proves P1 (not P0) is the player who redrew —
    // an empty P1 hand would pass the `all(...)` check vacuously (card-test
    // foot-gun #6).
    assert!(
        !p1_hand.is_empty(),
        "P1 must have redrawn fresh cards, not been left empty-handed, got {p1_hand:?}"
    );
    assert!(
        !p1_hand.iter().any(|n| n.starts_with("P1 Hand ")),
        "P1's original hand must be on the bottom after Puzzle Box resolves, got {p1_hand:?}"
    );
    assert!(
        p1_hand.iter().all(|n| n.starts_with("Lib ")),
        "P1's post-trigger hand must be freshly drawn library cards, got {p1_hand:?}"
    );

    // P0 controls the Box but is NOT the active player, so P0's hand is untouched.
    assert_eq!(
        p0_hand,
        vec!["P0 Keep X".to_string(), "P0 Keep Y".to_string()],
        "the Box's controller keeps their hand when a different player's draw step fires"
    );
}

/// CR 401.4 + CR 608.2c: an empty starting hand is a legal no-op — the whole-hand
/// move of zero non-turn-based cards plus the CR 504.1 mandatory draw-step draw
/// must not panic. The active player draws exactly one card for the step (the
/// turn-based draw), the trigger cycles that single card and redraws one, so the
/// hand settles at exactly one card.
#[test]
fn puzzle_box_empty_hand_is_legal_noop() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Teferi's Puzzle Box", 0, 0, PUZZLE_BOX_ORACLE)
        .as_artifact();
    // No cards in hand.
    scenario.with_library_top(
        P0,
        &[
            "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8",
        ],
    );

    let mut runner = scenario.build();
    at_untap(&mut runner, P0);
    runner.advance_to_phase(Phase::Draw);
    runner.advance_until_stack_empty();

    let hand = hand_names(&runner, P0);
    assert_eq!(
        hand.len(),
        1,
        "empty-hand active player ends the draw step holding only the turn-based draw, got {hand:?}"
    );
    assert!(
        hand.iter().all(|n| n.starts_with("Lib ")),
        "the surviving card must be a library draw, got {hand:?}"
    );
}

/// CR 608.2c: "draws that many cards" draws a count equal to the hand size the
/// trigger cycled — never `Fixed(1)` and never `0`. Setup: three distinct named
/// hand cards plus the CR 504.1 mandatory draw-step draw (which adds `Lib 1`
/// before the trigger resolves), so the trigger cycles four cards and draws four
/// fresh library cards (`Lib 2`..`Lib 5`). The count therefore tracks hand size,
/// not a hardcoded 1 or 0.
#[test]
fn puzzle_box_draw_count_equals_hand_size() {
    let mut scenario = GameScenario::new();
    scenario
        .add_creature_from_oracle(P0, "Teferi's Puzzle Box", 0, 0, PUZZLE_BOX_ORACLE)
        .as_artifact();
    scenario.with_cards_in_hand(P0, &["Name One", "Name Two", "Name Three"]);
    scenario.with_library_top(
        P0,
        &[
            "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8",
        ],
    );

    let mut runner = scenario.build();
    at_untap(&mut runner, P0);

    // After the draw step's turn-based draw (CR 504.1) but before the trigger
    // resolves, the hand holds the three named cards plus one library card.
    runner.advance_to_phase(Phase::Draw);
    let pre_trigger = hand_names(&runner, P0);
    assert_eq!(
        pre_trigger.len(),
        4,
        "three named + one turn-based draw before the trigger, got {pre_trigger:?}"
    );

    runner.advance_until_stack_empty();
    let hand = hand_names(&runner, P0);

    // The trigger drew a card for every card it cycled: hand size is preserved,
    // every named card is gone, and every surviving card is a fresh library draw.
    assert_eq!(
        hand.len(),
        pre_trigger.len(),
        "the trigger draws exactly as many cards as it cycled (hand size), got {hand:?}"
    );
    assert_eq!(
        hand.len(),
        4,
        "hand-size-driven draw of four (not Fixed(1) or 0), got {hand:?}"
    );
    assert!(
        !hand.iter().any(|n| n.starts_with("Name ")),
        "the named hand cards must have been cycled to the bottom, got {hand:?}"
    );
    assert!(
        hand.iter().all(|n| n.starts_with("Lib ")),
        "every surviving card is a fresh library draw, got {hand:?}"
    );
}
