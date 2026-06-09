//! Issue #879 — Obsessive Pursuit attack trigger binds X to sacrifices this
//! turn, still triggers at X = 0, and grants lifelink when X >= 3.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const OBSESSIVE_PURSUIT: &str = "Whenever you attack, put X +1/+1 counters on target \
attacking creature, where X is the number of permanents you've sacrificed this turn. \
If X is three or more, that creature gains lifelink until end of turn.";

fn record_sacrifice(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) {
    let state = runner.state_mut();
    let record =
        state.objects[&id].snapshot_for_zone_change(id, Some(Zone::Battlefield), Zone::Graveyard);
    state.sacrificed_permanents_this_turn.push(record);
}

fn p1p1_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Resolve the attack trigger without passing priority through combat.
fn resolve_attack_trigger(runner: &mut engine::game::scenario::GameRunner, target: ObjectId) {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose trigger target");
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.pass_both_players();
            }
            _ => break,
        }
    }
    runner.advance_until_stack_empty();
}

#[test]
fn issue_879_obsessive_pursuit_puts_counters_and_lifelink_after_three_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Attacker", 2, 2)
        .from_oracle_text(OBSESSIVE_PURSUIT)
        .id();
    let fodder: Vec<ObjectId> = (0..3)
        .map(|i| scenario.add_creature(P0, &format!("Fodder {i}"), 1, 1).id())
        .collect();

    let mut runner = scenario.build();
    for id in fodder {
        record_sacrifice(&mut runner, id);
    }

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    resolve_attack_trigger(&mut runner, attacker);

    assert_eq!(
        p1p1_counters(&runner, attacker),
        3,
        "Obsessive Pursuit must put X +1/+1 counters where X is sacrifices this turn"
    );
    assert!(
        runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|effect| {
                effect.affected == TargetFilter::SpecificObject { id: attacker }
                    && effect.modifications.iter().any(|modification| {
                        matches!(
                            modification,
                            engine::types::ability::ContinuousModification::AddKeyword {
                                keyword: Keyword::Lifelink
                            }
                        )
                    })
            }),
        "Obsessive Pursuit must register a lifelink transient effect for the targeted creature"
    );

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert!(
        runner.state().objects[&attacker].has_keyword(&Keyword::Lifelink),
        "Obsessive Pursuit must grant lifelink when the same X binding is three or more"
    );
}

#[test]
fn issue_879_obsessive_pursuit_triggers_with_zero_sacrifices() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Attacker", 2, 2)
        .from_oracle_text(OBSESSIVE_PURSUIT)
        .id();

    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // With a single attacker the engine auto-targets; otherwise it prompts.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::Priority { .. }
        ) && (matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ) || !runner.state().stack.is_empty()),
        "Obsessive Pursuit must fire on attack (prompt or auto-target on stack), got waiting_for={:?} stack={}",
        runner.state().waiting_for,
        runner.state().stack.len()
    );

    resolve_attack_trigger(&mut runner, attacker);

    assert_eq!(
        p1p1_counters(&runner, attacker),
        0,
        "X=0 should resolve after targeting without placing counters"
    );
}
