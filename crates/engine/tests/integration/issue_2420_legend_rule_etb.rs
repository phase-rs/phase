//! Regression for issue #2420: ETB triggers must fire when a newly cast legendary
//! copy is sacrificed to the legend rule.
//!
//! https://github.com/phase-rs/phase/issues/2420

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::StackEntryKind;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const LEGEND_ETB_DRAW_ORACLE: &str = "When this creature enters, draw a card.";
const LEGEND_ETB_TARGET_ORACLE: &str =
    "When this creature enters, destroy target artifact an opponent controls.";

fn hand_len(runner: &engine::game::scenario::GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

fn library_len(runner: &engine::game::scenario::GameRunner) -> usize {
    runner.state().players[P0.0 as usize].library.len()
}

#[test]
fn legend_rule_sacrifice_of_new_copy_still_stacks_etb_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library Card"]);

    let first = scenario
        .add_creature_from_oracle(P0, "Duplicant Legend", 2, 2, LEGEND_ETB_DRAW_ORACLE)
        .as_legendary()
        .id();

    let second = scenario
        .add_creature_to_hand_from_oracle(P0, "Duplicant Legend", 2, 2, LEGEND_ETB_DRAW_ORACLE)
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let hand_before = hand_len(&runner);
    let library_before = library_len(&runner);

    runner
        .act(GameAction::CastSpell {
            object_id: second,
            card_id: runner.state().objects[&second].card_id,
            targets: vec![],
        })
        .expect("cast second legendary copy");

    // Resolve the creature spell onto the battlefield.
    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("pass to resolve");
        } else {
            break;
        }
    }

    let (keep, sacrificed) = match &runner.state().waiting_for {
        WaitingFor::ChooseLegend { candidates, .. } => {
            assert!(candidates.contains(&first));
            assert!(candidates.contains(&second));
            let etb_on_stack = runner.state().stack.iter().any(|entry| {
                matches!(
                    &entry.kind,
                    StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == second
                )
            });
            let etb_deferred = runner
                .state()
                .deferred_triggers
                .iter()
                .any(|ctx| ctx.pending.source_id == second);
            assert!(
                etb_on_stack || etb_deferred,
                "ETB from entering copy must be on stack or deferred when legend choice opens; \
                 stack={:?} deferred={}",
                runner.state().stack.len(),
                runner.state().deferred_triggers.len()
            );
            (first, second)
        }
        other => panic!("expected ChooseLegend after second copy entered, got {other:?}"),
    };

    runner
        .act(GameAction::ChooseLegend { keep })
        .expect("keep the original copy, sacrifice the new one");

    assert_eq!(
        runner.state().objects[&sacrificed].zone,
        engine::types::zones::Zone::Graveyard,
        "newly cast copy must be sacrificed to the legend rule"
    );
    assert_eq!(
        runner.state().objects[&keep].zone,
        engine::types::zones::Zone::Battlefield,
        "original copy must remain on the battlefield"
    );

    runner.advance_until_stack_empty();

    // Resolve any remaining priority passes so the ETB trigger can resolve.
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).expect("pass for ETB");
    }

    assert_eq!(
        library_len(&runner),
        library_before - 1,
        "ETB draw must shrink the library even when the new copy was legend-ruled"
    );
    assert_eq!(
        hand_len(&runner),
        hand_before,
        "net hand size unchanged: cast spent the copy, ETB draw replaces it"
    );
}

#[test]
fn legend_rule_does_not_drop_targeted_etb_trigger_from_sacrificed_copy() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let artifact_a = scenario
        .add_creature(P1, "Opponent Artifact A", 0, 0)
        .as_artifact()
        .id();
    let _artifact_b = scenario
        .add_creature(P1, "Opponent Artifact B", 0, 0)
        .as_artifact()
        .id();

    let first = scenario
        .add_creature_from_oracle(P0, "Legendary Disenchanter", 2, 2, LEGEND_ETB_TARGET_ORACLE)
        .as_legendary()
        .id();

    let second = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Legendary Disenchanter",
            2,
            2,
            LEGEND_ETB_TARGET_ORACLE,
        )
        .as_legendary()
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::CastSpell {
            object_id: second,
            card_id: runner.state().objects[&second].card_id,
            targets: vec![],
        })
        .expect("cast second legendary copy");

    for _ in 0..8 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            runner
                .act(GameAction::PassPriority)
                .expect("pass to resolve");
        } else {
            break;
        }
    }

    // CR 704.3: legend-rule SBA resolves before the stacked ETB's targets are chosen.
    match &runner.state().waiting_for {
        WaitingFor::ChooseLegend { candidates, .. } => {
            assert!(candidates.contains(&first));
            assert!(candidates.contains(&second));
        }
        other => panic!("expected ChooseLegend before ETB targets, got {other:?}"),
    }

    // Sacrifice the newly cast copy to satisfy the legend rule.
    runner
        .act(GameAction::ChooseLegend { keep: first })
        .expect("keep original copy");

    // CR 603.3d: target selection surfaces on pipeline re-entry after SBAs settle.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "targeted ETB must prompt after legend rule, got {:?}",
        runner.state().waiting_for
    );

    runner
        .choose_first_legal_target()
        .expect("choose opponent's artifact for ETB");

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&artifact_a].zone,
        engine::types::zones::Zone::Graveyard,
        "ETB destroy effect must resolve even though the source was legend-ruled"
    );
}
