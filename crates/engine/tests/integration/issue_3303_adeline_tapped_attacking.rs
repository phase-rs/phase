//! GitHub issue #3303 — Adeline, Resplendent Cathar's attack tokens must enter
//! tapped and attacking.
//!
//! Oracle text:
//!   Vigilance
//!   Adeline's power is equal to the number of creatures you control.
//!   Whenever you attack, for each opponent, create a 1/1 white Human creature
//!   token that's tapped and attacking that player or a planeswalker they control.
//!
//! CR 508.4: tokens that enter tapped and attacking must be registered on
//! `combat.attackers` during a live combat step.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const ADELINE_ORACLE: &str = "Vigilance\n\
    Adeline's power is equal to the number of creatures you control.\n\
    Whenever you attack, for each opponent, create a 1/1 white Human creature token that's tapped and attacking that player or a planeswalker they control.";

fn human_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0
                && o.zone == Zone::Battlefield
                && o.is_token
                && o.card_types
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case("human"))
        })
        .map(|o| o.id)
        .collect()
}

fn resolve_attack_trigger(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![] })
                    .expect("order triggers");
            }
            other => panic!("unexpected waiting state during Adeline trigger: {other:?}"),
        }
    }
    panic!("Adeline trigger did not resolve");
}

#[test]
fn adeline_attack_tokens_enter_tapped_and_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let adeline = scenario
        .add_creature_from_oracle(P0, "Adeline, Resplendent Cathar", 3, 3, ADELINE_ORACLE)
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(adeline, AttackTarget::Player(P1))])
        .expect("declare Adeline attacking P1");

    resolve_attack_trigger(&mut runner);

    let tokens = human_tokens(&runner);
    assert_eq!(
        tokens.len(),
        1,
        "two-player game: one Human token per opponent"
    );

    let attacking: Vec<ObjectId> = runner
        .state()
        .combat
        .as_ref()
        .expect("combat must be live")
        .attackers
        .iter()
        .map(|a| a.object_id)
        .collect();

    for token in tokens {
        let obj = runner.state().objects.get(&token).expect("token exists");
        assert!(obj.tapped, "issue #3303: Human token must enter tapped");
        assert!(
            attacking.contains(&token),
            "issue #3303: Human token must enter attacking; attackers={attacking:?}"
        );
    }
}
