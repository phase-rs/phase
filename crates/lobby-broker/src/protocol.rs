//! Wire protocol for the lobby/matchmaking broker.
//!
//! This is the **lobby subset** of `server_core::protocol`, wire-compatible by
//! construction: every type uses the same `#[serde(tag = "type", content =
//! "data")]` shape and identical field names/`#[serde(default)]` attributes as
//! the canonical `ClientMessage`/`ServerMessage`, so the bytes on the wire are
//! byte-identical regardless of which enum (de)serializes a given frame.
//!
//! `LobbyGame` and `DraftLobbyMetadata` are **defined here** (the broker owns
//! the lobby-listing wire types) and re-exported by `server_core::protocol`, so
//! `server_core::ServerMessage::LobbyUpdate { games: Vec<LobbyGame> }` and the
//! broker both reference the same struct.
//!
//! Incoming frames are deserialized via a **two-stage parse** (`Envelope` →
//! tag match → variant): `#[serde(other)]` is invalid on adjacently-tagged
//! enums, so an unrecognized `type` is routed to the reject path explicitly
//! rather than collapsing into a magic catch-all variant (plan decision A2).

use engine::starter_decks::DeckData;
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::match_config::MatchConfig;
use serde::{Deserialize, Serialize};

/// Wire-protocol version shared by the native server, client, and Cloudflare
/// lobby Worker. Bump when any `ClientMessage` or `ServerMessage` variant is
/// added, removed, renamed, or has a field type changed. Adding a new optional
/// field with `#[serde(default)]` does not require a bump.
///
/// Note: renaming or removing a variant silently fails at JSON parse time
/// (clients see "Invalid message: unknown variant") rather than at the
/// handshake. When making such changes, plan a deprecation window where
/// both the old and new variants coexist, then bump and remove the old.
pub const PROTOCOL_VERSION: u32 = 11;

/// Minimum protocol version accepted by lobby-only brokers at the hello
/// handshake. Lobby traffic has a one-version rollout window; full game servers
/// may choose a stricter floor when state/action payloads change.
pub const MIN_SUPPORTED_PROTOCOL: u32 = PROTOCOL_VERSION.saturating_sub(1);

/// Public-lobby view of a single registered game. Populated by the server,
/// never by clients. Field shape mirrors the pre-extraction
/// `server_core::protocol::LobbyGame` exactly for wire compatibility.
/// `PartialEq` is additive over the pre-extraction type — needed so the broker's
/// `Outbound`/`LobbyServerMessage` can derive it for order-sequence assertions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// set, lobby UIs render this as the row's primary title and the host's
    /// name as secondary metadata. `None` means "use the host's name".
    #[serde(default)]
    pub room_name: Option<String>,
    /// True when this room is P2P-brokered (host runs the engine). False for
    /// server-run rooms. Derived from `host_peer_id` presence at publish time.
    #[serde(default)]
    pub is_p2p: bool,
    /// True when the host enabled Sandbox mode. Populated from
    /// `format_config.allow_debug_actions`.
    #[serde(default)]
    pub is_sandbox: bool,
    /// True when the room is configured as ranked.
    #[serde(default)]
    pub is_ranked: bool,
    /// When present, this lobby entry is a draft pod rather than a
    /// constructed-play room.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_metadata: Option<DraftLobbyMetadata>,
}

/// Metadata attached to a lobby entry when the room is a draft pod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftLobbyMetadata {
    /// Three-letter set code (e.g. "MKM", "OTJ"). For cube drafts, set to
    /// `"custom-cube"`; see [`DraftLobbyMetadata::cube_name`] for the
    /// human-readable cube name.
    pub set_code: String,
    /// Draft kind label: "Quick", "Premier", or "Traditional".
    pub draft_kind: String,
    /// Human-readable cube name when the pod is a cube draft. Absent for
    /// set drafts. Backward-compatible: `#[serde(default)]` accepts
    /// existing serialized records without the field; `skip_serializing_if`
    /// keeps the wire output byte-identical for set drafts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cube_name: Option<String>,
}

