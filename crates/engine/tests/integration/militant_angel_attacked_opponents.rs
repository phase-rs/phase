//! Runtime test for CR 508.6 — "the number of opponents you attacked this
//! turn" (Militant Angel) must resolve against real declare-attackers state.
//!
//! Militant Angel reads:
//!   "Whenever Militant Angel attacks, create a number of 1/1 white Soldier
//!    creature tokens equal to the number of opponents you attacked this turn."
//!
//! The `PlayerCount { OpponentAttackedThisTurn }` filter resolves against
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
use engine::types::ability::{PlayerFilter, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;

/// CR 508.6: after P0 declares a creature attacking P1, resolving
/// `PlayerCount { OpponentAttackedThisTurn }` from P0's perspective counts the
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
        })
        .expect("DeclareAttackers should succeed");

    let count = resolve_quantity(
        runner.state(),
        &QuantityExpr::Ref {
            qty: QuantityRef::PlayerCount {
                filter: PlayerFilter::OpponentAttackedThisTurn,
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
                filter: PlayerFilter::OpponentAttackedThisTurn,
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
