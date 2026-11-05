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
//! - Pregame companion reveals precede secret agenda choices (CR 103.2b / CR 103.2e)
//! - Pregame companion-reveal walks skip players who have left the game (CR 800.4a)

use engine::ai_support::legal_actions;
use engine::game::companion::check_companion_reveal;
use engine::game::conspiracy::record_secret_agenda_choices;
use engine::game::deck_loading::{
    create_command_zone_card_from_card_face, load_deck_into_state, DeckEntry, DeckPayload,
    PlayerDeckPayload,
};
use engine::game::derived_views::ClientGameStateRef;
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::interaction::{
    bind_interaction_authority, derive_viewer_interaction, submit_interaction,
};
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::visibility::{
    filter_events_for_viewer, filter_state_for_unseated_viewer, filter_state_for_viewer,
    HIDDEN_CARD_NAME,
};
use engine::types::ability::{
    ChosenAttribute, ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::{DeckSizeRule, FormatConfig, GameFormat};
use engine::types::game_state::{CompanionDeclaration, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunity, InteractionOpportunityResponse, InteractionPresentationSurface,
    InteractionReasonCode, InteractionResponse, InteractionResponseSpec, InteractionRoleCode,
    InteractionSessionId, InteractionSubmission,
};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{CostModifyMode, CostReductionReach, StaticMode};
use engine::types::zones::Zone;
use std::sync::Arc;

fn plain_card_face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        ..Default::default()
    }
}

const BRAGOS_FAVOR_ORACLE: &str = "Hidden agenda (Start the game with this conspiracy face down in the command zone and secretly choose a card name. You may turn this conspiracy face up any time and reveal that name.)\nSpells with the chosen name you cast cost {1} less to cast.";
const WORLDKNIT_ORACLE: &str = "(Start the game with this conspiracy face up in the command zone.)\nAs long as every card in your card pool started the game in your library or in the command zone, lands you control have \"{T}: Add one mana of any color.\"";
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
        face.oracle_text = Some(WORLDKNIT_ORACLE.to_string());
    }
    face
}

