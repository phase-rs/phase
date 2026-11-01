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
//! - Secretly chosen names (single and double agenda) are transferred to chosen attributes
//!   and power cost reductions when revealed

use engine::ai_support::legal_actions;
use engine::game::deck_loading::{
    create_command_zone_card_from_card_face, load_deck_into_state, DeckEntry, DeckPayload,
    PlayerDeckPayload,
};
use engine::game::derived_views::ClientGameStateRef;
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::visibility::{filter_state_for_viewer, HIDDEN_CARD_NAME};
use engine::types::ability::{
    ChosenAttribute, ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::{DeckSizeRule, FormatConfig, GameFormat};
use engine::types::game_state::GameState;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::statics::{CostModifyMode, CostReductionReach, StaticMode};

fn plain_card_face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        ..Default::default()
    }
}

fn conspiracy_card_face(name: &str, hidden_agenda: bool, chosen_names: Vec<String>) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        chosen_names,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Conspiracy);
    if hidden_agenda {
        face.oracle_text = Some(
            "Hidden agenda (Start the game with this conspiracy face down in the command zone and secretly choose a card name.)\nSpells with the chosen name you cast cost {1} less to cast."
                .to_string(),
        );
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

fn double_agenda_conspiracy_face(name: &str, chosen_names: Vec<String>) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        chosen_names,
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Conspiracy);
    face.oracle_text = Some(
        "Double agenda (Start the game with this conspiracy face down in the command zone and secretly choose two card names.)\nSpells with the chosen names you cast cost {1} less to cast."
            .to_string(),
    );
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
    face
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
    state.format_config = FormatConfig::limited_with_command_zone();

    // 40 main-deck cards + 2 command-zone cards
    let main_entries: Vec<DeckEntry> = (0..40)
        .map(|i| DeckEntry {
            card: plain_card_face(&format!("Plains {i}")),
            count: 1,
        })
        .collect();

    let worldknit_face = conspiracy_card_face("Worldknit", false, vec![]);
    let brago_face = conspiracy_card_face("Brago's Favor", true, vec!["Grizzly Bears".to_string()]);

    let commander_entries = vec![
        DeckEntry {
            card: worldknit_face.clone(),
            count: 1,
        },
        DeckEntry {
            card: brago_face.clone(),
            count: 1,
        },
    ];

    let p0_payload = PlayerDeckPayload {
        main_deck: main_entries.clone(),
        commander: commander_entries,
        ..Default::default()
    };
    let p1_payload = PlayerDeckPayload {
        main_deck: main_entries,
        ..Default::default()
    };
    let payload = DeckPayload {
        player: p0_payload,
        opponent: p1_payload,
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

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
    let mut state = GameState::new_two_player(7);
    state.format_config = FormatConfig::limited_with_command_zone();
    state.turn_number = 1;
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let brago_face = conspiracy_card_face("Brago's Favor", true, vec!["Grizzly Bears".to_string()]);
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: (0..40)
                .map(|i| DeckEntry {
                    card: plain_card_face(&format!("Plains {i}")),
                    count: 1,
                })
                .collect(),
            commander: vec![DeckEntry {
                card: brago_face,
                count: 1,
            }],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: (0..40)
                .map(|i| DeckEntry {
                    card: plain_card_face(&format!("Plains {i}")),
                    count: 1,
                })
                .collect(),
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    let brago_id = state.command_zone[0];
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
    let result = runner.act(GameAction::TurnFaceUp {
        object_id: brago_id,
        x: 0,
    });
    assert!(result.is_ok(), "turn face up special action succeeds");

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
    let mut state = GameState::new_two_player(7);
    state.format_config = FormatConfig::limited_with_command_zone();
    state.turn_number = 1;
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let double_agenda = double_agenda_conspiracy_face(
        "Double Agenda Conspiracy",
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()],
    );

    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: (0..40)
                .map(|i| DeckEntry {
                    card: plain_card_face(&format!("Plains {i}")),
                    count: 1,
                })
                .collect(),
            commander: vec![DeckEntry {
                card: double_agenda,
                count: 1,
            }],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: (0..40)
                .map(|i| DeckEntry {
                    card: plain_card_face(&format!("Plains {i}")),
                    count: 1,
                })
                .collect(),
            ..Default::default()
        },
        ..Default::default()
    };

    load_deck_into_state(&mut state, &payload);

    let obj_id = state.command_zone[0];
    assert!(state.objects[&obj_id].face_down);
    assert_eq!(
        state.objects[&obj_id].secret_chosen_names,
        vec!["Grizzly Bears".to_string(), "Runeclaw Bear".to_string()]
    );
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
    let mut state = GameState::new_two_player(7);
    state.format_config = FormatConfig::limited_with_command_zone();
    state.turn_number = 1;
    state.active_player = P0;
    state.priority_player = P0;
    state.phase = Phase::PreCombatMain;
    state.waiting_for = WaitingFor::Priority { player: P0 };

    let brago_face = conspiracy_card_face("Brago's Favor", true, vec!["Grizzly Bears".to_string()]);
    let brago_id = create_command_zone_card_from_card_face(&mut state, &brago_face, P0);

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

    let brago_face = conspiracy_card_face("Brago's Favor", true, vec!["Grizzly Bears".to_string()]);
    let payload = DeckPayload {
        player: PlayerDeckPayload {
            main_deck: (0..40)
                .map(|i| DeckEntry {
                    card: plain_card_face(&format!("Plains {i}")),
                    count: 1,
                })
                .collect(),
            commander: vec![DeckEntry {
                card: brago_face,
                count: 1,
            }],
            ..Default::default()
        },
        opponent: PlayerDeckPayload {
            main_deck: (0..40)
                .map(|i| DeckEntry {
                    card: plain_card_face(&format!("Plains {i}")),
                    count: 1,
                })
                .collect(),
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
