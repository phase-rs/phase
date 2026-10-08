//! Preacher of the Schism — both attack triggers are gated by life totals:
//!   1. "Whenever this creature attacks the player with the most life or tied for
//!      most life, create a 1/1 white Vampire creature token with lifelink."
//!   2. "Whenever this creature attacks while you have the most life or are tied
//!      for most life, you draw a card and you lose 1 life."
//!
//! CR 508.1m: both are read when attackers are declared. CR 102.1: "the most
//! life" is taken over ALL players, so a tie counts.
//!
//! Field report (2026-09-28): both gates were dropped by the parser, so every
//! attack made a token AND drew a card, whatever the life totals.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const PREACHER: &str = "Deathtouch\n\
Whenever this creature attacks the player with the most life or tied for most life, create a 1/1 white Vampire creature token with lifelink.\n\
Whenever this creature attacks while you have the most life or are tied for most life, you draw a card and you lose 1 life.";

struct Outcome {
    vampires: usize,
    drew: usize,
    life_lost: i32,
}

fn vampires(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == P0 && o.name.contains("Vampire"))
        .count()
}

fn attack(my_life: i32, their_life: i32) -> Outcome {
    attack_with(&[my_life, their_life], |_| {})
}

/// Seat one player per entry of `lives` (P0 attacks P1 with Preacher), declare
/// the attack, run `in_response` while both triggers are on the stack, then
/// resolve everything.
fn attack_with(lives: &[i32], in_response: impl FnOnce(&mut GameRunner)) -> Outcome {
    let mut scenario = GameScenario::new_n_player(lives.len() as u8, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let preacher = {
        let mut b = scenario.add_creature(P0, "Preacher of the Schism", 2, 4);
        b.from_oracle_text_with_keywords(&["Deathtouch"], PREACHER);
        b.id()
    };
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);
    for (seat, life) in lives.iter().enumerate() {
        scenario.with_life(PlayerId(seat as u8), *life);
    }
    let mut runner = scenario.build();

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance toward declare attackers");
            }
            other => panic!("unexpected waiting_for before attackers: {other:?}"),
        }
    }
    let my_life = lives[0];
    let hand_before = runner.state().players[0].hand.len();
    let vampires_before = vampires(&runner);
    runner
        .declare_attackers(&[(preacher, AttackTarget::Player(P1))])
        .expect("Preacher should be a legal attacker");
    for _ in 0..16 {
        if let WaitingFor::OrderTriggers { triggers, .. } = runner.state().waiting_for.clone() {
            let order = (0..triggers.len()).collect();
            runner
                .act(GameAction::OrderTriggers { order })
                .expect("ordering the two attack triggers should succeed");
        } else {
            break;
        }
    }
    in_response(&mut runner);
    runner.advance_until_stack_empty();
    Outcome {
        vampires: vampires(&runner) - vampires_before,
        drew: runner.state().players[0].hand.len() - hand_before,
        life_lost: my_life - runner.state().players[0].life,
    }
}

#[test]
fn tied_life_makes_token_and_draws() {
    let o = attack(20, 20);
    assert_eq!(o.vampires, 1, "defender tied for most life → token");
    assert_eq!(o.drew, 1, "you are tied for most life → draw");
    assert_eq!(o.life_lost, 1);
}

#[test]
fn behind_on_life_makes_token_only() {
    let o = attack(10, 20);
    assert_eq!(o.vampires, 1, "defender has the most life → token");
    assert_eq!(o.drew, 0, "you don't have the most life → no draw");
    assert_eq!(o.life_lost, 0);
}

#[test]
fn ahead_on_life_draws_only() {
    let o = attack(20, 10);
    assert_eq!(
        o.vampires, 0,
        "defender is not the most-life player → no token"
    );
    assert_eq!(o.drew, 1, "you have the most life → draw");
    assert_eq!(o.life_lost, 1);
}

/// CR 102.1: "the most life" is over ALL players, the attacker included. With
/// three players and the attacker leading (30 / 20 / 10), the defender (20)
/// is not the most-life player even though it leads the opponents — so no
/// token. An opponents-only maximum would wrongly make one.
#[test]
fn three_players_attacker_leads_so_defender_gets_no_token() {
    let o = attack_with(&[30, 20, 10], |_| {});
    assert_eq!(o.vampires, 0, "20 is not the most life at a 30/20/10 table");
    assert_eq!(o.drew, 1, "the attacker has the most life → draw");
    assert_eq!(o.life_lost, 1);
}

/// Three players, the defender leads (10 / 25 / 20): token, and no draw.
#[test]
fn three_players_defender_leads_makes_token_only() {
    let o = attack_with(&[10, 25, 20], |_| {});
    assert_eq!(o.vampires, 1, "the defender has the most life → token");
    assert_eq!(o.drew, 0, "the attacker is behind → no draw");
    assert_eq!(o.life_lost, 0);
}

/// CR 508.1m + CR 603.4: both gates are read when Preacher is declared an
/// attacker. The attacker leads at declaration (20 / 10); the defender then
/// gains life to 30 while the triggers are on the stack. The draw still
/// resolves (its "while" gate is not an intervening "if"), and the token
/// never triggered (the defender was not the most-life player at declaration).
#[test]
fn life_change_in_response_does_not_undo_the_declaration_read() {
    let o = attack_with(&[20, 10], |runner| {
        runner.state_mut().players[1].life = 30;
    });
    assert_eq!(
        o.vampires, 0,
        "the defender was behind when attackers were declared"
    );
    assert_eq!(
        o.drew, 1,
        "the draw gate passed at declaration and is not rechecked"
    );
    assert_eq!(o.life_lost, 1);
}
