//! Regression test for GitHub issue #270 — Well of Lost Dreams "may pay {X}"
//! life-gain trigger.
//!
//! Oracle text: "Whenever you gain life, you may pay {X}, where X is less
//! than or equal to the amount of life you gained. If you do, draw X cards."
//!
//! User-reported symptom: after gaining life and clicking "Yes" on the
//! optional `OptionalEffectChoice`, the engine takes one mana but draws no
//! cards and leaves the leftover mana to drain at end of step. The expected
//! flow is:
//!
//!   1. Life-gain event fires the trigger (CR 603.2).
//!   2. Trigger surfaces an `OptionalEffectChoice` to the controller
//!      (CR 603.5 — the "may" optional is chosen as the trigger resolves).
//!   3. After Yes, the engine surfaces a `PayAmountChoice` capped at the
//!      amount of life gained (CR 107.3a + CR 118.1 — X is announced now,
//!      the cap is the trigger-event amount).
//!   4. After submitting amount=N, the engine pays N generic mana and the
//!      `IfYouDo` SequentialSibling sub-ability draws N cards
//!      (CR 608.2c — "If you do" reads the optional-effect-performed
//!      signal from the parent payment).
//!   5. Resolution settles back to `Priority` with no residual mana.
//!
//! The pay.rs unit test
//! `pay_x_optional_may_pay_with_if_you_do_draw_full_chain` exercises the
//! full optional → PayAmount → IfYouDo Draw chain through direct
//! `resolve_ability_chain` calls; this stack-driven test additionally
//! verifies the path through `apply_life_gain` → trigger placement on the
//! stack → resolution via `GameAction`s, the path the user actually drives
//! from the browser UI.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Give P0 `count` colorless mana for paying X.
fn add_colorless_mana(runner: &mut engine::game::scenario::GameRunner, count: u32) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(ManaType::Colorless, dummy, false, vec![]));
    }
}

/// Drive priority/stack passes until either an `OptionalEffectChoice` /
/// `PayAmountChoice` surfaces or the stack settles.
fn advance_until_choice(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 64,
            "exhausted advance budget; final waiting_for = {:?}",
            runner.state().waiting_for
        );
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::PayAmountChoice { .. } => {
                return;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return;
            }
            _ => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass while advancing to a choice must succeed");
            }
        }
    }
}

/// CR 603.2 + CR 603.5 + CR 107.3a + CR 608.2c + CR 121.1: Accepting the
/// optional "you may pay {X}" prompt and submitting amount=3 must pay 3
/// generic mana and draw 3 cards. This is the happy-path stack-driven
/// reproduction for issue #270.
#[test]
fn well_of_lost_dreams_yes_then_pay_three_draws_three() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Well of Lost Dreams", Zone::Battlefield, db);
    // Stock P0's library with at least 3 cards so the Draw{X=3} has cards
    // to take. Padding keeps the library non-empty after the test.
    for _ in 0..10 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }
    // P1 needs a non-empty library so its own SBAs stay inert.
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    // 5 mana available — distinguishes the life-gained cap (3) from the
    // mana cap (5). A regression that collapses max to player-mana would
    // surface as max=5 and a 5-card draw.
    add_colorless_mana(&mut runner, 5);

    let life_before = runner.state().players[0].life;
    let hand_before = runner.state().players[0].hand.len();
    let library_before = runner.state().players[0].library.len();

    // Drive a production-path life-gain through the replacement pipeline.
    // `apply_life_gain` is the engine's single entry point for life gain.
    // CR 603.2 + CR 603.3b: the resulting `LifeChanged` event must then be
    // dispatched to `process_triggers` so the "Whenever you gain life"
    // trigger is collected and put on the stack (this is what `apply()`
    // does automatically inside the event loop).
    let mut events = Vec::new();
    let gained =
        engine::game::effects::life::apply_life_gain(runner.state_mut(), P0, 3, &mut events)
            .expect("life gain must resolve without deferring");
    assert_eq!(gained, 3, "no replacements should modify the gain");
    assert_eq!(
        runner.state().players[0].life,
        life_before + 3,
        "P0 must actually gain 3 life before the trigger fires"
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);

    // Advance the stack — the LifeGained trigger must surface its
    // OptionalEffectChoice.
    advance_until_choice(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                *player, P0,
                "CR 603.5: 'you may pay {{X}}' must prompt the trigger's controller"
            );
        }
        other => {
            panic!("expected OptionalEffectChoice from Well of Lost Dreams trigger, got {other:?}")
        }
    }

    // Click "Yes". The engine should transition to PayAmountChoice with
    // max=3 (capped by life-gained=3, NOT mana=5).
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accepting the optional 'may pay' prompt must succeed");
    advance_until_choice(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::PayAmountChoice {
            player, max, min, ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(
                *max, 3,
                "PayAmountChoice max must be capped by life gained (3), got {max} \
                 — regression in trigger-event amount cap"
            );
            assert_eq!(*min, 0, "X may be 0 (CR 107.3a)");
        }
        other => {
            panic!("after Yes, expected PayAmountChoice (CR 107.3a — announce X), got {other:?}")
        }
    }

    // Submit amount=3 — the full life-gained cap.
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("submitting X=3 within the legal [0,3] range must succeed");
    runner.advance_until_stack_empty();

    // CR 608.2c + CR 121.1: 3 cards drawn (the IfYouDo Draw{X=3} fired).
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "the IfYouDo Draw{{X=3}} sub-ability must draw 3 cards after submitting amount=3"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before - 3,
        "exactly 3 cards must move from library to hand"
    );

    // CR 118.1: 3 of 5 mana spent. A regression that paid {1} instead of
    // {X=3} would leave 4 mana here; a regression that paid all 5 mana
    // would leave 0.
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        2,
        "3 of 5 generic mana must be spent on the X cost"
    );
}

/// CR 603.5 + CR 608.2c: Declining the optional prompt skips both the
/// mana payment AND the IfYouDo Draw. No residue.
#[test]
fn well_of_lost_dreams_no_does_nothing() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Well of Lost Dreams", Zone::Battlefield, db);
    for _ in 0..10 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
    }
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    add_colorless_mana(&mut runner, 5);

    let hand_before = runner.state().players[0].hand.len();

    let mut events = Vec::new();
    engine::game::effects::life::apply_life_gain(runner.state_mut(), P0, 3, &mut events)
        .expect("life gain must resolve");
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    advance_until_choice(&mut runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "expected OptionalEffectChoice prompt; got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("declining the optional prompt must succeed");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "declining must not draw any cards (IfYouDo gate evaluates false)"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        5,
        "declining must not spend any mana"
    );
}
