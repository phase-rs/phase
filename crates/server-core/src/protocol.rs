use std::collections::HashMap;

use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::player::PlayerId;
use phase_ai::config::AiDifficulty;
use serde::{Deserialize, Serialize};

/// Wire-protocol version. Bump when any `ClientMessage` or `ServerMessage`
/// variant is added, removed, renamed, or has a field type changed. Adding a
/// new optional field with `#[serde(default)]` does not require a bump.
///
/// Note: renaming or removing a variant silently fails at JSON parse time
/// (clients see "Invalid message: unknown variant") rather than at the
/// handshake. When making such changes, plan a deprecation window where
/// both the old and new variants coexist, then bump and remove the old.
pub const PROTOCOL_VERSION: u32 = 4;

/// Git short-hash of the build. Emitted by `build.rs`; falls back to `"dev"`
/// when git isn't available (containers, source tarballs).
pub fn build_commit() -> &'static str {
    env!("PHASE_BUILD_COMMIT")
}

/// Advertised role of the server. `Full` runs game sessions end-to-end;
/// `LobbyOnly` acts as a matchmaking broker for P2P connections and rejects
/// game-state messages. Selected at server startup via the `--lobby-only`
/// CLI flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMode {
    Full,
    LobbyOnly,
}

pub use engine::starter_decks::DeckData;

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
    /// Display string (e.g. `"0.1.11"`). Human-readable; not a compatibility gate.
    #[serde(default)]
    pub host_version: String,
    /// Git short-hash of the host's build. The compatibility gate — clients on
    /// a different commit cannot join because GameState / rules may have diverged.
    #[serde(default)]
    pub host_build_commit: String,
    /// Number of seats currently occupied (host + joined guests, including AI
    /// if present). Updated as players join/leave.
    #[serde(default)]
    pub current_players: u32,
    /// Configured seat count for this game. For 1v1 formats this is 2; for
    /// Commander it ranges 2–4.
    #[serde(default)]
    pub max_players: u32,
    /// Game format (Standard, Commander, etc.) — lets lobby UIs filter or
    /// badge the row. Optional because older persisted entries predate the
    /// field.
    #[serde(default)]
    pub format: Option<GameFormat>,
    /// Optional per-match label distinct from the host's player name. When
    /// set, lobby UIs render this as the row's primary title ("Friday Night
    /// Commander") and the host's name as secondary metadata. `None` means
    /// "use the host's name as the room label" — the behavior before this
    /// field existed, preserved for backward compatibility.
    #[serde(default)]
    pub room_name: Option<String>,
    /// True when this room is P2P-brokered (host runs the engine). False for
    /// server-run rooms. Derived from `host_peer_id` presence at publish
    /// time. This field is populated by the server, never by clients.
    #[serde(default)]
    pub is_p2p: bool,
    /// When present, this lobby entry is a draft pod rather than a
    /// constructed-play room. Contains set code and draft kind so the
    /// lobby UI can badge the row appropriately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_metadata: Option<DraftLobbyMetadata>,
}

/// Metadata attached to a lobby entry when the room is a draft pod.
/// Lightweight — only the fields the lobby listing needs to render a
/// meaningful row. The full draft state lives in the host's WASM engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftLobbyMetadata {
    /// Three-letter set code (e.g. "MKM", "OTJ").
    pub set_code: String,
    /// Draft kind label: "Quick", "Premier", or "Traditional".
    pub draft_kind: String,
}

pub use seat_reducer::types::{DeckChoice, SeatKind, SeatMutation, SeatView};

