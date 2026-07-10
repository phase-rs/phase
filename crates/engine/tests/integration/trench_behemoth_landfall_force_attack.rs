//! Integration test: Trench Behemoth landfall → forced attack.
//!
//! Verifies both the parser AST shape and the runtime behavior:
//! 1. Parse the landfall trigger with "attacks during its controller's next
//!    combat phase if able" predicate → `GenericEffect { MustAttack }` with
//!    `Duration::UntilNextStepOf { step: EndCombat, player: Controller }`.
//! 2. No `Unimplemented` gaps in the trigger's execute effect (reach-guard).
//! 3. Runtime: a transient MustAttack effect with this duration causes the
//!    creature to be forced to attack during its controller's combat.
//! 4. Runtime: the pruner removes the effect at end of that combat.
//!
//! CR references:
//!   - CR 508.1d: A creature that must attack does so during the declare
//!     attackers step if able.
//!   - CR 603.1: Triggered abilities have a trigger condition and an effect.
//!   - CR 511.3: At end of combat, all creatures and planeswalkers are removed
//!     from combat. The combat phase ends.
use engine::game::combat::creature_must_attack;
use engine::game::layers::{
    evaluate_layers, flush_layers, prune_controller_end_combat_step_effects,
};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{ContinuousModification, Duration, Effect, PlayerScope, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;

/// Oracle text for Trench Behemoth's landfall trigger ability only.
/// The activated ability ("Return a land you control to its owner's hand: ...")
/// is omitted — it exercises no new parser path.
const TRENCH_BEHEMOTH_TRIGGER: &str =
    "Whenever a land enters the battlefield under your control, target creature an opponent controls attacks during its controller's next combat phase if able.";

/// CR 508.1d + CR 511.3: The landfall trigger's execute effect must contain a
/// `MustAttack` static with `Duration::UntilNextStepOf { step: EndCombat,
/// player: Controller }`, matching the "during its controller's next combat
/// phase if able" predicate.
#[test]
fn trench_behemoth_landfall_trigger_parses_must_attack_next_combat() {
    let parsed = parse_oracle_text(
        TRENCH_BEHEMOTH_TRIGGER,
        "Trench Behemoth",
        &[],
        &["Creature".to_string()],
        &["Kraken".to_string()],
    );

    // Exactly one trigger expected (the landfall trigger).
    assert_eq!(
        parsed.triggers.len(),
        1,
        "expected exactly one trigger from Trench Behemoth landfall text"
    );

    let trigger = &parsed.triggers[0];
    let execute = trigger
        .execute
        .as_ref()
        .expect("landfall trigger must have an execute ability");

    // Reach-guard: no Unimplemented in the trigger's effect.
    assert!(
        !matches!(*execute.effect, Effect::Unimplemented { .. }),
        "trigger effect must not be Unimplemented; got: {:?}",
        execute.effect
    );

    // The effect must be a GenericEffect containing a MustAttack static.
    let (static_abilities, duration) = match execute.effect.as_ref() {
        Effect::GenericEffect {
            static_abilities,
            duration,
            ..
        } => (static_abilities, duration),
        other => panic!("expected GenericEffect with MustAttack, got: {:?}", other),
    };

    // Verify MustAttack static is present.
    assert!(
        static_abilities
            .iter()
            .any(|s| matches!(s.mode, StaticMode::MustAttack)),
        "GenericEffect must contain a MustAttack static; got: {:?}",
        static_abilities
    );

    // Verify duration is UntilNextStepOf { step: EndCombat, player: Controller }.
    assert_eq!(
        *duration,
        Some(Duration::UntilNextStepOf {
            step: Phase::EndCombat,
            player: PlayerScope::Controller,
        }),
        "duration must be UntilNextStepOf {{ EndCombat, Controller }} for \
         'during its controller's next combat phase if able'"
    );
}

/// Reach-guard: the full Trench Behemoth Oracle text (both abilities) must not
/// produce any `Unimplemented` trigger effects.
#[test]
fn trench_behemoth_full_oracle_no_unimplemented_trigger() {
    let full_oracle = "Return a land you control to its owner's hand: Untap Trench Behemoth. It gains hexproof until end of turn.\n\
                       Whenever a land enters the battlefield under your control, target creature an opponent controls attacks during its controller's next combat phase if able.";
    let parsed = parse_oracle_text(
        full_oracle,
        "Trench Behemoth",
        &[],
        &["Creature".to_string()],
        &["Kraken".to_string()],
    );

    for (i, trigger) in parsed.triggers.iter().enumerate() {
        if let Some(exec) = &trigger.execute {
            assert!(
                !matches!(*exec.effect, Effect::Unimplemented { .. }),
                "trigger[{i}] execute must not be Unimplemented; got: {:?}",
                exec.effect
            );
        }
    }
}

// ─── Runtime tests ──────────────────────────────────────────────────────────

/// Helper: evaluate layers and check whether a creature must attack.
fn must_attack(runner: &mut engine::game::scenario::GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    creature_must_attack(runner.state(), id)
}

/// CR 508.1d + CR 511.3: A transient MustAttack effect with
/// `Duration::UntilNextStepOf { step: EndCombat, player: Controller }` forces
/// the creature to attack during its controller's combat, then expires at end
/// of that combat.
#[test]
fn trench_behemoth_runtime_must_attack_then_expires() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    // P0 controls Trench Behemoth (source of the effect).
    let behemoth = scenario.add_creature(P0, "Trench Behemoth", 7, 7).id();
    // P1 controls the target creature that must attack.
    let target = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // Simulate the landfall trigger resolution: add a transient MustAttack
    // effect targeting the opponent's creature with the correct duration.
    runner.state_mut().add_transient_continuous_effect(
        behemoth,
        P0,
        Duration::UntilNextStepOf {
            step: Phase::EndCombat,
            player: PlayerScope::Controller,
        },
        TargetFilter::SpecificObject { id: target },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttack,
        }],
        None,
    );

    // Set P1 as the active player (it's their combat step).
    runner.state_mut().active_player = P1;
    flush_layers(runner.state_mut());

    // CR 508.1d: the target creature must attack during P1's combat.
    assert!(
        must_attack(&mut runner, target),
        "target creature must be forced to attack during controller's combat"
    );

    // CR 511.3: at end of combat, the pruner removes the effect.
    prune_controller_end_combat_step_effects(runner.state_mut(), P1);
    flush_layers(runner.state_mut());

    // After pruning, the creature is no longer forced to attack.
    assert!(
        !must_attack(&mut runner, target),
        "target creature must NOT be forced to attack after end-of-combat pruning"
    );
}

