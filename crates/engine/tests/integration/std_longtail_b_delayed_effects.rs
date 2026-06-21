//! Standard long-tail batch B — delayed-trigger / timing / damage-anaphor
//! parser arms, exercised through the production `apply()` pipeline.
//!
//! Each test drives the parsed ability/trigger through real game actions
//! (activation, attack declaration, phase advance) and asserts an observable
//! game-state change that FLIPS if the corresponding parser arm is reverted.
//!
//! Cards covered:
//! - All-Out Assault — "When you next attack this turn, untap each creature you
//!   control" delayed `WhenNextEvent{YouAttack}` one-shot trigger.
//! - Fortune, Loyal Steed — "at end of combat" delayed-trigger prefix on an
//!   attack trigger body (`AtNextPhase{EndCombat}`).
//! - Fear of Burning Alive — "deals that amount of damage to target creature
//!   that player controls" (EventContextAmount + TriggeringPlayer binding).
//! - Y'shtola Rhul — additional end step gated by "first end step of the turn"
//!   (`FirstEndStepOfTurn`) loop guard.

use super::rules::{AttackTarget, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{DelayedTriggerCondition, Effect, QuantityRef, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

// ---------------------------------------------------------------------------
// All-Out Assault — delayed "When you next attack this turn, untap each
// creature you control".
// ---------------------------------------------------------------------------

/// CR 603.7: arming the one-shot "When you next attack this turn" delayed
/// trigger and then declaring attackers untaps each creature the controller
/// controls — including a tapped creature that is NOT attacking.
///
/// Revert assertion: without the `WhenNextEvent{YouAttack}` arm the clause
/// parses to `Effect::Unimplemented`, so resolving the ability arms nothing,
/// the attack fires no delayed trigger, and the tapped bystander stays tapped.
/// The `assert!(!tapped)` flips on revert.
#[test]
fn all_out_assault_untaps_creatures_when_you_next_attack() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A {0} activated ability carrying the exact residual clause. Activating it
    // arms the same `WhenNextEvent{YouAttack}` delayed trigger the All-Out
    // Assault ETB produces (the parser builds the identical structure for both).
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "All-Out Assault Source",
            0,
            1,
            "{0}: When you next attack this turn, untap each creature you control.",
        )
        .id();

    let attacker = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    // A second creature that will be tapped and is NOT attacking — the untap
    // must reach it too.
    let bystander = scenario.add_creature(P0, "Tapped Bystander", 1, 1).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&bystander)
        .unwrap()
        .tapped = true;

    let ability_index = runner
        .state()
        .objects
        .get(&source)
        .expect("source exists")
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::CreateDelayedTrigger { .. }))
        .expect("must parse a CreateDelayedTrigger activated ability");

    runner.activate(source, ability_index).resolve();

    // The armed delayed trigger must be a one-shot WhenNextEvent on YouAttack.
    let armed = runner
        .state()
        .delayed_triggers
        .iter()
        .any(|dt| matches!(dt.condition, DelayedTriggerCondition::WhenNextEvent { .. }));
    assert!(
        armed,
        "activating the ability must arm a WhenNextEvent delayed trigger; got {:?}",
        runner.state().delayed_triggers
    );

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    assert!(
        !runner
            .state()
            .objects
            .get(&bystander)
            .expect("bystander exists")
            .tapped,
        "the 'when you next attack' delayed trigger must untap each creature you \
         control, including the non-attacking tapped bystander"
    );
}

// ---------------------------------------------------------------------------
// Fortune, Loyal Steed — "at end of combat" delayed-trigger prefix.
// ---------------------------------------------------------------------------

/// CR 511.2 + CR 603.7a: an attack trigger whose body begins "at end of combat,
/// ..." schedules an `AtNextPhase{EndCombat}` delayed trigger instead of
/// resolving immediately.
///
/// Revert assertion: without the "at end of combat, " prefix arm (in
/// `strip_temporal_prefix` + the comma-split guard), the body parses with an
/// `Effect::Unimplemented("at end of combat")` head and no delayed trigger is
/// scheduled. `assert!(scheduled)` flips on revert.
#[test]
fn fortune_schedules_end_of_combat_delayed_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fortune = scenario
        .add_creature_from_oracle(
            P0,
            "Fortune, Loyal Steed",
            2,
            3,
            "Whenever Fortune attacks, at end of combat, exile it, then return it to \
             the battlefield under its owner's control.",
        )
        .id();

    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(fortune, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("DeclareAttackers should succeed");
    runner.advance_until_stack_empty();

    let scheduled = runner.state().delayed_triggers.iter().any(|dt| {
        matches!(
            dt.condition,
            DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat
            }
        )
    });
    assert!(
        scheduled,
        "Fortune's attack must schedule an AtNextPhase{{EndCombat}} delayed trigger; \
         got {:?}",
        runner.state().delayed_triggers
    );
}

// ---------------------------------------------------------------------------
// Fear of Burning Alive — "deals that amount of damage to target creature that
// player controls".
// ---------------------------------------------------------------------------

