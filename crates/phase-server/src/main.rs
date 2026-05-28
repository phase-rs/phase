mod admin;
mod draft_pools;
mod logging;
mod persistence;

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use engine::ai_support::{
    auto_pass_recommended as engine_auto_pass, legal_actions_full as engine_legal_actions_full,
};
use engine::database::CardDatabase;
use engine::game::derived_views::derive_views;
use engine::game::validate_name_deck_for_format;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use http::HeaderValue;
use lobby_broker::{check_build_commit, Broker, BrokerEnv, BuildCommitCheck, ConnState, Outbound};
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx};
use server_core::draft_session::DraftSessionManager;
use server_core::lobby::RegisterGameRequest;
use server_core::protocol::{
    build_commit, ClientMessage, ServerMessage, ServerMode, MIN_SUPPORTED_PROTOCOL,
    PROTOCOL_VERSION,
};
use server_core::resolve_deck;
use server_core::session::{ActionResult, GameSession, SessionManager};
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, info_span, warn, Instrument};

type SharedState = Arc<Mutex<SessionManager>>;
type SharedConnections =
    Arc<Mutex<HashMap<String, HashMap<PlayerId, mpsc::UnboundedSender<ServerMessage>>>>>;
type SharedDb = Arc<CardDatabase>;
/// The lobby registry, wrapped in the WASM-safe [`Broker`]. LobbyOnly broker
/// dispatch goes through `Broker::handle`/`on_disconnect`/`reap_expired`;
/// Full-mode and draft lobby-listing operations call through
/// `broker.lobby_mut()` (still the same `LobbyManager`, just owned by the
/// broker).
type SharedLobby = Arc<Mutex<Broker>>;
type SharedLobbySubscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<ServerMessage>>>>;
type SharedPlayerCount = Arc<AtomicU32>;
type SharedGameDb = Arc<persistence::GameDb>;
type SharedDraftState = Arc<Mutex<DraftSessionManager>>;
type SharedDraftPools = Arc<draft_pools::DraftPools>;
/// Spectator senders keyed by draft_code. Each spectator has a visibility + sender.
type SharedDraftSpectators = Arc<
    Mutex<
        HashMap<
            String,
            Vec<(
                draft_core::types::SpectatorVisibility,
                mpsc::UnboundedSender<ServerMessage>,
            )>,
        >,
    >,
>;

fn build_game_started_message(
    session: &GameSession,
    player: PlayerId,
    player_token: Option<String>,
) -> ServerMessage {
    let (legal_actions, spell_costs_all, by_object_all) = engine_legal_actions_full(&session.state);
    let auto_pass = engine_auto_pass(&session.state, &legal_actions);
    let is_actor = server_core::is_acting(&session.state, player);
    let filtered = server_core::filter_state_for_player(&session.state, player);
    let opponent_name = engine::game::players::opponents(&session.state, player)
        .first()
        .and_then(|opp| {
            let name = &session.display_names[opp.0 as usize];
            if name.is_empty() {
                None
            } else {
                Some(name.clone())
            }
        });
    let derived = derive_views(&filtered);

    ServerMessage::GameStarted {
        state: filtered,
        your_player: player,
        opponent_name,
        player_names: session.display_names.clone(),
        legal_actions: if is_actor { legal_actions } else { Vec::new() },
        auto_pass_recommended: if is_actor { auto_pass } else { false },
        spell_costs: if is_actor {
            spell_costs_all
        } else {
            HashMap::new()
        },
        legal_actions_by_object: if is_actor {
            by_object_all
        } else {
            HashMap::new()
        },
        derived,
        player_token,
    }
}

fn build_game_started_messages(session: &GameSession) -> Vec<(PlayerId, ServerMessage)> {
    (0..session.player_count)
        .map(PlayerId)
        .filter(|player| !session.ai_seats.contains(player))
        .map(|player| (player, build_game_started_message(session, player, None)))
        .collect()
}

fn build_state_update_message(result: &ActionResult, player: PlayerId) -> ServerMessage {
    let (
        raw_state,
        events,
        legal_actions,
        log_entries,
        auto_pass,
        spell_costs,
        legal_actions_by_object,
    ) = result;
    let is_actor = raw_state.waiting_for.acting_players().contains(&player);
    let filtered = server_core::filter_state_for_player(raw_state, player);
    let derived = derive_views(&filtered);

    ServerMessage::StateUpdate {
        state: filtered,
        events: events.clone(),
        legal_actions: if is_actor {
            legal_actions.clone()
        } else {
            Vec::new()
        },
        auto_pass_recommended: if is_actor { *auto_pass } else { false },
        eliminated_players: Vec::new(),
        log_entries: log_entries.clone(),
        spell_costs: if is_actor {
            spell_costs.clone()
        } else {
            HashMap::new()
        },
        legal_actions_by_object: if is_actor {
            legal_actions_by_object.clone()
        } else {
            HashMap::new()
        },
        derived,
    }
}

/// Server's advertised role, selected at startup via `--lobby-only`. Copied
/// into every handler so the dispatch path can gate disabled messages in
/// lobby-only mode without re-parsing CLI state.
type Mode = ServerMode;

/// Server-wide limits to prevent resource exhaustion and abuse.
const MAX_CONNECTIONS: u32 = 200;
const MAX_GAMES: usize = 100;
// The lobby-only broker capacity cap (`MAX_LOBBY_ENTRIES`) now lives in
// `lobby_broker::broker` — the broker enforces it inside `handle`.
const RATE_LIMIT_MESSAGES: u32 = 30;
const RATE_LIMIT_WINDOW_SECS: u64 = 1;
const MAX_WS_MESSAGE_BYTES: usize = 8 * 1024; // 8 KB

/// Native [`BrokerEnv`] implementation: wall clock via `SystemTime`, tokens /
/// codes via the `server_core` generators (which stay in `server-core` — they
/// are the native randomness source and must not move into the WASM leaf).
struct SysEnv;

impl BrokerEnv for SysEnv {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    fn new_token(&self) -> String {
        server_core::generate_player_token()
    }
    fn new_game_code(&self) -> String {
        server_core::generate_game_code()
    }
}

/// Simple per-socket token bucket rate limiter.
struct RateLimiter {
    count: u32,
    window_start: Instant,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            count: 0,
            window_start: Instant::now(),
        }
    }

    /// Returns `true` if the message is allowed, `false` if rate-limited.
    fn check(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start).as_secs() >= RATE_LIMIT_WINDOW_SECS {
            self.count = 0;
            self.window_start = now;
        }
        self.count += 1;
        self.count <= RATE_LIMIT_MESSAGES
    }
}

/// phase-server: multiplayer game server for phase.rs
#[derive(Parser)]
#[command(
    name = "phase-server",
    version,
    about = "Multiplayer game server for phase.rs"
)]
struct Cli {
    /// Port to listen on
    #[arg(short, long, default_value = "9374", env = "PORT")]
    port: u16,

    /// Path to card data directory (must contain card-data.json)
    #[arg(short, long, default_value = "data", env = "PHASE_DATA_DIR")]
    data_dir: String,

    /// Allowed CORS origin (use '*' for permissive, or a specific URL)
    #[arg(long, env = "PHASE_CORS_ORIGIN")]
    cors_origin: Option<String>,

    /// Emit logs as JSON (for production log aggregation)
    #[arg(long, env = "PHASE_LOG_JSON")]
    log_json: bool,

    /// Directory for log files. When set, logs to files instead of stdout.
    /// Main log: <dir>/phase-server.log, per-game logs: <dir>/games/<code>.log
    #[arg(long, env = "PHASE_LOG_DIR")]
    log_dir: Option<String>,

    /// Run as a lobby-only matchmaking broker for P2P games. In this mode
    /// the server rejects game-state messages (CreateGame, Action, Reconnect,
    /// Concede, Emote, SpectatorJoin) and only brokers PeerJS peer IDs via
    /// CreateGameWithSettings / JoinGameWithPassword / SubscribeLobby. The
    /// engine and game state never run server-side, eliminating engine/build
    /// drift between host and server.
    #[arg(long, env = "PHASE_LOBBY_ONLY")]
    lobby_only: bool,
}

/// Per-socket state tracking which game/player this connection belongs to.
struct SocketIdentity {
    game_code: Option<String>,
    player_id: Option<PlayerId>,
    player_token: Option<String>,
    lobby_subscribed: bool,
    /// Span for field inheritance — all events within this connection inherit game + player fields.
    session_span: Option<tracing::Span>,
    /// Set after a successful `ClientHello`. Until this is `Some`, only
    /// `ClientMessage::ClientHello` is accepted. Carries the client's build
    /// identity so downstream handlers (`CreateGameWithSettings`,
    /// `JoinGameWithPassword`) can stamp / compare against host builds.
    client_hello: Option<ClientHelloInfo>,
    /// Set in lobby-only mode when this socket registered a lobby entry as
    /// host. On disconnect (or explicit `UnregisterLobby`) the server drops
    /// the matching lobby entry so abandoned rooms don't linger until the
    /// 5-minute expiry. Empty in `Full` mode (handled via `game_code` +
    /// `SessionManager` cleanup).
    lobby_host_game: Option<String>,
    seat_reservations: Vec<(String, String)>,
    lobby_reservations: Vec<(String, String)>,
    /// Set when this socket is participating in a draft session.
    draft_code: Option<String>,
    draft_seat: Option<usize>,
    draft_token: Option<String>,
    /// Set when this socket is spectating a draft (T-60-09: action handler
    /// checks draft_seat.is_some() before processing, rejecting spectators).
    spectator_draft_code: Option<String>,
    spectator_visibility: Option<draft_core::types::SpectatorVisibility>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientHelloInfo {
    client_version: String,
    build_commit: String,
}

/// Outcome of evaluating the handshake gate against an incoming message.
/// Extracted into a pure function so the gate's invariants can be unit-tested
/// without spinning up a real WebSocket.
#[derive(Debug, PartialEq, Eq)]
enum HelloGateOutcome {
    /// First ClientHello on this socket, compatible protocol — store the info
    /// and continue the message loop (no further processing for this frame).
    Accept(ClientHelloInfo),
    /// ClientHello arrived but declares an incompatible protocol version.
    /// Send Error with this (client, server) pair and drop the frame.
    RejectProtocol { client: u32, server: u32 },
    /// A non-hello frame arrived before the handshake completed. Send Error
    /// ("ClientHello required before any other message") and drop.
    RejectHandshakeRequired,
    /// Handshake already completed and another ClientHello arrived. Ignore
    /// silently — this is a harmless misbehavior, not an error.
    IgnoreRedundantHello,
    /// Handshake already completed and a regular frame arrived — let the
    /// downstream match in `handle_client_message` handle it.
    PassThrough,
}

fn classify_hello_gate(
    hello_received: bool,
    msg: &ClientMessage,
    server_protocol_range: std::ops::RangeInclusive<u32>,
) -> HelloGateOutcome {
    match (hello_received, msg) {
        (
            false,
            ClientMessage::ClientHello {
                client_version,
                build_commit,
                protocol_version,
            },
        ) => {
            // Accept any client in the supported range. The `server` field on
            // RejectProtocol surfaces the *current* protocol version so the
            // error message tells the client what to upgrade (or downgrade) to.
            if !server_protocol_range.contains(protocol_version) {
                HelloGateOutcome::RejectProtocol {
                    client: *protocol_version,
                    server: *server_protocol_range.end(),
                }
            } else {
                HelloGateOutcome::Accept(ClientHelloInfo {
                    client_version: client_version.clone(),
                    build_commit: build_commit.clone(),
                })
            }
        }
        (false, _) => HelloGateOutcome::RejectHandshakeRequired,
        (true, ClientMessage::ClientHello { .. }) => HelloGateOutcome::IgnoreRedundantHello,
        (true, _) => HelloGateOutcome::PassThrough,
    }
}

/// Returns `Some(error_message)` when `msg` is disabled under the current
/// server `mode`. Called at the top of dispatch so each handler below can
/// assume the message reached it legitimately.
///
/// **Exhaustive by design.** Every `ClientMessage` variant is explicitly
/// listed so adding a new variant is a compile error until the author
/// decides its mode policy. A catch-all `_ => None` would default-allow
/// future variants in both modes, which is the wrong default for a
/// security-relevant gate.
fn reject_if_disabled(msg: &ClientMessage, mode: ServerMode) -> Option<&'static str> {
    const LOBBY_ONLY_REJECTION: &str =
        "Server is in lobby-only mode — this message is not supported";
    const FULL_MODE_REJECTION: &str = "UnregisterLobby is only valid on lobby-only servers";

    match msg {
        // Always allowed — handshake, lobby subscription, ping.
        ClientMessage::ClientHello { .. }
        | ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::Ping { .. } => None,

        // Game-state messages — disabled in lobby-only mode because the
        // server doesn't run a session in that mode.
        ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Action { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::SeatMutate { .. }
        | ClientMessage::Concede
        | ClientMessage::Emote { .. }
        | ClientMessage::SpectatorJoin { .. } => match mode {
            ServerMode::Full => None,
            ServerMode::LobbyOnly => Some(LOBBY_ONLY_REJECTION),
        },

        // Broker messages — re-purposed in lobby-only mode, still valid in
        // Full mode (the Full-mode handler path uses them for hosting/joining
        // normal server-run games).
        ClientMessage::CreateGameWithSettings { .. }
        | ClientMessage::JoinGameWithPassword { .. }
        | ClientMessage::LookupJoinTarget { .. } => None,

        // Draft messages — Full-only (draft sessions are server-hosted).
        ClientMessage::CreateDraftWithSettings { .. }
        | ClientMessage::JoinDraftWithPassword { .. }
        | ClientMessage::DraftAction { .. }
        | ClientMessage::ReconnectDraft { .. }
        | ClientMessage::SpectateDraft { .. } => match mode {
            ServerMode::Full => None,
            ServerMode::LobbyOnly => Some(LOBBY_ONLY_REJECTION),
        },

        // Lobby-only-exclusive.
        ClientMessage::UpdateLobbyMetadata { .. } | ClientMessage::UnregisterLobby { .. } => {
            match mode {
                ServerMode::Full => Some(FULL_MODE_REJECTION),
                ServerMode::LobbyOnly => None,
            }
        }
    }
}

/// Returns `Some(reason)` if `action` cannot legitimately come from a client
/// over the WebSocket draft protocol, or `None` if it is a valid client action.
///
/// **Exhaustive by design.** Every `DraftAction` variant is explicitly listed
/// so adding a new variant is a compile error until the author decides its
/// client-trust policy. A catch-all `_ => None` would default-allow future
/// variants, which is the wrong default for a security-relevant gate.
///
/// Rejected variants:
/// - `GeneratePairings`: server-hosted draft match play is not yet implemented;
///   pairings will be computed server-internal once that path lands.
/// - `SetSeatConnected`: engine state plumbing. The server-internal runtime in
///   `server-core/src/draft_session.rs` broadcasts connection state via
///   `draft_core::session::apply` directly. Accepting it from a client would
///   let a malicious authenticated player forge another seat's connection
///   state (GH #1254). Caller-binding at `draft_session.rs:247-249` resolves
///   the authenticated seat from the token but discards it (`let _seat = ...`),
///   so the payload's `seat: u8` is otherwise unchecked.
fn client_forbidden_draft_action_reason(action: &draft_core::types::DraftAction) -> Option<String> {
    use draft_core::types::DraftAction;
    match action {
        DraftAction::GeneratePairings { .. } => {
            Some("Server-hosted draft match play is not available yet".to_string())
        }
        DraftAction::SetSeatConnected { .. } => {
            Some("SetSeatConnected is server-internal; not allowed from client".to_string())
        }
        DraftAction::StartDraft
        | DraftAction::Pick { .. }
        | DraftAction::SubmitDeck { .. }
        | DraftAction::ReportMatchResult { .. }
        | DraftAction::AdvanceRound
        | DraftAction::ReplaceSeatWithBot { .. } => None,
    }
}

impl SocketIdentity {
    /// Set identity and create a tracing span for field inheritance.
    fn set_session(&mut self, game_code: String, player_id: PlayerId, player_token: String) {
        self.session_span = Some(tracing::info_span!(
            "game_session",
            game = %game_code,
            player = ?player_id,
        ));
        self.game_code = Some(game_code);
        self.player_id = Some(player_id);
        self.player_token = Some(player_token);
    }

