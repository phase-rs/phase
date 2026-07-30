//! Saddle state model (P3) — `saddled_by` persistence + `WaitingFor::SaddleMount`
//! cancel transition, driven through the real engine pipeline.
//!
//! CR 702.171a (Saddle is an activated ability, sorcery-speed), CR 702.171b
//! (the saddled designation lasts until end of turn / leaving the battlefield),
//! CR 702.171c (the creatures that saddled the permanent), CR 601.2c (backing
//! out of a mid-selection choice restores priority with no state to undo).

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::statics::{CrewAction, CrewContributionKind, StaticMode};
use engine::types::{StaticDefinition, TargetFilter};
use std::sync::Arc;

/// CR 702.171c: after a Saddle activation resolves, the Mount records the
/// creatures that saddled it in `saddled_by`; CR 702.171b: the record clears at
/// the end of the turn in lockstep with the saddled designation.
#[test]
fn saddled_by_records_payers_and_clears_at_end_of_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount = {
        let mut b = scenario.add_creature(P0, "Test Mount", 0, 4);
        b.with_keyword(Keyword::Saddle(2));
        b.id()
    };
    // CR 702.171a: a single 3-power creature satisfies "total power 2 or greater".
    let rider = scenario.add_creature(P0, "Rider", 3, 3).id();
    let second_rider = scenario.add_creature(P0, "Second Rider", 3, 3).id();

    let mut runner = scenario.build();

    // Announce the saddle activation (Priority → SaddleMount).
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("entering SaddleMount should succeed at sorcery speed");
    assert_eq!(runner.waiting_for_kind(), "SaddleMount");

    // Pay the cost by tapping the rider; pushes the Saddle stack entry.
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![rider],
        })
        .expect("announcing the saddle should succeed");

    // Resolve the Saddle stack entry through the real pipeline.
    runner.advance_until_stack_empty();

    let m = runner.state().objects.get(&mount).unwrap();
    assert!(m.is_saddled, "Mount must be saddled after resolution");
    // CR 702.171c: the saddling creature is recorded.
    assert_eq!(
        m.saddled_by,
        vec![rider],
        "saddled_by must record the creatures that saddled the Mount"
    );

    // CR 702.171c: a later Saddle activation in the same turn adds the new
    // saddling creature instead of replacing the earlier record.
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("entering a second SaddleMount should succeed");
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![second_rider],
        })
        .expect("announcing the second saddle should succeed");
    runner.advance_until_stack_empty();

    let m = runner.state().objects.get(&mount).unwrap();
    assert_eq!(
        m.saddled_by,
        vec![rider, second_rider],
        "saddled_by must accumulate creatures across same-turn Saddle activations"
    );

    // CR 702.171b: advance the real pipeline across the cleanup step (CR 514) into
    // the next turn. Cleanup clears the designation and the saddling-creature
    // record in lockstep. Drive the engine with its own `legal_actions` so the
    // turn keeps moving through combat declarations and step triggers without ever
    // casting or paying mana.
    let start_turn = runner.state().turn_number;
    let mut crossed_turn = false;
    for _ in 0..400 {
        if runner.state().turn_number > start_turn {
            crossed_turn = true;
            break;
        }
        let actions = engine::ai_support::legal_actions(runner.state());
        let progress = actions
            .iter()
            .find(|a| matches!(a, GameAction::PassPriority))
            .or_else(|| {
                actions.iter().find(|a| {
                    matches!(
                        a,
                        GameAction::DeclareAttackers { .. }
                            | GameAction::DeclareBlockers { .. }
                            | GameAction::SelectCards { .. }
                            | GameAction::ChooseTarget { .. }
                    )
                })
            })
            .cloned();
        match progress {
            Some(action) => {
                if runner.act(action).is_err() {
                    break;
                }
            }
            None => break,
        }
    }
    assert!(
        crossed_turn,
        "harness must cross the turn boundary; parked at turn {} phase {:?} waiting {:?}",
        runner.state().turn_number,
        runner.state().phase,
        runner.state().waiting_for,
    );

    let m = runner.state().objects.get(&mount).unwrap();
    assert!(
        !m.is_saddled,
        "saddled designation must clear at end of turn"
    );
    assert!(
        m.saddled_by.is_empty(),
        "saddled_by must clear at end of turn alongside is_saddled"
    );
}

