//! Fix for GitHub issue #7153 — The Eternal Wanderer's +1 ability must delay
//! the return to the beginning of the exiled permanent's OWNER's next end
//! step, not immediately "blink" the card back.
//!
//! Oracle text (verified against the live Scryfall API, 2026-08-28):
//!   "+1: Exile up to one target artifact or creature. Return that card to
//!   the battlefield under its owner's control at the beginning of that
//!   player's next end step."
//!
//! Reported bug: the engine previously modeled this as an immediate
//! exile-then-return (a "blink"), dropping the delayed-trigger wrapper
//! entirely. This is a **discriminator** test: it drives the real +1
//! activation through the real `apply()` pipeline and checks, in order:
//!   (a) the targeted creature is exiled;
//!   (b) a delayed trigger is installed (not an immediate return);
//!   (c) the delayed trigger does NOT fire during the ability controller's
//!       (P0's) own end step this turn — "that player" binds to the exiled
//!       card's OWNER (P1), not the ability's controller;
//!   (d) the card returns to the battlefield under its owner's (P1's)
//!       control at P1's next end step.
//!
//! Mutation-tested: reverting the delayed-trigger-suffix fix collapses the
//! +1 back to an immediate `ChangeZone -> ChangeZone` chain, which fails
//! checkpoint (b) (no delayed trigger installed) and (c) is vacuous because
//! the card is already back on the battlefield before the same-turn check.
//! Reverting only the owner-binding fix (leaving the sentinel resolved to
//! `ability.controller`) fails checkpoint (c): the card would return at
//! P0's own end step instead of staying exiled until P1's.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

#[test]
fn eternal_wanderer_plus_one_returns_at_exiled_owners_next_end_step() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wanderer = scenario.add_real_card(P0, "The Eternal Wanderer", Zone::Battlefield, db);
    // Owned AND controlled by P1 (a different player than the Wanderer's
    // controller P0) so the misparse's "your end step" (P0's) is
    // distinguishable from the correct "that player's" (the exiled card's
    // owner, P1) binding.
    let victim = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);

    // Pad both libraries so neither player decks out while priority is
    // passed through a full turn cycle to reach P1's end step.
    for _ in 0..30 {
        scenario.add_real_card(P0, "Plains", Zone::Library, db);
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    // CR 306.5b: seed the planeswalker's displayed loyalty and its loyalty
    // counter count together for this pre-existing battlefield fixture.
    {
        let obj = runner
            .state_mut()
            .objects
            .get_mut(&wanderer)
            .expect("The Eternal Wanderer remains on the battlefield");
        obj.loyalty = Some(5);
        obj.counters.insert(CounterType::Loyalty, 5);
    }

    let plus_one_index = runner.state().objects[&wanderer]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.cost.as_ref(),
                Some(AbilityCost::Loyalty { amount: 1 })
            )
        })
        .expect("The Eternal Wanderer must expose its +1 loyalty ability");

    runner
        .act(GameAction::ActivateAbility {
            source_id: wanderer,
            ability_index: plus_one_index,
        })
        .expect("+1 activation must be accepted");

    // Answer the "up to one target artifact or creature" slot by choosing
    // P1's creature (not declining).
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(victim)),
                    })
                    .expect("target selection for +1 must accept the chosen creature");
            }
            _ => break,
        }
    }

    runner.advance_until_stack_empty();

    // Checkpoint (a): the targeted creature is exiled.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Exile,
        "checkpoint (a): +1 must exile the targeted permanent"
    );

    // Checkpoint (b): a delayed trigger is installed — the reported bug
    // resolved this as an immediate blink with no delayed trigger at all.
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "checkpoint (b): the delayed return trigger must be installed instead \
         of returning the card immediately"
    );
    assert_eq!(
        runner.state().delayed_triggers[0].ability.targets,
        vec![TargetRef::Object(victim)],
        "checkpoint (b): the delayed trigger must snapshot the exiled victim"
    );

    // Pass priority through the remainder of the game, watching for P0's own
    // end step to confirm checkpoint (c) before the trigger is allowed to
    // fire on P1's end step.
    let mut checked_same_turn_end_step = false;
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() {
        guard += 1;
        assert!(
            guard < 512,
            "the owner-scoped delayed return never fired at P1's end step; \
             phase = {:?}, active_player = {:?}, dt = {}",
            runner.state().phase,
            runner.state().active_player,
            runner.state().delayed_triggers.len(),
        );

        // Checkpoint (c): while still in P0's own end step this turn, the
        // delayed trigger must NOT have fired — "that player" is the exiled
        // card's owner (P1), not the ability's controller (P0).
        if !checked_same_turn_end_step
            && runner.state().phase == Phase::End
            && runner.state().active_player == P0
        {
            assert_eq!(
                runner.state().objects[&victim].zone,
                Zone::Exile,
                "checkpoint (c): must NOT return at P0's own end step — 'that \
                 player' binds to the exiled card's owner (P1), not the \
                 ability's controller (P0)"
            );
            assert_eq!(
                runner.state().delayed_triggers.len(),
                1,
                "checkpoint (c): the delayed trigger must still be pending \
                 after P0's own end step"
            );
            checked_same_turn_end_step = true;
        }

        let _ = runner.act(GameAction::PassPriority);
        runner.advance_until_stack_empty();
    }

    assert!(
        checked_same_turn_end_step,
        "test setup error: never observed P0's own end step before the \
         delayed trigger fired — checkpoint (c) was not exercised"
    );

    // Checkpoint (d): the card is back on the battlefield under its OWNER's
    // (P1's) control, and the one-shot delayed trigger is consumed.
    assert_eq!(
        runner.state().objects[&victim].zone,
        Zone::Battlefield,
        "checkpoint (d): the exiled card must return to the battlefield at \
         the owner's next end step"
    );
    assert_eq!(
        runner.state().objects[&victim].controller,
        P1,
        "checkpoint (d): the returned card must be under its owner's control"
    );
    assert!(
        runner.state().delayed_triggers.is_empty(),
        "checkpoint (d): the one-shot delayed trigger must be consumed after \
         firing"
    );
}
