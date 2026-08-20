use std::collections::HashMap;

use engine::game::interaction::ObjectActionPayload;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::interaction::InteractionSubmission;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaCost;
use engine::types::match_config::MatchConfig;
use engine::types::player::PlayerId;
use phase_ai::config::AiDifficulty;
use serde::{Deserialize, Serialize};

use crate::session::FullSessionKey;
use crate::takeback::{RewindOption, RewindTarget};

/// Full game wire protocol version. Kept numerically aligned with the lobby
/// broker while state/action messages share the same WebSocket protocol enum.
pub const PROTOCOL_VERSION: u32 = lobby_broker::PROTOCOL_VERSION;

/// Minimum protocol version accepted by full game servers. Planechase changed
/// game-state/action payload shape, so stale clients must not join full games.
pub const MIN_SUPPORTED_PROTOCOL: u32 = PROTOCOL_VERSION;

/// Minimum protocol version accepted by lobby-only brokers.
pub const LOBBY_MIN_SUPPORTED_PROTOCOL: u32 = lobby_broker::MIN_SUPPORTED_PROTOCOL;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<DeckChoice>,
}

// `LobbyGame` and `DraftLobbyMetadata` are now DEFINED in `lobby-broker`
// (the WASM-safe broker crate owns the lobby-listing wire types) and
// re-exported here so `ServerMessage::LobbyUpdate { games: Vec<LobbyGame> }`
// and the broker reference the same struct. The serde shape is unchanged —
// wire bytes are byte-identical (guarded by tests/lobby_wire_contract.rs).
pub use lobby_broker::protocol::{DraftLobbyMetadata, LobbyGame, ServerErrorCode};

pub use seat_reducer::types::{DeckChoice, SeatKind, SeatMutation, SeatTeamInfo, SeatView};

/// Info about a single player slot in a waiting room, sent to all connected players.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerSlotInfo {
    pub player_id: u8,
    pub name: String,
    pub kind: SeatKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_info: Option<SeatTeamInfo>,
    #[serde(default)]
    pub reserved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reservation_expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankedPlayerResult {
    pub player_id: u8,
    pub rating_before: i32,
    pub rating_after: i32,
    pub rating_delta: i32,
}

/// Recipient-safe presentation of an immutable Full terminal result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalMatchDisplay {
    pub winner: Option<PlayerId>,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranked_result: Option<Vec<RankedPlayerResult>>,
}

/// Opaque identifier for one recipient's terminal delivery ledger row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalDeliveryId(pub String);

/// Opaque capability for reading and acknowledging a terminal result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalCredential(pub String);

/// Immutable terminal access tuple issued only to the matching recipient.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentTerminalDelivery {
    pub key: FullSessionKey,
    pub terminal_revision: u64,
    pub delivery_id: TerminalDeliveryId,
    pub credential: TerminalCredential,
    pub display: TerminalMatchDisplay,
}

/// Bootstrap proof held by a reconnecting player before the regular Full
/// session is attached. The request id makes retrying this terminal-only
/// exchange idempotent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalBootstrapRequest {
    pub key: FullSessionKey,
    pub player_token: String,
    pub request_id: String,
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
    /// Server-authoritative fast-forward of repeated priority passes. The
    /// authenticated session supplies both the requester and AI seats.
    ResolveAll {
        request_id: u64,
        max_resolutions: u32,
    },
    /// Read-only simulation of an exact automatic spell-cast action. The
    /// authenticated session, rather than the client, determines the actor.
    PreviewManaPayment {
        request_id: u64,
        action: GameAction,
    },
    /// One opaque, engine-authored interaction response. The client echoes the
    /// submission the engine published in `ViewerInteraction`; it never derives
    /// a `GameAction` from the opportunity schema. Like `Action`, the
    /// authenticated session — not the payload — determines the acting seat.
    ///
    /// Unlike `Action`, a bounds rejection on this variant is answered on
    /// `ServerMessage::ActionRejected`, not `ServerMessage::Error`: a free-form
    /// `Text` response can exceed `MAX_INTERACTION_STRING_LEN` by an ordinary
    /// paste, and the native client tears the session down on any `Error`.
    /// See `client_message_wire_guard::wire_rejection_message`.
    Interaction {
        submission: InteractionSubmission,
    },
    Reconnect {
        game_code: String,
        player_token: String,
        /// Server-issued Full session identity. Clients retain this exact key;
        /// they must never reconstruct a generation from the game code.
        full_key: FullSessionKey,
    },
    /// Permanently removes a full-mode game. The server authorizes this from
    /// the host's authenticated session; it is used to clean up a host-local
    /// native engine session that can no longer serve its P2P transport.
    AbandonGame,
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
        /// When true, the server/host starts the game as soon as every seat is
        /// occupied. Defaulted so older clients keep the new intended behavior
        /// without requiring a protocol-version bump.
        #[serde(default = "default_true")]
        start_when_full: bool,
        /// Enable ranked rating updates for this room.
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
    /// Read-only lookup used by typed-code joins before deck selection.
    /// Returns room metadata (`JoinTargetInfo`) without consuming a seat.
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
    Concede,
    /// Authenticated request to concede the entire current best-of-three
    /// match. The requester, winner, and trusted cause are deliberately not
    /// wire fields; the server binds them from the attached session.
    ConcedeMatch,
    /// Terminal-only recovery. This deliberately cannot attach a Full game
    /// session or fall through to ordinary reconnect handling.
    BootstrapTerminalDelivery {
        request: TerminalBootstrapRequest,
    },
    /// Reads an already-issued recipient delivery using its opaque capability.
    ReadTerminalResult {
        credential: TerminalCredential,
    },
    /// Idempotently acknowledges an already-issued recipient delivery.
    AckTerminalDelivery {
        delivery_id: TerminalDeliveryId,
        credential: TerminalCredential,
    },
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
        #[serde(default)]
        consumed_reservation_tokens: Vec<String>,
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
    CreateDraftWithSettings {
        display_name: String,
        set_code: String,
        kind: draft_core::types::DraftKind,
        public: bool,
        password: Option<String>,
        timer_seconds: Option<u32>,
        tournament_format: draft_core::types::TournamentFormat,
        pod_policy: draft_core::types::PodPolicy,
        pod_size: u8,
    },
    JoinDraftWithPassword {
        draft_code: String,
        display_name: String,
        password: Option<String>,
    },
    DraftAction {
        draft_code: String,
        action: draft_core::types::DraftAction,
    },
    ReconnectDraft {
        draft_code: String,
        player_token: String,
    },
    SpectateDraft {
        draft_code: String,
    },
    /// GH #1507: ask every other human player at the table to approve
    /// rolling the game back. Auto-approves when the requester is the only
    /// human seat (e.g. solo vs. AI).
    ///
    /// A **newtype** variant carrying `Option<RewindTarget>`, not a struct
    /// variant, and that shape is load-bearing. `ClientMessage` is adjacently
    /// tagged (`tag = "type"`, `content = "data"`); serde synthesizes a
    /// missing-`data` arm only for unit and newtype variants, and only an
    /// `Option<T>` payload recovers from it. The client omits `data` entirely
    /// for a last-action undo, so a struct variant here would make this server
    /// reject its own same-version client's frame with ``missing field
    /// `data` ``. `None` normalizes to [`RewindTarget::LastAction`], which is
    /// exactly what an omitted payload means.
    RequestTakeback(Option<RewindTarget>),
    /// Approve or decline the table's pending takeback request. Any single
    /// decline withdraws the request — rollback requires unanimous approval.
    RespondTakeback {
        approve: bool,
    },
    /// Withdraw a takeback request the caller themselves made.
    CancelTakeback,
}

