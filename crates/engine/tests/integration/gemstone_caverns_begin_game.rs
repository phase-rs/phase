//! Regression for GitHub issue #431 — Gemstone Caverns' opening-hand ability
//! silently dropped part of its text.
//!
//! Oracle text:
//!   "If this card is in your opening hand and you're not the starting player,
//!    you may begin the game with Gemstone Caverns on the battlefield with a
//!    luck counter on it. If you do, exile a card from your hand."
//!
//! Bug: the `BeginGame` ability was hardcoded to a bare `Effect::ChangeZone`,
//! dropping both "with a luck counter on it" (CR 122.1) and the entire
//! "If you do, exile a card from your hand" sentence (CR 701.13a). The fix
//! parses the line into a `ChangeZone` with `enter_with_counters` populated and
//! an `IfYouDo`-gated `sub_ability` for the exile.
//!
//! These tests drive the real begin-game / mulligan flow through `apply`:
//!   - accept the opt-in: Gemstone Caverns enters with a luck counter and an
//!     exile prompt is surfaced.
//!   - decline the opt-in: no exile prompt is surfaced (the `IfYouDo` gate
//!     evaluates false).
//!
//! No synthetic events — every step goes through `apply` / the public
//! game-start entry point.

use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::{apply, start_game_with_starting_player};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Build a 2-player game where the non-starting player (P1) has a 7-card
/// library consisting of Gemstone Caverns plus six basic lands. After the
/// opening-hand draw the entire library becomes P1's opening hand regardless of
/// shuffle order, so Gemstone Caverns is guaranteed to be in the opening hand.
///
/// Returns the state with the game started and the mulligan flow active.
fn setup_game(db: &CardDatabase) -> GameState {
    let mut state = GameState::new_two_player(42);

    let gemstone = db
        .get_face_by_name("Gemstone Caverns")
        .expect("Gemstone Caverns must be in the card database");
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must be in the card database");

    // P1 (non-starting player): exactly 7 library cards → full opening hand.
    create_object_from_card_face(&mut state, gemstone, PlayerId(1));
    for _ in 0..6 {
        create_object_from_card_face(&mut state, forest, PlayerId(1));
    }

    // P0 (starting player): a small library so the mulligan flow has cards to
    // draw from. P0 has no begin-game cards.
    for _ in 0..7 {
        create_object_from_card_face(&mut state, forest, PlayerId(0));
    }

    // P0 starts → P1 is the non-starting player, matching Gemstone Caverns'
    // flavor condition.
    let result = start_game_with_starting_player(&mut state, PlayerId(0));
    state.waiting_for = result.waiting_for;
    state
}

/// Drive both players to `Keep` through `apply`, leaving the game at the
/// begin-game opt-in prompt for Gemstone Caverns.
fn keep_both_hands(state: &mut GameState) {
    // Both players keep their opening hands. Mulligan decisions are submitted
    // in seat order; the starting player decides first.
    for &player in &[PlayerId(0), PlayerId(1)] {
        if let WaitingFor::MulliganDecision { .. } = &state.waiting_for {
            let result = apply(
                state,
                player,
                GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                },
            )
            .expect("Keep decision must succeed");
            state.waiting_for = result.waiting_for;
        }
    }
}

/// Locate Gemstone Caverns in P1's hand.
fn gemstone_in_hand(state: &GameState) -> engine::types::identifiers::ObjectId {
    *state.players[1]
        .hand
        .iter()
        .find(|id| state.objects[id].name == "Gemstone Caverns")
        .expect("Gemstone Caverns must be in P1's opening hand")
}

#[test]
fn gemstone_caverns_accept_enters_with_luck_counter_and_prompts_exile() {
    let Some(db) = load_db() else {
        return;
    };
    let mut state = setup_game(db);
    keep_both_hands(&mut state);

    let gemstone_id = gemstone_in_hand(&state);
    // Cards in hand that are NOT Gemstone Caverns — the exile sub-ability draws
    // from these. Gemstone Caverns itself leaves the hand when it enters the
    // battlefield, so it must be excluded from the exile-pool baseline.
    let non_gemstone_in_hand = state.players[1].hand.len() - 1;

    // The begin-game opt-in for Gemstone Caverns must be surfaced to P1.
    let WaitingFor::OptionalEffectChoice { player, .. } = &state.waiting_for else {
        panic!(
            "expected begin-game OptionalEffectChoice prompt, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, PlayerId(1), "the prompt must be for P1");

    // Accept the begin-game opt-in.
    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: true },
    )
    .expect("accepting the begin-game opt-in must succeed");
    state.waiting_for = result.waiting_for;

    // CR 103.6a: Gemstone Caverns is now on the battlefield.
    assert_eq!(
        state.objects[&gemstone_id].zone,
        Zone::Battlefield,
        "Gemstone Caverns must enter the battlefield after accepting",
    );

    // CR 122.1: it entered with exactly one luck counter — without this the
    // {T} ability would only ever tap for {C}.
    let luck = CounterType::Generic("luck".to_string());
    assert_eq!(
        state.objects[&gemstone_id].counters.get(&luck).copied(),
        Some(1),
        "Gemstone Caverns must enter with one luck counter, got counters {:?}",
        state.objects[&gemstone_id].counters,
    );

    // CR 701.13a: the `IfYouDo`-gated sub-ability must surface an exile prompt
    // — the player has not yet chosen which card to exile, so the game is NOT
    // at Priority and no card has left the exile pool yet.
    assert!(
        !matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "after accepting, an exile-a-card prompt must be surfaced, not Priority: {:?}",
        state.waiting_for,
    );
    let non_gemstone_now = state.players[1]
        .hand
        .iter()
        .filter(|id| state.objects[id].name != "Gemstone Caverns")
        .count();
    assert_eq!(
        non_gemstone_now, non_gemstone_in_hand,
        "the exile choice is still pending — no card may leave hand until it resolves",
    );
}

#[test]
fn gemstone_caverns_decline_surfaces_no_exile_prompt() {
    let Some(db) = load_db() else {
        return;
    };
    let mut state = setup_game(db);
    keep_both_hands(&mut state);

    let gemstone_id = gemstone_in_hand(&state);
    let hand_size_before = state.players[1].hand.len();

    let WaitingFor::OptionalEffectChoice { player, .. } = &state.waiting_for else {
        panic!(
            "expected begin-game OptionalEffectChoice prompt, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, PlayerId(1));

    // Decline the begin-game opt-in.
    let result = apply(
        &mut state,
        PlayerId(1),
        GameAction::DecideOptionalEffect { accept: false },
    )
    .expect("declining the begin-game opt-in must succeed");
    state.waiting_for = result.waiting_for;

    // Gemstone Caverns stays in hand — it was never put onto the battlefield.
    assert_eq!(
        state.objects[&gemstone_id].zone,
        Zone::Hand,
        "declining must leave Gemstone Caverns in hand",
    );

    // The `IfYouDo` gate evaluates false: no exile prompt is surfaced and the
    // game proceeds to Priority. The hand is intact — nothing was exiled.
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "declining must surface no exile prompt — the game proceeds to Priority: {:?}",
        state.waiting_for,
    );
    assert_eq!(
        state.players[1].hand.len(),
        hand_size_before,
        "declining must not exile any card from hand",
    );
}
