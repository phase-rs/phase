use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, StackEntry, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{parse_mtgjson_mana_cost, ManaColor};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const RADGULL_ORACLE: &str = "Flying\nWhenever this creature deals combat damage to a player, that player gets two rad counters if they don't have any rad counters. Otherwise, proliferate. (Choose any number of permanents and/or players, then give each another counter of each kind already there.)";

enum RadgullSource {
    Export,
    Oracle,
}

struct Board {
    runner: GameRunner,
    gull: ObjectId,
    hawk: ObjectId,
    selected: ObjectId,
    untouched: ObjectId,
}

fn board_with<T>(
    damaged_rad: u32,
    other_rad: u32,
    setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> T,
) -> (Board, T) {
    board_with_source(damaged_rad, other_rad, RadgullSource::Export, setup)
}

fn board_with_source<T>(
    damaged_rad: u32,
    other_rad: u32,
    source: RadgullSource,
    setup: impl FnOnce(&mut GameScenario, &CardDatabase) -> T,
) -> (Board, T) {
    let db = shared_card_db().expect("printed integration card database");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let gull = match source {
        RadgullSource::Export => {
            scenario.add_real_card(P0, "Vexing Radgull", Zone::Battlefield, db)
        }
        RadgullSource::Oracle => scenario
            .add_creature(P0, "Vexing Radgull", 1, 2)
            .with_subtypes(vec!["Bird", "Mutant"])
            .with_mana_cost(parse_mtgjson_mana_cost("{1}{U}"))
            .from_oracle_text_with_keywords(&["Flying"], RADGULL_ORACLE)
            .id(),
    };
    let hawk = scenario.add_real_card(P2, "Suntail Hawk", Zone::Battlefield, db);
    let selected = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let untouched = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    for object in [selected, untouched] {
        scenario.with_counter(object, CounterType::Plus1Plus1, 1);
        scenario.with_counter(object, CounterType::Stun, 2);
    }
    let extra = setup(&mut scenario, db);
    let mut runner = scenario.build();
    for (player, rad) in [(P0, other_rad), (P1, other_rad), (P2, damaged_rad)] {
        runner.state_mut().players[player.0 as usize]
            .add_player_counters(&PlayerCounterKind::Rad, rad);
    }
    runner.state_mut().players[1].add_player_counters(&PlayerCounterKind::Poison, 3);
    runner.state_mut().players[1].add_player_counters(&PlayerCounterKind::Experience, 2);
    runner.state_mut().players[2].add_player_counters(&PlayerCounterKind::Poison, 1);
    (
        Board {
            runner,
            gull,
            hawk,
            selected,
            untouched,
        },
        extra,
    )
}

fn board(damaged_rad: u32, other_rad: u32) -> Board {
    board_with(damaged_rad, other_rad, |_, _| ()).0
}

fn rad(runner: &GameRunner) -> [u32; 3] {
    [P0, P1, P2].map(|player| {
        runner.state().players[player.0 as usize].player_counter(&PlayerCounterKind::Rad)
    })
}

fn damage(events: &[GameEvent], gull: ObjectId, recipient: TargetRef) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::DamageDealt {
        source_id, target, amount: 1, is_combat: true, excess: _, source_incarnation: _,
    } if *source_id == gull && *target == recipient)
        })
        .count()
}

// Stop on the damage action, before any post-damage priority pass.
fn combat(
    runner: &mut GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
    block: Option<(ObjectId, ObjectId)>,
) -> Vec<GameEvent> {
    let before = rad(runner);
    runner.advance_to_combat();
    runner
        .declare_attackers(attacks)
        .expect("declare the printed attackers");
    assert!(
        runner.state().stack.is_empty(),
        "attack declaration is not Radgull's trigger"
    );
    assert_eq!(rad(runner), before);
    for _ in 0..24 {
        let result = match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { player, .. } => {
                let assignment = if player == P2 { block.as_slice() } else { &[] };
                runner.declare_blockers(assignment)
            }
            WaitingFor::Priority { .. } => {
                assert!(runner.state().stack.is_empty(), "no trigger before damage");
                runner.act(GameAction::PassPriority)
            }
            ref other => panic!("unexpected combat prompt: {other:?}"),
        }
        .expect("drive the declared attack to combat damage");
        if result.events.iter().any(|event| {
            matches!(
                event,
                GameEvent::DamageDealt {
                    is_combat: true,
                    ..
                }
            )
        }) {
            assert_eq!(runner.state().phase, Phase::CombatDamage);
            assert_eq!(rad(runner), before);
            return result.events;
        }
    }
    panic!(
        "combat did not reach damage: {:?}",
        runner.state().waiting_for
    );
}