/// The MustAttack effect must NOT expire during a combat phase that belongs to
/// a different player (the requirement is scoped to the controller's combat).
#[test]
fn trench_behemoth_runtime_not_pruned_during_wrong_players_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);

    let behemoth = scenario.add_creature(P0, "Trench Behemoth", 7, 7).id();
    let target = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // Install the MustAttack effect targeting P1's creature.
    runner.state_mut().add_transient_continuous_effect(
        behemoth,
        P0,
        Duration::UntilNextStepOf {
            step: Phase::EndCombat,
            player: PlayerScope::Controller,
        },
        TargetFilter::SpecificObject { id: target },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::MustAttack,
        }],
        None,
    );

    // P0 is the active player (P0's combat, NOT P1's).
    runner.state_mut().active_player = P0;
    flush_layers(runner.state_mut());

    // Prune at end of P0's combat — should NOT remove the effect because
    // the target creature is controlled by P1, not P0.
    prune_controller_end_combat_step_effects(runner.state_mut(), P0);
    flush_layers(runner.state_mut());

    // Switch to P1's combat — the creature should still be forced to attack.
    runner.state_mut().active_player = P1;
    flush_layers(runner.state_mut());

    assert!(
        must_attack(&mut runner, target),
        "target creature must still be forced to attack — P0's combat ending \
         must not prune an effect scoped to P1's combat"
    );

    // Now prune at end of P1's combat — this time it should expire.
    prune_controller_end_combat_step_effects(runner.state_mut(), P1);
    flush_layers(runner.state_mut());

    assert!(
        !must_attack(&mut runner, target),
        "target creature must no longer be forced after P1's combat ends"
    );
}

