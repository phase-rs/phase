//! Hercules, Olympian Hero and Venus, Torn Between Worlds put "that many"
//! +1/+1 counters on themselves ("on him" / "on her") when dealt damage.
//!
//! Regression for the "put that many ... counters" route change made for
//! #705: the gendered pronoun is the card itself, so the counters land on it
//! after nonlethal damage.

use crate::support::shared_card_db;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn damage_scenario() -> (GameScenario, [ObjectId; 2]) {
    let db = shared_card_db().expect("integration fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); 2],
    );
    let first = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    let second = scenario.add_real_card(P0, "Shock", Zone::Hand, db);
    (scenario, [first, second])
}

#[test]
fn hercules_gets_counters_only_for_the_first_damage_this_turn() {
    let (mut scenario, [first, second]) = damage_scenario();
    let hercules = scenario
        .add_creature_from_oracle(
            P0,
            "Hercules, Olympian Hero",
            3,
            3,
            "Whenever Hercules attacks, put a +1/+1 counter on him. He gains indestructible until end of turn.\nWhenever Hercules is dealt damage for the first time each turn, put that many +1/+1 counters on him. (He must survive the damage to get the counters.)",
        )
        .as_legendary()
        .with_subtypes(vec!["Demigod", "Warrior", "Hero"])
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(first).target_object(hercules).resolve();
    // CR 120.3e + CR 608.2c: the creature takes two damage, and the trigger uses that amount.
    assert_eq!(outcome.damage_marked(hercules), 2);
    assert_eq!(outcome.counters(hercules, CounterType::Plus1Plus1), 2);
    assert_eq!(outcome.stack_size(), 0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));

    runner.cast(second).target_object(hercules).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    let creature = &runner.state().objects[&hercules];
    assert_eq!(creature.zone, Zone::Battlefield);
    assert_eq!(creature.damage_marked, 4);
    assert_eq!(creature.counters.get(&CounterType::Plus1Plus1), Some(&2));
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn venus_gets_counters_for_each_damage_occurrence() {
    let (mut scenario, shocks) = damage_scenario();
    let venus = scenario
        .add_creature_from_oracle(
            P0,
            "Venus, Torn Between Worlds",
            5,
            5,
            "Whenever Venus is dealt damage, put that many +1/+1 counters on her. (She must survive the damage to get the counters.)\nWhenever a creature you control with a counter on it deals combat damage to a player, you may pay {U}. If you do, draw a card.",
        )
        .as_legendary()
        .with_subtypes(vec!["Mutant", "Frog", "Turtle"])
        .id();
    let mut runner = scenario.build();
    for (shock, total) in shocks.into_iter().zip([2, 4]) {
        let outcome = runner.cast(shock).target_object(venus).resolve();
        // CR 120.3e + CR 608.2c: each two-damage occurrence adds two counters.
        assert_eq!(outcome.damage_marked(venus), total);
        assert_eq!(outcome.counters(venus, CounterType::Plus1Plus1), total);
        assert_eq!(outcome.stack_size(), 0);
        assert!(matches!(
            outcome.final_waiting_for(),
            WaitingFor::Priority { .. }
        ));
    }
}
