//! Banding combat-damage assignment (CR 702.22).
//!
//! These tests drive the real combat pipeline through `apply`
//! (declare-attackers/blockers → combat-damage step → interactive assignment),
//! never hand-constructing the expected state. They cover the two banding
//! damage-assignment flips and the double-strike re-entry of the blocker-damage
//! prompt:
//!
//! - **CR 702.22k (B2)**: a blocker blocking a banding attacker has its damage
//!   divided by the ACTIVE player (a `WaitingFor::AssignBlockerDamage` prompt),
//!   not auto-split.
//! - **CR 702.22j (B1)**: an attacker blocked by a creature with banding has its
//!   damage divided by the DEFENDING player (`WaitingFor::AssignCombatDamage`
//!   with the defending player as chooser), not the active player.
//! - **Double strike**: the `AssignBlockerDamage` prompt is raised once in the
//!   first-strike sub-step and again in the regular sub-step (the per-sub-step
//!   `damage_assignments` skip-key is cleared at the FS→regular boundary).
//!
//! Creatures are built via `GameScenario` with inline keywords so the tests run
//! in CI without `client/public/card-data.json` (see `project_ci_no_client_card_data`).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

/// Pass priority repeatedly until the engine surfaces a combat-damage
/// assignment prompt (`AssignCombatDamage` / `AssignBlockerDamage`) or otherwise
/// stalls (no longer at a `Priority` window). Bounded to guard against a stuck
/// transition. Mirrors the test-side auto-pass loop other combat tests use.
fn pass_until_damage_prompt(runner: &mut GameRunner) {
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::AssignCombatDamage { .. } | WaitingFor::AssignBlockerDamage { .. } => {
                return;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Drive a single-defender combat from PreCombatMain: pass to declare-attackers,
/// declare `attacks` (with optional `bands`), pass the post-attack priority,
/// declare `blocks`, then pass priority until a damage-assignment prompt
/// surfaces. Leaves the runner paused at the prompt (or at the post-combat
/// priority window when no interactive assignment is required).
fn drive_to_first_damage_prompt(
    runner: &mut GameRunner,
    attacks: Vec<(ObjectId, AttackTarget)>,
    bands: Vec<Vec<ObjectId>>,
    blocks: Vec<(ObjectId, ObjectId)>,
) {
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers { attacks, bands })
        .expect("DeclareAttackers should succeed");
    // CR 508.2: active player gets priority after attackers — pass through it.
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: blocks,
            })
            .expect("DeclareBlockers should succeed");
    }
    pass_until_damage_prompt(runner);
}

/// CR 702.22k: A blocker blocking a banding attacker has its combat damage
/// divided by the ACTIVE player. X(2/2, Banding) + Y(2/2, no banding) attack
/// D(P1) as a BAND; D blocks one band member (X) with a single 4/4 blocker Z,
/// and CR 702.22h propagates the block to the whole band, so Z ends up blocking
/// both X and Y. Because Z blocks two attackers and one of them (X) has banding,
/// the active player A(P0) — not the defending player — divides Z's 4 damage.
#[test]
fn b2_active_player_divides_blocker_damage_into_banded_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario
        .add_creature(P0, "Banded Attacker", 2, 2)
        .with_keyword(Keyword::Banding)
        .id();
    let y = scenario.add_creature(P0, "Plain Attacker", 2, 2).id();
    let z = scenario.add_creature(P1, "Blocker", 4, 4).id();

    let mut runner = scenario.build();
    // CR 702.22c: X (banding) + Y (non-banding) is a legal band. CR 702.22h:
    // blocking X with Z propagates the block to Y, so Z blocks both.
    drive_to_first_damage_prompt(
        &mut runner,
        vec![(x, AttackTarget::Player(P1)), (y, AttackTarget::Player(P1))],
        vec![vec![x, y]],
        vec![(z, x)],
    );

    // CR 702.22k: the active player (P0) divides Z's damage among [X, Y].
    match runner.state().waiting_for.clone() {
        WaitingFor::AssignBlockerDamage {
            player,
            blocker_id,
            total_damage,
            attackers,
        } => {
            assert_eq!(
                player, P0,
                "CR 702.22k: active player divides blocker damage"
            );
            assert_eq!(blocker_id, z);
            assert_eq!(total_damage, 4, "Z's combat power");
            assert_eq!(attackers, vec![x, y], "Z blocks both banded X and plain Y");
        }
        other => panic!("expected AssignBlockerDamage prompt, got {other:?}"),
    }

    // Assign all 4 to X → X (toughness 2) dies, Y lives.
    runner
        .act(GameAction::AssignBlockerDamage {
            assignments: vec![(x, 4)],
        })
        .expect("valid blocker-damage division should succeed");
    // Drain any further prompts (the attackers' own damage, etc.) to completion.
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }

    let state = runner.state();
    assert!(
        !state.battlefield.contains(&x),
        "X took all 4 of Z's damage (>= its 2 toughness) and should be dead"
    );
    assert!(
        state.battlefield.contains(&y),
        "Y received 0 of Z's damage (only 2 from its own X-blocked exchange) and should live"
    );
}