// ─── End-to-end pipeline test ───────────────────────────────────────────────

/// End-to-end: parse → resolve → prune pipeline.
///
/// Drives the full landfall trigger through the real engine pipeline:
/// 1. Trench Behemoth on the battlefield under P0, a creature under P1.
/// 2. P0 plays a land → landfall trigger fires → targets P1's creature.
/// 3. Advance to P1's combat → the creature is forced to attack.
/// 4. Advance past P1's combat → the requirement expires.
///
/// This collapses both the "parse → resolve installs transient effect" arrow
/// and the "phase advance → pruner fires" arrow into one test.
#[test]
fn trench_behemoth_e2e_landfall_forces_attack_then_expires() {
    use engine::game::triggers::drain_order_triggers_with_identity;
    use engine::types::actions::GameAction;
    use engine::types::game_state::WaitingFor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Trench Behemoth with the landfall trigger.
    let _behemoth = scenario
        .add_creature_from_oracle(P0, "Trench Behemoth", 7, 7, TRENCH_BEHEMOTH_TRIGGER)
        .id();
    // P1 controls the creature that will be forced to attack.
    let target = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();
    // A Forest in P0's hand for the landfall trigger.
    let forest_id = scenario.add_land_to_hand(P0, "Forest").id();

    let mut runner = scenario.build();

    // ── Step 1: Play the land to fire the landfall trigger ──────────────
    let card_id = runner.state().objects[&forest_id].card_id;
    runner
        .act(GameAction::PlayLand {
            object_id: forest_id,
            card_id,
        })
        .expect("P0 should be able to play a Forest from hand");

    // ── Step 2: Resolve the trigger (target P1's creature) ─────────────
    // Drive through OrderTriggers → TriggerTargetSelection → stack resolution.
    for _ in 0..100 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::TriggerTargetSelection { .. } => {
                // The only legal target is P1's creature ("target creature an
                // opponent controls").
                runner
                    .choose_first_legal_target()
                    .expect("should select P1's creature as the target");
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }

    // Sanity: the trigger resolved and installed a transient effect.
    assert!(
        !runner.state().transient_continuous_effects.is_empty(),
        "resolving the landfall trigger must install a transient MustAttack effect"
    );

    // ── Step 3: Advance to P1's combat; assert forced attack ───────────
    // Simulate the start of P1's turn.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = Phase::PreCombatMain;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner.advance_to_combat();

    // CR 508.1d: the target creature must be forced to attack.
    assert!(
        must_attack(&mut runner, target),
        "P1's creature must be forced to attack during P1's combat \
         (parse → resolve → MustAttack pipeline)"
    );

    // ── Step 4: Declare the forced attacker and drive through combat ────
    // The creature is forced to attack P0 (the only opponent).
    use engine::game::combat::AttackTarget;
    runner
        .declare_attackers(&[(target, AttackTarget::Player(P0))])
        .expect("forced creature should be able to declare attack");

    // Drive through DeclareBlockers → CombatDamage → EndCombat → PostCombatMain.
    // combat_damage() passes priority through the remaining combat steps until
    // EndCombat or PostCombatMain, which triggers the pruner (CR 511.3).
    runner.combat_damage();

    // If we're not yet at PostCombatMain, pass priority to get there.
    for _ in 0..16 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    // CR 511.3: the pruner fires at EndCombat, removing the effect.
    assert!(
        !must_attack(&mut runner, target),
        "P1's creature must no longer be forced to attack after P1's combat ends \
         (phase advance → pruner pipeline)"
    );
}
