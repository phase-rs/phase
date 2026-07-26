//! Issue #6634 — Aven Courier's attack trigger.
//!
//! Claim-to-test matrix:
//! - source population authority → controlled-permanent union excludes opponent;
//! - stack vs. resolution timing → trigger target selection precedes NamedChoice;
//! - interactive continuation → ChooseOption resumes into PutChosenCounter;
//! - chosen-kind absence gate → add exactly once when absent, no-op when present;
//! - CR 608.2b all-targets-illegal path → no resolution choice or placement.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const AVEN_COURIER: &str = "Flying\n\
Whenever this creature attacks, choose a counter on a permanent you control. \
Put a counter of that kind on target permanent you control if it doesn't have a counter of that kind on it.";

fn setup(target_starts_with_stun: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aven = {
        let mut builder = scenario.add_creature(P0, "Aven Courier", 1, 1);
        builder.from_oracle_text_with_keywords(&["Flying"], AVEN_COURIER);
        builder.id()
    };
    let stun_source = scenario.add_creature(P0, "Stun Source", 2, 2).id();
    scenario.with_counter(stun_source, CounterType::Stun, 1);
    let plus_source = scenario.add_creature(P0, "Plus Source", 2, 2).id();
    scenario.with_counter(plus_source, CounterType::Plus1Plus1, 1);
    let hostile = scenario.add_creature(P1, "Hostile Counter", 2, 2).id();
    scenario.with_counter(hostile, CounterType::Loyalty, 1);
    let target = scenario.add_creature(P0, "Destination", 2, 2).id();
    if target_starts_with_stun {
        scenario.with_counter(target, CounterType::Stun, 1);
    }
    (scenario.build(), aven, target)
}

fn advance_to_declare_attackers(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering combat triggers should succeed");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance to declare attackers");
            }
            other => panic!("unexpected state before attackers: {other:?}"),
        }
    }
    panic!("expected DeclareAttackers");
}

/// CR 115.1d: choose Aven Courier's sole printed target while the attack
/// trigger is being put on the stack. A counter-kind prompt here would prove
/// the resolution-only source choice leaked into target announcement.
fn put_attack_trigger_on_stack(runner: &mut GameRunner, aven: ObjectId, target: ObjectId) {
    advance_to_declare_attackers(runner, P0);
    runner
        .declare_attackers(&[(aven, AttackTarget::Player(P1))])
        .expect("Aven should be a legal attacker");

    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("ordering attack triggers should succeed");
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert_eq!(
                    target_slots.len(),
                    1,
                    "only the printed destination is a stack target"
                );
                assert!(
                    target_slots[0]
                        .legal_targets
                        .contains(&TargetRef::Object(target)),
                    "controlled destination must be a legal target"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choosing Aven's destination should succeed");
                return;
            }
            WaitingFor::NamedChoice { .. } => {
                panic!("counter kind must not be chosen before the target is on the stack")
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should reach trigger targeting");
            }
            other => panic!("unexpected attack-trigger state: {other:?}"),
        }
    }
    panic!("expected TriggerTargetSelection");
}

fn resolve_to_counter_kind_choice(runner: &mut GameRunner) -> Vec<String> {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert!(matches!(choice_type, ChoiceType::CounterKind { .. }));
                return options;
            }
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            other => panic!("unexpected state resolving Aven trigger: {other:?}"),
        }
    }
    panic!("expected counter-kind NamedChoice");
}

fn stun_count(runner: &GameRunner, target: ObjectId) -> u32 {
    runner.state().objects[&target]
        .counters
        .get(&CounterType::Stun)
        .copied()
        .unwrap_or(0)
}

/// CR 608.2c + CR 608.2d + CR 122.1: the target is announced first, the
/// controller then chooses among kinds on controlled permanents, and the
/// chosen kind is placed because the target lacks it.
#[test]
fn attack_trigger_chooses_kind_at_resolution_and_adds_when_absent() {
    let (mut runner, aven, target) = setup(false);
    put_attack_trigger_on_stack(&mut runner, aven, target);

    let options = resolve_to_counter_kind_choice(&mut runner);
    assert_eq!(
        options,
        vec![
            CounterType::Plus1Plus1.as_str().into_owned(),
            CounterType::Stun.as_str().into_owned(),
        ],
        "choice domain is the union on controlled permanents"
    );
    assert!(
        !options.contains(&CounterType::Loyalty.as_str().into_owned()),
        "opponent-controlled counter kinds are not legal choices"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: CounterType::Stun.as_str().into_owned(),
        })
        .expect("choosing Stun should resume the trigger");
    runner.advance_until_stack_empty();
    assert_eq!(
        stun_count(&runner, target),
        1,
        "PutChosenCounter delegates one Stun placement through the normal pipeline"
    );
    assert!(
        runner.state().objects[&aven]
            .chosen_attributes
            .iter()
            .all(|attribute| !matches!(
                attribute,
                engine::types::ability::ChosenAttribute::Counter(_)
            )),
        "the resolution-only counter choice must not persist on Aven Courier"
    );
}

/// CR 608.2c + CR 122.1: the chosen-kind predicate is false when the target
/// already has that kind, so the placement instruction is a no-op.
#[test]
fn attack_trigger_does_not_add_when_chosen_kind_is_present() {
    let (mut runner, aven, target) = setup(true);
    put_attack_trigger_on_stack(&mut runner, aven, target);
    let options = resolve_to_counter_kind_choice(&mut runner);
    assert!(options.contains(&CounterType::Stun.as_str().into_owned()));

    runner
        .act(GameAction::ChooseOption {
            choice: CounterType::Stun.as_str().into_owned(),
        })
        .expect("choosing Stun should resume the trigger");
    runner.advance_until_stack_empty();
    assert_eq!(
        stun_count(&runner, target),
        1,
        "the EQ-zero gate prevents an additional Stun counter"
    );
}

/// CR 608.2b: when Aven Courier's sole target is illegal, the entire triggered
/// ability fails to resolve. No counter-kind choice is offered.
#[test]
fn all_targets_illegal_skips_counter_kind_choice_and_placement() {
    let (mut runner, aven, target) = setup(false);
    put_attack_trigger_on_stack(&mut runner, aven, target);
    assert!(runner.state().stack.iter().any(|entry| {
        entry.source_id == aven && matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })
    }));

    move_to_zone(runner.state_mut(), target, Zone::Graveyard, &mut Vec::new());
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::NamedChoice { .. } => {
                panic!("an all-targets-illegal trigger must not resolve or prompt")
            }
            other => panic!("unexpected state after target became illegal: {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "the all-targets-illegal trigger leaves the stack"
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Graveyard);
}
