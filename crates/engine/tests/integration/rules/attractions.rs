//! Integration tests for Unfinity Attractions (CR 717, CR 701.51, CR 701.52).

#![allow(unused_imports)]

use engine::game::attractions::{open_attractions, roll_to_visit_attractions};
use engine::game::deck_loading::create_attraction_deck_card;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::stack;
use engine::game::zones;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;
fn test_attraction_face(name: &str, oracle: &str, lights: Vec<u8>) -> CardFace {
    let parsed = parse_oracle_text(oracle, name, &[], &[], &["Attraction".to_string()]);
    CardFace {
        name: name.to_string(),
        mana_cost: ManaCost::default(),
        card_type: CardType {
            core_types: vec![CoreType::Artifact],
            subtypes: vec!["Attraction".to_string()],
            supertypes: vec![],
        },
        power: None,
        toughness: None,
        loyalty: None,
        defense: None,
        oracle_text: Some(oracle.to_string()),
        non_ability_text: None,
        flavor_name: None,
        keywords: vec![],
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: vec![],
        replacements: vec![],
        cleave_variant: None,
        color_override: None,
        color_identity: vec![],
        scryfall_oracle_id: None,
        modal: None,
        additional_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        solve_condition: None,
        strive_cost: None,
        brawl_commander: false,
        is_commander: false,
        is_oathbreaker: false,
        deck_copy_limit: None,
        parse_warnings: vec![],
        metadata: Default::default(),
        rarities: Default::default(),
        attraction_lights: lights,
    }
}

#[test]
fn open_attraction_moves_top_deck_card_to_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let face = test_attraction_face("Test Ride", "Visit — Draw a card.", vec![1, 2, 3, 4, 5, 6]);
    create_attraction_deck_card(runner.state_mut(), &face, P0);
    assert_eq!(runner.state().players[0].attraction_deck.len(), 1);

    let mut events = Vec::new();
    open_attractions(runner.state_mut(), P0, 1, &mut events).unwrap();

    assert!(runner.state().players[0].attraction_deck.is_empty());
    assert_eq!(runner.state().battlefield.len(), 1);
    let id = runner.state().battlefield[0];
    assert_eq!(runner.state().objects[&id].zone, Zone::Battlefield);
    assert!(events
        .iter()
        .any(|e| matches!(e, GameEvent::AttractionOpened { .. })));
}

#[test]
fn open_attractions_does_as_much_as_possible_when_deck_is_short() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let face = test_attraction_face("Test Ride", "Visit — Draw a card.", vec![1, 2, 3, 4, 5, 6]);
    create_attraction_deck_card(runner.state_mut(), &face, P0);

    let mut events = Vec::new();
    open_attractions(runner.state_mut(), P0, 2, &mut events).unwrap();

    assert!(runner.state().players[0].attraction_deck.is_empty());
    assert_eq!(runner.state().battlefield.len(), 1);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, GameEvent::AttractionOpened { .. }))
            .count(),
        1
    );
}

#[test]
fn attraction_leaving_for_graveyard_redirects_to_command_junkyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    let face = test_attraction_face("Test Ride", "Visit — Draw a card.", vec![1, 2, 3, 4, 5, 6]);
    let attraction_id = create_attraction_deck_card(runner.state_mut(), &face, P0);
    open_attractions(runner.state_mut(), P0, 1, &mut Vec::new()).unwrap();

    let mut events = Vec::new();
    zones::move_to_zone(
        runner.state_mut(),
        attraction_id,
        Zone::Graveyard,
        &mut events,
    );

    assert_eq!(runner.state().objects[&attraction_id].zone, Zone::Command);
    assert!(!runner.state().objects[&attraction_id].in_attraction_deck);
    assert!(runner.state().command_zone.contains(&attraction_id));
    assert!(!runner.state().players[0].graveyard.contains(&attraction_id));
    assert!(!runner.state().players[0]
        .attraction_deck
        .contains(&attraction_id));
}

#[test]
fn roll_to_visit_fires_visit_trigger_when_roll_matches_lights() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();

    for i in 0..10 {
        zones::create_object(
            runner.state_mut(),
            CardId(5000 + i),
            P0,
            format!("Library {i}"),
            Zone::Library,
        );
    }

    let face = test_attraction_face(
        "Prize Booth",
        "Visit — Draw a card.",
        vec![1, 2, 3, 4, 5, 6],
    );
    let attraction_id = create_attraction_deck_card(runner.state_mut(), &face, P0);
    open_attractions(runner.state_mut(), P0, 1, &mut Vec::new()).unwrap();
    assert_eq!(runner.state().battlefield[0], attraction_id);

    let hand_before = runner.state().players[0].hand.len();
    let mut events = Vec::new();
    roll_to_visit_attractions(runner.state_mut(), P0, &mut events);
    let roll = events
        .iter()
        .find_map(|e| {
            if let GameEvent::AttractionsRolledToVisit { roll, .. } = e {
                Some(*roll)
            } else {
                None
            }
        })
        .expect("roll-to-visit event");

    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::DieRolled {
            player_id: P0,
            sides: 6,
            result,
        } if *result == roll
    )));

    assert!(
        !runner.state().stack.is_empty(),
        "Visit trigger should be on the stack after rolling to visit"
    );
    let mut resolve_events = Vec::new();
    stack::resolve_top(runner.state_mut(), &mut resolve_events);

    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::AttractionVisited {
            attraction_id: id,
            ..
        } if *id == attraction_id
    )));
    assert!(
        runner.state().players[0].hand.len() > hand_before,
        "Visit — Draw a card should have resolved"
    );
}

#[test]
fn parser_open_an_attraction_effect() {
    let parsed = parse_oracle_text("Open an Attraction.", "Opener", &[], &[], &[]);
    assert!(
        parsed
            .abilities
            .iter()
            .any(|a| matches!(*a.effect, Effect::OpenAttractions { count: 1 })),
        "expected OpenAttractions {{ count: 1 }} effect, got {:?}",
        parsed
            .abilities
            .iter()
            .map(|a| &a.effect)
            .collect::<Vec<_>>()
    );
}

#[test]
fn parser_visit_line_becomes_visit_trigger() {
    let parsed = parse_oracle_text(
        "Visit — Create a 1/1 red Balloon creature token with flying.",
        "Balloon Stand",
        &[],
        &[],
        &["Attraction".to_string()],
    );
    assert!(
        parsed
            .triggers
            .iter()
            .any(|t| t.mode == TriggerMode::VisitAttraction),
        "expected VisitAttraction trigger"
    );
}
