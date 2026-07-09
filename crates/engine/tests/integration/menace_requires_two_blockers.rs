//! Menace blocking restriction — a creature with menace can only be blocked
//! by two or more creatures simultaneously (CR 702.111b).
//!
//! When a menace attacker faces a defender with only a single blocker
//! assignment, the engine must reject that declaration: one creature alone
//! cannot legally block a menace creature.
//!
//! This regression pins two complementary behaviors:
//!   1. With one declared blocker: the declaration is rejected, then the
//!      attacker goes through unblocked (dealing combat damage to the defending
//!      player).
//!   2. With two potential blockers: both creatures appear in
//!      `valid_block_targets` with the menace attacker listed, so the
//!      defender can legally block with both; the menace creature is blocked
//!      and deals no damage to the player.
//!
//! CR 702.111a: "Menace is an evasion ability."
//! CR 702.111b: "A creature with menace can't be blocked except by two or
//!   more creatures."
//! CR 509.1b: blocking declarations that disobey restrictions are illegal.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CombatDamageAssignmentMode, WaitingFor};
use engine::types::phase::Phase;

/// CR 702.111b: with only a single potential blocker on the battlefield, trying
/// to assign that lone creature to block the menace attacker must be rejected
/// by the engine (menace requires two or more simultaneous blockers). After the
/// illegal assignment is rejected, declaring empty blockers allows the attacker
/// to go through unblocked and deal combat damage to the defending player.
///
/// Discriminating: reverting menace-block validation would allow the illegal
/// single-creature block — the `DeclareBlockers` action would succeed instead
/// of returning an error, and the attacker would be incorrectly treated as
/// blocked (P1 takes 0 damage instead of 3).
#[test]
fn single_blocker_cannot_block_menace_attacker() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["A", "B", "C", "D"]);
    }

    let menace_creature = scenario.add_creature(P0, "Menace Bear", 3, 3).menace().id();
    // P1 has exactly ONE potential blocker — insufficient for menace.
    let single_blocker = scenario.add_creature(P1, "Lone Guard", 1, 1).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    let mut declared = false;
    let mut rejected_illegal_block = false;

    for _ in 0..400 {
        if runner.life(P1) != p1_life_before {
            break;
        }
        let acted = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                declared = true;
                runner.act(GameAction::DeclareAttackers {
                    attacks: vec![(menace_creature, AttackTarget::Player(P1))],
                    bands: vec![],
                })
            }
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => {
                // DISCRIMINATOR: try to block with a single creature.
                // The engine must reject this — menace requires 2+ blockers.
                let illegal_result = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![(single_blocker, menace_creature)],
                });
                if illegal_result.is_err() {
                    rejected_illegal_block = true;
                }
                // Declare empty blockers so combat can proceed.
                runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
            }
            _ => break,
        };
        if acted.is_err() {
            break;
        }
    }

    assert!(
        rejected_illegal_block,
        "the engine must reject a DeclareBlockers action that assigns a single creature \
         to block a menace attacker — menace requires two or more simultaneous blockers"
    );
    assert_eq!(
        runner.life(P1),
        p1_life_before - 3,
        "the menace attacker goes through unblocked and deals 3 combat damage to P1"
    );
}

/// CR 702.111b + CR 509.1b: when the defending player controls TWO or more
/// creatures, both may legally block the menace attacker simultaneously.
/// After blocking, the menace creature is assigned to two blockers and deals
/// no combat damage to the player (all damage is distributed among blockers).
///
/// Discriminating: reverting menace-block validation would either prevent the
/// two-creature block from being accepted, or misroute attacker damage to the
/// player rather than the blockers.
#[test]
fn menace_attacker_can_be_blocked_by_two_creatures() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["A", "B", "C", "D"]);
    }

    let menace_creature = scenario.add_creature(P0, "Menace Bear", 2, 2).menace().id();
    // P1 has TWO creatures — enough to legally block the menace attacker.
    let blocker_a = scenario.add_creature(P1, "Guard Alpha", 1, 3).id();
    let blocker_b = scenario.add_creature(P1, "Guard Beta", 1, 3).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    let mut declared = false;

    for _ in 0..400 {
        let acted = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { player, .. } if player == P0 && !declared => {
                declared = true;
                runner.act(GameAction::DeclareAttackers {
                    attacks: vec![(menace_creature, AttackTarget::Player(P1))],
                    bands: vec![],
                })
            }
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers {
                valid_block_targets,
                ..
            } => {
                // Both blockers must be able to target the menace attacker.
                let a_targets = valid_block_targets
                    .get(&blocker_a)
                    .cloned()
                    .unwrap_or_default();
                let b_targets = valid_block_targets
                    .get(&blocker_b)
                    .cloned()
                    .unwrap_or_default();
                assert!(
                    a_targets.contains(&menace_creature),
                    "blocker_a must list the menace attacker as a valid block target \
                     when two blockers are available (got: {a_targets:?})"
                );
                assert!(
                    b_targets.contains(&menace_creature),
                    "blocker_b must list the menace attacker as a valid block target \
                     when two blockers are available (got: {b_targets:?})"
                );
                // Declare both creatures as blockers — a legal two-creature block.
                runner.act(GameAction::DeclareBlockers {
                    assignments: vec![(blocker_a, menace_creature), (blocker_b, menace_creature)],
                })
            }
            WaitingFor::AssignCombatDamage { .. } => {
                // Attacker assigns its 2 power across 2 blockers: 1 to each.
                runner.act(GameAction::AssignCombatDamage {
                    mode: CombatDamageAssignmentMode::Normal,
                    assignments: vec![(blocker_a, 1), (blocker_b, 1)],
                    trample_damage: 0,
                    controller_damage: 0,
                })
            }
            _ => break,
        };
        if acted.is_err() {
            break;
        }
    }

    // The menace creature was blocked — no damage reaches the player.
    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "the menace attacker is blocked by two creatures and deals no damage to P1"
    );
}
