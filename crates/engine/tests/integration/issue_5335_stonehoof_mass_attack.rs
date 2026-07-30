//! Regression for GitHub issue #5335 — Stonehoof Chieftain with 10+ attackers.
//!
//! Oracle: Trample, indestructible
//! Whenever another creature you control attacks, it gains trample and indestructible until end of turn.
//!
//! https://github.com/phase-rs/phase/issues/5335

use engine::game::combat::AttackTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const STONEHOOF_ORACLE: &str = "Trample, indestructible\nWhenever another creature you control attacks, it gains trample and indestructible until end of turn.";

fn resolve_attack_triggers(runner: &mut GameRunner) {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 256,
            "stalled resolving attack triggers; waiting_for={:?} stack={} pending={:?}",
            runner.state().waiting_for,
            runner.state().stack.len(),
            runner
                .state()
                .pending_trigger_order
                .as_ref()
                .map(|o| o.groups.len())
        );
        let waiting = runner.state().waiting_for.clone();
        match waiting {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("OrderTriggers");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareBlockers { .. } | WaitingFor::Priority { .. } => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
    runner.advance_until_stack_empty();
}

fn assert_attacker_keywords(runner: &mut GameRunner, attackers: &[ObjectId]) {
    evaluate_layers(runner.state_mut());
    for id in attackers {
        let obj = runner.state().objects.get(id).expect("attacker on bf");
        assert!(
            obj.has_keyword(&Keyword::Trample),
            "attacker {id:?} must gain trample from Stonehoof"
        );
        assert!(
            obj.has_keyword(&Keyword::Indestructible),
            "attacker {id:?} must gain indestructible from Stonehoof"
        );
    }
}

#[test]
fn stonehoof_auto_orders_ten_per_attacker_attack_triggers_without_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Stonehoof Chieftain", 8, 8, STONEHOOF_ORACLE);
    let mut attackers = Vec::new();
    for i in 0..10 {
        attackers.push(scenario.add_creature(P0, &format!("Raider {i}"), 2, 2).id());
    }

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("declare attackers");

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "per-attacker Stonehoof triggers commute — CR 603.3b ordering is immaterial"
    );
    runner.advance_until_stack_empty();
    assert_attacker_keywords(&mut runner, &attackers);
}

#[test]
fn stonehoof_grants_trample_and_indestructible_to_each_of_ten_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Stonehoof Chieftain", 8, 8, STONEHOOF_ORACLE);
    let mut attackers = Vec::new();
    for i in 0..10 {
        attackers.push(scenario.add_creature(P0, &format!("Raider {i}"), 2, 2).id());
    }

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("declare attackers");

    resolve_attack_triggers(&mut runner);
    assert_attacker_keywords(&mut runner, &attackers);
}

#[test]
fn stonehoof_trample_damage_reaches_player_after_mass_attack_triggers() {
    use super::rules::run_combat;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Stonehoof Chieftain", 8, 8, STONEHOOF_ORACLE);
    let mut attackers = Vec::new();
    for i in 0..10 {
        attackers.push(scenario.add_creature(P0, &format!("Raider {i}"), 2, 2).id());
    }
    let blocker = scenario.add_creature(P1, "Lone Blocker", 2, 2).id();

    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    // Block only the first attacker; the other nine plus trample excess should connect.
    run_combat(
        &mut runner,
        attackers.clone(),
        vec![(blocker, attackers[0])],
    );

    // First attacker: 2 to blocker (lethal), 0 trample (exactly blocked).
    // Other nine: 2 each = 18. Total 18.
    assert_eq!(
        runner.life(P1),
        life_before - 18,
        "nine unblocked 2/2 tramplers deal 18; blocked attacker assigns lethal only"
    );
}