/// The lobby subset of `server_core::protocol::ClientMessage`. Wire-compatible:
/// the `type`/`data` tags and field shapes match the canonical enum exactly.
///
/// Deserialize incoming frames with [`parse_lobby_client_message`], NOT a bare
/// `serde_json::from_str` — that routes unknown tags to the reject path.
/// (No `PartialEq`: `DeckData` is not `PartialEq`, matching the canonical
/// `ClientMessage`, which is also not `PartialEq`.)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum LobbyClientMessage {
    ClientHello {
        client_version: String,
        build_commit: String,
        protocol_version: u32,
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
        format_config: Option<FormatConfig>,
        #[serde(default)]
        room_name: Option<String>,
        #[serde(default)]
        host_peer_id: Option<String>,
        #[serde(default)]
        draft_metadata: Option<DraftLobbyMetadata>,
        #[serde(default = "default_true")]
        start_when_full: bool,
        #[serde(default)]
        ranked: bool,
    },
    JoinGameWithPassword {
        game_code: String,
        deck: DeckData,
        display_name: String,
        password: Option<String>,
        #[serde(default)]
        reservation_token: Option<String>,
    },
    LookupJoinTarget {
        game_code: String,
        password: Option<String>,
        #[serde(default)]
        reserve: bool,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        release_reservation_token: Option<String>,
    },
    Ping {
        timestamp: u64,
    },
    UpdateLobbyMetadata {
        game_code: String,
        current_players: u8,
        max_players: u8,
        #[serde(default)]
        consumed_reservation_tokens: Vec<String>,
    },
    UnregisterLobby {
        game_code: String,
    },
}

fn default_player_count() -> u8 {
    2
}

fn default_true() -> bool {
    true
}

/// The lobby subset of `server_core::protocol::ServerMessage`. Includes the
/// point-reply variants (`ServerHello`, `GameCreated`, `PeerInfo`,
/// `JoinTargetInfo`, `Error`, `Pong`, `PasswordRequired`) AND the fan-out
/// variants (`LobbyUpdate`, `LobbyGame{Added,Updated,Removed}`, `PlayerCount`).
/// Wire-compatible with the canonical enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum LobbyServerMessage {
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
    Error {
        message: String,
    },
    LobbyUpdate {
        games: Vec<LobbyGame>,
    },
    LobbyGameAdded {
        game: LobbyGame,
    },
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
    JoinTargetInfo {
        game_code: String,
        is_p2p: bool,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        match_config: MatchConfig,
        player_count: u8,
        filled_seats: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_expires_at_ms: Option<u64>,
    },
    Pong {
        timestamp: u64,
    },
    PeerInfo {
        game_code: String,
        host_peer_id: String,
        #[serde(default)]
        format_config: Option<FormatConfig>,
        #[serde(default)]
        match_config: MatchConfig,
        player_count: u8,
        filled_seats: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_token: Option<String>,
    },
}

/// Advertised role of the server. Mirrors `server_core::protocol::ServerMode`
/// exactly (same variants, same serde shape) so `ServerHello` is wire-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMode {
    Full,
    LobbyOnly,
}

/// Two-stage parse envelope: pull the `type` tag and keep `data` as raw JSON,
/// so an unrecognized tag can be rejected explicitly rather than failing the
/// whole parse (or, worse, collapsing into a magic variant via the
/// `#[serde(other)]` mechanism that is invalid on adjacently-tagged enums).
#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type")]
    tag: String,
    #[serde(borrow)]
    data: Option<&'a serde_json::value::RawValue>,
}

/// Outcome of parsing an incoming lobby frame.
#[derive(Debug)]
pub enum ParsedFrame {
    /// A recognized lobby message. Boxed because `LobbyClientMessage` is far
    /// larger than the string variants (clippy `large_enum_variant`).
    Message(Box<LobbyClientMessage>),
    /// The frame was malformed JSON, a recognized tag whose `data` failed to
    /// deserialize, or a well-formed frame whose field values exceeded the
    /// bounds in [`crate::validation`]. Carries a human-readable reason for the
    /// `Error` reply.
    Malformed(String),
    /// The frame's `type` is not a known lobby tag. The shell routes this to
    /// the same reject path as a mode-disabled message.
    UnknownTag(String),
}

