//! Razorkin Needlehead — "Whenever an opponent draws a card, this creature
//! deals 1 damage to them." deals the damage to the opponent who drew.
//!
//! Regression for issue #2869: the bare player anaphor "them" was resolved as an
//! object anaphor (`TriggeringSource`, the source creature) with no player
//! referent, so the draw trigger fired but dealt 0 damage. Per CR 603.2 and CR
//! 608.2c, the ordinary draw trigger fires for the opponent's draw and the
//! effect instructions make "them" the opponent who drew; the parser now falls
//! back to the player-actor trigger subject ("an opponent") and binds "them" to
//! `TriggeringPlayer`.
//!
//! Discriminating end-to-end: starting from P0's pre-combat main, the turn rolls
//! into P1's turn where P1's draw step makes P1 (an opponent of P0, who controls
//! Razorkin) draw — so the trigger must deal 1 damage to P1 and none to P0.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

const ORACLE: &str = "Whenever an opponent draws a card, this creature deals 1 damage to them.";

#[test]
fn razorkin_deals_damage_to_the_opponent_who_draws() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Stock libraries so the draw steps have cards to draw.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
    scenario.add_creature_from_oracle(P0, "Razorkin Needlehead", 1, 1, ORACLE);

    let mut runner = scenario.build();
    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    // Roll forward through P0's turn into P1's turn; P1's draw step makes P1 (an
    // opponent of Razorkin's controller P0) draw a card, triggering Razorkin to
    // deal 1 damage to P1. Pass priority and decline combat so the turn rolls
    // over, and stop once P1's life changes.
    for _ in 0..400 {
        if runner.life(P1) != p1_before {
            break;
        }
        let acted = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            _ => break,
        };
        if acted.is_err() {
            break;
        }
    }

    assert_eq!(
        runner.life(P1),
        p1_before - 1,
        "the opponent who drew (P1) must take 1 damage from Razorkin"
    );
    assert_eq!(
        runner.life(P0),
        p0_before,
        "Razorkin's controller takes no damage from their own draws"
    );
}
