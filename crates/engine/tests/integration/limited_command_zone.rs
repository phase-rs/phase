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
//!   during priority (CR 702.106c / CR 116.2k)

use engine::ai_support::legal_actions;
use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use engine::game::scenario::{GameRunner, P0, P1};
use engine::game::visibility::{filter_state_for_viewer, HIDDEN_CARD_NAME};
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::{DeckSizeRule, FormatConfig, GameFormat};
use engine::types::game_state::GameState;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

fn plain_card_face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        ..Default::default()
    }
}

fn conspiracy_card_face(name: &str, hidden_agenda: bool) -> CardFace {
    let mut face = CardFace {
        name: name.to_string(),
        ..Default::default()
    };
    face.card_type.core_types.push(CoreType::Conspiracy);
    if hidden_agenda {
        face.oracle_text = Some(
            "Hidden agenda (Start the game with this conspiracy face down in the command zone.)"
                .to_string(),
        );
    } else {
        face.oracle_text = Some("Conspiracy text".to_string());
    }
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

    let worldknit_face = conspiracy_card_face("Worldknit", false);
    let brago_face = conspiracy_card_face("Brago's Favor", true);

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

    let brago_face = conspiracy_card_face("Brago's Favor", true);
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

    // CR 702.106c / CR 116.2k: candidate offer is available during priority
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
fn limited_without_command_zone_does_not_place_command_cards() {
    let mut state = GameState::new_two_player(7);
    state.format_config = FormatConfig::limited(); // command_zone: false

    let brago_face = conspiracy_card_face("Brago's Favor", true);
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
