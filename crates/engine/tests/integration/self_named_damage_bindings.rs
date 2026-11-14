//! Production regressions for self-named damage sources and their recipients.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SIN_PRODDER_ORACLE: &str = "Menace\nAt the beginning of your upkeep, reveal the top card of your library. Any opponent may have you put that card into your graveyard. If a player does, this creature deals damage to that player equal to that card's mana value. Otherwise, put that card into your hand.";
const CRAIG_BOONE_ORACLE: &str = "Reach, lifelink\nOne for My Baby — Whenever you attack with two or more creatures, put two quest counters on Craig Boone. When you do, Craig Boone deals damage equal to the number of quest counters on it to up to one target creature unless that creature's controller has Craig Boone deal that much damage to them.";

/// CR 603.3d + CR 118.12 + CR 608.2c + CR 120.1: the accepting opponent is
/// the recipient of Sin Prodder's damage, and Sin Prodder remains its source.
/// The top card's mana value differs from the source's and the event is an
/// actual upkeep trigger resolved through public choice actions.
#[test]
fn sin_prodder_upkeep_acceptor_receives_source_damage_equal_to_top_mana_value() {
    let (mut runner, prodder, revealed) = sin_prodder_scenario();
    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);
    let p2_life = life(&runner, PlayerId(2));

    runner.auto_advance_to_main_phase();
    reach_sin_prodder_choice(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OpponentMayChoice { player: P1, .. }
    ));
    let accepted = runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("P1 accepts Sin Prodder's public choice");
    let mut events = accepted.events;
    drain_stack(&mut runner, &mut events);

    assert_eq!(
        life(&runner, P0),
        p0_life,
        "the upkeep controller is not damaged"
    );
    assert_eq!(
        life(&runner, P1),
        p1_life - 3,
        "the accepting player takes the top card's mana value"
    );
    assert_eq!(
        life(&runner, PlayerId(2)),
        p2_life,
        "the other opponent is not damaged"
    );
    assert_eq!(runner.state().objects[&revealed].zone, Zone::Graveyard);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                GameEvent::DamageDealt { source_id, .. } if *source_id == prodder
            ))
            .count(),
        1,
        "Sin Prodder deals damage to exactly the accepting player: {events:#?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: engine::types::ability::TargetRef::Player(target),
                amount: 3,
                is_combat: false,
                ..
            } if *source_id == prodder && *target == P1
        )),
        "Sin Prodder must be the source of exactly the mana-value damage to P1: {events:#?}"
    );
}

/// CR 603.3d + CR 118.12 + CR 608.2c: if each opponent declines in turn,
/// the revealed card goes to its owner's hand and no damage is dealt.
#[test]
fn sin_prodder_upkeep_prompts_successive_opponents_when_all_decline() {
    let (mut runner, prodder, revealed) = sin_prodder_scenario();
    let initial_life = [
        life(&runner, P0),
        life(&runner, P1),
        life(&runner, PlayerId(2)),
    ];

    runner.auto_advance_to_main_phase();
    reach_sin_prodder_choice(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OpponentMayChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P1 declines Sin Prodder's public choice");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OpponentMayChoice {
                player: PlayerId(2),
                ..
            }
        ),
        "after P1 declines, the next opponent must receive the choice: {:?}",
        runner.state().waiting_for
    );
    let declined = runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("P2 declines Sin Prodder's public choice");
    let mut events = declined.events;
    drain_stack(&mut runner, &mut events);

    assert_eq!(runner.state().objects[&revealed].zone, Zone::Hand);
    assert_eq!(
        [
            life(&runner, P0),
            life(&runner, P1),
            life(&runner, PlayerId(2))
        ],
        initial_life
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt { source_id, .. } if *source_id == prodder
        )),
        "no player declines into damage: {events:#?}"
    );
}

