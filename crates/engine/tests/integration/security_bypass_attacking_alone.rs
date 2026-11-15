//! Security Bypass — recipient-scoped attacking-alone evasion and granted
//! combat-damage Connive trigger.
//!
//! Oracle verified from the generated MTGJSON data. CR 506.5 defines
//! "attacking alone" across the whole combat, CR 509.1b applies block
//! restrictions, CR 611.3a continuously re-evaluates the static ability, and
//! CR 113.8 makes the enchanted creature's controller control the granted
//! triggered ability.

use engine::game::combat::{can_block_pair, AttackTarget};
use engine::game::effects::attach::attach_to;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const SECURITY_BYPASS_ORACLE: &str = "Enchant creature\n\
As long as enchanted creature is attacking alone, it can't be blocked.\n\
Enchanted creature has \"Whenever this creature deals combat damage to a player, it connives.\" \
(Its controller draws a card, then discards a card. If they discarded a nonland card, they put a \
+1/+1 counter on this creature.)";

fn add_security_bypass(scenario: &mut GameScenario, aura_controller: PlayerId) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(aura_controller, "Security Bypass", SECURITY_BYPASS_ORACLE)
        .with_subtypes(vec!["Aura"])
        .id()
}

fn attach_and_assert(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    attach_to(runner.state_mut(), aura, host);
    assert_eq!(
        runner.state().objects[&aura]
            .attached_to
            .and_then(|target| target.as_object()),
        Some(host),
        "Security Bypass must be legally attached to the creature host"
    );
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("passing priority should reach declare attackers"),
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        };
    }
    panic!("did not reach declare attackers");
}

fn drive_until_trigger_stacked(runner: &mut GameRunner) {
    for _ in 0..64 {
        if !runner.state().stack.is_empty() {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("passing priority should advance combat"),
            WaitingFor::DeclareBlockers { .. } => runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("defender should be able to declare no blockers"),
            WaitingFor::OrderTriggers { triggers, .. } => runner
                .act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("default trigger order should be accepted"),
            ref other => panic!("unexpected wait state while advancing combat: {other:?}"),
        };
    }
    panic!("Security Bypass trigger never reached the stack");
}

#[test]
fn solo_enchanted_attacker_is_unblockable_and_aura_departure_removes_grant() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Bypassed Operative", 3, 3).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();
    let aura = add_security_bypass(&mut scenario, P2);
    let mut runner = scenario.build();
    attach_and_assert(&mut runner, aura, host);

    assert_eq!(runner.state().objects[&aura].controller, P2);
    assert_eq!(runner.state().objects[&host].controller, P0);
    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(host, AttackTarget::Player(P1))])
        .expect("solo attack must be legal");
    assert!(
        !can_block_pair(runner.state(), blocker, host),
        "the enchanted creature must be unblockable while attacking alone"
    );

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), aura, Zone::Graveyard, &mut events);
    assert_eq!(runner.state().objects[&aura].zone, Zone::Graveyard);
    assert!(
        can_block_pair(runner.state(), blocker, host),
        "the evasion grant must disappear when Security Bypass leaves the battlefield"
    );
}

#[test]
fn a_global_coattacker_makes_the_enchanted_creature_blockable() {
    let mut scenario = GameScenario::new_n_player(3, 43);
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Bypassed Operative", 3, 3).id();
    let coattacker = scenario.add_creature(P0, "Other Operative", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Guard", 2, 2).id();
    let aura = add_security_bypass(&mut scenario, P2);
    let mut runner = scenario.build();
    attach_and_assert(&mut runner, aura, host);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (host, AttackTarget::Player(P1)),
            (coattacker, AttackTarget::Player(P2)),
        ])
        .expect("both attacks must be legal");
    assert!(
        can_block_pair(runner.state(), blocker, host),
        "CR 506.5 counts all attackers, even those attacking another defender"
    );
}

#[test]
fn enchanted_host_controls_and_resolves_the_granted_connive_trigger() {
    let mut scenario = GameScenario::new_n_player(3, 44);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Nonland Top"]);
    let host = scenario.add_creature(P0, "Bypassed Operative", 3, 3).id();
    let aura = add_security_bypass(&mut scenario, P2);
    let mut runner = scenario.build();
    attach_and_assert(&mut runner, aura, host);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(host, AttackTarget::Player(P1))])
        .expect("solo attack must be legal");
    drive_until_trigger_stacked(&mut runner);

    let trigger = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == host)
        .expect("the enchanted host's granted damage trigger must reach the stack");
    assert_eq!(
        trigger.controller, P0,
        "the host's controller, not the Aura's controller, controls the granted trigger"
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().players[0].hand.is_empty(),
        "Connive must draw and then discard the only nonland card"
    );
    assert_eq!(
        runner.state().objects[&host]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "discarding a nonland card while conniving must put a +1/+1 counter on the host"
    );
}
