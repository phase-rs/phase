//! Nykthos Paragon: "Whenever you gain life, you may put that many +1/+1
//! counters on each creature you control. Do this only once each turn."
//!
//! Regression for the "put that many ... counters" route change made for
//! #705: the clause is a mass placement on every creature its controller
//! controls (not one chosen creature), it is optional, and the once-per-turn
//! limit is spent only when the player takes the indicated action.

use crate::support::shared_card_db;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

#[test]
fn nykthos_paragon_counts_only_the_life_gain_it_accepts() {
    let db = shared_card_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]); 4],
    );
    scenario.with_library_top(P0, &["Plains"]);
    let paragon = scenario
        .add_creature_from_oracle(
            P0,
            "Nykthos Paragon",
            4,
            6,
            "Whenever you gain life, you may put that many +1/+1 counters on each creature you control. Do this only once each turn.",
        )
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let ally = scenario.add_real_card(P0, "Grizzly Bears", Zone::Battlefield, db);
    let opponent = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let first = scenario.add_real_card(P0, "Chaplain's Blessing", Zone::Hand, db);
    let second = scenario.add_real_card(P0, "Revitalize", Zone::Hand, db);
    let third = scenario.add_real_card(P0, "Chaplain's Blessing", Zone::Hand, db);
    let mut runner = scenario.build();

    for (spell, gained, accept, expected) in [(first, 5, false, 0), (second, 3, true, 3)] {
        let outcome = runner.cast(spell).commit().stop_at_optional().resolve();
        outcome.assert_life_delta(P0, gained);
        // CR 603.5: the controller chooses as the optional trigger resolves.
        assert!(
            matches!(
                outcome.final_waiting_for(),
                WaitingFor::OptionalEffectChoice { player: P0, source_id, .. }
                    if *source_id == paragon
            ),
            "expected P0's Paragon choice, got {:?}",
            outcome.final_waiting_for()
        );
        runner
            .act(GameAction::DecideOptionalEffect { accept })
            .unwrap();
        // CR 608.2c: accepting uses the amount from this occurrence for each creature P0 controls.
        assert_counters(
            &runner,
            [(paragon, expected), (ally, expected), (opponent, 0)],
        );
    }

    let life_before = runner.life(P0);
    runner.cast(third).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    assert_eq!(runner.life(P0) - life_before, 5);
    // CR 603.2h: once P0 takes the indicated action, the ability stops triggering this turn.
    assert_counters(&runner, [(paragon, 3), (ally, 3), (opponent, 0)]);
}

fn assert_counters(runner: &GameRunner, expected: [(ObjectId, u32); 3]) {
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for (creature, count) in expected {
        assert_eq!(
            runner.state().objects[&creature]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            count,
            "counter count on {creature:?}"
        );
    }
}
