//! Runtime coverage for Shirei's source-zone intervening-if on its delayed return.
//!
//! Oracle (verbatim):
//! "Whenever a creature with power 1 or less is put into your graveyard from the
//! battlefield, you may return that card to the battlefield at the beginning of
//! the next end step if Shirei is still on the battlefield."
//!
//! CR 603.4 requires the intervening-if to hold both when the delayed trigger
//! fires and when it resolves. CR 400.7 makes a blinked permanent a new object,
//! so the old Shirei incarnation cannot satisfy the delayed source condition.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SHIREI: &str = "Whenever a creature with power 1 or less is put into your graveyard from the battlefield, you may return that card to the battlefield at the beginning of the next end step if Shirei is still on the battlefield.";

struct ShireiFixture {
    runner: GameRunner,
    shirei: ObjectId,
    triggering: ObjectId,
    unrelated: ObjectId,
}

fn fixture() -> ShireiFixture {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let shirei = scenario
        .add_creature_from_oracle(P0, "Shirei, Shizo's Caretaker", 2, 2, SHIREI)
        .id();
    let triggering = scenario.add_creature(P0, "Triggering Bear", 1, 1).id();
    let unrelated = scenario.add_creature(P0, "Unrelated Bear", 1, 1).id();

    ShireiFixture {
        runner: scenario.build(),
        shirei,
        triggering,
        unrelated,
    }
}

/// Sacrifice through the debug action so the normal zone-change and trigger
/// collection pipeline is exercised, then accept Shirei's optional creation of
/// the delayed trigger.
fn create_delayed_return(fixture: &mut ShireiFixture) {
    fixture.runner.state_mut().debug_mode = true;
    fixture
        .runner
        .act(GameAction::Debug(DebugAction::Sacrifice {
            object_id: fixture.triggering,
        }))
        .expect("sacrifice the eligible creature");

    for _ in 0..80 {
        if !fixture.runner.state().delayed_triggers.is_empty()
            && fixture.runner.state().stack.is_empty()
        {
            return;
        }
        match fixture.runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                fixture
                    .runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Shirei's may-return delayed trigger");
            }
            WaitingFor::Priority { .. } => {
                fixture
                    .runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while creating Shirei's delayed trigger");
            }
            WaitingFor::OrderTriggers { .. } => {
                fixture.runner.advance_until_stack_empty();
            }
            other => panic!("unexpected prompt creating delayed trigger: {other:?}"),
        }
    }
    panic!("Shirei's delayed trigger was not created");
}

fn settle(runner: &mut GameRunner) {
    for _ in 0..120 {
        if runner.state().stack.is_empty()
            && runner.state().delayed_triggers.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Shirei's optional return");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving Shirei");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty();
            }
            other => panic!("unexpected prompt resolving Shirei: {other:?}"),
        }
    }
    panic!("Shirei resolution did not settle");
}

fn move_raw(runner: &mut GameRunner, object_id: ObjectId, zone: Zone) {
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), object_id, zone, &mut events);
}

fn zone(runner: &GameRunner, object_id: ObjectId) -> Zone {
    runner.state().objects[&object_id].zone
}

fn advance_to_end_step(runner: &mut GameRunner) {
    for _ in 0..200 {
        if runner.state().phase == Phase::End {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while advancing to Shirei's end step");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers while advancing to Shirei's end step");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers while advancing to Shirei's end step");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty();
            }
            other => panic!("unexpected prompt advancing to end step: {other:?}"),
        }
    }
    panic!("failed to reach Shirei's end step");
}

#[test]
fn shirei_remains_and_returns_the_triggering_incarnation_only() {
    let mut fixture = fixture();
    let original_triggering_incarnation =
        fixture.runner.state().objects[&fixture.triggering].incarnation;
    let unrelated_incarnation = fixture.runner.state().objects[&fixture.unrelated].incarnation;

    create_delayed_return(&mut fixture);
    advance_to_end_step(&mut fixture.runner);
    settle(&mut fixture.runner);

    assert_eq!(zone(&fixture.runner, fixture.shirei), Zone::Battlefield);
    assert_eq!(zone(&fixture.runner, fixture.triggering), Zone::Battlefield);
    assert_eq!(zone(&fixture.runner, fixture.unrelated), Zone::Battlefield);
    assert_ne!(
        fixture.runner.state().objects[&fixture.triggering].incarnation,
        original_triggering_incarnation,
        "the returned card must be a new incarnation of the triggering object"
    );
    assert_eq!(
        fixture.runner.state().objects[&fixture.unrelated].incarnation,
        unrelated_incarnation,
        "the unrelated eligible creature must remain untouched"
    );
}

#[test]
fn shirei_leaving_before_delayed_trigger_fire_prevents_return() {
    let mut fixture = fixture();
    create_delayed_return(&mut fixture);
    move_raw(&mut fixture.runner, fixture.shirei, Zone::Graveyard);

    advance_to_end_step(&mut fixture.runner);
    assert!(
        fixture.runner.state().stack.is_empty(),
        "a false Shirei gate must prevent the delayed ability from reaching the stack"
    );
    assert!(
        fixture.runner.state().delayed_triggers.is_empty(),
        "the one-shot next-end-step delayed ability must be consumed after its false gate"
    );
    settle(&mut fixture.runner);

    assert_eq!(zone(&fixture.runner, fixture.shirei), Zone::Graveyard);
    assert_eq!(zone(&fixture.runner, fixture.triggering), Zone::Graveyard);
    assert_eq!(zone(&fixture.runner, fixture.unrelated), Zone::Battlefield);
}

#[test]
fn shirei_leaving_after_delayed_trigger_fires_prevents_resolution() {
    let mut fixture = fixture();
    create_delayed_return(&mut fixture);
    advance_to_end_step(&mut fixture.runner);
    assert!(
        !fixture.runner.state().stack.is_empty(),
        "the delayed trigger must reach the stack while Shirei is still present"
    );

    move_raw(&mut fixture.runner, fixture.shirei, Zone::Graveyard);
    settle(&mut fixture.runner);

    assert_eq!(zone(&fixture.runner, fixture.shirei), Zone::Graveyard);
    assert_eq!(zone(&fixture.runner, fixture.triggering), Zone::Graveyard);
    assert_eq!(zone(&fixture.runner, fixture.unrelated), Zone::Battlefield);
}

#[test]
fn same_id_shirei_reentry_does_not_satisfy_old_delayed_source() {
    let mut fixture = fixture();
    let old_shirei_incarnation = fixture.runner.state().objects[&fixture.shirei].incarnation;
    create_delayed_return(&mut fixture);

    move_raw(&mut fixture.runner, fixture.shirei, Zone::Exile);
    move_raw(&mut fixture.runner, fixture.shirei, Zone::Battlefield);
    assert_eq!(zone(&fixture.runner, fixture.shirei), Zone::Battlefield);
    assert_ne!(
        fixture.runner.state().objects[&fixture.shirei].incarnation,
        old_shirei_incarnation,
        "blink must create a new Shirei incarnation"
    );

    advance_to_end_step(&mut fixture.runner);
    settle(&mut fixture.runner);

    assert_eq!(zone(&fixture.runner, fixture.shirei), Zone::Battlefield);
    assert_eq!(zone(&fixture.runner, fixture.triggering), Zone::Graveyard);
    assert_eq!(zone(&fixture.runner, fixture.unrelated), Zone::Battlefield);
}