fn attack(board: &mut Board, block: Option<(ObjectId, ObjectId)>) -> Vec<GameEvent> {
    combat(
        &mut board.runner,
        &[(board.gull, AttackTarget::Player(P2))],
        block,
    )
}

fn assert_event_binding(entry: &StackEntry, events: &[GameEvent], gull: ObjectId, seat: PlayerId) {
    assert_eq!((entry.source_id, entry.controller), (gull, P0));
    let StackEntryKind::TriggeredAbility {
        ability,
        condition,
        trigger_event,
        ..
    } = &entry.kind
    else {
        panic!("expected pending damage trigger: {entry:?}");
    };
    assert!(
        ability.targets.is_empty(),
        "the event recipient is not a declared target"
    );
    assert!(condition.is_none(), "the postfix if is not intervening");
    let event = trigger_event
        .as_ref()
        .expect("stack entry retains its damage event");
    assert!(events.contains(event));
    assert_eq!(
        damage(std::slice::from_ref(event), gull, TargetRef::Player(seat)),
        1
    );
}

fn assert_trigger(board: &Board, events: &[GameEvent], before: [u32; 3]) -> ObjectId {
    let runner = &board.runner;
    assert_eq!(damage(events, board.gull, TargetRef::Player(P2)), 1);
    assert_eq!(
        [runner.life(P0), runner.life(P1), runner.life(P2)],
        [20, 20, 19]
    );
    assert_eq!(
        rad(runner),
        before,
        "no counters before the ability resolves"
    );
    assert_eq!(runner.state().stack.len(), 1);
    let trigger = runner
        .state()
        .stack
        .back()
        .expect("combat damage trigger on stack");
    assert_event_binding(trigger, events, board.gull, P2);
    trigger.id
}

fn finish(
    runner: &mut GameRunner,
    id: ObjectId,
    selections: &[TargetRef],
) -> (usize, Vec<GameEvent>) {
    assert_eq!(
        runner.state().stack.back().expect("pending stack entry").id,
        id
    );
    let mut choices = 0;
    let mut events = Vec::new();
    for _ in 0..12 {
        assert_eq!(
            runner.state().phase,
            Phase::CombatDamage,
            "stop in combat before rad processing"
        );
        let action = match &runner.state().waiting_for {
            WaitingFor::ProliferateChoice { player, eligible } => {
                assert_eq!(*player, P0, "ability controller makes the choice");
                assert!(
                    !selections.is_empty()
                        && selections.iter().all(|target| eligible.contains(target))
                );
                choices += 1;
                GameAction::SelectTargets {
                    targets: selections.to_vec(),
                }
            }
            WaitingFor::Priority { .. }
                if runner
                    .state()
                    .stack
                    .back()
                    .is_none_or(|entry| entry.id != id) =>
            {
                assert!(runner.state().stack.iter().all(|entry| entry.id != id));
                return (choices, events);
            }
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            other => panic!("unexpected resolution prompt: {other:?}"),
        };
        events.extend(
            runner
                .act(action)
                .expect("resolve the current stack entry")
                .events,
        );
    }
    panic!("stack entry {id:?} did not finish");
}

fn rad_changes(events: &[GameEvent]) -> Vec<(PlayerId, i32)> {
    events
        .iter()
        .filter_map(|event| {
            if let GameEvent::PlayerCounterChanged {
                player,
                counter_kind: PlayerCounterKind::Rad,
                delta,
            } = event
            {
                Some((*player, *delta))
            } else {
                None
            }
        })
        .collect()
}

