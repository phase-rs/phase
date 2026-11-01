//! Tests for optional command zone in Limited formats (CR 905: Conspiracy Draft).
//!
//! Verifies:
//! - FormatConfig validation allows `command_zone: true` for `GameFormat::Limited`
//! - `FormatConfig::limited_with_command_zone()` provides an enabled command zone
//! - Deck loading places command-zone cards into the command zone without netting
//!   them against the 40-card main deck
//! - Command zone cards are not marked `is_commander`
//! - Hidden agenda conspiracies enter face down (CR 905.4a / CR 702.106a)
//! - Regular conspiracies enter face up (CR 905.4)
//! - Face-down conspiracies are redacted for observers and visible to controllers in viewer projection
//! - Face-down conspiracies in the command zone can be turned face up as a special action
//!   during priority (CR 702.106c / CR 116.2j / CR 905.4a)
//! - The pregame secret agenda prompt validates its names and keeps them private
//!   (CR 702.106a / CR 702.106f)
//! - Secretly chosen names (single and double agenda) are transferred to chosen attributes
//!   and power cost reductions when revealed

use engine::ai_support::legal_actions;
use engine::game::conspiracy::record_secret_agenda_choices;
use engine::game::deck_loading::{
    create_command_zone_card_from_card_face, load_deck_into_state, DeckEntry, DeckPayload,
    PlayerDeckPayload,
};
use engine::game::derived_views::ClientGameStateRef;
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::visibility::{filter_state_for_viewer, HIDDEN_CARD_NAME};
use engine::types::ability::{
    ChosenAttribute, ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::{DeckSizeRule, FormatConfig, GameFormat};
use engine::types::game_state::GameState;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, CostReductionReach, StaticMode};
use std::sync::Arc;

fn plain_card_face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        ..Default::default()
    }
}

const BRAGOS_FAVOR_ORACLE: &str = "Hidden agenda (Start the game with this conspiracy face down in the command zone and secretly choose a card name. You may turn this conspiracy face up any time and reveal that name.)\nSpells with the chosen name you cast cost {1} less to cast.";
const SUMMONERS_BOND_ORACLE: &str = "Double agenda (Start the game with this conspiracy face down in the command zone and secretly choose two different card names. You may turn this conspiracy face up any time and reveal those names.)\nWhenever you cast a creature spell with one of the chosen names, you may search your library for a creature card with the other chosen name, reveal it, put it into your hand, then shuffle.";

fn conspiracy_card_face(name: &str, hidden_agenda: bool) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Conspiracy);
    if hidden_agenda {
        face.oracle_text = Some(BRAGOS_FAVOR_ORACLE.to_string());
        face.static_abilities.push(
            StaticDefinition::new(StaticMode::ModifyCost {
                mode: CostModifyMode::Reduce,
                amount: ManaCost::generic(1),
                spell_filter: Some(TargetFilter::HasChosenName),
                dynamic_count: None,
                reach: CostReductionReach::SpillsToGeneric,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::card().controller(ControllerRef::You),
            )),
        );
    } else {
        face.oracle_text = Some("Conspiracy text".to_string());
    }
    face
}

fn summoners_bond_face() -> CardFace {
    let mut face = CardFace {
        name: "Summoner's Bond".to_string(),
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Conspiracy);
    face.oracle_text = Some(SUMMONERS_BOND_ORACLE.to_string());
    face
}

fn forty_card_main() -> Vec<DeckEntry> {
    (0..40)
        .map(|i| DeckEntry {
            card: plain_card_face(&format!("Plains {i}")),
            count: 1,
        })
        .collect()
}

/// Two players with 40-card decks; each player's listed cards sit in the slot
/// the deck format uses for command-zone cards.
fn load_limited_decks(state: &mut GameState, p0_cards: Vec<CardFace>, p1_cards: Vec<CardFace>) {
    state.format_config = FormatConfig::limited_with_command_zone();
    state.all_card_names = known_card_names();
    let slot = |cards: Vec<CardFace>| {
        cards
            .into_iter()
            .map(|card| DeckEntry { card, count: 1 })
            .collect()
    };
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: forty_card_main(),
            commander: slot(p0_cards),
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: forty_card_main(),
            commander: slot(p1_cards),
            ..Default::default()
        },
        ..Default::default()
    };
    load_deck_into_state(state, &payload);
}

fn known_card_names() -> Arc<[String]> {
    ["Grizzly Bears", "Runeclaw Bear", "Plains 0"]
        .map(str::to_string)
        .into()
}

