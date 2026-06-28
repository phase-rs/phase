//! False Peace (POR) / Empty City Ruse (PTK): "Target player/opponent skips all
//! combat phases of their next turn."
//!
//! These tests drive the REAL phase-flow pipeline (`start_next_turn` +
//! `advance_phase`) rather than inspecting store contents, so they fail if the
//! §5 replacement applier/injection or the §4 turn-flow promotion/clear is
//! reverted.
//!
//! Runtime model (CR 614.10 + CR 614.10a + CR 500.11):
//! - Each effect arms one `pending` skip on `combat_phase_skip_next_turn[P]`.
//! - `start_next_turn` binds one pending skip (`pending -= 1`, `active = true`)
//!   on P's first NON-skipped turn (CR 614.10a: it waits past skipped turns).
//! - While `active`, a virtual BeginPhase replacement prevents every combat
//!   phase that turn (including extra combat phases).
//! - `start_next_turn` releases the binding (`active = false`) at the start of
//!   P's following turn; combat is normal again unless another pending skip
//!   rebinds. So two stacked skips skip combat on P's next two non-skipped turns
//!   (CR 614.10a: stacked "skip next" effects are independently satisfied).

use engine::game::effects::skip_next_step;
use engine::game::turns::{advance_phase, start_next_turn};
use engine::types::ability::{
    Effect, QuantityExpr, ResolvedAbility, SkipScope, StepSkipTarget, TargetFilter, TargetRef,
};
use engine::types::game_state::{CombatPhaseSkipState, ExtraPhase, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Build the False Peace effect: a turn-scoped combat skip targeting `player`.
/// The target player (not the controller) drives resolution.
fn skip_all_combat_ability(player: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::SkipNextStep {
            target: TargetFilter::Player,
            step: StepSkipTarget::CombatPhase,
            count: QuantityExpr::Fixed { value: 1 },
            scope: SkipScope::AllOfNextTurn,
        },
        vec![TargetRef::Player(player)],
        ObjectId(999),
        PlayerId(0),
    )
}

/// Resolve the effect to arm one more pending skip for `player`. Safe to call
/// repeatedly (CR 614.10a stacking) — each call increments `pending` by one and
/// must not bind a turn on its own.
fn arm_skip(state: &mut GameState, player: PlayerId) {
    let before = state.combat_phase_skip_next_turn[player.0 as usize].pending;
    let ability = skip_all_combat_ability(player);
    let mut events = Vec::new();
    skip_next_step::resolve(state, &ability, &mut events).expect("skip resolves");
    let slot = state.combat_phase_skip_next_turn[player.0 as usize];
    assert_eq!(
        slot.pending,
        before + 1,
        "effect must arm one more pending skip"
    );
    assert!(!slot.active, "arming alone must not bind a turn");
}

/// Drive `advance_phase` from PreCombatMain and report the phase the active
/// player lands in after the (possibly-skipped) combat phase, plus the
/// combat-phase counter for the turn.
fn run_combat_segment(state: &mut GameState) -> (Phase, u32) {
    state.phase = Phase::PreCombatMain;
    state.combat_phases_started_this_turn = 0;
    let mut events = Vec::new();
    advance_phase(state, &mut events);
    (state.phase, state.combat_phases_started_this_turn)
}

/// (a) The bound turn never enters any combat step: advancing from PreCombatMain
/// lands directly in PostCombatMain and `combat_phases_started_this_turn` stays
/// 0. Fails if the applier/injection is reverted (combat would run normally).
#[test]
fn bound_turn_skips_all_combat_phases() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);

    // Arm for P1 (who becomes active after start_next_turn) and promote.
    arm_skip(&mut state, PlayerId(1));
    let mut events = Vec::new();
    start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 0,
            active: true
        },
        "the pending skip must bind (active) on the bound turn"
    );

    let (phase, combats) = run_combat_segment(&mut state);
    assert_eq!(
        phase,
        Phase::PostCombatMain,
        "combat must be skipped straight to postcombat main"
    );
    assert_eq!(combats, 0, "no combat phase may begin on the bound turn");
}

/// (b) An EXTRA combat phase scheduled that turn is ALSO skipped. This
/// discriminates the turn-scope (AllOfNextTurn) from a finite count:1 skip —
/// a count:1 skip would consume on the first combat and let the extra one run.
#[test]
fn extra_combat_phase_on_bound_turn_is_also_skipped() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    arm_skip(&mut state, PlayerId(1));
    let mut events = Vec::new();
    start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, PlayerId(1));

    // Schedule an extra combat phase after EndCombat (Aurelia/Najeela pattern).
    // When the main combat is skipped, the phase cascade still reaches EndCombat
    // (skipped), which pops this extra entry and inserts an extra BeginCombat —
    // which, because the marker is Active, is ALSO prevented. A finite count:1
    // skip would have been consumed by the first combat and let this one run.
    state.extra_phases.push(ExtraPhase {
        anchor: Phase::EndCombat,
        phase: Phase::BeginCombat,
    });

    // Single segment drives the whole combat cascade including the extra combat.
    let (phase, combats) = run_combat_segment(&mut state);
    assert_eq!(
        combats, 0,
        "neither the main nor the extra combat phase may begin while Active"
    );
    assert_eq!(
        phase,
        Phase::PostCombatMain,
        "after both combats are skipped, the turn reaches postcombat main"
    );
    assert!(
        state.extra_phases.is_empty(),
        "the extra combat phase entry was consumed (and skipped), not left pending"
    );
}