/// CR 510.1e: the active player's blocker-damage division must sum to the
/// blocker's combat power. Assigning 3 of a 4-power blocker's damage is illegal.
#[test]
fn b2_blocker_division_must_equal_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario
        .add_creature(P0, "Banded Attacker", 2, 2)
        .with_keyword(Keyword::Banding)
        .id();
    let y = scenario.add_creature(P0, "Plain Attacker", 2, 2).id();
    let z = scenario.add_creature(P1, "Blocker", 4, 4).id();

    let mut runner = scenario.build();
    drive_to_first_damage_prompt(
        &mut runner,
        vec![(x, AttackTarget::Player(P1)), (y, AttackTarget::Player(P1))],
        vec![vec![x, y]],
        vec![(z, x)],
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AssignBlockerDamage { .. }
    ));

    // Sum (3) != Z's combat power (4) → rejected (CR 510.1e).
    let err = runner
        .act(GameAction::AssignBlockerDamage {
            assignments: vec![(x, 3)],
        })
        .expect_err("under-assigning the blocker's power must be rejected");
    assert!(
        matches!(err, engine::game::engine::EngineError::InvalidAction(_)),
        "expected InvalidAction for a sub-power division, got {err:?}"
    );
}

/// CR 510.1d: every target of the blocker's damage must be an attacker the
/// blocker is actually blocking. Assigning to a creature Z does not block is
/// illegal even if the total matches Z's power.
#[test]
fn b2_blocker_division_target_must_be_blocked_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario
        .add_creature(P0, "Banded Attacker", 2, 2)
        .with_keyword(Keyword::Banding)
        .id();
    let y = scenario.add_creature(P0, "Plain Attacker", 2, 2).id();
    let z = scenario.add_creature(P1, "Blocker", 4, 4).id();
    // W attacks too but is NOT blocked by Z, so Z may not assign damage to it.
    let w = scenario.add_creature(P0, "Unblocked Attacker", 2, 2).id();

    let mut runner = scenario.build();
    // X + Y attack as a band (Z blocks the band via X, CR 702.22h); W attacks
    // outside the band and is left unblocked.
    drive_to_first_damage_prompt(
        &mut runner,
        vec![
            (x, AttackTarget::Player(P1)),
            (y, AttackTarget::Player(P1)),
            (w, AttackTarget::Player(P1)),
        ],
        vec![vec![x, y]],
        vec![(z, x)],
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::AssignBlockerDamage { .. }
    ));

    // Total (4) matches Z's power, but W is not blocked by Z → rejected.
    let err = runner
        .act(GameAction::AssignBlockerDamage {
            assignments: vec![(w, 4)],
        })
        .expect_err("assigning to an unblocked attacker must be rejected");
    assert!(
        matches!(err, engine::game::engine::EngineError::InvalidAction(_)),
        "expected InvalidAction for an unblocked target, got {err:?}"
    );
}

/// Control for B2: the `AssignBlockerDamage` flip requires the blocker to be
/// blocking 2+ attackers (CR 510.1d only gives a division *choice* with 2+
/// targets). A banding attacker that is single-blocked assigns all the blocker's
/// damage to the lone attacker (CR 510.1c), so no prompt is raised — proving the
/// flip is gated on multi-block, not on banding presence alone. (With vanilla
/// creatures a blocker can only block 2+ attackers via a band, so the
/// no-multi-block case is the meaningful control.)
#[test]
fn b2_control_single_blocked_banded_attacker_no_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario
        .add_creature(P0, "Banded Attacker", 2, 2)
        .with_keyword(Keyword::Banding)
        .id();
    let z = scenario.add_creature(P1, "Blocker", 4, 4).id();

    let mut runner = scenario.build();
    drive_to_first_damage_prompt(
        &mut runner,
        vec![(x, AttackTarget::Player(P1))],
        vec![],
        vec![(z, x)],
    );

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AssignBlockerDamage { .. }
        ),
        "banded attacker single-blocked → blocker assigns all damage to it, no division prompt"
    );
}

/// CR 702.22j: An attacker blocked by a creature with banding has its combat
/// damage divided by the DEFENDING player. X(3/3, no banding) attacks D(P1); D
/// blocks with TWO creatures, one of which has Banding. Because one blocker has
/// banding, the defending player D — rather than X's controller (the active
/// player) — chooses how X's damage is assigned.
#[test]
fn b1_defending_player_divides_attacker_damage_when_blocker_has_banding() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let banded_blocker = scenario
        .add_creature(P1, "Banded Blocker", 2, 2)
        .with_keyword(Keyword::Banding)
        .id();
    let plain_blocker = scenario.add_creature(P1, "Plain Blocker", 2, 2).id();

    let mut runner = scenario.build();
    drive_to_first_damage_prompt(
        &mut runner,
        vec![(x, AttackTarget::Player(P1))],
        vec![],
        vec![(banded_blocker, x), (plain_blocker, x)],
    );

    // CR 702.22j: the defending player (P1) divides X's damage among its blockers.
    match runner.state().waiting_for.clone() {
        WaitingFor::AssignCombatDamage {
            player,
            attacker_id,
            ..
        } => {
            assert_eq!(
                player, P1,
                "CR 702.22j: defending player divides damage of a banding-blocked attacker"
            );
            assert_eq!(attacker_id, x);
        }
        other => panic!("expected AssignCombatDamage prompt, got {other:?}"),
    }
}