/// CR 702.139: the committed Lurrus fixture. Its Companion condition is
/// fulfilled by the fixture decks (40 permanents with mana value 0), so a seat
/// that lists it in its sideboard is offered a companion reveal.
fn companion_face() -> CardFace {
    crate::support::shared_card_db()
        .expect("committed integration database is required")
        .get_face_by_name("Lurrus of the Dream-Den")
        .expect("fixture holds Lurrus")
        .clone()
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
    assert_eq!(p1_brago.mana_cost, ManaCost::NoCost);
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
    let brago_id = create_command_zone_card_from_card_face(&mut state, &brago_face, None, P0);
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

fn agenda_opportunity(state: &GameState, viewer: PlayerId) -> Option<InteractionOpportunity> {
    derive_viewer_interaction(state, &filter_state_for_viewer(state, viewer), viewer)
        .opportunities
        .into_iter()
        .next()
}

fn text_submission(opportunity: &InteractionOpportunity, values: &[&str]) -> InteractionSubmission {
    InteractionSubmission {
        interaction_id: opportunity.interaction_id.clone(),
        response: InteractionResponse::Text {
            values: values.iter().map(|value| value.to_string()).collect(),
        },
    }
}

fn assert_agenda_text_schema(opportunity: &InteractionOpportunity, source: &str, count: u32) {
    let InteractionOpportunityResponse::Schema { spec, candidates } = &opportunity.response else {
        panic!(
            "an exact list limits the agenda to its entries: {:?}",
            opportunity.response
        );
    };
    assert!(
        matches!(
            spec,
            InteractionResponseSpec::Text {
                allow_arbitrary: true,
                count: spec_count,
                ..
            } if *spec_count == count
        ),
        "{spec:?}"
    );
    assert!(candidates.is_empty(), "{candidates:?}");
    assert_eq!(opportunity.progress.minimum, count);
    assert_eq!(opportunity.progress.maximum, Some(count));
    assert!(opportunity.surfaces.iter().any(|surface| matches!(
        surface,
        InteractionPresentationSurface::Value {
            role: InteractionRoleCode::Source,
            value,
            ..
        } if value == source
    )));
}

// CR 702.106a / CR 702.106f: a player answering through the interaction schema
// names cards of their own choosing, not the one set generated for the AI.
#[test]
fn pregame_agenda_interaction_accepts_player_supplied_names() {
    let mut state = GameState::new_two_player(7);
    load_limited_decks(
        &mut state,
        vec![conspiracy_card_face("Brago's Favor", true)],
        vec![summoners_bond_face()],
    );
    let brago_id = state.command_zone[0];
    let bond_id = state.command_zone[1];
    start_game_with_starting_player(&mut state, P0);
    state.all_card_names = known_card_names();
    bind_interaction_authority(&mut state, InteractionSessionId("agenda".to_string()))
        .expect("valid interaction authority binding");

    assert!(
        agenda_opportunity(&state, P1).is_none(),
        "the other seat is offered nothing to answer"
    );

    // Reach guard: the AI's single answer is not the name submitted below.
    let player_name = "Runeclaw Bear";
    assert!(matches!(
        legal_actions(&state).as_slice(),
        [GameAction::ChooseSecretAgendaNames { names }]
            if names.len() == 1 && names[0] != player_name
    ));

    let hidden_agenda = agenda_opportunity(&state, P0).expect("P0 owes an agenda name");
    assert_agenda_text_schema(&hidden_agenda, "Brago's Favor", 1);

    for (values, code) in [
        (vec![], Some(InteractionReasonCode::ConstraintUnsatisfied)),
        (
            vec!["Grizzly Bears", player_name],
            Some(InteractionReasonCode::ConstraintUnsatisfied),
        ),
        (vec!["Not A Real Card"], None),
    ] {
        let rejection =
            submit_interaction(&mut state, P0, text_submission(&hidden_agenda, &values))
                .expect_err("not a legal agenda answer");
        if let Some(code) = code {
            assert_eq!(rejection.code, code);
        }
        assert!(state.objects[&brago_id].secret_chosen_names.is_empty());
    }
    submit_interaction(
        &mut state,
        P0,
        text_submission(&hidden_agenda, &[player_name]),
    )
    .expect("any real card name answers a hidden agenda");
    assert_eq!(
        state.objects[&brago_id].secret_chosen_names,
        vec![player_name.to_string()]
    );

    let double_agenda = agenda_opportunity(&state, P1).expect("P1 owes two agenda names");
    assert_agenda_text_schema(&double_agenda, "Summoner's Bond", 2);
    assert!(
        agenda_opportunity(&state, P0).is_none(),
        "CR 702.106f: P0 learns nothing about how many names P1 owes"
    );
    submit_interaction(
        &mut state,
        P1,
        text_submission(&double_agenda, &["Runeclaw Bear", "runeclaw bear"]),
    )
    .expect_err("double agenda requires two different cards");
    assert!(state.objects[&bond_id].secret_chosen_names.is_empty());
    submit_interaction(
        &mut state,
        P1,
        text_submission(&double_agenda, &["Runeclaw Bear", "Plains 0"]),
    )
    .expect("two different real card names answer a double agenda");

    assert_eq!(
        state.objects[&bond_id].secret_chosen_names,
        vec!["Runeclaw Bear".to_string(), "Plains 0".to_string()]
    );
    assert!(matches!(
        state.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));
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

    // Control: the same answer through the same boundary from the asked seat.
    apply(&mut state, P0, names(&["Grizzly Bears"])).expect("the asked seat may answer");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));
}