/// The set of `type` tags this broker recognizes. Kept as a function (not a
/// const slice match) so it stays trivially in sync with the enum variants —
/// every arm of [`deserialize_variant`] has a matching entry here.
fn is_known_lobby_tag(tag: &str) -> bool {
    matches!(
        tag,
        "ClientHello"
            | "SubscribeLobby"
            | "UnsubscribeLobby"
            | "CreateGameWithSettings"
            | "JoinGameWithPassword"
            | "LookupJoinTarget"
            | "Ping"
            | "UpdateLobbyMetadata"
            | "UnregisterLobby"
    )
}

/// Parse an incoming WebSocket text frame into a [`ParsedFrame`]. Unknown tags
/// route to [`ParsedFrame::UnknownTag`] (reject), malformed JSON or bad payload
/// to [`ParsedFrame::Malformed`].
pub fn parse_lobby_client_message(text: &str) -> ParsedFrame {
    let envelope: Envelope = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(e) => return ParsedFrame::Malformed(e.to_string()),
    };

    if !is_known_lobby_tag(&envelope.tag) {
        return ParsedFrame::UnknownTag(envelope.tag);
    }

    // Re-serialize the {type, data} pair and let the adjacently-tagged enum's
    // own deserializer handle it. This keeps a single source of truth for the
    // field-level deserialization (defaults, renames) rather than duplicating
    // every variant's field parsing here.
    let data_json = envelope.data.map(|d| d.get()).unwrap_or("null");
    let reconstructed = format!(
        r#"{{"type":{},"data":{}}}"#,
        json_string(&envelope.tag),
        data_json
    );
    match serde_json::from_str::<LobbyClientMessage>(&reconstructed) {
        Ok(msg) => match crate::validation::validate_lobby_message(&msg) {
            Ok(()) => ParsedFrame::Message(Box::new(msg)),
            Err(reason) => ParsedFrame::Malformed(reason),
        },
        Err(e) => ParsedFrame::Malformed(e.to_string()),
    }
}

/// Serialize a string as a JSON string literal (quotes + escaping).
fn json_string(s: &str) -> String {
    serde_json::to_string(s).expect("string always serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tags_parse_to_messages() {
        let frame = r#"{"type":"Ping","data":{"timestamp":42}}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::Message(msg) => match *msg {
                LobbyClientMessage::Ping { timestamp } => assert_eq!(timestamp, 42),
                other => panic!("expected Ping, got {other:?}"),
            },
            other => panic!("expected Ping, got {other:?}"),
        }
    }

    #[test]
    fn unit_variant_with_no_data_parses() {
        let frame = r#"{"type":"SubscribeLobby"}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::Message(msg) => {
                assert!(matches!(*msg, LobbyClientMessage::SubscribeLobby))
            }
            other => panic!("expected SubscribeLobby, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tag_routes_to_reject() {
        // `Action` is a real canonical tag but NOT a lobby tag — must reject,
        // not parse into a magic variant.
        let frame = r#"{"type":"Action","data":{"action":"PassPriority"}}"#;
        match parse_lobby_client_message(frame) {
            ParsedFrame::UnknownTag(tag) => assert_eq!(tag, "Action"),
            other => panic!("expected UnknownTag, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_routes_to_malformed() {
        assert!(matches!(
            parse_lobby_client_message("not json"),
            ParsedFrame::Malformed(_)
        ));
    }

    #[test]
    fn known_tag_with_bad_payload_routes_to_malformed() {
        let frame = r#"{"type":"Ping","data":{"timestamp":"not a number"}}"#;
        assert!(matches!(
            parse_lobby_client_message(frame),
            ParsedFrame::Malformed(_)
        ));
    }

    #[test]
    fn well_formed_frame_with_out_of_bounds_field_routes_to_malformed() {
        // Valid JSON and a known tag, but the display name exceeds the bound,
        // so validation rejects it at the parse boundary.
        let long_name = "a".repeat(21);
        let frame = format!(
            r#"{{"type":"CreateGameWithSettings","data":{{"deck":{{"main_deck":[]}},"display_name":"{long_name}","public":true,"password":null,"timer_seconds":null}}}}"#
        );
        assert!(matches!(
            parse_lobby_client_message(&frame),
            ParsedFrame::Malformed(_)
        ));
    }
}
