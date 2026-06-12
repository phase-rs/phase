//! Issue #2924 — Suppressor Skyguard end-to-end combat-damage prevention.
//!
//! Oracle text: "Flying\nWhenever a player attacks you, if that player has
//! another opponent who isn't being attacked, prevent all combat damage that
//! would be dealt to you this combat."
//!
//! The original bug report is "no combat damage is prevented in real combat."
//! Earlier regression tests pinned the individual links (the intervening-if
//! count resolves correctly at detection, the resolver stamps an
//! `UntilEndOfCombat` prevention shield, and the parser produces the right
//! AST), but none drove the *integrated* pipeline: a real attack declaration in
//! multiplayer → the trigger fires and resolves → a prevention shield is
//! created on the attacked player → combat damage is actually prevented →
//! the attacked player's life is unchanged.
//!
//! These tests drive the full `apply` pipeline. The discriminator is the
//! attacked player's life total after combat damage:
//!   - APPLY: P1 attacks ONLY P0 while P2 (P1's other opponent) is un-attacked
//!     → the intervening-if is satisfied → all combat damage to P0 is
//!     prevented → P0's life is UNCHANGED.
//!   - CONTROL: P1 attacks BOTH P0 and P2 → P1 has no un-attacked opponent →
//!     the intervening-if is FALSE → no shield → P0 LOSES life equal to the
//!     attacker's power. Same board minus one attack declaration flips the
//!     outcome, proving the multiplayer condition + prevention work end-to-end
//!     (not merely that prevention works when a shield exists).
//!   - GUARD (2-player): P1 attacks P0 but P1 has no other opponent → the
//!     intervening-if is FALSE → P0 takes damage.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 506.2 + CR 508.6: a player is "being attacked" when an attacker is
//!     declared against them; the un-attacked-opponent count is read from the
//!     real declare-attackers ledger.
//!   - CR 603.4: the intervening "if" is checked when the trigger would fire and
//!     again on resolution; if false at either point the ability does nothing.
//!   - CR 510.1b / CR 510.2 / CR 120.1: combat damage is assigned to the
//!     unblocked attacker's defending player, then dealt during the combat damage
//!     step and reduces the recipient's life.
//!   - CR 615.1: a prevention shield replaces the would-be damage with nothing.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Convenience constant for the third player (no `P2` const in the scenario
/// module).
const P2: PlayerId = PlayerId(2);

const SUPPRESSOR_SKYGUARD: &str = "Flying\nWhenever a player attacks you, if that player has \
another opponent who isn't being attacked, prevent all combat damage that would be dealt to \
you this combat.";

/// Hand the turn to `attacker` (the active player) so it can declare attackers,
/// then pass priority until the engine waits for attacker declaration. Mirrors
/// the `runner.state_mut().active_player = P1` idiom used by
/// `doran_attack_block_pump.rs` for "the opponent is the attacker this turn".
fn hand_turn_to(runner: &mut GameRunner, attacker: PlayerId) {
    runner.state_mut().active_player = attacker;
    runner.state_mut().priority_player = attacker;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: attacker };

    // Pass priority around the table until the active player reaches the
    // DeclareAttackers step (bounded; a turn has a fixed number of steps).
    for _ in 0..16 {
        if runner.waiting_for_kind() == "DeclareAttackers" {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            return;
        }
    }
}

/// After attackers are declared, drive the engine through the rest of combat:
/// pass priority around the table, auto-submit empty blockers when prompted
/// (the attacks here are all unblocked), and drain the trigger from the stack,
/// until combat damage has been dealt and the stack is empty.
fn resolve_combat(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.waiting_for_kind() {
            "DeclareBlockers" => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("DeclareBlockers (no blocks) should succeed");
            }
            _ => {
                // Pass priority for whoever currently holds it; this advances
                // the phase machine through the combat-damage step and resolves
                // the prevention trigger on the stack.
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
        if runner.state().phase == Phase::PostCombatMain && runner.state().stack.is_empty() {
            break;
        }
    }
    runner.advance_until_stack_empty();
}