// CR 800.4a + CR 905.3: Conspiracy Draft is multiplayer, so a player can leave
// with agendas still unnamed. The game must keep asking the players who remain,
// never fall through to priority before any hand is drawn.
#[test]
fn pregame_agenda_prompt_survives_a_conceding_chooser() {
    let mut state = GameState::new(FormatConfig::limited_with_command_zone(), 3, 7);
    state.all_card_names = known_card_names();
    let seat = |cards: Vec<CardFace>| PlayerDeckPayload {
        main_deck: forty_card_main(),
        commander: cards
            .into_iter()
            .map(|card| DeckEntry { card, count: 1 })
            .collect(),
        ..Default::default()
    };
    let brago = || conspiracy_card_face("Brago's Favor", true);
    let payload = DeckPayload {
        player: seat(vec![brago()]),
        opponent: seat(vec![]),
        ai_decks: vec![seat(vec![brago()])],
        ..Default::default()
    };
    load_deck_into_state(&mut state, &payload);
    start_game_with_starting_player(&mut state, P0);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::SecretAgendaChoice { player: P0, .. }
    ));

    let p0_brago = owned_command_zone_ids(&state, P0)[0];
    let p2_brago = owned_command_zone_ids(&state, PlayerId(2))[0];
    let conceded =
        apply(&mut state, P0, GameAction::Concede { player_id: P0 }).expect("P0 concedes");

    // CR 702.106e: the departing chooser's face-down conspiracy is revealed to
    // every survivor; the surviving P2's agenda stays hidden.
    for survivor in [P1, PlayerId(2)] {
        assert_eq!(
            revealed_conspiracies(&conceded.events, &state, survivor),
            vec![(P0, vec![p0_brago], vec!["Brago's Favor".to_string()])],
            "survivor {survivor:?} sees the departing player's conspiracy"
        );
    }
    assert!(state.objects[&p2_brago].face_down);

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::SecretAgendaChoice {
                player: PlayerId(2),
                ..
            }
        ),
        "the remaining agenda is still asked: {:?}",
        state.waiting_for
    );
    apply(&mut state, PlayerId(2), names(&["Grizzly Bears"])).expect("P2 answers");
    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    let pending_players: Vec<PlayerId> = pending.iter().map(|e| e.player).collect();
    assert_eq!(
        pending_players,
        vec![PlayerId(1), PlayerId(2)],
        "eliminated P0 is excluded from the new mulligan round"
    );

    // CR 702.106e: a later departure reveals only its own player's conspiracy;
    // the first departure's card is not disclosed again.
    let conceded = apply(
        &mut state,
        PlayerId(2),
        GameAction::Concede {
            player_id: PlayerId(2),
        },
    )
    .expect("P2 concedes");
    assert_eq!(
        revealed_conspiracies(&conceded.events, &state, P1),
        vec![(
            PlayerId(2),
            vec![p2_brago],
            vec!["Brago's Favor".to_string()]
        )],
    );
}

/// CR 702.139a + CR 800.4a + CR 103.2b: the first companion-reveal walk
/// (`check_all_companion_reveals`, the pregame entry) must skip a departed
/// player. Concede through the public action path before the game starts,
/// while the seat still owns an eligible companion, then start the game: the
/// walk must not offer that seat a reveal and the survivors must reach
/// mulligans with only living seats pending.
#[test]
fn eliminated_companion_holder_is_skipped_for_pregame_walks() {
    let mut state = GameState::new(FormatConfig::limited_with_command_zone(), 3, 7);
    state.all_card_names = known_card_names();
    let seat = |sideboard: Vec<CardFace>| PlayerDeckPayload {
        main_deck: forty_card_main(),
        sideboard: sideboard
            .into_iter()
            .map(|card| DeckEntry { card, count: 1 })
            .collect(),
        ..Default::default()
    };
    let payload = DeckPayload {
        player: seat(vec![companion_face()]),
        opponent: seat(vec![]),
        ai_decks: vec![seat(vec![])],
        ..Default::default()
    };
    load_deck_into_state(&mut state, &payload);

    // CR 104.3a: the public concession path, before pregame begins.
    apply(&mut state, P0, GameAction::Concede { player_id: P0 })
        .expect("P0 concedes before the game begins");
    assert!(!engine::game::players::is_alive(&state, P0));
    // Reach guard: the departed seat's pool still holds the eligible companion,
    // which is exactly what the first walk must decline to offer.
    assert!(check_companion_reveal(&state, P0).is_some());

    start_game_with_starting_player(&mut state, P1);
    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    let pending_players: Vec<PlayerId> = pending.iter().map(|entry| entry.player).collect();
    assert_eq!(
        pending_players,
        vec![P1, PlayerId(2)],
        "the departed companion holder gets no reveal prompt and no mulligan entry"
    );
}

/// CR 103.2b + CR 103.2e + CR 702.106a: companion reveals are step 103.2b and
/// conspiracies enter the command zone in step 103.2e, with hidden-agenda names
/// chosen under CR 702.106a. The production pregame path must offer the
/// companion reveal first, then the agenda prompt, and only then mulligans.
#[test]
fn companion_reveals_precede_secret_agenda_choices() {
    let mut state = GameState::new(FormatConfig::limited_with_command_zone(), 2, 7);
    state.all_card_names = known_card_names();
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: forty_card_main(),
            sideboard: vec![DeckEntry {
                card: companion_face(),
                count: 1,
            }],
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
    let brago_id = owned_command_zone_ids(&state, P0)[0];

    start_game_with_starting_player(&mut state, P0);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::CompanionReveal { player: P0, .. }
        ),
        "CR 103.2b: the companion reveal comes before the agenda prompt: {:?}",
        state.waiting_for
    );

    let mut runner = GameRunner::from_state(state);
    runner
        .act(GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Decline,
        })
        .expect("decline the companion");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::SecretAgendaChoice {
                player: P0,
                object_id,
                name_count: 1,
            } if object_id == brago_id
        ),
        "CR 103.2e: the secret agenda prompt follows the companion step: {:?}",
        runner.state().waiting_for
    );

    runner
        .act(names(&["Grizzly Bears"]))
        .expect("name the hidden agenda");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::MulliganDecision { .. }
        ),
        "mulligans follow the agenda step: {:?}",
        runner.state().waiting_for
    );
}

