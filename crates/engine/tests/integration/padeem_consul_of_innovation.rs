//! Padeem, Consul of Innovation: the upkeep trigger keeps its intervening-if
//! and compares controlled artifacts against the table-wide greatest artifact
//! mana value.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AggregateFunction, CardTypeSetSource, Comparator, ContinuousModification,
    Effect, FilterProp, ObjectProperty, QuantityExpr, QuantityRef, TargetFilter, TriggerCondition,
    TypeFilter,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const PADEEM_ORACLE: &str = "Artifacts you control have hexproof. (They can't be the targets of spells or abilities your opponents control.)\nAt the beginning of your upkeep, if you control the artifact with the greatest mana value or tied for the greatest mana value, draw a card.";

fn contains_unimplemented(definition: &AbilityDefinition) -> bool {
    matches!(definition.effect.as_ref(), Effect::Unimplemented { .. })
        || definition
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || definition
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

fn assert_padeem_condition(condition: &TriggerCondition) {
    let TriggerCondition::ControlsType {
        filter: TargetFilter::Typed(candidate),
    } = condition
    else {
        panic!("expected controlled-artifact type condition, got {condition:?}");
    };

    assert_eq!(
        candidate.controller,
        Some(engine::types::ControllerRef::You)
    );
    assert_eq!(candidate.type_filters, vec![TypeFilter::Artifact]);
    assert_eq!(
        candidate.properties.len(),
        2,
        "expected exactly the battlefield and mana-value membership properties: {candidate:?}"
    );
    let battlefield_count = candidate
        .properties
        .iter()
        .filter(|property| {
            matches!(
                property,
                FilterProp::InZone {
                    zone: Zone::Battlefield
                }
            )
        })
        .count();
    assert_eq!(
        battlefield_count, 1,
        "expected exactly one battlefield property: {candidate:?}"
    );
    let mut cmc_properties = candidate
        .properties
        .iter()
        .filter(|property| matches!(property, FilterProp::Cmc { .. }));
    let Some(FilterProp::Cmc {
        comparator: Comparator::EQ,
        value:
            QuantityExpr::Ref {
                qty: QuantityRef::PropertyAggregate(aggregate),
            },
    }) = cmc_properties.next()
    else {
        panic!("expected exact membership in the artifact mana-value maximum: {candidate:?}");
    };
    assert!(
        cmc_properties.next().is_none(),
        "expected exactly one mana-value membership property: {candidate:?}"
    );
    assert_eq!(aggregate.function(), AggregateFunction::Max);
    assert_eq!(aggregate.property(), ObjectProperty::ManaValue);
    let CardTypeSetSource::Objects {
        filter: TargetFilter::Typed(population),
    } = aggregate.source()
    else {
        panic!("expected an artifact object population, got {aggregate:?}");
    };
    assert_eq!(population.type_filters, vec![TypeFilter::Artifact]);
    assert!(
        population.controller.is_none(),
        "the ranked artifact population must include every controller"
    );
}

fn parsed_padeem() -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(
        PADEEM_ORACLE,
        "Padeem, Consul of Innovation",
        &[],
        &["Creature".to_string()],
        &["Vedalken".to_string(), "Artificer".to_string()],
    )
}

struct Board {
    runner: GameRunner,
    padeem: ObjectId,
    p0_artifact: ObjectId,
    p1_artifact: ObjectId,
}

fn board(p0_artifact_mv: u32, p1_artifact_mv: u32, p1_nonartifact_mv: Option<u32>) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let padeem = scenario
        .add_creature(P0, "Padeem, Consul of Innovation", 1, 4)
        .from_oracle_text(PADEEM_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let p0_artifact = scenario
        .add_creature(P0, "P0 Artifact", 2, 2)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(p0_artifact_mv))
        .id();
    let p1_artifact = scenario
        .add_creature(P1, "P1 Artifact", 2, 2)
        .as_artifact()
        .with_mana_cost(ManaCost::generic(p1_artifact_mv))
        .id();
    if let Some(mana_value) = p1_nonartifact_mv {
        scenario
            .add_creature(P1, "P1 Nonartifact", 9, 9)
            .with_mana_cost(ManaCost::generic(mana_value));
    }
    scenario.add_card_to_library_top(P0, "P0 Draw Card");
    scenario.add_card_to_library_top(P1, "P1 Draw Card");
    Board {
        runner: scenario.build(),
        padeem,
        p0_artifact,
        p1_artifact,
    }
}

fn padeem_stack_entries(runner: &GameRunner, padeem: ObjectId) -> Vec<&engine::types::StackEntry> {
    runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == padeem)
        .collect()
}