/// Setup shortcut for tests about what happens after the pregame prompt, which
/// `pregame_agenda_prompt_*` below drives through the action path.
fn choose_names(state: &mut GameState, id: ObjectId, names: &[&str]) {
    state.all_card_names = known_card_names();
    record_secret_agenda_choices(
        state,
        id,
        P0,
        names.iter().map(|name| name.to_string()).collect(),
    )
    .expect("valid agenda names");
}

fn names(names: &[&str]) -> GameAction {
    GameAction::ChooseSecretAgendaNames {
        names: names.iter().map(|name| name.to_string()).collect(),
    }
}

fn priority_state() -> GameState {
    let mut state = GameState::new_two_player(7);
    state.turn_number = 1;
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state
}

#[test]
fn limited_format_config_allows_optional_command_zone() {
    let default_limited = FormatConfig::limited();
    assert!(!default_limited.command_zone);
    assert_eq!(default_limited.format, GameFormat::Limited);
    assert_eq!(default_limited.deck_size, DeckSizeRule::Minimum(40));
    assert!(!default_limited.uses_commander);

    let limited_cz = FormatConfig::limited_with_command_zone();
    assert!(limited_cz.command_zone);
    assert_eq!(limited_cz.format, GameFormat::Limited);
    assert_eq!(limited_cz.deck_size, DeckSizeRule::Minimum(40));
    assert!(!limited_cz.uses_commander);

    // Serialization & deserialization round-trip passes built_in_axes_no_looser_than_rules
    let serialized = serde_json::to_string(&limited_cz).expect("serialization succeeds");
    let deserialized: FormatConfig =
        serde_json::from_str(&serialized).expect("deserialization succeeds");
    assert!(deserialized.command_zone);
    assert_eq!(deserialized.format, GameFormat::Limited);

    // Other non-command-zone formats (e.g. Standard) still reject command_zone = true
    let mut illegal_standard = FormatConfig::standard();
    illegal_standard.command_zone = true;
    let err =
        serde_json::from_str::<FormatConfig>(&serde_json::to_string(&illegal_standard).unwrap());
    assert!(err.is_err());
    let msg = err.unwrap_err().to_string();
    assert!(msg.contains("FormatConfig.command_zone is true, but Standard requires exactly false"));
}

#[test]
fn limited_command_zone_cards_loaded_separately_from_main_deck() {
    let mut state = GameState::new_two_player(7);

    // 40 main-deck cards + 2 command-zone cards
    load_limited_decks(
        &mut state,
        vec![
            conspiracy_card_face("Worldknit", false),
            conspiracy_card_face("Brago's Favor", true),
        ],
        vec![],
    );

    // CR 905.4: Conspiracies are NOT included in the player's deck.
    // Library must contain all 40 main deck cards (no netting occurred).
    let p0_library = &state.players[0].library;
    assert_eq!(
        p0_library.len(),
        40,
        "library maintains full 40 cards without netting"
    );

    // Command zone has the 2 conspiracy cards
    assert_eq!(
        state.command_zone.len(),
        2,
        "command zone holds both conspiracy cards"
    );

    let worldknit_id = state
        .command_zone
        .iter()
        .copied()
        .find(|&id| state.objects.get(&id).map(|o| o.name.as_str()) == Some("Worldknit"))
        .expect("Worldknit present in command zone");

    let brago_id = state
        .command_zone
        .iter()
        .copied()
        .find(|&id| id != worldknit_id)
        .expect("Brago's Favor present in command zone");
    choose_names(&mut state, brago_id, &["Grizzly Bears"]);

    let worldknit_obj = state.objects.get(&worldknit_id).unwrap();
    let brago_obj = state.objects.get(&brago_id).unwrap();

    // Not commanders!
    assert!(!worldknit_obj.is_commander, "conspiracy is not a commander");
    assert!(!brago_obj.is_commander, "conspiracy is not a commander");

    // Worldknit starts face-up (CR 905.4)
    assert!(
        !worldknit_obj.face_down,
        "CR 905.4: non-hidden-agenda starts face up"
    );

    // Brago's Favor starts face-down (CR 905.4a / CR 702.106a)
    assert!(
        brago_obj.face_down,
        "CR 905.4a: hidden agenda starts face down"
    );
    assert_eq!(
        brago_obj.secret_chosen_names,
        vec!["Grizzly Bears".to_string()],
        "secret chosen names preserved on object"
    );

    // Check visibility projection
    let p0_view = filter_state_for_viewer(&state, P0);
    let p0_brago = p0_view.objects.get(&brago_id).unwrap();
    assert_eq!(
        p0_brago.name, "Brago's Favor",
        "controller can view own face-down conspiracy"
    );

    let p1_view = filter_state_for_viewer(&state, P1);
    let p1_brago = p1_view.objects.get(&brago_id).unwrap();
    assert_eq!(
        p1_brago.name, HIDDEN_CARD_NAME,
        "opponent cannot view face-down conspiracy identity"
    );
    assert!(p1_brago.secret_chosen_names.is_empty());
    assert!(p1_brago.chosen_attributes.is_empty());
    assert!(p1_brago.abilities.is_empty());
    assert!(p1_brago.static_definitions.is_empty());
    assert!(p1_brago.trigger_definitions.is_empty());
    assert!(p1_brago.replacement_definitions.is_empty());
    assert!(p1_brago.card_types.core_types.is_empty());
    assert_eq!(p1_brago.mana_cost, ManaCost::default());
    assert!(p1_brago.keywords.is_empty());
    assert!(p1_brago.color.is_empty());

    // Blocker 1: Serialization regression through ClientGameStateRef / serde_json
    // Opponent wire representation leaks NO card name, secret name, abilities, or card types
    let p1_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P1)))
        .expect("wire state serializes");
    assert!(
        !p1_wire.contains("Brago's Favor"),
        "opponent wire representation must not contain secret conspiracy name"
    );
    assert!(
        !p1_wire.contains("Grizzly Bears"),
        "opponent wire representation must not contain secret chosen card name"
    );
}

