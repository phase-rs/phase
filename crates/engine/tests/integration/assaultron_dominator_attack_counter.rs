//! Assaultron Dominator {3}{R} Artifact Creature — "Whenever an artifact
//! creature you control attacks, you may pay {E}. If you do, put your choice
//! of a +1/+1, first strike, or trample counter on that creature."
//!
//! Sibling of the Path of Discovery fix (issue #8361): the same shared
//! chain hand-off seam. The trigger head is a slot-less event-bound
//! `TargetOnly { TriggeringSource }` whose child (`ChooseOneOf` over
//! `PutCounter { ParentTarget }` branches) must inherit the attacking
//! creature. Pre-fix the child read the original empty target list and the
//! counter fell back to the trigger source.
//!
//! CR 508.1 (declare attackers), CR 608.2k (an untargeted object previously
//! referred to by the trigger condition is still affected), CR 608.2c
//! (instructions use the bound referent).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Assaultron Dominator {3}{R} Artifact Creature — verbatim Oracle text. The
/// ETB energy ability never fires here: the fixture places Assaultron
/// directly, so only the attack trigger is exercised.
const ASSAULTRON_DOMINATOR: &str = "When this creature enters, you get {E}{E} (two energy counters).\nWhenever an artifact creature you control attacks, you may pay {E}. If you do, put your choice of a +1/+1, first strike, or trample counter on that creature.";

#[test]
fn assaultron_counter_lands_on_the_attacker_not_the_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dominator = scenario
        .add_creature_from_oracle(P0, "Assaultron Dominator", 4, 4, ASSAULTRON_DOMINATOR)
        .as_artifact_creature()
        .id();
    let thopter = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .as_artifact_creature()
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].energy = 1;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(thopter, AttackTarget::Player(P1))])
        .expect("declaring the attacker is legal");

    // Drive: trigger ordering, then priority passes until the "you may pay"
    // prompt surfaces.
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering triggers is legal");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::ChooseOneOfBranch { .. } => break,
            other => panic!("unexpected prompt while resolving the attack trigger: {other:?}"),
        }
    }

    // CR 118.12: accept the "you may pay {E}" cost.
    match runner.state().waiting_for.clone() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(player, P0, "Assaultron's controller pays the {{E}}");
            runner
                .act(GameAction::DecideOptionalEffect { accept: true })
                .expect("accepting the payment is legal");
        }
        other => panic!("expected the pay-{{E}} prompt, got {other:?}"),
    }
    assert_eq!(
        runner.state().players[P0.0 as usize].energy,
        0,
        "reach: the {{E}} payment was spent"
    );

    // Choose the +1/+1 branch (first of +1/+1, first strike, trample).
    match runner.state().waiting_for.clone() {
        WaitingFor::ChooseOneOfBranch {
            branch_descriptions,
            ..
        } => {
            let index = branch_descriptions
                .iter()
                .position(|d| d.contains("+1/+1"))
                .expect("a +1/+1 branch");
            runner
                .act(GameAction::ChooseBranch { index })
                .expect("choosing the branch is legal");
        }
        other => panic!("expected the counter-choice branches, got {other:?}"),
    }

    let state = runner.state();
    assert_eq!(
        state.objects[&thopter]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the counter must land on the attacking creature"
    );
    assert_eq!(
        state.objects[&dominator]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "the counter must not fall back to the trigger source"
    );
    assert!(
        state.stack.is_empty(),
        "the attack trigger must be fully resolved"
    );
    // CR 508.2: after attackers are declared, the active player gets priority.
    assert_eq!(state.phase, Phase::DeclareAttackers);
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the active player must hold priority after the trigger, got {:?}",
        state.waiting_for
    );
}
