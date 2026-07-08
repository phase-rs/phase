//! Issue #1325 — Ellie, Brick Master must create a Cordyceps Infected token for
//! the attacking player when any player attacks one of your opponents.
//!
//! Oracle text (Distract the Horde):
//!   Whenever a player attacks one of your opponents, that attacking player
//!   creates a tapped 1/1 black Fungus Zombie creature token named Cordyceps
//!   Infected that's attacking that opponent.
//!
//! CR 508.4: the token must enter tapped and attacking the defended opponent.
//! CR 506.2 + CR 603.7c: "that attacking player" is the player who declared
//! attackers in the triggering event, not Ellie's controller.
//!
//! https://github.com/phase-rs/phase/issues/1325

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const ELLIE_ORACLE: &str = "Partner—Survivors\n\
Distract the Horde — Whenever a player attacks one of your opponents, that attacking player creates a tapped 1/1 black Fungus Zombie creature token named Cordyceps Infected that's attacking that opponent.";

fn cordyceps_tokens(runner: &GameRunner, controller: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == controller
                && o.zone == Zone::Battlefield
                && o.is_token
                && o.name.eq_ignore_ascii_case("Cordyceps Infected")
        })
        .map(|o| o.id)
        .collect()
}

fn resolve_attack_triggers(runner: &mut GameRunner) {
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
            other => panic!("unexpected waiting state during Ellie trigger: {other:?}"),
        }
    }
    panic!("Ellie trigger did not resolve");
}

#[test]
fn ellie_creates_token_when_you_attack_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ellie = scenario
        .add_creature_from_oracle(P0, "Ellie, Brick Master", 2, 1, ELLIE_ORACLE)
        .id();

    let attacker = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attack on opponent");

    resolve_attack_triggers(&mut runner);

    let tokens = cordyceps_tokens(&runner, P0);
    assert_eq!(
        tokens.len(),
        1,
        "issue #1325: P0 attacking P1 must create exactly one Cordyceps Infected"
    );

    let token = runner.state().objects.get(&tokens[0]).expect("token");
    assert!(token.tapped, "Cordyceps Infected must enter tapped");
    assert!(
        token.color.contains(&ManaColor::Black),
        "Cordyceps Infected must be black"
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
    assert!(
        attacking.contains(&tokens[0]),
        "Cordyceps Infected must enter attacking; attackers={attacking:?}"
    );

    // Sanity: Ellie herself did not create the token — the attacking player did.
    assert_ne!(ellie, tokens[0]);
}

#[test]
fn ellie_creates_token_for_other_attacking_player_in_three_player_game() {
    const P2: PlayerId = PlayerId(2);

    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Ellie, Brick Master", 2, 1, ELLIE_ORACLE)
        .id();

    let p1_attacker = scenario.add_creature(P1, "Hill Giant", 3, 3).id();

    let mut runner = scenario.build();
    // P1's combat step — P2 is also an opponent of Ellie's controller (P0).
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(p1_attacker, AttackTarget::Player(P2))])
        .expect("P1 attacks P2");

    resolve_attack_triggers(&mut runner);

    assert_eq!(
        cordyceps_tokens(&runner, P1).len(),
        1,
        "issue #1325: when P1 attacks P0's opponent P2, P1 must create Cordyceps Infected"
    );
    assert!(
        cordyceps_tokens(&runner, P0).is_empty(),
        "Ellie's controller must not create the token when another player attacks"
    );
}
