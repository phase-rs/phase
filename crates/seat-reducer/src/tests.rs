use engine::game::deck_loading::PlayerDeckList;
use engine::types::format::FormatConfig;
use phase_ai::config::Platform;

use crate::types::*;

struct NoOpResolver;
impl DeckResolver for NoOpResolver {
    fn resolve(&self, _choice: &DeckChoice) -> Result<PlayerDeckList, String> {
        Ok(PlayerDeckList {
            main_deck: vec![],
            sideboard: vec![],
            commander: vec![],
            ..Default::default()
        })
    }
}

fn ctx() -> ReducerCtx<'static> {
    static RESOLVER: NoOpResolver = NoOpResolver;
    ReducerCtx {
        platform: Platform::Native,
        deck_resolver: &RESOLVER,
    }
}

fn two_player_full() -> SeatState {
    SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Medium,
                deck: DeckChoice::Random,
            },
        ],
        tokens: vec!["host-token".to_string(), String::new()],
        format: FormatConfig::standard(),
        game_started: false,
    }
}

fn two_player_waiting() -> SeatState {
    SeatState {
        seats: vec![SeatKind::HostHuman, SeatKind::WaitingHuman],
        tokens: vec!["host-token".to_string(), String::new()],
        format: FormatConfig::standard(),
        game_started: false,
    }
}

#[test]
fn start_on_full_room_succeeds() {
    let mut state = two_player_full();
    let delta = crate::apply(&mut state, SeatMutation::Start, &ctx()).unwrap();
    assert!(delta.now_started);
    assert!(state.game_started);
}

#[test]
fn start_on_not_full_room_fails() {
    let mut state = two_player_waiting();
    let err = crate::apply(&mut state, SeatMutation::Start, &ctx()).unwrap_err();
    assert_eq!(err, SeatError::NotFull);
    assert!(!state.game_started);
}

#[test]
fn start_after_game_started_fails() {
    let mut state = two_player_full();
    state.game_started = true;
    let err = crate::apply(&mut state, SeatMutation::Start, &ctx()).unwrap_err();
    assert_eq!(err, SeatError::GameStarted);
}

#[test]
fn is_full_requires_all_claimed() {
    let full = two_player_full();
    assert!(full.is_full());

    let waiting = two_player_waiting();
    assert!(!waiting.is_full());
}

#[test]
fn is_full_with_joined_human() {
    let state = SeatState {
        seats: vec![SeatKind::HostHuman, SeatKind::JoinedHuman],
        tokens: vec!["host".to_string(), "guest".to_string()],
        format: FormatConfig::standard(),
        game_started: false,
    };
    assert!(state.is_full());
}

#[test]
fn to_view_strips_tokens() {
    let state = two_player_full();
    let view = state.to_view();
    assert_eq!(view.seats, state.seats);
    assert!(view.team_info.is_empty());
    assert!(view.is_full);
    assert!(!view.game_started);
    // SeatView has no tokens field — the type itself enforces the invariant
}

#[test]
fn non_team_formats_omit_team_metadata() {
    for format in [FormatConfig::standard(), FormatConfig::commander()] {
        let state = SeatState {
            seats: vec![SeatKind::HostHuman, SeatKind::WaitingHuman],
            tokens: vec!["host".to_string(), String::new()],
            format,
            game_started: false,
        };
        let view = state.to_view();
        assert!(view.team_info.is_empty());

        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("teamInfo").is_none());
    }
}

#[test]
fn two_headed_giant_team_metadata_maps_seats() {
    let state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::WaitingHuman,
            SeatKind::WaitingHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec![
            "host".to_string(),
            String::new(),
            String::new(),
            String::new(),
        ],
        format: FormatConfig::two_headed_giant(),
        game_started: false,
    };
    let view = state.to_view();
    let team_indices: Vec<u8> = view
        .team_info
        .iter()
        .map(|info| info.unwrap().team_index)
        .collect();
    let positions: Vec<u8> = view
        .team_info
        .iter()
        .map(|info| info.unwrap().position_in_team)
        .collect();

    assert_eq!(team_indices, vec![0, 0, 1, 1]);
    assert_eq!(positions, vec![0, 1, 0, 1]);
}

