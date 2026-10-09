//! Ghalta and Mavren — "Whenever you attack, choose one —" modal token trigger.
//!
//! Parser-shape assertions alone cannot prove runtime behavior: this file
//! drives the real combat pipeline (`advance_to_combat` + `declare_attackers`)
//! so the attack trigger fires from a genuine attack declaration, then drives
//! the modal choice to resolution for both modes.
//!
//! CR 508.3d (whenever-you-attack triggers) + CR 508.4 (tokens created already
//! attacking) + CR 700.2b (modal choice).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

const GHALTA_AND_MAVREN: &str = "Trample\nWhenever you attack, choose one —\n\
    \u{2022} Create a tapped and attacking X/X green Dinosaur creature token with trample, \
    where X is the greatest power among other attacking creatures.\n\
    \u{2022} Create X 1/1 white Vampire creature tokens with lifelink, \
    where X is the number of other attacking creatures.";

// Board: Ghalta and Mavren (12/12) + two other attackers with DIFFERENT
// powers (3 and 7), both strictly less than Ghalta's own 12 — so a
// wrongly-inclusive "other attacking creatures" reading (Ghalta counted)
// is distinguishable from the correct one at both the max (12 vs 7) and
// count (3 vs 2) level.
fn board() -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ghalta = scenario
        .add_creature_from_oracle(P0, "Ghalta and Mavren", 12, 12, GHALTA_AND_MAVREN)
        .id();
    let alpha = scenario.add_creature(P0, "Test Attacker Alpha", 3, 3).id();
    let beta = scenario.add_creature(P0, "Test Attacker Beta", 7, 7).id();
    (scenario.build(), ghalta, alpha, beta)
}

fn attack_with_all(runner: &mut GameRunner, ghalta: ObjectId, alpha: ObjectId, beta: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (ghalta, AttackTarget::Player(P1)),
            (alpha, AttackTarget::Player(P1)),
            (beta, AttackTarget::Player(P1)),
        ])
        .expect("declaring all three attackers must succeed");
}

/// Bounded drive: pick `mode` at the first AbilityModeChoice, pass priority
/// to let the ability resolve, then stop.
fn drive_choose_mode(runner: &mut GameRunner, mode: usize) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![mode],
                    })
                    .expect("choosing Ghalta and Mavren's mode must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    return;
                }
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    panic!("Ghalta and Mavren resolution did not settle after 40 iterations");
}

#[test]
fn mode1_dinosaur_token_power_excludes_ghalta_and_enters_attacking() {
    let (mut runner, ghalta, alpha, beta) = board();
    attack_with_all(&mut runner, ghalta, alpha, beta);
    drive_choose_mode(&mut runner, 0);

    let state = runner.state();
    let dinosaurs: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Dinosaur"))
        .collect();

    assert_eq!(
        dinosaurs.len(),
        1,
        "mode 1 must create exactly one Dinosaur token"
    );
    let token = dinosaurs[0];
    assert_eq!(
        token.power,
        Some(7),
        "X must be the greatest power among OTHER attacking creatures (max(3, 7) = 7) — \
         Ghalta's own printed power (12) must be excluded, or this would read 12"
    );
    assert_eq!(
        token.toughness,
        Some(7),
        "X/X token: toughness must equal power"
    );
    assert!(token.tapped, "Oracle text: the token must enter tapped");
    assert!(
        state
            .combat
            .as_ref()
            .expect("combat is active")
            .attackers
            .iter()
            .any(|a| a.object_id == token.id && a.defending_player == P1),
        "CR 508.4: the token must enter already attacking P1"
    );
    assert!(
        token.has_keyword(&Keyword::Trample),
        "the token must have trample"
    );
}

#[test]
fn mode2_vampire_token_count_excludes_ghalta() {
    let (mut runner, ghalta, alpha, beta) = board();
    attack_with_all(&mut runner, ghalta, alpha, beta);
    drive_choose_mode(&mut runner, 1);

    let state = runner.state();
    let vampires: Vec<_> = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| {
            o.is_token && o.controller == P0 && o.card_types.subtypes.iter().any(|s| s == "Vampire")
        })
        .collect();

    assert_eq!(
        vampires.len(),
        2,
        "X must be the number of OTHER attacking creatures (alpha, beta) — \
         Ghalta must not count itself, or this would read 3"
    );
    for v in &vampires {
        assert_eq!(v.power, Some(1));
        assert_eq!(v.toughness, Some(1));
        assert!(v.has_keyword(&Keyword::Lifelink));
        assert!(
            !v.tapped,
            "mode 2 tokens are not stated to enter tapped or attacking"
        );
        assert!(
            !state
                .combat
                .as_ref()
                .expect("combat is active")
                .attackers
                .iter()
                .any(|attacker| attacker.object_id == v.id),
            "mode 2 tokens must not enter attacking"
        );
    }
}