/// CR 601.2c: backing out of the Saddle creature-selection state restores
/// priority with no state to undo — no creatures are tapped and the Mount is
/// not saddled.
#[test]
fn cancel_saddle_restores_priority_without_side_effects() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount = {
        let mut b = scenario.add_creature(P0, "Test Mount", 0, 4);
        b.with_keyword(Keyword::Saddle(2));
        b.id()
    };
    let rider = scenario.add_creature(P0, "Rider", 3, 3).id();

    let mut runner = scenario.build();

    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("entering SaddleMount should succeed at sorcery speed");
    assert_eq!(runner.waiting_for_kind(), "SaddleMount");

    // CR 601.2c: cancel out of the selection.
    runner
        .act(GameAction::CancelCast)
        .expect("cancelling the saddle should succeed");

    assert_eq!(
        runner.waiting_for_kind(),
        "Priority",
        "cancelling SaddleMount must restore priority"
    );
    assert!(
        !runner.state().objects.get(&rider).unwrap().tapped,
        "cancelling must not tap any creature (cost is paid only at announcement)"
    );
    assert!(
        !runner.state().objects.get(&mount).unwrap().is_saddled,
        "cancelling must leave the Mount unsaddled"
    );
}

fn grant_saddle_power_delta(
    runner: &mut engine::game::scenario::GameRunner,
    rider: engine::types::identifiers::ObjectId,
) {
    let static_def = StaticDefinition::new(StaticMode::CrewContribution {
        kind: CrewContributionKind::PowerDelta { delta: 2 },
        actions: vec![CrewAction::Saddle],
    })
    .affected(TargetFilter::SelfRef);
    let obj = runner.state_mut().objects.get_mut(&rider).unwrap();
    Arc::make_mut(&mut obj.base_static_definitions).push(static_def.clone());
    obj.static_definitions.push(static_def);
}

/// CR 702.171a: Saddle uses the same adjusted contribution authority as Crew.
/// A 1/1 creature with "saddles Mounts as though its power were 2 greater"
/// contributes exactly 3, so it can saddle a Saddle 3 Mount but cannot saddle a
/// Saddle 4 Mount.
#[test]
fn saddle_activation_uses_adjusted_contribution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount = {
        let mut b = scenario.add_creature(P0, "Test Mount", 0, 4);
        b.with_keyword(Keyword::Saddle(3));
        b.id()
    };
    let rider = scenario.add_creature(P0, "Low Power Rider", 1, 1).id();
    let mut runner = scenario.build();
    grant_saddle_power_delta(&mut runner, rider);

    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![],
        })
        .expect("1/1 rider with +2 saddle contribution must enter SaddleMount");
    match &runner.state().waiting_for {
        engine::types::game_state::WaitingFor::SaddleMount {
            eligible_creatures,
            contributions,
            ..
        } => {
            let index = eligible_creatures
                .iter()
                .position(|&id| id == rider)
                .expect("rider must be eligible");
            assert_eq!(
                contributions[index], 3,
                "saddle contribution must be base power 1 plus the +2 delta"
            );
        }
        other => panic!("Expected SaddleMount, got {other:?}"),
    }
    runner
        .act(GameAction::SaddleMount {
            mount_id: mount,
            creature_ids: vec![rider],
        })
        .expect("adjusted saddle contribution 3 must satisfy Saddle 3");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mount = {
        let mut b = scenario.add_creature(P0, "Test Mount", 0, 4);
        b.with_keyword(Keyword::Saddle(4));
        b.id()
    };
    let rider = scenario.add_creature(P0, "Low Power Rider", 1, 1).id();
    let mut runner = scenario.build();
    grant_saddle_power_delta(&mut runner, rider);

    assert!(
        runner
            .act(GameAction::SaddleMount {
                mount_id: mount,
                creature_ids: vec![],
            })
            .is_err(),
        "adjusted saddle contribution is exactly 3 and must not satisfy Saddle 4"
    );
}