    /// Project the shell's per-socket identity into the broker's [`ConnState`]
    /// view immediately before a broker call. `SocketIdentity` remains the
    /// single per-socket store; the broker mutates a transient view that the
    /// shell syncs back with [`SocketIdentity::absorb_conn_state`].
    fn to_conn_state(&self) -> ConnState {
        ConnState {
            client_hello: self
                .client_hello
                .as_ref()
                .map(|h| lobby_broker::ClientHelloInfo {
                    client_version: h.client_version.clone(),
                    build_commit: h.build_commit.clone(),
                }),
            subscribed: self.lobby_subscribed,
            host_game: self.lobby_host_game.clone(),
            reservations: self.lobby_reservations.clone(),
        }
    }

    /// Write the broker's [`ConnState`] mutations back into the shell identity
    /// after a broker call. `client_hello` is shell-owned (set by the handshake
    /// gate, never by the broker in the native shell) so it is not copied back.
    fn absorb_conn_state(&mut self, conn: ConnState) {
        self.lobby_subscribed = conn.subscribed;
        self.lobby_host_game = conn.host_game;
        self.lobby_reservations = conn.reservations;
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let _log_guard = logging::init_logging(cli.log_dir.as_deref(), cli.log_json);
    let mode: Mode = if cli.lobby_only {
        ServerMode::LobbyOnly
    } else {
        ServerMode::Full
    };
    info!(?mode, "server mode selected");
    let data_path = Path::new(&cli.data_dir);
    let export_path = data_path.join("card-data.json");
    let card_db = if export_path.exists() {
        CardDatabase::from_export(&export_path).expect("Failed to load card-data.json")
    } else {
        CardDatabase::from_mtgjson(&data_path.join("mtgjson/test_fixture.json"))
            .expect("Failed to load card database")
    };
    info!(cards = card_db.card_count(), "card database loaded");
    let db: SharedDb = Arc::new(card_db);

    // Initialize SQLite persistence
    let game_db_path = data_path.join("games.db");
    let game_db: SharedGameDb =
        Arc::new(persistence::GameDb::open(&game_db_path).expect("Failed to open game database"));
    // Clean up stale sessions (>24 hours old)
    if let Ok(deleted) = game_db.delete_stale(86400) {
        if deleted > 0 {
            info!(count = deleted, "cleaned up stale persisted sessions");
        }
    }

    let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
    let draft_sessions: SharedDraftState = Arc::new(Mutex::new(DraftSessionManager::new()));
    let draft_pools_path = data_path.join("draft-pools.json");
    let draft_pools: SharedDraftPools = match draft_pools::DraftPools::from_path(&draft_pools_path)
    {
        Ok(pools) => {
            info!(sets = pools.len(), "draft pools loaded");
            Arc::new(pools)
        }
        Err(e) => {
            warn!(
                path = %draft_pools_path.display(),
                error = %e,
                "draft pools unavailable; server-hosted drafts cannot start"
            );
            Arc::new(draft_pools::DraftPools::default())
        }
    };
    let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
    let draft_spectators: SharedDraftSpectators = Arc::new(Mutex::new(HashMap::new()));
    let lobby: SharedLobby = Arc::new(Mutex::new(Broker::new()));
    let lobby_subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
    let player_count: SharedPlayerCount = Arc::new(AtomicU32::new(0));

    // Restore persisted game sessions from disk. In lobby-only mode the
    // server runs no engine, so persisted GameState snapshots can't be
    // replayed — skip the restore pass entirely and let SQLite ignore the
    // stale rows until operators clean them up manually.
    if matches!(mode, ServerMode::Full) {
        match game_db.load_all() {
            Ok(persisted_games) => {
                let mut mgr = state.lock().await;
                let mut lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby_mut();
                let mut restored = 0u32;

                for (game_code, json) in &persisted_games {
                    match serde_json::from_str::<server_core::PersistedSession>(json) {
                        Ok(ps) => {
                            let lobby_meta = ps.lobby_meta.clone();
                            let is_started = ps.game_started;
                            let session =
                                server_core::session::GameSession::from_persisted(ps, db.as_ref());

                            // Register all non-AI human players as disconnected
                            // to start the 120s grace period from now
                            let default_grace = mgr.reconnect.grace_period;
                            for (i, token) in session.player_tokens.iter().enumerate() {
                                let pid = PlayerId(i as u8);
                                if !token.is_empty() && !session.ai_seats.contains(&pid) {
                                    mgr.reconnect.record_disconnect(
                                        &session.game_code,
                                        pid,
                                        default_grace,
                                    );
                                }
                            }

                            // Restore lobby entry if game hasn't started.
                            // Persisted sessions pre-date version metadata;
                            // restored lobbies appear without a version badge.
                            if let Some(meta) = lobby_meta {
                                if !is_started {
                                    lob.register_game(
                                        game_code,
                                        RegisterGameRequest {
                                            host_name: meta.host_name,
                                            public: meta.public,
                                            password: meta.password,
                                            timer_seconds: meta.timer_seconds,
                                            current_players: session.current_player_count(),
                                            max_players: session.player_count as u32,
                                            format_config: Some(
                                                session.state.format_config.clone(),
                                            ),
                                            match_config: session.state.match_config,
                                            ..Default::default()
                                        },
                                        &SysEnv,
                                    );
                                }
                            }

                            mgr.restore_session(session);
                            restored += 1;
                        }
                        Err(e) => {
                            warn!(game = %game_code, error = %e, "failed to restore session, deleting");
                            let _ = game_db.delete_session(game_code);
                        }
                    }
                }

                if restored > 0 {
                    info!(count = restored, "restored active games from disk");
                }
            }
            Err(e) => {
                error!(error = %e, "failed to load persisted sessions");
            }
        }

        // Restore persisted draft sessions from disk
        match game_db.load_all_drafts() {
            Ok(persisted_drafts) => {
                let mut dsm = draft_sessions.lock().await;
                let mut restored_drafts = 0u32;
                for (draft_code, json) in &persisted_drafts {
                    match serde_json::from_str::<server_core::persist::PersistedDraftSession>(json)
                    {
                        Ok(ps) => {
                            let timer_ms = ps.timer_remaining_ms;
                            dsm.restore_session(ps);
                            if let Some(ms) = timer_ms {
                                info!(draft = %draft_code, remaining_ms = ms, "draft session has pending timer");
                            }
                            restored_drafts += 1;
                        }
                        Err(e) => {
                            warn!(draft = %draft_code, error = %e, "failed to restore draft session, deleting");
                            let _ = game_db.delete_draft_session(draft_code);
                        }
                    }
                }
                if restored_drafts > 0 {
                    info!(count = restored_drafts, "restored draft sessions from disk");
                }
            }
            Err(e) => error!(error = %e, "failed to load persisted draft sessions"),
        }
    }

    // Spawn background task for grace period and lobby expiry
    let bg_state = state.clone();
    let bg_draft_state = draft_sessions.clone();
    let bg_connections = connections.clone();
    let bg_draft_spectators = draft_spectators.clone();
    let bg_lobby = lobby.clone();
    let bg_lobby_subs = lobby_subscribers.clone();
    let bg_game_db = game_db.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;

            // Check reconnect grace period expiry
            let expired = {
                let mut mgr = bg_state.lock().await;
                mgr.reconnect.check_expired()
            };
            if !expired.is_empty() {
                // Remove in-memory sessions first (state lock → connections lock order)
                {
                    let mut mgr = bg_state.lock().await;
                    for game_code in &expired {
                        mgr.sessions.remove(game_code);
                    }
                }
                // Notify connected players and clean up persistence
                let conns = bg_connections.lock().await;
                for game_code in &expired {
                    info!(game = %game_code, reason = "disconnect_expired", "game over");
                    if let Some(players) = conns.get(game_code) {
                        let msg = ServerMessage::GameOver {
                            winner: None,
                            reason: "Opponent disconnected (grace period expired)".to_string(),
                        };
                        for sender in players.values() {
                            let _ = sender.send(msg.clone());
                        }
                    }
                    if let Err(e) = bg_game_db.delete_session(game_code) {
                        error!(game = %game_code, error = %e, "failed to delete persisted session");
                    }
                }
            }

            // Check lobby game expiry (5 minute timeout for waiting games).
            // The broker reaps stale entries and returns the LobbyGameRemoved
            // fan-out outbounds; the Full-mode session/db deletion stays here
            // (the broker is WASM-safe and has no SQLite/SessionManager). The
            // expired codes are recovered from the returned outbounds.
            let reap_outbounds = {
                let mut broker = bg_lobby.lock().await;
                broker.reap_expired(300, &SysEnv)
            };
            if !reap_outbounds.is_empty() {
                let expired_lobby: Vec<String> = reap_outbounds
                    .iter()
                    .filter_map(|ob| match ob {
                        Outbound::ToSubscribers(
                            lobby_broker::LobbyServerMessage::LobbyGameRemoved { game_code },
                        ) => Some(game_code.clone()),
                        _ => None,
                    })
                    .collect();
                info!(count = expired_lobby.len(), "expiring stale lobby games");
                let mut mgr = bg_state.lock().await;
                for game_code in &expired_lobby {
                    mgr.sessions.remove(game_code);
                    if let Err(e) = bg_game_db.delete_session(game_code) {
                        error!(game = %game_code, error = %e, "failed to delete expired lobby session");
                    }
                }
                drop(mgr);

                let subs = bg_lobby_subs.lock().await;
                for ob in reap_outbounds {
                    if let Outbound::ToSubscribers(msg) = ob {
                        let server_msg = to_server_message(msg);
                        for sub in subs.iter() {
                            let _ = sub.send(server_msg.clone());
                        }
                    }
                }
            }

            // Check draft disconnect grace period expiry — auto-pick for disconnected seats
            let draft_expired = {
                let mut mgr = bg_draft_state.lock().await;
                mgr.reconnect.check_expired_with_players()
            };
            if !draft_expired.is_empty() {
                let mut mgr = bg_draft_state.lock().await;
                for (draft_code, player_id) in &draft_expired {
                    let seat = player_id.0;
                    if let Some(session) = mgr.sessions.get(draft_code.as_str()) {
                        if session.session.status == draft_core::types::DraftStatus::Drafting
                            && !session.connected[seat as usize]
                        {
                            match mgr.pick_random_for_seat(draft_code, seat, None) {
                                Ok(()) => {
                                    info!(
                                        draft = %draft_code,
                                        seat,
                                        "auto-picked for disconnected seat (grace expired)"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        draft = %draft_code,
                                        seat,
                                        error = %e,
                                        "auto-pick on grace expiry failed"
                                    );
                                }
                            }
                        }
                    }
                }
                // Broadcast updated views + persist for any modified drafts
                let affected_drafts: Vec<String> = draft_expired
                    .iter()
                    .map(|(code, _)| code.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .into_iter()
                    .collect();
                drop(mgr);
                for draft_code in &affected_drafts {
                    // Broadcast to players
                    let views: Vec<_> = {
                        let mgr = bg_draft_state.lock().await;
                        let Some(session) = mgr.sessions.get(draft_code) else {
                            continue;
                        };
                        let pod_size = session.player_tokens.len();
                        (0..pod_size).map(|i| session.view_for_seat(i)).collect()
                    };
                    broadcast_draft_views(draft_code, &views, &bg_connections, &bg_draft_state)
                        .await;
                    // Broadcast to spectators
                    broadcast_draft_spectator_views(
                        draft_code,
                        &bg_draft_state,
                        &bg_draft_spectators,
                    )
                    .await;
                    // Persist
                    persist_draft_session_async(&bg_game_db, draft_code, &bg_draft_state).await;
                }
            }
        }
    });

    let cors = match cli.cors_origin.as_deref() {
        Some("*") | None => CorsLayer::permissive(),
        Some(origin) => CorsLayer::new()
            .allow_origin(origin.parse::<HeaderValue>().expect("invalid CORS origin")),
    };

    // Keep references for shutdown flush (Arcs are cheap to clone)
    let shutdown_state = state.clone();
    let shutdown_draft_state = draft_sessions.clone();
    let shutdown_game_db = game_db.clone();

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .route("/admin/drafts", get(admin::admin_list_drafts))
        .route(
            "/admin/drafts/{code}",
            get(admin::admin_get_draft).delete(admin::admin_delete_draft),
        )
        .route("/p2p-draft-backup", post(admin::p2p_backup_store))
        .route(
            "/p2p-draft-backup/{code}",
            get(admin::p2p_backup_get).delete(admin::p2p_backup_delete),
        )
        .layer(cors)
        .with_state(AppState {
            sessions: state,
            draft_sessions,
            draft_pools,
            connections,
            db,
            lobby,
            lobby_subscribers,
            player_count,
            game_db,
            draft_spectators,
            mode,
        });

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", cli.port))
        .await
        .expect("failed to bind");
    info!(port = %cli.port, "phase-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    // Flush all active sessions to SQLite before exiting so they survive restart
    let mgr = shutdown_state.lock().await;
    let mut persisted = 0u32;
    for (game_code, session) in &mgr.sessions {
        let snapshot = session.to_persisted();
        match serde_json::to_string(&snapshot) {
            Ok(json) => {
                if let Err(e) = shutdown_game_db.save_session(game_code, &json) {
                    error!(game = %game_code, error = %e, "failed to persist session on shutdown");
                } else {
                    persisted += 1;
                }
            }
            Err(e) => {
                error!(game = %game_code, error = %e, "failed to serialize session on shutdown");
            }
        }
    }
    if persisted > 0 {
        info!(
            count = persisted,
            "flushed active sessions to disk on shutdown"
        );
    }

    // Flush all active draft sessions to SQLite
    let dsm = shutdown_draft_state.lock().await;
    let mut flushed_drafts = 0u32;
    for (draft_code, session) in &dsm.sessions {
        let snapshot = session.to_persisted();
        match serde_json::to_string(&snapshot) {
            Ok(json) => {
                if let Err(e) = shutdown_game_db.save_draft_session(draft_code, &json) {
                    error!(draft = %draft_code, error = %e, "failed to persist draft on shutdown");
                } else {
                    flushed_drafts += 1;
                }
            }
            Err(e) => {
                error!(draft = %draft_code, error = %e, "failed to serialize draft for shutdown");
            }
        }
    }
    if flushed_drafts > 0 {
        info!(
            count = flushed_drafts,
            "flushed draft sessions to disk on shutdown"
        );
    }
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to register SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => info!("received Ctrl+C, shutting down"),
            _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("failed to listen for Ctrl+C");
        info!("received Ctrl+C, shutting down");
    }
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Clone)]
struct AppState {
    sessions: SharedState,
    draft_sessions: SharedDraftState,
    draft_pools: SharedDraftPools,
    connections: SharedConnections,
    db: SharedDb,
    lobby: SharedLobby,
    lobby_subscribers: SharedLobbySubscribers,
    player_count: SharedPlayerCount,
    game_db: SharedGameDb,
    draft_spectators: SharedDraftSpectators,
    mode: Mode,
}