/// CR 702.139a + CR 800.4a: the second walk (`advance_companion_reveal`, after a
/// player answers their own offer) must also skip a departed player later in seat
/// order. P1 is next after P0 and holds an eligible companion, so before the fix
/// declining P0's offer would create a `CompanionReveal` for a seat that left.
#[test]
fn companion_reveal_walk_skips_a_conceding_later_seat() {
    let mut state = GameState::new(FormatConfig::limited_with_command_zone(), 3, 7);
    state.all_card_names = known_card_names();
    let seat = |companion: Vec<CardFace>| PlayerDeckPayload {
        main_deck: forty_card_main(),
        sideboard: companion
            .into_iter()
            .map(|card| DeckEntry { card, count: 1 })
            .collect(),
        ..Default::default()
    };
    let payload = DeckPayload {
        player: seat(vec![companion_face()]),
        opponent: seat(vec![companion_face()]),
        ai_decks: vec![seat(vec![])],
        ..Default::default()
    };
    load_deck_into_state(&mut state, &payload);
    start_game_with_starting_player(&mut state, P0);
    assert!(matches!(
        state.waiting_for,
        WaitingFor::CompanionReveal { player: P0, .. }
    ));
    // Reach guard: P1, the seat the post-P0 walk would visit next, is eligible.
    assert!(matches!(
        check_companion_reveal(&state, P1),
        Some(WaitingFor::CompanionReveal { player, .. }) if player == P1
    ));

    apply(&mut state, P1, GameAction::Concede { player_id: P1 }).expect("P1 concedes");
    assert!(
        matches!(state.waiting_for, WaitingFor::CompanionReveal { player, .. } if player == P0),
        "a later seat's departure leaves P0's own offer intact: {:?}",
        state.waiting_for
    );

    let mut runner = GameRunner::from_state(state);
    runner
        .act(GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Decline,
        })
        .expect("decline P0's companion");

    let WaitingFor::MulliganDecision { pending, .. } = &runner.state().waiting_for else {
        panic!(
            "expected MulliganDecision, got {:?}",
            runner.state().waiting_for
        );
    };
    let pending_players: Vec<PlayerId> = pending.iter().map(|entry| entry.player).collect();
    assert_eq!(
        pending_players,
        vec![P0, PlayerId(2)],
        "the departed P1 is never offered its companion and gets no mulligan entry"
    );
}

