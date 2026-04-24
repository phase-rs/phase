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
use axum::routing::get;
use axum::Router;
use clap::Parser;
use engine::ai_support::{
    auto_pass_recommended as engine_auto_pass, legal_actions_full as engine_legal_actions_full,
};
use engine::database::CardDatabase;
use engine::game::derived_views::derive_views;
use engine::game::{validate_deck_for_format, DeckCompatibilityRequest};
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use http::HeaderValue;
use seat_reducer::types::{DeckChoice, DeckResolver, ReducerCtx};
use server_core::lobby::{LobbyManager, RegisterGameRequest};
use server_core::protocol::{
    build_commit, ClientMessage, ServerMessage, ServerMode, PROTOCOL_VERSION,
};
use server_core::resolve_deck;
use server_core::session::SessionManager;
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, info_span, warn, Instrument};

type SharedState = Arc<Mutex<SessionManager>>;
type SharedConnections =
    Arc<Mutex<HashMap<String, HashMap<PlayerId, mpsc::UnboundedSender<ServerMessage>>>>>;
type SharedDb = Arc<CardDatabase>;
type SharedLobby = Arc<Mutex<LobbyManager>>;
type SharedLobbySubscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<ServerMessage>>>>;
type SharedPlayerCount = Arc<AtomicU32>;
type SharedGameDb = Arc<persistence::GameDb>;

/// Server's advertised role, selected at startup via `--lobby-only`. Copied
/// into every handler so the dispatch path can gate disabled messages in
/// lobby-only mode without re-parsing CLI state.
type Mode = ServerMode;

