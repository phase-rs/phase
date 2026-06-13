//! Discriminating integration tests for **Caesar, Legion's Emperor**
//! (GitHub issue #2857).
//!
//! Oracle text:
//!   Whenever you attack, you may sacrifice another creature. When you do,
//!   choose two —
//!   • Create two 1/1 red and white Soldier creature tokens with haste that
//!     are tapped and attacking.
//!   • You draw a card and you lose 1 life.
//!   • Caesar deals damage equal to the number of creature tokens you control
//!     to target opponent.
//!
//! The bug (issue #2857): the choose-two modes fired on attack WITHOUT the
//! optional sacrifice ever being paid. The reflexive `When you do` gate was
//! dropped and the modal was attached directly to the trigger's `execute`.
//!
//! Rules-correct behavior (CR 603.12 + CR 700.2b):
//!   - The modes are gated behind the optional `Sacrifice another creature`
//!     cost. Declining the sacrifice resolves no modes
//!     (`should_resolve_subability_on_optional_decline`, WhenYouDo -> false).
//!   - Paying the cost prompts `WaitingFor::AbilityModeChoice` (choose two of
//!     three), then resolves the chosen modes.
//!
//! These tests drive the REAL combat pipeline (`advance_to_combat` +
//! `declare_attackers`) so the attack trigger fires from a genuine
//! declare-attackers event, and the mode-1 tokens enter into a live combat
//! (CR 508.4 `enter_attacking` requires `state.combat`).
//!
//! CR 508.4: tokens that enter tapped and attacking. CR 120.1: damage.
//! CR 603.12: reflexive `When you do`. CR 700.2b: modal choice.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const CAESAR_ORACLE: &str = "Whenever you attack, you may sacrifice another creature. When you do, choose two —\n\
    • Create two 1/1 red and white Soldier creature tokens with haste that are tapped and attacking.\n\
    • You draw a card and you lose 1 life.\n\
    • Caesar deals damage equal to the number of creature tokens you control to target opponent.";

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Soldier *tokens* P0 controls on the battlefield. `is_token` excludes Caesar
/// itself (a non-token Human Soldier).
fn soldier_tokens(runner: &GameRunner) -> Vec<ObjectId> {
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
                    .any(|s| s.eq_ignore_ascii_case("soldier"))
        })
        .map(|o| o.id)
        .collect()
}

fn seed_library(scenario: &mut GameScenario, n: usize) {
    for i in 0..n {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
}

/// Build a 2-player board: Caesar (the attacker) plus `num_fodder` sacrificeable
/// creatures for P0. Returns (runner, caesar, fodder_ids).
fn caesar_board(num_fodder: usize, library_cards: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let caesar = scenario
        .add_creature_from_oracle(P0, "Caesar, Legion's Emperor", 4, 4, CAESAR_ORACLE)
        .id();
    let mut fodder = Vec::new();
    for i in 0..num_fodder {
        fodder.push(scenario.add_creature(P0, &format!("Fodder {i}"), 1, 1).id());
    }
    seed_library(&mut scenario, library_cards);

    let runner = scenario.build();
    (runner, caesar, fodder)
}

/// Advance to combat and declare Caesar as the attacker against P1, firing the
/// "Whenever you attack" trigger.
fn attack_with_caesar(runner: &mut GameRunner, caesar: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(caesar, AttackTarget::Player(P1))])
        .expect("declaring Caesar as attacker must succeed");
}

/// Bounded drive loop. `accept_sacrifice` decides the optional `you may
/// sacrifice`; `sacrifice` is selected when a sacrifice choice is raised;
/// `modes` are chosen at the modal prompt; `mode_target` (a player) is submitted
/// for the damage mode's target prompt.
fn drive(
    runner: &mut GameRunner,
    accept_sacrifice: bool,
    sacrifice: Option<ObjectId>,
    modes: Vec<usize>,
    mode_target: Option<PlayerId>,
) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: accept_sacrifice,
                    })
                    .expect("optional sacrifice decision must succeed");
            }
            WaitingFor::EffectZoneChoice { .. } => {
                let pick = sacrifice.expect("a sacrifice target must be provided");
                runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("selecting the sacrifice must succeed");
            }
            WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.clone(),
                    })
                    .expect("choosing modes must succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let p = mode_target.expect("a mode target must be provided");
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(p)],
                    })
                    .expect("selecting the mode target must succeed");
            }
            other => panic!("unexpected waiting state during Caesar resolution: {other:?}"),
        }
    }
    panic!("Caesar resolution did not settle after 80 iterations — likely a stall");
}

