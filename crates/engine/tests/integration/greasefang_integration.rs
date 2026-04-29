//! Integration tests for Greasefang, Okiba Boss.
//!
//! Validates the full sequence of Greasefang's combat trigger:
//! 1. Trigger fires at the beginning of combat, returning a Vehicle from graveyard.
//! 2. The Vehicle enters the battlefield and gains haste.
//! 3. A delayed trigger is registered to bounce the Vehicle at the beginning of
//!    the controller's next end step (CR 603.7a).

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GREASEFANG_ORACLE: &str =
    "At the beginning of combat on your turn, return target Vehicle card from your graveyard to the battlefield. It gains haste. Return it to its owner's hand at the beginning of your next end step.";

/// Advance through game states until the given phase is active.
///
/// Handles all WaitingFor variants that block phase progression:
/// - Priority: pass it
/// - DeclareAttackers: declare no attackers (Greasefang can attack but we skip)
/// - DeclareBlockers: declare no blockers
/// - TriggerTargetSelection / TargetSelection: stops so caller can handle it
fn advance_to_phase(runner: &mut engine::game::scenario::GameRunner, target_phase: Phase) {
    for _ in 0..60 {
        if runner.state().phase == target_phase {
            break;
        }
        match &runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.act(GameAction::DeclareAttackers { attacks: vec![] });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            // Stop and let the caller handle target selection
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                break;
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// Resolve all pending triggers and target selections until Priority with empty stack.
///
/// Automatically selects the first legal target whenever target selection is requested.
fn flush_triggers(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..60 {
        match &runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                let target = target_slots
                    .first()
                    .and_then(|slot| slot.legal_targets.first())
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target })
                    .expect("trigger target selection should succeed");
            }
            WaitingFor::DeclareAttackers { .. } => {
                let _ = runner.act(GameAction::DeclareAttackers { attacks: vec![] });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------

/// Greasefang's trigger fires at BeginCombat, returning the target Vehicle from
/// the graveyard to the battlefield. The Vehicle gains haste and a delayed trigger
/// is registered to bounce it at the beginning of the controller's next end step.
#[test]
fn greasefang_returns_vehicle_gains_haste_then_bounced_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Greasefang, Okiba Boss on the battlefield (entered previous turn — no summoning sickness)
    let _greasefang_id = scenario
        .add_creature_from_oracle(P0, "Greasefang, Okiba Boss", 4, 3, GREASEFANG_ORACLE)
        .id();

    // Parhelion II: a 5/5 legendary artifact — Vehicle (flying, first strike, vigilance, crew 4).
    // We build it as an artifact with the Vehicle subtype so Greasefang's trigger can target it.
    // It starts on the battlefield so we can immediately move it to the graveyard via state_mut.
    let parhelion_id = scenario
        .add_creature(P0, "Parhelion II", 5, 5)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .id();
    let parhelion2_id = scenario
        .add_creature(P0, "Parhelion II", 5, 5)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .id();

    let mut runner = scenario.build();

    // ── Move Parhelion II to P0's graveyard ───────────────────────────────────
    {
        let state = runner.state_mut();
        zones::remove_from_zone(state, parhelion_id, Zone::Battlefield, P0);
        zones::add_to_zone(state, parhelion_id, Zone::Graveyard, P0);
        state.objects.get_mut(&parhelion_id).unwrap().zone = Zone::Graveyard;
    }

    // ── Move Parhelion II to P0's graveyard ───────────────────────────────────
    {
        let state = runner.state_mut();
        zones::remove_from_zone(state, parhelion2_id, Zone::Battlefield, P0);
        zones::add_to_zone(state, parhelion2_id, Zone::Graveyard, P0);
        state.objects.get_mut(&parhelion2_id).unwrap().zone = Zone::Graveyard;
    }

    assert_eq!(
        runner.state().objects[&parhelion_id].zone,
        Zone::Graveyard,
        "Parhelion II should start in P0's graveyard"
    );

    // ── Advance to BeginCombat, where Greasefang's trigger fires ─────────────
    // The trigger requires target selection (Vehicle in graveyard), so the loop
    // breaks as soon as TriggerTargetSelection is reached.
    advance_to_phase(&mut runner, Phase::BeginCombat);
    // Engine should be asking for the graveyard Vehicle target.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Expected TriggerTargetSelection for Greasefang's trigger, got: {:?}",
        runner.state().waiting_for
    );

    // Parhelion II is the only Vehicle in the graveyard — it must be the legal target.
    let target_id = match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            let legal = &target_slots[0].legal_targets;
            assert_eq!(legal.len(), 2, "Exactly one legal target (Parhelion II)");
            match &legal[0] {
                TargetRef::Object(id) => *id,
                other => panic!("Expected Object target, got {other:?}"),
            }
        }
        _ => unreachable!(),
    };
    assert_eq!(
        target_id, parhelion_id,
        "The legal trigger target should be Parhelion II"
    );

    // ── Select Parhelion II and resolve the trigger chain ────────────────────
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(parhelion_id)),
        })
        .expect("selecting Parhelion II should succeed");

    // Resolve: ChangeZone (GY → BF) → GenericEffect (gains haste) → CreateDelayedTrigger
    runner.advance_until_stack_empty();

    // ── Assert: Parhelion is on the battlefield ───────────────────────────────
    let parhelion_obj = &runner.state().objects[&parhelion_id];
    assert_eq!(
        parhelion_obj.zone,
        Zone::Battlefield,
        "Parhelion II should be on the battlefield after Greasefang's trigger"
    );

    // ── Assert: Parhelion has haste ───────────────────────────────────────────
    assert!(
        parhelion_obj.keywords.contains(&Keyword::Haste),
        "Parhelion II should have haste granted by Greasefang's trigger"
    );

    // ── Assert: one delayed trigger registered for end-step bounce ────────────
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Exactly one delayed trigger (bounce at beginning of next end step) should be registered"
    );

    // ── Advance through the rest of combat and PostCombatMain to End step ─────
    // We skip attackers to keep the test focused on the delayed-trigger bounce.
    advance_to_phase(&mut runner, Phase::End);
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "Should have reached the End step"
    );

    // At the beginning of End, the delayed trigger fires and bounces Parhelion.
    // flush_triggers resolves it (Bounce has no additional target selection).
    // assert!(
    //     false,
    //     "type {:?}", runner.state().objects[&runner.state().stack.first().unwrap().source_id]
    // );
    flush_triggers(&mut runner);

    let greasefang_zone = runner.state().objects[&_greasefang_id].zone;
    assert_eq!(
        greasefang_zone,
        Zone::Battlefield,
        "greasefang should be on the battlefield"
    );
    // ── Assert: Parhelion is back in P0's hand ────────────────────────────────
    let parhelion_zone = runner.state().objects[&parhelion_id].zone;
    assert_eq!(
        parhelion_zone,
        Zone::Hand,
        "Parhelion II should be in P0's hand after the end-step bounce"
    );

    assert!(
        !runner.state().battlefield.contains(&parhelion_id),
        "Parhelion II must not remain on the battlefield after the bounce"
    );
}
