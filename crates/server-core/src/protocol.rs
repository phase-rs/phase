use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::log::GameLogEntry;
use engine::types::match_config::MatchConfig;
use engine::types::player::PlayerId;
use phase_ai::config::AiDifficulty;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckData {
    pub main_deck: Vec<String>,
    pub sideboard: Vec<String>,
}

/// AI seat configuration sent by the client when creating a game with AI opponents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSeatRequest {
    pub seat_index: u8,
    pub difficulty: AiDifficulty,
    pub deck_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyGame {
    pub game_code: String,
    pub host_name: String,
    pub created_at: u64,
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    CreateGame {
        deck: DeckData,
    },
    JoinGame {
        game_code: String,
        deck: DeckData,
    },
    Action {
        action: GameAction,
    },
    Reconnect {
        game_code: String,
        player_token: String,
    },
    SubscribeLobby,
    UnsubscribeLobby,
    CreateGameWithSettings {
        deck: DeckData,
        display_name: String,
        public: bool,
        password: Option<String>,
        timer_seconds: Option<u32>,
        #[serde(default = "default_player_count")]
        player_count: u8,
        #[serde(default)]
        match_config: MatchConfig,
        #[serde(default)]
        ai_seats: Vec<AiSeatRequest>,
    },
    JoinGameWithPassword {
        game_code: String,
        deck: DeckData,
        display_name: String,
        password: Option<String>,
    },
    Concede,
    Emote {
        emote: String,
    },
    SpectatorJoin {
        game_code: String,
    },
}

