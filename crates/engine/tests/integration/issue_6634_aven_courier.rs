//! Issue #6634 — Aven Courier's attack trigger.
//!
//! Claim-to-test matrix:
//! - source authority → controlled counter-bearing permanent excludes opponent;
//! - stack vs. resolution timing → trigger target selection precedes source choice;
//! - interactive continuation → source selection then ChooseOption resume placement;
//! - chosen-kind absence gate → add exactly once when absent, no-op when present;
//! - CR 608.2b all-targets-illegal path → no resolution choice or placement.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    ChoiceType, ControllerRef, Effect, FilterProp, QuantityExpr, QuantityRef, ResolvedAbility,
    TargetFilter, TargetRef, ThisWayCause, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const AVEN_COURIER: &str = "Flying\n\
Whenever this creature attacks, choose a counter on a permanent you control. \
Put a counter of that kind on target permanent you control if it doesn't have a counter of that kind on it.";

fn setup(target_starts_with_stun: bool) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aven = {
        let mut builder = scenario.add_creature(P0, "Aven Courier", 1, 1);
        builder.from_oracle_text_with_keywords(&["Flying"], AVEN_COURIER);
        builder.id()
    };
    let stun_source = scenario.add_creature(P0, "Stun Source", 2, 2).id();
    scenario.with_counter(stun_source, CounterType::Stun, 1);
    scenario.with_counter(stun_source, CounterType::Plus1Plus1, 1);
    let plus_source = scenario.add_creature(P0, "Plus Source", 2, 2).id();
    scenario.with_counter(plus_source, CounterType::Plus1Plus1, 1);
    let hostile = scenario.add_creature(P1, "Hostile Counter", 2, 2).id();
    scenario.with_counter(hostile, CounterType::Loyalty, 1);
    let target = scenario.add_creature(P0, "Destination", 2, 2).id();
    if target_starts_with_stun {
        scenario.with_counter(target, CounterType::Stun, 1);
    }
    (scenario.build(), aven, target, stun_source)
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

fn resolve_to_counter_kind_choice(
    runner: &mut GameRunner,
    counter_source: ObjectId,
) -> Vec<String> {
    for _ in 0..20 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseFromZoneChoice { cards, count, .. } => {
                assert_eq!(count, 1);
                assert!(
                    cards.contains(&counter_source),
                    "the declared controlled counter-bearing permanent must be offered"
                );
                assert!(
                    cards
                        .iter()
                        .all(|id| runner.state().objects[id].controller == P0),
                    "opponent-controlled permanents must not be offered as counter sources"
                );
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![counter_source],
                    })
                    .expect("selecting Aven's counter-source permanent should succeed");
            }
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
/// controller then selects a controlled permanent and a kind on that
/// permanent, and the chosen kind is placed because the target lacks it.
#[test]
fn attack_trigger_chooses_kind_at_resolution_and_adds_when_absent() {
    let (mut runner, aven, target, counter_source) = setup(false);
    put_attack_trigger_on_stack(&mut runner, aven, target);

    let options = resolve_to_counter_kind_choice(&mut runner, counter_source);
    assert_eq!(
        options,
        vec![
            CounterType::Plus1Plus1.as_str().into_owned(),
            CounterType::Stun.as_str().into_owned(),
        ],
        "only kinds on the selected controlled permanent are legal"
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
    let (mut runner, aven, target, counter_source) = setup(true);
    put_attack_trigger_on_stack(&mut runner, aven, target);
    let options = resolve_to_counter_kind_choice(&mut runner, counter_source);
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
    let (mut runner, aven, target, _) = setup(false);
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
            WaitingFor::ChooseFromZoneChoice { .. } => {
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

/// CR 608.2c + CR 122.1: The production `repeat_for:
/// DistinctCounterKindsAmong` consumer must snapshot only counter kinds inside
/// a complemented `TrackedSetFiltered` domain, including its nested object
/// predicate and producer-action provenance. The observable +1/+1 counter
/// count proves the loop ran once for Lore, not once for every raw tracked or
/// unrelated battlefield kind.
#[test]
fn filtered_tracked_set_complement_drives_one_production_repeat_iteration() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Repeat Source", 2, 2).id();
    let outsider = scenario.add_creature(P0, "Battlefield Outsider", 2, 2).id();
    scenario.with_counter(outsider, CounterType::Shield, 1);
    let mut runner = scenario.build();

    let add_member = |state: &mut engine::types::game_state::GameState,
                      card_id,
                      controller,
                      name: &str,
                      color,
                      counter_type| {
        let id = create_object(
            state,
            CardId(card_id),
            controller,
            name.to_string(),
            Zone::Exile,
        );
        let object = state.objects.get_mut(&id).expect("created tracked member");
        object.card_types.core_types.push(CoreType::Creature);
        object.base_card_types = object.card_types.clone();
        object.color = vec![color];
        object.counters.insert(counter_type, 1);
        id
    };
    let excluded_red = add_member(
        runner.state_mut(),
        10,
        P0,
        "Red Sacrificed",
        ManaColor::Red,
        CounterType::Plus1Plus1,
    );
    let included_green = add_member(
        runner.state_mut(),
        11,
        P0,
        "Green Sacrificed",
        ManaColor::Green,
        CounterType::Lore,
    );
    let wrong_cause = add_member(
        runner.state_mut(),
        12,
        P0,
        "Green Exiled",
        ManaColor::Green,
        CounterType::Stun,
    );
    let wrong_controller = add_member(
        runner.state_mut(),
        13,
        P1,
        "Opponent Sacrificed",
        ManaColor::Green,
        CounterType::Loyalty,
    );

    let tracked = TrackedSetId(17);
    runner.state_mut().tracked_object_sets.insert(
        tracked,
        vec![excluded_red, included_green, wrong_cause, wrong_controller],
    );
    runner.state_mut().tracked_set_member_causes.insert(
        tracked,
        [
            (excluded_red, ThisWayCause::Sacrificed),
            (included_green, ThisWayCause::Sacrificed),
            (wrong_cause, ThisWayCause::Exiled),
            (wrong_controller, ThisWayCause::Sacrificed),
        ]
        .into_iter()
        .collect(),
    );

    let filtered_domain = TargetFilter::TrackedSetFiltered {
        id: tracked,
        filter: Box::new(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        )),
        caused_by: Some(ThisWayCause::Sacrificed),
    };
    let complement = TargetFilter::Not {
        filter: Box::new(TargetFilter::And {
            filters: vec![
                filtered_domain,
                TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::HasColor {
                    color: ManaColor::Red,
                }])),
            ],
        }),
    };
    let mut ability = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
        vec![],
        source,
        P0,
    );
    ability.repeat_for = Some(QuantityExpr::Ref {
        qty: QuantityRef::DistinctCounterKindsAmong { filter: complement },
    });

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("production repeat-for chain resolves");

    assert_eq!(
        runner.state().objects[&source]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "only Lore from the non-red sacrificed controlled member drives an iteration; \
         wrong cause, wrong controller, and battlefield outsider kinds stay outside the domain"
    );
}
