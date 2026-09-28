//! `WaitingFor::DeclareBlockers.block_capacities` (CR 509.1a + CR 101.1) — the
//! prompt's per-blocker block-limit projection. Mirrors
//! `must_be_blocked_blocker_prompt.rs`'s driving helpers.
//!
//! Oracle text verified verbatim against `data/mtgjson/AtomicCards.json`:
//! Foriysian Brigade ("This creature can block an additional creature each
//! combat."), Palace Guard ("This creature can block any number of
//! creatures."), and High Ground ("Each creature you control can block an
//! additional creature each combat.").

use engine::game::combat::{validate_blockers_for_player, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use std::collections::HashMap;

const FORIYSIAN_BRIGADE: &str = "This creature can block an additional creature each combat.";
const PALACE_GUARD: &str = "This creature can block any number of creatures.";
const HIGH_GROUND: &str = "Each creature you control can block an additional creature each combat.";

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

/// Read `block_capacities` off the current `DeclareBlockers` prompt.
fn block_capacities(runner: &GameRunner) -> HashMap<ObjectId, Option<u32>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            block_capacities, ..
        } => block_capacities.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

/// Read `valid_block_targets` off the current `DeclareBlockers` prompt.
fn valid_block_targets(runner: &GameRunner) -> HashMap<ObjectId, Vec<ObjectId>> {
    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers {
            valid_block_targets,
            ..
        } => valid_block_targets.clone(),
        other => panic!("expected DeclareBlockers, got {other:?}"),
    }
}

/// The prompt publishes each able blocker's capacity, matching what the
/// declaration validator itself enforces.
#[test]
fn prompt_publishes_each_blockers_capacity() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_creature(P0, "Insect", 1, 1).id())
        .collect();
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    let guard = scenario
        .add_creature_from_oracle(P1, "Palace Guard", 1, 4, PALACE_GUARD)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    drive_to_declare_attackers(&mut runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("declaring attackers should succeed");
    drive_to_declare_blockers(&mut runner);

    let capacities = block_capacities(&runner);
    assert_eq!(
        capacities,
        HashMap::from([(brigade, Some(2)), (guard, None), (bear, Some(1)),]),
        "each able blocker's published capacity matches its ExtraBlockers grant"
    );

    let valid = valid_block_targets(&runner);
    let mut capacity_keys: Vec<_> = capacities.keys().copied().collect();
    capacity_keys.sort();
    let mut valid_keys: Vec<_> = valid.keys().copied().collect();
    valid_keys.sort();
    assert_eq!(
        capacity_keys, valid_keys,
        "reach guard: block_capacities is keyed by every able blocker, same as valid_block_targets"
    );

    let s = runner.state();
    assert!(
        validate_blockers_for_player(s, P1, &[(brigade, attackers[0]), (brigade, attackers[1])])
            .is_ok(),
        "Brigade blocking 2 is within its published capacity"
    );
    assert!(
        validate_blockers_for_player(
            s,
            P1,
            &[
                (brigade, attackers[0]),
                (brigade, attackers[1]),
                (brigade, attackers[2])
            ]
        )
        .is_err(),
        "Brigade blocking 3 exceeds its published capacity"
    );
    assert!(
        validate_blockers_for_player(
            s,
            P1,
            &[
                (guard, attackers[0]),
                (guard, attackers[1]),
                (guard, attackers[2])
            ]
        )
        .is_ok(),
        "Guard's published None capacity means any number"
    );
    assert!(
        validate_blockers_for_player(s, P1, &[(bear, attackers[0]), (bear, attackers[1])]).is_err(),
        "Bear's published Some(1) capacity means only one attacker"
    );
}

/// A mid-prompt debug refresh (producer:
/// `combat.rs::refresh_combat_declaration_waiting_for`) recomputes the map:
/// a blocker that can no longer block loses its key (its static ability is
/// still active — only the constraint surface changed), and unaffected
/// blockers keep their published capacity.
#[test]
fn mid_prompt_refresh_recomputes_it() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<ObjectId> = (0..3)
        .map(|_| scenario.add_creature(P0, "Insect", 1, 1).id())
        .collect();
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    let guard = scenario
        .add_creature_from_oracle(P1, "Palace Guard", 1, 4, PALACE_GUARD)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    drive_to_declare_attackers(&mut runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("declaring attackers should succeed");
    drive_to_declare_blockers(&mut runner);

    let before = block_capacities(&runner);
    assert_eq!(
        before.get(&brigade),
        Some(&Some(2)),
        "reach guard: Brigade starts published"
    );
    assert_eq!(before.get(&guard), Some(&None));
    assert_eq!(before.get(&bear), Some(&Some(1)));

    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::SetTapped {
            object_id: brigade,
            tapped: true,
        }))
        .expect("debug SetTapped should refresh the blocker prompt");

    let after = block_capacities(&runner);
    assert!(
        !after.contains_key(&brigade),
        "the tapped Brigade can no longer block, so its key is gone, even though its static is unaffected"
    );
    assert_eq!(
        after.get(&guard),
        Some(&None),
        "Guard's capacity is unaffected"
    );
    assert_eq!(
        after.get(&bear),
        Some(&Some(1)),
        "Bear's capacity is unaffected"
    );
}

/// High Ground's own ruling: "High Ground's effect is cumulative. If you have
/// a creature that can already block an additional creature, now it can
/// block three creatures." With Foriysian Brigade's own grant plus High
/// Ground's group grant, the Brigade's published capacity is 3 (1 base + 1 +
/// 1), not 2 (highest-wins would stop at the higher single grant).
#[test]
fn high_ground_stacks_cumulatively_with_a_creatures_own_grant() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attackers: Vec<ObjectId> = (0..4)
        .map(|_| scenario.add_creature(P0, "Insect", 1, 1).id())
        .collect();
    let brigade = scenario
        .add_creature_from_oracle(P1, "Foriysian Brigade", 2, 4, FORIYSIAN_BRIGADE)
        .id();
    scenario.add_enchantment_from_oracle(P1, "High Ground", HIGH_GROUND);

    let mut runner = scenario.build();
    drive_to_declare_attackers(&mut runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: attackers
                .iter()
                .map(|id| (*id, AttackTarget::Player(P1)))
                .collect(),
            bands: vec![],
        })
        .expect("declaring attackers should succeed");
    drive_to_declare_blockers(&mut runner);

    let capacities = block_capacities(&runner);
    assert_eq!(
        capacities.get(&brigade),
        Some(&Some(3)),
        "Brigade's own grant and High Ground's group grant are cumulative, not highest-wins"
    );

    let s = runner.state();
    assert!(
        validate_blockers_for_player(
            s,
            P1,
            &[
                (brigade, attackers[0]),
                (brigade, attackers[1]),
                (brigade, attackers[2])
            ]
        )
        .is_ok(),
        "Brigade blocking 3 attackers is within its published cumulative capacity"
    );
    assert!(
        validate_blockers_for_player(
            s,
            P1,
            &[
                (brigade, attackers[0]),
                (brigade, attackers[1]),
                (brigade, attackers[2]),
                (brigade, attackers[3])
            ]
        )
        .is_err(),
        "Brigade blocking 4 attackers exceeds its published cumulative capacity"
    );
}