/// CR 702.139a + CR 800.4a + CR 103.2b: a departure by the **current**
/// companion chooser must resume the reveal walk at that chooser's seat cursor.
/// Later living seats keep their offers, and the agenda step that follows the
/// walk (CR 103.2e) still runs before mulligans. P0 remains eligible after
/// declining, so reaching P2 without re-offering P0 proves the walk continued
/// from the cursor instead of restarting from the front of seat order.
#[test]
fn conceding_current_companion_chooser_resumes_the_walk_before_agendas() {
    let mut state = GameState::new(FormatConfig::limited_with_command_zone(), 3, 7);
    state.all_card_names = known_card_names();
    let p2 = PlayerId(2);
    let entries = |cards: Vec<CardFace>| {
        cards
            .into_iter()
            .map(|card| DeckEntry { card, count: 1 })
            .collect::<Vec<_>>()
    };
    let seat = |companions: Vec<CardFace>| PlayerDeckPayload {
        main_deck: forty_card_main(),
        sideboard: entries(companions),
        ..Default::default()
    };
    let payload = DeckPayload {
        player: seat(vec![companion_face()]),
        opponent: seat(vec![companion_face()]),
        ai_decks: vec![PlayerDeckPayload {
            main_deck: forty_card_main(),
            sideboard: entries(vec![companion_face()]),
            commander: entries(vec![conspiracy_card_face("Brago's Favor", true)]),
            ..Default::default()
        }],
        ..Default::default()
    };
    load_deck_into_state(&mut state, &payload);
    let p2_brago = owned_command_zone_ids(&state, p2)[0];

    // Reach guard: every seat holds an eligible companion, so the walk visits
    // all three in seat order and P0's decline alone cannot end it.
    for player in [P0, P1, p2] {
        assert!(
            check_companion_reveal(&state, player).is_some(),
            "seat {player:?} must hold an eligible companion for this walk"
        );
    }

    start_game_with_starting_player(&mut state, P0);
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::CompanionReveal { player: P0, .. }
        ),
        "CR 103.2b: the walk starts at the first seat: {:?}",
        state.waiting_for
    );

    apply(
        &mut state,
        P0,
        GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Decline,
        },
    )
    .expect("decline P0's companion");
    assert!(
        matches!(state.waiting_for, WaitingFor::CompanionReveal { player, .. } if player == P1),
        "the walk reaches P1: {:?}",
        state.waiting_for
    );

    // CR 104.3a: the current chooser leaves. Two players remain, so this is not
    // a game-ending departure.
    apply(&mut state, P1, GameAction::Concede { player_id: P1 }).expect("P1 concedes");

    // P0 still *could* be offered (declining does not consume the sideboard
    // offer), so P2's prompt is only reachable through the departure cursor.
    assert!(
        check_companion_reveal(&state, P0).is_some(),
        "P0 remains eligible, so a restarted walk would re-offer it"
    );
    assert!(
        matches!(state.waiting_for, WaitingFor::CompanionReveal { player, .. } if player == p2),
        "CR 800.4a: resume after the departed chooser without re-offering P0: {:?}",
        state.waiting_for
    );

    apply(
        &mut state,
        p2,
        GameAction::DeclareCompanion {
            choice: CompanionDeclaration::Decline,
        },
    )
    .expect("decline P2's companion");

    // CR 103.2e + CR 702.106a: the walk's end still hands off to the surviving
    // seat's unnamed agenda, not to ordinary priority.
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::SecretAgendaChoice {
                player,
                object_id,
                name_count: 1,
            } if player == p2 && object_id == p2_brago
        ),
        "the surviving agenda is still asked: {:?}",
        state.waiting_for
    );
    apply(&mut state, p2, names(&["Grizzly Bears"])).expect("P2 answers its agenda");

    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    let pending_players: Vec<PlayerId> = pending.iter().map(|entry| entry.player).collect();
    assert_eq!(
        pending_players,
        vec![P0, p2],
        "mulligans follow the agenda step for exactly the living seats"
    );
}

/// CR 702.106e: a player who leaves after the pregame procedure reveals their
/// face-down conspiracy to all players, once, without turning it face up: the
/// secret name is not published as a chosen name, and an already face-up
/// conspiracy is not part of the reveal.
#[test]
fn departing_player_reveals_face_down_conspiracies_after_pregame() {
    let mut state = priority_state();
    load_limited_decks(
        &mut state,
        vec![
            conspiracy_card_face("Brago's Favor", true),
            conspiracy_card_face("Worldknit", false),
        ],
        vec![],
    );
    let brago_id = state
        .command_zone
        .iter()
        .copied()
        .find(|id| state.objects[id].face_down)
        .expect("hidden agenda starts face down");
    choose_names(&mut state, brago_id, &["Grizzly Bears"]);
    assert_eq!(
        filter_state_for_viewer(&state, P1).objects[&brago_id].name,
        HIDDEN_CARD_NAME,
        "identity is hidden from the opponent before the departure"
    );

    let conceded =
        apply(&mut state, P0, GameAction::Concede { player_id: P0 }).expect("P0 concedes");

    assert_eq!(
        revealed_conspiracies(&conceded.events, &state, P1),
        vec![(P0, vec![brago_id], vec!["Brago's Favor".to_string()])],
        "exactly one reveal, of the face-down conspiracy only"
    );
    assert!(
        !conceded
            .events
            .iter()
            .any(|event| matches!(event, GameEvent::CardNamed { .. })),
        "the departure reveal is not the turn-face-up special action"
    );
    assert!(
        !state.command_zone.contains(&brago_id),
        "CR 800.4a: the conspiracy leaves the game after it is revealed"
    );
}

