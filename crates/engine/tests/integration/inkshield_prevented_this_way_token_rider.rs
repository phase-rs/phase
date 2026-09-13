//! Inkshield (STX #6) — #8777 reproduction: the prevention shield's
//! "for each 1 damage prevented this way" token payoff must actually fire.
//!
//! Oracle text under test (verified against Scryfall 2026-09-12):
//!   "Prevent all combat damage that would be dealt to you this turn. For each
//!    1 damage prevented this way, create a 2/1 white and black Inkling
//!    creature token with flying."
//!
//! CR 615 (prevention) + CR 615.5 (prevented-this-way follow-up) +
//! CR 510.2 (simultaneous combat damage) + CR 111.1 (token creation).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const INKSHIELD_TEXT: &str = "Prevent all combat damage that would be dealt to you this turn. \
     For each 1 damage prevented this way, create a 2/1 white and black Inkling creature token \
     with flying.";

/// Cast Inkshield from P0's hand on P0's own pre-combat main, then flip the
/// active player to P1 so P1's combat runs into the turn-scoped shield.
/// Mirrors `comeuppance.rs`'s `cast_comeuppance_then_p1_turn`.
fn cast_inkshield_then_p1_turn(
    scenario_setup: impl FnOnce(&mut GameScenario),
) -> engine::game::scenario::GameRunner {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let inkshield = scenario
        .add_spell_to_hand_from_oracle(P0, "Inkshield", true, INKSHIELD_TEXT)
        .id();
    scenario_setup(&mut scenario);

    let mut runner = scenario.build();
    runner.cast(inkshield).resolve();
    runner.state_mut().active_player = P1;
    runner
}

fn run_combat(
    runner: &mut engine::game::scenario::GameRunner,
    attacks: &[(ObjectId, AttackTarget)],
) {
    let mut attacked = false;
    for _ in 0..400 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let a = if player == P1 {
                    attacks.to_vec()
                } else {
                    vec![]
                };
                if runner.declare_attackers(&a).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                // The attacker is unblocked in every prompt: this fixture's
                // whole point is that its combat damage reaches P0 and is
                // prevented there.
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

fn inkling_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0 && o.name.contains("Inkling"))
        .count()
}

/// #8777 — a single unblocked 3/3 attacker: all 3 combat damage is prevented
/// AND three 2/1 flying Inklings are created (CR 615.5 + CR 510.2).
#[test]
fn inkshield_prevents_combat_damage_and_creates_one_inkling_per_damage() {
    let mut attacker_id = None;
    let mut runner = cast_inkshield_then_p1_turn(|sc| {
        attacker_id = Some(sc.add_creature(P1, "Raging Bear", 3, 3).id());
    });
    let attacker = attacker_id.unwrap();
    let p0_life_before = runner.life(P0);

    // Reach-guard: the shield really is installed before combat.
    assert!(
        runner
            .state()
            .pending_damage_replacements
            .iter()
            .any(|r| r.shield_kind.is_shield()),
        "reach guard: Inkshield's prevention shield must be installed before combat"
    );

    runner.advance_to_combat();
    run_combat(&mut runner, &[(attacker, AttackTarget::Player(P0))]);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "Inkshield prevents all combat damage dealt to its controller"
    );
    assert_eq!(
        inkling_count(&runner),
        3,
        "CR 615.5: one 2/1 Inkling per 1 damage prevented this way (3 prevented)"
    );
}
