//! Fblthp, Impossibly Lost (Reality Fracture, `FRA`) — runtime coverage for the
//! passive received-damage trigger head.
//!
//! Oracle text (MTGJSON `AtomicCards.json`):
//! > When one or more of your opponents are dealt combat damage during your
//! > turn, draw two cards. If your library has no cards in it, you win the game.
//! > Fblthp's owner shuffles him into their library. (If you draw from an empty
//! > library this way, you still win the game.)
//!
//! Ruling (MTGJSON, 2026-08-21):
//! > Losing the game due to drawing from an empty library happens as a
//! > state-based action. … Since Fblthp's ability causes you to win the game
//! > while that ability is still resolving, this happens before those
//! > state-based actions would be checked and performed. Notably, drawing cards
//! > from an empty library at any other time while Fblthp is on the battlefield
//! > will still cause you to lose the game.
//!
//! Production entry points: the combat-damage step emits the damage event that
//! the trigger head matches (CR 510.2), and `combat_damage()` drives the
//! resulting trigger to completion. Every negative row pairs a reach guard
//! proving its input got past the upstream short-circuit the negation relies on.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const ORACLE: &str = "When one or more of your opponents are dealt combat damage during your turn, draw two cards. If your library has no cards in it, you win the game. Fblthp's owner shuffles him into their library.";

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player must exist")
}

fn library_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .expect("player must exist")
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

/// Drive the declared attackers through combat damage to the end of combat.
///
/// CR 508.2: the active player gets priority after attackers are declared.
/// CR 509.1a: a defending player with no creatures has nothing to declare, so
/// the engine auto-declares no blockers and never surfaces a `DeclareBlockers`
/// prompt. The production `combat_damage()` driver passes priority through the
/// declare-blockers and combat-damage steps (CR 510.2) and drains the ordering
/// prompt for the damage triggers it creates (CR 603.3b). A single batching
/// trigger needs no ordering prompt, so it resolves inside this drive; the rows
/// assert its outcome (draw count, shuffle destination, win) rather than a
/// transient stack entry.
fn drive_combat_damage(runner: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    drive_to_declare_attackers(runner);
    runner
        .declare_attackers(attacks)
        .expect("attackers should be legal");
    let _ = runner.combat_damage();
}

/// CR 603.2c + CR 120.4b + CR 510.2: combat damage to two opponents is one trigger
/// event → exactly one trigger; CR 121.1: draw two; card text: then shuffle him
/// into his owner's library.
#[test]
fn combat_damage_to_two_opponents_is_one_trigger() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C", "Lib D", "Lib E"] {
        scenario.add_card_to_library_top(P0, name);
    }
    let fblthp = scenario
        .add_creature_from_oracle(P0, "Fblthp, Impossibly Lost", 1, 1, ORACLE)
        .id();
    let a1 = scenario.add_creature(P0, "Attacker One", 1, 1).id();
    let a2 = scenario.add_creature(P0, "Attacker Two", 1, 1).id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    drive_combat_damage(
        &mut runner,
        &[
            (a1, AttackTarget::Player(P1)),
            (a2, AttackTarget::Player(P2)),
        ],
    );

    assert_eq!(runner.life(P1), 19, "reach guard: P1 took combat damage");
    assert_eq!(runner.life(P2), 19, "reach guard: P2 took combat damage");
    assert_eq!(hand_len(&runner, P0), hand_before + 2, "draw two cards");
    assert_eq!(
        library_len(&runner, P0),
        4,
        "5 − 2 drawn + Fblthp shuffled in"
    );
    assert_eq!(runner.state().objects[&fblthp].zone, Zone::Library);
    assert!(runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0")
        .library
        .contains(&fblthp));
}