#[test]
fn limited_command_zone_turn_hidden_agenda_face_up_special_action() {
    let mut state = priority_state();
    load_limited_decks(
        &mut state,
        vec![conspiracy_card_face("Brago's Favor", true)],
        vec![],
    );

    let brago_id = state.command_zone[0];
    choose_names(&mut state, brago_id, &["Grizzly Bears"]);
    assert!(state.objects[&brago_id].face_down);
    assert_eq!(
        state.objects[&brago_id].secret_chosen_names,
        vec!["Grizzly Bears".to_string()]
    );
    assert!(
        state.objects[&brago_id].chosen_attributes.is_empty(),
        "secret chosen name is not public before turn face up"
    );

    // CR 702.106c / CR 116.2j / CR 905.4a: candidate offer is available during priority
    let actions = legal_actions(&state);
    assert!(
        actions.iter().any(
            |a| matches!(a, GameAction::TurnFaceUp { object_id, .. } if *object_id == brago_id)
        ),
        "face-down conspiracy is offered as turn-face-up legal action"
    );

    let mut runner = GameRunner::from_state(state);
    let result = runner
        .act(GameAction::TurnFaceUp {
            object_id: brago_id,
            x: 0,
        })
        .expect("turn face up special action succeeds");

    // CR 702.106c: turning it face up publicly reveals the chosen name.
    assert!(result.events.iter().any(|event| matches!(
        event,
        GameEvent::CardNamed { source_id, name, .. }
            if *source_id == Some(brago_id) && name == "Grizzly Bears"
    )));

    // Face down status is cleared
    let brago_after = &runner.state().objects[&brago_id];
    assert!(!brago_after.face_down, "conspiracy is now face up");
    assert_eq!(brago_after.name, "Brago's Favor");
    assert_eq!(
        brago_after.chosen_attributes,
        vec![ChosenAttribute::CardName("Grizzly Bears".to_string())],
        "secret name transferred to chosen_attributes upon turning face up"
    );

    // Both players can see its identity now
    let p1_view = filter_state_for_viewer(runner.state(), P1);
    assert_eq!(p1_view.objects[&brago_id].name, "Brago's Favor");

    // Priority remains with P0
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    );
}

#[test]
fn limited_command_zone_double_agenda_two_chosen_names() {
    let mut state = priority_state();
    load_limited_decks(&mut state, vec![summoners_bond_face()], vec![]);

    let obj_id = state.command_zone[0];
    assert!(state.objects[&obj_id].face_down);
    choose_names(&mut state, obj_id, &["Grizzly Bears", "Runeclaw Bear"]);
    assert!(state.objects[&obj_id].chosen_attributes.is_empty());

    let mut runner = GameRunner::from_state(state);
    let result = runner.act(GameAction::TurnFaceUp {
        object_id: obj_id,
        x: 0,
    });
    assert!(result.is_ok(), "turn face up succeeds");

    let obj_after = &runner.state().objects[&obj_id];
    assert!(!obj_after.face_down);
    assert_eq!(
        obj_after.chosen_attributes,
        vec![
            ChosenAttribute::CardName("Grizzly Bears".to_string()),
            ChosenAttribute::CardName("Runeclaw Bear".to_string()),
        ],
        "both secretly chosen names are transferred to chosen_attributes"
    );
}

