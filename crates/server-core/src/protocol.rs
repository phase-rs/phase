use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckData {
    pub main_deck: Vec<String>,
    pub sideboard: Vec<String>,
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
        legal_actions: Vec<GameAction>,
    },
    StateUpdate {
        state: GameState,
        events: Vec<GameEvent>,
        #[serde(default)]
        legal_actions: Vec<GameAction>,
    },
    ActionRejected {
        reason: String,
    },
    OpponentDisconnected {
        grace_seconds: u32,
    },
    OpponentReconnected,
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
        let msg = ServerMessage::OpponentReconnected;
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
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::CreateGameWithSettings {
                display_name,
                public,
                password,
                timer_seconds,
                ..
            } => {
                assert_eq!(display_name, "Alice");
                assert!(public);
                assert_eq!(password, Some("secret".to_string()));
                assert_eq!(timer_seconds, Some(60));
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
            legal_actions: vec![GameAction::PassPriority],
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
                assert_eq!(your_player, PlayerId(0));
                assert_eq!(opponent_name, Some("Opponent".to_string()));
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
}