/// Info about a single player slot in a waiting room, sent to all connected players.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSlotInfo {
    pub player_id: u8,
    pub name: String,
    pub kind: SeatKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ClientMessage {
    /// First frame the client must send after receiving `ServerHello`. Carries
    /// the client's version identity so the server can enforce compatibility
    /// before accepting any game-affecting message.
    ClientHello {
        client_version: String,
        build_commit: String,
        protocol_version: u32,
    },
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
        #[serde(default)]
        format_config: Option<FormatConfig>,
        /// Optional distinct label for this room, separate from the host's
        /// player name. Routed into `LobbyGame.room_name`.
        #[serde(default)]
        room_name: Option<String>,
        /// PeerJS peer ID of the host, set when the client registers with a
        /// lobby-only server so guests can dial the host directly over P2P.
        /// `None` in `Full` server mode (the server runs the engine and P2P
        /// is not used). `Some("")` is treated identically to `None`.
        #[serde(default)]
        host_peer_id: Option<String>,
        /// Draft-specific metadata. When present, the lobby entry is badged
        /// as a draft pod instead of a constructed-play room.
        #[serde(default)]
        draft_metadata: Option<DraftLobbyMetadata>,
    },
    JoinGameWithPassword {
        game_code: String,
        deck: DeckData,
        display_name: String,
        password: Option<String>,
    },
    /// Read-only lookup used by typed-code joins before deck selection.
    /// Returns room metadata (`JoinTargetInfo`) without consuming a seat.
    LookupJoinTarget {
        game_code: String,
        password: Option<String>,
    },
    Concede,
    Emote {
        emote: String,
    },
    SpectatorJoin {
        game_code: String,
    },
    Ping {
        timestamp: u64,
    },
    /// Sent by a P2P host to update the lobby listing's player counts as
    /// guests join or leave over P2P. The server has no visibility into P2P
    /// connections, so the host must push count updates explicitly. Rejected
    /// if the caller's socket isn't the one that registered the game.
    UpdateLobbyMetadata {
        game_code: String,
        current_players: u8,
        max_players: u8,
    },
    SeatMutate {
        mutation: SeatMutation,
    },
    /// Sent by a P2P host on a `LobbyOnly` server once their game is live
    /// (guest(s) have dialed in and the P2P session is established) so the
    /// lobby listing is removed immediately instead of waiting for the host
    /// socket to close or the 5-minute expiry to fire. The server rejects
    /// this message if the caller's socket isn't the one that registered
    /// the given `game_code`.
    UnregisterLobby {
        game_code: String,
    },
}