/// CR 102.1: the trigger event requires the controller's turn. P1 is active and
/// damages P2 — P2 is an opponent of P0, so the recipient filter ALONE would
/// fire; only the EventTime window suppresses it. The reach guard (P2 lost 1)
/// proves the only failing axis is the window.
#[test]
fn combat_damage_outside_the_controllers_turn_does_not_fire() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Lib A", "Lib B", "Lib C", "Lib D", "Lib E"] {
        scenario.add_card_to_library_top(P0, name);
    }
    let fblthp = scenario
        .add_creature_from_oracle(P0, "Fblthp, Impossibly Lost", 1, 1, ORACLE)
        .id();
    let attacker = scenario.add_creature(P1, "P1 Attacker", 1, 1).id();
    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P2))])
        .expect("P1 attacks P2");
    let hand_before = hand_len(&runner, P0);

    // Negative row: `advance_until_stack_empty` returns immediately on an empty
    // stack, so it cannot drive the combat-damage step here; `combat_damage()`
    // is the production driver that passes priority through the step. Only the
    // runner's state accessors are read below — no `Outcome` accessor.
    let _ = runner.combat_damage();

    assert_eq!(
        runner.life(P2),
        19,
        "reach guard: P2 is an opponent of P0 and took damage"
    );
    assert_eq!(
        hand_len(&runner, P0),
        hand_before,
        "outside P0's turn the trigger must not fire"
    );
    assert_eq!(runner.state().objects[&fblthp].zone, Zone::Battlefield);
    assert!(runner.state().stack.iter().all(|e| e.source_id != fblthp));
}

/// Card text + CR 104.2b: the win check reads the library AFTER the draw, so
/// drawing the last two cards wins. A pre-draw check would leave no winner.
#[test]
fn win_after_the_draw_empties_the_library() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["L1", "L2"] {
        scenario.add_card_to_library_top(P0, name);
    }
    scenario.add_creature_from_oracle(P0, "Fblthp, Impossibly Lost", 1, 1, ORACLE);
    let attacker = scenario.add_creature(P0, "Attacker", 1, 1).id();
    let mut runner = scenario.build();
    let hand_before = hand_len(&runner, P0);

    drive_combat_damage(&mut runner, &[(attacker, AttackTarget::Player(P1))]);

    assert_eq!(
        runner.life(P1),
        19,
        "reach guard: combat damage landed on an opponent"
    );
    assert_eq!(hand_len(&runner, P0), hand_before + 2, "draw two cards");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert!(runner
        .state()
        .players
        .iter()
        .all(|p| p.id == P0 || p.is_eliminated));
    // CR 704.5b never fires for P0 here: the library held exactly two cards and
    // the draw took exactly two, so `attempted_empty_library` is `2 < 2 == false`
    // (game/effects/draw.rs) and `drew_from_empty_library` stays false; the
    // terminal player-loss SBA net therefore has nothing to eliminate P0 for.
    assert!(
        !runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P0)
            .expect("P0")
            .is_eliminated
    );
}

/// Ruling 2026-08-21 + CR 121.4 / CR 704.5b: the ability's own resolution names P0
/// the winner even though its draw came from an empty library. This row asserts
/// the win outcome and the empty-library fact only; it deliberately asserts
/// nothing about P0's elimination state (see the follow-up note below).
#[test]
fn win_survives_a_draw_from_an_empty_library() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // No library cards at all: the Draw 2 is an empty-library draw.
    scenario.add_creature_from_oracle(P0, "Fblthp, Impossibly Lost", 1, 1, ORACLE);
    let attacker = scenario.add_creature(P0, "Attacker", 1, 1).id();
    let mut runner = scenario.build();

    drive_combat_damage(&mut runner, &[(attacker, AttackTarget::Player(P1))]);

    assert_eq!(
        runner.life(P1),
        19,
        "reach guard: combat damage landed on an opponent"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "the win resolved even though the draw came from an empty library"
    );
    let p0 = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0");
    assert!(
        p0.drew_from_empty_library,
        "the draw really came from an empty library"
    );
    assert_eq!(hand_len(&runner, P0), 0, "there was nothing to draw");
}

// Follow-up note (report-only; no code in this change). The row above does not
// prove the ruling's "not eliminated" clause. Today the engine's terminal
// player-loss SBA safety net runs after the win lands: `reconcile_terminal_sba`
// (`crates/engine/src/game/engine.rs`) calls `sba::has_pending_player_loss_sba`
// and then `sba::check_state_based_actions`, and neither reads
// `state.waiting_for == WaitingFor::GameOver` nor a `game_end` flag — so
// `collect_draw_from_empty_losers` (`crates/engine/src/game/sba.rs`) still
// eliminates P0 on the `drew_from_empty_library` flag that the ability's own
// draw set. That is a core-engine defect in `engine.rs`/`sba.rs`, outside this
// parser-only change's scope: follow-up issue (filed separately).