#[test]
fn two_headed_giant_team_metadata_survives_seat_mutations() {
    let mut state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::WaitingHuman,
            SeatKind::JoinedHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec![
            "host".to_string(),
            String::new(),
            "guest".to_string(),
            String::new(),
        ],
        format: FormatConfig::two_headed_giant(),
        game_started: false,
    };

    crate::apply(
        &mut state,
        SeatMutation::SetKind {
            seat_index: 1,
            kind: SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Medium,
                deck: DeckChoice::Random,
            },
        },
        &ctx(),
    )
    .unwrap();

    let view = state.to_view();
    assert_eq!(
        view.team_info,
        vec![
            Some(SeatTeamInfo {
                team_index: 0,
                position_in_team: 0
            }),
            Some(SeatTeamInfo {
                team_index: 0,
                position_in_team: 1
            }),
            Some(SeatTeamInfo {
                team_index: 1,
                position_in_team: 0
            }),
            Some(SeatTeamInfo {
                team_index: 1,
                position_in_team: 1
            }),
        ]
    );
}

#[test]
fn is_pregame_reflects_game_started() {
    let mut state = two_player_full();
    assert!(state.is_pregame());
    state.game_started = true;
    assert!(!state.is_pregame());
}

#[test]
fn waiting_seat_can_become_ai() {
    let mut state = two_player_waiting();
    let delta = crate::apply(
        &mut state,
        SeatMutation::SetKind {
            seat_index: 1,
            kind: SeatKind::Ai {
                difficulty: phase_ai::config::AiDifficulty::Hard,
                deck: DeckChoice::Random,
            },
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(delta.new_ai.len(), 1);
    assert!(matches!(state.seats[1], SeatKind::Ai { .. }));
}

#[test]
fn remove_waiting_seat_respects_min_players() {
    let mut state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::WaitingHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec!["host".to_string(), String::new(), String::new()],
        format: FormatConfig::commander(),
        game_started: false,
    };
    let delta = crate::apply(&mut state, SeatMutation::Remove { seat_index: 2 }, &ctx()).unwrap();
    assert_eq!(state.seats.len(), 2);
    assert!(delta.renumbering.is_some());
}

#[test]
fn remove_rejects_claimed_human_seat() {
    let mut state = SeatState {
        seats: vec![
            SeatKind::HostHuman,
            SeatKind::JoinedHuman,
            SeatKind::WaitingHuman,
        ],
        tokens: vec!["host".to_string(), "guest".to_string(), String::new()],
        format: FormatConfig::commander(),
        game_started: false,
    };
    let err = crate::apply(&mut state, SeatMutation::Remove { seat_index: 1 }, &ctx()).unwrap_err();
    assert_eq!(err, SeatError::SeatClaimed);
}

/// The `SetKind` the client sends for an AI seat holding `deck_data`, spelled
/// the way the client spells it (`expandParsedDeck`'s key order).
fn client_set_kind(deck_data: &str) -> String {
    format!(
        r#"{{"type":"SetKind","data":{{"seatIndex":1,"kind":{{"type":"Ai","data":{{"difficulty":"Medium","deck":{{"type":"DeckList","data":{deck_data}}}}}}}}}}}"#
    )
}

/// Apply the client's `SetKind` JSON to a fresh two-seat room and return the
/// seat as the reducer stores, and later echoes, it.
fn echoed_ai_seat(deck_data: &str) -> SeatKind {
    let mutation: SeatMutation = serde_json::from_str(&client_set_kind(deck_data))
        .expect("the client's SetKind JSON deserializes");
    let mut state = two_player_waiting();
    let delta = crate::apply(&mut state, mutation, &ctx()).expect("SetKind applies");
    assert_eq!(delta.new_ai.len(), 1, "the seat became an AI seat");
    assert!(
        matches!(
            &state.seats[1],
            SeatKind::Ai {
                deck: DeckChoice::DeckList(_),
                ..
            }
        ),
        "the seat holds the submitted DeckList"
    );
    state.seats[1].clone()
}

/// Engine-authored wire golden for the host's AI-deck dropdown
/// (`client/src/multiplayer/deckListData.ts`). The dropdown once keyed a
/// `DeckList` by the client's own spelling of the deck, which the engine's echo
/// (every list present, engine field order, `bracket_tier` always serialized)
/// can never equal, and the hand-written echo fixture agreed only with itself.
/// Serializing the reducer's real output here and failing on drift keeps the
/// client's fixture the engine's actual echo, including array order.
#[test]
fn deck_list_seat_echo_wire_golden_matches_the_client_fixture() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SeatEchoGolden {
        plain: SeatKind,
        companion: SeatKind,
        declared_tier: SeatKind,
        forest_island: SeatKind,
        island_forest: SeatKind,
    }

    // `expandParsedDeck`'s key order, minus the engine-only lists.
    fn client_spelling(main_deck: &str, companion: &str, tier: &str) -> String {
        format!(
            r#"{{"main_deck":{main_deck},"sideboard":[],"commander":[],"planar_deck":[],"scheme_deck":[],"sticker_sheets":[],"signature_spell":[],"companion":{companion}{tier}}}"#
        )
    }

    let golden = SeatEchoGolden {
        plain: echoed_ai_seat(&client_spelling(r#"["Island"]"#, "[]", "")),
        companion: echoed_ai_seat(&client_spelling(
            r#"["Island"]"#,
            r#"["Lurrus of the Dream-Den"]"#,
            "",
        )),
        // The HostSetup spelling: an explicit declared tier.
        declared_tier: echoed_ai_seat(&client_spelling(
            r#"["Island"]"#,
            "[]",
            r#","bracket_tier":"optimized""#,
        )),
        forest_island: echoed_ai_seat(&client_spelling(r#"["Forest","Island"]"#, "[]", "")),
        island_forest: echoed_ai_seat(&client_spelling(r#"["Island","Forest"]"#, "[]", "")),
    };

    // The engine keeps the submitted array order.
    match &golden.island_forest {
        SeatKind::Ai {
            deck: DeckChoice::DeckList(data),
            ..
        } => assert_eq!(data.main_deck, vec!["Island", "Forest"]),
        other => panic!("expected an AI seat holding a DeckList, got {other:?}"),
    }

    // Serialize the struct directly: `to_value` can re-sort keys.
    let wire = serde_json::to_string_pretty(&golden).unwrap();
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../client/src/test/fixtures/seat-deck-echo-wire.json"
    );
    if std::env::var_os("UPDATE_WIRE_GOLDEN").is_some() {
        std::fs::write(path, format!("{wire}\n")).expect("write the seat deck echo golden");
    }
    let committed = std::fs::read_to_string(path).expect("committed seat deck echo golden");
    assert_eq!(
        wire,
        committed.trim_end(),
        "the client's seat deck echo golden drifted — re-run with UPDATE_WIRE_GOLDEN=1"
    );
}

/// The two deck payload types the host hand-off feeds name the same lists.
#[test]
fn deck_data_and_player_deck_list_serialize_the_same_fields() {
    fn sorted_keys(value: serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    }
    let deck_data =
        sorted_keys(serde_json::to_value(engine::starter_decks::DeckData::default()).unwrap());
    let player_deck_list = sorted_keys(serde_json::to_value(PlayerDeckList::default()).unwrap());
    assert!(deck_data.contains(&"companion".to_string()));
    assert_eq!(deck_data, player_deck_list);
}
