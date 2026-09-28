use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);
const KARAZIKAR_ORACLE: &str = "Whenever you attack a player, tap target creature that player controls and goad it. (Until your next turn, that creature attacks each combat if able and attacks a player other than you if able.)\nWhenever an opponent attacks another one of your opponents, you and the attacking player each draw a card and lose 1 life.";

fn setup() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let karazikar = scenario
        .add_creature_from_oracle(P0, "Karazikar, the Eye Tyrant", 5, 5, KARAZIKAR_ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    (scenario.build(), karazikar, attacker)
}

fn stack_triggers_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .count()
}

#[test]
fn karazikar_does_not_trigger_when_opponent_attacks_controller() {
    let (mut runner, karazikar, attacker) = setup();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 should be able to attack Karazikar's controller");

    // CR 508.3e: the attacked player must be another opponent, not the
    // controller of the triggered ability.
    assert_eq!(stack_triggers_from(&runner, karazikar), 0);
}

#[test]
fn karazikar_triggers_when_opponent_attacks_another_opponent() {
    let (mut runner, karazikar, attacker) = setup();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P2))])
        .expect("P1 should be able to attack another opponent P2");

    // CR 508.3e: P1 attacking P2 matches the player-versus-another-player
    // trigger event relative to Karazikar's controller P0.
    assert_eq!(stack_triggers_from(&runner, karazikar), 1);
}