async fn ws_handler(ws: WebSocketUpgrade, State(app_state): State<AppState>) -> impl IntoResponse {
    let current = app_state.player_count.load(Ordering::Relaxed);
    if current >= MAX_CONNECTIONS {
        warn!(
            online_count = current,
            limit = MAX_CONNECTIONS,
            "connection limit reached, rejecting"
        );
        return (http::StatusCode::SERVICE_UNAVAILABLE, "Server full").into_response();
    }

    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                app_state.sessions,
                app_state.draft_sessions,
                app_state.draft_pools,
                app_state.connections,
                app_state.db,
                app_state.lobby,
                app_state.lobby_subscribers,
                app_state.player_count,
                app_state.game_db,
                app_state.draft_spectators,
                app_state.mode,
            )
        })
        .into_response()
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket(
    mut socket: WebSocket,
    state: SharedState,
    draft_state: SharedDraftState,
    draft_pools: SharedDraftPools,
    connections: SharedConnections,
    db: SharedDb,
    lobby: SharedLobby,
    lobby_subscribers: SharedLobbySubscribers,
    player_count: SharedPlayerCount,
    game_db: SharedGameDb,
    draft_spectators: SharedDraftSpectators,
    mode: Mode,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    let count = player_count.fetch_add(1, Ordering::Relaxed) + 1;
    info!(online_count = count, "client connected");
    broadcast_player_count(&lobby_subscribers, count).await;

    let mut identity = SocketIdentity {
        game_code: None,
        player_id: None,
        player_token: None,
        lobby_subscribed: false,
        session_span: None,
        client_hello: None,
        lobby_host_game: None,
        seat_reservations: Vec::new(),
        lobby_reservations: Vec::new(),
        draft_code: None,
        draft_seat: None,
        draft_token: None,
        spectator_draft_code: None,
        spectator_visibility: None,
    };
    let mut rate_limiter = RateLimiter::new();

    // Greet the client with our version identity. The client uses this to
    // decide whether to proceed (protocol-version mismatch ⇒ it gives up
    // before sending any game-affecting frame). The advertised `mode` lets
    // the client route host/join flows through WS (Full) or P2P+broker
    // (LobbyOnly) without probing.
    let hello = ServerMessage::ServerHello {
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        build_commit: build_commit().to_string(),
        protocol_version: PROTOCOL_VERSION,
        mode,
    };
    if let Ok(json) = serde_json::to_string(&hello) {
        if socket.send(Message::text(json)).await.is_err() {
            let count = player_count.fetch_sub(1, Ordering::Relaxed) - 1;
            broadcast_player_count(&lobby_subscribers, count).await;
            return;
        }
    }

    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if socket.send(Message::text(json)).await.is_err() {
                        break;
                    }
                }
            }

            result = socket.recv() => {
                match result {
                    Some(Ok(msg)) => {
                        let text = match msg {
                            Message::Text(t) => t.to_string(),
                            Message::Close(_) => break,
                            _ => continue,
                        };

                        if !rate_limiter.check() {
                            debug!("rate limit exceeded, dropping message");
                            continue;
                        }

                        let client_msg: ClientMessage = match serde_json::from_str(&text) {
                            Ok(m) => m,
                            Err(e) => {
                                warn!(error = %e, "failed to parse client message");
                                let err_msg = ServerMessage::Error {
                                    message: format!("Invalid message: {}", e),
                                };
                                if let Ok(json) = serde_json::to_string(&err_msg) {
                                    let _ = socket.send(Message::text(json)).await;
                                }
                                continue;
                            }
                        };

                        let span = identity.session_span.clone()
                            .unwrap_or_else(|| info_span!("ws_message"));
                        handle_client_message(
                            client_msg,
                            &mut socket,
                            &state,
                            &draft_state,
                            &draft_pools,
                            &connections,
                            &db,
                            &lobby,
                            &lobby_subscribers,
                            &player_count,
                            &game_db,
                            &draft_spectators,
                            &tx,
                            &mut identity,
                            mode,
                        )
                        .instrument(span)
                        .await;
                    }
                    Some(Err(_)) | None => break,
                }
            }
        }
    }

    // Socket closed -- handle disconnect
    info!(
        game = ?identity.game_code,
        player = ?identity.player_id,
        "client disconnected"
    );
    // Handle draft session disconnect
    if let (Some(draft_code), Some(seat)) = (&identity.draft_code, identity.draft_seat) {
        let mut mgr = draft_state.lock().await;
        mgr.handle_disconnect(draft_code, seat);
    }

    if let (Some(game_code), Some(player_id)) = (&identity.game_code, &identity.player_id) {
        let mut mgr = state.lock().await;
        mgr.handle_disconnect(game_code, *player_id);

        // Notify all other connected players about this disconnection
        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            let msg = ServerMessage::OpponentDisconnected {
                grace_seconds: 120,
                player: Some(*player_id),
            };
            for (&pid, sender) in players.iter() {
                if pid != *player_id {
                    let _ = sender.send(msg.clone());
                }
            }
        }
    }

    if !identity.seat_reservations.is_empty() {
        let changed = {
            let mut mgr = state.lock().await;
            mgr.release_reservations(&identity.seat_reservations)
        };
        if changed {
            for (game_code, _) in &identity.seat_reservations {
                broadcast_player_slots(&state, &connections, game_code).await;
                let updated = {
                    let current = {
                        let mut mgr = state.lock().await;
                        mgr.sessions.get_mut(game_code).map(|session| {
                            session.cleanup_expired_reservations();
                            session.current_player_count()
                        })
                    };
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    if let Some(current) = current {
                        lob.set_current_players(game_code, current, &SysEnv);
                    }
                    lob.public_game(game_code)
                };
                if let Some(game) = updated {
                    broadcast_to_lobby_subscribers(
                        &lobby_subscribers,
                        ServerMessage::LobbyGameUpdated { game },
                    )
                    .await;
                }
            }
        }
    }

    // Lobby teardown (reservation releases → host-entry removal → subscriber
    // pruning) is the broker's `on_disconnect`. It emits, in order, a
    // LobbyGameUpdated per released reservation, then a LobbyGameRemoved if
    // this socket owned an entry, then RemoveSubscriber. The 5-minute
    // staleness reaper is the fallback if this path doesn't fire (e.g. crash).
    // Player-count decrement + broadcast stays shell-side (unconditional).
    {
        let mut conn = identity.to_conn_state();
        let outbounds = {
            let mut broker = lobby.lock().await;
            broker.on_disconnect(&mut conn)
        };
        identity.absorb_conn_state(conn);
        apply_outbounds(outbounds, &tx, &lobby_subscribers, &player_count).await;
    }

    let count = player_count.fetch_sub(1, Ordering::Relaxed) - 1;
    broadcast_player_count(&lobby_subscribers, count).await;
}

async fn broadcast_player_count(lobby_subscribers: &SharedLobbySubscribers, count: u32) {
    let subs = lobby_subscribers.lock().await;
    let msg = ServerMessage::PlayerCount { count };
    for sub in subs.iter() {
        let _ = sub.send(msg.clone());
    }
}

/// Send PlayerSlotsUpdate to all connected players in a game.
async fn broadcast_player_slots(
    state: &SharedState,
    connections: &SharedConnections,
    game_code: &str,
) {
    let slots = {
        let mgr = state.lock().await;
        match mgr.sessions.get(game_code) {
            Some(session) => session.player_slot_info(),
            None => return,
        }
    };
    let msg = ServerMessage::PlayerSlotsUpdate { slots };
    let conns = connections.lock().await;
    if let Some(players) = conns.get(game_code) {
        for sender in players.values() {
            let _ = sender.send(msg.clone());
        }
    }
}

async fn broadcast_to_lobby_subscribers(
    lobby_subscribers: &SharedLobbySubscribers,
    msg: ServerMessage,
) {
    let subs = lobby_subscribers.lock().await;
    for sub in subs.iter() {
        let _ = sub.send(msg.clone());
    }
}

/// Translate a broker [`lobby_broker::LobbyServerMessage`] into the canonical
/// transport [`ServerMessage`]. Pure field-mapping at the serialization
/// boundary — the two enums are wire-compatible (guarded by the lobby wire
/// contract test); the shared payload types (`LobbyGame`, `FormatConfig`,
/// `MatchConfig`) are the same structs, so this is a zero-cost re-tag.
fn to_server_message(m: lobby_broker::LobbyServerMessage) -> ServerMessage {
    use lobby_broker::LobbyServerMessage as L;
    match m {
        L::ServerHello {
            server_version,
            build_commit,
            protocol_version,
            mode,
        } => ServerMessage::ServerHello {
            server_version,
            build_commit,
            protocol_version,
            mode: match mode {
                lobby_broker::ServerMode::Full => ServerMode::Full,
                lobby_broker::ServerMode::LobbyOnly => ServerMode::LobbyOnly,
            },
        },
        L::GameCreated {
            game_code,
            player_token,
        } => ServerMessage::GameCreated {
            game_code,
            player_token,
        },
        L::Error { message } => ServerMessage::Error { message },
        L::LobbyUpdate { games } => ServerMessage::LobbyUpdate { games },
        L::LobbyGameAdded { game } => ServerMessage::LobbyGameAdded { game },
        L::LobbyGameUpdated { game } => ServerMessage::LobbyGameUpdated { game },
        L::LobbyGameRemoved { game_code } => ServerMessage::LobbyGameRemoved { game_code },
        L::PlayerCount { count } => ServerMessage::PlayerCount { count },
        L::PasswordRequired { game_code } => ServerMessage::PasswordRequired { game_code },
        L::JoinTargetInfo {
            game_code,
            is_p2p,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
            reservation_expires_at_ms,
        } => ServerMessage::JoinTargetInfo {
            game_code,
            is_p2p,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
            reservation_expires_at_ms,
        },
        L::Pong { timestamp } => ServerMessage::Pong { timestamp },
        L::PeerInfo {
            game_code,
            host_peer_id,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
        } => ServerMessage::PeerInfo {
            game_code,
            host_peer_id,
            format_config,
            match_config,
            player_count,
            filled_seats,
            reservation_token,
        },
    }
}

/// Project a canonical [`ClientMessage`] onto the broker's lobby subset
/// [`lobby_broker::LobbyClientMessage`]. The native shell already deserialized
/// and gated the full `ClientMessage` (unknown tags rejected at parse time, so
/// the two-stage `Envelope` path is unneeded here — it serves the DO shell).
/// Returns `None` for non-lobby messages, which the caller dispatches normally.
fn to_lobby_client_message(msg: &ClientMessage) -> Option<lobby_broker::LobbyClientMessage> {
    use lobby_broker::LobbyClientMessage as L;
    Some(match msg {
        ClientMessage::ClientHello {
            client_version,
            build_commit,
            protocol_version,
        } => L::ClientHello {
            client_version: client_version.clone(),
            build_commit: build_commit.clone(),
            protocol_version: *protocol_version,
        },
        ClientMessage::SubscribeLobby => L::SubscribeLobby,
        ClientMessage::UnsubscribeLobby => L::UnsubscribeLobby,
        ClientMessage::Ping { timestamp } => L::Ping {
            timestamp: *timestamp,
        },
        ClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            public,
            password,
            timer_seconds,
            player_count,
            match_config,
            ai_seats: _,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            start_when_full,
        } => L::CreateGameWithSettings {
            deck: deck.clone(),
            display_name: display_name.clone(),
            public: *public,
            password: password.clone(),
            timer_seconds: *timer_seconds,
            player_count: *player_count,
            match_config: *match_config,
            format_config: format_config.clone(),
            room_name: room_name.clone(),
            host_peer_id: host_peer_id.clone(),
            draft_metadata: draft_metadata.clone(),
            start_when_full: *start_when_full,
        },
        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => L::JoinGameWithPassword {
            game_code: game_code.clone(),
            deck: deck.clone(),
            display_name: display_name.clone(),
            password: password.clone(),
            reservation_token: reservation_token.clone(),
        },
        ClientMessage::LookupJoinTarget {
            game_code,
            password,
            reserve,
            display_name,
            release_reservation_token,
        } => L::LookupJoinTarget {
            game_code: game_code.clone(),
            password: password.clone(),
            reserve: *reserve,
            display_name: display_name.clone(),
            release_reservation_token: release_reservation_token.clone(),
        },
        ClientMessage::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
            consumed_reservation_tokens,
        } => L::UpdateLobbyMetadata {
            game_code: game_code.clone(),
            current_players: *current_players,
            max_players: *max_players,
            consumed_reservation_tokens: consumed_reservation_tokens.clone(),
        },
        ClientMessage::UnregisterLobby { game_code } => L::UnregisterLobby {
            game_code: game_code.clone(),
        },
        _ => return None,
    })
}

/// Run a lobby-broker dispatch end to end: project the message, hold the lobby
/// lock for the synchronous `Broker::handle`, drop it, then interpret the
/// returned outbounds. Centralizes the lock/sync-back discipline so each arm is
/// a one-liner.
async fn dispatch_broker(
    msg: &ClientMessage,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
) {
    let Some(lobby_msg) = to_lobby_client_message(msg) else {
        return;
    };
    dispatch_broker_msg(
        lobby_msg,
        lobby,
        lobby_subscribers,
        player_count,
        tx,
        identity,
    )
    .await;
}

/// Lower-level broker dispatch taking an already-projected
/// [`lobby_broker::LobbyClientMessage`]. Used by arms that destructured the
/// owned `ClientMessage` (so `&client_msg` is no longer available) but whose
/// LobbyOnly path delegates to the broker.
async fn dispatch_broker_msg(
    lobby_msg: lobby_broker::LobbyClientMessage,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
) {
    let mut conn = identity.to_conn_state();
    let outbounds = {
        let mut broker = lobby.lock().await;
        broker.handle(&mut conn, lobby_msg, &SysEnv)
    };
    identity.absorb_conn_state(conn);
    apply_outbounds(outbounds, tx, lobby_subscribers, player_count).await;
}

/// Interpret an ordered `Vec<Outbound>` from the broker over the shell's
/// transport. `ToSelf` point replies go through this connection's mpsc sender
/// (same path the pre-extraction SubscribeLobby used); fan-out and
/// subscriber/count side effects use the existing `lobby_subscribers` /
/// `player_count` machinery. Order is preserved exactly as returned.
async fn apply_outbounds(
    outbounds: Vec<Outbound>,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
) {
    for ob in outbounds {
        match ob {
            Outbound::ToSelf(msg) => {
                // Point replies go through this connection's mpsc sender (drained
                // by the select loop), exactly as the pre-extraction
                // SubscribeLobby path did. Using `tx` rather than a direct
                // `socket.send` preserves ordering relative to concurrently
                // broadcast frames that may also land in this conn's queue.
                let _ = tx.send(to_server_message(msg));
            }
            Outbound::ToSubscribers(msg) => {
                broadcast_to_lobby_subscribers(lobby_subscribers, to_server_message(msg)).await;
            }
            Outbound::AddSubscriber => {
                let mut subs = lobby_subscribers.lock().await;
                subs.push(tx.clone());
            }
            Outbound::RemoveSubscriber => {
                let mut subs = lobby_subscribers.lock().await;
                subs.retain(|s| !s.is_closed());
            }
            Outbound::SendPlayerCountToSelf => {
                let count = player_count.load(Ordering::Relaxed);
                let _ = tx.send(ServerMessage::PlayerCount { count });
            }
        }
    }
}

