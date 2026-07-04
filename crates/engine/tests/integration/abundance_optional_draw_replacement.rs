//! Runtime pipeline regression for issue #516 — Abundance.
//!
//! Abundance's Oracle text: "If you would draw a card, you may instead choose
//! land or nonland and reveal cards from the top of your library until you
//! reveal a card of the chosen kind. Put that card into your hand and put all
//! other cards revealed this way on the bottom of your library in any order."
//!
//! Before the fix, the parser:
//!   1. Did not split "choose land or nonland and reveal cards ..." at the
//!      bare " and ", so the whole clause fell to `Effect::Unimplemented`.
//!   2. Did not lift "you may instead" to `ReplacementMode::Optional`, so the
//!      replacement always fired without a player prompt.
//!   3. Did not route the "of the chosen kind" filter into
//!      `Effect::RevealUntil`'s filter dispatch.
//!
//! The fix is purely parser-composition — every runtime primitive
//! (`ReplacementMode::Optional`, `Effect::Choose(Labeled)`,
//! `Effect::RevealUntil`, `FilterProp::IsChosenLandOrNonlandKind`,
//! random-order bottom placement via `shuffle_to_bottom`) already exists.
//!
//! These tests drive the real engine pipeline through `GameAction`s:
//! prompt the draw via `DebugAction::DrawCards`, accept the optional
//! replacement, pick "Land" / "Nonland", and assert the resulting hand /
//! library composition. The +1-vs-+2 hand-size discriminator is the load-bearing
//! assertion (CR 614.6: only the accept branch replaces the event — synthesizing
//! a synthetic draw on decline would double-draw on accept).

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Set up a scenario with Abundance on `P0`'s battlefield and a deterministic
/// library: top → `top` (the user-chosen-kind cards are placed in this order).
/// Library order is `lib_top_to_bottom[0]` on top of the library.
fn scenario_with_abundance_and_library(
    db: &CardDatabase,
    lib_top_to_bottom: &[&str],
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Abundance", Zone::Battlefield, db);
    // Use `add_real_card` for library cards so they get full card data
    // (core types, oracle text, etc.). `add_card_to_library_top` creates
    // anonymous objects with no types — which would make the
    // `IsChosenLandOrNonlandKind` filter reject every Land as a non-Land.
    // `add_real_card` `push_back`s onto the library; the engine treats
    // `library.front()` (position 0) as the top (see `cascade.rs:60` /
    // `casting.rs:1151`), so the first element pushed ends up on top.
    for name in lib_top_to_bottom.iter() {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    // P1 needs *some* library so SBAs don't fire.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;
    runner
}

fn issue_single_draw(runner: &mut engine::game::scenario::GameRunner) {
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
}

fn assert_named_choice(runner: &engine::game::scenario::GameRunner, expected_options: &[&str]) {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice { options, .. } => {
            for opt in expected_options {
                assert!(
                    options.iter().any(|o| o == opt),
                    "expected NamedChoice options to include {opt}, got {options:?}"
                );
            }
        }
        other => panic!("expected NamedChoice, got {:?}", other),
    }
}

