//! Attack-declaration qualifier regression tests.
//!
//! These cover event-only attack conditions that must be checked when the
//! attack event fires, then stripped from the stack entry so resolution does
//! not re-evaluate mutable attacker state.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{AttackersDeclaredCountSubject, Comparator, TriggerCondition};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::{AttackTargetFilter, TriggerMode};

use super::rules::AttackTarget;

const P2: PlayerId = PlayerId(2);

const HYDRA_INFILTRATION: &str =
    "When this enchantment enters, target opponent discards two cards.\n\
Whenever a creature you control attacks alone, target opponent loses 1 life and you gain 1 life.";

const AURELIA_ATTACKS: &str = "Flying, vigilance, haste\n\
Whenever a player attacks with three or more creatures, you draw a card.\n\
Whenever a player attacks with five or more creatures, Aurelia deals 3 damage to each of your opponents and you gain 3 life.";

const FIREMANE_COMMANDO: &str = "Flying\n\
Whenever you attack with two or more creatures, draw a card.\n\
Whenever another player attacks with two or more creatures, they draw a card if none of those creatures attacked you.";

const TROUBLE_IN_PAIRS: &str =
    "If an opponent would begin an extra turn, that player skips that turn instead.\n\
Whenever an opponent attacks you with two or more creatures, draws their second card each turn, or casts their second spell each turn, you draw a card.";

fn add_enchantment_from_oracle(
    scenario: &mut GameScenario,
    controller: PlayerId,
    name: &str,
    oracle: &str,
) -> ObjectId {
    let mut builder = scenario.add_creature(controller, name, 0, 1);
    builder.as_enchantment().from_oracle_text(oracle);
    builder.id()
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn order_triggers_if_needed(runner: &mut GameRunner) {
    while let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering attack triggers should succeed");
    }
}

fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass should advance toward declare attackers");
    }
    panic!("expected DeclareAttackers");
}

fn stack_condition_for_source(
    runner: &GameRunner,
    source_id: ObjectId,
) -> Option<TriggerCondition> {
    runner.state().stack.iter().find_map(|entry| {
        if entry.source_id != source_id {
            return None;
        }
        match &entry.kind {
            StackEntryKind::TriggeredAbility { condition, .. } => condition.clone(),
            _ => None,
        }
    })
}

fn assert_stack_condition_stripped(runner: &GameRunner, source_id: ObjectId) {
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source_id)
        .expect("expected attack trigger on the stack");
    match &entry.kind {
        StackEntryKind::TriggeredAbility { condition, .. } => {
            assert_eq!(
                condition, &None,
                "event-only attack qualifier must be stripped from stack condition"
            );
        }
        other => panic!("expected triggered ability, got {other:?}"),
    }
}

fn assert_any_stack_condition_stripped(runner: &GameRunner) {
    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        .expect("expected a triggered ability on the stack");
    match &entry.kind {
        StackEntryKind::TriggeredAbility { condition, .. } => {
            assert_eq!(
                condition, &None,
                "event-only attack qualifier must be stripped from stack condition"
            );
        }
        other => panic!("expected triggered ability, got {other:?}"),
    }
}

fn choose_trigger_player_target(runner: &mut GameRunner, target: PlayerId) {
    for _ in 0..16 {
        order_triggers_if_needed(runner);
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let target_ref = engine::types::ability::TargetRef::Player(target);
                let slot = &target_slots[selection.current_slot];
                assert!(
                    slot.legal_targets.contains(&target_ref),
                    "target {target:?} must be legal for trigger slot {slot:?}"
                );
                assert!(
                    runner
                        .state()
                        .pending_trigger
                        .as_ref()
                        .is_some_and(|pending| pending.condition.is_none()),
                    "pending trigger condition should be stripped before target selection"
                );
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target_ref),
                    })
                    .expect("choosing trigger target should succeed");
                return;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward trigger target selection");
            }
            _ => {}
        }
    }
    panic!("expected TriggerTargetSelection");
}