/// CR 508.1 + CR 603.12 + CR 603.3d + CR 118.12a + CR 608.2c + CR 120.1:
/// two public attack declarations put four quest counters on Craig Boone.
/// Accepting the unless alternative must make Craig deal that primary amount
/// to the chosen creature's controller.
#[test]
fn craig_boone_second_attack_acceptance_uses_quest_counter_amount() {
    let (mut runner, craig, partner, target) = craig_after_first_attack();
    let p1_life = life(&runner, P1);

    declare_two_attackers(&mut runner, craig, partner);
    let (events, saw_target_selection, saw_unless) =
        resolve_craig_trigger(&mut runner, Some(target), Some(true));

    assert!(saw_target_selection, "the trigger reaches target selection");
    assert!(
        saw_unless,
        "the chosen creature's controller receives the unless prompt"
    );
    assert_eq!(quest_counters(&runner, craig), 4);
    assert_eq!(life(&runner, P1), p1_life - 4);
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(player),
                amount: 4,
                is_combat: false,
                ..
            } if *source_id == craig && *player == P1
        )),
        "Craig must deal four noncombat damage to the target's controller: {events:#?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(object),
                ..
            } if *source_id == craig && *object == target
        )),
        "accepting the alternative prevents primary damage to the creature: {events:#?}"
    );
}

/// CR 508.1 + CR 603.12 + CR 118.12a + CR 608.2c + CR 120.1: declining the
/// same alternative leaves the primary damage on the chosen creature, at the
/// same four-counter amount.
#[test]
fn craig_boone_second_attack_decline_uses_quest_counter_amount_on_creature() {
    let (mut runner, craig, partner, target) = craig_after_first_attack();
    let p1_life = life(&runner, P1);

    declare_two_attackers(&mut runner, craig, partner);
    let (events, saw_target_selection, saw_unless) =
        resolve_craig_trigger(&mut runner, Some(target), Some(false));

    assert!(saw_target_selection, "the trigger reaches target selection");
    assert!(
        saw_unless,
        "the chosen creature's controller receives the unless prompt"
    );
    assert_eq!(quest_counters(&runner, craig), 4);
    assert_eq!(
        life(&runner, P1),
        p1_life,
        "declining does not damage the controller"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Object(object),
                amount: 4,
                is_combat: false,
                ..
            } if *source_id == craig && *object == target
        )),
        "Craig must deal four noncombat damage to the chosen creature: {events:#?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(_),
                ..
            } if *source_id == craig
        )),
        "declining the alternative must not damage a player: {events:#?}"
    );
}

/// CR 508.1 + CR 603.12 + CR 608.2c: declining the optional target on the
/// second reflexive trigger resolves neither damage branch.
#[test]
fn craig_boone_second_attack_with_no_target_deals_no_damage() {
    let (mut runner, craig, partner, _) = craig_after_first_attack();
    let p0_life = life(&runner, P0);
    let p1_life = life(&runner, P1);

    declare_two_attackers(&mut runner, craig, partner);
    let (events, saw_target_selection, saw_unless) = resolve_craig_trigger(&mut runner, None, None);

    assert!(
        saw_target_selection,
        "the trigger reaches optional target selection before declining the target"
    );
    assert!(!saw_unless, "no target means no unless cost prompt");
    assert_eq!(quest_counters(&runner, craig), 4);
    assert_eq!(life(&runner, P0), p0_life);
    assert_eq!(life(&runner, P1), p1_life);
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt { source_id, .. } if *source_id == craig
        )),
        "neither branch deals damage without a target: {events:#?}"
    );
}

fn craig_after_first_attack() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(2, 0xC2_A19);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Craig's draw"]);
    scenario.with_library_top(P1, &["Opponent's draw"]);
    let craig = scenario
        .add_creature_from_oracle(P0, "Craig Boone", 2, 2, CRAIG_BOONE_ORACLE)
        .id();
    let partner = scenario.add_creature(P0, "Attack Partner", 2, 2).id();
    let target = scenario.add_creature(P1, "Durable Target", 0, 12).id();
    let mut runner = scenario.build();

    declare_two_attackers(&mut runner, craig, partner);
    let (events, saw_target_selection, saw_unless) = resolve_craig_trigger(&mut runner, None, None);
    assert!(
        saw_target_selection,
        "the first reflexive trigger reaches optional target selection"
    );
    assert!(
        !saw_unless,
        "the first reflexive trigger deliberately has no target"
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        GameEvent::DamageDealt { source_id, .. } if *source_id == craig
    )));
    assert_eq!(quest_counters(&runner, craig), 2);
    advance_to_next_p0_precombat(&mut runner);
    (runner, craig, partner, target)
}