fn default_player_count() -> u8 {
    2
}

fn default_true() -> bool {
    true
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
        /// Public base URL clients should advertise when sharing a join code
        /// (e.g. `https://x.ngrok-free.app` from an embedded tunnel, or a
        /// `PUBLIC_URL` reverse proxy). Lets a host connected over `localhost`
        /// still surface a reachable `<code>@<host>` string. Additive and
        /// optional: older clients ignore it, older servers omit it. `None` for
        /// LobbyOnly brokers and for servers with no advertised address.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        public_url: Option<String>,
    },
    GameCreated {
        game_code: String,
        player_token: String,
        /// Present only for Full authoritative sessions. Lobby-only brokers do
        /// not create a Full runtime and therefore cannot issue this key.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_key: Option<FullSessionKey>,
    },
    /// Confirms the authenticated server seat for one connection before a
    /// pregame room has started. A host-side P2P bridge binds this identity to
    /// its already-authenticated PeerJS seat; it must never infer it from join
    /// order.
    SessionAttached {
        game_code: String,
        player_id: PlayerId,
        player_token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_key: Option<FullSessionKey>,
    },
    GameStarted {
        /// Monotonic server-authored snapshot revision. Every viewer of this
        /// authoritative state receives the same revision.
        state_revision: u64,
        state: GameState,
        your_player: PlayerId,
        opponent_name: Option<String>,
        #[serde(default)]
        player_names: Vec<String>,
        #[serde(default)]
        legal_actions: Vec<GameAction>,
        #[serde(default)]
        auto_pass_recommended: bool,
        /// Ordered CR 116.2c offers projected by the engine for direct rendering.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        end_continuous_effect_offers: Vec<GameAction>,
        /// Exact engine-authored actions for the deterministic mana-payment shortcut.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mana_payment_shortcut_actions: Vec<GameAction>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        spell_costs: HashMap<ObjectId, ManaCost>,
        /// Per-card grouping of `legal_actions` keyed by `GameAction::source_object()`.
        /// Frontends use this map for "what can I do with this card?" lookups without
        /// introspecting `GameAction` variants client-side. Empty for non-actors.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        legal_actions_by_object: HashMap<ObjectId, Vec<ObjectActionPayload>>,
        /// Engine-authored presentation projections computed alongside
        /// `state`. See `engine::game::derived_views::DerivedViews`.
        /// Required for Commander-format games so the CommanderDamage HUD
        /// renders; empty in non-Commander formats (JIT short-circuit).
        #[serde(default)]
        derived: engine::game::derived_views::DerivedViews,
        /// Viewer-scoped interactive opportunities derived from the same
        /// authoritative state as this filtered snapshot.
        viewer_interaction: engine::types::interaction::ViewerInteraction,
        /// Included for joiners so they can persist the token for reconnection.
        /// Omitted (None) for hosts (who get it via GameCreated) and reconnects.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        player_token: Option<String>,
        /// The exact Full identity associated with this state stream. It is
        /// omitted only for wire-compatible non-Full producers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        full_key: Option<FullSessionKey>,
        /// Engine events produced by `start_game` — currently the d20
        /// first-player contest (`StartingPlayerContest`) event. Populated ONLY
        /// on the initial post-start broadcast; empty for late joiners and
        /// reconnects (a reconnecting player must not re-see the contest). The
        /// contest is public (no `visibility.rs` redaction), so it goes to every
        /// seat. `serde(default)` keeps this back-compat for older clients.
        #[serde(default)]
        events: Vec<GameEvent>,
        /// Turn boundaries this session currently offers as rollback targets.
        /// Populated here as well as on `StateUpdate` so a reconnect mid-game
        /// sees the list immediately rather than waiting for the next action.
        /// Empty for every deployment except a `SingleUser` sidecar.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rewind_targets: Vec<RewindOption>,
    },
    StateUpdate {
        /// Monotonic server-authored snapshot revision. Reused for read-only
        /// snapshots and advanced only by authoritative state transitions.
        state_revision: u64,
        state: GameState,
        events: Vec<GameEvent>,
        #[serde(default)]
        legal_actions: Vec<GameAction>,
        #[serde(default)]
        auto_pass_recommended: bool,
        /// Ordered CR 116.2c offers projected by the engine for direct rendering.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        end_continuous_effect_offers: Vec<GameAction>,
        /// Exact engine-authored actions for the deterministic mana-payment shortcut.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mana_payment_shortcut_actions: Vec<GameAction>,
        #[serde(default)]
        eliminated_players: Vec<PlayerId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        log_entries: Vec<GameLogEntry>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        spell_costs: HashMap<ObjectId, ManaCost>,
        /// Per-card grouping of `legal_actions` keyed by `GameAction::source_object()`.
        /// Empty for non-actors.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        legal_actions_by_object: HashMap<ObjectId, Vec<ObjectActionPayload>>,
        /// Engine-authored presentation projections for this state snapshot.
        /// See `engine::game::derived_views::DerivedViews`. Always populated
        /// by server construction sites — the `#[serde(default)]` exists
        /// only for wire-format forward compatibility, never as an intended
        /// silent fallback (CLAUDE.md: engine owns all logic).
        #[serde(default)]
        derived: engine::game::derived_views::DerivedViews,
        /// Viewer-scoped interactive opportunities derived from the same
        /// authoritative state as this filtered snapshot.
        viewer_interaction: engine::types::interaction::ViewerInteraction,
        /// Turn boundaries this session currently offers as rollback targets,
        /// published alongside the state they describe rather than out of band.
        /// Empty for every deployment except a `SingleUser` sidecar.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rewind_targets: Vec<RewindOption>,
    },
    ActionRejected {
        reason: String,
    },
    /// Confirms an authenticated action that intentionally produced no state
    /// transition. The submitting adapter resolves its pending request without
    /// caching or publishing a replacement snapshot.
    ActionNoOp,
    /// Requester-only rejection for a native Resolve All batch. The request
    /// identifier prevents unrelated action failures from settling this promise.
    ResolveAllRejected {
        request_id: u64,
        reason: String,
    },
    /// Requester-only acknowledgement for a native Resolve All batch. The
    /// matching StateUpdate is sent first and carries the authoritative state.
    ResolveAllResult {
        request_id: u64,
        items_resolved: u32,
        total: u32,
    },
    /// Acknowledges a host-authorized permanent game cleanup.
    GameAbandoned {
        game_code: String,
    },
    /// Mana sources the engine's automatic payment path would use for one
    /// `PreviewManaPayment` request. Sent only to the requesting player.
    ManaPaymentPreview {
        request_id: u64,
        source_ids: Vec<ObjectId>,
    },
    ManaPaymentPreviewRejected {
        request_id: u64,
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
        /// Present for ranked games where a two-player result produced
        /// rating changes for both seats.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ranked_result: Option<Vec<RankedPlayerResult>>,
    },
    /// Terminal-only bootstrap response. `None` means the exact keyed Full
    /// session has no prepared terminal artifact, so the caller may attempt
    /// its ordinary reconnect path on a separate socket.
    TerminalBootstrapResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<CurrentTerminalDelivery>,
    },
    /// Terminal-only capability read response.
    TerminalResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery: Option<CurrentTerminalDelivery>,
    },
    /// Terminal-only acknowledgement receipt.
    TerminalDeliveryAcknowledged {
        delivery_id: TerminalDeliveryId,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_expires_at_ms: Option<u64>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_token: Option<String>,
    },
    DraftCreated {
        draft_code: String,
        player_token: String,
        seat_index: u8,
    },
    DraftJoined {
        draft_code: String,
        player_token: String,
        seat_index: u8,
        view: draft_core::view::DraftPlayerView,
    },
    DraftStateUpdate {
        view: draft_core::view::DraftPlayerView,
    },
    DraftMatchStart {
        match_id: String,
        round: u8,
        game_code: String,
        player_token: String,
        your_player: PlayerId,
        opponent_name: String,
    },
    DraftTimerSync {
        remaining_ms: u32,
    },
    DraftActionRejected {
        reason: String,
    },
    DraftOver {
        standings: Vec<draft_core::view::StandingEntry>,
    },
    DraftSpectatorView {
        view: draft_core::view::SpectatorDraftView,
    },
    /// GH #1507: a human player has requested a takeback. Sent to every
    /// connected seat (including the requester) so the UI can prompt the
    /// other human players for approval.
    TakebackRequested {
        requester: PlayerId,
        requester_name: String,
    },
    /// The pending takeback request has been resolved, either by unanimous
    /// approval, a decline, or the requester cancelling it. When
    /// `approved` is true, a `StateUpdate` carrying the rolled-back state
    /// is sent to every seat immediately before this message.
    TakebackResolved {
        approved: bool,
        /// The player whose response concluded the request: the decliner,
        /// or the requester on self-cancel. `None` when every human seat
        /// approved without a final distinguished responder (e.g. the
        /// requester was the sole human seat).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolved_by: Option<PlayerId>,
    },
}

