use super::*;
use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter};
use crate::types::triggers::TriggerMode;
use crate::types::zones::Zone;

/// Dawn-Blessed Pennant: "Whenever a permanent you control of the chosen
/// type enters the battlefield, you gain 1 life." The trigger line is the
/// first clause before the comma.
#[test]
fn permanent_you_control_of_chosen_type_enters() {
    let input = "whenever a permanent you control of the chosen type enters the battlefield";
    let result = try_parse_controlled_chosen_type_enters(input);
    assert!(result.is_some(), "should parse: {input}");
    let (mode, def) = result.unwrap();
    assert_eq!(mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
    match &def.valid_card {
        Some(TargetFilter::Typed(typed)) => {
            assert!(
                typed.type_filters.contains(&TypeFilter::Permanent),
                "expected Permanent type filter, got {:?}",
                typed.type_filters
            );
            assert_eq!(typed.controller, Some(ControllerRef::You));
            assert!(
                typed.properties.contains(&FilterProp::IsChosenCreatureType),
                "expected IsChosenCreatureType prop, got {:?}",
                typed.properties
            );
            assert!(
                !typed.properties.contains(&FilterProp::Another),
                "should not have Another prop"
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

/// Bare "enters" without "the battlefield" suffix.
#[test]
fn permanent_you_control_of_chosen_type_enters_bare() {
    let input = "whenever a permanent you control of the chosen type enters";
    let result = try_parse_controlled_chosen_type_enters(input);
    assert!(result.is_some(), "should parse bare enters: {input}");
    let (mode, def) = result.unwrap();
    assert_eq!(mode, TriggerMode::ChangesZone);
    assert_eq!(def.destination, Some(Zone::Battlefield));
}

/// Production dispatch must reach the chosen-type parser before the bare
/// controlled-subtype parser.
#[test]
fn dispatch_routes_chosen_type_enters_before_bare_subtype_parser() {
    let input = "whenever a permanent you control of the chosen type enters the battlefield";
    let result = try_parse_special_trigger_pattern(input);
    assert!(result.is_some(), "dispatch should parse: {input}");
    let (mode, def) = result.unwrap();
    assert_eq!(mode, TriggerMode::ChangesZone);
    match &def.valid_card {
        Some(TargetFilter::Typed(typed)) => {
            assert_eq!(typed.controller, Some(ControllerRef::You));
            assert!(
                typed.properties.contains(&FilterProp::IsChosenCreatureType),
                "expected dispatch to preserve IsChosenCreatureType prop, got {:?}",
                typed.properties
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

/// Molten Echoes: "Whenever a nontoken creature you control of the chosen
/// type enters the battlefield".
#[test]
fn nontoken_creature_you_control_of_chosen_type_enters() {
    let input =
        "whenever a nontoken creature you control of the chosen type enters the battlefield";
    let result = try_parse_controlled_chosen_type_enters(input);
    assert!(result.is_some(), "should parse: {input}");
    let (_, def) = result.unwrap();
    match &def.valid_card {
        Some(TargetFilter::Typed(typed)) => {
            assert!(
                typed.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter, got {:?}",
                typed.type_filters
            );
            assert_eq!(typed.controller, Some(ControllerRef::You));
            assert!(
                typed.properties.contains(&FilterProp::IsChosenCreatureType),
                "expected IsChosenCreatureType prop"
            );
            assert!(
                typed.properties.contains(&FilterProp::NonToken),
                "expected NonToken prop, got {:?}",
                typed.properties
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

/// "Another" variant: "Whenever another creature you control of the chosen
/// type enters the battlefield".
#[test]
fn another_creature_you_control_of_chosen_type_enters() {
    let input = "whenever another creature you control of the chosen type enters the battlefield";
    let result = try_parse_controlled_chosen_type_enters(input);
    assert!(result.is_some(), "should parse: {input}");
    let (_, def) = result.unwrap();
    match &def.valid_card {
        Some(TargetFilter::Typed(typed)) => {
            assert!(
                typed.type_filters.contains(&TypeFilter::Creature),
                "expected Creature type filter"
            );
            assert_eq!(typed.controller, Some(ControllerRef::You));
            assert!(
                typed.properties.contains(&FilterProp::IsChosenCreatureType),
                "expected IsChosenCreatureType prop"
            );
            assert!(
                typed.properties.contains(&FilterProp::Another),
                "expected Another prop, got {:?}",
                typed.properties
            );
        }
        other => panic!("expected Typed filter, got {other:?}"),
    }
}

/// Reject trailing garbage after "enters the battlefield".
#[test]
fn rejects_trailing_garbage() {
    let input = "whenever a permanent you control of the chosen type enters the battlefield foo";
    let result = try_parse_controlled_chosen_type_enters(input);
    assert!(result.is_none(), "should reject trailing garbage");
}