/// (c) CR 614.10a wait: with `turns_to_skip[P] > 0` AND a Pending combat skip,
/// the combat skip lands on P's first NON-skipped turn, not the skipped one.
#[test]
fn combat_skip_waits_past_a_skipped_turn() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);

    // P1 will skip their next whole turn AND has a pending combat skip.
    arm_skip(&mut state, PlayerId(1));
    state.turns_to_skip[1] = 1;

    // start_next_turn: P1's turn is skipped entirely (turns_to_skip fast-path),
    // recursing to P0's turn. The combat skip must NOT bind to the skipped turn.
    let mut events = Vec::new();
    start_next_turn(&mut state, &mut events);
    assert_eq!(
        state.active_player,
        PlayerId(0),
        "P1's turn was skipped; active returns to P0"
    );
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 1,
            active: false
        },
        "combat skip must still be pending — it did not bind to the skipped turn"
    );

    // P0's turn (active now) has no marker -> normal combat.
    let (phase, combats) = run_combat_segment(&mut state);
    assert_eq!(phase, Phase::BeginCombat);
    assert_eq!(combats, 1, "P0 has normal combat");

    // Advance to P1's next (non-skipped) turn: now the combat skip binds.
    state.phase = Phase::Cleanup;
    let mut events2 = Vec::new();
    start_next_turn(&mut state, &mut events2);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 0,
            active: true
        },
        "combat skip binds to P1's first non-skipped turn"
    );
    let (phase2, combats2) = run_combat_segment(&mut state);
    assert_eq!(phase2, Phase::PostCombatMain, "P1's combat is now skipped");
    assert_eq!(combats2, 0);
}

/// (d) Cleared after one turn: the turn AFTER the bound turn has normal combat.
#[test]
fn marker_clears_after_bound_turn() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    arm_skip(&mut state, PlayerId(1));

    // P1's bound turn (combat skipped).
    let mut events = Vec::new();
    start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 0,
            active: true
        }
    );
    let (phase, combats) = run_combat_segment(&mut state);
    assert_eq!(phase, Phase::PostCombatMain);
    assert_eq!(combats, 0);

    // P0's turn.
    state.phase = Phase::Cleanup;
    let mut events2 = Vec::new();
    start_next_turn(&mut state, &mut events2);
    assert_eq!(state.active_player, PlayerId(0));

    // P1's NEXT turn: the marker was cleared at the start of this following turn.
    state.phase = Phase::Cleanup;
    let mut events3 = Vec::new();
    start_next_turn(&mut state, &mut events3);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState::default(),
        "marker must be cleared after the bound turn"
    );
    let (phase2, combats2) = run_combat_segment(&mut state);
    assert_eq!(phase2, Phase::BeginCombat, "P1 has normal combat again");
    assert_eq!(combats2, 1);
}

/// (e) Negative: a player with no marker plays normal combat — guards against
/// the virtual replacement over-matching.
#[test]
fn player_without_marker_has_normal_combat() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    let mut events = Vec::new();
    start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState::default()
    );

    let (phase, combats) = run_combat_segment(&mut state);
    assert_eq!(phase, Phase::BeginCombat, "no marker -> normal combat");
    assert_eq!(combats, 1);
}

/// (f) CR 614.10a (the regression guard for stacked skips): two `AllOfNextTurn`
/// skips aimed at the same player before their next turn make that player skip
/// combat on their next TWO non-skipped turns — one effect is satisfied by the
/// first skipped turn, the other waits and binds to the second. This FAILS on
/// the original single-marker model, which collapsed both into one skipped turn.
#[test]
fn two_stacked_skips_skip_combat_on_next_two_turns() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);

    // Two False Peace effects resolve at P1 before their next turn.
    arm_skip(&mut state, PlayerId(1));
    arm_skip(&mut state, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 2,
            active: false
        },
        "two stacked skips accumulate to pending: 2"
    );

    // P1's first turn: one skip binds, combat skipped, one still pending.
    let mut events = Vec::new();
    start_next_turn(&mut state, &mut events);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 1,
            active: true
        },
        "first turn binds one skip; the second remains pending"
    );
    let (phase, combats) = run_combat_segment(&mut state);
    assert_eq!(phase, Phase::PostCombatMain);
    assert_eq!(combats, 0, "P1's first turn skips combat");

    // P0's intervening turn (no marker -> normal combat).
    state.phase = Phase::Cleanup;
    let mut events2 = Vec::new();
    start_next_turn(&mut state, &mut events2);
    assert_eq!(state.active_player, PlayerId(0));

    // P1's SECOND turn: the first binding released, the second skip now binds.
    state.phase = Phase::Cleanup;
    let mut events3 = Vec::new();
    start_next_turn(&mut state, &mut events3);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState {
            pending: 0,
            active: true
        },
        "second skip binds to P1's next turn (CR 614.10a)"
    );
    let (phase2, combats2) = run_combat_segment(&mut state);
    assert_eq!(
        phase2,
        Phase::PostCombatMain,
        "P1's second turn also skips combat"
    );
    assert_eq!(combats2, 0);

    // P0's turn, then P1's THIRD turn: both skips satisfied -> normal combat.
    state.phase = Phase::Cleanup;
    let mut events4 = Vec::new();
    start_next_turn(&mut state, &mut events4);
    assert_eq!(state.active_player, PlayerId(0));
    state.phase = Phase::Cleanup;
    let mut events5 = Vec::new();
    start_next_turn(&mut state, &mut events5);
    assert_eq!(state.active_player, PlayerId(1));
    assert_eq!(
        state.combat_phase_skip_next_turn[1],
        CombatPhaseSkipState::default(),
        "both skips satisfied; marker cleared"
    );
    let (phase3, combats3) = run_combat_segment(&mut state);
    assert_eq!(
        phase3,
        Phase::BeginCombat,
        "P1's third turn has normal combat"
    );
    assert_eq!(combats3, 1);
}