/// Control for B1: with NO banding on either blocker, the attacker's controller
/// (the active player) divides X's damage — the chooser flip is conditional on a
/// blocking creature having banding (CR 510.1c is the default; CR 702.22j the
/// exception).
#[test]
fn b1_control_no_banding_active_player_divides_attacker_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let blocker_a = scenario.add_creature(P1, "Blocker A", 2, 2).id();
    let blocker_b = scenario.add_creature(P1, "Blocker B", 2, 2).id();

    let mut runner = scenario.build();
    drive_to_first_damage_prompt(
        &mut runner,
        vec![(x, AttackTarget::Player(P1))],
        vec![],
        vec![(blocker_a, x), (blocker_b, x)],
    );

    match runner.state().waiting_for.clone() {
        WaitingFor::AssignCombatDamage {
            player,
            attacker_id,
            ..
        } => {
            assert_eq!(
                player, P0,
                "CR 510.1c: with no banding, the active player (attacker's controller) divides"
            );
            assert_eq!(attacker_id, x);
        }
        other => panic!("expected AssignCombatDamage prompt, got {other:?}"),
    }
}

/// CR 510.4 + CR 702.22k: A double-strike blocker blocking a banded multi-attacker
/// band raises the `AssignBlockerDamage` prompt in BOTH the first-strike and the
/// regular combat-damage sub-steps. This proves the per-sub-step
/// `damage_assignments` skip-key is cleared at the FS→regular boundary (the
/// blocker is neither skipped nor re-prompted within a sub-step).
#[test]
fn double_strike_blocker_reprompts_in_regular_substep() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let x = scenario
        .add_creature(P0, "Banded Attacker", 1, 4)
        .with_keyword(Keyword::Banding)
        .id();
    let y = scenario.add_creature(P0, "Plain Attacker", 1, 4).id();
    // Double-strike 4/4 blocker blocking both banded X and plain Y.
    let z = scenario
        .add_creature(P1, "Double Striker", 4, 4)
        .with_keyword(Keyword::DoubleStrike)
        .id();

    let mut runner = scenario.build();
    // X + Y attack as a band; Z blocks the band via X (CR 702.22h propagation).
    drive_to_first_damage_prompt(
        &mut runner,
        vec![(x, AttackTarget::Player(P1)), (y, AttackTarget::Player(P1))],
        vec![vec![x, y]],
        vec![(z, x)],
    );

    // First-strike sub-step: the active player divides Z's first-strike damage.
    match runner.state().waiting_for.clone() {
        WaitingFor::AssignBlockerDamage {
            player,
            blocker_id,
            total_damage,
            ..
        } => {
            assert_eq!(player, P0);
            assert_eq!(blocker_id, z);
            assert_eq!(total_damage, 4, "first-strike sub-step: Z's full power");
        }
        other => panic!("expected first-strike AssignBlockerDamage, got {other:?}"),
    }
    // Split 2/2 across the band so neither attacker (toughness 4) dies yet.
    runner
        .act(GameAction::AssignBlockerDamage {
            assignments: vec![(x, 2), (y, 2)],
        })
        .expect("first-strike blocker division should succeed");

    // Drive to the next damage prompt (the regular sub-step).
    pass_until_damage_prompt(&mut runner);

    // CR 510.4: the regular sub-step clears the skip-key, so Z is prompted AGAIN
    // (not skipped, not infinitely re-prompted within the first-strike sub-step).
    match runner.state().waiting_for.clone() {
        WaitingFor::AssignBlockerDamage {
            player,
            blocker_id,
            total_damage,
            ..
        } => {
            assert_eq!(player, P0);
            assert_eq!(
                blocker_id, z,
                "regular sub-step re-prompts the same blocker"
            );
            assert_eq!(total_damage, 4, "regular sub-step: Z's full power again");
        }
        other => panic!("expected regular-substep AssignBlockerDamage, got {other:?}"),
    }
    // Resolve the regular sub-step; combat should then complete without hanging.
    runner
        .act(GameAction::AssignBlockerDamage {
            assignments: vec![(x, 4)],
        })
        .expect("regular blocker division should succeed");
    for _ in 0..16 {
        match runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AssignBlockerDamage { .. }
        ),
        "combat must complete — no lingering blocker-damage prompt"
    );
}
