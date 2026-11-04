//! Shakedown Heavy — CR 118.12: the defending player decides the draw; only
//! an accept untaps it and removes it from combat (CR 506.4). This module pins
//! the decline: Shakedown Heavy stays tapped and attacking.
//!
//! "Whenever this creature attacks, defending player may have you draw a card.
//! If they do, untap this creature and remove it from combat."

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const SHAKEDOWN_HEAVY: &str = "Menace\nWhenever this creature attacks, defending player may have you draw a card. If they do, untap this creature and remove it from combat.";

fn attack(accept: bool) -> (GameRunner, ObjectId, Vec<PlayerId>, usize) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        for index in 0..3 {
            scenario.add_spell_to_library_top(player, &format!("Library {index}"), false);
        }
    }
    let heavy = scenario
        .add_creature_from_oracle(P0, "Shakedown Heavy", 6, 4, SHAKEDOWN_HEAVY)
        .id();
    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(heavy, AttackTarget::Player(P1))])
        .expect("declare Shakedown Heavy as an attacker");
    let mut actors = Vec::new();
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                actors.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("answer Shakedown Heavy's draw choice");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return (runner, heavy, actors, hand_before);
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while the attack trigger resolves");
            }
            other => panic!("unexpected Shakedown Heavy prompt: {other:?}"),
        }
    }
    panic!("Shakedown Heavy's attack trigger did not resolve");
}

fn is_attacking(runner: &GameRunner, heavy: ObjectId) -> bool {
    runner
        .state()
        .combat
        .as_ref()
        .is_some_and(|combat| combat.attackers.iter().any(|a| a.object_id == heavy))
}

fn hand_len(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

/// R4 — CR 118.12: on decline the controller does not draw and Shakedown Heavy
/// stays tapped and attacking; "untap … and remove it from combat" is the
/// "If they do" effect.
#[test]
fn decline_stays_tapped_and_attacking() {
    let (runner, heavy, actors, hand_before) = attack(false);
    let tapped = runner.state().objects[&heavy].tapped;
    let attacking = is_attacking(&runner, heavy);
    assert_eq!(actors, vec![P1]);
    assert_eq!(hand_len(&runner), hand_before);
    assert!(tapped, "CR 118.12: a decline leaves Shakedown Heavy tapped");
    assert!(attacking, "CR 118.12: a decline leaves it attacking");
}
