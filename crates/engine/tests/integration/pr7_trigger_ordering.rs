//! PR-7 Part B2 — production-path (headline) test for the CR 603.3b trigger-ordering
//! fix: a simultaneous, order-dependent, TARGETED batch is ordered exactly ONCE, not
//! re-prompted after every target selection.
//!
//! Root cause (pre-fix): `dispatch_collected_triggers` parks the already-ordered tail
//! into the flat `deferred_triggers` vec on a target pause; the re-drain
//! (`drain_deferred_trigger_queue_unchecked`) re-runs `begin_trigger_ordering` on the
//! still-order-dependent tail and RE-PROMPTS — a second `OrderTriggers` popup on a batch
//! the player already ordered. CR 603.3b (docs line 2586): a simultaneous batch is
//! ordered once in the two-part APNAP process, never re-ordered per target.
//!
//! Fix: an ephemeral `DecisionTemplate` coverage marker registered when the group is
//! ordered lets the gate's 3rd arm auto-apply the chosen order (coverage-only) to every
//! shrinking parked-tail suffix instead of re-prompting.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.3b (docs 2586): simultaneous triggers are ordered once, in APNAP order.
//!   - CR 608.2b (docs 2789): a triggered ability's targets are chosen as it goes on
//!     the stack.
//!   - CR 704.5g: a creature with lethal marked damage is destroyed by SBA. CR 704.3:
//!     all applicable state-based actions are performed simultaneously as a single
//!     event — the three deaths are one ordering batch.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Three same-controller creatures with DISTINCT targeted dies-triggers (distinct damage
/// amounts ⇒ non-identical abilities ⇒ `group_is_order_independent == false`, so the
/// batch genuinely prompts rather than auto-ordering). Each "deals N damage to any
/// target" trigger pauses on target selection at stack placement.
#[test]
fn simultaneous_targeted_dies_batch_orders_once_not_per_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let a = scenario
        .add_creature_from_oracle(
            P0,
            "Bolt Golem A",
            2,
            2,
            "When this creature dies, it deals 1 damage to any target.",
        )
        .id();
    let b = scenario
        .add_creature_from_oracle(
            P0,
            "Bolt Golem B",
            2,
            2,
            "When this creature dies, it deals 2 damage to any target.",
        )
        .id();
    let c = scenario
        .add_creature_from_oracle(
            P0,
            "Bolt Golem C",
            2,
            2,
            "When this creature dies, it deals 3 damage to any target.",
        )
        .id();

    let mut runner = scenario.build();

    // Kill all three simultaneously via a single lethal-damage SBA pass (CR 704.3 +
    // CR 704.5g: all lethal-damage destructions happen as one simultaneous event).
    for id in [a, b, c] {
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .damage_marked = 2;
    }
    let mut sba = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba);

    // The three sources are OFF the battlefield (in the graveyard) when their dies
    // triggers are ordered and re-drained — proves the resolver matches on parked
    // trigger-context identity, not battlefield presence (T3: a battlefield matcher
    // would re-prompt every dies batch).
    for id in [a, b, c] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Graveyard,
            "dies-trigger source must be in the graveyard when ordered"
        );
    }

    // Fire the batch: three simultaneous same-controller order-dependent triggers.
    engine::game::triggers::process_triggers(runner.state_mut(), &sba);

    // Non-vacuity: the batch is genuinely order-DEPENDENT — it produces one real
    // `OrderTriggers` prompt over all three triggers (an order-INDEPENDENT batch would
    // auto-order with no prompt at all, making a "== 1" count trivially true).
    match &runner.state().waiting_for {
        WaitingFor::OrderTriggers { player, triggers } => {
            assert_eq!(*player, P0, "P0 controls all three dies triggers");
            assert_eq!(
                triggers.len(),
                3,
                "all three order-dependent triggers await a single CR 603.3b ordering"
            );
        }
        other => panic!("expected an initial OrderTriggers prompt, got {other:?}"),
    }

    // Drive the whole batch through the REAL apply() pipeline: answer the ordering, then
    // each target. Count the ordering prompts and capture the mid-pause template state.
    let mut order_prompts = 0usize;
    let mut target_prompts = 0usize;
    let mut templates_mid_batch = 0usize;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                order_prompts += 1;
                let order: Vec<usize> = (0..triggers.len()).collect();
                runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("submitting the CR 603.3b order must succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                // T6: mid-batch (a target is pending, batch not yet resolved) the
                // ephemeral coverage marker is REGISTERED — capture the peak.
                templates_mid_batch =
                    templates_mid_batch.max(runner.state().decision_templates.len());
                target_prompts += 1;
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Player(P1)],
                    })
                    .expect("any target accepts the opponent player");
            }
            _ => break,
        }
    }

    // THE FIX (discriminator): the whole batch is ordered exactly ONCE. Reverting the
    // ephemeral registration OR the gate's 3rd arm makes the parked tail re-prompt, so
    // this count becomes 2 (initial + one re-prompt of the [B,C] tail).
    assert_eq!(
        order_prompts, 1,
        "CR 603.3b: the simultaneous batch is ordered ONCE, not re-prompted per target \
         (pre-fix this is 2: the re-drained [B,C] tail re-prompts)"
    );
    assert_eq!(
        target_prompts, 3,
        "each of the three targeted dies triggers still chooses its own target"
    );

    // T6 non-vacuity: the ephemeral marker existed MID-batch (registered), then was
    // CLEARED at the batch-completion boundary — not vacuously absent throughout.
    assert!(
        templates_mid_batch >= 1,
        "the ephemeral coverage marker must be registered while the batch is mid-flight"
    );
    assert!(
        runner.state().decision_templates.is_empty(),
        "CR 603.3b resolution boundary: the ephemeral marker is cleared once the batch \
         fully resolves"
    );
}
