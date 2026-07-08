//! Regression test for GitHub issue #787 — Once Upon a Time.
//!
//! "If this spell is the first spell you've cast this game, you may cast it
//! without paying its mana cost. Look at the top five cards of your library.
//! You may reveal a creature or land card from among them and put it into your
//! hand. Put the rest on the bottom of your library in a random order."
//!
//! Reported behavior: "I haven't cast a spell yet this game and it's not
//! castable for free as it should be." Once Upon a Time's defining feature is
//! the `CastWithoutManaCost` alternative cost (CR 601.2b/118.9), gated by the
//! `FirstSpellThisGame` condition (`spells_cast_this_game[player] == 0`).
//!
//! This drives the real database-parsed card end to end: with no mana and as
//! the first spell of the game it must be castable for free, the cast must
//! offer the no-cost-vs-printed choice, and the window must close once any
//! spell has already been cast this game.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, AdditionalCost};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn once_upon_a_time_free_as_first_spell_of_game() {
    let Some(db) = load_db() else { return };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let once = scenario.add_real_card(P0, "Once Upon a Time", Zone::Hand, db);
    // A second copy proves the free-cast window closes through the *real* cast
    // pipeline (not just a hand-mutated counter) once the first is cast.
    let once_again = scenario.add_real_card(P0, "Once Upon a Time", Zone::Hand, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // Precondition: no spell cast yet, and (no lands added) no mana available.
    assert_eq!(
        runner
            .state()
            .spells_cast_this_game
            .get(&P0)
            .copied()
            .unwrap_or(0),
        0,
        "no spell has been cast this game"
    );

    // First spell of the game with no mana: the free-cast alternative must make
    // it castable. This is the exact symptom reported in #787.
    assert!(
        can_cast_object_now(runner.state(), P0, once),
        "Once Upon a Time must be castable for free as the first spell of the game"
    );

    // The window closes once any spell has been cast this game — with no mana it
    // is then uncastable. Discriminates the free-cast gate from an unconditional
    // free cast.
    runner.state_mut().spells_cast_this_game.insert(P0, 1);
    assert!(
        !can_cast_object_now(runner.state(), P0, once),
        "the free-cast window must close after a spell has been cast this game"
    );
    runner.state_mut().spells_cast_this_game.insert(P0, 0);

    // Drive the cast: with no mana the engine offers the no-cost alternative
    // against the (unaffordable) printed cost.
    let card_id = runner.state().objects[&once].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: once,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the first spell of the game should begin");
    assert!(
        matches!(
            result.waiting_for,
            WaitingFor::OptionalCostChoice {
                cost: AdditionalCost::Choice(
                    AbilityCost::Mana {
                        cost: ManaCost::NoCost
                    },
                    _
                ),
                ..
            }
        ),
        "expected a free-vs-printed cost choice, got {:?}",
        result.waiting_for
    );

    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("choosing the free alternative should put the spell on the stack");

    assert_eq!(
        runner.state().stack.len(),
        1,
        "the spell should be on the stack after the free cast"
    );
    assert!(
        matches!(runner.state().stack[0].kind, StackEntryKind::Spell { .. }),
        "the stack entry should be the cast spell"
    );

    // The real cast pipeline must record the spell so the first-spell gate
    // actually closes — not just the hand-mutated counter checked above.
    assert_eq!(
        runner
            .state()
            .spells_cast_this_game
            .get(&P0)
            .copied()
            .unwrap_or(0),
        1,
        "casting Once Upon a Time must increment spells_cast_this_game via the real pipeline"
    );

    // A second copy is no longer the first spell of the game, so with no mana
    // it is uncastable — proving the window closed through the actual cast.
    assert!(
        !can_cast_object_now(runner.state(), P0, once_again),
        "a second Once Upon a Time must not be free-castable after the first was cast"
    );
}