#[test]
fn limited_command_zone_cost_reduction_with_chosen_name() {
    let mut state = priority_state();
    state.format_config = FormatConfig::limited_with_command_zone();

    let brago_face = conspiracy_card_face("Brago's Favor", true);
    let brago_id = create_command_zone_card_from_card_face(&mut state, &brago_face, P0);
    choose_names(&mut state, brago_id, &["Grizzly Bears"]);

    // Forest on battlefield
    let card_id = engine::types::identifiers::CardId(state.next_object_id);
    let forest_id = engine::game::zones::create_object(
        &mut state,
        card_id,
        P0,
        "Forest".to_string(),
        engine::types::zones::Zone::Battlefield,
    );
    let forest_obj = state.objects.get_mut(&forest_id).unwrap();
    forest_obj.card_types.core_types.push(CoreType::Land);
    forest_obj
        .card_types
        .supertypes
        .push(engine::types::card_type::Supertype::Basic);
    forest_obj.card_types.subtypes.push("Forest".to_string());
    forest_obj.base_card_types = forest_obj.card_types.clone();
    forest_obj.entered_battlefield_turn = Some(0);
    forest_obj.summoning_sick = false;
    let mana_ability = engine::types::ability::AbilityDefinition::new(
        engine::types::ability::AbilityKind::Activated,
        engine::types::ability::Effect::Mana {
            produced: engine::types::ability::ManaProduction::Fixed {
                colors: vec![ManaColor::Green],
                contribution: engine::types::ability::ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(engine::types::ability::AbilityCost::Tap);
    std::sync::Arc::make_mut(&mut forest_obj.abilities).push(mana_ability.clone());
    std::sync::Arc::make_mut(&mut forest_obj.base_abilities).push(mana_ability);

    // Grizzly Bears in hand ({1}{G})
    let bear_card_id = engine::types::identifiers::CardId(state.next_object_id);
    let bear_id = engine::game::zones::create_object(
        &mut state,
        bear_card_id,
        P0,
        "Grizzly Bears".to_string(),
        engine::types::zones::Zone::Hand,
    );
    let bear_obj = state.objects.get_mut(&bear_id).unwrap();
    bear_obj.card_types.core_types.push(CoreType::Creature);
    bear_obj.base_card_types = bear_obj.card_types.clone();
    bear_obj.mana_cost = ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 1,
    };
    bear_obj.power = Some(2);
    bear_obj.toughness = Some(2);

    let mut runner = GameRunner::from_state(state);

    // While face-down: Brago's Favor does not reduce cost. Grizzly Bears costs {1}{G} (2 mana).
    // With only 1 Forest ({G}), Grizzly Bears cannot be cast.
    let actions = legal_actions(runner.state());
    assert!(
        !actions
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == bear_id)),
        "cannot cast Grizzly Bears with only 1 Forest while conspiracy is face down"
    );

    // Turn Brago's Favor face-up
    let turn_up = runner.act(GameAction::TurnFaceUp {
        object_id: brago_id,
        x: 0,
    });
    assert!(turn_up.is_ok(), "turn face up succeeds");

    // Now Brago's Favor functions from command zone!
    // Spells with chosen name "Grizzly Bears" cost {1} less -> now costs {G}.
    // With 1 Forest, Grizzly Bears is now a legal cast!
    let actions_after = legal_actions(runner.state());
    assert!(
        actions_after
            .iter()
            .any(|a| matches!(a, GameAction::CastSpell { object_id, .. } if *object_id == bear_id)),
        "can cast Grizzly Bears for reduced cost after turning conspiracy face up"
    );

    let outcome = runner.cast(bear_id).resolve();
    assert!(
        outcome.is_tapped(forest_id),
        "Forest was tapped to pay the reduced cost"
    );
}

#[test]
fn limited_without_command_zone_does_not_place_command_cards() {
    let mut state = GameState::new_two_player(7);
    state.format_config = FormatConfig::limited(); // command_zone: false

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: forty_card_main(),
            commander: vec![DeckEntry {
                card: conspiracy_card_face("Brago's Favor", true),
                count: 1,
            }],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: forty_card_main(),
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    // When command_zone is disabled, commander slot cards are not placed
    assert!(
        state.command_zone.is_empty(),
        "command zone is disabled so no cards are placed"
    );
    assert_eq!(state.players[0].library.len(), 40);
}