impl ServerMessage {
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            code: None,
        }
    }

    pub fn deck_rejected(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            code: Some(ServerErrorCode::DeckRejected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef};
    use engine::types::format::GameFormat;
    use engine::types::game_state::ProductionOverride;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::mana::{
        ManaSourcePenalty, ManaSourceSelection, ManaType, TapsForManaSelection,
    };
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
                ..Default::default()
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
                ..Default::default()
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
        let action = GameAction::TapLandForMana {
            selection: ManaSourceSelection {
                source: ObjectIncarnationRef::of(ObjectId(7), 3),
                ability_index: None,
                mana_type: ManaType::Green,
                output: engine::types::mana::ManaSourceOutput::Concrete(ManaType::Green),
                atomic_combination: None,
                restrictions: Vec::new(),
                penalty: ManaSourcePenalty::None,
                taps_for_mana: vec![TapsForManaSelection {
                    source: ObjectIncarnationRef::of(ObjectId(9), 2),
                    occurrence: TriggerDefinitionOccurrenceRef::Printed {
                        base_set: TriggerBaseSetInstanceRef::INITIAL,
                        printed_index: 0,
                    },
                    production_override: ProductionOverride::SingleColor(ManaType::Red),
                }],
            },
        };
        let msg = ClientMessage::Action {
            action: action.clone(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Action {
                action: restored_action,
            } => {
                assert_eq!(restored_action, action);
            }
            _ => panic!("wrong variant"),
        }

        let GameAction::TapLandForMana { selection } = action else {
            unreachable!("fixture action is a land-mana selection");
        };
        let generic = GameAction::ActivateManaSource { selection };
        let json = serde_json::to_string(&ClientMessage::Action {
            action: generic.clone(),
        })
        .unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::Action {
                action: restored_action,
            } => assert_eq!(restored_action, generic),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_mana_payment_preview_roundtrips() {
        let msg = ClientMessage::PreviewManaPayment {
            request_id: 7,
            action: GameAction::PassPriority,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::PreviewManaPayment { request_id, action } => {
                assert_eq!(request_id, 7);
                assert_eq!(action, GameAction::PassPriority);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_mana_payment_preview_roundtrips() {
        let msg = ServerMessage::ManaPaymentPreview {
            request_id: 7,
            source_ids: vec![ObjectId(12)],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::ManaPaymentPreview {
                request_id,
                source_ids,
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(source_ids, vec![ObjectId(12)]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn meld_actions_roundtrip() {
        use engine::game::combat::AttackTarget;
        use engine::types::identifiers::ObjectId;

        for action in [
            GameAction::ChooseMeldPair {
                source_id: ObjectId(10),
                partner_id: ObjectId(11),
            },
            GameAction::ChooseEntryAttackTarget {
                target: AttackTarget::Battle(ObjectId(12)),
            },
        ] {
            let json = serde_json::to_string(&ClientMessage::Action {
                action: action.clone(),
            })
            .unwrap();
            let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
            assert!(
                matches!(parsed, ClientMessage::Action { action: parsed_action } if parsed_action == action)
            );
        }
    }

    #[test]
    fn server_message_game_created_roundtrips() {
        let msg = ServerMessage::GameCreated {
            game_code: "XYZ789".to_string(),
            player_token: "abc123def456".to_string(),
            full_key: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameCreated {
                game_code,
                player_token,
                full_key,
            } => {
                assert_eq!(game_code, "XYZ789");
                assert_eq!(player_token, "abc123def456");
                assert!(full_key.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_game_over_roundtrips() {
        let msg = ServerMessage::GameOver {
            winner: Some(PlayerId(1)),
            reason: "opponent conceded".to_string(),
            ranked_result: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameOver {
                winner,
                reason,
                ranked_result,
            } => {
                assert_eq!(winner, Some(PlayerId(1)));
                assert_eq!(reason, "opponent conceded");
                assert!(ranked_result.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn terminal_bootstrap_request_roundtrips_with_exact_full_key() {
        let msg = ClientMessage::BootstrapTerminalDelivery {
            request: TerminalBootstrapRequest {
                key: FullSessionKey {
                    game_code: "TERM01".to_string(),
                    generation: 4,
                },
                player_token: "pre-terminal-token".to_string(),
                request_id: "retry-1".to_string(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::BootstrapTerminalDelivery { request } => {
                assert_eq!(request.key.game_code, "TERM01");
                assert_eq!(request.key.generation, 4);
                assert_eq!(request.request_id, "retry-1");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn terminal_delivery_response_roundtrips() {
        let msg = ServerMessage::TerminalBootstrapResult {
            delivery: Some(CurrentTerminalDelivery {
                key: FullSessionKey {
                    game_code: "TERM01".to_string(),
                    generation: 4,
                },
                terminal_revision: 9,
                delivery_id: TerminalDeliveryId("delivery-0".to_string()),
                credential: TerminalCredential("credential".to_string()),
                display: TerminalMatchDisplay {
                    winner: Some(PlayerId(1)),
                    reason: "Match conceded".to_string(),
                    ranked_result: None,
                },
            }),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::TerminalBootstrapResult {
                delivery: Some(delivery),
            } => {
                assert_eq!(delivery.key.generation, 4);
                assert_eq!(delivery.terminal_revision, 9);
                assert_eq!(delivery.delivery_id.0, "delivery-0");
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
                ..Default::default()
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
            start_when_full: true,
            ranked: false,
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
                ..Default::default()
            },
            display_name: "Bob".to_string(),
            password: None,
            reservation_token: None,
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
            reserve: false,
            display_name: None,
            release_reservation_token: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::LookupJoinTarget {
                game_code,
                password,
                reserve,
                display_name,
                release_reservation_token,
            } => {
                assert_eq!(game_code, "ABC123");
                assert_eq!(password, Some("pw".to_string()));
                assert!(!reserve);
                assert_eq!(display_name, None);
                assert_eq!(release_reservation_token, None);
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
    fn client_message_concede_match_roundtrips_without_authority_payload() {
        let json = serde_json::to_string(&ClientMessage::ConcedeMatch).unwrap();
        assert_eq!(json, r#"{"type":"ConcedeMatch"}"#);
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::ConcedeMatch));
    }

    #[test]
    fn client_message_abandon_game_roundtrips() {
        let json = serde_json::to_string(&ClientMessage::AbandonGame).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::AbandonGame));
    }

    #[test]
    fn session_attached_roundtrips_with_only_its_own_token() {
        let msg = ServerMessage::SessionAttached {
            game_code: "ABC123".to_string(),
            player_id: PlayerId(1),
            player_token: "token".to_string(),
            full_key: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::SessionAttached {
                game_code,
                player_id,
                player_token,
                full_key,
            } => {
                assert_eq!(game_code, "ABC123");
                assert_eq!(player_id, PlayerId(1));
                assert_eq!(player_token, "token");
                assert!(full_key.is_none());
            }
            other => panic!("wrong variant: {other:?}"),
        }
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

    mod emote_guard_tests {
        use crate::emote_guard::{guard_emote, MAX_EMOTE_LEN};

        #[test]
        fn emote_accepts_valid_text() {
            assert!(guard_emote("GG").is_ok());
        }

        #[test]
        fn emote_rejects_oversized_text() {
            let err = guard_emote(&"a".repeat(MAX_EMOTE_LEN + 1)).unwrap_err();
            assert!(err.contains("emote"));
        }
    }

    #[test]
    fn server_message_game_started_with_opponent_name_roundtrips() {
        let state = GameState::new_two_player(42);
        let action = GameAction::PassPriority;
        let end_offer = GameAction::EndContinuousEffect {
            group: engine::types::game_state::EndEffectGroupId(8),
            source_name: "Calming Licid".to_string(),
            cost: ManaCost::Cost {
                shards: vec![engine::types::mana::ManaCostShard::White],
                generic: 0,
            },
        };
        let interaction_action_id = engine::game::interaction::interaction_action_id(&action);
        let viewer_interaction =
            engine::game::interaction::derive_viewer_interaction(&state, &state, PlayerId(0));
        let msg = ServerMessage::GameStarted {
            state_revision: 0,
            state: state.clone(),
            your_player: PlayerId(0),
            opponent_name: Some("Opponent".to_string()),
            player_names: vec!["Me".to_string(), "Opponent".to_string()],
            legal_actions: vec![action.clone()],
            auto_pass_recommended: false,
            end_continuous_effect_offers: vec![end_offer.clone()],
            mana_payment_shortcut_actions: vec![],
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::from([(
                engine::types::identifiers::ObjectId(7),
                vec![engine::game::interaction::ObjectActionPayload {
                    action,
                    interaction_action_id: interaction_action_id.clone(),
                }],
            )]),
            derived: Default::default(),
            viewer_interaction: viewer_interaction.clone(),
            player_token: None,
            full_key: None,
            events: vec![],
            rewind_targets: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::GameStarted {
                your_player,
                opponent_name,
                player_names,
                legal_actions,
                end_continuous_effect_offers,
                legal_actions_by_object,
                viewer_interaction: decoded_viewer_interaction,
                ..
            } => {
                assert_eq!(your_player, PlayerId(0));
                assert_eq!(opponent_name, Some("Opponent".to_string()));
                assert_eq!(player_names.len(), 2);
                assert_eq!(legal_actions.len(), 1);
                assert_eq!(end_continuous_effect_offers, vec![end_offer]);
                assert_eq!(decoded_viewer_interaction, viewer_interaction);
                assert_eq!(
                    legal_actions_by_object[&engine::types::identifiers::ObjectId(7)][0]
                        .interaction_action_id,
                    interaction_action_id
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_game_started_without_opponent_name_roundtrips() {
        let state = GameState::new_two_player(42);
        let msg = ServerMessage::GameStarted {
            state_revision: 0,
            state: state.clone(),
            your_player: PlayerId(1),
            opponent_name: None,
            player_names: vec![],
            legal_actions: vec![],
            auto_pass_recommended: false,
            end_continuous_effect_offers: vec![],
            mana_payment_shortcut_actions: vec![],
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
            derived: Default::default(),
            viewer_interaction: engine::game::interaction::derive_viewer_interaction(
                &state,
                &state,
                PlayerId(1),
            ),
            player_token: None,
            full_key: None,
            events: vec![],
            rewind_targets: vec![],
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
                is_sandbox: false,
                is_ranked: false,
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
                is_sandbox: false,
                is_ranked: false,
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
                is_sandbox: false,
                is_ranked: false,
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
            deck: None,
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
            deck: None,
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
                ..Default::default()
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
                deck: None,
            }],
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            start_when_full: true,
            ranked: false,
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
    fn seat_mutation_deck_list_choice_roundtrips() {
        let msg = ClientMessage::SeatMutate {
            mutation: SeatMutation::SetKind {
                seat_index: 1,
                kind: SeatKind::Ai {
                    difficulty: AiDifficulty::Medium,
                    deck: DeckChoice::DeckList(Box::new(DeckData {
                        main_deck: vec!["Forest".to_string(); 60],
                        ..Default::default()
                    })),
                },
            },
        };

        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::SeatMutate {
                mutation:
                    SeatMutation::SetKind {
                        kind:
                            SeatKind::Ai {
                                deck: DeckChoice::DeckList(deck),
                                ..
                            },
                        ..
                    },
            } => {
                assert_eq!(deck.main_deck.len(), 60);
                assert_eq!(deck.main_deck[0], "Forest");
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
            mode: ServerMode::Full,
            public_url: Some("https://x.ngrok-free.app".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::ServerHello {
                server_version,
                build_commit,
                protocol_version,
                mode,
                public_url,
            } => {
                assert_eq!(server_version, "0.1.11");
                assert_eq!(build_commit, "abc1234");
                assert_eq!(protocol_version, PROTOCOL_VERSION);
                assert_eq!(mode, ServerMode::Full);
                assert_eq!(public_url.as_deref(), Some("https://x.ngrok-free.app"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_hello_omits_public_url_when_none() {
        // `skip_serializing_if` keeps the wire identical to a server with no
        // advertised URL — and identical to the lobby-broker ServerHello, which
        // has no such field (asserted by the lobby wire-contract test).
        let msg = ServerMessage::ServerHello {
            server_version: "0.1.11".to_string(),
            build_commit: "abc1234".to_string(),
            protocol_version: PROTOCOL_VERSION,
            mode: ServerMode::LobbyOnly,
            public_url: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("public_url"), "None must be omitted: {json}");
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
            is_sandbox: false,
            is_ranked: false,
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
            reservation_token: None,
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
            reservation_token: None,
            reservation_expires_at_ms: None,
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
                ..Default::default()
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
            start_when_full: true,
            ranked: false,
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

    #[test]
    fn client_message_create_sealed_draft_with_settings_roundtrips() {
        let msg = ClientMessage::CreateDraftWithSettings {
            display_name: "Alice".to_string(),
            set_code: "MKM".to_string(),
            kind: draft_core::types::DraftKind::Sealed,
            public: true,
            password: Some("secret".to_string()),
            timer_seconds: Some(75),
            tournament_format: draft_core::types::TournamentFormat::Swiss,
            pod_policy: draft_core::types::PodPolicy::Competitive,
            pod_size: 8,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::CreateDraftWithSettings {
                display_name,
                set_code,
                kind,
                public,
                password,
                timer_seconds,
                pod_size,
                ..
            } => {
                assert_eq!(display_name, "Alice");
                assert_eq!(set_code, "MKM");
                assert_eq!(kind, draft_core::types::DraftKind::Sealed);
                assert!(public);
                assert_eq!(password, Some("secret".to_string()));
                assert_eq!(timer_seconds, Some(75));
                assert_eq!(pod_size, 8);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_join_draft_with_password_roundtrips() {
        let msg = ClientMessage::JoinDraftWithPassword {
            draft_code: "ABCD12".to_string(),
            display_name: "Bob".to_string(),
            password: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::JoinDraftWithPassword {
                draft_code,
                display_name,
                password,
            } => {
                assert_eq!(draft_code, "ABCD12");
                assert_eq!(display_name, "Bob");
                assert_eq!(password, None);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_draft_action_roundtrips() {
        let msg = ClientMessage::DraftAction {
            draft_code: "ABCD12".to_string(),
            action: draft_core::types::DraftAction::Pick {
                seat: 3,
                card_instance_id: "card-001".to_string(),
            },
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::DraftAction { draft_code, action } => {
                assert_eq!(draft_code, "ABCD12");
                assert_eq!(
                    action,
                    draft_core::types::DraftAction::Pick {
                        seat: 3,
                        card_instance_id: "card-001".to_string(),
                    }
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_reconnect_draft_roundtrips() {
        let msg = ClientMessage::ReconnectDraft {
            draft_code: "ABCD12".to_string(),
            player_token: "tok123".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::ReconnectDraft {
                draft_code,
                player_token,
            } => {
                assert_eq!(draft_code, "ABCD12");
                assert_eq!(player_token, "tok123");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_created_roundtrips() {
        let msg = ServerMessage::DraftCreated {
            draft_code: "ABCD12".to_string(),
            player_token: "tok123".to_string(),
            seat_index: 0,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftCreated {
                draft_code,
                player_token,
                seat_index,
            } => {
                assert_eq!(draft_code, "ABCD12");
                assert_eq!(player_token, "tok123");
                assert_eq!(seat_index, 0);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_state_update_roundtrips() {
        use draft_core::types::*;
        use draft_core::view::{DraftPlayerView, DraftPoolGroups};

        let first_pull = DraftCardInstance {
            instance_id: "pack-1-card-1".to_string(),
            name: "First Pull".to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: vec!["W".to_string()],
            cmc: 1,
            type_line: "Creature — Test".to_string(),
            draft_effect: None,
        };
        let second_pull = DraftCardInstance {
            instance_id: "pack-2-card-1".to_string(),
            name: "Second Pull".to_string(),
            set_code: "TST".to_string(),
            collector_number: "2".to_string(),
            rarity: "uncommon".to_string(),
            colors: vec!["U".to_string()],
            cmc: 2,
            type_line: "Instant".to_string(),
            draft_effect: None,
        };
        let pool = vec![first_pull.clone(), second_pull.clone()];
        let pool_groups = DraftPoolGroups::from_pool(&pool);
        let view = DraftPlayerView {
            status: DraftStatus::Deckbuilding,
            kind: DraftKind::Sealed,
            current_pack_number: 0,
            pick_number: 2,
            pass_direction: PassDirection::Left,
            current_pack: None,
            pool,
            draft_effects: vec![first_pull.clone()],
            pool_groups,
            sealed_packs: Some(vec![vec![first_pull], vec![second_pull]]),
            seats: Vec::new(),
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: Vec::new(),
            timer_remaining_ms: Some(5000),
            standings: Vec::new(),
            current_round: 0,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            pairings: Vec::new(),
            match_config: DraftKind::Sealed.match_config(),
        };
        let msg = ServerMessage::DraftStateUpdate { view: view.clone() };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftStateUpdate { view: v } => {
                assert_eq!(v.status, DraftStatus::Deckbuilding);
                assert_eq!(v.pick_number, 2);
                assert_eq!(v.timer_remaining_ms, Some(5000));
                assert_eq!(v.pool_groups, view.pool_groups);
                assert_eq!(v.draft_effects, view.draft_effects);
                assert_eq!(
                    v.sealed_packs
                        .as_ref()
                        .expect("sealed packs survive JSON transport")
                        .iter()
                        .map(|pack| {
                            pack.iter()
                                .map(|card| card.instance_id.as_str())
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>(),
                    vec![vec!["pack-1-card-1"], vec!["pack-2-card-1"]]
                );
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_match_start_roundtrips() {
        let msg = ServerMessage::DraftMatchStart {
            match_id: "r1-t0".to_string(),
            round: 1,
            game_code: "GAME01".to_string(),
            player_token: "tok456".to_string(),
            your_player: PlayerId(0),
            opponent_name: "Bob".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftMatchStart {
                match_id,
                round,
                game_code,
                player_token,
                your_player,
                opponent_name,
            } => {
                assert_eq!(match_id, "r1-t0");
                assert_eq!(round, 1);
                assert_eq!(game_code, "GAME01");
                assert_eq!(player_token, "tok456");
                assert_eq!(your_player, PlayerId(0));
                assert_eq!(opponent_name, "Bob");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_timer_sync_roundtrips() {
        let msg = ServerMessage::DraftTimerSync {
            remaining_ms: 12345,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftTimerSync { remaining_ms } => {
                assert_eq!(remaining_ms, 12345);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_action_rejected_roundtrips() {
        let msg = ServerMessage::DraftActionRejected {
            reason: "Not your turn".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftActionRejected { reason } => {
                assert_eq!(reason, "Not your turn");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_over_roundtrips() {
        use draft_core::view::StandingEntry;

        let msg = ServerMessage::DraftOver {
            standings: vec![StandingEntry {
                seat_index: 0,
                display_name: "Alice".to_string(),
                match_wins: 3,
                match_losses: 0,
                game_wins: 6,
                game_losses: 1,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftOver { standings } => {
                assert_eq!(standings.len(), 1);
                assert_eq!(standings[0].display_name, "Alice");
                assert_eq!(standings[0].match_wins, 3);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_spectate_draft_roundtrips() {
        let msg = ClientMessage::SpectateDraft {
            draft_code: "ABCD12".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::SpectateDraft { draft_code } => {
                assert_eq!(draft_code, "ABCD12");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_draft_spectator_view_roundtrips() {
        use draft_core::types::*;
        use draft_core::view::SpectatorDraftView;

        let view = SpectatorDraftView {
            status: DraftStatus::Drafting,
            kind: DraftKind::Premier,
            current_pack_number: 1,
            pick_number: 5,
            pass_direction: PassDirection::Right,
            seats: Vec::new(),
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: Vec::new(),
            standings: Vec::new(),
            current_round: 0,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            pairings: Vec::new(),
            match_config: DraftKind::Premier.match_config(),
            pools: None,
            current_packs: None,
        };
        let msg = ServerMessage::DraftSpectatorView { view };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::DraftSpectatorView { view: v } => {
                assert_eq!(v.status, DraftStatus::Drafting);
                assert_eq!(v.pick_number, 5);
                assert!(v.pools.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn protocol_version_is_32() {
        assert_eq!(PROTOCOL_VERSION, 32);
    }

    /// The bump alone is inert — a version number nobody enforces prevents no
    /// pairing. This is the assertion with teeth: full-game servers accept ONLY
    /// the current protocol, so an older peer cannot complete a handshake with
    /// a server that may answer an accepted action on a variant it does not
    /// understand.
    ///
    /// REVERT-PROBE: relax to `PROTOCOL_VERSION - 1` — the exact regression
    /// this guards — and this test reds while `protocol_version_is_32` stays
    /// green, which is why the two are separate assertions.
    #[test]
    fn full_game_floor_is_current_only_not_a_rollout_window() {
        assert_eq!(
            MIN_SUPPORTED_PROTOCOL, PROTOCOL_VERSION,
            "full-game servers must refuse every stale client; a rollout window here \
             re-admits the v23 pairing that drops the engine-owned family channel"
        );
        // The lobby floor is deliberately looser, and must NOT be tightened to
        // match: lobby traffic carries matchmaking metadata only.
        assert_eq!(LOBBY_MIN_SUPPORTED_PROTOCOL, PROTOCOL_VERSION - 1);
    }

    #[test]
    fn client_message_request_takeback_roundtrips() {
        let msg = ClientMessage::RequestTakeback(None);
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::RequestTakeback(None)));
    }

    #[test]
    fn resolve_all_wire_frames_carry_only_server_safe_metadata() {
        let request = ClientMessage::ResolveAll {
            request_id: 7,
            max_resolutions: 5_000,
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"type":"ResolveAll","data":{"request_id":7,"max_resolutions":5000}}"#
        );

        let result = ServerMessage::ResolveAllResult {
            request_id: 7,
            items_resolved: 3,
            total: 52,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert_eq!(
            json,
            r#"{"type":"ResolveAllResult","data":{"request_id":7,"items_resolved":3,"total":52}}"#
        );
        assert!(!json.contains("waiting_for"));

        let rejected = ServerMessage::ResolveAllRejected {
            request_id: 7,
            reason: "Resolve All requires your priority".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&rejected).unwrap(),
            r#"{"type":"ResolveAllRejected","data":{"request_id":7,"reason":"Resolve All requires your priority"}}"#
        );
    }

    /// R15. The last-action frame the client actually sends carries **no**
    /// `data` key at all, which is byte-identical to the frame every deployed
    /// client already sends. `ClientMessage` is adjacently tagged, and serde
    /// synthesizes a missing-content arm only for unit and newtype variants —
    /// and only an `Option<T>` payload recovers from it. A struct variant here
    /// would reject this frame with ``missing field `data` ``, so this
    /// assertion is what fails if the variant shape regresses.
    #[test]
    fn request_takeback_without_data_decodes_as_last_action() {
        let parsed: ClientMessage =
            serde_json::from_str(r#"{"type":"RequestTakeback"}"#).expect("absent data must decode");
        assert!(matches!(parsed, ClientMessage::RequestTakeback(None)));

        let null_data: ClientMessage =
            serde_json::from_str(r#"{"type":"RequestTakeback","data":null}"#)
                .expect("explicit null data must decode");
        assert!(matches!(null_data, ClientMessage::RequestTakeback(None)));

        // `None` is the wire spelling of "this client predates turn rewind",
        // and the transport normalizes it with `unwrap_or_default()`. Pin the
        // default so that normalization cannot silently change meaning.
        assert_eq!(RewindTarget::default(), RewindTarget::LastAction);
    }

    /// R15. The turn-rewind frame is the only `data`-bearing shape, and its
    /// exact bytes are the contract `ws-adapter.ts` is written against.
    #[test]
    fn request_takeback_turn_start_roundtrips_with_exact_wire_bytes() {
        let msg = ClientMessage::RequestTakeback(Some(RewindTarget::TurnStart { turn_number: 7 }));
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(
            json,
            r#"{"type":"RequestTakeback","data":{"kind":"turn_start","turn_number":7}}"#
        );
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            ClientMessage::RequestTakeback(Some(RewindTarget::TurnStart { turn_number: 7 }))
        ));

        let last_action = ClientMessage::RequestTakeback(Some(RewindTarget::LastAction));
        let json = serde_json::to_string(&last_action).unwrap();
        assert_eq!(
            json,
            r#"{"type":"RequestTakeback","data":{"kind":"last_action"}}"#
        );
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            ClientMessage::RequestTakeback(Some(RewindTarget::LastAction))
        ));
    }

    /// R15. `rewind_targets` must survive a `StateUpdate` round-trip, must be
    /// omitted from the wire entirely when empty (same shape as
    /// `server_hello_omits_public_url_when_none`), and must decode to an empty
    /// vec when a producer omits it.
    #[test]
    fn state_update_rewind_targets_roundtrip_and_omission() {
        let state = GameState::new_two_player(42);
        let viewer_interaction =
            engine::game::interaction::derive_viewer_interaction(&state, &state, PlayerId(0));
        let build = |rewind_targets: Vec<RewindOption>| ServerMessage::StateUpdate {
            state_revision: 4,
            state: state.clone(),
            events: vec![],
            legal_actions: vec![],
            auto_pass_recommended: false,
            end_continuous_effect_offers: vec![],
            mana_payment_shortcut_actions: vec![],
            eliminated_players: vec![],
            log_entries: vec![],
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
            derived: Default::default(),
            viewer_interaction: viewer_interaction.clone(),
            rewind_targets,
        };

        let populated = build(vec![RewindOption {
            turn_number: 3,
            active_player: PlayerId(1),
        }]);
        let json = serde_json::to_string(&populated).unwrap();
        assert!(json.contains(r#""rewind_targets":[{"turn_number":3,"active_player":1}]"#));
        match serde_json::from_str::<ServerMessage>(&json).unwrap() {
            ServerMessage::StateUpdate { rewind_targets, .. } => {
                assert_eq!(
                    rewind_targets,
                    vec![RewindOption {
                        turn_number: 3,
                        active_player: PlayerId(1),
                    }]
                );
            }
            _ => panic!("wrong variant"),
        }

        let empty = serde_json::to_string(&build(vec![])).unwrap();
        assert!(
            !empty.contains("rewind_targets"),
            "an empty list must not appear on the wire at all"
        );
        match serde_json::from_str::<ServerMessage>(&empty).unwrap() {
            ServerMessage::StateUpdate { rewind_targets, .. } => {
                assert!(rewind_targets.is_empty(), "absent field decodes to empty");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_respond_takeback_roundtrips() {
        let msg = ClientMessage::RespondTakeback { approve: false };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientMessage::RespondTakeback { approve } => assert!(!approve),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn client_message_cancel_takeback_roundtrips() {
        let msg = ClientMessage::CancelTakeback;
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(parsed, ClientMessage::CancelTakeback));
    }

    #[test]
    fn server_message_action_no_op_roundtrips() {
        let json = serde_json::to_string(&ServerMessage::ActionNoOp).unwrap();
        assert_eq!(json, r#"{"type":"ActionNoOp"}"#);
        assert!(matches!(
            serde_json::from_str::<ServerMessage>(&json).unwrap(),
            ServerMessage::ActionNoOp
        ));
    }

    #[test]
    fn server_message_takeback_requested_roundtrips() {
        let msg = ServerMessage::TakebackRequested {
            requester: PlayerId(1),
            requester_name: "Alice".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::TakebackRequested {
                requester,
                requester_name,
            } => {
                assert_eq!(requester, PlayerId(1));
                assert_eq!(requester_name, "Alice");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_takeback_resolved_roundtrips() {
        let msg = ServerMessage::TakebackResolved {
            approved: true,
            resolved_by: Some(PlayerId(0)),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerMessage = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerMessage::TakebackResolved {
                approved,
                resolved_by,
            } => {
                assert!(approved);
                assert_eq!(resolved_by, Some(PlayerId(0)));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn server_message_takeback_resolved_omits_resolved_by_when_none() {
        let msg = ServerMessage::TakebackResolved {
            approved: false,
            resolved_by: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            !json.contains("resolved_by"),
            "None must be omitted: {json}"
        );
    }
}
