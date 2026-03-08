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
    },
    StateUpdate {
        state: GameState,
        events: Vec<GameEvent>,
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
}
