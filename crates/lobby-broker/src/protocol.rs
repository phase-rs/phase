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

/// Machine-readable reasons for server error replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerErrorCode {
    DeckRejected,
}

/// Wire-protocol version shared by the native server, client, and Cloudflare
/// lobby Worker. Bump when any `ClientMessage` or `ServerMessage` variant is
/// added, removed, renamed, or has a field type changed. Adding a new optional
/// field with `#[serde(default)]` does not require a bump — **unless the client
/// stops deriving a fallback for it.** That clause is about wire
/// *parseability*, not capability: once the client renders a feature only from
/// the new field, an old server that omits it produces a silent feature loss
/// rather than a parse error, and the handshake is the only place that pairing
/// can be refused. See 24.
///
/// Note: renaming or removing a variant silently fails at JSON parse time
/// (clients see "Invalid message: unknown variant") rather than at the
/// handshake. When making such changes, plan a deprecation window where
/// both the old and new variants coexist, then bump and remove the old.
///
/// 33 — `LegendCandidateIdentity::Unknown` prevents face-down legend candidates
///      from publishing an affirmative original/copy identity.
/// 32 — `DerivedViews::legend_candidate_identities` publishes the engine-authored
///      original/copy/token-copy identity for each active legend-rule choice. The
///      field is `#[serde(default)]`, but the client deliberately no longer derives
///      this rules-sensitive identity from raw objects; an older server would
///      silently omit every choice identity.
/// 31 — `WaitingFor::LoopShortcut` publishes the engine-issued `declaration`, and
///      `InteractionResponseSpec::Shortcut` publishes `preview`, the per-axis
///      consequence of the offered count. Both are `Option` and neither type sets
///      `deny_unknown_fields`, so a v30 peer still *parses* the frame — this is a
///      capability bump like 24, not a parse bump. UNLIKE 24, no pairing is left to
///      exercise the gap, so this entry names no silent-drop hazard. Full-game floors
///      are exact-match on both sides (`server_core::MIN_SUPPORTED_PROTOCOL ==
///      PROTOCOL_VERSION`, and `MIN_SUPPORTED_SERVER_PROTOCOL` in
///      `client/src/adapter/ws-adapter.ts`), so a v31/v30 full-game pair is refused
///      at the handshake and never sends an action frame. The one-version window is
///      this file's `MIN_SUPPORTED_PROTOCOL` below, and it is lobby-only:
///      `DeclareShortcut` rides `ClientMessage::Action`, which `LobbyClientMessage`
///      has no variant for at all, and which `reject_if_disabled` in
///      `crates/phase-server/src/main.rs` answers under `ServerMode::LobbyOnly` with
///      an explicit rejection rather than a silent drop. The P2P games this broker
///      matchmakes are gated tighter still, on build-commit equality
///      (`check_build_commit`), not on a protocol window.
/// 30 — Serialized player-action completion provenance and modal continuations.
/// 29 — Added requester-correlated `ResolveAllRejected` response frames.
/// 28 — Added native `ResolveAll` request/result frames.
/// 27 — Added `DraftKind::Sealed`, serialized by draft WebSocket messages.
/// 26 — Added `ServerMessage::ActionNoOp` for accepted transport no-ops.
/// 25 — `DebugCardEntries` added a serialized, private resolution frame for
///      multi-card sandbox battlefield entries that pause for replacement or
///      as-enters choices. Old peers cannot deserialize that `GameState` shape.
/// 24 — `DerivedViews::unbounded_families` carries the engine-owned per-seat
///      family collapse state behind each `∞` badge. The field is
///      `#[serde(default)]`, so this is a capability bump rather than a parse
///      bump: the client deleted its row-flag OR-fold derivation, so a v23
///      server that omits the field would leave a new client rendering NO
///      infinity badges at all, silently and with no parse error to catch it.
/// 23 — `PayableResource::ManaGeneric` changed from `{ per_x }` to
///      `{ base_cost: ManaCost }` (#6410) — a `GameState` payload field type
///      change, and `base_cost` intentionally carries no `#[serde(default)]`
///      (a missing `base_cost` must fail deserialization, not silently
///      resolve to a zero-cost payment), so old and new peers can't parse
///      each other's serialized snapshots.
/// 20 — Actor-scoped priority-passing settings and filtered per-player state.
/// 19 — Connive's exact `EventObjectSnapshot` subject and resident paused
///      post-replacement drains changed serialized full-game state. Phase 4
///      later pinned the existing v2 resolution wire shape; it did not add a
///      second protocol change.
/// 18 — Serialized GameState trigger provenance and paused logical zone-change
///      owners are now wire-visible.
/// 16 — Meld pair/attacking-entry choices after mana-payment preview variants.
/// 15 — Mana-payment preview request/response variants.
/// 14 — `PrecastCopyShortcut` action and its two `WaitingFor` variants.
/// 13 — `WaitingFor::MulliganBottomCards` removed from the full-game state
///      payload; mulligan bottoming folded into a
///      `MulliganDecisionPhase::BottomCards` sub-phase on
///      `WaitingFor::MulliganDecision`.
pub const PROTOCOL_VERSION: u32 = 33;

