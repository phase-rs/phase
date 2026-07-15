//! `DeclareBlockers` prompts expose blocker IDs in stable numeric order.
//!
//! These tests drive each production producer: initial step entry, the
//! multiplayer transition to the next defender, and a mid-prompt debug refresh.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

fn sorted_ids(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort_unstable_by_key(|id| id.0);
    ids
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
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

fn drive_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare blockers");
            }
            ref other => panic!("expected priority or declare blockers, got {other:?}"),
        }
    }
    panic!("did not reach declare blockers");
}

fn declare_attacks(runner: &mut GameRunner, attacks: Vec<(ObjectId, AttackTarget)>) {
    drive_to_declare_attackers(runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks,
            bands: vec![],
        })
        .expect("declaring attackers should succeed");
    drive_to_declare_blockers(runner);
}

fn assert_blocker_prompt(
    runner: &GameRunner,
    expected_player: PlayerId,
    expected_ids: &[ObjectId],
    expected_attacker: ObjectId,
) {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            player,
            valid_blocker_ids,
            valid_block_targets,
            ..
        } => {
            assert_eq!(*player, expected_player, "prompted defending player");
            assert_eq!(
                valid_blocker_ids, expected_ids,
                "valid blocker IDs must be complete and numerically ordered"
            );
            assert_eq!(
                valid_block_targets.len(),
                expected_ids.len(),
                "the ordered ID list and target map must describe the same blockers"
            );
            for blocker_id in expected_ids {
                assert_eq!(
                    valid_block_targets.get(blocker_id),
                    Some(&vec![expected_attacker]),
                    "each expected blocker must retain its player-scoped target"
                );
            }
        }
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

#[test]
fn initial_blocker_prompt_orders_all_valid_ids_numerically() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blockers = (0..8)
        .map(|index| {
            scenario
                .add_creature(P1, &format!("Blocker {index}"), 1, 1)
                .id()
        })
        .collect::<Vec<_>>();
    let expected = sorted_ids(blockers);

    let mut runner = scenario.build();
    declare_attacks(&mut runner, vec![(attacker, AttackTarget::Player(P1))]);

    assert_eq!(
        expected.len(),
        8,
        "fixture must exercise at least eight keys"
    );
    assert_blocker_prompt(&runner, P1, &expected, attacker);
}

#[test]
fn multiplayer_blocker_prompts_order_each_defenders_scoped_ids() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_to_p1 = scenario.add_creature(P0, "Attacker to P1", 2, 2).id();
    let attacker_to_p2 = scenario.add_creature(P0, "Attacker to P2", 2, 2).id();

    let mut p1_blockers = Vec::new();
    let mut p2_blockers = Vec::new();
    for index in 0..8 {
        p2_blockers.push(
            scenario
                .add_creature(P2, &format!("P2 Blocker {index}"), 1, 1)
                .id(),
        );
        p1_blockers.push(
            scenario
                .add_creature(P1, &format!("P1 Blocker {index}"), 1, 1)
                .id(),
        );
    }
    let expected_p1 = sorted_ids(p1_blockers);
    let expected_p2 = sorted_ids(p2_blockers);

    let mut runner = scenario.build();
    declare_attacks(
        &mut runner,
        vec![
            (attacker_to_p1, AttackTarget::Player(P1)),
            (attacker_to_p2, AttackTarget::Player(P2)),
        ],
    );

    assert_blocker_prompt(&runner, P1, &expected_p1, attacker_to_p1);
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![],
        })
        .expect("P1 declaring no blockers should advance to P2");
    assert_blocker_prompt(&runner, P2, &expected_p2, attacker_to_p2);
}

#[test]
fn debug_tap_refresh_preserves_numeric_order_for_remaining_blockers() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blockers = (0..8)
        .map(|index| {
            scenario
                .add_creature(P1, &format!("Refresh Blocker {index}"), 1, 1)
                .id()
        })
        .collect::<Vec<_>>();
    let expected_initial = sorted_ids(blockers);
    let tapped_blocker = expected_initial[3];

    let mut runner = scenario.build();
    declare_attacks(&mut runner, vec![(attacker, AttackTarget::Player(P1))]);
    assert_blocker_prompt(&runner, P1, &expected_initial, attacker);
    let initial_valid_blocker_ids = match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            valid_blocker_ids, ..
        } => valid_blocker_ids,
        other => panic!("expected DeclareBlockers, got {other:?}"),
    };
    assert!(
        initial_valid_blocker_ids.contains(&tapped_blocker),
        "precondition: the blocker being tapped must be present initially"
    );

    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::SetTapped {
            object_id: tapped_blocker,
            tapped: true,
        }))
        .expect("debug SetTapped should refresh the blocker prompt");

    let expected_remaining = expected_initial
        .into_iter()
        .filter(|id| *id != tapped_blocker)
        .collect::<Vec<_>>();
    assert_blocker_prompt(&runner, P1, &expected_remaining, attacker);
}
