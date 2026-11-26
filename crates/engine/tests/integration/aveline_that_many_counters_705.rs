//! Aveline de Grandpré: "Whenever a creature you control with deathtouch deals
//! combat damage to a player, put that many +1/+1 counters on that creature."
//!
//! Issue #705: the trigger fired but placed no counters, because "put that many
//! ... counters on that creature" bound the recipient to a parent target that
//! an untargeted trigger never has. CR 608.2k: "that creature" is the
//! triggering creature, and under CR 608.2c "that many" is the damage that
//! creature dealt in its own trigger occurrence. Each row is a real unblocked
//! attack.

use crate::support::shared_card_db;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

// Verbatim Oracle text (MTGJSON AtomicCards).
const AVELINE: &str = "Deathtouch\nWhenever a creature you control with deathtouch deals combat damage to a player, put that many +1/+1 counters on that creature.\nDisguise {B}{G} (You may cast this card face down for {3} as a 2/2 creature with ward {2}. Turn it face up any time for its disguise cost.)";

fn scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // CR 302.6: battlefield fixtures default to creatures eligible to attack.
    let aveline = scenario
        .add_creature_from_oracle(P0, "Aveline de Grandpré", 3, 3, AVELINE)
        .as_legendary()
        .with_subtypes(vec!["Human", "Assassin"])
        .id();
    (scenario, aveline)
}

fn deal_unblocked_combat_damage(scenario: GameScenario, attackers: &[ObjectId]) -> GameRunner {
    let mut runner = scenario.build();
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&attacker| (attacker, AttackTarget::Player(P1)))
        .collect();
    runner.declare_attackers(&attacks).unwrap();
    // P1 has no eligible blockers, so the engine submits the empty declaration.
    for _ in 0..4 {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    runner
}

fn finish_triggers(runner: &mut GameRunner, count: usize) {
    assert_eq!(runner.state().stack.len(), count);
    for _ in 0..count {
        runner.act(GameAction::PassPriority).unwrap();
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

fn counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn aveline_alone_gets_three_counters() {
    let (scenario, aveline) = scenario();
    let mut runner = deal_unblocked_combat_damage(scenario, &[aveline]);
    // CR 510.2 + CR 120.3a: the unblocked attack deals three damage.
    assert_eq!(runner.life(P1), 17);
    finish_triggers(&mut runner, 1);
    // CR 608.2k: Aveline is the creature referred to by this trigger.
    assert_eq!(counters(&runner, aveline), 3);
}

#[test]
fn aveline_and_rats_each_get_counters_for_their_own_damage() {
    let (mut scenario, aveline) = scenario();
    let rats = scenario.add_real_card(
        P0,
        "Typhoid Rats",
        Zone::Battlefield,
        shared_card_db().expect("integration fixture"),
    );
    let mut runner = deal_unblocked_combat_damage(scenario, &[aveline, rats]);
    // CR 510.2 + CR 120.3a: the two unblocked attackers deal four damage.
    assert_eq!(runner.life(P1), 16);
    // CR 603.2c + CR 603.3b: two damage occurrences produce two triggers for P0 to order.
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!(
            "expected trigger ordering, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((*player, triggers.len()), (P0, 2));
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .unwrap();
    finish_triggers(&mut runner, 2);
    // CR 608.2k: each trigger places counters on its own damage dealer.
    assert_eq!(counters(&runner, aveline), 3);
    assert_eq!(counters(&runner, rats), 1);
}

#[test]
fn aveline_ignores_damage_from_creatures_without_deathtouch() {
    let (mut scenario, aveline) = scenario();
    let bears = scenario.add_real_card(
        P0,
        "Grizzly Bears",
        Zone::Battlefield,
        shared_card_db().expect("integration fixture"),
    );
    let mut runner = deal_unblocked_combat_damage(scenario, &[aveline, bears]);
    // CR 510.2 + CR 120.3a: both attackers deal damage, regardless of deathtouch.
    assert_eq!(runner.life(P1), 15);
    finish_triggers(&mut runner, 1);
    assert_eq!(counters(&runner, aveline), 3);
    assert_eq!(counters(&runner, bears), 0);
}