/// CR 702.106e: when the game ends, every face-down conspiracy still in the
/// command zone is revealed to all players. The departing player's card is
/// revealed by the departure and not again at the game end.
#[test]
fn game_end_reveals_every_remaining_face_down_conspiracy_once() {
    let mut state = priority_state();
    load_limited_decks(
        &mut state,
        vec![conspiracy_card_face("Brago's Favor", true)],
        vec![conspiracy_card_face("Brago's Favor", true)],
    );
    let p0_brago = owned_command_zone_ids(&state, P0)[0];
    let p1_brago = owned_command_zone_ids(&state, P1)[0];
    assert!(state.objects[&p1_brago].face_down);
    assert_eq!(
        filter_state_for_viewer(&state, P0).objects[&p1_brago].name,
        HIDDEN_CARD_NAME,
        "P1's agenda is hidden from P0 during the game"
    );

    let conceded =
        apply(&mut state, P0, GameAction::Concede { player_id: P0 }).expect("P0 concedes");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P1) }
    ));
    for viewer in [P0, P1] {
        assert_eq!(
            revealed_conspiracies(&conceded.events, &state, viewer),
            vec![
                (P0, vec![p0_brago], vec!["Brago's Favor".to_string()]),
                (P1, vec![p1_brago], vec!["Brago's Favor".to_string()]),
            ],
            "viewer {viewer:?} sees the departure reveal, then the game-end reveal"
        );
    }
    let game_over = conceded
        .events
        .iter()
        .position(|event| matches!(event, GameEvent::GameOver { .. }))
        .expect("the game ends");
    assert!(
        conceded.events[..game_over]
            .iter()
            .filter(|event| matches!(event, GameEvent::CardsRevealed { .. }))
            .count()
            == 2,
        "both reveals happen before the game is over"
    );
    assert!(
        state.objects[&p1_brago].face_down,
        "the game-end reveal does not turn the conspiracy face up"
    );
}