/// Fire-and-forget persistence of a game session to SQLite.
fn persist_session_async(
    game_db: &SharedGameDb,
    game_code: &str,
    session: &server_core::session::GameSession,
) {
    let db = game_db.clone();
    let persisted = session.to_persisted();
    let code = game_code.to_string();
    tokio::task::spawn_blocking(move || match serde_json::to_string(&persisted) {
        Ok(json) => {
            if let Err(e) = db.save_session(&code, &json) {
                error!(game = %code, error = %e, "failed to persist game session");
            }
        }
        Err(e) => {
            error!(game = %code, error = %e, "failed to serialize game session");
        }
    });
}

/// Broadcast `DraftSpectatorView` to all spectators watching a draft.
/// Prunes disconnected spectators (closed sender channels).
async fn broadcast_draft_spectator_views(
    draft_code: &str,
    draft_state: &SharedDraftState,
    draft_spectators: &SharedDraftSpectators,
) {
    let mut specs = draft_spectators.lock().await;
    let Some(spectators) = specs.get_mut(draft_code) else {
        return;
    };

    let mgr = draft_state.lock().await;
    let Some(session) = mgr.sessions.get(draft_code) else {
        return;
    };

    // Retain only live senders, sending views to each
    spectators.retain(|(visibility, sender)| {
        let view = draft_core::view::filter_for_spectator(&session.session, *visibility);
        let msg = ServerMessage::DraftSpectatorView { view };
        sender.send(msg).is_ok()
    });

    // Clean up empty entries
    if spectators.is_empty() {
        specs.remove(draft_code);
    }
}

/// Fire-and-forget persistence of a draft session to SQLite.
async fn persist_draft_session_async(
    game_db: &SharedGameDb,
    draft_code: &str,
    draft_state: &SharedDraftState,
) {
    let mgr = draft_state.lock().await;
    let Some(session) = mgr.sessions.get(draft_code) else {
        return;
    };
    let snapshot = session.to_persisted();
    let db = game_db.clone();
    let code = draft_code.to_string();
    tokio::task::spawn_blocking(move || match serde_json::to_string(&snapshot) {
        Ok(json) => {
            if let Err(e) = db.save_draft_session(&code, &json) {
                error!(draft = %code, error = %e, "failed to persist draft session");
            }
        }
        Err(e) => {
            error!(draft = %code, error = %e, "failed to serialize draft session");
        }
    });
}

/// Fire-and-forget deletion of a persisted game session.
fn delete_session_async(game_db: &SharedGameDb, game_code: &str) {
    let db = game_db.clone();
    let code = game_code.to_string();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = db.delete_session(&code) {
            error!(game = %code, error = %e, "failed to delete persisted session");
        }
    });
}

/// If this game_code belongs to a draft tournament, auto-report the match
/// result to the DraftSessionManager and broadcast updated views. This
/// implements Pitfall 6 from RESEARCH: clients must NOT send
/// ReportMatchResult for server-hosted drafts — the server handles it.
async fn report_draft_game_over(
    draft_state: &SharedDraftState,
    connections: &SharedConnections,
    game_code: &str,
    winner: Option<PlayerId>,
) {
    let draft_code = {
        let mgr = draft_state.lock().await;
        mgr.draft_for_game_code(game_code)
    };
    let Some(draft_code) = draft_code else {
        return;
    };

    // Find the match_id and winner_seat from the draft session
    let (match_id, winner_seat) = {
        let mgr = draft_state.lock().await;
        let Some(session) = mgr.sessions.get(&draft_code) else {
            return;
        };
        // Find the match_id that maps to this game_code
        let match_entry = session
            .active_matches
            .iter()
            .find(|(_, gc)| gc.as_str() == game_code);
        let Some((match_id, _)) = match_entry else {
            warn!(draft = %draft_code, game = %game_code, "game_code not found in active_matches");
            return;
        };
        let match_id = match_id.clone();

        // Map PlayerId winner to seat index
        let winner_seat = winner.map(|pid| pid.0);

        (match_id, winner_seat)
    };

    info!(
        draft = %draft_code,
        game = %game_code,
        match_id = %match_id,
        winner_seat = ?winner_seat,
        "auto-reporting draft match result from GameOver"
    );

    let views = {
        let mut mgr = draft_state.lock().await;
        let action = draft_core::types::DraftAction::ReportMatchResult {
            match_id,
            winner_seat,
        };
        match mgr.apply_system_action(&draft_code, action, None) {
            Ok(views) => views,
            Err(e) => {
                warn!(draft = %draft_code, error = %e, "failed to auto-report draft match result");
                return;
            }
        }
    };

    // Broadcast updated views to all draft pod members
    let conns = connections.lock().await;
    if let Some(players) = conns.get(&draft_code) {
        for (pid, sender) in players.iter() {
            let seat = pid.0 as usize;
            if let Some(view) = views.get(seat) {
                let _ = sender.send(ServerMessage::DraftStateUpdate { view: view.clone() });
            }
        }
    }
}

/// Broadcast `DraftStateUpdate` to all connected sockets in a draft pod.
/// Iterates the connections map and filters by `identity.draft_code` match.
/// Because `SocketIdentity` is per-socket state (not stored globally), we
/// instead iterate draft session seats and send per-seat views via the
/// connections map keyed by draft_code.
async fn broadcast_draft_views(
    draft_code: &str,
    views: &[draft_core::view::DraftPlayerView],
    connections: &SharedConnections,
    draft_state: &SharedDraftState,
) {
    let conns = connections.lock().await;
    // Draft connections are stored under the draft_code in the connections map
    if let Some(players) = conns.get(draft_code) {
        for (pid, sender) in players.iter() {
            let seat = pid.0 as usize;
            if let Some(view) = views.get(seat) {
                let msg = ServerMessage::DraftStateUpdate { view: view.clone() };
                let _ = sender.send(msg);
            }
        }
    } else {
        // Fallback: broadcast to all sockets that have a matching draft_code
        // by sending the first view (for reconnect cases where identity is set
        // but connection may not be in the draft_code map yet)
        let _ = draft_state; // suppress unused
    }
}

/// Spawn a pick timer task. When the timer expires, auto-pick a random card
/// for any seat that hasn't picked yet. Aborts the previous timer if one exists.
fn spawn_pick_timer(
    draft_state: SharedDraftState,
    connections: SharedConnections,
    draft_code: String,
    pick_seconds: u32,
) {
    let timer_draft_code = draft_code.clone();
    let timer_draft_state = draft_state.clone();
    let timer_connections = connections;

    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(pick_seconds as u64)).await;

        let mut mgr = timer_draft_state.lock().await;
        let Some(session) = mgr.sessions.get_mut(&timer_draft_code) else {
            return;
        };

        // Only auto-pick if still in Drafting status
        if session.session.status != draft_core::types::DraftStatus::Drafting {
            return;
        }

        info!(draft = %timer_draft_code, "pick timer expired — auto-picking for pending seats");

        // Find seats that still have a current pack (haven't picked yet)
        let pod_size = session.player_tokens.len();
        for seat_idx in 0..pod_size {
            if let Some(pack) = &session.session.current_pack[seat_idx] {
                if !pack.0.is_empty() {
                    let card_id = pack.0[0].instance_id.clone();
                    let action = draft_core::types::DraftAction::Pick {
                        seat: seat_idx as u8,
                        card_instance_id: card_id,
                    };
                    if let Err(e) = draft_core::session::apply(&mut session.session, action, None) {
                        warn!(
                            draft = %timer_draft_code,
                            seat = seat_idx,
                            error = %e,
                            "auto-pick failed"
                        );
                    }
                }
            }
        }

        // Broadcast updated views
        let views: Vec<_> = (0..pod_size).map(|i| session.view_for_seat(i)).collect();
        drop(mgr);

        let conns = timer_connections.lock().await;
        if let Some(players) = conns.get(&timer_draft_code) {
            for (pid, sender) in players.iter() {
                let seat = pid.0 as usize;
                if let Some(view) = views.get(seat) {
                    let _ = sender.send(ServerMessage::DraftStateUpdate { view: view.clone() });
                }
            }
        }
    });

    // Store the handle so it can be aborted if all picks come in early
    tokio::spawn(async move {
        let mut mgr = draft_state.lock().await;
        if let Some(session) = mgr.sessions.get_mut(&draft_code) {
            // Abort previous timer if any (T-59-07: prevent timer task accumulation)
            if let Some(prev) = session.timer_task.take() {
                prev.abort();
            }
            session.timer_task = Some(handle);
        }
    });
}

struct ServerDeckResolver<'a> {
    db: &'a CardDatabase,
}

impl DeckResolver for ServerDeckResolver<'_> {
    fn resolve(
        &self,
        choice: &DeckChoice,
    ) -> Result<engine::game::deck_loading::PlayerDeckList, String> {
        let deck = match choice {
            DeckChoice::Random => server_core::starter_decks::random_starter_deck(),
            DeckChoice::Named(name) => server_core::starter_decks::find_starter_deck(name)
                .ok_or_else(|| format!("Starter deck not found: {name}"))?,
            DeckChoice::DeckList(deck) => deck.as_ref().clone(),
        };
        // The reducer stays at the name-only layer (see `DeckResolver` docs),
        // but we MUST still validate the names against the card database here
        // — otherwise a deck containing unresolvable names propagates through
        // `apply_seat_delta` as `None`, and `start_game` silently substitutes
        // an empty `PlayerDeckPayload` (see `Session::start_game`). The result
        // is CR 704.5b losing every player on their first draw step with no
        // user-visible error. Validating here causes the reducer to return
        // `Err`, which phase-server then surfaces to the client.
        server_core::resolve_deck(self.db, &deck)?;
        Ok(engine::game::deck_loading::PlayerDeckList {
            main_deck: deck.main_deck,
            sideboard: deck.sideboard,
            commander: deck.commander,
        })
    }
}

async fn broadcast_game_started(
    state: &SharedState,
    connections: &SharedConnections,
    game_db: &SharedGameDb,
    game_code: &str,
) {
    let player_messages = {
        let mut mgr = state.lock().await;
        let Some(session) = mgr.sessions.get_mut(game_code) else {
            return;
        };

        session.run_ai();
        persist_session_async(game_db, game_code, session);
        build_game_started_messages(session)
    };

    {
        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            for (pid, msg) in &player_messages {
                if let Some(sender) = players.get(pid) {
                    let _ = sender.send(msg.clone());
                }
            }
        }
    }
}

async fn require_host(identity: &SocketIdentity, socket: &mut WebSocket) -> Result<(), ()> {
    if identity.player_id != Some(PlayerId(0)) {
        let msg = ServerMessage::Error {
            message: "Only the host can modify seats.".to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::text(json)).await;
        }
        return Err(());
    }
    Ok(())
}

fn is_joining_current_game(identity: &SocketIdentity, target_game_code: &str) -> bool {
    identity
        .game_code
        .as_deref()
        .is_some_and(|active| active == target_game_code)
        || identity
            .lobby_host_game
            .as_deref()
            .is_some_and(|hosted| hosted == target_game_code)
}

async fn reject_joining_current_game(
    identity: &SocketIdentity,
    target_game_code: &str,
    socket: &mut WebSocket,
) -> Result<(), ()> {
    if !is_joining_current_game(identity, target_game_code) {
        return Ok(());
    }

    let msg = ServerMessage::Error {
        message: "You are already in this game.".to_string(),
    };
    if let Ok(json) = serde_json::to_string(&msg) {
        let _ = socket.send(Message::text(json)).await;
    }
    Err(())
}

async fn draft_pack_generator_for_start(
    draft_state: &SharedDraftState,
    draft_pools: &SharedDraftPools,
    draft_code: &str,
) -> Result<draft_core::pack_generator::PackGenerator, String> {
    let set_code = {
        let mgr = draft_state.lock().await;
        let session = mgr
            .sessions
            .get(draft_code)
            .ok_or_else(|| format!("Draft not found: {draft_code}"))?;
        session.config.set_code.clone()
    };

    draft_pools
        .generator_for_set(&set_code)
        .ok_or_else(|| format!("No draft pool data for set: {set_code}"))
}

