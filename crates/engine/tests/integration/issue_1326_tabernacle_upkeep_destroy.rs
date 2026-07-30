//! Regression for GitHub issue #1326 — Tabernacle-granted upkeep triggers must
//! destroy creatures when the {1} upkeep cost is not paid.

use engine::game::layers::{flush_layers, mark_layers_full};
use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TABERNACLE_ORACLE: &str =
    "All creatures have \"At the beginning of your upkeep, destroy this creature unless you pay {1}.\"";

fn patch_tabernacle_as_land(
    state: &mut engine::types::game_state::GameState,
    tabernacle: ObjectId,
) {
    let tab = state.objects.get_mut(&tabernacle).unwrap();
    tab.card_types.core_types = vec![CoreType::Land];
    tab.base_card_types.core_types = vec![CoreType::Land];
    tab.power = None;
    tab.toughness = None;
    tab.base_power = None;
    tab.base_toughness = None;
}

fn decline_bear_upkeep_and_assert_destroyed(
    runner: &mut engine::game::scenario::GameRunner,
    bear: ObjectId,
) {
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "Bear upkeep trigger must stop at unless payment, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("decline upkeep payment");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "declining Tabernacle upkeep must destroy the creature"
    );
}

#[test]
fn tabernacle_static_parses_grant_trigger_with_unless_pay() {
    use engine::parser::oracle_static::parse_static_line;
    use engine::types::ability::ContinuousModification;

    let def = parse_static_line(TABERNACLE_ORACLE).expect("Tabernacle static must parse");
    let grant = def
        .modifications
        .iter()
        .find_map(|m| match m {
            ContinuousModification::GrantTrigger { trigger } => Some(trigger),
            _ => None,
        })
        .expect("Tabernacle must parse as GrantTrigger");
    assert!(
        grant.unless_pay.is_some(),
        "granted upkeep trigger must carry unless_pay, got {:?}",
        grant.unless_pay
    );
}

#[test]
fn tabernacle_grant_trigger_survives_consecutive_layer_flush() {
    let mut scenario = GameScenario::new();
    let tabernacle = scenario
        .add_creature_from_oracle(
            P0,
            "The Tabernacle at Pendrell Vale",
            0,
            0,
            TABERNACLE_ORACLE,
        )
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    patch_tabernacle_as_land(runner.state_mut(), tabernacle);

    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    assert!(
        runner
            .state()
            .objects
            .get(&bear)
            .unwrap()
            .trigger_definitions
            .as_slice()
            .iter()
            .any(|t| t.definition.unless_pay.is_some()),
        "Tabernacle GrantTrigger must survive back-to-back layer flushes"
    );
}

#[test]
fn tabernacle_creature_destroyed_when_upkeep_cost_declined_shape() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);

    let tabernacle = scenario
        .add_creature_from_oracle(
            P0,
            "The Tabernacle at Pendrell Vale",
            0,
            0,
            TABERNACLE_ORACLE,
        )
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    patch_tabernacle_as_land(runner.state_mut(), tabernacle);
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    runner.state_mut().phase = Phase::Upkeep;
    runner.state_mut().active_player = P0;
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    decline_bear_upkeep_and_assert_destroyed(&mut runner, bear);
}

#[test]
fn tabernacle_creature_destroyed_after_upkeep_sba_and_phase_triggers_shape() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let tabernacle = scenario
        .add_creature_from_oracle(
            P0,
            "The Tabernacle at Pendrell Vale",
            0,
            0,
            TABERNACLE_ORACLE,
        )
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    patch_tabernacle_as_land(runner.state_mut(), tabernacle);
    mark_layers_full(runner.state_mut());
    flush_layers(runner.state_mut());

    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);
    assert!(
        sba_events.is_empty(),
        "Tabernacle land must not die to zero-toughness SBAs, got {sba_events:?}"
    );

    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    decline_bear_upkeep_and_assert_destroyed(&mut runner, bear);
}

#[test]
fn tabernacle_creature_destroyed_via_upkeep_auto_advance() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);

    let tabernacle = scenario
        .add_creature_from_oracle(
            P0,
            "The Tabernacle at Pendrell Vale",
            0,
            0,
            TABERNACLE_ORACLE,
        )
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();

    let mut runner = scenario.build();
    patch_tabernacle_as_land(runner.state_mut(), tabernacle);
    mark_layers_full(runner.state_mut());

    let mut events = Vec::new();
    let waiting = engine::game::turns::auto_advance(runner.state_mut(), &mut events);
    runner.state_mut().waiting_for = waiting;

    decline_bear_upkeep_and_assert_destroyed(&mut runner, bear);
}