/// TEST 1 — RED ON HEAD. Caesar attacks; the optional sacrifice is DECLINED.
/// No modes may fire: no Soldier tokens, no card drawn, no life lost, no damage.
/// On the buggy HEAD the modes fired unconditionally on attack, so all four
/// assertions failed.
#[test]
fn caesar_attack_without_sacrifice_does_nothing() {
    let (mut runner, caesar, _fodder) = caesar_board(1, 3);

    let p0_life_before = life(&runner, P0);
    let p1_life_before = life(&runner, P1);
    let p0_hand_before = hand_len(&runner, P0);

    attack_with_caesar(&mut runner, caesar);
    drive(&mut runner, false, None, vec![], None);

    assert!(
        soldier_tokens(&runner).is_empty(),
        "declining the sacrifice must create NO Soldier tokens (mode 1 must not fire)"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "declining the sacrifice must draw NO card (mode 2 must not fire)"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life_before,
        "declining the sacrifice must lose NO life (mode 2 must not fire)"
    );
    assert_eq!(
        life(&runner, P1),
        p1_life_before,
        "declining the sacrifice must deal NO damage (mode 3 must not fire)"
    );
}

/// TEST 2 — accept the sacrifice, reach the modal, choose mode 1 (tokens) +
/// mode 2 (draw/lose). Assert two 1/1 Soldier tokens that are TAPPED and
/// ATTACKING (real combat state), a card drawn, and 1 life lost.
#[test]
fn caesar_attack_with_sacrifice_prompts_modal_and_makes_tokens() {
    let (mut runner, caesar, fodder) = caesar_board(1, 3);

    let p0_life_before = life(&runner, P0);
    let p0_hand_before = hand_len(&runner, P0);

    attack_with_caesar(&mut runner, caesar);
    drive(&mut runner, true, Some(fodder[0]), vec![0, 1], None);

    assert_eq!(
        runner.state().objects.get(&fodder[0]).map(|o| o.zone),
        Some(Zone::Graveyard),
        "the sacrificed fodder creature must be in the graveyard"
    );

    let tokens = soldier_tokens(&runner);
    assert_eq!(
        tokens.len(),
        2,
        "mode 1 must create exactly two Soldier tokens; got {tokens:?}"
    );
    for &t in &tokens {
        let obj = runner.state().objects.get(&t).expect("token exists");
        assert!(obj.tapped, "Soldier tokens enter tapped (CR 508.4)");
        assert_eq!(obj.power, Some(1), "Soldier tokens are 1/1");
        assert_eq!(obj.toughness, Some(1), "Soldier tokens are 1/1");
    }
    // Real combat state: both tokens are registered as attackers (CR 508.4).
    let attacking: Vec<ObjectId> = runner
        .state()
        .combat
        .as_ref()
        .expect("combat is live")
        .attackers
        .iter()
        .map(|a| a.object_id)
        .collect();
    for &t in &tokens {
        assert!(
            attacking.contains(&t),
            "each Soldier token must be ATTACKING (CR 508.4); attackers: {attacking:?}"
        );
    }

    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "mode 2 must draw exactly one card"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life_before - 1,
        "mode 2 must lose exactly 1 life"
    );
}

/// TEST 3 — mode 3 dynamic damage equals the creature-token count. Choosing
/// mode 1 (tokens) + mode 3 (damage) makes two tokens, so the targeted opponent
/// takes exactly 2 damage.
#[test]
fn caesar_mode3_damage_equals_token_count() {
    let (mut runner, caesar, fodder) = caesar_board(1, 1);

    let p1_life_before = life(&runner, P1);

    attack_with_caesar(&mut runner, caesar);
    drive(&mut runner, true, Some(fodder[0]), vec![0, 2], Some(P1));

    let tokens = soldier_tokens(&runner);
    assert_eq!(tokens.len(), 2, "mode 1 made two creature tokens");

    assert_eq!(
        life(&runner, P1),
        p1_life_before - 2,
        "mode 3 deals damage equal to the number of creature tokens you control \
         (2) to the targeted opponent"
    );
}
