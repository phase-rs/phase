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
    // CR 701.52a: ONE turn-based action emits ONE roll-to-visit event carrying
    // every surviving result. With no count-raising replacement in play there is
    // exactly one survivor.
    let rolls = events
        .iter()
        .find_map(|e| {
            if let GameEvent::AttractionsRolledToVisit { rolls, .. } = e {
                Some(rolls.clone())
            } else {
                None
            }
        })
        .expect("roll-to-visit event");
    assert_eq!(
        rolls.len(),
        1,
        "CR 701.52a: an unreplaced roll-to-visit has exactly one surviving result"
    );
    let roll = rolls[0];

    assert!(events.iter().any(|e| matches!(
        e,
        GameEvent::DieRolled {
            player_id: P0,
            sides: 6,
            result,
        } if *result == Some(roll)
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

/// Barbarian Class level 1, verified verbatim against Scryfall:
/// > If you would roll one or more dice, instead roll that many dice plus one
/// > and ignore the lowest roll.
const BARBARIAN_CLASS_L1: &str = "If you would roll one or more dice, instead roll that many \
dice plus one and ignore the lowest roll.";

/// Pixie Guide, verified verbatim against Scryfall.
const PIXIE_GUIDE: &str = "Grant an Advantage — If you would roll one or more dice, instead \
roll that many dice plus one and ignore the lowest roll.";

/// Build a `PreCombatMain` runner controlling one all-lights Attraction, so the
/// roll-to-visit fires no matter which face comes up and every surviving result
/// produces exactly one visit.
fn runner_with_all_lights_attraction(
    scenario: GameScenario,
) -> (GameRunner, engine::types::identifiers::ObjectId) {
    let mut runner = scenario.build();
    let face = test_attraction_face(
        "Prize Booth",
        "Visit — Draw a card.",
        vec![1, 2, 3, 4, 5, 6],
    );
    let attraction_id = create_attraction_deck_card(runner.state_mut(), &face, P0);
    open_attractions(runner.state_mut(), P0, 1, &mut Vec::new()).unwrap();
    assert!(
        runner.state().battlefield.contains(&attraction_id),
        "reach-guard: the Attraction must be on the battlefield to roll to visit"
    );
    (runner, attraction_id)
}

fn rolled_to_visit(events: &[GameEvent]) -> Option<Vec<u8>> {
    events.iter().find_map(|event| match event {
        GameEvent::AttractionsRolledToVisit { rolls, .. } => Some(rolls.clone()),
        _ => None,
    })
}

fn die_rolls(events: &[GameEvent]) -> Vec<u8> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DieRolled {
                result: Some(result),
                ..
            } => Some(*result),
            _ => None,
        })
        .collect()
}

/// CR 701.52a + CR 614.1a: the roll-to-visit turn-based action is a die roll, so
/// a count-raising CR 706.6 replacement applies to it. TWO naturals are rolled
/// and ONE is ignored, so exactly one `DieRolled` reaches the log and the single
/// turn-based action still reports exactly one surviving result.
#[test]
fn roll_to_visit_applies_count_raising_die_roll_replacement() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    let (mut runner, attraction_id) = runner_with_all_lights_attraction(scenario);

    let mut events = Vec::new();
    roll_to_visit_attractions(runner.state_mut(), P0, &mut events);

    // Reach-guard: the turn-based action must have happened at all — the failure
    // value on this path is "no event", which would pass a length assertion
    // vacuously.
    let rolls = rolled_to_visit(&events)
        .expect("reach-guard: the roll-to-visit turn-based action must report a result");

    // CR 706.6: the replacement raised the count to two and ignored one roll, so
    // exactly one survivor remains — an ignored roll "never happened".
    assert_eq!(
        rolls.len(),
        1,
        "CR 706.6: two naturals rolled, the lowest ignored, one survivor"
    );
    assert_eq!(
        die_rolls(&events),
        rolls,
        "CR 706.6: an ignored roll emits no DieRolled event"
    );

    // CR 701.52a: visiting is decided per surviving RESULT, and the all-lights
    // Attraction matches every face, so the single survivor visits it once.
    let visits: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::AttractionVisited { attraction_id: id, .. } if *id == attraction_id
            )
        })
        .collect();
    assert_eq!(
        visits.len(),
        1,
        "CR 701.52a: one surviving result visits the all-lights Attraction exactly once"
    );
}

/// CR 616.1 + CR 703.4g (regression): two applicable die-roll replacements make
/// the affected player order them. The roll-to-visit has no resolution frame, so
/// it parks its own continuation — ordering the replacements must FINISH the
/// turn-based action, not silently cancel it.
#[test]
fn roll_to_visit_resumes_after_replacement_ordering_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Barbarian Class", BARBARIAN_CLASS_L1);
    scenario.add_enchantment_from_oracle(P0, "Pixie Guide", PIXIE_GUIDE);
    let (mut runner, attraction_id) = runner_with_all_lights_attraction(scenario);

    let mut events = Vec::new();
    roll_to_visit_attractions(runner.state_mut(), P0, &mut events);

    // Reach-guard: without a real CR 616.1 suspension the rest of this test
    // would assert against the ordinary inline path and prove nothing.
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::ReplacementChoice { .. }
        ),
        "reach-guard: two applicable replacements must surface a CR 616.1 ordering choice"
    );
    assert!(
        rolled_to_visit(&events).is_none(),
        "CR 616.1: nothing is rolled until the ordering choice is answered"
    );

    let result = runner
        .act(engine::types::actions::GameAction::ChooseReplacement { index: 0 })
        .expect("the CR 616.1 ordering choice must be accepted");

    // CR 703.4g: the turn-based action must complete once the choice settles.
    let rolls = rolled_to_visit(&result.events)
        .expect("CR 616.1: answering the ordering choice must FINISH the roll-to-visit");
    // CR 706.6: three naturals rolled (1 + 1 + 1), two ignored — one survivor.
    assert_eq!(
        rolls.len(),
        1,
        "CR 706.6: two applied replacements ignore two of the three rolls"
    );
    assert_eq!(
        die_rolls(&result.events),
        rolls,
        "CR 706.6: ignored rolls emit no DieRolled event"
    );
    assert!(
        result.events.iter().any(|event| matches!(
            event,
            GameEvent::AttractionVisited { attraction_id: id, .. } if *id == attraction_id
        )),
        "CR 701.52a: the surviving result must still visit the Attraction"
    );
    // CR 603.2: the Visit trigger must actually reach the stack — the resume
    // path runs `process_triggers` like the inline path does, so a visit that
    // fires no trigger would be a silent half-completion.
    assert!(
        !runner.state().stack.is_empty(),
        "CR 603.2: the Visit trigger must be on the stack after the resumed roll-to-visit"
    );
    // CR 706.4: the turn-based action is not a resolution, so it leaves no
    // resolution-scoped die result behind for an unrelated effect to read.
    assert_eq!(
        runner.state().die_result_this_resolution,
        None,
        "CR 706.4: the roll-to-visit stamps no resolution-scoped die result"
    );
    // The parked continuation frame must not survive its own resume.
    assert!(
        runner.state().pending_die_roll_instruction.is_none(),
        "the parked roll-to-visit continuation must be consumed by the resume"
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
