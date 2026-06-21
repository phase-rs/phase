//! Regression for issue #3883: Kari Zev, Skyship Raider must create a Ragavan
//! token tapped and attacking when she attacks.
//!
//! https://github.com/phase-rs/phase/issues/3883

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const KARI_ZEV_ORACLE: &str = "First strike, menace\n\
    Whenever Kari Zev attacks, create Ragavan, a legendary 2/1 red Monkey creature token. \
    Ragavan enters tapped and attacking. Exile that token at end of combat.";

fn ragavan_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0
                && o.zone == Zone::Battlefield
                && o.is_token
                && o.name.eq_ignore_ascii_case("Ragavan")
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
            other => panic!("unexpected waiting state during Kari Zev trigger: {other:?}"),
        }
    }
    panic!("Kari Zev trigger did not resolve");
}

#[test]
fn kari_zev_attack_creates_ragavan_tapped_and_attacking() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kari = scenario
        .add_creature_from_oracle(P0, "Kari Zev, Skyship Raider", 1, 3, KARI_ZEV_ORACLE)
        .id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kari, AttackTarget::Player(P1))])
        .expect("declare Kari Zev attacking P1");

    resolve_attack_trigger(&mut runner);

    let tokens = ragavan_tokens(&runner);
    assert_eq!(
        tokens.len(),
        1,
        "Kari Zev must create exactly one Ragavan token"
    );

    let token = tokens[0];
    let obj = runner.state().objects.get(&token).expect("token exists");
    assert!(obj.tapped, "Ragavan must enter tapped");

    let attacking: Vec<ObjectId> = runner
        .state()
        .combat
        .as_ref()
        .expect("combat must be live during attack trigger resolution")
        .attackers
        .iter()
        .map(|a| a.object_id)
        .collect();
    assert!(
        attacking.contains(&token),
        "Ragavan must enter attacking; attackers={attacking:?}"
    );
}