fn assert_collateral(board: &Board, player_increment: u32, object_increment: u32) {
    let state = board.runner.state();
    assert_eq!(
        state.players[0].player_counter(&PlayerCounterKind::Poison),
        0
    );
    assert_eq!(
        state.players[1].player_counter(&PlayerCounterKind::Poison),
        3 + player_increment
    );
    assert_eq!(
        state.players[1].player_counter(&PlayerCounterKind::Experience),
        2 + player_increment
    );
    assert_eq!(
        state.players[2].player_counter(&PlayerCounterKind::Poison),
        1
    );
    for (object, delta) in [(board.selected, object_increment), (board.untouched, 0)] {
        let counters = &state.objects[&object].counters;
        assert_eq!(counters.get(&CounterType::Plus1Plus1), Some(&(1 + delta)));
        assert_eq!(counters.get(&CounterType::Stun), Some(&(2 + delta)));
    }
}

#[test]
fn a_flying_blocker_takes_the_damage_instead_of_the_player() {
    let mut board = board(0, 3);
    let block = (board.hawk, board.gull);
    let events = attack(&mut board, Some(block));
    assert_eq!(
        damage(&events, board.gull, TargetRef::Object(board.hawk)),
        1
    );
    assert_eq!(damage(&events, board.gull, TargetRef::Player(P2)), 0);
    assert_eq!(
        [
            board.runner.life(P0),
            board.runner.life(P1),
            board.runner.life(P2)
        ],
        [20; 3]
    );
    assert!(board.runner.state().stack.is_empty());
    assert!(matches!(
        board.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(rad_changes(&events).is_empty());
    assert_eq!(rad(&board.runner), [3, 3, 0]);
    assert_collateral(&board, 0, 0);
}

#[test]
fn give_two_rad_only_when_the_damaged_player_has_none() {
    let mut board = board(0, 3);
    assert_zero_branch(&mut board);
}

#[test]
fn live_oracle_parser_drives_the_zero_rad_branch() {
    let (mut board, ()) = board_with_source(0, 3, RadgullSource::Oracle, |_, _| ());
    assert_zero_branch(&mut board);
}

fn assert_zero_branch(board: &mut Board) {
    let events = attack(board, None);
    let id = assert_trigger(board, &events, [3, 3, 0]);
    let selections = [TargetRef::Player(P1), TargetRef::Object(board.selected)];
    let (prompts, events) = finish(&mut board.runner, id, &selections);
    assert_eq!((rad(&board.runner), prompts), ([3, 3, 2], 0));
    assert_eq!(rad_changes(&events), [(P2, 2)]);
    assert_collateral(board, 0, 0);
}

#[test]
fn proliferate_only_when_the_damaged_player_already_has_rad() {
    let mut board = board(4, 0);
    let events = attack(&mut board, None);
    let id = assert_trigger(&board, &events, [0, 0, 4]);
    let selections = [TargetRef::Player(P1), TargetRef::Object(board.selected)];
    let (prompts, events) = finish(&mut board.runner, id, &selections);
    assert_eq!((rad(&board.runner), prompts), ([0, 0, 4], 1));
    assert!(rad_changes(&events).is_empty());
    assert_collateral(&board, 1, 1);
}

#[test]
fn the_pending_trigger_reads_rad_after_a_ghoul_dies_in_response() {
    let (mut board, (ghoul, bolt, mountain)) = board_with(0, 0, |scenario, db| {
        let ghoul = scenario.add_real_card(P0, "Feral Ghoul", Zone::Battlefield, db);
        let bolt = scenario.add_real_card(P0, "Lightning Bolt", Zone::Hand, db);
        let mountain = scenario.add_basic_land(P0, ManaColor::Red);
        (ghoul, bolt, mountain)
    });
    let combat_events = attack(&mut board, None);
    let gull_trigger = assert_trigger(&board, &combat_events, [0, 0, 0]);
    let runner = &mut board.runner;
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    runner
        .act(GameAction::ActivateAbility {
            source_id: mountain,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("tap Mountain for red in response");

    let bolt_entry = {
        let committed = runner.cast(bolt).target_object(ghoul).commit();
        assert_eq!(committed.state().stack.len(), 2);
        assert_eq!(committed.state().stack.front().unwrap().id, gull_trigger);
        assert!(committed.events().iter().any(|event| matches!(
            event,
            GameEvent::ManaExpended {
                player_id: P0,
                amount_spent: 1,
                new_cumulative: 1,
            }
        )));
        let entry = committed.state().stack.back().unwrap();
        assert_eq!((entry.source_id, entry.controller), (bolt, P0));
        entry.id
    };
    let (_, events) = finish(runner, bolt_entry, &[]);
    assert!(events
        .iter()
        .any(|event| matches!(event, GameEvent::DamageDealt {
        source_id, target: TargetRef::Object(target), amount: 3, is_combat: false, ..
    } if *source_id == bolt && *target == ghoul)));
    assert_eq!(runner.state().objects[&ghoul].zone, Zone::Graveyard);
    assert_eq!(rad(runner), [0, 0, 0], "the death trigger is still pending");
    let ghoul_trigger = runner.state().stack.back().unwrap();
    assert_eq!(
        (ghoul_trigger.source_id, ghoul_trigger.controller),
        (ghoul, P0)
    );
    let (_, events) = finish(runner, ghoul_trigger.id, &[]);
    let mut changes = rad_changes(&events);
    changes.sort_unstable_by_key(|(seat, _)| seat.0);
    assert_eq!(changes, [(P1, 2), (P2, 2)]);
    assert_eq!(rad(runner), [0, 2, 2]);
    let pending = runner.state().stack.back().unwrap();
    assert_eq!(pending.id, gull_trigger);
    assert_event_binding(pending, &combat_events, board.gull, P2);

    let (prompts, events) = finish(runner, gull_trigger, &[TargetRef::Object(board.selected)]);
    assert_eq!((rad(runner), prompts), ([0, 2, 2], 1));
    assert!(rad_changes(&events).is_empty());
    assert_collateral(&board, 0, 1);
}

#[test]
fn simultaneous_attackers_bind_their_own_damaged_seats() {
    let (mut board, second_gull) = board_with(0, 3, |scenario, db| {
        scenario.add_real_card(P1, "Suntail Hawk", Zone::Battlefield, db);
        scenario.add_real_card(P0, "Vexing Radgull", Zone::Battlefield, db)
    });
    let runner = &mut board.runner;
    let events = combat(
        runner,
        &[
            (board.gull, AttackTarget::Player(P2)),
            (second_gull, AttackTarget::Player(P1)),
        ],
        None,
    );
    assert_eq!(
        [runner.life(P0), runner.life(P1), runner.life(P2)],
        [20, 19, 19]
    );
    assert_eq!(rad(runner), [3, 3, 0]);
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!("distinct damage recipients require a trigger ordering choice");
    };
    assert_eq!(*player, P0);
    let [first, second] = triggers.as_slice() else {
        panic!("expected exactly two triggers in the ordering choice");
    };
    let (top, bottom) = (first.source_id, second.source_id);
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .expect("reverse the offered trigger order");
    assert_eq!(runner.state().stack.len(), 2);
    assert_eq!(runner.state().stack.back().unwrap().source_id, top);
    assert_eq!(runner.state().stack.front().unwrap().source_id, bottom);
    let ids = [(board.gull, P2), (second_gull, P1)].map(|(source, seat)| {
        let entry = runner
            .state()
            .stack
            .iter()
            .find(|entry| entry.source_id == source)
            .unwrap();
        assert_event_binding(entry, &events, source, seat);
        entry.id
    });
    assert_ne!(ids[0], ids[1]);
    let stack_order = [
        runner.state().stack.back().unwrap().id,
        runner.state().stack.front().unwrap().id,
    ];
    let mut expected_rad = [3, 3, 0];
    let object_only = [TargetRef::Object(board.selected)];
    let player_and_object = [TargetRef::Player(P2), TargetRef::Object(board.selected)];
    for id in stack_order {
        let selections = if id == ids[1] && expected_rad[2] == 2 {
            player_and_object.as_slice()
        } else {
            object_only.as_slice()
        };
        let (prompts, events) = finish(runner, id, selections);
        if id == ids[0] {
            expected_rad[2] = 2;
            assert_eq!(prompts, 0);
            assert_eq!(rad_changes(&events), [(P2, 2)]);
        } else {
            assert_eq!(id, ids[1]);
            assert_eq!(prompts, 1);
            if expected_rad[2] == 2 {
                expected_rad[2] = 3;
                assert_eq!(rad_changes(&events), [(P2, 1)]);
            } else {
                assert!(rad_changes(&events).is_empty());
            }
        }
        assert_eq!(rad(runner), expected_rad);
    }
    assert_eq!(
        runner.state().players[2].player_counter(&PlayerCounterKind::Poison),
        if expected_rad[2] == 3 { 2 } else { 1 }
    );
    let counters = &runner.state().objects;
    assert_eq!(counters[&board.selected].counters[&CounterType::Stun], 3);
    assert_eq!(counters[&board.untouched].counters[&CounterType::Stun], 2);
}

#[test]
fn both_pending_triggers_read_the_departed_players_last_rad_total() {
    for before in [0, 4] {
        let (mut board, second_gull) = board_with(before, 0, |scenario, db| {
            scenario.add_real_card(P0, "Vexing Radgull", Zone::Battlefield, db)
        });
        let runner = &mut board.runner;
        let events = combat(
            runner,
            &[
                (board.gull, AttackTarget::Player(P2)),
                (second_gull, AttackTarget::Player(P2)),
            ],
            None,
        );
        assert_eq!(runner.life(P2), 18);
        if let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for {
            assert_eq!(*player, P0);
            assert_eq!(triggers.len(), 2);
            runner
                .act(GameAction::OrderTriggers { order: vec![0, 1] })
                .expect("order both pending damage triggers before the concession");
        }
        assert_eq!(runner.state().stack.len(), 2);
        for source in [board.gull, second_gull] {
            let entry = runner
                .state()
                .stack
                .iter()
                .find(|entry| entry.source_id == source)
                .expect("independent printed source");
            assert_event_binding(entry, &events, source, P2);
        }
        let pending = [
            runner.state().stack.back().unwrap().id,
            runner.state().stack.front().unwrap().id,
        ];
        assert_eq!(rad(runner), [0, 0, before]);
        runner
            .act(GameAction::Concede { player_id: P2 })
            .expect("damaged player concedes");
        assert!(runner.state().players[2].is_eliminated);
        assert_eq!(
            runner.state().stack.len(),
            2,
            "P0 still controls both triggers"
        );
        let expected_choices = usize::from(before > 0);
        for id in pending {
            let (choices, events) = finish(runner, id, &[TargetRef::Object(board.selected)]);
            assert_eq!(
                rad(runner),
                [0, 0, before],
                "CR 800.4i: departure freezes P2's counter information"
            );
            assert_eq!(
                choices, expected_choices,
                "branch reads the departed seat's last rad total"
            );
            assert!(rad_changes(&events).is_empty());
        }
        assert_collateral(&board, 0, (expected_choices as u32) * 2);
    }
}

#[test]
fn solemnity_prevents_the_grant_without_opening_otherwise() {
    let (mut board, solemnity) = board_with(0, 3, |scenario, db| {
        scenario.add_real_card(P0, "Solemnity", Zone::Battlefield, db)
    });
    let events = attack(&mut board, None);
    let id = assert_trigger(&board, &events, [3, 3, 0]);
    let selections = [TargetRef::Player(P1), TargetRef::Object(board.selected)];
    let (prompts, events) = finish(&mut board.runner, id, &selections);
    assert_eq!(
        board.runner.state().objects[&solemnity].zone,
        Zone::Battlefield
    );
    assert_eq!((rad(&board.runner), prompts), ([3, 3, 0], 0));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, GameEvent::EffectResolved {
        kind: EffectKind::GivePlayerCounter, source_id, ..
    } if *source_id == board.gull)),
        "the zero-rad branch was selected despite Solemnity"
    );
    assert!(rad_changes(&events).is_empty());
    assert_collateral(&board, 0, 0);
}
