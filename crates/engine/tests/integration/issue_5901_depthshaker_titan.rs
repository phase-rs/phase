//! Regression tests for issue #5901 — Depthshaker Titan: "When this creature
//! enters, any number of target noncreature artifacts you control become 3/3
//! artifact creatures. Sacrifice them at the beginning of the next end step."
//!
//! Reported symptom: the Titan sacrificed ITSELF at the next end step.
//!
//! Root cause: the ETB trigger's "any number of target ..." slot may legally
//! be filled with ZERO targets (CR 115.6; target choice for this triggered
//! ability follows CR 603.3d). The delayed trigger's inner
//! `Sacrifice { target: ParentTarget }` snapshots its subject at creation time
//! via `parent_target_snapshot` (`game/effects/delayed_trigger.rs`), whose
//! referent ladder was: root-chain chosen targets → the node's own propagated
//! targets → the creation event's source object (`TriggeringSource`). With
//! zero chosen targets the first two tiers are empty, so the ladder fell
//! through to the event-context tier and bound the ETB event's source — the
//! Titan itself — as "them", sacrificing it at the next end step (CR 603.7c:
//! the anaphor's referent is the empty chosen set, not the trigger's own
//! source). Fixed by returning the empty set from the snapshot when the
//! resolving root chain DECLARED a chooseable target slot (`multi_target` /
//! `optional_targeting`): reaching the fallback with such a declaration means
//! the player chose zero targets. The event-source fallback is preserved for
//! slotless parents (a dies/LTB trigger's "exile it at the beginning of the
//! next end step", where "it" genuinely names the event source).

use super::rules::{GameScenario, Phase, WaitingFor, P0};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const DEPTHSHAKER_TITAN: &str = "When this creature enters, any number of target noncreature artifacts you control become 3/3 artifact creatures. Sacrifice them at the beginning of the next end step.\nEach artifact creature you control has melee, trample, and haste. (Whenever a creature with melee attacks, it gets +1/+1 until end of turn for each opponent you attacked this combat.)";

/// Drive the game to the first end step with a settled stack, answering the
/// prompts that surface on the way (combat declarations, any sacrifice
/// selection raised by the delayed trigger). Returns the card pools offered by
/// any `EffectZoneChoice`, so tests can assert none was offered.
fn settle_to_end_step(runner: &mut super::rules::GameRunner) -> Vec<Vec<ObjectId>> {
    let mut pools = Vec::new();
    let mut settled = false;
    for _ in 0..120 {
        let at_end = runner.state().phase == Phase::End;
        let stack_len = runner.state().stack.len();
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { cards, count, .. } => {
                pools.push(cards.clone());
                let pick: Vec<_> = cards.into_iter().take(count.max(1)).collect();
                runner
                    .act(GameAction::SelectCards { cards: pick })
                    .expect("submit zone-choice selection");
            }
            WaitingFor::Priority { .. } => {
                if at_end && stack_len == 0 {
                    settled = true;
                    // End step reached and settled (whether or not the delayed
                    // trigger fired) — stop so the asserts see final zones.
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            other => panic!("unexpected WaitingFor while settling end step: {other:?}"),
        }
    }
    assert!(
        settled,
        "did not reach a settled end step within 120 actions"
    );
    pools
}

/// Baseline (already-correct path, guards against over-fixing): two chosen
/// artifacts are animated and BOTH are sacrificed at the next end step; the
/// Titan itself and an unchosen artifact survive.
#[test]
fn depthshaker_titan_sacrifices_only_chosen_artifacts() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let titan = scenario
        .add_creature_to_hand(P0, "Depthshaker Titan", 8, 8)
        .from_oracle_text(DEPTHSHAKER_TITAN)
        .id();
    let chosen_a = scenario
        .add_creature(P0, "Chosen Mox A", 0, 0)
        .as_artifact()
        .id();
    let chosen_b = scenario
        .add_creature(P0, "Chosen Mox B", 0, 0)
        .as_artifact()
        .id();
    let unchosen = scenario
        .add_creature(P0, "Unchosen Monument", 0, 0)
        .as_artifact()
        .id();

    let mut runner = scenario.build();
    runner
        .cast(titan)
        .free_cast()
        .target_objects(&[chosen_a, chosen_b])
        .resolve();

    // The ETB animation must be live before the end step.
    for id in [chosen_a, chosen_b] {
        let obj = &runner.state().objects[&id];
        assert!(
            obj.card_types
                .core_types
                .contains(&engine::types::card_type::CoreType::Creature),
            "chosen artifact must be animated to a creature before end step"
        );
    }

    settle_to_end_step(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects.get(&titan).map(|o| o.zone),
        Some(Zone::Battlefield),
        "the Titan itself must NOT be sacrificed by its own delayed trigger"
    );
    assert_eq!(
        state.objects.get(&chosen_a).map(|o| o.zone),
        Some(Zone::Graveyard),
        "first chosen artifact must be sacrificed at the next end step"
    );
    assert_eq!(
        state.objects.get(&chosen_b).map(|o| o.zone),
        Some(Zone::Graveyard),
        "second chosen artifact must be sacrificed at the next end step"
    );
    assert_eq!(
        state.objects.get(&unchosen).map(|o| o.zone),
        Some(Zone::Battlefield),
        "an unchosen artifact must survive the delayed trigger"
    );
}

/// The reported bug: "any number of target ... artifacts" legally includes
/// ZERO targets. With no chosen artifacts the delayed sacrifice has no
/// subject — it must NOT fall back to sacrificing the Titan itself.
///
/// Pre-fix this failed with the Titan in the graveyard at the end step
/// (`parent_target_snapshot`'s event-context tier bound the ETB event source).
#[test]
fn depthshaker_titan_zero_targets_does_not_sacrifice_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let titan = scenario
        .add_creature_to_hand(P0, "Depthshaker Titan", 8, 8)
        .from_oracle_text(DEPTHSHAKER_TITAN)
        .id();
    let bystander = scenario
        .add_creature(P0, "Bystander Monument", 0, 0)
        .as_artifact()
        .id();

    let mut runner = scenario.build();
    runner.cast(titan).free_cast().resolve();

    let pools = settle_to_end_step(&mut runner);

    let state = runner.state();
    assert_eq!(
        state.objects.get(&titan).map(|o| o.zone),
        Some(Zone::Battlefield),
        "with zero chosen artifacts the Titan must NOT sacrifice itself"
    );
    assert_eq!(
        state.objects.get(&bystander).map(|o| o.zone),
        Some(Zone::Battlefield),
        "an untargeted artifact must not be sacrificed either"
    );
    assert!(
        pools.is_empty(),
        "no sacrifice choice should be offered when nothing was animated; got {pools:?}"
    );
}