#[allow(clippy::too_many_arguments)]
async fn handle_client_message(
    client_msg: ClientMessage,
    socket: &mut WebSocket,
    state: &SharedState,
    draft_state: &SharedDraftState,
    draft_pools: &SharedDraftPools,
    connections: &SharedConnections,
    db: &SharedDb,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    game_db: &SharedGameDb,
    draft_spectators: &SharedDraftSpectators,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
    mode: Mode,
) {
    // Handshake gate: ClientHello must be the first message. See
    // `classify_hello_gate` for the full truth table.
    match classify_hello_gate(
        identity.client_hello.is_some(),
        &client_msg,
        MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
    ) {
        HelloGateOutcome::Accept(info) => {
            info!(
                version = %info.client_version,
                commit = %info.build_commit,
                "ClientHello accepted"
            );
            identity.client_hello = Some(info);
            return;
        }
        HelloGateOutcome::RejectProtocol { client, server } => {
            warn!(
                client_protocol = client,
                server_protocol = server,
                "protocol version mismatch at ClientHello"
            );
            // Branch on which side is older so the user-facing remedy points at
            // the right party. "Please update" is wrong when the *server* is
            // the older one (post-bump preview server rolled back, or operator
            // running a stale build behind a freshly-deployed client).
            let remedy = if client < server {
                "Please update your client."
            } else {
                "This server is older than your client; wait for the rollout to complete."
            };
            let msg = ServerMessage::Error {
                message: format!(
                    "Protocol version mismatch (client={client} server={server}). {remedy}"
                ),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
        HelloGateOutcome::RejectHandshakeRequired => {
            warn!("client sent non-hello message before ClientHello");
            let msg = ServerMessage::Error {
                message: "ClientHello required before any other message".to_string(),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            return;
        }
        HelloGateOutcome::IgnoreRedundantHello => {
            debug!("ignoring redundant ClientHello");
            return;
        }
        HelloGateOutcome::PassThrough => {
            // Fall through to the regular dispatch below.
        }
    }

    // Mode gate: some messages are meaningless in one mode or the other.
    // Rejecting here keeps every handler below single-purpose — they never
    // need to second-guess whether the message should reach them.
    if let Some(reason) = reject_if_disabled(&client_msg, mode) {
        warn!(?mode, msg = ?std::mem::discriminant(&client_msg), %reason, "rejecting message disabled by server mode");
        let msg = ServerMessage::Error {
            message: reason.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&msg) {
            let _ = socket.send(Message::text(json)).await;
        }
        return;
    }

    match client_msg {
        ClientMessage::ClientHello { .. } => {
            // Unreachable: IgnoreRedundantHello above handled this case.
            debug!("unreachable ClientHello arm");
        }
        ClientMessage::CreateGame { deck } => {
            info!(deck_size = deck.main_deck.len(), "CreateGame");
            {
                let mgr = state.lock().await;
                if mgr.sessions.len() >= MAX_GAMES {
                    warn!(limit = MAX_GAMES, "max games reached, rejecting CreateGame");
                    let msg = ServerMessage::Error {
                        message: "Server is at game capacity, please try again later".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }
            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(error = %e, "CreateGame: deck resolve failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            let (game_code, player_token) = mgr.create_game(resolved);
            info!(game = %game_code, "game created");

            identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

            let mut conns = connections.lock().await;
            conns
                .entry(game_code.clone())
                .or_default()
                .insert(PlayerId(0), tx.clone());

            let msg = ServerMessage::GameCreated {
                game_code,
                player_token,
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::JoinGame { game_code, deck } => {
            info!(game = %game_code, deck_size = deck.main_deck.len(), "JoinGame");
            if reject_joining_current_game(identity, &game_code, socket)
                .await
                .is_err()
            {
                return;
            }

            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGame: deck resolve failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            match mgr.join_game(&game_code, resolved) {
                Ok((player_token, _filtered_state)) => {
                    mgr.set_card_names(&game_code, db.card_names());
                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    let joiner = session.player_for_token(&player_token).unwrap();
                    let started_messages = if session.is_full() {
                        session.run_ai();
                        persist_session_async(game_db, &game_code, session);
                        Some((
                            build_game_started_message(session, joiner, Some(player_token.clone())),
                            build_game_started_messages(session),
                        ))
                    } else {
                        None
                    };
                    info!(game = %game_code, player = ?joiner, "player joined");
                    identity.set_session(game_code.clone(), joiner, player_token.clone());
                    drop(mgr);

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    // Only send GameStarted when the game is full (all seats claimed)
                    if let Some((msg, other_messages)) = started_messages {
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }

                        // Send GameStarted to all other connected players
                        if let Some(players) = conns.get(&game_code) {
                            for (pid, msg) in other_messages {
                                if pid != joiner {
                                    if let Some(sender) = players.get(&pid) {
                                        let _ = sender.send(msg);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGame failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::Action { action } => {
            let game_code = match &identity.game_code {
                Some(c) => c.clone(),
                None => {
                    warn!("Action received but not in a game");
                    let msg = ServerMessage::Error {
                        message: "Not in a game".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };
            let player_token = match &identity.player_token {
                Some(t) => t.clone(),
                None => {
                    let msg = ServerMessage::Error {
                        message: "No player token".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            debug!(game = %game_code, player = ?identity.player_id, action = ?action, "Action");

            // Apply human action and collect AI follow-up results while holding the lock.
            // Filtering is deferred until after the lock is dropped to reduce contention.
            let action_result = {
                let lock_start = std::time::Instant::now();
                let mut mgr = state.lock().await;
                match mgr.handle_action(&game_code, &player_token, action) {
                    Ok(human_result) => {
                        // Run AI follow-up actions (still inside lock — needs &mut state)
                        let ai_results = match mgr.sessions.get_mut(&game_code) {
                            Some(session) => session.run_ai(),
                            None => vec![],
                        };
                        let session = mgr.sessions.get(&game_code).unwrap();
                        let eliminated = session.state.eliminated_players.clone();
                        let player_count = session.player_count;
                        let game_over_winner = match &session.state.waiting_for {
                            engine::types::game_state::WaitingFor::GameOver { winner } => {
                                Some(*winner)
                            }
                            _ => None,
                        };

                        // Persist or delete based on game-over state
                        if let Some(winner) = game_over_winner {
                            info!(game = %game_code, winner = ?winner, reason = "game_rules", "game over");
                            delete_session_async(game_db, &game_code);

                            // Auto-report draft match result if this game belongs to a draft
                            // (spawn as a separate task to avoid holding the state lock)
                            let ds = draft_state.clone();
                            let cs = connections.clone();
                            let gc = game_code.clone();
                            tokio::spawn(async move {
                                report_draft_game_over(&ds, &cs, &gc, winner).await;
                            });
                        } else {
                            persist_session_async(game_db, &game_code, session);
                        }

                        let lock_ms = lock_start.elapsed().as_millis();
                        info!(
                            game = %game_code,
                            lock_ms,
                            ai_actions = ai_results.len(),
                            "action processed (lock held)"
                        );

                        Ok((human_result, ai_results, eliminated, player_count))
                    }
                    Err(e) => Err(e),
                }
            }; // lock dropped — filtering happens below without blocking other games

            match action_result {
                Ok((
                    (
                        raw_state,
                        events,
                        legal_actions,
                        log_entries,
                        auto_pass_rec,
                        spell_costs,
                        legal_actions_by_object,
                    ),
                    ai_results,
                    eliminated,
                    player_count,
                )) => {
                    // Filter state per-player outside the lock
                    let filtered_states: Vec<(PlayerId, GameState)> = (0..player_count)
                        .map(|i| {
                            let pid = PlayerId(i);
                            (pid, server_core::filter_state_for_player(&raw_state, pid))
                        })
                        .collect();

                    // Broadcast human action result
                    {
                        let conns = connections.lock().await;
                        if let Some(players) = conns.get(&game_code) {
                            for (pid, pstate) in &filtered_states {
                                if let Some(s) = players.get(pid) {
                                    let actors = raw_state.waiting_for.acting_players();
                                    let is_actor = actors.contains(pid);
                                    let player_legals = if ai_results.is_empty() && is_actor {
                                        legal_actions.clone()
                                    } else {
                                        // AI will act next — don't send legal actions yet
                                        vec![]
                                    };
                                    let p_auto_pass = if ai_results.is_empty() && is_actor {
                                        auto_pass_rec
                                    } else {
                                        false
                                    };
                                    let p_spell_costs = if ai_results.is_empty() && is_actor {
                                        spell_costs.clone()
                                    } else {
                                        HashMap::new()
                                    };
                                    let p_by_object = if ai_results.is_empty() && is_actor {
                                        legal_actions_by_object.clone()
                                    } else {
                                        HashMap::new()
                                    };
                                    let _ = s.send(ServerMessage::StateUpdate {
                                        state: pstate.clone(),
                                        events: events.clone(),
                                        legal_actions: player_legals,
                                        auto_pass_recommended: p_auto_pass,
                                        eliminated_players: eliminated.clone(),
                                        log_entries: log_entries.clone(),
                                        spell_costs: p_spell_costs,
                                        legal_actions_by_object: p_by_object,
                                        derived: derive_views(pstate),
                                    });
                                }
                            }
                        }
                    }

                    // Broadcast AI follow-up results with delays
                    for (i, result) in ai_results.iter().enumerate() {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let (
                            ai_raw_state,
                            ai_events,
                            ai_legal,
                            ai_log_entries,
                            ai_auto_pass,
                            ai_spell_costs,
                            ai_by_object,
                        ) = result;
                        let is_last = i == ai_results.len() - 1;

                        // Filter AI state per-player outside the lock
                        let ai_filtered: Vec<(PlayerId, GameState)> = (0..player_count)
                            .map(|j| {
                                let pid = PlayerId(j);
                                (pid, server_core::filter_state_for_player(ai_raw_state, pid))
                            })
                            .collect();

                        let ai_actors = ai_raw_state.waiting_for.acting_players();
                        let conns = connections.lock().await;
                        if let Some(players) = conns.get(&game_code) {
                            for (pid, pstate) in &ai_filtered {
                                if let Some(s) = players.get(pid) {
                                    let is_actor = ai_actors.contains(pid);
                                    let player_legals = if is_last && is_actor {
                                        ai_legal.clone()
                                    } else {
                                        vec![]
                                    };
                                    let p_auto_pass = if is_last && is_actor {
                                        *ai_auto_pass
                                    } else {
                                        false
                                    };
                                    let p_spell_costs = if is_last && is_actor {
                                        ai_spell_costs.clone()
                                    } else {
                                        HashMap::new()
                                    };
                                    let p_by_object = if is_last && is_actor {
                                        ai_by_object.clone()
                                    } else {
                                        HashMap::new()
                                    };
                                    let _ = s.send(ServerMessage::StateUpdate {
                                        state: pstate.clone(),
                                        events: ai_events.clone(),
                                        legal_actions: player_legals,
                                        auto_pass_recommended: p_auto_pass,
                                        eliminated_players: eliminated.clone(),
                                        log_entries: ai_log_entries.clone(),
                                        spell_costs: p_spell_costs,
                                        legal_actions_by_object: p_by_object,
                                        derived: derive_views(pstate),
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let msg = ServerMessage::ActionRejected { reason: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::Reconnect {
            game_code,
            player_token,
        } => {
            info!(game = %game_code, "Reconnect attempt");

            // Determine game phase and handle reconnect in a single lock
            // to avoid TOCTOU races (game could fill between check and action).
            enum ReconnectOutcome {
                HostingOk {
                    player: PlayerId,
                    slot_info: Vec<server_core::PlayerSlotInfo>,
                },
                InGame {
                    player: PlayerId,
                    game_started_msg: Box<ServerMessage>,
                    ai_result: Option<Box<ActionResult>>,
                },
                Err(String),
            }

            let outcome = {
                let mut mgr = state.lock().await;
                let is_waiting = mgr
                    .sessions
                    .get(&game_code)
                    .map(|s| s.is_pregame())
                    .unwrap_or(false);

                if is_waiting {
                    // Hosting reconnect: game exists but hasn't started yet.
                    // Scope session borrow to avoid conflicting with reconnect manager.
                    let session_result = mgr.sessions.get_mut(&game_code).map(|session| {
                        let player = session.player_for_token(&player_token);
                        if let Some(p) = player {
                            session.connected[p.0 as usize] = true;
                            let slot_info = session.player_slot_info();
                            Ok((p, slot_info))
                        } else {
                            Err("Invalid player token".to_string())
                        }
                    });
                    match session_result {
                        Some(Ok((player, slot_info))) => {
                            mgr.reconnect.remove_disconnect(&game_code, player);
                            ReconnectOutcome::HostingOk { player, slot_info }
                        }
                        Some(Err(e)) => ReconnectOutcome::Err(e),
                        None => ReconnectOutcome::Err(format!("Game not found: {}", game_code)),
                    }
                } else {
                    // In-game reconnect: game is full and started
                    match mgr.handle_reconnect(&game_code, &player_token) {
                        Ok(_filtered_state) => {
                            let session = mgr.sessions.get_mut(&game_code).unwrap();
                            let player = session.player_for_token(&player_token).unwrap();
                            let ai_results = session.run_ai();
                            let ai_result = ai_results.last().cloned().map(Box::new);
                            if ai_result.is_some() {
                                persist_session_async(game_db, &game_code, session);
                            }
                            let game_started_msg =
                                build_game_started_message(session, player, None);
                            ReconnectOutcome::InGame {
                                player,
                                game_started_msg: Box::new(game_started_msg),
                                ai_result,
                            }
                        }
                        Err(e) => ReconnectOutcome::Err(e),
                    }
                }
            }; // lock dropped

            match outcome {
                ReconnectOutcome::HostingOk { player, slot_info } => {
                    info!(game = %game_code, player = ?player, "hosting reconnect succeeded");
                    identity.set_session(game_code.clone(), player, player_token.clone());

                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(game_code.clone())
                            .or_default()
                            .insert(player, tx.clone());
                    }

                    // Re-send GameCreated so the client resumes hosting state
                    let msg = ServerMessage::GameCreated {
                        game_code,
                        player_token,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // Send current room state
                    let slots_msg = ServerMessage::PlayerSlotsUpdate { slots: slot_info };
                    let _ = tx.send(slots_msg);
                }

                ReconnectOutcome::InGame {
                    player,
                    game_started_msg,
                    ai_result,
                } => {
                    info!(game = %game_code, player = ?player, "reconnect succeeded");
                    identity.set_session(game_code.clone(), player, player_token);

                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(game_code.clone())
                            .or_default()
                            .insert(player, tx.clone());

                        // Notify all other players about the reconnection
                        let reconnect_msg = ServerMessage::OpponentReconnected {
                            player: Some(player),
                        };
                        if let Some(game_conns) = conns.get(&game_code) {
                            for (&pid, sender) in game_conns.iter() {
                                if pid != player {
                                    let _ = sender.send(reconnect_msg.clone());
                                }
                            }
                        }
                    }

                    if let Ok(json) = serde_json::to_string(&game_started_msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    if let Some(result) = ai_result {
                        let conns = connections.lock().await;
                        if let Some(game_conns) = conns.get(&game_code) {
                            for (&pid, sender) in game_conns.iter() {
                                if pid != player {
                                    let _ = sender.send(build_state_update_message(&result, pid));
                                }
                            }
                        }
                    }
                }

                ReconnectOutcome::Err(e) => {
                    error!(game = %game_code, error = %e, "reconnect failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::SubscribeLobby | ClientMessage::UnsubscribeLobby => {
            // Mode-agnostic: lobby (un)subscription behaves identically on Full
            // and LobbyOnly servers, so the broker is the single authority for
            // both. AddSubscriber/RemoveSubscriber + the LobbyUpdate snapshot +
            // PlayerCount come back as ordered outbounds.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            public,
            password,
            timer_seconds,
            player_count: requested_player_count,
            match_config,
            ai_seats,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            start_when_full,
        } => {
            info!(
                display_name = %display_name,
                public = public,
                has_password = password.is_some(),
                timer = ?timer_seconds,
                deck_size = deck.main_deck.len(),
                ai_seats = ai_seats.len(),
                has_peer_id = host_peer_id.as_deref().is_some_and(|s| !s.is_empty()),
                "CreateGameWithSettings"
            );

            // --- Lobby-only broker path ------------------------------
            //
            // In this mode the server doesn't run a game — it only publishes
            // the host's PeerJS peer ID so guests can dial them directly. The
            // broker owns peer-id validation, re-registration cleanup, the
            // capacity cap, registration, GameCreated reply, and the public
            // LobbyGameAdded fan-out (in order). Deck data, AI seats, and
            // format-legality are host-authoritative and irrelevant here.
            if matches!(mode, ServerMode::LobbyOnly) {
                dispatch_broker_msg(
                    lobby_broker::LobbyClientMessage::CreateGameWithSettings {
                        deck: deck.clone(),
                        display_name: display_name.clone(),
                        public,
                        password: password.clone(),
                        timer_seconds,
                        player_count: requested_player_count,
                        match_config,
                        format_config: format_config.clone(),
                        room_name: room_name.clone(),
                        host_peer_id: host_peer_id.clone(),
                        draft_metadata: draft_metadata.clone(),
                        start_when_full,
                    },
                    lobby,
                    lobby_subscribers,
                    player_count,
                    tx,
                    identity,
                )
                .await;
                return;
            }

            {
                let mgr = state.lock().await;
                if mgr.sessions.len() >= MAX_GAMES {
                    warn!(
                        limit = MAX_GAMES,
                        "max games reached, rejecting CreateGameWithSettings"
                    );
                    let msg = ServerMessage::Error {
                        message: "Server is at game capacity, please try again later".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }
            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(error = %e, "CreateGameWithSettings: deck resolve failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            // Validate player deck against the selected format
            if let Some(ref fc) = format_config {
                if let Err(reasons) = validate_name_deck_for_format(
                    db,
                    &deck.main_deck,
                    &deck.sideboard,
                    &deck.commander,
                    fc.format,
                    Some(match_config.match_type),
                ) {
                    let msg = ServerMessage::Error {
                        message: format!(
                            "Deck not legal for {}: {}",
                            fc.format.label(),
                            reasons.join("; ")
                        ),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            }

            let pc = requested_player_count.clamp(2, 6);
            let mut ai_requests = Vec::new();
            for seat in &ai_seats {
                if seat.seat_index == 0 || seat.seat_index >= pc {
                    continue;
                }
                let ai_deck_data = match &seat.deck {
                    Some(DeckChoice::DeckList(deck)) => deck.as_ref().clone(),
                    Some(DeckChoice::Named(name)) => {
                        server_core::starter_decks::find_starter_deck(name).unwrap_or_else(|| {
                            warn!(deck = %name, "unknown AI deck name, using random");
                            server_core::starter_decks::random_starter_deck()
                        })
                    }
                    Some(DeckChoice::Random) | None => match &seat.deck_name {
                        Some(name) if name.eq_ignore_ascii_case("random") => {
                            server_core::starter_decks::random_starter_deck()
                        }
                        Some(name) => server_core::starter_decks::find_starter_deck(name)
                            .unwrap_or_else(|| {
                                warn!(deck = %name, "unknown AI deck name, using random");
                                server_core::starter_decks::random_starter_deck()
                            }),
                        None => server_core::starter_decks::random_starter_deck(),
                    },
                };
                if let Some(ref fc) = format_config {
                    if let Err(reasons) = validate_name_deck_for_format(
                        db,
                        &ai_deck_data.main_deck,
                        &ai_deck_data.sideboard,
                        &ai_deck_data.commander,
                        fc.format,
                        Some(match_config.match_type),
                    ) {
                        let msg = ServerMessage::Error {
                            message: format!(
                                "AI deck for seat {} not legal for {}: {}",
                                seat.seat_index,
                                fc.format.label(),
                                reasons.join("; ")
                            ),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
                let ai_resolved = match resolve_deck(db, &ai_deck_data) {
                    Ok(d) => d,
                    Err(e) => {
                        error!(error = %e, "AI deck resolve failed, cloning host deck");
                        resolved.clone()
                    }
                };
                ai_requests.push((seat.seat_index, seat.difficulty, ai_resolved));
            }

            if !ai_requests.is_empty() && ai_requests.len() as u8 == pc - 1 {
                // --- AI game path: create, start, and run initial AI actions ---
                let (game_code, player_token, game_started_msg) = {
                    let mut mgr = state.lock().await;
                    let (game_code, player_token) = mgr.create_game_with_ai(
                        resolved,
                        display_name.clone(),
                        timer_seconds,
                        match_config,
                        ai_requests,
                        db.card_names(),
                        format_config.clone(),
                        db.as_ref(),
                    );

                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    session.run_ai();
                    let game_started_msg = build_game_started_message(session, PlayerId(0), None);

                    // Persist the AI game session
                    persist_session_async(game_db, &game_code, session);

                    (game_code, player_token, game_started_msg)
                }; // lock dropped

                identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

                {
                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(PlayerId(0), tx.clone());
                }

                // Send GameCreated, then GameStarted (no lobby registration for AI games)
                let created_msg = ServerMessage::GameCreated {
                    game_code: game_code.clone(),
                    player_token,
                };
                if let Ok(json) = serde_json::to_string(&created_msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                if let Ok(json) = serde_json::to_string(&game_started_msg) {
                    let _ = socket.send(Message::text(json)).await;
                }

                info!(game = %game_code, host = %display_name, "AI game started");
            } else {
                // --- Standard multiplayer path ---
                let mut mgr = state.lock().await;
                // Capture the format before `format_config` is consumed so we
                // can stamp it on the lobby entry below.
                let format_config_for_lobby = format_config.clone();
                let (game_code, player_token) = mgr.create_game_n_players(
                    resolved,
                    display_name.clone(),
                    timer_seconds,
                    pc,
                    match_config,
                    format_config,
                );
                info!(game = %game_code, host = %display_name, players = pc, "game created via lobby");

                if let Some(session) = mgr.sessions.get_mut(&game_code) {
                    session.start_when_full = start_when_full;
                    for (seat_index, difficulty, deck) in &ai_requests {
                        let seat = *seat_index as usize;
                        session.display_names[seat] = format!("AI ({difficulty:?})");
                        session.connected[seat] = true;
                        session.decks[seat] = Some(deck.clone());
                        let pid = PlayerId(*seat_index);
                        session.ai_seats.insert(pid);
                        let config = phase_ai::config::create_config_for_players(
                            *difficulty,
                            phase_ai::config::Platform::Native,
                            pc,
                        );
                        session.ai_configs.insert(pid, config);
                    }
                }

                identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

                let mut conns = connections.lock().await;
                conns
                    .entry(game_code.clone())
                    .or_default()
                    .insert(PlayerId(0), tx.clone());

                let mut lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby_mut();
                // Pull the client's advertised build identity from the
                // stored ClientHello. `client_hello` is guaranteed Some here
                // because the handshake gate at the top of this function
                // rejects any non-hello frame when it's None.
                let (host_version, host_build_commit) = identity
                    .client_hello
                    .as_ref()
                    .map(|h| (h.client_version.clone(), h.build_commit.clone()))
                    .unwrap_or_default();
                lob.register_game(
                    &game_code,
                    RegisterGameRequest {
                        host_name: display_name.clone(),
                        public,
                        password: password.clone(),
                        timer_seconds,
                        host_version,
                        host_build_commit,
                        // Initial count reflects the host plus any AI seats
                        // configured at creation time; further updates flow
                        // through `set_current_players` as guests join.
                        current_players: mgr
                            .sessions
                            .get(&game_code)
                            .map(|s| s.current_player_count())
                            .unwrap_or(1),
                        // Use the clamped `pc` (not the raw request) so the
                        // lobby listing's max_players matches the session's
                        // actual capacity. A hostile client sending
                        // `player_count: 100` would otherwise advertise
                        // "1/100 players" while the game ran with 6.
                        max_players: pc as u32,
                        format_config: format_config_for_lobby,
                        match_config,
                        // Trim then drop empty strings so the client can't
                        // smuggle a blank room_name that would render as an
                        // empty row title. `None` is the "use host name"
                        // fallback both here and in the client.
                        room_name: room_name
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        // Full-mode server runs the engine itself — no
                        // PeerJS peer is involved, so this stays empty.
                        host_peer_id: String::new(),
                        // Draft metadata is P2P-only for now; Full-mode
                        // servers don't host draft pods.
                        draft_metadata: None,
                    },
                    &SysEnv,
                );

                // Store lobby metadata on the session and persist to SQLite
                if let Some(session) = mgr.sessions.get_mut(&game_code) {
                    session.lobby_meta = Some(server_core::PersistedLobbyMeta {
                        host_name: display_name,
                        public,
                        password,
                        timer_seconds,
                        start_when_full,
                    });
                    persist_session_async(game_db, &game_code, session);
                }

                let msg = ServerMessage::GameCreated {
                    game_code: game_code.clone(),
                    player_token,
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }

                // Send initial slot state so host sees themselves in the room
                broadcast_player_slots(state, connections, &game_code).await;

                if public {
                    let games = lob.public_games();
                    if let Some(game) = games.into_iter().find(|g| g.game_code == game_code) {
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameAdded { game },
                        )
                        .await;
                    }
                }

                let count = player_count.load(Ordering::Relaxed);
                broadcast_player_count(lobby_subscribers, count).await;
            }
        }

        ClientMessage::LookupJoinTarget {
            game_code,
            password,
            reserve,
            display_name,
            release_reservation_token,
        } => {
            info!(game = %game_code, "LookupJoinTarget");

            if reject_joining_current_game(identity, &game_code, socket)
                .await
                .is_err()
            {
                return;
            }

            let mut reservation_token = None;
            let mut reservation_expires_at_ms = None;

            let info = {
                let lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby();

                let guest_commit = identity
                    .client_hello
                    .as_ref()
                    .map(|h| h.build_commit.as_str())
                    .unwrap_or("");
                let host_commit = lob.host_build_commit(&game_code).unwrap_or("");
                if let BuildCommitCheck::Reject { host, guest } =
                    check_build_commit(host_commit, guest_commit)
                {
                    warn!(game = %game_code, %host, %guest, "build mismatch — refusing lookup");
                    if let Ok(json) = serde_json::to_string(&ServerMessage::Error {
                        message: format!(
                            "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
                        ),
                    }) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                } else {
                    match lob.verify_password(&game_code, password.as_deref()) {
                        Ok(()) => match lob.join_target_info(&game_code) {
                            Some(info) => info,
                            None => {
                                let msg = ServerMessage::Error {
                                    message: format!("Game not found in lobby: {game_code}"),
                                };
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    let _ = socket.send(Message::text(json)).await;
                                }
                                return;
                            }
                        },
                        Err(e) if e == "password_required" => {
                            let msg = ServerMessage::PasswordRequired {
                                game_code: game_code.clone(),
                            };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                        Err(e) => {
                            warn!(game = %game_code, error = %e, "lookup password verification failed");
                            let msg = ServerMessage::Error { message: e };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    }
                }
            };

            if let Some(token) = release_reservation_token.as_deref() {
                if info.is_p2p {
                    let released = {
                        let mut lob = lobby.lock().await;
                        lob.lobby_mut().release_reservation(&game_code, token)
                    };
                    if released {
                        identity
                            .lobby_reservations
                            .retain(|(code, t)| code != &game_code || t != token);
                        let game = {
                            let lob = lobby.lock().await;
                            lob.lobby().public_game(&game_code)
                        };
                        if let Some(game) = game {
                            broadcast_to_lobby_subscribers(
                                lobby_subscribers,
                                ServerMessage::LobbyGameUpdated { game },
                            )
                            .await;
                        }
                    }
                } else {
                    let released = {
                        let mut mgr = state.lock().await;
                        mgr.release_reservation(&game_code, token)
                    };
                    if released {
                        identity
                            .seat_reservations
                            .retain(|(code, t)| code != &game_code || t != token);
                        broadcast_player_slots(state, connections, &game_code).await;
                        let updated = {
                            let current = {
                                let mgr = state.lock().await;
                                mgr.sessions
                                    .get(&game_code)
                                    .map(|session| session.current_player_count())
                            };
                            let mut lob_guard = lobby.lock().await;
                            let lob = lob_guard.lobby_mut();
                            if let Some(current) = current {
                                lob.set_current_players(&game_code, current, &SysEnv);
                            }
                            lob.public_game(&game_code)
                        };
                        if let Some(game) = updated {
                            broadcast_to_lobby_subscribers(
                                lobby_subscribers,
                                ServerMessage::LobbyGameUpdated { game },
                            )
                            .await;
                        }
                    }
                }
            }

            if info.max_players > 0 && info.current_players >= info.max_players {
                let msg = ServerMessage::Error {
                    message: format!("Game {game_code} is full"),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            if reserve {
                if info.is_p2p {
                    let reserve_result = {
                        let mut lob = lobby.lock().await;
                        lob.lobby_mut().reserve_seat(
                            &game_code,
                            display_name.unwrap_or_else(|| "Player".to_string()),
                            &SysEnv,
                        )
                    };
                    match reserve_result {
                        Ok(reservation) => {
                            reservation_token = Some(reservation.token.clone());
                            reservation_expires_at_ms = reservation.expires_at_ms;
                            identity
                                .lobby_reservations
                                .push((game_code.clone(), reservation.token));
                            let game = {
                                let lob = lobby.lock().await;
                                lob.lobby().public_game(&game_code)
                            };
                            if let Some(game) = game {
                                broadcast_to_lobby_subscribers(
                                    lobby_subscribers,
                                    ServerMessage::LobbyGameUpdated { game },
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            let msg = ServerMessage::Error { message: e };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    }
                } else {
                    let reserve_result = {
                        let mut mgr = state.lock().await;
                        mgr.reserve_seat(
                            &game_code,
                            display_name.unwrap_or_else(|| "Player".to_string()),
                            password.is_some(),
                        )
                    };
                    match reserve_result {
                        Ok(reservation) => {
                            reservation_token = Some(reservation.token.clone());
                            reservation_expires_at_ms = reservation.expires_at_ms;
                            identity
                                .seat_reservations
                                .push((game_code.clone(), reservation.token));
                            broadcast_player_slots(state, connections, &game_code).await;
                            let updated = {
                                let current = {
                                    let mgr = state.lock().await;
                                    mgr.sessions
                                        .get(&game_code)
                                        .map(|session| session.current_player_count())
                                };
                                let mut lob_guard = lobby.lock().await;
                                let lob = lob_guard.lobby_mut();
                                if let Some(current) = current {
                                    lob.set_current_players(&game_code, current, &SysEnv);
                                }
                                lob.public_game(&game_code)
                            };
                            if let Some(game) = updated {
                                broadcast_to_lobby_subscribers(
                                    lobby_subscribers,
                                    ServerMessage::LobbyGameUpdated { game },
                                )
                                .await;
                            }
                        }
                        Err(e) => {
                            let msg = ServerMessage::Error { message: e };
                            if let Ok(json) = serde_json::to_string(&msg) {
                                let _ = socket.send(Message::text(json)).await;
                            }
                            return;
                        }
                    }
                }
            }

            let msg = ServerMessage::JoinTargetInfo {
                game_code: game_code.clone(),
                is_p2p: info.is_p2p,
                format_config: info.format_config,
                match_config: info.match_config,
                player_count: info.max_players as u8,
                filled_seats: (info.current_players + u32::from(reservation_token.is_some()))
                    .min(info.max_players) as u8,
                reservation_token,
                reservation_expires_at_ms,
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
            info!(game = %game_code, is_p2p = info.is_p2p, "sent JoinTargetInfo");
        }

        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => {
            info!(game = %game_code, joiner = %display_name, "JoinGameWithPassword");

            if reject_joining_current_game(identity, &game_code, socket)
                .await
                .is_err()
            {
                return;
            }

            // --- Lobby-only broker path ------------------------------
            //
            // The broker runs the build-commit + password gates, the
            // not-brokerable / seat-full checks, reservation consumption, and
            // hands back PeerInfo so the guest can dial over PeerJS. No session
            // is created server-side. The deck is ignored — the host validates
            // guest decks over P2P once the connection is up.
            if matches!(mode, ServerMode::LobbyOnly) {
                dispatch_broker_msg(
                    lobby_broker::LobbyClientMessage::JoinGameWithPassword {
                        game_code: game_code.clone(),
                        deck: deck.clone(),
                        display_name: display_name.clone(),
                        password: password.clone(),
                        reservation_token: reservation_token.clone(),
                    },
                    lobby,
                    lobby_subscribers,
                    player_count,
                    tx,
                    identity,
                )
                .await;
                return;
            }

            {
                let lob_guard = lobby.lock().await;
                let lob = lob_guard.lobby();

                // Build-commit gate: see `check_build_commit` for the
                // policy. If both host and guest publish commits and they
                // differ, the guest is running a different engine than the
                // host and joining would diverge GameState on resolution.
                let guest_commit = identity
                    .client_hello
                    .as_ref()
                    .map(|h| h.build_commit.as_str())
                    .unwrap_or("");
                let host_commit = lob.host_build_commit(&game_code).unwrap_or("");
                if let BuildCommitCheck::Reject { host, guest } =
                    check_build_commit(host_commit, guest_commit)
                {
                    warn!(game = %game_code, %host, %guest, "build mismatch — refusing join");
                    let msg = ServerMessage::Error {
                        message: format!(
                            "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
                        ),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

                match lob.verify_password(&game_code, password.as_deref()) {
                    Ok(()) => {}
                    Err(e) if e == "password_required" => {
                        info!(game = %game_code, "password required, prompting client");
                        let msg = ServerMessage::PasswordRequired {
                            game_code: game_code.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                    Err(e) => {
                        warn!(game = %game_code, error = %e, "password verification failed");
                        let msg = ServerMessage::Error { message: e };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
            }

            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGameWithPassword: deck resolve failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            enum JoinOutcome {
                Waiting {
                    player_token: String,
                    joiner: PlayerId,
                    slot_info: Vec<server_core::PlayerSlotInfo>,
                    current_count: u32,
                    filtered_state: Box<engine::types::game_state::GameState>,
                },
                Started {
                    player_token: String,
                    joiner: PlayerId,
                    public_before: bool,
                },
            }

            let join_outcome = {
                let mut mgr = state.lock().await;
                match mgr.join_game_with_name_and_reservation(
                    &game_code,
                    resolved,
                    display_name,
                    reservation_token.clone(),
                ) {
                    Ok((player_token, filtered_state)) => {
                        mgr.set_card_names(&game_code, db.card_names());
                        let session = mgr.sessions.get_mut(&game_code).unwrap();
                        let joiner = session.player_for_token(&player_token).unwrap();
                        info!(game = %game_code, player = ?joiner, "player joined via lobby");

                        if let Some(token) = reservation_token.as_deref() {
                            identity
                                .seat_reservations
                                .retain(|(code, t)| code != &game_code || t != token);
                        }

                        let should_start = session.is_full() && session.start_when_full;
                        let public_before =
                            session.lobby_meta.as_ref().is_some_and(|meta| meta.public);
                        if should_start {
                            session.start_game(db.as_ref());
                        }

                        // Persist updated session (now has the new player)
                        persist_session_async(game_db, &game_code, session);

                        if should_start {
                            Ok(JoinOutcome::Started {
                                player_token,
                                joiner,
                                public_before,
                            })
                        } else {
                            Ok(JoinOutcome::Waiting {
                                player_token,
                                joiner,
                                slot_info: session.player_slot_info(),
                                current_count: session.current_player_count(),
                                filtered_state: Box::new(filtered_state),
                            })
                        }
                    }
                    Err(e) => Err(e),
                }
            };

            match join_outcome {
                Ok(JoinOutcome::Waiting {
                    player_token,
                    joiner,
                    slot_info,
                    current_count,
                    filtered_state,
                }) => {
                    let filtered_state = *filtered_state;
                    identity.set_session(game_code.clone(), joiner, player_token);

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    // Notify all connected players about the updated room state
                    let slots_msg = ServerMessage::PlayerSlotsUpdate { slots: slot_info };
                    if let Some(players) = conns.get(&game_code) {
                        for sender in players.values() {
                            let _ = sender.send(slots_msg.clone());
                        }
                    }

                    let updated = {
                        let mut lob_guard = lobby.lock().await;
                        let lob = lob_guard.lobby_mut();
                        lob.set_current_players(&game_code, current_count, &SysEnv);
                        lob.public_game(&game_code)
                    };
                    if let Some(game) = updated {
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameUpdated { game },
                        )
                        .await;
                    }

                    let derived = derive_views(&filtered_state);
                    let msg = ServerMessage::StateUpdate {
                        state: filtered_state,
                        events: vec![],
                        legal_actions: vec![],
                        auto_pass_recommended: false,
                        eliminated_players: vec![],
                        log_entries: vec![],
                        spell_costs: HashMap::new(),
                        legal_actions_by_object: HashMap::new(),
                        derived,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    let count = player_count.load(Ordering::Relaxed);
                    broadcast_player_count(lobby_subscribers, count).await;
                }
                Ok(JoinOutcome::Started {
                    player_token,
                    joiner,
                    public_before,
                }) => {
                    identity.set_session(game_code.clone(), joiner, player_token);

                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(game_code.clone())
                            .or_default()
                            .insert(joiner, tx.clone());
                    }

                    let removed = {
                        let mut lob_guard = lobby.lock().await;
                        let lob = lob_guard.lobby_mut();
                        let existed = lob.has_game(&game_code);
                        lob.unregister_game(&game_code);
                        existed
                    };
                    if removed && public_before {
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameRemoved {
                                game_code: game_code.clone(),
                            },
                        )
                        .await;
                    }
                    broadcast_game_started(state, connections, game_db, &game_code).await;
                }
                Err(e) => {
                    error!(game = %game_code, error = %e, "JoinGameWithPassword failed");
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::Concede => {
            let game_code = match &identity.game_code {
                Some(c) => c.clone(),
                None => {
                    let msg = ServerMessage::Error {
                        message: "Not in a game".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };
            let player_id = match identity.player_id {
                Some(p) => p,
                None => return,
            };

            info!(game = %game_code, player = ?player_id, "player conceded");

            let conceded_msg = ServerMessage::Conceded { player: player_id };
            // In 2-player, the opponent wins. In multiplayer, game continues unless only 1 remains.
            let mgr_ref = state.lock().await;
            let winner = if let Some(session) = mgr_ref.sessions.get(&game_code) {
                let living: Vec<_> = session
                    .state
                    .players
                    .iter()
                    .filter(|p| p.id != player_id && !p.is_eliminated)
                    .map(|p| p.id)
                    .collect();
                if living.len() == 1 {
                    Some(living[0])
                } else {
                    None
                }
            } else {
                None
            };
            drop(mgr_ref);

            info!(game = %game_code, winner = ?winner, reason = "concession", "game over");

            let game_over_msg = ServerMessage::GameOver {
                winner,
                reason: "Opponent conceded".to_string(),
            };

            let conns = connections.lock().await;
            if let Some(players) = conns.get(&game_code) {
                for sender in players.values() {
                    let _ = sender.send(conceded_msg.clone());
                    let _ = sender.send(game_over_msg.clone());
                }
            }
            drop(conns);

            // Auto-report draft match result if this game belongs to a draft
            report_draft_game_over(draft_state, connections, &game_code, winner).await;

            let mut mgr = state.lock().await;
            mgr.sessions.remove(&game_code);
            delete_session_async(game_db, &game_code);
        }

        ClientMessage::SpectatorJoin { game_code } => {
            debug!(game = %game_code, "spectator join request");
            // Spectator support is planned but not yet implemented
            let msg = ServerMessage::Error {
                message: "Spectator mode not yet available".to_string(),
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
        }

        ClientMessage::Emote { emote } => {
            let game_code = match &identity.game_code {
                Some(c) => c.clone(),
                None => return,
            };
            let player_id = match identity.player_id {
                Some(p) => p,
                None => return,
            };

            debug!(game = %game_code, player = ?player_id, emote = %emote, "emote");
            let msg = ServerMessage::Emote {
                from_player: player_id,
                emote,
            };

            // Send emote to all other players in the game
            let conns = connections.lock().await;
            if let Some(game_conns) = conns.get(&game_code) {
                for (&pid, sender) in game_conns.iter() {
                    if pid != player_id {
                        let _ = sender.send(msg.clone());
                    }
                }
            }
        }

        ClientMessage::Ping { .. } => {
            // Mode-agnostic keepalive: the broker is the single authority for
            // the Pong reply on both Full and LobbyOnly servers.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::SeatMutate { mutation } => {
            if matches!(mode, ServerMode::LobbyOnly) {
                let msg = ServerMessage::Error {
                    message: "Seat mutations are not available on lobby-only servers.".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }
            if require_host(identity, socket).await.is_err() {
                return;
            }

            let Some(game_code) = identity.game_code.clone() else {
                return;
            };

            let (slot_info, kicked_players, started, current_players, max_players, public_before) = {
                let mut mgr = state.lock().await;
                let Some(session) = mgr.sessions.get_mut(&game_code) else {
                    let msg = ServerMessage::Error {
                        message: format!("Game not found: {game_code}"),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                };

                let public_before = session.lobby_meta.as_ref().is_some_and(|meta| meta.public);
                let mut seat_state = session.seat_state();
                let delta_result = {
                    let resolver = ServerDeckResolver { db: db.as_ref() };
                    let ctx = ReducerCtx {
                        platform: phase_ai::config::Platform::Native,
                        deck_resolver: &resolver,
                    };
                    seat_reducer::apply(&mut seat_state, mutation, &ctx)
                };
                let delta = match delta_result {
                    Ok(delta) => delta,
                    Err(err) => {
                        let msg = ServerMessage::Error {
                            message: format!("Seat mutation failed: {err:?}"),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                };

                let kicked_players = delta
                    .invalidated_tokens
                    .iter()
                    .filter_map(|token| {
                        session
                            .player_for_token(token)
                            .map(|pid| (pid, token.clone()))
                    })
                    .collect::<Vec<_>>();

                session.apply_seat_delta(seat_state, &delta, db.as_ref());
                let started = delta.now_started
                    || (session.is_full() && session.start_when_full && session.is_pregame());
                if started {
                    session.start_game(db.as_ref());
                }
                let slot_info = session.player_slot_info();
                let current_players = session.current_player_count();
                let max_players = session.player_count;
                persist_session_async(game_db, &game_code, session);
                (
                    slot_info,
                    kicked_players,
                    started,
                    current_players,
                    max_players,
                    public_before,
                )
            };

            {
                let mut conns = connections.lock().await;
                if let Some(players) = conns.get_mut(&game_code) {
                    for (pid, _) in &kicked_players {
                        if let Some(sender) = players.remove(pid) {
                            let _ = sender.send(ServerMessage::Error {
                                message: "You were removed from the room by the host.".to_string(),
                            });
                        }
                    }

                    let msg = ServerMessage::PlayerSlotsUpdate {
                        slots: slot_info.clone(),
                    };
                    for sender in players.values() {
                        let _ = sender.send(msg.clone());
                    }
                }
            }

            if started {
                let removed = {
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    let existed = lob.has_game(&game_code);
                    lob.unregister_game(&game_code);
                    existed
                };
                if removed && public_before {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameRemoved {
                            game_code: game_code.clone(),
                        },
                    )
                    .await;
                }
                broadcast_game_started(state, connections, game_db, &game_code).await;
            } else {
                let updated = {
                    let mut lob_guard = lobby.lock().await;
                    let lob = lob_guard.lobby_mut();
                    lob.set_current_players(&game_code, current_players, &SysEnv);
                    lob.set_max_players(&game_code, max_players);
                    lob.public_game(&game_code)
                };
                if let Some(game) = updated {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameUpdated { game },
                    )
                    .await;
                }
            }
        }

        ClientMessage::UpdateLobbyMetadata { .. } => {
            // LobbyOnly-exclusive (rejected in Full mode by reject_if_disabled).
            // The broker owns the ownership check, reservation consumption,
            // count/max updates, and the LobbyGameUpdated fan-out.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }

        ClientMessage::CreateDraftWithSettings {
            display_name,
            set_code,
            kind,
            public,
            password,
            timer_seconds,
            tournament_format,
            pod_policy,
            pod_size,
        } => {
            info!(
                display_name = %display_name,
                set_code = %set_code,
                kind = ?kind,
                public,
                pod_size,
                "CreateDraftWithSettings"
            );

            if !draft_pools.contains_set(&set_code) {
                let msg = ServerMessage::DraftActionRejected {
                    reason: format!("No draft pool data for set: {set_code}"),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let config = draft_core::types::DraftConfig {
                source: draft_core::types::DraftSource::Set {
                    code: set_code.clone(),
                },
                set_code: set_code.clone(),
                kind,
                pod_size,
                cards_per_pack: 14,
                pack_count: 3,
                min_deck_size: 40,
                addable_cards: draft_core::types::DeckAddableCards::standard_basics(),
                rng_seed: rand::random(),
                tournament_format,
                pod_policy,
                spectator_visibility: draft_core::types::SpectatorVisibility::default(),
            };

            let (draft_code, player_token, seat_index) = {
                let mut mgr = draft_state.lock().await;
                mgr.create_draft(config, display_name.clone())
            };

            identity.draft_code = Some(draft_code.clone());
            identity.draft_seat = Some(seat_index as usize);
            identity.draft_token = Some(player_token.clone());

            // Register this connection in the connections map under draft_code
            {
                let mut conns = connections.lock().await;
                conns
                    .entry(draft_code.clone())
                    .or_default()
                    .insert(PlayerId(seat_index), tx.clone());
            }

            // Register in lobby so draft appears in the lobby list
            {
                let (host_version, host_build_commit) = identity
                    .client_hello
                    .as_ref()
                    .map(|h| (h.client_version.clone(), h.build_commit.clone()))
                    .unwrap_or_default();
                let mut lob_guard = lobby.lock().await;
                lob_guard.lobby_mut().register_game(
                    &draft_code,
                    RegisterGameRequest {
                        host_name: display_name.clone(),
                        public,
                        password,
                        timer_seconds,
                        host_version,
                        host_build_commit,
                        current_players: 1,
                        max_players: pod_size as u32,
                        format_config: None,
                        match_config: Default::default(),
                        room_name: None,
                        host_peer_id: String::new(),
                        draft_metadata: Some(server_core::protocol::DraftLobbyMetadata {
                            set_code,
                            draft_kind: format!("{kind:?}"),
                            cube_name: None,
                        }),
                    },
                    &SysEnv,
                );
            }

            let msg = ServerMessage::DraftCreated {
                draft_code: draft_code.clone(),
                player_token,
                seat_index,
            };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }

            if public {
                let game = {
                    let lob = lobby.lock().await;
                    lob.lobby().public_game(&draft_code)
                };
                if let Some(game) = game {
                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameAdded { game },
                    )
                    .await;
                }
            }

            info!(draft = %draft_code, host = %display_name, "draft created");
        }

        ClientMessage::JoinDraftWithPassword {
            draft_code,
            display_name,
            password,
        } => {
            info!(draft = %draft_code, joiner = %display_name, "JoinDraftWithPassword");

            let result = {
                let mut mgr = draft_state.lock().await;
                mgr.join_draft(&draft_code, display_name.clone(), password.as_deref())
            };

            match result {
                Ok((player_token, seat_index, view)) => {
                    identity.draft_code = Some(draft_code.clone());
                    identity.draft_seat = Some(seat_index as usize);
                    identity.draft_token = Some(player_token.clone());

                    // Register this connection in the connections map under draft_code
                    {
                        let mut conns = connections.lock().await;
                        conns
                            .entry(draft_code.clone())
                            .or_default()
                            .insert(PlayerId(seat_index), tx.clone());
                    }

                    // Update lobby seats_filled count
                    {
                        let mgr = draft_state.lock().await;
                        if let Some(session) = mgr.sessions.get(&draft_code) {
                            let filled = session
                                .player_tokens
                                .iter()
                                .filter(|t| !t.is_empty())
                                .count();
                            let mut lob_guard = lobby.lock().await;
                            let lob = lob_guard.lobby_mut();
                            lob.set_current_players(&draft_code, filled as u32, &SysEnv);
                            if let Some(game) = lob.public_game(&draft_code) {
                                broadcast_to_lobby_subscribers(
                                    lobby_subscribers,
                                    ServerMessage::LobbyGameUpdated { game },
                                )
                                .await;
                            }
                        }
                    }

                    let msg = ServerMessage::DraftJoined {
                        draft_code,
                        player_token,
                        seat_index,
                        view,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::DraftAction { draft_code, action } => {
            let token = match &identity.draft_token {
                Some(t) => t.clone(),
                None => {
                    let msg = ServerMessage::DraftActionRejected {
                        reason: "Not in a draft session".to_string(),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            debug!(draft = %draft_code, action = ?action, "DraftAction");

            if let Some(reason) = client_forbidden_draft_action_reason(&action) {
                let msg = ServerMessage::DraftActionRejected { reason };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            // Check if this is a StartDraft action (triggers timer)
            let is_start = matches!(action, draft_core::types::DraftAction::StartDraft);
            let pack_generator = if is_start {
                match draft_pack_generator_for_start(draft_state, draft_pools, &draft_code).await {
                    Ok(generator) => Some(generator),
                    Err(reason) => {
                        let msg = ServerMessage::DraftActionRejected { reason };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }
            } else {
                None
            };

            let result = {
                let mut mgr = draft_state.lock().await;
                mgr.handle_draft_action(
                    &draft_code,
                    &token,
                    action,
                    pack_generator
                        .as_ref()
                        .map(|generator| generator as &dyn draft_core::pack_source::PackSource),
                )
            };

            match result {
                Ok(views) => {
                    // Broadcast DraftStateUpdate to all connected sockets in the pod
                    broadcast_draft_views(&draft_code, &views, connections, draft_state).await;

                    // If draft just started, spawn pick timer
                    if is_start {
                        spawn_pick_timer(
                            draft_state.clone(),
                            connections.clone(),
                            draft_code.clone(),
                            75, // default pick timer seconds
                        );
                    }

                    // Check if pairings were generated (status transitioned to MatchInProgress)
                    {
                        let mgr = draft_state.lock().await;
                        if let Some(session) = mgr.sessions.get(&draft_code) {
                            if session.session.status
                                == draft_core::types::DraftStatus::MatchInProgress
                            {
                                // Pairings generated — send DraftMatchStart to each paired player
                                // (This is a simplified stub; full game session spawning
                                // requires deck resolution and session creation which
                                // depends on the deckbuilding flow from Plan 03/04)
                                info!(
                                    draft = %draft_code,
                                    pairings = session.session.pairings.len(),
                                    "pairings generated — match spawning deferred to Plan 03/04"
                                );
                            }
                        }
                    }

                    // Persist draft session after mutation
                    persist_draft_session_async(game_db, &draft_code, draft_state).await;

                    // Broadcast to spectators
                    broadcast_draft_spectator_views(&draft_code, draft_state, draft_spectators)
                        .await;
                }
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::ReconnectDraft {
            draft_code,
            player_token,
        } => {
            info!(draft = %draft_code, "ReconnectDraft attempt");

            let result = {
                let mut mgr = draft_state.lock().await;
                mgr.handle_reconnect(&draft_code, &player_token)
            };

            match result {
                Ok(view) => {
                    // Restore identity
                    let seat = {
                        let mgr = draft_state.lock().await;
                        mgr.sessions
                            .get(&draft_code)
                            .and_then(|s| s.seat_for_token(&player_token))
                    };
                    if let Some(seat) = seat {
                        identity.draft_code = Some(draft_code.clone());
                        identity.draft_seat = Some(seat);
                        identity.draft_token = Some(player_token);
                    }

                    let msg = ServerMessage::DraftStateUpdate { view };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    info!(draft = %draft_code, "draft reconnect succeeded");
                }
                Err(reason) => {
                    let msg = ServerMessage::DraftActionRejected { reason };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }

        ClientMessage::SpectateDraft { draft_code } => {
            let drafts = draft_state.lock().await;
            if let Some(session) = drafts.sessions.get(&draft_code) {
                // Derive visibility from session config (host-configured, per D-07)
                let visibility = session.config.spectator_visibility;
                let view = draft_core::view::filter_for_spectator(&session.session, visibility);
                // Record spectator identity (T-60-09: prevents spectator from sending DraftAction)
                identity.spectator_draft_code = Some(draft_code.clone());
                identity.spectator_visibility = Some(visibility);
                // Register spectator sender for live broadcasts
                {
                    let mut specs = draft_spectators.lock().await;
                    specs
                        .entry(draft_code.clone())
                        .or_default()
                        .push((visibility, tx.clone()));
                }
                let msg = ServerMessage::DraftSpectatorView { view };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                info!(draft = %draft_code, ?visibility, "spectator connected to draft");
            } else {
                let msg = ServerMessage::Error {
                    message: "Draft not found".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
            }
        }

        ClientMessage::UnregisterLobby { .. } => {
            // LobbyOnly-exclusive (rejected in Full mode by reject_if_disabled).
            // The broker owns the ownership check, removal, LobbyGameRemoved
            // fan-out, and clearing the host-game ownership stamp.
            dispatch_broker(
                &client_msg,
                lobby,
                lobby_subscribers,
                player_count,
                tx,
                identity,
            )
            .await;
        }
    }
}

#[cfg(test)]
mod mode_gate_tests {
    use super::*;
    use engine::types::actions::GameAction;
    use server_core::protocol::DeckData;

    fn deck() -> DeckData {
        DeckData {
            main_deck: vec!["Forest".into()],
            sideboard: vec![],
            commander: vec![],
        }
    }

    #[test]
    fn lobby_only_rejects_game_state_messages() {
        let disabled: Vec<ClientMessage> = vec![
            ClientMessage::CreateGame { deck: deck() },
            ClientMessage::JoinGame {
                game_code: "X".into(),
                deck: deck(),
            },
            ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            ClientMessage::Reconnect {
                game_code: "X".into(),
                player_token: "t".into(),
            },
            ClientMessage::Concede,
            ClientMessage::Emote { emote: "GG".into() },
            ClientMessage::SpectatorJoin {
                game_code: "X".into(),
            },
            ClientMessage::CreateDraftWithSettings {
                display_name: "A".into(),
                set_code: "TST".into(),
                kind: draft_core::types::DraftKind::Premier,
                public: true,
                password: None,
                timer_seconds: None,
                tournament_format: draft_core::types::TournamentFormat::Swiss,
                pod_policy: draft_core::types::PodPolicy::Competitive,
                pod_size: 8,
            },
            ClientMessage::JoinDraftWithPassword {
                draft_code: "X".into(),
                display_name: "B".into(),
                password: None,
            },
            ClientMessage::DraftAction {
                draft_code: "X".into(),
                action: draft_core::types::DraftAction::StartDraft,
            },
            ClientMessage::ReconnectDraft {
                draft_code: "X".into(),
                player_token: "t".into(),
            },
        ];
        for msg in disabled {
            assert!(
                reject_if_disabled(&msg, ServerMode::LobbyOnly).is_some(),
                "expected {msg:?} to be rejected in lobby-only mode"
            );
        }
    }

    #[test]
    fn lobby_only_allows_broker_and_lifecycle_messages() {
        let allowed: Vec<ClientMessage> = vec![
            ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            ClientMessage::SubscribeLobby,
            ClientMessage::UnsubscribeLobby,
            ClientMessage::Ping { timestamp: 0 },
            ClientMessage::UpdateLobbyMetadata {
                game_code: "X".into(),
                current_players: 2,
                max_players: 4,
                consumed_reservation_tokens: Vec::new(),
            },
            ClientMessage::UnregisterLobby {
                game_code: "X".into(),
            },
        ];
        for msg in allowed {
            assert!(
                reject_if_disabled(&msg, ServerMode::LobbyOnly).is_none(),
                "expected {msg:?} to be allowed in lobby-only mode"
            );
        }
    }

    #[test]
    fn full_mode_rejects_lobby_only_messages() {
        let msgs = vec![
            ClientMessage::UpdateLobbyMetadata {
                game_code: "X".into(),
                current_players: 2,
                max_players: 4,
                consumed_reservation_tokens: Vec::new(),
            },
            ClientMessage::UnregisterLobby {
                game_code: "X".into(),
            },
        ];
        for msg in msgs {
            assert!(
                reject_if_disabled(&msg, ServerMode::Full).is_some(),
                "expected {msg:?} to be rejected in full mode"
            );
        }
    }

    #[test]
    fn full_mode_allows_game_state_messages() {
        let msgs: Vec<ClientMessage> = vec![
            ClientMessage::CreateGame { deck: deck() },
            ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            ClientMessage::Concede,
            ClientMessage::Ping { timestamp: 0 },
            ClientMessage::CreateDraftWithSettings {
                display_name: "A".into(),
                set_code: "TST".into(),
                kind: draft_core::types::DraftKind::Premier,
                public: true,
                password: None,
                timer_seconds: None,
                tournament_format: draft_core::types::TournamentFormat::Swiss,
                pod_policy: draft_core::types::PodPolicy::Competitive,
                pod_size: 8,
            },
            ClientMessage::DraftAction {
                draft_code: "X".into(),
                action: draft_core::types::DraftAction::StartDraft,
            },
        ];
        for m in msgs {
            assert!(reject_if_disabled(&m, ServerMode::Full).is_none());
        }
    }
}

#[cfg(test)]
mod handshake_tests {
    use super::*;
    use engine::types::actions::GameAction;
    use server_core::protocol::DeckData;

    fn empty_identity() -> SocketIdentity {
        SocketIdentity {
            game_code: None,
            player_id: None,
            player_token: None,
            lobby_subscribed: false,
            session_span: None,
            client_hello: None,
            lobby_host_game: None,
            seat_reservations: Vec::new(),
            lobby_reservations: Vec::new(),
            draft_code: None,
            draft_seat: None,
            draft_token: None,
            spectator_draft_code: None,
            spectator_visibility: None,
        }
    }

    fn empty_deck() -> DeckData {
        DeckData {
            main_deck: vec!["Forest".into()],
            sideboard: vec![],
            commander: vec![],
        }
    }

    #[test]
    fn accepts_matching_client_hello() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::Accept(ClientHelloInfo {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
            })
        );
    }

    #[test]
    fn accepts_min_supported_protocol_below_current() {
        // Range hello gate: a client one version behind (e.g., release after
        // the server has rolled forward to preview) must still be admitted to
        // the lobby. Cross-version game interop is gated separately at join
        // boundaries (per-game protocol-version filtering, follow-up work).
        // `MIN_SUPPORTED_PROTOCOL < PROTOCOL_VERSION` is true by construction
        // (MIN derives from PROTOCOL_VERSION.saturating_sub(1)) whenever
        // PROTOCOL_VERSION > 0; no runtime assert needed.
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.10".into(),
                build_commit: "old1234".into(),
                protocol_version: MIN_SUPPORTED_PROTOCOL,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert!(matches!(outcome, HelloGateOutcome::Accept(_)));
    }

    #[test]
    fn rejects_client_hello_below_min_supported() {
        // Two versions behind is outside the supported window; reject.
        let too_old = MIN_SUPPORTED_PROTOCOL.saturating_sub(1);
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.0".into(),
                build_commit: "ancient1".into(),
                protocol_version: too_old,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: too_old,
                server: PROTOCOL_VERSION,
            }
        );
    }

    #[test]
    fn rejects_client_hello_with_zero_protocol_version() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: 0,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(
            outcome,
            HelloGateOutcome::RejectProtocol {
                client: 0,
                server: PROTOCOL_VERSION,
            }
        );
    }

    #[test]
    fn rejects_client_hello_with_future_protocol_version() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.2.0".into(),
                build_commit: "def5678".into(),
                protocol_version: PROTOCOL_VERSION + 1,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert!(matches!(outcome, HelloGateOutcome::RejectProtocol { .. }));
    }

    #[test]
    fn rejects_non_hello_frame_before_handshake() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::CreateGame { deck: empty_deck() },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::SubscribeLobby,
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::Ping { timestamp: 1 },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);
    }

    #[test]
    fn ignores_redundant_hello_after_accept() {
        let outcome = classify_hello_gate(
            true,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::IgnoreRedundantHello);
    }

    #[test]
    fn passes_through_regular_frames_after_handshake() {
        let outcome = classify_hello_gate(
            true,
            &ClientMessage::Action {
                action: GameAction::PassPriority,
            },
            MIN_SUPPORTED_PROTOCOL..=PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::PassThrough);
    }

    #[test]
    fn build_commit_allows_matching() {
        assert_eq!(
            check_build_commit("abc1234", "abc1234"),
            BuildCommitCheck::Allow
        );
    }

    #[test]
    fn build_commit_allows_when_either_side_is_empty() {
        // Restored sessions / legacy clients are treated as unknown.
        assert_eq!(check_build_commit("", "abc1234"), BuildCommitCheck::Allow);
        assert_eq!(check_build_commit("abc1234", ""), BuildCommitCheck::Allow);
        assert_eq!(check_build_commit("", ""), BuildCommitCheck::Allow);
    }

    #[test]
    fn build_commit_rejects_when_both_populated_and_different() {
        assert_eq!(
            check_build_commit("abc1234", "def5678"),
            BuildCommitCheck::Reject {
                host: "abc1234".into(),
                guest: "def5678".into(),
            }
        );
    }

    #[test]
    fn joining_current_game_is_rejected_by_helper() {
        let mut identity = empty_identity();
        identity.game_code = Some("GAME01".to_string());
        identity.player_id = Some(PlayerId(0));

        assert!(is_joining_current_game(&identity, "GAME01"));
        assert!(!is_joining_current_game(&identity, "GAME02"));

        let mut lobby_identity = empty_identity();
        lobby_identity.lobby_host_game = Some("GAME01".to_string());
        assert!(is_joining_current_game(&lobby_identity, "GAME01"));
        assert!(!is_joining_current_game(&lobby_identity, "GAME02"));
    }

    #[test]
    fn joining_without_active_game_is_allowed_by_helper() {
        let identity = empty_identity();
        assert!(!is_joining_current_game(&identity, "GAME01"));
    }

    // ------------------------------------------------------------------
    // GH #1254: MP wire-trust — client cannot forge another seat's
    // connection state via DraftAction::SetSeatConnected.
    // ------------------------------------------------------------------

    #[test]
    fn client_forbidden_draft_action_rejects_set_seat_connected() {
        // The forged payload: a malicious authenticated client passes
        // *another* seat's index. The handler currently discards the
        // token-resolved seat (`let _seat = ...` at draft_session.rs:247),
        // so the payload's `seat` would flow through unchecked without
        // this filter. Reject the variant outright — it's engine state
        // plumbing, not user intent.
        let action = draft_core::types::DraftAction::SetSeatConnected {
            seat: 3,
            connected: true,
        };
        let reason = client_forbidden_draft_action_reason(&action);
        assert!(
            reason.is_some(),
            "SetSeatConnected MUST be rejected when sent from a client"
        );
        let msg = reason.unwrap();
        assert!(
            msg.contains("server-internal"),
            "rejection reason should explain why: got {msg:?}"
        );
    }

    #[test]
    fn client_forbidden_draft_action_rejects_generate_pairings() {
        // Regression coverage: this rejection predates GH #1254 and must
        // continue to fire. The user-facing reason ("not available yet")
        // is distinct from SetSeatConnected ("server-internal"); both
        // are forbidden but for different reasons.
        let action = draft_core::types::DraftAction::GeneratePairings { round: 1 };
        let reason = client_forbidden_draft_action_reason(&action);
        assert!(reason.is_some());
        assert!(reason.unwrap().contains("not available yet"));
    }

    #[test]
    fn client_forbidden_draft_action_allows_legitimate_variants() {
        // Every variant that IS allowed from a client must return None.
        // If a new DraftAction variant lands and the helper's exhaustive
        // match doesn't handle it, this test fails at compile time on
        // the function — and the security-relevant decision is made
        // explicitly, not by default-allow.
        let allowed = [
            draft_core::types::DraftAction::StartDraft,
            draft_core::types::DraftAction::Pick {
                seat: 0,
                card_instance_id: "x".into(),
            },
            draft_core::types::DraftAction::SubmitDeck {
                seat: 0,
                main_deck: vec![],
            },
            draft_core::types::DraftAction::ReportMatchResult {
                match_id: "m1".into(),
                winner_seat: Some(0),
            },
            draft_core::types::DraftAction::AdvanceRound,
            draft_core::types::DraftAction::ReplaceSeatWithBot {
                seat: 1,
                name: None,
            },
        ];
        for action in allowed {
            assert!(
                client_forbidden_draft_action_reason(&action).is_none(),
                "expected {action:?} to be allowed from client"
            );
        }
    }
}