/// CR 120.1: the Delirium trigger body parses "that amount of damage" as the
/// just-dealt damage amount (`EventContextAmount`) and binds "target creature
/// that player controls" to the damaged opponent (`TriggeringPlayer`).
///
/// This is a parse-shape assertion that two independent things hold together:
/// (1) the damage amount is `EventContextAmount` (the "that amount of damage"
/// arm), and (2) the target controller is `TriggeringPlayer` (the opponent
/// recipient widening of `is_damage_done_trigger_pattern`). Reverting either arm
/// changes one of these — without arm (1) the whole body is `Unimplemented`;
/// without arm (2) the controller is `You`. The runtime semantics (deal damage
/// equal to the noncombat damage to a creature the damaged opponent controls)
/// have no shipped honest-fallback, so this shape assertion documents the
/// supported lowering; the runtime DealDamage path itself is long-established.
#[test]
fn fear_of_burning_alive_amount_and_triggering_player_binding() {
    let parsed = parse_oracle_text(
        "When this creature enters, it deals 4 damage to each opponent.\nDelirium — \
         Whenever a source you control deals noncombat damage to an opponent, if there \
         are four or more card types among cards in your graveyard, this creature deals \
         that amount of damage to target creature that player controls.",
        "Fear of Burning Alive",
        &[],
        &["Creature".to_string()],
        &[],
    );

    let delirium = parsed
        .triggers
        .iter()
        .find_map(|t| {
            let exec = t.execute.as_deref()?;
            match exec.effect.as_ref() {
                Effect::DealDamage { amount, target, .. } => Some((amount.clone(), target.clone())),
                _ => None,
            }
        })
        .expect("Delirium trigger must lower to a DealDamage effect, not Unimplemented");

    let (amount, target) = delirium;
    assert!(
        matches!(
            amount,
            engine::types::ability::QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            }
        ),
        "amount must be the just-dealt damage (EventContextAmount), got {amount:?}"
    );
    assert!(
        matches!(
            target,
            TargetFilter::Typed(ref tf)
                if tf.controller == Some(engine::types::ability::ControllerRef::TriggeringPlayer)
        ),
        "target must bind 'that player controls' to the damaged opponent \
         (TriggeringPlayer), got {target:?}"
    );
}

// ---------------------------------------------------------------------------
// Y'shtola Rhul — additional end step gated by "first end step of the turn".
// ---------------------------------------------------------------------------

/// CR 500.8 + CR 513.1: the end-step trigger schedules an additional end step
/// ONLY during the first end step of the turn. The `FirstEndStepOfTurn` gate
/// reads `state.end_steps_started_this_turn`; on the second (extra) end step the
/// gate is false, so no further end step is scheduled — the turn does not loop.
///
/// Revert assertion: without the end-step counter + `FirstEndStepOfTurn`
/// condition, the gate is dropped, the additional end step is scheduled on every
/// end step, and the loop never terminates (the bounded action loop below would
/// exhaust its iteration guard). The guarded termination is the discriminator.
#[test]
fn yshtola_schedules_exactly_one_extra_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Y'shtola's end-step trigger. A creature to exile/return so the trigger has
    // a legal target. The additional-end-step clause is gated by "first end step
    // of the turn".
    scenario.add_creature_from_oracle(
        P0,
        "Y'shtola Rhul",
        2,
        3,
        "At the beginning of your end step, exile target creature you control, then \
         return it to the battlefield under its owner's control. Then if it's the \
         first end step of the turn, there is an additional end step after this step.",
    );

    let mut runner = scenario.build();

    // Advance P0's turn to (and through) its end step(s). With the loop guard the
    // turn rolls over to P1; without it, P0's end step recurs forever and the
    // bounded loop trips its own assertion. Track the peak per-turn end-step
    // counter while on P0's turn (it resets at the start of P1's turn). The two
    // end steps occur back-to-back with no intervening non-End phase, so the
    // counter — not a phase-edge detector — is the reliable observation.
    let mut peak_p0_end_steps = 0u32;
    for _ in 0..600 {
        let state = runner.state();
        if state.active_player == P0 {
            peak_p0_end_steps = peak_p0_end_steps.max(state.end_steps_started_this_turn);
        }
        // Stop once we have safely reached P1's turn — proves the loop terminated.
        if state.active_player == P1 {
            break;
        }
        let acted = match &state.waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            other => panic!("unexpected wait state during turn advance: {other:?}"),
        };
        acted.expect("action during turn advance should succeed");
    }

    assert_eq!(
        runner.state().active_player,
        P1,
        "the turn must roll over to P1 — the additional end step must NOT loop"
    );
    // CR 500.8: the first end step schedules exactly one extra; the extra end
    // step's gate (FirstEndStepOfTurn, false on the second) schedules no further
    // step, so P0 reaches exactly two end steps this turn.
    assert_eq!(
        peak_p0_end_steps, 2,
        "Y'shtola must add exactly one additional end step (two total), not zero \
         and not an unbounded number"
    );
}
