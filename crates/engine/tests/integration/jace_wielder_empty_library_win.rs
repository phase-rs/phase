//! Runtime regression: the "draw from empty library → win" replacement class
//! (Laboratory Maniac, Jace, Wielder of Mysteries).
//!
//! Oracle text (static replacement):
//!   "If you would draw a card while your library has no cards in it, you win
//!    the game instead."
//!
//! Reported bug: a 4-player Commander game ended with the active player winning
//! out of nowhere — every opponent (including a player at 32 life, non-empty
//! library, no commander damage) was eliminated the moment a card was played.
//!
//! Root cause: the parser dropped the "while your library has no cards in it"
//! antecedent, so the replacement was stored with `condition: null`. That makes
//! the replacement match on *every* draw. On a non-empty draw it stashed a
//! `WinTheGame` post-replacement continuation that was never drained (the draw
//! proceeded normally), leaking the continuation into a later turn where it
//! drained against the active player — eliminating all of *their* opponents.
//!
//! The fix gates the replacement on `ZoneCardCount(Library) == 0`, so a
//! non-empty draw no longer matches, stashes, or leaks.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Jace, Wielder of Mysteries on P0's battlefield. `p0_library` cards are added
/// to P0's library; P1 always gets a non-empty library so its own SBAs are inert.
fn scenario_with_jace(
    db: &CardDatabase,
    p0_library: &[&str],
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Jace, Wielder of Mysteries", Zone::Battlefield, db);
    for name in p0_library {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;
    runner
}

/// CR 104.2b + CR 104.3c: The "while your library has no cards in it" antecedent
/// gates the replacement. Drawing from a NON-empty library must not win the game
/// — and must not stash a post-replacement `WinTheGame` continuation that would
/// leak into a later turn (the reported bug).
#[test]
fn jace_draw_from_nonempty_library_does_not_win_and_does_not_leak() {
    let Some(db) = load_db() else {
        return;
    };

    let mut runner = scenario_with_jace(db, &["Plains", "Island", "Forest"]);
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "precondition: game must not already be over"
    );

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "drawing from a 3-card library must NOT win the game. waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().players[1].is_eliminated,
        "opponent must not be eliminated by a normal draw"
    );
    // The load-bearing leak assertion: a non-empty draw must not stash a
    // post-replacement continuation. A leaked `WinTheGame` continuation is what
    // drained against the wrong player on a later turn in the bug report.
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "a non-empty draw must NOT leak a stashed post-replacement continuation; \
         found {:?}",
        runner.state().post_replacement_continuation()
    );
}

/// CR 614.6 + CR 614.11 + CR 704.3: With an empty library, the replacement fires,
/// the original draw is fully replaced (CR 614.6) so the
/// `drew_from_empty_library` SBA never trips (CR 704.5b), and the substituted
/// `WinTheGame` continuation drains in the same resolution step (CR 704.3) so
/// the controller wins before priority is offered to anyone.
#[test]
fn jace_draw_from_empty_library_wins() {
    let Some(db) = load_db() else {
        return;
    };

    let mut runner = scenario_with_jace(db, &[]);

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "drawing from an empty library must win the game for Jace's controller. \
         waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().players[1].is_eliminated,
        "opponent must be eliminated when P0 wins"
    );
    // CR 614.6: the original draw never happens, so the empty-library SBA
    // (CR 704.5b) must NOT trip on the substituted draw.
    assert!(
        !runner.state().players[0].drew_from_empty_library,
        "the replaced draw never happens (CR 614.6); empty-library flag must stay false"
    );
    // CR 704.3: the continuation must drain in the same resolution step — no
    // leak into the next priority pass.
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "post_replacement_continuation must drain in the same step; found {:?}",
        runner.state().post_replacement_continuation()
    );
}

/// CR 614.6 + CR 614.11 + CR 704.3: Laboratory Maniac is the original printing
/// of this class — same replacement template, different card. Pinning a sibling
/// test ensures the fix is at the *class* level (any "if you would draw a card
/// while your library has no cards in it, you win the game instead" replacement)
/// and not card-specific to Jace.
#[test]
fn laboratory_maniac_draw_from_empty_library_wins() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Laboratory Maniac", Zone::Battlefield, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw must succeed");
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "drawing from an empty library must win the game for Laboratory Maniac's controller. \
         waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        !runner.state().players[0].drew_from_empty_library,
        "the replaced draw never happens (CR 614.6); empty-library flag must stay false"
    );
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "post_replacement_continuation must drain in the same step; found {:?}",
        runner.state().post_replacement_continuation()
    );
}

/// CR 504.1 + CR 614.6 + CR 614.11 + CR 704.3: The user-reported scenario was a
/// 4-player Commander game where the *natural* draw-step draw ended the game
/// spuriously. The runtime path for that draw is `turns.rs::execute_draw`, NOT
/// `effects::draw::resolve` or `DebugAction::DrawCards`. This test drives the
/// engine from `Phase::Untap` through `auto_advance_to_main_phase`, which
/// invokes `execute_draw` during the Draw step. With an empty library and Jace
/// on the battlefield, the pre-zero + drain must make P0 win — and the
/// `WinTheGame` continuation must NOT leak past the draw step.
#[test]
fn jace_natural_draw_step_from_empty_library_wins() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_real_card(P0, "Jace, Wielder of Mysteries", Zone::Battlefield, db);
    // P0 library intentionally empty — that's the precondition under test.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    // Drive the natural turn flow: Untap → Upkeep → Draw → PreCombatMain.
    // `execute_draw` runs during the Draw step; the empty-library replacement
    // must fire, pre-zero the draw, and drain the WinTheGame continuation
    // before priority is offered.
    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "natural draw step from an empty library must win the game for Jace's \
         controller. waiting_for={:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().players[1].is_eliminated,
        "opponent must be eliminated when P0 wins via the natural draw step"
    );
    assert!(
        !runner.state().players[0].drew_from_empty_library,
        "the replaced draw never happens (CR 614.6); empty-library flag must stay false"
    );
    assert!(
        runner.state().post_replacement_continuation().is_none(),
        "post_replacement_continuation must drain in the same step; found {:?}",
        runner.state().post_replacement_continuation()
    );
}
