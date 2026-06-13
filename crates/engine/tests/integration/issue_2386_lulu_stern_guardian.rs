//! Regression for issue #2386: Lulu, Stern Guardian attack trigger.
//!
//! https://github.com/phase-rs/phase/issues/2386
//!
//! - Must fire once per attack declaration (AttackersDeclared), not once per attacker.
//! - Stun counter target must be restricted to creatures attacking Lulu's controller.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const LULU_ORACLE: &str = "Whenever an opponent attacks you, choose target creature attacking you. Put a stun counter on that creature.\n{3}{U}: Proliferate.";

fn offered_creature_targets(runner: &engine::game::scenario::GameRunner) -> Vec<ObjectId> {
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { selection, .. } => selection
            .current_legal_targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .collect(),
        other => panic!("expected TriggerTargetSelection, got {other:?}"),
    }
}

#[test]
fn issue_2386_lulu_fires_once_and_only_targets_attackers() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lulu = scenario
        .add_creature_from_oracle(P0, "Lulu, Stern Guardian", 2, 3, LULU_ORACLE)
        .id();
    let attacker_a = scenario.add_creature(P1, "Attacker A", 2, 2).id();
    let attacker_b = scenario.add_creature(P1, "Attacker B", 2, 2).id();
    let bench = scenario.add_creature(P1, "Bench Creature", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();

    runner
        .declare_attackers(&[
            (attacker_a, AttackTarget::Player(P0)),
            (attacker_b, AttackTarget::Player(P0)),
        ])
        .expect("declare attackers");

    let mut trigger_prompts = 0;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { source_id, .. } => {
                trigger_prompts += 1;
                assert_eq!(
                    source_id,
                    Some(lulu),
                    "Lulu's trigger should be the one requesting targets"
                );
                let offered = offered_creature_targets(&runner);
                assert_eq!(
                    offered.len(),
                    2,
                    "only creatures attacking P0 should be legal targets, got {offered:?}"
                );
                assert!(offered.contains(&attacker_a));
                assert!(offered.contains(&attacker_b));
                assert!(
                    !offered.contains(&bench),
                    "non-attacking creature must not be a legal stun target"
                );
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(attacker_a)],
                    })
                    .expect("select attacking creature");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("unexpected waiting_for while resolving Lulu: {other:?}"),
        }
    }

    assert_eq!(
        trigger_prompts, 1,
        "Lulu must trigger once per attack declaration, not once per attacker"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&attacker_a)
            .and_then(|obj| obj.counters.get(&CounterType::Stun))
            .copied(),
        Some(1),
        "selected attacker should receive a stun counter"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&attacker_b)
            .and_then(|obj| obj.counters.get(&CounterType::Stun))
            .is_none(),
        "unselected attacker should not receive a stun counter"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&bench)
            .and_then(|obj| obj.counters.get(&CounterType::Stun))
            .is_none(),
        "non-attacking creature should not receive a stun counter"
    );
}