// CR 905.4: only conspiracy cards start the game in a Limited command zone. The
// non-conspiracy in the slot is the row a pass-everything loader would seat.
#[test]
fn limited_command_zone_seats_only_conspiracies() {
    let mut state = GameState::new_two_player(7);
    load_limited_decks(
        &mut state,
        vec![
            plain_card_face("Grizzly Bears"),
            conspiracy_card_face("Worldknit", false),
        ],
        vec![],
    );

    let seated: Vec<&str> = state
        .command_zone
        .iter()
        .map(|id| state.objects[id].name.as_str())
        .collect();
    assert_eq!(seated, vec!["Worldknit"]);
}

// CR 702.106a + CR 702.106f: the production path from deck load to mulligans.
// P1 holds the double agenda so the opponent's view of it can be compared with
// P0's view of a hidden agenda, and P0's prompt is proved to come first.
#[test]
fn pregame_agenda_prompt_collects_secret_names_for_every_player() {
    let mut state = GameState::new_two_player(7);
    load_limited_decks(
        &mut state,
        vec![conspiracy_card_face("Brago's Favor", true)],
        vec![summoners_bond_face()],
    );
    let brago_id = state.command_zone[0];
    let bond_id = state.command_zone[1];

    start_game_with_starting_player(&mut state, P0);
    assert_eq!(
        state.waiting_for,
        WaitingFor::SecretAgendaChoice {
            player: P0,
            object_id: brago_id,
            name_count: 1,
        },
        "no mulligan until the agenda is named"
    );

    let mut runner = GameRunner::from_state(state);

    // Hidden agenda: exactly one real card name.
    for rejected in [
        names(&[]),
        names(&["Grizzly Bears", "Runeclaw Bear"]),
        names(&["Not A Real Card"]),
    ] {
        assert!(runner.act(rejected).is_err());
        assert!(runner.state().objects[&brago_id]
            .secret_chosen_names
            .is_empty());
    }
    let answered = runner.act(names(&["Grizzly Bears"])).expect("legal answer");
    assert!(
        answered.events.is_empty(),
        "the secret choice publishes no event: {:?}",
        answered.events
    );

    // CR 702.106f: the controller sees two names are required; nobody else does.
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::SecretAgendaChoice {
            player: P1,
            object_id: bond_id,
            name_count: 2,
        }
    );
    assert_eq!(
        filter_state_for_viewer(runner.state(), P1).waiting_for,
        runner.state().waiting_for
    );
    let p0_view = filter_state_for_viewer(runner.state(), P0);
    assert_eq!(
        p0_view.waiting_for,
        WaitingFor::SecretAgendaChoice {
            player: P1,
            object_id: bond_id,
            name_count: 1,
        }
    );
    assert_eq!(p0_view.objects[&bond_id].agenda_name_count, 0);
    assert!(
        p0_view.objects[&bond_id].is_conspiracy,
        "a face-down conspiracy stays displayable as one"
    );

    // The AI's offer answers the prompt with two different names.
    let offered = legal_actions(runner.state());
    assert!(matches!(
        offered.as_slice(),
        [GameAction::ChooseSecretAgendaNames { names }]
            if names.len() == 2 && names[0] != names[1]
    ));

    // Double agenda: exactly two different names.
    for rejected in [
        names(&["Grizzly Bears"]),
        names(&["Grizzly Bears", "grizzly bears"]),
    ] {
        assert!(runner.act(rejected).is_err());
        assert!(runner.state().objects[&bond_id]
            .secret_chosen_names
            .is_empty());
    }
    runner
        .act(names(&["Grizzly Bears", "Runeclaw Bear"]))
        .expect("legal answer");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::MulliganDecision { .. }
        ),
        "pregame continues once every agenda is named: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&bond_id].secret_chosen_names,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()]
    );
    for viewer in [Some(P0), None] {
        let wire = serde_json::to_string(&ClientGameStateRef::wrap(runner.state(), viewer))
            .expect("wire state serializes");
        assert!(!wire.contains("Summoner's Bond"), "viewer {viewer:?}");
        assert!(!wire.contains("Runeclaw Bear"), "viewer {viewer:?}");
    }
}

// CR 702.106a: another player can't answer for the seat being asked.
#[test]
fn pregame_agenda_prompt_rejects_other_seats() {
    let mut state = GameState::new_two_player(7);
    load_limited_decks(
        &mut state,
        vec![conspiracy_card_face("Brago's Favor", true)],
        vec![],
    );
    start_game_with_starting_player(&mut state, P0);

    assert!(apply(&mut state, P1, names(&["Grizzly Bears"])).is_err());
    assert!(matches!(
        state.waiting_for,
        WaitingFor::SecretAgendaChoice { player: P0, .. }
    ));
}