#[test]
fn hydra_shape_and_lone_attacker_trigger_strips_min_co_attackers() {
    let parsed = parse_oracle_text(
        HYDRA_INFILTRATION,
        "HYDRA Infiltration",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = &parsed.triggers[1];
    assert_eq!(trigger.mode, TriggerMode::Attacks);
    assert!(matches!(
        trigger.condition.as_ref(),
        Some(TriggerCondition::Not { ref condition })
            if matches!(condition.as_ref(), TriggerCondition::MinCoAttackers { minimum: 1, filter: None })
    ));

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let hydra =
        add_enchantment_from_oracle(&mut scenario, P0, "HYDRA Infiltration", HYDRA_INFILTRATION);
    let attacker = scenario.add_creature(P0, "Solo Agent", 2, 2).id();
    let mut runner = scenario.build();

    let p0_life_before = life(&runner, P0);
    let p1_life_before = life(&runner, P1);
    let p2_life_before = life(&runner, P2);
    hand_turn_to(&mut runner, P0);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("solo attacker should be legal");
    choose_trigger_player_target(&mut runner, P2);
    assert_stack_condition_stripped(&runner, hydra);
    if let Some(combat) = &mut runner.state_mut().combat {
        combat.attackers.clear();
    }
    runner.advance_until_stack_empty();

    assert_eq!(life(&runner, P0), p0_life_before + 1);
    assert_eq!(life(&runner, P1), p1_life_before);
    assert_eq!(life(&runner, P2), p2_life_before - 1);
}

#[test]
fn hydra_two_attackers_do_not_trigger_attacks_alone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let hydra =
        add_enchantment_from_oracle(&mut scenario, P0, "HYDRA Infiltration", HYDRA_INFILTRATION);
    let first = scenario.add_creature(P0, "First Agent", 2, 2).id();
    let second = scenario.add_creature(P0, "Second Agent", 2, 2).id();
    let mut runner = scenario.build();
    let p1_life_before = life(&runner, P1);

    hand_turn_to(&mut runner, P0);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ])
        .expect("two attackers should be legal");
    runner.advance_until_stack_empty();

    assert!(
        !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == hydra)
            && runner
                .state()
                .pending_trigger
                .as_ref()
                .is_none_or(|pending| pending.source_id != hydra),
        "HYDRA attacks-alone trigger must not be pending or stacked for two attackers"
    );
    assert_eq!(life(&runner, P1), p1_life_before);
}

#[test]
fn aurelia_style_triggering_player_attack_count_strips_and_survives_mutation() {
    let parsed = parse_oracle_text(
        AURELIA_ATTACKS,
        "Aurelia, the Law Above",
        &[],
        &["Creature".to_string()],
        &[],
    );
    assert!(matches!(
        parsed.triggers[0].condition.as_ref(),
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::Controller {
                scope: engine::types::ability::ControllerRef::TriggeringPlayer,
                ..
            },
            comparator: Comparator::GE,
            count: 3,
        })
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Drawn Card");
    let _aurelia = scenario
        .add_creature_from_oracle(P0, "Aurelia, the Law Above", 4, 4, AURELIA_ATTACKS)
        .id();
    let attackers = [
        scenario.add_creature(P1, "Attacker A", 2, 2).id(),
        scenario.add_creature(P1, "Attacker B", 2, 2).id(),
        scenario.add_creature(P1, "Attacker C", 2, 2).id(),
    ];
    let mut runner = scenario.build();
    let p0_hand_before = hand_size(&runner, P0);

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&attackers.map(|id| (id, AttackTarget::Player(P0))))
        .expect("three attackers should be legal");
    order_triggers_if_needed(&mut runner);
    assert_any_stack_condition_stripped(&runner);
    runner
        .state_mut()
        .objects
        .get_mut(&attackers[1])
        .unwrap()
        .controller = P0;
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P0), p0_hand_before + 1);
}

