//! Production coverage for one-or-more controller-batched damage triggers that
//! bind to each damaged player independently.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn count_treasures(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| {
            object.controller == P0
                && object.zone == Zone::Battlefield
                && object.is_token
                && object.name.eq_ignore_ascii_case("Treasure")
        })
        .count()
}

fn plus_one_counters(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object_id)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

fn drive_until_source_triggers_are_stacked(
    runner: &mut GameRunner,
    source: ObjectId,
    expected_count: usize,
) {
    for _ in 0..128 {
        let count = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count();
        if count == expected_count {
            return;
        }

        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should advance the stack");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("defender should be able to declare no blockers");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger order should be accepted");
            }
            other => panic!("unexpected wait state while stacking triggers: {other:?}"),
        }
    }
    panic!("source triggers did not reach the stack");
}

#[test]
fn professional_face_breaker_creates_one_treasure_for_each_combat_damage_player() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let face_breaker =
        scenario.add_real_card(P0, "Professional Face-Breaker", Zone::Battlefield, db);
    let attacker_p1 = scenario.add_creature(P0, "Attacker P1", 1, 1).id();
    let attacker_p2 = scenario.add_creature(P0, "Attacker P2", 1, 1).id();
    let attacker_p3 = scenario.add_creature(P0, "Attacker P3", 1, 1).id();
    let mut runner = scenario.build();
    let treasures_before = count_treasures(&runner);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (attacker_p1, AttackTarget::Player(P1)),
            (attacker_p2, AttackTarget::Player(P2)),
            (attacker_p3, AttackTarget::Player(P3)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, face_breaker, 3);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 19, "P1 took combat damage");
    assert_eq!(runner.life(P2), 19, "P2 took combat damage");
    assert_eq!(runner.life(P3), 19, "P3 took combat damage");
    assert_eq!(
        count_treasures(&runner),
        treasures_before + 3,
        "Professional Face-Breaker must fire once for every damaged player"
    );
}

#[test]
fn professional_face_breaker_batches_multiple_matching_sources_for_one_player() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let face_breaker =
        scenario.add_real_card(P0, "Professional Face-Breaker", Zone::Battlefield, db);
    let first = scenario.add_creature(P0, "First attacker", 1, 1).id();
    let second = scenario.add_creature(P0, "Second attacker", 1, 1).id();
    let mut runner = scenario.build();
    let treasures_before = count_treasures(&runner);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, face_breaker, 1);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 18, "both attackers dealt combat damage");
    assert_eq!(
        count_treasures(&runner),
        treasures_before + 1,
        "two matching creatures damaging one player remain one occurrence"
    );
}

#[test]
fn malcolm_still_creates_treasure_per_distinct_damaged_opponent() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let malcolm = scenario.add_real_card(P0, "Malcolm, Keen-Eyed Navigator", Zone::Battlefield, db);
    let pirate_p1 = scenario
        .add_creature(P0, "Pirate P1", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let pirate_p2_a = scenario
        .add_creature(P0, "Pirate P2 A", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let pirate_p2_b = scenario
        .add_creature(P0, "Pirate P2 B", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let mut runner = scenario.build();
    let treasures_before = count_treasures(&runner);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (pirate_p1, AttackTarget::Player(P1)),
            (pirate_p2_a, AttackTarget::Player(P2)),
            (pirate_p2_b, AttackTarget::Player(P2)),
        ])
        .expect("Pirates should be legal attackers");
    drive_until_source_triggers_are_stacked(&mut runner, malcolm, 1);

    runner.advance_until_stack_empty();
    assert_eq!(
        count_treasures(&runner),
        treasures_before + 2,
        "Malcolm's nonexact opponent scope must retain its aggregate behavior"
    );
}

#[test]
fn francisco_explores_once_for_each_noncombat_player_damage_recipient() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let francisco = scenario.add_real_card(P0, "Francisco, Fowl Marauder", Zone::Battlefield, db);
    let crew = scenario.add_real_card(P0, "Lightning-Rig Crew", Zone::Battlefield, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Forest", Zone::Library, db);
    }
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: crew,
            ability_index: 0,
        })
        .expect("Lightning-Rig Crew activation should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, francisco, 3);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 19, "Crew damaged P1");
    assert_eq!(runner.life(P2), 19, "Crew damaged P2");
    assert_eq!(runner.life(P3), 19, "Crew damaged P3");
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "each Francisco trigger must resolve its own explore"
    );
    assert_eq!(
        plus_one_counters(&runner, francisco),
        0,
        "the three real Forest explores use the land branch rather than a synthetic counter"
    );
}

#[test]
fn the_thing_keeps_only_hero_damage_in_its_stored_event_context() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thing = scenario.add_real_card(P0, "The Thing, Ben Grimm", Zone::Battlefield, db);
    let hero = scenario
        .add_creature(P0, "Hero attacker", 1, 1)
        .with_subtypes(vec!["Hero"])
        .id();
    let non_hero = scenario.add_creature(P0, "Non-Hero attacker", 1, 1).id();
    let mut runner = scenario.build();

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (hero, AttackTarget::Player(P1)),
            (non_hero, AttackTarget::Player(P1)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, thing, 1);

    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == thing)
        .expect("The Thing trigger should be on the production stack");
    let StackEntryKind::TriggeredAbility {
        trigger_event:
            Some(GameEvent::CombatDamageDealtToPlayer {
                player_id,
                source_amounts,
                total_damage,
            }),
        ..
    } = &entry.kind
    else {
        panic!("The Thing must retain a normalized combat-damage event, got {entry:?}");
    };
    assert_eq!(*player_id, P1);
    assert_eq!(source_amounts, &vec![(hero, 1)]);
    assert_eq!(*total_damage, 1);

    runner.advance_until_stack_empty();
    assert_eq!(
        plus_one_counters(&runner, thing),
        2,
        "The Thing resolves once from the Hero-only context"
    );
}