fn default_player_count() -> u8 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    GameCreated {
        game_code: String,
        player_token: String,
    },
    GameStarted {
        state: GameState,
        your_player: PlayerId,
        opponent_name: Option<String>,
        #[serde(default)]
        player_names: Vec<String>,
        #[serde(default)]
        legal_actions: Vec<GameAction>,
    },
    StateUpdate {
        state: GameState,
        events: Vec<GameEvent>,
        #[serde(default)]
        legal_actions: Vec<GameAction>,
        #[serde(default)]
        eliminated_players: Vec<PlayerId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        log_entries: Vec<GameLogEntry>,
    },
    ActionRejected {
        reason: String,
    },
    OpponentDisconnected {
        grace_seconds: u32,
        #[serde(default)]
        player: Option<PlayerId>,
    },
    OpponentReconnected {
        #[serde(default)]
        player: Option<PlayerId>,
    },
    GameOver {
        winner: Option<PlayerId>,
        reason: String,
    },
    Error {
        message: String,
    },
    LobbyUpdate {
        games: Vec<LobbyGame>,
    },
    LobbyGameAdded {
        game: LobbyGame,
    },
    LobbyGameRemoved {
        game_code: String,
    },
    PlayerCount {
        count: u32,
    },
    PasswordRequired {
        game_code: String,
    },
    Conceded {
        player: PlayerId,
    },
    Emote {
        from_player: PlayerId,
        emote: String,
    },
    TimerUpdate {
        player: PlayerId,
        remaining_seconds: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_create_game_roundtrips() {
        let msg = ClientMessage::CreateGame {
            deck: DeckData {
                main_deck: vec!["Lightning Bolt".to_string(); 4],
                sideboard: Vec::new(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::CreateGame { deck } => {
                assert_eq!(deck.main_deck.len(), 4);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_join_game_roundtrips() {
        let msg = ClientMessage::JoinGame {
            game_code: "ABC123".to_string(),
            deck: DeckData {
                main_deck: vec!["Forest".to_string()],
                sideboard: Vec::new(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::JoinGame { game_code, .. } => {
                assert_eq!(game_code, "ABC123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_action_roundtrips() {
        let msg = ClientMessage::Action {
            action: GameAction::PassPriority,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Action { action } => {
                assert_eq!(action, GameAction::PassPriority);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_game_created_roundtrips() {
        let msg = ServerMessage::GameCreated {
            game_code: "XYZ789".to_string(),
            player_token: "abc123def456".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameCreated {
                game_code,
                player_token,
            } => {
                assert_eq!(game_code, "XYZ789");
                assert_eq!(player_token, "abc123def456");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_game_over_roundtrips() {
        let msg = ServerMessage::GameOver {
            winner: Some(PlayerId(1)),
            reason: "opponent conceded".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameOver { winner, reason } => {
                assert_eq!(winner, Some(PlayerId(1)));
                assert_eq!(reason, "opponent conceded");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_tagged_json_format() {
        let msg = ServerMessage::OpponentReconnected { player: None };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "OpponentReconnected");
    }

    #[test]
    fn client_message_subscribe_lobby_roundtrips() {
        let msg = ClientMessage::SubscribeLobby;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::SubscribeLobby));
    }

    #[test]
    fn client_message_unsubscribe_lobby_roundtrips() {
        let msg = ClientMessage::UnsubscribeLobby;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::UnsubscribeLobby));
    }

    #[test]
    fn client_message_create_game_with_settings_roundtrips() {
        let msg = ClientMessage::CreateGameWithSettings {
            deck: DeckData {
                main_deck: vec!["Forest".to_string()],
                sideboard: Vec::new(),
            },
            display_name: "Alice".to_string(),
            public: true,
            password: Some("secret".to_string()),
            timer_seconds: Some(60),
            player_count: 4,
            match_config: MatchConfig::default(),
            ai_seats: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::CreateGameWithSettings {
                display_name,
                public,
                password,
                timer_seconds,
                player_count,
                match_config,
                ..
            } => {
                assert_eq!(display_name, "Alice");
                assert!(public);
                assert_eq!(password, Some("secret".to_string()));
                assert_eq!(timer_seconds, Some(60));
                assert_eq!(player_count, 4);
                assert_eq!(match_config, MatchConfig::default());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_game_with_settings_missing_match_config_defaults_to_bo1() {
        let json = r#"{
          "type":"CreateGameWithSettings",
          "data":{
            "deck":{"main_deck":["Forest"],"sideboard":[]},
            "display_name":"Alice",
            "public":true,
            "password":null,
            "timer_seconds":null,
            "player_count":2
          }
        }"#;
        let parsed: ClientMessage = serde_json::from_str(json).unwrap();
        match parsed {
            ClientMessage::CreateGameWithSettings { match_config, .. } => {
                assert_eq!(match_config, MatchConfig::default());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_join_game_with_password_roundtrips() {
        let msg = ClientMessage::JoinGameWithPassword {
            game_code: "ABC123".to_string(),
            deck: DeckData {
                main_deck: vec!["Forest".to_string()],
                sideboard: Vec::new(),
            },
            display_name: "Bob".to_string(),
            password: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::JoinGameWithPassword {
                game_code,
                display_name,
                password,
                ..
            } => {
                assert_eq!(game_code, "ABC123");
                assert_eq!(display_name, "Bob");
                assert_eq!(password, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_concede_roundtrips() {
        let msg = ClientMessage::Concede;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::Concede));
    }

    #[test]
    fn client_message_emote_roundtrips() {
        let msg = ClientMessage::Emote {
            emote: "GG".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Emote { emote } => assert_eq!(emote, "GG"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_game_started_with_opponent_name_roundtrips() {
        let state = GameState::new_two_player(42);
        let msg = ServerMessage::GameStarted {
            state: state.clone(),
            your_player: PlayerId(0),
            opponent_name: Some("Opponent".to_string()),
            player_names: vec!["Me".to_string(), "Opponent".to_string()],
            legal_actions: vec![GameAction::PassPriority],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameStarted {
                your_player,
                opponent_name,
                player_names,
                legal_actions,
                ..
            } => {
                assert_eq!(your_player, PlayerId(0));
                assert_eq!(opponent_name, Some("Opponent".to_string()));
                assert_eq!(player_names.len(), 2);
                assert_eq!(legal_actions.len(), 1);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_game_started_without_opponent_name_roundtrips() {
        let state = GameState::new_two_player(42);
        let msg = ServerMessage::GameStarted {
            state,
            your_player: PlayerId(1),
            opponent_name: None,
            player_names: vec![],
            legal_actions: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameStarted {
                your_player,
                opponent_name,
                legal_actions,
                ..
            } => {
                assert_eq!(your_player, PlayerId(1));
                assert_eq!(opponent_name, None);
                assert!(legal_actions.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_lobby_update_roundtrips() {
        let msg = ServerMessage::LobbyUpdate {
            games: vec![LobbyGame {
                game_code: "ABC123".to_string(),
                host_name: "Alice".to_string(),
                created_at: 1700000000,
                has_password: false,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::LobbyUpdate { games } => {
                assert_eq!(games.len(), 1);
                assert_eq!(games[0].game_code, "ABC123");
                assert_eq!(games[0].host_name, "Alice");
                assert!(!games[0].has_password);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_lobby_game_added_roundtrips() {
        let msg = ServerMessage::LobbyGameAdded {
            game: LobbyGame {
                game_code: "XYZ789".to_string(),
                host_name: "Bob".to_string(),
                created_at: 1700000000,
                has_password: true,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::LobbyGameAdded { game } => {
                assert_eq!(game.game_code, "XYZ789");
                assert!(game.has_password);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_lobby_game_removed_roundtrips() {
        let msg = ServerMessage::LobbyGameRemoved {
            game_code: "ABC123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::LobbyGameRemoved { game_code } => {
                assert_eq!(game_code, "ABC123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_player_count_roundtrips() {
        let msg = ServerMessage::PlayerCount { count: 42 };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::PlayerCount { count } => assert_eq!(count, 42),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_password_required_roundtrips() {
        let msg = ServerMessage::PasswordRequired {
            game_code: "ABC123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::PasswordRequired { game_code } => {
                assert_eq!(game_code, "ABC123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_conceded_roundtrips() {
        let msg = ServerMessage::Conceded {
            player: PlayerId(0),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::Conceded { player } => assert_eq!(player, PlayerId(0)),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_emote_roundtrips() {
        let msg = ServerMessage::Emote {
            from_player: PlayerId(1),
            emote: "Nice!".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::Emote { from_player, emote } => {
                assert_eq!(from_player, PlayerId(1));
                assert_eq!(emote, "Nice!");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_timer_update_roundtrips() {
        let msg = ServerMessage::TimerUpdate {
            player: PlayerId(0),
            remaining_seconds: 30,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::TimerUpdate {
                player,
                remaining_seconds,
            } => {
                assert_eq!(player, PlayerId(0));
                assert_eq!(remaining_seconds, 30);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn ai_seat_request_roundtrips() {
        let req = AiSeatRequest {
            seat_index: 1,
            difficulty: AiDifficulty::Hard,
            deck_name: Some("Mono Red".to_string()),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: AiSeatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.seat_index, 1);
        assert_eq!(parsed.difficulty, AiDifficulty::Hard);
        assert_eq!(parsed.deck_name, Some("Mono Red".to_string()));
    }

    #[test]
    fn ai_seat_request_uses_camel_case_keys() {
        let req = AiSeatRequest {
            seat_index: 1,
            difficulty: AiDifficulty::Medium,
            deck_name: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("seatIndex").is_some());
        assert!(json.get("deckName").is_some());
        // Verify snake_case keys are NOT present
        assert!(json.get("seat_index").is_none());
        assert!(json.get("deck_name").is_none());
    }

    #[test]
    fn create_game_with_settings_ai_seats_roundtrips() {
        let msg = ClientMessage::CreateGameWithSettings {
            deck: DeckData {
                main_deck: vec!["Forest".to_string()],
                sideboard: Vec::new(),
            },
            display_name: "Host".to_string(),
            public: false,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: MatchConfig::default(),
            ai_seats: vec![AiSeatRequest {
                seat_index: 1,
                difficulty: AiDifficulty::VeryHard,
                deck_name: None,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::CreateGameWithSettings { ai_seats, .. } => {
                assert_eq!(ai_seats.len(), 1);
                assert_eq!(ai_seats[0].seat_index, 1);
                assert_eq!(ai_seats[0].difficulty, AiDifficulty::VeryHard);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_game_with_settings_missing_ai_seats_defaults_to_empty() {
        let json = r#"{
          "type":"CreateGameWithSettings",
          "data":{
            "deck":{"main_deck":["Forest"],"sideboard":[]},
            "display_name":"Alice",
            "public":true,
            "password":null,
            "timer_seconds":null,
            "player_count":2
          }
        }"#;
        let parsed: ClientMessage = serde_json::from_str(json).unwrap();
        match parsed {
            ClientMessage::CreateGameWithSettings { ai_seats, .. } => {
                assert!(ai_seats.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }
}
