//! Regression for GitHub issue #2431 — Ultima, Origin of Oblivion's mana-doubling
//! trigger must fire when the controller taps a land for {C}.
//!
//! Oracle: "Whenever you tap a land for {C}, add an additional {C}."

use std::sync::Arc;

use engine::game::mana_sources::activatable_mana_actions_for_player;
use engine::game::scenario::{GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaProduction, QuantityExpr,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{LoopAction, LoopDetectionMode};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

fn tap_land_action(
    state: &engine::types::game_state::GameState,
    object_id: engine::types::identifiers::ObjectId,
) -> GameAction {
    activatable_mana_actions_for_player(
        state,
        state.waiting_for.acting_player().expect("acting player"),
    )
    .into_iter()
    .find(|action| {
        matches!(action, GameAction::TapLandForMana { selection }
            if selection.source.object_id == object_id)
    })
    .expect("land must expose semantic mana action")
}

fn ultima_mana_trigger() -> engine::types::ability::TriggerDefinition {
    let parsed = parse_oracle_text(
        "Whenever you tap a land for {C}, add an additional {C}.",
        "Ultima, Origin of Oblivion",
        &[],
        &[String::from("Creature")],
        &[],
    );
    parsed.triggers.into_iter().next().expect("one trigger")
}

fn add_colorless_land(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature(P0, "Colorless Land", 0, 0)
        .with_ability_definition(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        )
        .id()
}

#[test]
fn ultima_tap_land_for_c_doubles_colorless_mana() {
    let trigger = ultima_mana_trigger();
    assert_eq!(trigger.mode, TriggerMode::TapsForMana);
    assert_eq!(
        trigger.taps_for_mana_produced,
        Some(vec![ManaType::Colorless])
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Ultima, Origin of Oblivion", 5, 5)
        .with_trigger_definition(trigger);
    let land = add_colorless_land(&mut scenario);

    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.base_power = None;
        obj.base_toughness = None;
        Arc::make_mut(&mut obj.abilities);
    }

    let action = tap_land_action(runner.state(), land);
    let expected_selection = match &action {
        GameAction::TapLandForMana { selection } => selection.clone(),
        other => panic!("expected TapLandForMana, got {other:?}"),
    };
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    runner
        .act(action)
        .expect("tapping a colorless land must succeed");

    assert!(matches!(
        &runner.state().last_loop_action_sequence[..],
        [step]
            if matches!(
                &step.action,
                LoopAction::TapLandForMana { selection } if selection == &expected_selection
            )
    ));

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Colorless),
        2,
        "Ultima must add an additional {{C}} when a land is tapped for colorless mana"
    );
}

#[test]
fn ultima_tap_land_for_c_ignores_colored_mana() {
    let trigger = ultima_mana_trigger();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Ultima, Origin of Oblivion", 5, 5)
        .with_trigger_definition(trigger);
    let forest = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    let mut runner = scenario.build();
    let action = tap_land_action(runner.state(), forest);
    runner
        .act(action)
        .expect("tapping Forest for {G} must succeed");

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "Ultima must not double mana when the land was tapped for colored mana"
    );
}
