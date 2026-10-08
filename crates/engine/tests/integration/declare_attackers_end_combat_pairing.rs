//! Public-API positive controls for the `(EndCombat, DeclareAttackers)` wedge
//! fix (CR 508.8 / CR 511.1 / CR 514.3a).
//!
//! **All three tests here pass BOTH before and after the fix.** That is their
//! entire purpose: they prove the discriminating assertions in
//! `crates/engine/src/game/turns_declare_attackers_wedge_tests.rs` are not
//! trivially true, and that the healthy paths are unchanged by the fix.
//!
//! * `declare_no_attackers_reaches_end_combat_priority` — non-vacuity for the
//!   drain rows: with an EMPTY deferred queue the empty declaration already
//!   reaches `Priority` at `Phase::EndCombat`.
//! * `start_game_skip_mulligan_with_empty_queue_reaches_upkeep_priority` —
//!   non-vacuity for row A11: the pipeline-free game-start walk reaches
//!   `Upkeep` with an empty stack when nothing is parked.
//! * `cleanup_with_empty_queue_advances_the_turn` — non-vacuity for row A12's
//!   `turn_number == recorded` assertion: an ordinary end-of-turn pass still
//!   advances the turn, so the CR 514.3a pause does not stall normal turns.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// Row A4 — positive control / non-vacuity.
///
/// CR 508.8: declaring no attackers skips the declare blockers and combat
/// damage steps. CR 511.1: end of combat has no turn-based actions and the
/// active player gets priority.
///
/// FIXTURE NOTE: the declaration prompt is installed by walking the real turn
/// machinery from `BeginCombat`, not by `at_phase(Phase::DeclareAttackers)` —
/// `at_phase` sets `waiting_for` to `Priority`, so submitting `DeclareAttackers`
/// there is rejected with `ActionNotAllowed`. Starting the walk at `BeginCombat`
/// also keeps it clear of the draw step, whose empty scenario library would end
/// the game (CR 704.5b).
#[test]
fn declare_no_attackers_reaches_end_combat_priority() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::BeginCombat);
    scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::DeclareAttackers);

    assert_eq!(
        runner.state().phase,
        Phase::DeclareAttackers,
        "the walk must reach the declaration step"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ),
        "CR 508.1: the declaration prompt must be live, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        })
        .expect("declaring no attackers must succeed");

    let state = runner.state();
    assert_eq!(state.phase, Phase::EndCombat, "CR 508.8");
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == state.active_player),
        "CR 511.1: expected Priority for the active player, got {:?}",
        state.waiting_for
    );
    assert!(
        state.deferred_triggers.is_empty(),
        "nothing was parked, so nothing may be queued"
    );
}

/// Row A11's negative sibling — non-vacuity for the game-start walk.
///
/// With no parked queue, `start_game_skip_mulligan` reaches the first upkeep
/// with `Priority` and an EMPTY stack (CR 501.1, CR 502.4, CR 503.1).
#[test]
fn start_game_skip_mulligan_with_empty_queue_reaches_upkeep_priority() {
    let scenario = GameScenario::new();
    let mut runner = scenario.build();

    let result = engine::game::start_game_skip_mulligan(runner.state_mut());

    let state = runner.state();
    assert_eq!(state.phase, Phase::Upkeep, "CR 501.1 + CR 502.4");
    assert!(
        matches!(result.waiting_for, WaitingFor::Priority { player } if player == state.active_player),
        "CR 503.1: expected Priority for the active player, got {:?}",
        result.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "nothing was parked, so the stack must stay empty, got {:?}",
        state.stack
    );
    assert!(state.deferred_triggers.is_empty());
}

/// Row A13 — A12's negative sibling.
///
/// An end-of-turn pass with an EMPTY deferred queue advances the turn exactly as
/// it does today. This proves A12's `turn_number == recorded` assertion is not
/// trivially satisfiable, and that the CR 514.3a cleanup pause does not stall
/// ordinary turns.
///
/// FIXTURE NOTE: placed directly at the end step with `at_phase(Phase::End)`
/// rather than walked there — a `GameScenario` library is empty, so a walk that
/// crosses the draw step ends the game (CR 704.5b) and no seat can submit.
#[test]
fn cleanup_with_empty_queue_advances_the_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    // A noncreature permanent only: a legal attacker could park a combat walk
    // at `Phase::DeclareAttackers`.
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::White);
    let mut runner = scenario.build();

    let turn_before = runner.state().turn_number;
    assert_eq!(runner.state().phase, Phase::End);
    assert!(runner.state().deferred_triggers.is_empty());

    // Pass until the turn rolls over.
    for _ in 0..8 {
        if runner.state().turn_number > turn_before {
            break;
        }
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority must succeed");
    }

    let state = runner.state();
    assert_eq!(
        state.turn_number,
        turn_before + 1,
        "an ordinary end-of-turn pass must advance the turn"
    );
    assert_eq!(
        state.active_player, P1,
        "the turn must pass to the other seat"
    );
    assert!(state.deferred_triggers.is_empty());
}