/// Server-wide limits to prevent resource exhaustion and abuse.
const MAX_CONNECTIONS: u32 = 200;
const MAX_GAMES: usize = 100;
/// Capacity cap for the lobby-only broker path. `LobbyManager` is otherwise
/// unbounded — without this gate an abusive client could pin an arbitrary
/// number of `LobbyGameMeta` entries in memory until the 5-minute
/// `check_expired` pass reaps them.
const MAX_LOBBY_ENTRIES: usize = 200;
const RATE_LIMIT_MESSAGES: u32 = 30;
const RATE_LIMIT_WINDOW_SECS: u64 = 1;
const MAX_WS_MESSAGE_BYTES: usize = 8 * 1024; // 8 KB

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
    server_protocol: u32,
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
            if *protocol_version != server_protocol {
                HelloGateOutcome::RejectProtocol {
                    client: *protocol_version,
                    server: server_protocol,
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

/// Outcome of the build-commit check on `JoinGameWithPassword`. The host's
/// and guest's commits must either both be populated and equal, or at least
/// one must be empty (restored session, legacy client) for the join to proceed.
#[derive(Debug, PartialEq, Eq)]
enum BuildCommitCheck {
    Allow,
    Reject { host: String, guest: String },
}

fn check_build_commit(host_commit: &str, guest_commit: &str) -> BuildCommitCheck {
    if !guest_commit.is_empty() && !host_commit.is_empty() && host_commit != guest_commit {
        BuildCommitCheck::Reject {
            host: host_commit.to_owned(),
            guest: guest_commit.to_owned(),
        }
    } else {
        BuildCommitCheck::Allow
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

        // Lobby-only-exclusive.
        ClientMessage::UpdateLobbyMetadata { .. } | ClientMessage::UnregisterLobby { .. } => {
            match mode {
                ServerMode::Full => Some(FULL_MODE_REJECTION),
                ServerMode::LobbyOnly => None,
            }
        }
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
    let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
    let lobby: SharedLobby = Arc::new(Mutex::new(LobbyManager::new()));
    let lobby_subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
    let player_count: SharedPlayerCount = Arc::new(AtomicU32::new(0));

    // Restore persisted game sessions from disk. In lobby-only mode the
    // server runs no engine, so persisted GameState snapshots can't be
    // replayed — skip the restore pass entirely and let SQLite ignore the
    // stale rows until operators clean them up manually.
    if matches!(mode, ServerMode::Full) {
        let card_names = db.card_names();
        match game_db.load_all() {
            Ok(persisted_games) => {
                let mut mgr = state.lock().await;
                let mut lob = lobby.lock().await;
                let mut restored = 0u32;

                for (game_code, json) in &persisted_games {
                    match serde_json::from_str::<server_core::PersistedSession>(json) {
                        Ok(ps) => {
                            let lobby_meta = ps.lobby_meta.clone();
                            let is_started = ps.game_started;
                            let session =
                                server_core::session::GameSession::from_persisted(ps, &card_names);

                            // Register all non-AI human players as disconnected
                            // to start the 120s grace period from now
                            for (i, token) in session.player_tokens.iter().enumerate() {
                                let pid = PlayerId(i as u8);
                                if !token.is_empty() && !session.ai_seats.contains(&pid) {
                                    mgr.reconnect.record_disconnect(&session.game_code, pid);
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
    }

    // Spawn background task for grace period and lobby expiry
    let bg_state = state.clone();
    let bg_connections = connections.clone();
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

            // Check lobby game expiry (5 minute timeout for waiting games)
            let expired_lobby = {
                let mut lob = bg_lobby.lock().await;
                lob.check_expired(300)
            };
            if !expired_lobby.is_empty() {
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
                for game_code in &expired_lobby {
                    let msg = ServerMessage::LobbyGameRemoved {
                        game_code: game_code.clone(),
                    };
                    for sub in subs.iter() {
                        let _ = sub.send(msg.clone());
                    }
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
    let shutdown_game_db = game_db.clone();

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .layer(cors)
        .with_state((
            state,
            connections,
            db,
            lobby,
            lobby_subscribers,
            player_count,
            game_db,
            mode,
        ));

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

type AppState = (
    SharedState,
    SharedConnections,
    SharedDb,
    SharedLobby,
    SharedLobbySubscribers,
    SharedPlayerCount,
    SharedGameDb,
    Mode,
);

async fn ws_handler(
    ws: WebSocketUpgrade,
    State((state, connections, db, lobby, lobby_subscribers, player_count, game_db, mode)): State<
        AppState,
    >,
) -> impl IntoResponse {
    let current = player_count.load(Ordering::Relaxed);
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
                state,
                connections,
                db,
                lobby,
                lobby_subscribers,
                player_count,
                game_db,
                mode,
            )
        })
        .into_response()
}

#[allow(clippy::too_many_arguments)]
async fn handle_socket(
    mut socket: WebSocket,
    state: SharedState,
    connections: SharedConnections,
    db: SharedDb,
    lobby: SharedLobby,
    lobby_subscribers: SharedLobbySubscribers,
    player_count: SharedPlayerCount,
    game_db: SharedGameDb,
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
                            &connections,
                            &db,
                            &lobby,
                            &lobby_subscribers,
                            &player_count,
                            &game_db,
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

    // In lobby-only mode, a host's socket disconnect is the signal that the
    // lobby row should be cleaned up — the server never tracked a
    // GameSession for them, so the `handle_disconnect` path above is a
    // no-op. Drop the lobby entry and broadcast the removal so any
    // subscribed clients update immediately. The 5-minute `check_expired`
    // pass is a fallback for cases where this branch doesn't fire (e.g.
    // process crash).
    if let Some(game_code) = identity.lobby_host_game.clone() {
        let removed = {
            let mut lob = lobby.lock().await;
            let existed = lob.has_game(&game_code);
            lob.unregister_game(&game_code);
            existed
        };
        if removed {
            info!(game = %game_code, "lobby host disconnected — lobby entry removed");
            broadcast_to_lobby_subscribers(
                &lobby_subscribers,
                ServerMessage::LobbyGameRemoved {
                    game_code: game_code.clone(),
                },
            )
            .await;
        }
    }

    if identity.lobby_subscribed {
        let mut subs = lobby_subscribers.lock().await;
        subs.retain(|s| !s.is_closed());
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

struct ServerDeckResolver<'a> {
    db: &'a CardDatabase,
}

impl DeckResolver for ServerDeckResolver<'_> {
    fn resolve(
        &self,
        choice: &DeckChoice,
    ) -> Result<engine::game::deck_loading::PlayerDeckPayload, String> {
        let deck = match choice {
            DeckChoice::Random => server_core::starter_decks::random_starter_deck(),
            DeckChoice::Named(name) => server_core::starter_decks::find_starter_deck(name)
                .ok_or_else(|| format!("Starter deck not found: {name}"))?,
        };
        server_core::resolve_deck(self.db, &deck)
    }
}

async fn broadcast_game_started(
    state: &SharedState,
    connections: &SharedConnections,
    game_db: &SharedGameDb,
    game_code: &str,
) {
    let (player_messages, ai_results, player_count) = {
        let mut mgr = state.lock().await;
        let Some(session) = mgr.sessions.get_mut(game_code) else {
            return;
        };

        let (legal_actions, spell_costs_all, by_object_all) =
            engine_legal_actions_full(&session.state);
        let auto_pass = engine_auto_pass(&session.state, &legal_actions);
        let actor = server_core::acting_player(&session.state);
        let player_names = session.display_names.clone();

        let player_messages = (0..session.player_count)
            .filter(|pid| !session.ai_seats.contains(&PlayerId(*pid)))
            .map(|pid| {
                let player = PlayerId(pid);
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
                let is_actor = actor == Some(player);
                let derived = derive_views(&filtered);
                (
                    player,
                    ServerMessage::GameStarted {
                        state: filtered,
                        your_player: player,
                        opponent_name,
                        player_names: player_names.clone(),
                        legal_actions: if is_actor {
                            legal_actions.clone()
                        } else {
                            Vec::new()
                        },
                        auto_pass_recommended: if is_actor { auto_pass } else { false },
                        spell_costs: if is_actor {
                            spell_costs_all.clone()
                        } else {
                            HashMap::new()
                        },
                        legal_actions_by_object: if is_actor {
                            by_object_all.clone()
                        } else {
                            HashMap::new()
                        },
                        derived,
                        player_token: None,
                    },
                )
            })
            .collect::<Vec<_>>();

        let ai_results = session.run_ai();
        let player_count = session.player_count;
        persist_session_async(game_db, game_code, session);
        (player_messages, ai_results, player_count)
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

    for result in ai_results {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let (
            raw_state,
            events,
            legal_actions,
            log_entries,
            auto_pass,
            spell_costs,
            legal_actions_by_object,
        ) = result;
        let actor = {
            let mgr = state.lock().await;
            mgr.sessions
                .get(game_code)
                .and_then(|session| server_core::acting_player(&session.state))
        };
        let filtered_states: Vec<(PlayerId, GameState)> = (0..player_count)
            .map(PlayerId)
            .map(|pid| (pid, server_core::filter_state_for_player(&raw_state, pid)))
            .collect();

        let conns = connections.lock().await;
        if let Some(players) = conns.get(game_code) {
            for (pid, pstate) in &filtered_states {
                if let Some(sender) = players.get(pid) {
                    let is_actor = actor == Some(*pid);
                    let _ = sender.send(ServerMessage::StateUpdate {
                        state: pstate.clone(),
                        events: events.clone(),
                        legal_actions: if is_actor {
                            legal_actions.clone()
                        } else {
                            Vec::new()
                        },
                        auto_pass_recommended: if is_actor { auto_pass } else { false },
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
                        derived: derive_views(pstate),
                    });
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

#[allow(clippy::too_many_arguments)]
async fn handle_client_message(
    client_msg: ClientMessage,
    socket: &mut WebSocket,
    state: &SharedState,
    connections: &SharedConnections,
    db: &SharedDb,
    lobby: &SharedLobby,
    lobby_subscribers: &SharedLobbySubscribers,
    player_count: &SharedPlayerCount,
    game_db: &SharedGameDb,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
    mode: Mode,
) {
    // Handshake gate: ClientHello must be the first message. See
    // `classify_hello_gate` for the full truth table.
    match classify_hello_gate(
        identity.client_hello.is_some(),
        &client_msg,
        PROTOCOL_VERSION,
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
            let msg = ServerMessage::Error {
                message: format!(
                    "Protocol version mismatch (client={client} server={server}). Please update."
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
                Ok((player_token, filtered_state)) => {
                    mgr.set_card_names(&game_code, db.card_names());
                    let session = mgr.sessions.get(&game_code).unwrap();
                    let joiner = session.player_for_token(&player_token).unwrap();
                    info!(game = %game_code, player = ?joiner, "player joined");
                    identity.set_session(game_code.clone(), joiner, player_token.clone());

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    // Only send GameStarted when the game is full (all seats claimed)
                    if session.is_full() {
                        let (legal_actions, spell_costs_all, by_object_all) =
                            engine_legal_actions_full(&session.state);
                        let auto_pass = engine_auto_pass(&session.state, &legal_actions);
                        let actor = server_core::acting_player(&session.state);
                        let player_names = session.display_names.clone();

                        // Send GameStarted to the joiner
                        let is_joiner_actor = actor == Some(joiner);
                        let joiner_legals = if is_joiner_actor {
                            legal_actions.clone()
                        } else {
                            vec![]
                        };
                        let derived_joiner = derive_views(&filtered_state);
                        let msg = ServerMessage::GameStarted {
                            state: filtered_state,
                            your_player: joiner,
                            opponent_name: None,
                            player_names: player_names.clone(),
                            legal_actions: joiner_legals,
                            auto_pass_recommended: if is_joiner_actor { auto_pass } else { false },
                            spell_costs: if is_joiner_actor {
                                spell_costs_all.clone()
                            } else {
                                HashMap::new()
                            },
                            legal_actions_by_object: if is_joiner_actor {
                                by_object_all.clone()
                            } else {
                                HashMap::new()
                            },
                            derived: derived_joiner,
                            player_token: Some(player_token.clone()),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }

                        // Send GameStarted to all other connected players
                        for (&pid, sender) in conns.get(&game_code).unwrap().iter() {
                            if pid != joiner {
                                let p_state =
                                    server_core::filter_state_for_player(&session.state, pid);
                                let is_actor = actor == Some(pid);
                                let p_legals = if is_actor {
                                    legal_actions.clone()
                                } else {
                                    vec![]
                                };
                                let derived_p = derive_views(&p_state);
                                let _ = sender.send(ServerMessage::GameStarted {
                                    state: p_state,
                                    your_player: pid,
                                    opponent_name: None,
                                    player_names: player_names.clone(),
                                    legal_actions: p_legals,
                                    auto_pass_recommended: if is_actor { auto_pass } else { false },
                                    spell_costs: if is_actor {
                                        spell_costs_all.clone()
                                    } else {
                                        HashMap::new()
                                    },
                                    legal_actions_by_object: if is_actor {
                                        by_object_all.clone()
                                    } else {
                                        HashMap::new()
                                    },
                                    derived: derived_p,
                                    player_token: None,
                                });
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
                        let actor = server_core::acting_player(&session.state);
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

                        Ok((human_result, ai_results, actor, eliminated, player_count))
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
                    actor,
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
                                    let player_legals =
                                        if ai_results.is_empty() && actor == Some(*pid) {
                                            legal_actions.clone()
                                        } else {
                                            // AI will act next — don't send legal actions yet
                                            vec![]
                                        };
                                    let p_auto_pass =
                                        if ai_results.is_empty() && actor == Some(*pid) {
                                            auto_pass_rec
                                        } else {
                                            false
                                        };
                                    let p_spell_costs =
                                        if ai_results.is_empty() && actor == Some(*pid) {
                                            spell_costs.clone()
                                        } else {
                                            HashMap::new()
                                        };
                                    let p_by_object =
                                        if ai_results.is_empty() && actor == Some(*pid) {
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

                        let conns = connections.lock().await;
                        if let Some(players) = conns.get(&game_code) {
                            for (pid, pstate) in &ai_filtered {
                                if let Some(s) = players.get(pid) {
                                    let player_legals = if is_last && actor == Some(*pid) {
                                        ai_legal.clone()
                                    } else {
                                        vec![]
                                    };
                                    let p_auto_pass = if is_last && actor == Some(*pid) {
                                        *ai_auto_pass
                                    } else {
                                        false
                                    };
                                    let p_spell_costs = if is_last && actor == Some(*pid) {
                                        ai_spell_costs.clone()
                                    } else {
                                        HashMap::new()
                                    };
                                    let p_by_object = if is_last && actor == Some(*pid) {
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
                    ai_results: Vec<server_core::session::ActionResult>,
                },
                Err(String),
            }

            let outcome = {
                let mut mgr = state.lock().await;
                let is_waiting = mgr
                    .sessions
                    .get(&game_code)
                    .map(|s| !s.is_full())
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
                        Ok(filtered_state) => {
                            let session = mgr.sessions.get_mut(&game_code).unwrap();
                            let player = session.player_for_token(&player_token).unwrap();
                            let player_names = session.display_names.clone();

                            let opponent_name =
                                engine::game::players::opponents(&session.state, player)
                                    .first()
                                    .and_then(|&opp| {
                                        let name = &session.display_names[opp.0 as usize];
                                        if name.is_empty() {
                                            None
                                        } else {
                                            Some(name.clone())
                                        }
                                    });

                            let (legal_actions_all, spell_costs_all, by_object_all) =
                                engine_legal_actions_full(&session.state);
                            let auto_pass = engine_auto_pass(&session.state, &legal_actions_all);
                            let actor = server_core::acting_player(&session.state);
                            let is_actor = actor == Some(player);
                            let player_legals = if is_actor { legal_actions_all } else { vec![] };

                            let derived_reconnect = derive_views(&filtered_state);
                            let game_started_msg = ServerMessage::GameStarted {
                                state: filtered_state,
                                your_player: player,
                                opponent_name,
                                player_names,
                                legal_actions: player_legals,
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
                                derived: derived_reconnect,
                                player_token: None,
                            };

                            let ai_results = session.run_ai();
                            ReconnectOutcome::InGame {
                                player,
                                game_started_msg: Box::new(game_started_msg),
                                ai_results,
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
                    ai_results,
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

                    // Broadcast AI follow-up results with delays (filter outside lock)
                    for result in ai_results {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let (
                            raw_state,
                            events,
                            legal_actions,
                            log_entries,
                            auto_pass,
                            spell_costs,
                            legal_actions_by_object,
                        ) = result;
                        let actor = {
                            let mgr = state.lock().await;
                            let session = mgr.sessions.get(&game_code).unwrap();
                            server_core::acting_player(&session.state)
                        };
                        let filtered = server_core::filter_state_for_player(&raw_state, player);
                        let is_actor = actor == Some(player);
                        let player_legals = if is_actor { legal_actions } else { vec![] };
                        let derived = derive_views(&filtered);
                        let _ = tx.send(ServerMessage::StateUpdate {
                            state: filtered,
                            events,
                            legal_actions: player_legals,
                            auto_pass_recommended: if is_actor { auto_pass } else { false },
                            eliminated_players: vec![],
                            log_entries,
                            spell_costs: if is_actor {
                                spell_costs
                            } else {
                                HashMap::new()
                            },
                            legal_actions_by_object: if is_actor {
                                legal_actions_by_object
                            } else {
                                HashMap::new()
                            },
                            derived,
                        });
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

        ClientMessage::SubscribeLobby => {
            debug!("lobby subscription");
            identity.lobby_subscribed = true;

            {
                let mut subs = lobby_subscribers.lock().await;
                subs.push(tx.clone());
            }

            let lob = lobby.lock().await;
            let games = lob.public_games();
            debug!(games = games.len(), "sending lobby state");
            let _ = tx.send(ServerMessage::LobbyUpdate { games });

            let count = player_count.load(Ordering::Relaxed);
            let _ = tx.send(ServerMessage::PlayerCount { count });
        }

        ClientMessage::UnsubscribeLobby => {
            debug!("lobby unsubscribe");
            identity.lobby_subscribed = false;
            let mut subs = lobby_subscribers.lock().await;
            subs.retain(|s| !s.is_closed());
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
            // the host's PeerJS peer ID so guests can dial them directly.
            // Deck data, AI seats, and format-legality checks are
            // host-authoritative and irrelevant here: they're all enforced
            // on the host's machine when the P2P game actually starts.
            if matches!(mode, ServerMode::LobbyOnly) {
                let peer_id = match host_peer_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    Some(id) => id.to_string(),
                    None => {
                        warn!("lobby-only CreateGameWithSettings missing host_peer_id");
                        let msg = ServerMessage::Error {
                            message: "host_peer_id is required on lobby-only servers".to_string(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                };

                // Re-registration cleanup: if this socket already owns a
                // lobby entry, drop it before registering the new one.
                // Without this, a client that calls CreateGameWithSettings
                // twice would orphan the first entry until the 5-minute
                // expiry — and disconnect cleanup only sees the latest.
                if let Some(previous) = identity.lobby_host_game.take() {
                    let removed = {
                        let mut lob = lobby.lock().await;
                        let existed = lob.has_game(&previous);
                        lob.unregister_game(&previous);
                        existed
                    };
                    if removed {
                        info!(game = %previous, "replacing previous lobby entry from same socket");
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameRemoved {
                                game_code: previous,
                            },
                        )
                        .await;
                    }
                }

                // Capacity cap: `LobbyManager` has no built-in limit, so a
                // lobby-only server would otherwise accept unbounded
                // entries until `check_expired` reaps them after 5
                // minutes. Check under the same lock we'll register with.
                {
                    let lob = lobby.lock().await;
                    if lob.len() >= MAX_LOBBY_ENTRIES {
                        warn!(
                            entries = lob.len(),
                            limit = MAX_LOBBY_ENTRIES,
                            "lobby full, rejecting CreateGameWithSettings"
                        );
                        let msg = ServerMessage::Error {
                            message: "Server lobby is full, please try again shortly".to_string(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }

                let game_code = server_core::generate_game_code();
                let player_token = server_core::generate_player_token();
                let pc = requested_player_count.clamp(2, 6);
                let format_config_for_lobby = format_config.clone();
                let (host_version, host_build_commit) = identity
                    .client_hello
                    .as_ref()
                    .map(|h| (h.client_version.clone(), h.build_commit.clone()))
                    .unwrap_or_default();

                {
                    let mut lob = lobby.lock().await;
                    lob.register_game(
                        &game_code,
                        RegisterGameRequest {
                            host_name: display_name.clone(),
                            public,
                            password: password.clone(),
                            timer_seconds,
                            host_version,
                            host_build_commit,
                            // The host fills seat 0 immediately; P2P guests
                            // join over PeerJS and the host pushes updated
                            // counts via a LobbyGameUpdated broadcast from
                            // the client when it observes new connections.
                            current_players: 1,
                            max_players: pc as u32,
                            format_config: format_config_for_lobby,
                            match_config,
                            room_name: room_name
                                .as_deref()
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string),
                            host_peer_id: peer_id,
                        },
                    );
                }

                // Remember this socket owns the entry so disconnect cleanup
                // can find it. `set_session` is deliberately not called —
                // there is no GameSession / PlayerId here.
                identity.lobby_host_game = Some(game_code.clone());

                let msg = ServerMessage::GameCreated {
                    game_code: game_code.clone(),
                    player_token,
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }

                if public {
                    let game = {
                        let lob = lobby.lock().await;
                        lob.public_game(&game_code)
                    };
                    if let Some(game) = game {
                        broadcast_to_lobby_subscribers(
                            lobby_subscribers,
                            ServerMessage::LobbyGameAdded { game },
                        )
                        .await;
                    }
                }

                info!(game = %game_code, host = %display_name, "lobby-only game registered");
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
                let validation_request = DeckCompatibilityRequest {
                    main_deck: deck.main_deck.clone(),
                    sideboard: deck.sideboard.clone(),
                    commander: deck.commander.clone(),
                    selected_format: Some(fc.format),
                    selected_match_type: None,
                };
                if let Err(reasons) = validate_deck_for_format(db, &validation_request) {
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

            if !ai_seats.is_empty() {
                // --- AI game path: create, start, and run initial AI actions ---
                let mut ai_requests = Vec::new();
                for seat in &ai_seats {
                    let ai_deck_data = match &seat.deck_name {
                        Some(name) if name.eq_ignore_ascii_case("random") => {
                            server_core::starter_decks::random_starter_deck()
                        }
                        Some(name) => server_core::starter_decks::find_starter_deck(name)
                            .unwrap_or_else(|| {
                                warn!(deck = %name, "unknown AI deck name, using random");
                                server_core::starter_decks::random_starter_deck()
                            }),
                        None => server_core::starter_decks::random_starter_deck(),
                    };
                    let ai_resolved = match resolve_deck(db, &ai_deck_data) {
                        Ok(d) => d,
                        Err(e) => {
                            error!(error = %e, "AI deck resolve failed, cloning host deck");
                            resolved.clone()
                        }
                    };
                    ai_requests.push((seat.seat_index, seat.difficulty, ai_resolved));
                }

                let (game_code, player_token, game_started_msg, ai_results) = {
                    let mut mgr = state.lock().await;
                    let (game_code, player_token) = mgr.create_game_with_ai(
                        resolved,
                        display_name.clone(),
                        timer_seconds,
                        match_config,
                        ai_requests,
                        db.card_names(),
                        format_config.clone(),
                    );

                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    let (legal_actions, spell_costs_all, by_object_all) =
                        engine_legal_actions_full(&session.state);
                    let auto_pass = engine_auto_pass(&session.state, &legal_actions);
                    let actor = server_core::acting_player(&session.state);
                    let player_names = session.display_names.clone();

                    let is_actor = actor == Some(PlayerId(0));
                    let host_legals = if is_actor { legal_actions } else { vec![] };
                    let host_state =
                        server_core::filter_state_for_player(&session.state, PlayerId(0));

                    let derived_host = derive_views(&host_state);
                    let game_started_msg = ServerMessage::GameStarted {
                        state: host_state,
                        your_player: PlayerId(0),
                        opponent_name: Some(session.display_names[1].clone()),
                        player_names,
                        legal_actions: host_legals,
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
                        derived: derived_host,
                        player_token: None,
                    };

                    let ai_results = session.run_ai();

                    // Persist the AI game session
                    persist_session_async(game_db, &game_code, session);

                    (game_code, player_token, game_started_msg, ai_results)
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

                // Broadcast initial AI actions (e.g. mulligan decisions) with delays
                // Filter outside the lock for each AI result
                for result in ai_results {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let (
                        raw_state,
                        events,
                        legal_actions,
                        log_entries,
                        auto_pass,
                        spell_costs,
                        legal_actions_by_object,
                    ) = result;
                    let actor = {
                        let mgr = state.lock().await;
                        let session = mgr.sessions.get(&game_code).unwrap();
                        server_core::acting_player(&session.state)
                    };
                    let filtered = server_core::filter_state_for_player(&raw_state, PlayerId(0));
                    {
                        let is_actor = actor == Some(PlayerId(0));
                        let player_legals = if is_actor { legal_actions } else { vec![] };
                        let derived = derive_views(&filtered);
                        let _ = tx.send(ServerMessage::StateUpdate {
                            state: filtered,
                            events,
                            legal_actions: player_legals,
                            auto_pass_recommended: if is_actor { auto_pass } else { false },
                            eliminated_players: vec![],
                            log_entries,
                            spell_costs: if is_actor {
                                spell_costs
                            } else {
                                HashMap::new()
                            },
                            legal_actions_by_object: if is_actor {
                                legal_actions_by_object
                            } else {
                                HashMap::new()
                            },
                            derived,
                        });
                    }
                }

                info!(game = %game_code, host = %display_name, "AI game started");
            } else {
                // --- Standard multiplayer path ---
                let mut mgr = state.lock().await;
                let pc = requested_player_count.clamp(2, 6);
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

                identity.set_session(game_code.clone(), PlayerId(0), player_token.clone());

                let mut conns = connections.lock().await;
                conns
                    .entry(game_code.clone())
                    .or_default()
                    .insert(PlayerId(0), tx.clone());

                let mut lob = lobby.lock().await;
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
                    },
                );

                // Store lobby metadata on the session and persist to SQLite
                if let Some(session) = mgr.sessions.get_mut(&game_code) {
                    session.lobby_meta = Some(server_core::PersistedLobbyMeta {
                        host_name: display_name,
                        public,
                        password,
                        timer_seconds,
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
        } => {
            info!(game = %game_code, "LookupJoinTarget");

            let lookup = {
                let lob = lobby.lock().await;

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
                    Err(ServerMessage::Error {
                        message: format!(
                            "Build mismatch: host is on {host}, you are on {guest}. Refresh to update."
                        ),
                    })
                } else {
                    match lob.verify_password(&game_code, password.as_deref()) {
                        Ok(()) => match lob.join_target_info(&game_code) {
                            Some(info) => Ok(info),
                            None => Err(ServerMessage::Error {
                                message: format!("Game not found in lobby: {game_code}"),
                            }),
                        },
                        Err(e) if e == "password_required" => {
                            Err(ServerMessage::PasswordRequired {
                                game_code: game_code.clone(),
                            })
                        }
                        Err(e) => {
                            warn!(game = %game_code, error = %e, "lookup password verification failed");
                            Err(ServerMessage::Error { message: e })
                        }
                    }
                }
            };

            let info = match lookup {
                Ok(info) => info,
                Err(msg) => {
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            if info.max_players > 0 && info.current_players >= info.max_players {
                let msg = ServerMessage::Error {
                    message: format!("Game {game_code} is full"),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let msg = ServerMessage::JoinTargetInfo {
                game_code: game_code.clone(),
                is_p2p: info.is_p2p,
                format_config: info.format_config,
                match_config: info.match_config,
                player_count: info.max_players as u8,
                filled_seats: info.current_players as u8,
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
        } => {
            info!(game = %game_code, joiner = %display_name, "JoinGameWithPassword");

            // --- Lobby-only broker path ------------------------------
            //
            // Run the same build-commit + password gates, then hand back
            // the host's peer ID so the guest can dial over PeerJS. No
            // session is created server-side. Lobby entry stays so
            // additional guests (Commander 3–4p) can still join; the host
            // explicitly drops it via `UnregisterLobby` once its P2P
            // connections are live.
            if matches!(mode, ServerMode::LobbyOnly) {
                let lob = lobby.lock().await;

                let guest_commit = identity
                    .client_hello
                    .as_ref()
                    .map(|h| h.build_commit.as_str())
                    .unwrap_or("");
                let host_commit = lob.host_build_commit(&game_code).unwrap_or("");
                if let BuildCommitCheck::Reject { host, guest } =
                    check_build_commit(host_commit, guest_commit)
                {
                    warn!(game = %game_code, %host, %guest, "build mismatch — refusing join (lobby-only)");
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
                        let msg = ServerMessage::PasswordRequired {
                            game_code: game_code.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                    Err(e) => {
                        warn!(game = %game_code, error = %e, "password verification failed (lobby-only)");
                        let msg = ServerMessage::Error { message: e };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }
                        return;
                    }
                }

                // Atomic fetch of peer_id + seat counts — both are needed
                // for PeerInfo and pulling them separately would let a
                // concurrent UnregisterLobby land between reads. An entry
                // registered by a Full-mode server has an empty peer_id;
                // `broker_info` treats that as "not brokerable" so the
                // error below can distinguish it from a missing game.
                let info = match lob.join_target_info(&game_code) {
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
                };
                if !info.is_p2p {
                    let msg = ServerMessage::Error {
                        message: format!(
                            "Game {game_code} is hosted on a Full-mode server and cannot be brokered"
                        ),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

                // Seat-full rejection: the host owns final seat assignment
                // over P2P, but if the lobby already advertises the room
                // as full there's no point handing out a PeerInfo the
                // host would immediately refuse.
                if info.max_players > 0 && info.current_players >= info.max_players {
                    let msg = ServerMessage::Error {
                        message: format!("Game {game_code} is full"),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }

                let msg = ServerMessage::PeerInfo {
                    game_code: game_code.clone(),
                    host_peer_id: info.host_peer_id,
                    format_config: info.format_config,
                    match_config: info.match_config,
                    player_count: info.max_players as u8,
                    filled_seats: info.current_players as u8,
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                info!(game = %game_code, joiner = %display_name, "sent PeerInfo to guest");
                // Deck ignored intentionally — the host validates its
                // guests' decks over P2P once the connection is up. This
                // also means a guest doesn't waste bandwidth shipping a
                // full deck list to a broker that can't read it.
                let _ = deck;
                return;
            }

            {
                let lob = lobby.lock().await;

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

            let mut mgr = state.lock().await;
            match mgr.join_game_with_name(&game_code, resolved, display_name) {
                Ok((player_token, filtered_state)) => {
                    mgr.set_card_names(&game_code, db.card_names());
                    let session = mgr.sessions.get(&game_code).unwrap();
                    let joiner = session.player_for_token(&player_token).unwrap();
                    info!(game = %game_code, player = ?joiner, "player joined via lobby");

                    // Persist updated session (now has the new player)
                    persist_session_async(game_db, &game_code, session);
                    identity.set_session(game_code.clone(), joiner, player_token.clone());

                    // Build slot info before releasing session borrow
                    let slot_info = session.player_slot_info();
                    let current_count = session.current_player_count();

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
                        let mut lob = lobby.lock().await;
                        lob.set_current_players(&game_code, current_count);
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

        ClientMessage::Ping { timestamp } => {
            let msg = ServerMessage::Pong { timestamp };
            if let Ok(json) = serde_json::to_string(&msg) {
                let _ = socket.send(Message::text(json)).await;
            }
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

                session.apply_seat_delta(seat_state, &delta);
                if delta.now_started {
                    session.start_game();
                }
                let slot_info = session.player_slot_info();
                let current_players = session.current_player_count();
                let max_players = session.player_count;
                let started = delta.now_started;
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
                    let mut lob = lobby.lock().await;
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
                    let mut lob = lobby.lock().await;
                    lob.set_current_players(&game_code, current_players);
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

        ClientMessage::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
        } => {
            let is_owner = identity
                .lobby_host_game
                .as_deref()
                .is_some_and(|g| g == game_code);
            if !is_owner {
                let msg = ServerMessage::Error {
                    message: "Only the lobby host can update metadata".to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let updated = {
                let mut lob = lobby.lock().await;
                lob.set_current_players(&game_code, current_players as u32);
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

        ClientMessage::UnregisterLobby { game_code } => {
            // Ownership check: only the socket that registered this entry
            // (stored in `identity.lobby_host_game`) may tear it down.
            // Without this gate any client that subscribed to the lobby
            // could drop someone else's listing by guessing codes.
            let is_owner = identity
                .lobby_host_game
                .as_deref()
                .is_some_and(|g| g == game_code);
            if !is_owner {
                warn!(game = %game_code, "UnregisterLobby rejected — socket is not the registered host");
                let msg = ServerMessage::Error {
                    message: "UnregisterLobby only allowed for the host that registered the game"
                        .to_string(),
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }
                return;
            }

            let removed = {
                let mut lob = lobby.lock().await;
                let existed = lob.has_game(&game_code);
                lob.unregister_game(&game_code);
                existed
            };
            if removed {
                info!(game = %game_code, "lobby entry removed by host (UnregisterLobby)");
                broadcast_to_lobby_subscribers(
                    lobby_subscribers,
                    ServerMessage::LobbyGameRemoved {
                        game_code: game_code.clone(),
                    },
                )
                .await;
            }
            // Clear so disconnect cleanup doesn't try to unregister again.
            identity.lobby_host_game = None;
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
            PROTOCOL_VERSION,
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
    fn rejects_client_hello_with_zero_protocol_version() {
        let outcome = classify_hello_gate(
            false,
            &ClientMessage::ClientHello {
                client_version: "0.1.11".into(),
                build_commit: "abc1234".into(),
                protocol_version: 0,
            },
            PROTOCOL_VERSION,
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
            PROTOCOL_VERSION,
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
            PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::CreateGame { deck: empty_deck() },
            PROTOCOL_VERSION,
        );
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(false, &ClientMessage::SubscribeLobby, PROTOCOL_VERSION);
        assert_eq!(outcome, HelloGateOutcome::RejectHandshakeRequired);

        let outcome = classify_hello_gate(
            false,
            &ClientMessage::Ping { timestamp: 1 },
            PROTOCOL_VERSION,
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
            PROTOCOL_VERSION,
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
            PROTOCOL_VERSION,
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
}