/// Minimum protocol version accepted by lobby-only brokers at the hello
/// handshake **from clients that predate [`LOBBY_PROTOCOL_VERSION`]** — the
/// legacy path only. Lobby traffic has a one-version rollout window; full game
/// servers may choose a stricter floor when state/action payloads change.
///
/// Being derived from [`PROTOCOL_VERSION`] is exactly the defect
/// [`LOBBY_PROTOCOL_VERSION`] fixes: a `GameState`-only bump slides this floor
/// even though no lobby message changed. It survives only so already-deployed
/// clients stay reachable; new gating must use [`MIN_SUPPORTED_LOBBY_PROTOCOL`].
pub const MIN_SUPPORTED_PROTOCOL: u32 = PROTOCOL_VERSION.saturating_sub(1);

/// Wire-protocol version of the **lobby** message set ([`LobbyClientMessage`] /
/// [`LobbyServerMessage`]), independent of [`PROTOCOL_VERSION`].
///
/// Bump ONLY when a lobby variant is added, removed, renamed, or has a field
/// type changed. A full-game bump must NOT move this number: no lobby variant
/// carries `GameState` or `GameAction`, so full-game churn cannot break lobby
/// traffic.
///
/// Sharing one integer between the two surfaces is what took preview
/// multiplayer down: `PROTOCOL_VERSION` moved twice for `GameState`-only
/// changes, [`MIN_SUPPORTED_PROTOCOL`] is derived from it, and the deployed
/// broker's window went disjoint from the shipped client's. This constant is
/// the fix — it moves only for reasons the lobby can actually observe.
///
/// 1 — Initial lobby-owned version, covering the `LobbyClientMessage` /
///     `LobbyServerMessage` variant sets, unchanged since #1880.
pub const LOBBY_PROTOCOL_VERSION: u32 = 1;

/// Lowest [`LOBBY_PROTOCOL_VERSION`] a broker accepts from a client.
///
/// There is deliberately **no upper bound**. A client newer than the broker can
/// only fail by sending a lobby variant this broker does not know, and
/// [`parse_lobby_client_message`] already answers that with an explicit
/// [`ParsedFrame::UnknownTag`] rejection scoped to the offending frame. That is
/// a loud, per-feature failure; refusing the whole connection instead evicts a
/// client over a variant it may never send.
pub const MIN_SUPPORTED_LOBBY_PROTOCOL: u32 = 1;

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
    /// Draft kind label: "Quick", "Premier", "Traditional", or "Sealed".
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
        /// The client's [`LOBBY_PROTOCOL_VERSION`]. `None` from clients built
        /// before the lobby owned its own version; those fall back to the
        /// `protocol_version` window. Additive and optional, so an older broker
        /// ignores it and an older client omits it — no `PROTOCOL_VERSION` bump
        /// is required for either direction to keep parsing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lobby_protocol_version: Option<u32>,
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
        /// This broker's [`LOBBY_PROTOCOL_VERSION`]. Advertised alongside — not
        /// instead of — `protocol_version`, which older clients still gate on
        /// and which therefore must keep tracking the full-game constant.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lobby_protocol_version: Option<u32>,
    },
    GameCreated {
        game_code: String,
        player_token: String,
    },
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<ServerErrorCode>,
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

impl LobbyServerMessage {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            code: None,
        }
    }
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

    /// The two surfaces must not share a number. `scripts/check-protocol-version.mjs`
    /// carries the structural half of this guard — it matches
    /// `LOBBY_PROTOCOL_VERSION` only against a bare integer literal, so
    /// re-deriving it from `PROTOCOL_VERSION` fails the cross-language gate
    /// rather than silently re-coupling the lobby to full-game churn.
    #[test]
    fn lobby_protocol_version_is_independent_of_the_full_game_one() {
        assert_eq!(LOBBY_PROTOCOL_VERSION, 1);
        assert_eq!(MIN_SUPPORTED_LOBBY_PROTOCOL, 1);
        assert_ne!(
            LOBBY_PROTOCOL_VERSION, PROTOCOL_VERSION,
            "the lobby must version its own message set, not alias the full-game one"
        );
        // A `const` block, not a runtime assert: both operands are constants, so
        // this is decidable at compile time. A floor above the current version
        // would refuse every client, and that should fail the BUILD rather than
        // wait for someone to run the test suite.
        const {
            assert!(
                MIN_SUPPORTED_LOBBY_PROTOCOL <= LOBBY_PROTOCOL_VERSION,
                "a floor above the current version would refuse every client"
            )
        };
    }

    #[test]
    fn protocol_version_tracks_full_game_wire_additions() {
        assert_eq!(PROTOCOL_VERSION, 33);
        // Lobby keeps its one-version rollout window; full-game servers stay
        // current-only (`server_core::MIN_SUPPORTED_PROTOCOL == PROTOCOL_VERSION`),
        // which is what refuses an older full-game peer whose GameState cannot
        // understand a success acknowledgment the submitting client awaits.
        assert_eq!(MIN_SUPPORTED_PROTOCOL, 32);
    }

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