#[test]
fn aurelia_style_two_attackers_do_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Drawn Card");
    let aurelia = scenario
        .add_creature_from_oracle(P0, "Aurelia, the Law Above", 4, 4, AURELIA_ATTACKS)
        .id();
    let attackers = [
        scenario.add_creature(P1, "Attacker A", 2, 2).id(),
        scenario.add_creature(P1, "Attacker B", 2, 2).id(),
    ];
    let mut runner = scenario.build();
    let p0_hand_before = hand_size(&runner, P0);

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&attackers.map(|id| (id, AttackTarget::Player(P0))))
        .expect("two attackers should be legal");
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P0), p0_hand_before);
    assert!(
        !runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == aurelia),
        "three-or-more attack trigger must not stack for two attackers"
    );
}

#[test]
fn firemane_preserves_true_attack_target_intervening_if_after_controller_count_strip() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P1, "P1 Drawn Card");
    let firemane = scenario
        .add_creature_from_oracle(P0, "Firemane Commando", 4, 3, FIREMANE_COMMANDO)
        .id();
    let first = scenario.add_creature(P1, "Attacker A", 2, 2).id();
    let second = scenario.add_creature(P1, "Attacker B", 2, 2).id();
    let mut runner = scenario.build();
    let p1_hand_before = hand_size(&runner, P1);

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P2)),
            (second, AttackTarget::Player(P2)),
        ])
        .expect("attacking another opponent should be legal");
    order_triggers_if_needed(&mut runner);
    let condition = stack_condition_for_source(&runner, firemane)
        .expect("Firemane stack condition should preserve attacked-you gate");
    assert!(matches!(
        condition,
        TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::AttackTarget {
                controller: engine::types::ability::ControllerRef::You,
                attacked: AttackTargetFilter::Player,
                ..
            },
            comparator: Comparator::EQ,
            count: 0,
        }
    ));
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P1), p1_hand_before + 1);
}

#[test]
fn firemane_true_attack_target_condition_gates_attacks_at_controller() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P1, "P1 Drawn Card");
    scenario.add_creature_from_oracle(P0, "Firemane Commando", 4, 3, FIREMANE_COMMANDO);
    let first = scenario.add_creature(P1, "Attacker A", 2, 2).id();
    let second = scenario.add_creature(P1, "Attacker B", 2, 2).id();
    let mut runner = scenario.build();
    let p1_hand_before = hand_size(&runner, P1);

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P0)),
            (second, AttackTarget::Player(P0)),
        ])
        .expect("attacking Firemane controller should be legal");
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P1), p1_hand_before);
}

#[test]
fn trouble_attack_target_filter_strips_attack_target_threshold_on_stack() {
    let parsed = parse_oracle_text(
        TROUBLE_IN_PAIRS,
        "Trouble in Pairs",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers[0].attack_target_filter.as_ref(),
        Some(&AttackTargetFilter::Player)
    );
    assert!(matches!(
        parsed.triggers[0].condition.as_ref(),
        Some(TriggerCondition::AttackersDeclaredCount {
            subject: AttackersDeclaredCountSubject::AttackTarget {
                attacked: AttackTargetFilter::Player,
                ..
            },
            comparator: Comparator::GE,
            count: 2,
        })
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Drawn Card");
    let trouble =
        add_enchantment_from_oracle(&mut scenario, P0, "Trouble in Pairs", TROUBLE_IN_PAIRS);
    let first = scenario.add_creature(P1, "Attacker A", 2, 2).id();
    let second = scenario.add_creature(P1, "Attacker B", 2, 2).id();
    let mut runner = scenario.build();
    let p0_hand_before = hand_size(&runner, P0);

    hand_turn_to(&mut runner, P1);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P0)),
            (second, AttackTarget::Player(P0)),
        ])
        .expect("attacking Trouble controller should be legal");
    order_triggers_if_needed(&mut runner);
    assert_stack_condition_stripped(&runner, trouble);
    runner.advance_until_stack_empty();

    assert_eq!(hand_size(&runner, P0), p0_hand_before + 1);
}