fn declare_two_attackers(runner: &mut GameRunner, craig: ObjectId, partner: ObjectId) {
    reach_attack_declaration(runner);
    runner
        .declare_attackers(&[
            (craig, AttackTarget::Player(P1)),
            (partner, AttackTarget::Player(P1)),
        ])
        .expect("the two-creature attack declaration should succeed");
}

fn reach_attack_declaration(runner: &mut GameRunner) {
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(player, P0, "Craig Boone's controller declares attackers");
                return;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to declare attackers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers before attackers");
            }
            other => panic!("unexpected prompt before attacker declaration: {other:?}"),
        }
    }
    panic!(
        "did not reach attacker declaration: {:?}",
        runner.state().waiting_for
    );
}

fn resolve_craig_trigger(
    runner: &mut GameRunner,
    target: Option<ObjectId>,
    unless_pay: Option<bool>,
) -> (Vec<GameEvent>, bool, bool) {
    let mut events = Vec::new();
    let mut saw_target_selection = false;
    let mut saw_unless = false;
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                saw_target_selection = true;
                record_action(
                    runner,
                    GameAction::ChooseTarget {
                        target: target.map(TargetRef::Object),
                    },
                    &mut events,
                );
            }
            WaitingFor::UnlessPayment { player, .. } => {
                assert_eq!(
                    player, P1,
                    "the chosen creature's controller pays or declines"
                );
                saw_unless = true;
                record_action(
                    runner,
                    GameAction::PayUnlessCost {
                        pay: unless_pay.expect("the test specifies an unless decision"),
                    },
                    &mut events,
                );
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                return (events, saw_target_selection, saw_unless);
            }
            WaitingFor::Priority { .. } => {
                record_action(runner, GameAction::PassPriority, &mut events);
            }
            WaitingFor::OrderTriggers { triggers, .. } => record_action(
                runner,
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
                &mut events,
            ),
            other => panic!("unexpected prompt resolving Craig Boone: {other:?}"),
        }
    }
    panic!(
        "Craig Boone's trigger did not finish: {:?}",
        runner.state().waiting_for
    );
}

fn advance_to_next_p0_precombat(runner: &mut GameRunner) {
    let starting_turn = runner.state().turn_number;
    for _ in 0..240 {
        if runner.state().turn_number > starting_turn
            && runner.state().active_player == P0
            && runner.state().phase == Phase::PreCombatMain
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority through the remaining turn");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .declare_attackers(&[])
                    .expect("opponent declares no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("no blockers are declared");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers while advancing the turn");
            }
            other => panic!("unexpected prompt advancing to Craig's next turn: {other:?}"),
        }
    }
    panic!(
        "did not reach Craig's next precombat main: {:?}",
        runner.state().waiting_for
    );
}

fn reach_sin_prodder_choice(runner: &mut GameRunner) {
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OpponentMayChoice { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("Sin Prodder's upkeep trigger did not reach its choice")
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Sin Prodder's upkeep trigger");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order upkeep triggers before resolving Sin Prodder");
            }
            other => panic!("unexpected prompt resolving Sin Prodder: {other:?}"),
        }
    }
    panic!(
        "Sin Prodder's upkeep trigger did not reach its choice: {:?}",
        runner.state().waiting_for
    );
}

fn quest_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Generic("quest".to_string()))
        .copied()
        .unwrap_or(0)
}

fn sin_prodder_scenario() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 0x51_9D);
    scenario.at_phase(Phase::Untap);
    let prodder = scenario
        .add_creature_from_oracle(P0, "Sin Prodder", 2, 2, SIN_PRODDER_ORACLE)
        .id();
    scenario.add_spell_to_library_top(P0, "Under the Top Card", false);
    let revealed = scenario
        .add_spell_to_library_top(P0, "Three Mana Value", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    (scenario.build(), prodder, revealed)
}

fn drain_stack(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => record_action(runner, GameAction::PassPriority, events),
            WaitingFor::OrderTriggers { triggers, .. } => record_action(
                runner,
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                },
                events,
            ),
            other => panic!("unexpected prompt while draining resolved trigger: {other:?}"),
        }
    }
    panic!(
        "trigger stack did not finish: {:?}",
        runner.state().waiting_for
    );
}

fn record_action(runner: &mut GameRunner, action: GameAction, events: &mut Vec<GameEvent>) {
    let result = runner
        .act(action)
        .expect("public game action should succeed");
    events.extend(result.events);
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}