/// Build a 3-player game with Suppressor Skyguard on P0's battlefield and a
/// vanilla attacker of `attacker_power` on P1's battlefield. Returns the runner
/// plus the attacker id.
fn build_three_player(attacker_power: i32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Skyguard is printed 1/3; give it toughness 6 so it comfortably survives
    // (it isn't attacking — the trigger is on P0's Skyguard and fires when P1
    // attacks P0). Its exact body is irrelevant to the life assertion.
    scenario.add_creature_from_oracle(P0, "Suppressor Skyguard", 1, 6, SUPPRESSOR_SKYGUARD);

    let attacker = scenario
        .add_creature(P1, "Hostile Bear", attacker_power, attacker_power)
        .id();

    (scenario.build(), attacker)
}

/// APPLY (core bug-fix proof): P1 attacks ONLY P0; P2 is a living, un-attacked
/// opponent of P1, so the intervening-if "that player has another opponent who
/// isn't being attacked" is satisfied. All combat damage to P0 must be
/// prevented and P0's life must be UNCHANGED.
#[test]
fn skyguard_prevents_combat_damage_when_attacker_has_unattacked_opponent() {
    let (mut runner, attacker) = build_three_player(3);
    let p0_life_before = runner.life(P0);

    hand_turn_to(&mut runner, P1);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("P1 DeclareAttackers (vs P0 only) should succeed");
    resolve_combat(&mut runner);

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "CR 615.1 (#2924): P1 attacked only P0 while P2 is an un-attacked \
         opponent, so the Suppressor Skyguard trigger must prevent all combat \
         damage dealt to P0 — P0's life must be unchanged"
    );
}

/// CONTROL (discriminator): same board, but P1 attacks BOTH P0 and P2, so P1
/// has NO un-attacked opponent. The intervening-if is FALSE → no shield → the
/// attacker's combat damage goes through and P0 LOSES life equal to its power.
/// This case FAILS if the multiplayer condition logic is wrong or reverted.
#[test]
fn skyguard_does_not_prevent_when_attacker_attacks_every_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Suppressor Skyguard", 1, 6, SUPPRESSOR_SKYGUARD);
    let attacker_vs_p0 = scenario.add_creature(P1, "Hostile Bear", 3, 3).id();
    let attacker_vs_p2 = scenario.add_creature(P1, "Hostile Bear 2", 2, 2).id();
    let mut runner = scenario.build();

    let p0_life_before = runner.life(P0);

    hand_turn_to(&mut runner, P1);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (attacker_vs_p0, AttackTarget::Player(P0)),
                (attacker_vs_p2, AttackTarget::Player(P2)),
            ],
            bands: vec![],
        })
        .expect("P1 DeclareAttackers (vs P0 and P2) should succeed");
    resolve_combat(&mut runner);

    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "CR 603.4 (#2924): P1 attacked every opponent (P0 and P2), so it has no \
         un-attacked opponent — the intervening-if is FALSE, no prevention shield \
         is created, and P0 takes the 3-power attacker's combat damage"
    );
}

/// GUARD (2-player): P1 attacks P0 but P1 has no other opponent at all, so the
/// "another opponent who isn't being attacked" condition is FALSE and damage
/// goes through. Confirms the trigger does not over-fire in heads-up play.
#[test]
fn skyguard_does_not_prevent_in_two_player_game() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Suppressor Skyguard", 1, 6, SUPPRESSOR_SKYGUARD);
    let attacker = scenario.add_creature(P1, "Hostile Bear", 3, 3).id();
    let mut runner = scenario.build();

    let p0_life_before = runner.life(P0);

    hand_turn_to(&mut runner, P1);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P0))],
            bands: vec![],
        })
        .expect("P1 DeclareAttackers (2-player, vs P0) should succeed");
    resolve_combat(&mut runner);

    assert_eq!(
        runner.life(P0),
        p0_life_before - 3,
        "CR 603.4 (#2924): in a 2-player game P1 has no other opponent, so the \
         intervening-if is FALSE — no shield, P0 takes the attacker's 3 damage"
    );
}