/// CR 702.106c + CR 702.106e: the game-end reveal publishes the printed card to
/// every player, but the secretly chosen names stay the controller's. An opponent's
/// game-end `ClientGameStateRef` must carry the printed double agenda and neither of
/// its chosen names — nor how many names it takes.
#[test]
fn game_end_reveal_keeps_secret_agenda_names_private_to_the_controller() {
    let mut state = priority_state();
    load_limited_decks(&mut state, vec![], vec![summoners_bond_face()]);
    let bond_id = owned_command_zone_ids(&state, P1)[0];
    record_secret_agenda_choices(
        &mut state,
        bond_id,
        P1,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()],
    )
    .expect("valid double-agenda names");
    assert!(
        state.objects[&bond_id].face_down,
        "the double agenda is still face down when the game ends"
    );
    assert_eq!(
        filter_state_for_viewer(&state, P0).objects[&bond_id].name,
        HIDDEN_CARD_NAME,
        "reach guard: the agenda is hidden from its opponent before the game ends"
    );

    // The secrecy must hold in the running game too, not only in the game-end
    // projection: the opponent's face-down view and wire carry neither the names
    // nor the count. The controller's wire carries both, so these assertions
    // discriminate the redaction from a field the wire never serializes.
    let in_game_opponent_view = filter_state_for_viewer(&state, P0);
    assert!(in_game_opponent_view.objects[&bond_id]
        .secret_chosen_names
        .is_empty());
    assert_eq!(in_game_opponent_view.objects[&bond_id].agenda_name_count, 0);
    let in_game_opponent_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P0)))
        .expect("wire state serializes");
    assert!(!in_game_opponent_wire.contains("Runeclaw Bear"));
    assert!(!in_game_opponent_wire.contains("Grizzly Bears"));
    assert!(!in_game_opponent_wire.contains("secret_chosen_names"));
    assert!(!in_game_opponent_wire.contains("agenda_name_count"));
    let in_game_controller_wire =
        serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P1)))
            .expect("wire state serializes");
    assert!(in_game_controller_wire.contains("Runeclaw Bear"));
    assert!(in_game_controller_wire.contains("agenda_name_count"));

    apply(&mut state, P0, GameAction::Concede { player_id: P0 }).expect("P0 concedes");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P1) }
    ));
    assert!(
        state.objects[&bond_id].face_down,
        "the game-end reveal does not turn the conspiracy face up"
    );
    assert_eq!(
        state.objects[&bond_id].secret_chosen_names,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()],
        "the authoritative state keeps the controller's choices"
    );

    // Reach guard: the controller receives both the printed card and the choices,
    // so the opponent assertions below discriminate only the agenda privacy.
    let controller_view = filter_state_for_viewer(&state, P1);
    assert_eq!(controller_view.objects[&bond_id].name, "Summoner's Bond");
    assert_eq!(
        controller_view.objects[&bond_id].secret_chosen_names,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()]
    );
    assert_eq!(controller_view.objects[&bond_id].agenda_name_count, 2);

    let opponent_view = filter_state_for_viewer(&state, P0);
    assert_eq!(
        opponent_view.objects[&bond_id].name, "Summoner's Bond",
        "CR 702.106e: the printed card is public at game end"
    );
    assert!(
        opponent_view.objects[&bond_id]
            .card_types
            .core_types
            .contains(&CoreType::Conspiracy),
        "the printed card type is revealed with the card"
    );
    assert!(
        opponent_view.objects[&bond_id].is_conspiracy,
        "the display classification stays"
    );
    assert!(opponent_view.objects[&bond_id]
        .secret_chosen_names
        .is_empty());
    assert_eq!(opponent_view.objects[&bond_id].agenda_name_count, 0);

    // The wire payload is the actual leak vector.
    let opponent_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P0)))
        .expect("wire state serializes");
    assert!(
        opponent_wire.contains("Summoner's Bond"),
        "the printed card identity is public at game end"
    );
    assert!(
        !opponent_wire.contains("Runeclaw Bear"),
        "the opponent's wire carries no secret agenda name"
    );
    assert!(!opponent_wire.contains("Grizzly Bears"));
    assert!(!opponent_wire.contains("secret_chosen_names"));
    assert!(!opponent_wire.contains("agenda_name_count"));

    let controller_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P1)))
        .expect("wire state serializes");
    assert!(controller_wire.contains("Runeclaw Bear"));
    assert!(controller_wire.contains("agenda_name_count"));

    let spectator_view = filter_state_for_unseated_viewer(&state);
    assert!(spectator_view.objects[&bond_id]
        .secret_chosen_names
        .is_empty());
    assert_eq!(spectator_view.objects[&bond_id].agenda_name_count, 0);

    let spectator_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, None))
        .expect("spectator wire state serializes");
    assert!(
        !spectator_wire.contains("Runeclaw Bear"),
        "the spectator's wire carries no secret agenda name"
    );
    assert!(!spectator_wire.contains("Grizzly Bears"));
    assert!(!spectator_wire.contains("secret_chosen_names"));
    assert!(!spectator_wire.contains("agenda_name_count"));
}

/// CR 702.106e + CR 702.106f: a game that ends before the agenda prompt is
/// answered still reveals only the printed card. The stamped name count is the
/// whole remaining secret — no name was ever recorded — and a double-agenda
/// count would disclose that more than one name exists.
#[test]
fn game_end_reveal_hides_an_unanswered_double_agenda_count() {
    let mut state = priority_state();
    load_limited_decks(&mut state, vec![], vec![summoners_bond_face()]);
    let bond_id = owned_command_zone_ids(&state, P1)[0];
    assert_eq!(
        state.objects[&bond_id].agenda_name_count, 2,
        "the double agenda is stamped at creation"
    );
    assert!(
        state.objects[&bond_id].secret_chosen_names.is_empty(),
        "reach guard: this game ends before the names are chosen"
    );
    assert!(
        state.objects[&bond_id].face_down,
        "the double agenda is still face down when the game ends"
    );

    apply(&mut state, P0, GameAction::Concede { player_id: P0 }).expect("P0 concedes");
    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P1) }
    ));

    let opponent_view = filter_state_for_viewer(&state, P0);
    assert_eq!(
        opponent_view.objects[&bond_id].name, "Summoner's Bond",
        "CR 702.106e: the printed card is public at game end"
    );
    assert_eq!(
        opponent_view.objects[&bond_id].agenda_name_count, 0,
        "CR 702.106f: an unanswered double agenda does not disclose its count"
    );

    let opponent_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P0)))
        .expect("wire state serializes");
    assert!(
        opponent_wire.contains("Summoner's Bond"),
        "the printed card identity is public at game end"
    );
    assert!(
        !opponent_wire.contains("agenda_name_count"),
        "the count is the only remaining carrier when no name was chosen"
    );

    // Reach guard: the same marker does serialize for the controller, so its
    // absence above is the redaction and not a field the wire never carries.
    let controller_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P1)))
        .expect("wire state serializes");
    assert!(
        controller_wire.contains("\"agenda_name_count\":2"),
        "the controller keeps the count of their own unanswered agenda"
    );

    // The seam redacts over the final object set, so it must hold for a wire
    // with no seat as well: a spectator is a third viewer class, not a player
    // whose private access could be read from `seat_order`.
    let spectator_view = filter_state_for_unseated_viewer(&state);
    assert_eq!(
        spectator_view.objects[&bond_id].agenda_name_count, 0,
        "CR 702.106f: an unanswered double agenda does not disclose its count to a spectator"
    );
    let spectator_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, None))
        .expect("spectator wire state serializes");
    assert!(
        !spectator_wire.contains("agenda_name_count"),
        "the spectator's wire carries no agenda name count"
    );
}

