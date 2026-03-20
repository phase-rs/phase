use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use clap::Parser;
use engine::ai_support::legal_actions as engine_legal_actions;
use engine::database::CardDatabase;
use engine::types::player::PlayerId;
use http::HeaderValue;
use server_core::lobby::LobbyManager;
use server_core::protocol::{ClientMessage, ServerMessage};
use server_core::resolve_deck;
use server_core::session::SessionManager;
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, warn};

type SharedState = Arc<Mutex<SessionManager>>;
type SharedConnections =
    Arc<Mutex<HashMap<String, HashMap<PlayerId, mpsc::UnboundedSender<ServerMessage>>>>>;
type SharedDb = Arc<CardDatabase>;
type SharedLobby = Arc<Mutex<LobbyManager>>;
type SharedLobbySubscribers = Arc<Mutex<Vec<mpsc::UnboundedSender<ServerMessage>>>>;
type SharedPlayerCount = Arc<AtomicU32>;

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
}

/// Per-socket state tracking which game/player this connection belongs to.
struct SocketIdentity {
    game_code: Option<String>,
    player_id: Option<PlayerId>,
    player_token: Option<String>,
    lobby_subscribed: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "phase_server=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
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

    let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
    let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));
    let lobby: SharedLobby = Arc::new(Mutex::new(LobbyManager::new()));
    let lobby_subscribers: SharedLobbySubscribers = Arc::new(Mutex::new(Vec::new()));
    let player_count: SharedPlayerCount = Arc::new(AtomicU32::new(0));

    // Spawn background task for grace period and lobby expiry
    let bg_state = state.clone();
    let bg_connections = connections.clone();
    let bg_lobby = lobby.clone();
    let bg_lobby_subs = lobby_subscribers.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;

            // Check reconnect grace period expiry
            let expired = {
                let mut mgr = bg_state.lock().await;
                mgr.reconnect.check_expired()
            };
            for game_code in &expired {
                info!(game = %game_code, "grace period expired, ending game");
                let conns = bg_connections.lock().await;
                if let Some(players) = conns.get(game_code) {
                    let msg = ServerMessage::GameOver {
                        winner: None,
                        reason: "Opponent disconnected (grace period expired)".to_string(),
                    };
                    for sender in players.values() {
                        let _ = sender.send(msg.clone());
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
        ));

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", cli.port))
        .await
        .expect("failed to bind");
    info!(port = %cli.port, "phase-server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");
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
);

async fn ws_handler(
    ws: WebSocketUpgrade,
    State((state, connections, db, lobby, lobby_subscribers, player_count)): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        handle_socket(
            socket,
            state,
            connections,
            db,
            lobby,
            lobby_subscribers,
            player_count,
        )
    })
}

