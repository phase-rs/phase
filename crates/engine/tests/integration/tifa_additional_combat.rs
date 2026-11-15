//! Tifa, Martial Artist — the possessive first-combat-phase sentence
//! conditional on the additional-combat follow-up.
//!
//! Verbatim Oracle text (Scryfall, 2026-10-01; 4/4):
//!   "Melee (Whenever this creature attacks, it gets +1/+1 until end of turn
//!    for each opponent you attacked this combat.)
//!    Whenever one or more creatures you control with power 7 or greater deal
//!    combat damage to a player, untap all creatures you control. If it's the
//!    first combat phase of your turn, there is an additional combat phase
//!    after this phase."
//!
//! CR 500.8 + CR 506.1 — an additional combat phase is added after the phase
//! the effect resolves in. CR 109.5 — "your" is the ability's controller, so
//! the possessive phrase composes `IsYourTurn` with `FirstCombatPhaseOfTurn`.
//! CR 608.2c — the second sentence is ordinary resolution text bound to the
//! follow-up ability; the untap stays unconditional.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{ExtraPhase, GameState, WaitingFor};
use engine::types::identifiers::{ExtraPhaseId, ObjectId};
use engine::types::phase::{Phase, PhaseGroup, TurnSegment};

const TIFA: &str = "Melee (Whenever this creature attacks, it gets +1/+1 until end of turn for each opponent you attacked this combat.)\n\
Whenever one or more creatures you control with power 7 or greater deal combat damage to a player, untap all creatures you control. If it's the first combat phase of your turn, there is an additional combat phase after this phase.";

/// An unrestricted added `segment`, taken after the step `anchor` ends.
fn added(anchor: Phase, segment: TurnSegment) -> ExtraPhase {
    ExtraPhase {
        anchor,
        segment,
        attacker_restriction: None,
        attacker_restriction_source: None,
        id: ExtraPhaseId::default(),
    }
}

/// The scheduled entries with their minted `id` cleared: these tests assert
/// anchors and phases, not identities.
fn scheduled(state: &GameState) -> Vec<ExtraPhase> {
    state
        .extra_phases
        .iter()
        .map(|entry| ExtraPhase {
            id: ExtraPhaseId::default(),
            ..entry.clone()
        })
        .collect()
}

/// Tifa and a 7-power vanilla attacker under P0 at the precombat main phase.
fn tifa_fixture() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tifa = scenario
        .add_creature_from_oracle(P0, "Tifa, Martial Artist", 4, 4, TIFA)
        .id();
    let attacker = scenario.add_vanilla(P0, 7, 7);
    let runner = scenario.build();
    (runner, tifa, attacker)
}

/// Passes through the turn until `done` holds, answering each declaration with
/// `attackers` (each attacking P1; empty = no attack) and no blockers.
/// Returns every step entered on the way, in order.
fn drive_until(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    done: impl Fn(&GameRunner) -> bool,
) -> Vec<Phase> {
    let mut entered = Vec::new();
    for _ in 0..300 {
        if done(runner) {
            return entered;
        }
        let action = match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
                attacks: attackers
                    .iter()
                    .map(|id| (*id, AttackTarget::Player(P1)))
                    .collect(),
                bands: vec![],
            },
            WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
                assignments: vec![],
            },
            _ => GameAction::PassPriority,
        };
        let result = runner.act(action).expect("the turn advances");
        entered.extend(result.events.iter().filter_map(|event| match event {
            GameEvent::PhaseChanged { phase } => Some(*phase),
            _ => None,
        }));
    }
    panic!("the stop condition was never reached; entered {entered:?}");
}

/// The steps that begin a combat phase, a postcombat main phase or the ending
/// phase, in the order entered.
fn milestones(entered: &[Phase]) -> Vec<Phase> {
    entered
        .iter()
        .copied()
        .filter(|phase| {
            matches!(
                phase,
                Phase::BeginCombat | Phase::PostCombatMain | Phase::End
            )
        })
        .collect()
}

/// Row 5.3: in the first combat phase the follow-up condition is true, so one
/// additional combat phase is scheduled after this combat; the unconditional
/// untap resolves.
#[test]
fn first_combat_phase_adds_one_additional_combat() {
    let (mut runner, _tifa, attacker) = tifa_fixture();
    let p1_before = runner.life(P1);

    drive_until(&mut runner, &[attacker], |r| {
        !r.state().extra_phases.is_empty()
    });

    assert_eq!(
        runner.life(P1),
        p1_before - 7,
        "reach guard: the 7-power attacker dealt combat damage"
    );
    assert!(
        !runner.state().objects[&attacker].tapped,
        "the trigger's unconditional untap resolved"
    );
    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        1,
        "the trigger resolved in the first combat phase"
    );
    assert_eq!(
        scheduled(runner.state()),
        vec![added(
            Phase::EndCombat,
            TurnSegment::Phase(PhaseGroup::Combat)
        )],
        "exactly one additional combat phase is scheduled after this combat"
    );
}

/// Row 5.4: the added combat happens, its combat-damage trigger fires (the
/// untap proves it), but in the SECOND combat phase of the turn the
/// `FirstCombatPhaseOfTurn` conjunct is false, so no further phase is added.
#[test]
fn second_combat_phase_does_not_add_again() {
    let (mut runner, _tifa, attacker) = tifa_fixture();

    drive_until(&mut runner, &[attacker], |r| {
        !r.state().extra_phases.is_empty()
    });
    let entered = drive_until(&mut runner, &[attacker], |r| r.state().phase == Phase::End);

    assert_eq!(
        runner
            .state()
            .steps_started_this_turn
            .count(Phase::BeginCombat),
        2,
        "the added combat phase occurred exactly once"
    );
    assert_eq!(
        milestones(&entered),
        vec![Phase::BeginCombat, Phase::PostCombatMain, Phase::End],
        "exactly one added combat follows the regular combat phase, before the \
         regular postcombat main; entered {entered:?}"
    );
    assert!(
        runner.state().extra_phases.is_empty(),
        "no second additional phase may remain scheduled"
    );
    assert!(
        !runner.state().objects[&attacker].tapped,
        "reach guard: the second combat-damage trigger's untap resolved after re-declaring"
    );
}

/// Row 5.5: "of your turn" — a trigger resolving while the seat is not the
/// controller's turn skips the follow-up (the untap still resolves).
/// SYNTHETIC SETUP: the active player is flipped after the trigger is already
/// stacked, so the resolution-time `IsYourTurn` conjunct can be exercised
/// without building a whole opponent turn.
#[test]
fn not_your_turn_does_not_add_a_phase() {
    let (mut runner, tifa, attacker) = tifa_fixture();
    let p1_before = runner.life(P1);

    drive_until(&mut runner, &[attacker], |r| {
        r.state().stack.iter().any(|entry| entry.source_id == tifa)
    });
    assert_eq!(
        runner.life(P1),
        p1_before - 7,
        "reach guard: combat damage was dealt and the trigger stacked"
    );

    // Synthetic seat flip: the trigger is already on the stack; CR 109.5's
    // "your turn" now reads P1's turn, so the follow-up's IsYourTurn conjunct
    // is false.
    runner.state_mut().active_player = P1;

    runner.advance_until_stack_empty();
    assert!(
        runner.state().extra_phases.is_empty(),
        "not the controller's turn → no additional phase"
    );
    assert!(
        !runner.state().objects[&attacker].tapped,
        "the unconditional untap still resolved"
    );
}
