//! Finneas, Ace Archer: "put a +1/+1 counter on each other creature you control
//! that's a token or a Rabbit. Then if creatures you control have total power 10
//! or greater, draw a card."
//!
//! CR 111.1 + CR 205.3: the relative clause is a disjunction across a token
//! property and a subtype; each leg keeps "other" and "you control".
//! CR 508.3a: the attack trigger fires when Finneas is declared as an attacker.
//! CR 608.2c: the draw checks total power after the counters are placed.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

/// Verbatim Oracle text (MTGJSON `text`).
const FINNEAS: &str = "Reach, vigilance\n\
Whenever Finneas attacks, put a +1/+1 counter on each other creature you control that's a token or a Rabbit. Then if creatures you control have total power 10 or greater, draw a card.";

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn hand_size(runner: &GameRunner) -> usize {
    runner.state().players[0].hand.len()
}

/// The counters land on exactly the other tokens and Rabbits you control; the
/// draw follows the post-counter total power (9 stays below 10, 10 reaches it).
#[test]
fn finneas_counters_tokens_or_rabbits_then_draws_at_ten_power() {
    for (bear_power, draws) in [(1, 0), (2, 1)] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Forest", "Forest"]);
        let finneas = scenario
            .add_creature_from_oracle(P0, "Finneas, Ace Archer", 2, 2, FINNEAS)
            .with_subtypes(vec!["Rabbit", "Archer"])
            .id();
        let token = scenario.add_creature(P0, "Squirrel", 1, 1).id();
        let rabbit = scenario
            .add_creature(P0, "Rabbit Friend", 1, 1)
            .with_subtypes(vec!["Rabbit"])
            .id();
        let rabbit_token = scenario
            .add_creature(P0, "Rabbit Token", 1, 1)
            .with_subtypes(vec!["Rabbit"])
            .id();
        let bear = scenario
            .add_creature(P0, "Bear", bear_power, bear_power)
            .id();
        let opposing_token = scenario.add_creature(P1, "Opposing Squirrel", 1, 1).id();
        let opposing_rabbit = scenario
            .add_creature(P1, "Opposing Rabbit", 1, 1)
            .with_subtypes(vec!["Rabbit"])
            .id();
        let mut runner = scenario.build();
        for id in [token, rabbit_token, opposing_token] {
            runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
        }
        runner.advance_to_combat();
        let hand_before = hand_size(&runner);

        runner
            .declare_attackers(&[(finneas, AttackTarget::Player(P1))])
            .expect("Finneas attacks");
        runner.advance_until_stack_empty();

        assert_eq!(p1p1(&runner, token), 1, "a token you control");
        assert_eq!(p1p1(&runner, rabbit), 1, "a Rabbit you control");
        assert_eq!(
            p1p1(&runner, rabbit_token),
            1,
            "both legs still mean one counter"
        );
        assert_eq!(p1p1(&runner, finneas), 0, "\"other\" excludes Finneas");
        assert_eq!(p1p1(&runner, bear), 0, "neither a token nor a Rabbit");
        assert_eq!(p1p1(&runner, opposing_token), 0, "not yours");
        assert_eq!(p1p1(&runner, opposing_rabbit), 0, "not yours");
        assert_eq!(
            hand_size(&runner) - hand_before,
            draws,
            "total power 2 + 2 + 2 + 2 + {bear_power}"
        );
    }
}