/// CR 702.106e + CR 800.4a: a named agenda is revealed when its controller
/// leaves, and leaves the game with its owner. The engine retains the departed
/// object in exile. The reveal is not the turn-face-up action (CR 702.106c),
/// so its secret names stay private even on that retained-object projection,
/// which does not pass through the command-zone face-down leaf.
#[test]
fn conceding_named_agenda_keeps_secret_names_off_every_other_wire() {
    let mut state = priority_state();
    load_limited_decks(&mut state, vec![], vec![summoners_bond_face()]);
    let bond_id = owned_command_zone_ids(&state, P1)[0];
    record_secret_agenda_choices(
        &mut state,
        bond_id,
        P1,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()],
    )
    .expect("valid double-agenda names");

    apply(&mut state, P1, GameAction::Concede { player_id: P1 }).expect("P1 concedes");

    assert!(matches!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    ));
    assert!(
        !state.command_zone.contains(&bond_id),
        "CR 800.4a: the departing player's agenda leaves the command zone"
    );
    let departed = &state.objects[&bond_id];
    assert_eq!(
        departed.zone,
        Zone::Exile,
        "the engine retains the departed object in exile"
    );
    assert!(
        departed.face_down,
        "CR 702.106e: the departure reveal does not turn the agenda face up"
    );
    assert_eq!(
        departed.secret_chosen_names,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()],
        "reach guard: the authoritative state keeps the controller's choices"
    );

    let opponent_view = filter_state_for_viewer(&state, P0);
    assert!(opponent_view.objects[&bond_id]
        .secret_chosen_names
        .is_empty());
    assert_eq!(opponent_view.objects[&bond_id].agenda_name_count, 0);
    let opponent_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P0)))
        .expect("wire state serializes");
    assert!(!opponent_wire.contains("Runeclaw Bear"));
    assert!(!opponent_wire.contains("Grizzly Bears"));
    assert!(!opponent_wire.contains("secret_chosen_names"));
    assert!(!opponent_wire.contains("agenda_name_count"));

    let spectator_view = filter_state_for_unseated_viewer(&state);
    assert!(spectator_view.objects[&bond_id]
        .secret_chosen_names
        .is_empty());
    assert_eq!(spectator_view.objects[&bond_id].agenda_name_count, 0);
    let spectator_wire = serde_json::to_string(&ClientGameStateRef::wrap(&state, None))
        .expect("spectator wire state serializes");
    assert!(
        !spectator_wire.contains("Runeclaw Bear"),
        "the spectator's wire carries no secret agenda name"
    );
    assert!(!spectator_wire.contains("Grizzly Bears"));
    assert!(!spectator_wire.contains("secret_chosen_names"));
    assert!(!spectator_wire.contains("agenda_name_count"));
}

/// The command-zone objects `owner` owns, in command-zone order.
fn owned_command_zone_ids(state: &GameState, owner: PlayerId) -> Vec<ObjectId> {
    state
        .command_zone
        .iter()
        .copied()
        .filter(|id| state.objects[id].owner == owner)
        .collect()
}

/// The `CardsRevealed` events `viewer` receives from `events`.
fn revealed_conspiracies(
    events: &[GameEvent],
    state: &GameState,
    viewer: PlayerId,
) -> Vec<(PlayerId, Vec<ObjectId>, Vec<String>)> {
    filter_events_for_viewer(events, state, viewer)
        .into_iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed {
                player,
                card_ids,
                card_names,
            } => Some((player, card_ids, card_names)),
            _ => None,
        })
        .collect()
}