/// Positive parser reach: both printed abilities parse, the static grant and
/// upkeep draw are typed, and no unimplemented/warning residue hides a dropped
/// clause.
#[test]
fn padeem_full_oracle_parses_hexproof_and_typed_upkeep_condition() {
    let parsed = parsed_padeem();
    assert_eq!(
        parsed.abilities.len()
            + parsed.triggers.len()
            + parsed.statics.len()
            + parsed.replacements.len(),
        2,
        "Padeem has exactly two printed abilities"
    );
    assert!(parsed.abilities.is_empty());
    assert_eq!(parsed.statics.len(), 1);
    assert_eq!(parsed.triggers.len(), 1);
    assert!(parsed.replacements.is_empty());
    assert!(
        parsed.parse_warnings.is_empty(),
        "Padeem must have no warning or unconsumed fragment: {:?}",
        parsed.parse_warnings
    );

    let static_definition = &parsed.statics[0];
    assert_eq!(static_definition.mode, StaticMode::Continuous);
    let Some(TargetFilter::Typed(affected)) = static_definition.affected.as_ref() else {
        panic!("artifact hexproof static must have a typed affected filter");
    };
    assert_eq!(affected.type_filters, vec![TypeFilter::Artifact]);
    assert_eq!(affected.controller, Some(engine::types::ControllerRef::You));
    assert_eq!(
        static_definition.modifications,
        vec![ContinuousModification::AddKeyword {
            keyword: Keyword::Hexproof,
        }]
    );

    let trigger = &parsed.triggers[0];
    assert_eq!(trigger.mode, TriggerMode::Phase);
    assert_eq!(trigger.phase, Some(Phase::Upkeep));
    assert_padeem_condition(
        trigger
            .condition
            .as_ref()
            .expect("Padeem's upkeep trigger must retain its intervening-if"),
    );
    let execute = trigger
        .execute
        .as_deref()
        .expect("Padeem's upkeep trigger must draw");
    assert_eq!(
        execute.effect.as_ref(),
        &Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    );
    assert!(!contains_unimplemented(execute));
}

/// CR 202.3 + CR 603.4: a strictly larger opponent artifact makes the
/// intervening-if false at trigger time, so no Padeem ability reaches the stack
/// and no card is drawn.
#[test]
fn padeem_does_not_trigger_when_opponent_artifact_has_greater_mana_value() {
    let Board {
        mut runner, padeem, ..
    } = board(3, 5, None);
    let parsed = parsed_padeem();
    assert_padeem_condition(parsed.triggers[0].condition.as_ref().unwrap());
    let battlefield_padeem = runner
        .state()
        .objects
        .get(&padeem)
        .expect("Padeem remains on the battlefield");
    assert_eq!(
        battlefield_padeem.name.as_str(),
        "Padeem, Consul of Innovation"
    );
    assert_padeem_condition(
        battlefield_padeem.trigger_definitions[0]
            .definition
            .condition
            .as_ref()
            .expect("the battlefield Padeem source must carry the typed condition"),
    );

    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    runner.advance_to_upkeep();
    assert!(
        padeem_stack_entries(&runner, padeem).is_empty(),
        "the false intervening-if must keep Padeem off the stack"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before
    );
}

/// CR 202.3 + CR 603.4: equal greatest artifact mana values satisfy the exact
/// aggregate-membership predicate. A larger nonartifact does not participate
/// in the ranked population.
#[test]
fn padeem_triggers_on_tied_greatest_artifact_ignoring_larger_nonartifact() {
    let Board {
        mut runner, padeem, ..
    } = board(5, 5, Some(9));
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner.advance_to_upkeep();
    assert_eq!(
        padeem_stack_entries(&runner, padeem).len(),
        1,
        "exactly one Padeem upkeep trigger must be on the stack"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 1
    );
}

/// CR 202.3 + CR 603.4: the condition is checked again on resolution. Raising
/// the opponent's existing artifact above P0's maximum after the trigger is on
/// the stack removes the ability without a draw.
#[test]
fn padeem_rechecks_greatest_artifact_at_resolution() {
    let Board {
        mut runner,
        padeem,
        p1_artifact,
        ..
    } = board(5, 3, None);
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner.advance_to_upkeep();
    assert_eq!(padeem_stack_entries(&runner, padeem).len(), 1);
    let artifact = runner
        .state_mut()
        .objects
        .get_mut(&p1_artifact)
        .expect("fixture artifact exists");
    artifact.mana_cost = ManaCost::generic(9);
    artifact.base_mana_cost = artifact.mana_cost.clone();

    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "the false resolution-time recheck must prevent the draw"
    );
}

/// CR 109.4b + CR 109.5 + CR 603.4: the trigger and its word "you" keep P0 as
/// controller after triggering even if the live Padeem permanent changes
/// controller before resolution.
#[test]
fn padeem_trigger_keeps_captured_controller_after_source_control_changes() {
    let Board {
        mut runner,
        padeem,
        p0_artifact,
        p1_artifact,
    } = board(5, 3, None);
    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();
    let p1_hand_before = runner.state().players[P1.0 as usize].hand.len();

    runner.advance_to_upkeep();
    let entries = padeem_stack_entries(&runner, padeem);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].controller, P0, "the stack entry captures P0");

    let live_padeem = runner
        .state_mut()
        .objects
        .get_mut(&padeem)
        .expect("Padeem remains on the battlefield");
    live_padeem.base_controller = Some(P1);
    live_padeem.controller = P1;
    let battlefield_padeem = runner
        .state()
        .objects
        .get(&padeem)
        .expect("Padeem remains on the battlefield");
    let p0_artifact = runner
        .state()
        .objects
        .get(&p0_artifact)
        .expect("P0's artifact remains on the battlefield");
    let p1_artifact = runner
        .state()
        .objects
        .get(&p1_artifact)
        .expect("P1's artifact remains on the battlefield");
    assert_eq!(battlefield_padeem.controller, P1);
    assert_eq!(p0_artifact.controller, P0);
    assert_eq!(p0_artifact.effective_mana_value(), 5);
    assert_eq!(p1_artifact.controller, P1);
    assert_eq!(p1_artifact.effective_mana_value(), 3);

    runner.advance_until_stack_empty();
    assert_eq!(
        runner
            .state()
            .objects
            .get(&padeem)
            .expect("Padeem remains on the battlefield")
            .controller,
        P1
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before + 1
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        p1_hand_before
    );
}