fn hand_card_names(state: &engine::types::game_state::GameState, player: PlayerId) -> Vec<String> {
    state.players[player.0 as usize]
        .hand
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

fn library_card_names(
    state: &engine::types::game_state::GameState,
    player: PlayerId,
) -> Vec<String> {
    state.players[player.0 as usize]
        .library
        .iter()
        .filter_map(|id| state.objects.get(id).map(|o| o.name.clone()))
        .collect()
}

/// CR 614.1a + CR 614.6 + CR 614.11 + CR 701.20a: Accepting the optional
/// "you may instead" replacement and choosing "Land" reveals cards from the
/// top of the library until the first land is found. That land enters the
/// hand; the non-land cards revealed before it go to the bottom of the
/// library in a random order. Hand size increases by exactly 1 — the
/// discriminating signal that the original draw event was replaced, NOT
/// supplemented (the load-bearing +1-vs-+2 check).
#[test]
fn abundance_accept_choose_land_puts_first_land_into_hand() {
    let Some(db) = load_db() else {
        return;
    };

    // Library top → bottom: Bear (nonland), Hill Giant (nonland), Forest (land),
    // Plains (land), Mountain (land). Choosing "Land" must reveal Bear and
    // Hill Giant (misses), then Forest (hit) — Forest enters hand, Bear and
    // Hill Giant go to the library bottom in random order.
    let mut runner = scenario_with_abundance_and_library(
        db,
        &[
            "Grizzly Bears",
            "Hill Giant",
            "Forest",
            "Plains",
            "Mountain",
        ],
    );

    let hand_before = runner.state().players[0].hand.len();
    let library_before = runner.state().players[0].library.len();

    issue_single_draw(&mut runner);

    // Pause: optional replacement prompt.
    let WaitingFor::ReplacementChoice { .. } = runner.state().waiting_for else {
        panic!(
            "expected ReplacementChoice (Abundance is optional), got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Abundance's optional replacement");

    // Pause: NamedChoice over ["Land", "Nonland"].
    assert_named_choice(&runner, &["Land", "Nonland"]);
    runner
        .act(GameAction::ChooseOption {
            choice: "Land".to_string(),
        })
        .expect("choose Land");

    // Drive any follow-up resolution (the chain may need a no-op tick).
    runner.advance_until_stack_empty();

    let hand_after_names = hand_card_names(runner.state(), P0);
    let library_after_names = library_card_names(runner.state(), P0);
    let hand_after = hand_after_names.len();
    let library_after = library_after_names.len();

    // ── Discriminating assertion: +1 hand, not +2. ─────────────────────────
    // If "you may instead" wired up `decline: Some(draw)` instead of `None`,
    // the accept branch would resolve the chain AND fall through to draw,
    // yielding +2. The CR-correct behaviour (CR 614.6 — the draw is fully
    // replaced) requires +1.
    assert_eq!(
        hand_after,
        hand_before + 1,
        "Abundance accept-and-choose-Land must yield exactly +1 hand card \
         (the kept land), not +2 — the original draw is fully replaced. \
         hand_before={hand_before}, hand_after_names={hand_after_names:?}"
    );

    // The kept card must be Forest (first land from the top).
    assert!(
        hand_after_names.contains(&"Forest".to_string()),
        "Forest must be in hand; got {hand_after_names:?}"
    );

    // Forest moved to hand; library loses exactly 1. The two non-land misses
    // (Grizzly Bears, Hill Giant) are relocated from the top to the bottom of
    // the library in a random order.
    assert_eq!(
        library_after,
        library_before - 1,
        "library loses exactly 1 card (Forest moved to hand); the revealed misses \
         are relocated within the library, not removed"
    );
    let bottom_two: Vec<_> = library_after_names.iter().rev().take(2).cloned().collect();
    assert!(
        bottom_two.contains(&"Grizzly Bears".to_string())
            && bottom_two.contains(&"Hill Giant".to_string()),
        "Grizzly Bears and Hill Giant must be at the bottom of the library \
         (in random order — the two non-land misses revealed before Forest); \
         got bottom_two={bottom_two:?}, library={library_after_names:?}"
    );
}

/// CR 614.6: Declining the optional replacement falls back to the original
/// draw event, which proceeds unmodified. The player draws exactly one
/// card — the top of the library — and no cards are revealed or moved to
/// the bottom. The +1-with-no-shuffle signature distinguishes this from
/// the accept branch (which would also be +1, but with the rest pile
/// rearranged).
#[test]
fn abundance_decline_falls_through_to_normal_draw() {
    let Some(db) = load_db() else {
        return;
    };

    let mut runner =
        scenario_with_abundance_and_library(db, &["Grizzly Bears", "Forest", "Plains", "Mountain"]);

    let hand_before = runner.state().players[0].hand.len();
    let library_before_names = library_card_names(runner.state(), P0);

    issue_single_draw(&mut runner);

    // Find the Decline option among the candidates.
    let WaitingFor::ReplacementChoice { candidates, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected ReplacementChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    let descriptions: Vec<&str> = candidates.iter().map(|c| c.description.as_str()).collect();
    assert_eq!(
        descriptions,
        vec!["Accept", "Decline"],
        "Abundance optional replacement must surface exactly Accept/Decline"
    );
    let decline_idx = 1;
    runner
        .act(GameAction::ChooseReplacement { index: decline_idx })
        .expect("decline Abundance's optional replacement");

    runner.advance_until_stack_empty();

    let hand_after_names = hand_card_names(runner.state(), P0);
    let library_after_names = library_card_names(runner.state(), P0);

    // The original draw proceeds: top library card enters hand, nothing else
    // moves. +1 hand, library loses 1 (the top card), library tail is
    // unchanged (no shuffle).
    assert_eq!(
        hand_after_names.len(),
        hand_before + 1,
        "decline must result in exactly +1 hand card (the natural draw); got {hand_after_names:?}"
    );
    assert!(
        hand_after_names.contains(&"Grizzly Bears".to_string()),
        "top library card (Grizzly Bears) must be drawn on decline; got {hand_after_names:?}"
    );
    assert_eq!(
        library_after_names,
        library_before_names[1..].to_vec(),
        "decline must NOT shuffle the library — the rest of the deck stays in original order"
    );
}

/// CR 614.1a: Abundance replaces only its controller's draws. When the active
/// player is an opponent, that opponent's draw must not surface Abundance's
/// optional replacement prompt.
#[test]
fn abundance_does_not_replace_opponent_draw_during_opponents_turn() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Abundance", Zone::Battlefield, db);
    for name in ["Grizzly Bears", "Forest", "Plains"] {
        scenario.add_real_card(P1, name, Zone::Library, db);
    }
    for _ in 0..5 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;

    let hand_before = runner.state().players[1].hand.len();
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P1,
            count: 1,
        }))
        .expect("debug draw for opponent must succeed");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "P0's Abundance must not replace P1's draw during P1's turn; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        hand_card_names(runner.state(), P1).len(),
        hand_before + 1,
        "opponent draw must proceed normally without Abundance replacement"
    );
}
