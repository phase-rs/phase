//! Runtime test for CR 508.6 — "the number of opponents you attacked this
//! turn" (Militant Angel) must resolve against real declare-attackers state.
//!
//! Militant Angel reads:
//!   "Whenever Militant Angel attacks, create a number of 1/1 white Soldier
//!    creature tokens equal to the number of opponents you attacked this turn."
//!
//! The `PlayerCount { OpponentAttacked { You, ThisTurn } }` filter resolves against
//! `state.attacked_defenders_this_turn[controller]`, which is populated by
//! `record_attackers_declared` during the real DeclareAttackers step (CR 508.5:
//! the defending player is the player/planeswalker-controller/battle-protector
//! the creature is attacking). This test drives the full pipeline through
//! `apply` — it does NOT hand-insert into the substrate map (that would be a
//! shape test). It then resolves the public `resolve_quantity` against the
//! post-declare state and asserts the count reflects the opponent attacked.
//!
//! These tests use synthetic creatures (`add_creature`), so no card database is
//! loaded and they run identically in CI and local Tilt.

use engine::game::combat::AttackTarget;
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{AttackScope, AttackSubject, PlayerFilter, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Convenience constant for the third player (no `P2` const in the scenario
/// module).
const P2: PlayerId = PlayerId(2);

/// CR 508.6: after P0 declares a creature attacking P1, resolving
/// `PlayerCount { OpponentAttacked { You, ThisTurn } }` from P0's perspective counts the
/// one opponent attacked this turn.
#[test]
fn opponents_attacked_this_turn_counts_declared_defender() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let mut runner = scenario.build();

    // Drive the real declare-attackers step so `record_attackers_declared`
    // populates `attacked_defenders_this_turn` (no hand-insertion).
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
            },
        },
        P0,
        attacker,
    );

    assert_eq!(
        count, 1,
        "P0 attacked exactly one opponent (P1) this turn (CR 508.6)"
    );
}

/// Negative control: with no attackers declared, the substrate is empty and the
/// count is 0 (CR 508.6 — a player has only "attacked" players against whom they
/// declared attackers).
#[test]
fn opponents_attacked_this_turn_is_zero_without_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _attacker = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let runner = scenario.build();

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
            },
        },
        P0,
        _attacker,
    );

    assert_eq!(
        count, 0,
        "no attacks declared this turn means 0 opponents attacked (CR 508.6)"
    );
}

/// CR 508.6: in a 3-player game, when P0 declares creatures attacking BOTH P1
/// and P2, resolving `PlayerCount { OpponentAttacked { You, ThisTurn } }` from P0 counts
/// both attacked opponents (Gemini-specified multi-opponent coverage for
/// Militant Angel's token fan-out).
#[test]
fn opponents_attacked_this_turn_counts_multiple_defenders() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_vs_p1 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let attacker_vs_p2 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let mut runner = scenario.build();

    // Pass priority for every player (3-player game) until the active player
    // reaches the declare-attackers step, then declare both attackers in one
    // step so `record_attackers_declared` records both defenders (no
    // hand-insertion into the substrate).
    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p1, AttackTarget::Player(P1)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
            },
        },
        P0,
        attacker_vs_p1,
    );

    assert_eq!(
        count, 2,
        "P0 attacked both opponents (P1 and P2) this turn (CR 508.6)"
    );
}

/// CR 702.121a: Melee's magnitude — `OpponentAttacked { You, ThisCombat }` — must
/// count the distinct opponents attacked in the CURRENT combat, resolved from the
/// current combat's declaration ledger (not the turn-scoped ledger). With P0
/// attacking both P1 and P2 this combat, the combat-scoped count is 2, so a
/// Melee creature would get +2/+2.
#[test]
fn opponents_attacked_this_combat_counts_current_combat_defenders() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_vs_p1 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let attacker_vs_p2 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let mut runner = scenario.build();

    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p1, AttackTarget::Player(P1)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisCombat,
                },
            },
        },
        P0,
        attacker_vs_p1,
    );

    assert_eq!(
        count, 2,
        "P0 attacked both opponents this combat — Melee pumps +2/+2 (CR 702.121a)"
    );
}

/// CR 702.121a + CR 506.4: Melee counts opponents attacked this combat from
/// declaration history, not from the live attacker set. Removing attackers from
/// combat before the trigger resolves must not reduce the count.
#[test]
fn opponents_attacked_this_combat_survives_attackers_leaving_combat() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_vs_p1 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let attacker_vs_p2 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let mut runner = scenario.build();

    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p1, AttackTarget::Player(P1)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    runner
        .state_mut()
        .combat
        .as_mut()
        .expect("combat exists")
        .attackers
        .clear();

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisCombat,
                },
            },
        },
        P0,
        attacker_vs_p1,
    );

    assert_eq!(
        count, 2,
        "Melee still counts both opponents attacked this combat after attackers leave combat"
    );
}

/// CR 702.121a: Melee counts the opponents attacked this combat even if an
/// attacked opponent leaves the game before the trigger resolves.
#[test]
fn opponents_attacked_this_combat_counts_eliminated_defender() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_vs_p1 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let attacker_vs_p2 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let mut runner = scenario.build();

    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p1, AttackTarget::Player(P1)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P2)
        .expect("P2 exists")
        .is_eliminated = true;

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisCombat,
                },
            },
        },
        P0,
        attacker_vs_p1,
    );

    assert_eq!(
        count, 2,
        "Melee still counts P2 because P0 attacked P2 this combat"
    );
}

/// CR 102.3 + CR 800.4a: an opponent is a player still in the game, and a
/// player who has left the game (`is_eliminated`) no longer participates, so a
/// defender P0 attacked this turn but who has since left the game must NOT be
/// counted by `PlayerCount { OpponentAttacked { You, ThisTurn } }`. Discriminating test
/// for the `!p.is_eliminated` guard on the resolving arm: with both defenders
/// attacked the count is 2 (see the multi-defender test); eliminating one
/// attacked opponent drops it to 1.
#[test]
fn opponents_attacked_this_turn_excludes_eliminated_defender() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_vs_p1 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let attacker_vs_p2 = scenario.add_creature(P0, "Soldier", 2, 2).id();
    let mut runner = scenario.build();

    for _ in 0..12 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            break;
        }
        let _ = runner.act(GameAction::PassPriority);
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p1, AttackTarget::Player(P1)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");

    // CR 800.4a + CR 102.3: P2 has left the game and is no longer an opponent.
    // The attacked-defenders ledger still records the declaration, but the
    // resolving arm must filter eliminated players so the count reflects only
    // opponents still in the game.
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P2)
        .expect("P2 exists")
        .is_eliminated = true;

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttacked {
                    subject: AttackSubject::You,
                    scope: AttackScope::ThisTurn,
                },
            },
        },
        P0,
        attacker_vs_p1,
    );

    assert_eq!(
        count, 1,
        "eliminating attacked opponent P2 drops the count to 1 (only P1 remains)"
    );
}
