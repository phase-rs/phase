//! Issue #4836: The Mindskinner prevents combat damage to opponents but must
//! also mill that many cards for each opponent.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;

const MINDSKINNER_ORACLE: &str = "The Mindskinner can't be blocked.\n\
If a source you control would deal damage to an opponent, prevent that damage and each opponent mills that many cards.";

fn resolve_combat(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.waiting_for_kind() {
            "DeclareBlockers" => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare blockers");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
        if runner.state().phase == Phase::PostCombatMain && runner.state().stack.is_empty() {
            break;
        }
    }
    runner.advance_until_stack_empty();
}

#[test]
fn mindskinner_parses_prevention_with_mill_followup() {
    let mut scenario = GameScenario::new();
    let mindskinner = scenario
        .add_creature_from_oracle(P0, "The Mindskinner", 10, 1, MINDSKINNER_ORACLE)
        .id();
    let runner = scenario.build();
    let repl = runner
        .state()
        .objects
        .get(&mindskinner)
        .expect("Mindskinner on battlefield")
        .replacement_definitions
        .iter_unchecked()
        .find(|r| r.event == ReplacementEvent::DamageDone)
        .expect("damage prevention replacement");
    assert!(
        repl.execute.is_some(),
        "Mindskinner must carry a mill follow-up on its prevention replacement"
    );
}

#[test]
fn mindskinner_attack_mills_opponent_for_prevented_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for i in 0..15 {
        scenario.with_library_top(P1, &[&format!("Opp Library {i}")]);
    }
    let mindskinner = scenario
        .add_creature_from_oracle(P0, "The Mindskinner", 10, 1, MINDSKINNER_ORACLE)
        .id();
    let mut runner = scenario.build();

    let opp_library_before = runner.state().players[1].library.len();
    let opp_life_before = runner.life(P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(mindskinner, AttackTarget::Player(P1))])
        .expect("declare Mindskinner attacking P1");
    resolve_combat(&mut runner);

    assert_eq!(
        runner.life(P1),
        opp_life_before,
        "combat damage to the opponent must be prevented"
    );
    assert_eq!(
        runner.state().players[1].library.len(),
        opp_library_before - 10,
        "opponent must mill 10 cards (Mindskinner's power) when damage is prevented"
    );
    assert!(
        runner.state().objects[&mindskinner].tapped,
        "Mindskinner should be tapped from attacking"
    );
}
