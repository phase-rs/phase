//! Issue #1424 — The Scarab God upkeep X and Zombie copy tokens.
//!
//! Upkeep: "each opponent loses X life and you scry X, where X is the number of
//! Zombies you control." With two Zombies on the battlefield, each opponent
//! must lose 2 life when the trigger resolves.
//!
//! CR 107.3i (shared X), CR 119.3 (life loss), CR 701.22a (scry).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::types::ability::{
    ControllerRef, Effect, PlayerFilter, QuantityExpr, QuantityRef, TargetFilter, TypeFilter,
};
use engine::types::events::GameEvent;
use engine::types::phase::Phase;

const SCARAB_UPKEEP: &str = "At the beginning of your upkeep, each opponent loses X life and you scry X, where X is the number of Zombies you control.";

/// CR 107.3i + CR 119.3: two Zombies controlled by P0 → X = 2 → P1 loses 2.
#[test]
fn scarab_god_upkeep_opponent_loses_life_per_zombie_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.with_life(P1, 20);
    let scarab = scenario
        .add_creature_from_oracle(P0, "The Scarab God", 5, 5, SCARAB_UPKEEP)
        .id();
    scenario
        .add_creature(P0, "Zombie A", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();
    scenario
        .add_creature(P0, "Zombie B", 2, 2)
        .with_subtypes(vec!["Zombie"])
        .id();

    let mut runner = scenario.build();
    let triggers = runner
        .state()
        .objects
        .get(&scarab)
        .expect("scarab on battlefield")
        .trigger_definitions
        .len();
    assert!(
        triggers > 0,
        "Scarab God oracle must install at least one upkeep trigger, got {triggers}"
    );
    let upkeep = runner
        .state()
        .objects
        .get(&scarab)
        .unwrap()
        .trigger_definitions
        .iter_unchecked()
        .find(|t| t.phase == Some(Phase::Upkeep))
        .expect("upkeep phase trigger");
    let execute = upkeep.execute.as_ref().expect("execute");
    assert_eq!(
        execute.player_scope,
        Some(PlayerFilter::Opponent),
        "each opponent loses … must scope to opponents"
    );
    match &*execute.effect {
        Effect::LoseLife { amount, .. } => match amount {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(tf),
                    },
            } => {
                assert_eq!(tf.controller, Some(ControllerRef::You));
                assert!(tf
                    .type_filters
                    .contains(&TypeFilter::Subtype("Zombie".to_string())));
            }
            other => panic!("expected zombie ObjectCount amount, got {other:?}"),
        },
        other => panic!("expected LoseLife execute, got {other:?}"),
    }

    runner.state_mut().phase = Phase::Upkeep;
    runner.state_mut().active_player = P0;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );
    assert!(
        !runner.state().stack.is_empty(),
        "Scarab God upkeep must put a trigger on the stack"
    );
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        18,
        "P1 must lose 2 life (one per Zombie controlled by P0): 20 - 2 = 18"
    );
}