fn default_player_count() -> u8 {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ServerMessage {
    /// Sent unprompted immediately on WebSocket accept. The client compares
    /// `protocol_version` against its own and refuses to proceed on mismatch.
    /// `build_commit` is the git short-hash of the server binary; it is used
    /// by the lobby to gate joins when host and guest are on different builds.
    ServerHello {
        server_version: String,
        build_commit: String,
        protocol_version: u32,
        mode: ServerMode,
    },
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
        #[serde(default)]
        auto_pass_recommended: bool,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        spell_costs: HashMap<ObjectId, ManaCost>,
        /// Per-card grouping of `legal_actions` keyed by `GameAction::source_object()`.
        /// Frontends use this map for "what can I do with this card?" lookups without
        /// introspecting `GameAction` variants client-side. Empty for non-actors.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        legal_actions_by_object: HashMap<ObjectId, Vec<GameAction>>,
        /// Engine-authored presentation projections computed alongside
        /// `state`. See `engine::game::derived_views::DerivedViews`.
        /// Required for Commander-format games so the CommanderDamage HUD
        /// renders; empty in non-Commander formats (JIT short-circuit).
        #[serde(default)]
        derived: engine::game::derived_views::DerivedViews,
        /// Included for joiners so they can persist the token for reconnection.
        /// Omitted (None) for hosts (who get it via GameCreated) and reconnects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        player_token: Option<String>,
    },
    StateUpdate {
        state: GameState,
        events: Vec<GameEvent>,
        #[serde(default)]
        legal_actions: Vec<GameAction>,
        #[serde(default)]
        auto_pass_recommended: bool,
        #[serde(default)]
        eliminated_players: Vec<PlayerId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        log_entries: Vec<GameLogEntry>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        spell_costs: HashMap<ObjectId, ManaCost>,
        /// Per-card grouping of `legal_actions` keyed by `GameAction::source_object()`.
        /// Empty for non-actors.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        legal_actions_by_object: HashMap<ObjectId, Vec<GameAction>>,
        /// Engine-authored presentation projections for this state snapshot.
        /// See `engine::game::derived_views::DerivedViews`. Always populated
        /// by server construction sites — the `#[serde(default)]` exists
        /// only for wire-format forward compatibility, never as an intended
        /// silent fallback (CLAUDE.md: engine owns all logic).
        #[serde(default)]
        derived: engine::game::derived_views::DerivedViews,
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
    /// Broadcast when an existing lobby entry's mutable state changes
    /// (e.g. `current_players` ticks up as a guest joins). Lets clients
    /// refresh a single row without a full `LobbyUpdate` resync.
    LobbyGameUpdated {
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
    /// Read-only response describing how a typed-code join should be routed.
    /// Returned by `LookupJoinTarget` on both Full and LobbyOnly servers.
    JoinTargetInfo {
        game_code: String,
        is_p2p: bool,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        match_config: MatchConfig,
        player_count: u8,
        filled_seats: u8,
    },
    PlayerSlotsUpdate {
        slots: Vec<PlayerSlotInfo>,
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
    Pong {
        timestamp: u64,
    },
    /// Sent by a `LobbyOnly` server in response to `JoinGameWithPassword`.
    /// Hands the guest the host's PeerJS peer ID and room metadata so the
    /// guest can dial the host directly; the server never touches game
    /// state in this mode. `filled_seats` and `player_count` let the guest
    /// refuse to dial a full room without paying a P2P handshake.
    PeerInfo {
        game_code: String,
        host_peer_id: String,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        match_config: MatchConfig,
        player_count: u8,
        filled_seats: u8,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn load_fixture(path: &str) -> Value {
        serde_json::from_str(path).unwrap()
    }

    #[test]
    fn client_message_create_game_roundtrips() {
        let msg = ClientMessage::CreateGame {
            deck: DeckData {
                main_deck: vec!["Lightning Bolt".to_string(); 4],
                sideboard: Vec::new(),
                commander: Vec::new(),
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
                commander: Vec::new(),
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
                commander: Vec::new(),
            },
            display_name: "Alice".to_string(),
            public: true,
            password: Some("secret".to_string()),
            timer_seconds: Some(60),
            player_count: 4,
            match_config: MatchConfig::default(),
            ai_seats: vec![],
            format_config: None,
            room_name: Some("Friday Night Commander".to_string()),
            host_peer_id: None,
            draft_metadata: None,
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
                room_name,
                ..
            } => {
                assert_eq!(display_name, "Alice");
                assert!(public);
                assert_eq!(password, Some("secret".to_string()));
                assert_eq!(timer_seconds, Some(60));
                assert_eq!(player_count, 4);
                assert_eq!(match_config, MatchConfig::default());
                assert_eq!(room_name, Some("Friday Night Commander".to_string()));
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
                commander: Vec::new(),
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
    fn client_message_lookup_join_target_roundtrips() {
        let msg = ClientMessage::LookupJoinTarget {
            game_code: "ABC123".to_string(),
            password: Some("pw".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::LookupJoinTarget {
                game_code,
                password,
            } => {
                assert_eq!(game_code, "ABC123");
                assert_eq!(password, Some("pw".to_string()));
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
            auto_pass_recommended: false,
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
            derived: Default::default(),
            player_token: None,
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
            auto_pass_recommended: false,
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
            derived: Default::default(),
            player_token: None,
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
                host_version: "0.1.11".to_string(),
                host_build_commit: "abc1234".to_string(),
                current_players: 1,
                max_players: 2,
                format: None,
                room_name: None,
                is_p2p: false,
                draft_metadata: None,
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
                host_version: "0.1.11".to_string(),
                host_build_commit: "abc1234".to_string(),
                current_players: 1,
                max_players: 2,
                format: None,
                room_name: None,
                is_p2p: true,
                draft_metadata: None,
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
    fn server_message_lobby_game_updated_roundtrips() {
        let msg = ServerMessage::LobbyGameUpdated {
            game: LobbyGame {
                game_code: "ABC123".to_string(),
                host_name: "Alice".to_string(),
                created_at: 1700000000,
                has_password: false,
                host_version: "0.1.11".to_string(),
                host_build_commit: "abc1234".to_string(),
                current_players: 2,
                max_players: 4,
                format: Some(GameFormat::Commander),
                room_name: Some("Board-wipe special".to_string()),
                is_p2p: false,
                draft_metadata: None,
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::LobbyGameUpdated { game } => {
                assert_eq!(game.game_code, "ABC123");
                assert_eq!(game.current_players, 2);
                assert_eq!(game.max_players, 4);
                assert_eq!(game.format, Some(GameFormat::Commander));
                assert_eq!(game.room_name, Some("Board-wipe special".to_string()));
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
                commander: Vec::new(),
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
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
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
    fn client_message_ping_roundtrips() {
        let msg = ClientMessage::Ping {
            timestamp: 1700000000123,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Ping { timestamp } => assert_eq!(timestamp, 1700000000123),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_pong_roundtrips() {
        let msg = ServerMessage::Pong {
            timestamp: 1700000000123,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::Pong { timestamp } => assert_eq!(timestamp, 1700000000123),
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

    #[test]
    fn client_hello_roundtrips() {
        let msg = ClientMessage::ClientHello {
            client_version: "0.1.11".to_string(),
            build_commit: "abc1234".to_string(),
            protocol_version: PROTOCOL_VERSION,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::ClientHello {
                client_version,
                build_commit,
                protocol_version,
            } => {
                assert_eq!(client_version, "0.1.11");
                assert_eq!(build_commit, "abc1234");
                assert_eq!(protocol_version, PROTOCOL_VERSION);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_hello_roundtrips() {
        let msg = ServerMessage::ServerHello {
            server_version: "0.1.11".to_string(),
            build_commit: "abc1234".to_string(),
            protocol_version: PROTOCOL_VERSION,
            mode: ServerMode::LobbyOnly,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::ServerHello {
                server_version,
                build_commit,
                protocol_version,
                mode,
            } => {
                assert_eq!(server_version, "0.1.11");
                assert_eq!(build_commit, "abc1234");
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(mode, ServerMode::LobbyOnly);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn lobby_game_with_full_metadata_roundtrips() {
        let game = LobbyGame {
            game_code: "ABC123".to_string(),
            host_name: "Alice".to_string(),
            created_at: 1700000000,
            has_password: false,
            host_version: "0.2.0".to_string(),
            host_build_commit: "def5678".to_string(),
            current_players: 2,
            max_players: 4,
            format: Some(GameFormat::Commander),
            room_name: Some("Spellslingers".to_string()),
            is_p2p: true,
            draft_metadata: None,
        };
        let json = serde_json::to_string(&game).unwrap();
        let parsed: LobbyGame = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.host_version, "0.2.0");
        assert_eq!(parsed.host_build_commit, "def5678");
        assert_eq!(parsed.current_players, 2);
        assert_eq!(parsed.max_players, 4);
        assert_eq!(parsed.format, Some(GameFormat::Commander));
        assert_eq!(parsed.room_name, Some("Spellslingers".to_string()));
        assert!(parsed.is_p2p);
        assert!(parsed.draft_metadata.is_none());
    }

    #[test]
    fn lobby_game_without_optional_metadata_deserializes_with_defaults() {
        // Older clients / persisted entries may lack the new fields.
        let json = r#"{
            "game_code": "OLD123",
            "host_name": "Legacy",
            "created_at": 1700000000,
            "has_password": false
        }"#;
        let parsed: LobbyGame = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.host_version, "");
        assert_eq!(parsed.host_build_commit, "");
        assert_eq!(parsed.current_players, 0);
        assert_eq!(parsed.max_players, 0);
        assert_eq!(parsed.format, None);
        assert_eq!(parsed.room_name, None);
        // Pre-PR-2 servers never emitted is_p2p; decoding such a payload must
        // default to `false` so legacy rows are treated as server-run.
        assert!(!parsed.is_p2p);
    }

    #[test]
    fn build_commit_is_nonempty() {
        // Whether in git or not, build.rs always emits something.
        assert!(!build_commit().is_empty());
    }

    #[test]
    fn peer_info_roundtrips() {
        let msg = ServerMessage::PeerInfo {
            game_code: "ABC123".to_string(),
            host_peer_id: "peer-host-xyz".to_string(),
            format_config: None,
            match_config: MatchConfig::default(),
            player_count: 4,
            filled_seats: 2,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::PeerInfo {
                game_code,
                host_peer_id,
                player_count,
                filled_seats,
                ..
            } => {
                assert_eq!(game_code, "ABC123");
                assert_eq!(host_peer_id, "peer-host-xyz");
                assert_eq!(player_count, 4);
                assert_eq!(filled_seats, 2);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn join_target_info_roundtrips() {
        let msg = ServerMessage::JoinTargetInfo {
            game_code: "ABC123".to_string(),
            is_p2p: true,
            format_config: Some(FormatConfig::commander()),
            match_config: MatchConfig::default(),
            player_count: 4,
            filled_seats: 2,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::JoinTargetInfo {
                game_code,
                is_p2p,
                format_config,
                player_count,
                filled_seats,
                ..
            } => {
                assert_eq!(game_code, "ABC123");
                assert!(is_p2p);
                assert_eq!(format_config, Some(FormatConfig::commander()));
                assert_eq!(player_count, 4);
                assert_eq!(filled_seats, 2);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn unregister_lobby_roundtrips() {
        let msg = ClientMessage::UnregisterLobby {
            game_code: "ABC123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::UnregisterLobby { game_code } => {
                assert_eq!(game_code, "ABC123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_game_with_settings_host_peer_id_roundtrips() {
        let msg = ClientMessage::CreateGameWithSettings {
            deck: DeckData {
                main_deck: vec!["Forest".to_string()],
                sideboard: Vec::new(),
                commander: Vec::new(),
            },
            display_name: "Alice".to_string(),
            public: true,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: MatchConfig::default(),
            ai_seats: vec![],
            format_config: None,
            room_name: None,
            host_peer_id: Some("peer-host-abc".to_string()),
            draft_metadata: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::CreateGameWithSettings { host_peer_id, .. } => {
                assert_eq!(host_peer_id, Some("peer-host-abc".to_string()));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn create_game_with_settings_missing_host_peer_id_defaults_to_none() {
        // Full-mode clients never send host_peer_id; it should deserialize
        // as None so those clients keep working.
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
            ClientMessage::CreateGameWithSettings { host_peer_id, .. } => {
                assert_eq!(host_peer_id, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn game_started_fixture_matches_server_message_contract() {
        let fixture = load_fixture(include_str!(
            "../../../fixtures/adapter-contract/game_started.json"
        ));
        let parsed: ServerMessage = serde_json::from_value(fixture).unwrap();
        match parsed {
            ServerMessage::GameStarted {
                your_player,
                opponent_name,
                legal_actions,
                ..
            } => {
                assert_eq!(your_player, PlayerId(0));
                assert_eq!(opponent_name.as_deref(), Some("Opponent"));
                assert_eq!(legal_actions, vec![GameAction::PassPriority]);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn state_update_fixture_matches_server_message_contract() {
        let fixture = load_fixture(include_str!(
            "../../../fixtures/adapter-contract/state_update.json"
        ));
        let parsed: ServerMessage = serde_json::from_value(fixture).unwrap();
        match parsed {
            ServerMessage::StateUpdate {
                events,
                legal_actions,
                ..
            } => {
                assert_eq!(events.len(), 1);
                assert_eq!(legal_actions, vec![GameAction::PassPriority]);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