async fn handle_socket(
    mut socket: WebSocket,
    state: SharedState,
    connections: SharedConnections,
    db: SharedDb,
    lobby: SharedLobby,
    lobby_subscribers: SharedLobbySubscribers,
    player_count: SharedPlayerCount,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    let count = player_count.fetch_add(1, Ordering::Relaxed) + 1;
    info!(online = count, "client connected");
    broadcast_player_count(&lobby_subscribers, count).await;

    let mut identity = SocketIdentity {
        game_code: None,
        player_id: None,
        player_token: None,
        lobby_subscribed: false,
    };

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

                        handle_client_message(
                            client_msg,
                            &mut socket,
                            &state,
                            &connections,
                            &db,
                            &lobby,
                            &lobby_subscribers,
                            &player_count,
                            &tx,
                            &mut identity,
                        )
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

async fn broadcast_to_lobby_subscribers(
    lobby_subscribers: &SharedLobbySubscribers,
    msg: ServerMessage,
) {
    let subs = lobby_subscribers.lock().await;
    for sub in subs.iter() {
        let _ = sub.send(msg.clone());
    }
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
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
) {
    match client_msg {
        ClientMessage::CreateGame { deck } => {
            info!(deck_size = deck.main_deck.len(), "CreateGame");
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

            identity.game_code = Some(game_code.clone());
            identity.player_id = Some(PlayerId(0));
            identity.player_token = Some(player_token.clone());

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
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(joiner);
                    identity.player_token = Some(player_token.clone());

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    // Only send GameStarted when the game is full (all seats claimed)
                    if session.is_full() {
                        let legal_actions = engine_legal_actions(&session.state);
                        let actor = server_core::acting_player(&session.state.waiting_for);
                        let player_names = session.display_names.clone();

                        // Send GameStarted to the joiner
                        let joiner_legals = if actor == Some(joiner) {
                            legal_actions.clone()
                        } else {
                            vec![]
                        };
                        let msg = ServerMessage::GameStarted {
                            state: filtered_state,
                            your_player: joiner,
                            opponent_name: None,
                            player_names: player_names.clone(),
                            legal_actions: joiner_legals,
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }

                        // Send GameStarted to all other connected players
                        for (&pid, sender) in conns.get(&game_code).unwrap().iter() {
                            if pid != joiner {
                                let p_state =
                                    server_core::filter_state_for_player(&session.state, pid);
                                let p_legals = if actor == Some(pid) {
                                    legal_actions.clone()
                                } else {
                                    vec![]
                                };
                                let _ = sender.send(ServerMessage::GameStarted {
                                    state: p_state,
                                    your_player: pid,
                                    opponent_name: None,
                                    player_names: player_names.clone(),
                                    legal_actions: p_legals,
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

            // Apply human action and collect AI follow-up results while holding the lock
            let action_result = {
                let mut mgr = state.lock().await;
                match mgr.handle_action(&game_code, &player_token, action) {
                    Ok(human_result) => {
                        // Run AI follow-up actions
                        let ai_results = match mgr.sessions.get_mut(&game_code) {
                            Some(session) => session.run_ai_and_filter(),
                            None => vec![],
                        };
                        let session = mgr.sessions.get(&game_code).unwrap();
                        let actor = server_core::acting_player(&session.state.waiting_for);
                        let eliminated = session.state.eliminated_players.clone();
                        Ok((human_result, ai_results, actor, eliminated))
                    }
                    Err(e) => Err(e),
                }
            }; // lock dropped

            match action_result {
                Ok((
                    (filtered_states, events, legal_actions, log_entries),
                    ai_results,
                    actor,
                    eliminated,
                )) => {
                    debug!(game = %game_code, events = events.len(), "action applied");

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
                                    let _ = s.send(ServerMessage::StateUpdate {
                                        state: pstate.clone(),
                                        events: events.clone(),
                                        legal_actions: player_legals,
                                        eliminated_players: eliminated.clone(),
                                        log_entries: log_entries.clone(),
                                    });
                                }
                            }
                        }
                    }

                    // Broadcast AI follow-up results with delays
                    for (i, result) in ai_results.iter().enumerate() {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let (ai_filtered, ai_events, ai_legal, ai_log_entries) = result;
                        let is_last = i == ai_results.len() - 1;
                        let conns = connections.lock().await;
                        if let Some(players) = conns.get(&game_code) {
                            for (pid, pstate) in ai_filtered {
                                if let Some(s) = players.get(pid) {
                                    let player_legals = if is_last && actor == Some(*pid) {
                                        ai_legal.clone()
                                    } else {
                                        vec![]
                                    };
                                    let _ = s.send(ServerMessage::StateUpdate {
                                        state: pstate.clone(),
                                        events: ai_events.clone(),
                                        legal_actions: player_legals,
                                        eliminated_players: eliminated.clone(),
                                        log_entries: ai_log_entries.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(game = %game_code, error = %e, "action rejected");
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

            // Extract all data while holding the lock, then send messages after
            let reconnect_result = {
                let mut mgr = state.lock().await;
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

                        let legal_actions_all = engine_legal_actions(&session.state);
                        let actor = server_core::acting_player(&session.state.waiting_for);
                        let player_legals = if actor == Some(player) {
                            legal_actions_all
                        } else {
                            vec![]
                        };

                        let game_started_msg = ServerMessage::GameStarted {
                            state: filtered_state,
                            your_player: player,
                            opponent_name,
                            player_names,
                            legal_actions: player_legals,
                        };

                        // Run AI follow-up if AI is next to act after reconnect
                        let ai_results = session.run_ai_and_filter();

                        Ok((player, game_started_msg, ai_results))
                    }
                    Err(e) => Err(e),
                }
            }; // lock dropped

            match reconnect_result {
                Ok((player, game_started_msg, ai_results)) => {
                    info!(game = %game_code, player = ?player, "reconnect succeeded");
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(player);
                    identity.player_token = Some(player_token);

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

                    // Broadcast AI follow-up results with delays
                    for result in ai_results {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        let (filtered_states, events, legal_actions, log_entries) = result;
                        let actor = {
                            let mgr = state.lock().await;
                            let session = mgr.sessions.get(&game_code).unwrap();
                            server_core::acting_player(&session.state.waiting_for)
                        };
                        for (pid, pstate) in &filtered_states {
                            if *pid == player {
                                let player_legals = if actor == Some(*pid) {
                                    legal_actions.clone()
                                } else {
                                    vec![]
                                };
                                let _ = tx.send(ServerMessage::StateUpdate {
                                    state: pstate.clone(),
                                    events: events.clone(),
                                    legal_actions: player_legals,
                                    eliminated_players: vec![],
                                    log_entries: log_entries.clone(),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
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
        } => {
            info!(
                display_name = %display_name,
                public = public,
                has_password = password.is_some(),
                timer = ?timer_seconds,
                deck_size = deck.main_deck.len(),
                ai_seats = ai_seats.len(),
                "CreateGameWithSettings"
            );
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
                    );

                    let session = mgr.sessions.get_mut(&game_code).unwrap();
                    let legal_actions = engine_legal_actions(&session.state);
                    let actor = server_core::acting_player(&session.state.waiting_for);
                    let player_names = session.display_names.clone();

                    let host_legals = if actor == Some(PlayerId(0)) {
                        legal_actions
                    } else {
                        vec![]
                    };
                    let host_state =
                        server_core::filter_state_for_player(&session.state, PlayerId(0));

                    let game_started_msg = ServerMessage::GameStarted {
                        state: host_state,
                        your_player: PlayerId(0),
                        opponent_name: Some(session.display_names[1].clone()),
                        player_names,
                        legal_actions: host_legals,
                    };

                    let ai_results = session.run_ai_and_filter();
                    (game_code, player_token, game_started_msg, ai_results)
                }; // lock dropped

                identity.game_code = Some(game_code.clone());
                identity.player_id = Some(PlayerId(0));
                identity.player_token = Some(player_token.clone());

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
                for result in ai_results {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    let (filtered_states, events, legal_actions, log_entries) = result;
                    let actor = {
                        let mgr = state.lock().await;
                        let session = mgr.sessions.get(&game_code).unwrap();
                        server_core::acting_player(&session.state.waiting_for)
                    };
                    for (pid, pstate) in &filtered_states {
                        if *pid == PlayerId(0) {
                            let player_legals = if actor == Some(*pid) {
                                legal_actions.clone()
                            } else {
                                vec![]
                            };
                            let _ = tx.send(ServerMessage::StateUpdate {
                                state: pstate.clone(),
                                events: events.clone(),
                                legal_actions: player_legals,
                                eliminated_players: vec![],
                                log_entries: log_entries.clone(),
                            });
                        }
                    }
                }

                info!(game = %game_code, host = %display_name, "AI game started");
            } else {
                // --- Standard multiplayer path ---
                let mut mgr = state.lock().await;
                let pc = requested_player_count.clamp(2, 6);
                let (game_code, player_token) = mgr.create_game_n_players(
                    resolved,
                    display_name.clone(),
                    timer_seconds,
                    pc,
                    match_config,
                );
                info!(game = %game_code, host = %display_name, players = pc, "game created via lobby");

                identity.game_code = Some(game_code.clone());
                identity.player_id = Some(PlayerId(0));
                identity.player_token = Some(player_token.clone());

                let mut conns = connections.lock().await;
                conns
                    .entry(game_code.clone())
                    .or_default()
                    .insert(PlayerId(0), tx.clone());

                let mut lob = lobby.lock().await;
                lob.register_game(&game_code, display_name, public, password, timer_seconds);

                let msg = ServerMessage::GameCreated {
                    game_code: game_code.clone(),
                    player_token,
                };
                if let Ok(json) = serde_json::to_string(&msg) {
                    let _ = socket.send(Message::text(json)).await;
                }

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

        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
        } => {
            info!(game = %game_code, joiner = %display_name, "JoinGameWithPassword");
            {
                let lob = lobby.lock().await;
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
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(joiner);
                    identity.player_token = Some(player_token.clone());

                    let player_names = session.display_names.clone();

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(joiner, tx.clone());

                    // Only send GameStarted when the game is full
                    if session.is_full() {
                        let legal_actions = engine_legal_actions(&session.state);
                        let actor = server_core::acting_player(&session.state.waiting_for);

                        // Find first opponent name for backward compat
                        let joiner_opp_name =
                            engine::game::players::opponents(&session.state, joiner)
                                .first()
                                .and_then(|&opp| {
                                    let name = &session.display_names[opp.0 as usize];
                                    if name.is_empty() {
                                        None
                                    } else {
                                        Some(name.clone())
                                    }
                                });

                        let joiner_legals = if actor == Some(joiner) {
                            legal_actions.clone()
                        } else {
                            vec![]
                        };
                        let msg = ServerMessage::GameStarted {
                            state: filtered_state,
                            your_player: joiner,
                            opponent_name: joiner_opp_name,
                            player_names: player_names.clone(),
                            legal_actions: joiner_legals,
                        };
                        if let Ok(json) = serde_json::to_string(&msg) {
                            let _ = socket.send(Message::text(json)).await;
                        }

                        // Send GameStarted to all other connected players
                        for (&pid, sender) in conns.get(&game_code).unwrap().iter() {
                            if pid != joiner {
                                let p_state =
                                    server_core::filter_state_for_player(&session.state, pid);
                                let opp_name =
                                    engine::game::players::opponents(&session.state, pid)
                                        .first()
                                        .and_then(|&opp| {
                                            let name = &session.display_names[opp.0 as usize];
                                            if name.is_empty() {
                                                None
                                            } else {
                                                Some(name.clone())
                                            }
                                        });
                                let p_legals = if actor == Some(pid) {
                                    legal_actions.clone()
                                } else {
                                    vec![]
                                };
                                let _ = sender.send(ServerMessage::GameStarted {
                                    state: p_state,
                                    your_player: pid,
                                    opponent_name: opp_name,
                                    player_names: player_names.clone(),
                                    legal_actions: p_legals,
                                });
                            }
                        }
                    }

                    let mut lob = lobby.lock().await;
                    lob.unregister_game(&game_code);

                    broadcast_to_lobby_subscribers(
                        lobby_subscribers,
                        ServerMessage::LobbyGameRemoved {
                            game_code: game_code.clone(),
                        },
                    )
                    .await;

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
    }
}
