use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use engine::database::CardDatabase;
use engine::types::player::PlayerId;
use phase_ai::get_legal_actions;
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

    let port = std::env::var("PORT").unwrap_or_else(|_| "9374".to_string());
    let data_root = std::env::var("PHASE_DATA_DIR").unwrap_or_else(|_| "data".to_string());
    let data_path = Path::new(&data_root);
    let export_path = data_path.join("card-data.json");
    let card_db = if export_path.exists() {
        CardDatabase::from_export(&export_path).expect("Failed to load card-data.json")
    } else {
        CardDatabase::load_json(
            &data_path.join("mtgjson/test_fixture.json"),
            &data_path.join("abilities"),
        )
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

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .layer(CorsLayer::permissive())
        .with_state((
            state,
            connections,
            db,
            lobby,
            lobby_subscribers,
            player_count,
        ));

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("failed to bind");
    info!(port = %port, "phase-server listening");
    axum::serve(listener, app).await.expect("server error");
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

        let opponent = PlayerId(1 - player_id.0);
        let conns = connections.lock().await;
        if let Some(opp_sender) = conns.get(game_code).and_then(|m| m.get(&opponent)) {
            let _ = opp_sender.send(ServerMessage::OpponentDisconnected { grace_seconds: 120 });
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
                    info!(game = %game_code, "player joined, game starting");
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(PlayerId(1));
                    identity.player_token = Some(player_token.clone());

                    let session = mgr.sessions.get(&game_code).unwrap();
                    let legal_actions = get_legal_actions(&session.state);
                    let actor = server_core::acting_player(&session.state.waiting_for);
                    let p1_legals = if actor == Some(PlayerId(1)) {
                        legal_actions.clone()
                    } else {
                        vec![]
                    };
                    let p0_legals = if actor == Some(PlayerId(0)) {
                        legal_actions
                    } else {
                        vec![]
                    };

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(PlayerId(1), tx.clone());

                    let msg = ServerMessage::GameStarted {
                        state: filtered_state,
                        your_player: PlayerId(1),
                        opponent_name: None,
                        legal_actions: p1_legals,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    let p0_state = server_core::filter_state_for_player(
                        &mgr.sessions.get(&game_code).unwrap().state,
                        PlayerId(0),
                    );
                    if let Some(p0_sender) = conns.get(&game_code).and_then(|m| m.get(&PlayerId(0)))
                    {
                        info!(game = %game_code, "sending GameStarted to host");
                        let _ = p0_sender.send(ServerMessage::GameStarted {
                            state: p0_state,
                            your_player: PlayerId(0),
                            opponent_name: None,
                            legal_actions: p0_legals,
                        });
                    } else {
                        warn!(game = %game_code, "host channel not found in connections");
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
            let mut mgr = state.lock().await;
            match mgr.handle_action(&game_code, &player_token, action) {
                Ok((p0_state, p1_state, events, legal_actions)) => {
                    debug!(game = %game_code, events = events.len(), "action applied");
                    let actor = server_core::acting_player(
                        &mgr.sessions.get(&game_code).unwrap().state.waiting_for,
                    );
                    let p0_legals = if actor == Some(PlayerId(0)) {
                        legal_actions.clone()
                    } else {
                        vec![]
                    };
                    let p1_legals = if actor == Some(PlayerId(1)) {
                        legal_actions
                    } else {
                        vec![]
                    };

                    let conns = connections.lock().await;
                    if let Some(players) = conns.get(&game_code) {
                        if let Some(s) = players.get(&PlayerId(0)) {
                            let _ = s.send(ServerMessage::StateUpdate {
                                state: p0_state,
                                events: events.clone(),
                                legal_actions: p0_legals,
                            });
                        }
                        if let Some(s) = players.get(&PlayerId(1)) {
                            let _ = s.send(ServerMessage::StateUpdate {
                                state: p1_state,
                                events,
                                legal_actions: p1_legals,
                            });
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
            let mut mgr = state.lock().await;
            match mgr.handle_reconnect(&game_code, &player_token) {
                Ok(filtered_state) => {
                    let session = mgr.sessions.get(&game_code).unwrap();
                    let player = session.player_for_token(&player_token).unwrap();
                    let opponent = PlayerId(1 - player.0);
                    let opp_name = &session.display_names[opponent.0 as usize];
                    let opponent_name = if opp_name.is_empty() {
                        None
                    } else {
                        Some(opp_name.clone())
                    };

                    let legal_actions_all = get_legal_actions(&session.state);
                    let actor = server_core::acting_player(&session.state.waiting_for);
                    let player_legals = if actor == Some(player) {
                        legal_actions_all
                    } else {
                        vec![]
                    };

                    info!(game = %game_code, player = ?player, "reconnect succeeded");
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(player);
                    identity.player_token = Some(player_token);

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(player, tx.clone());

                    let msg = ServerMessage::GameStarted {
                        state: filtered_state,
                        your_player: player,
                        opponent_name,
                        legal_actions: player_legals,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    if let Some(opp_sender) = conns.get(&game_code).and_then(|m| m.get(&opponent)) {
                        let _ = opp_sender.send(ServerMessage::OpponentReconnected);
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
        } => {
            info!(
                display_name = %display_name,
                public = public,
                has_password = password.is_some(),
                timer = ?timer_seconds,
                deck_size = deck.main_deck.len(),
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

            let mut mgr = state.lock().await;
            let (game_code, player_token) =
                mgr.create_game_with_settings(resolved, display_name.clone(), timer_seconds);
            info!(game = %game_code, host = %display_name, "game created via lobby");

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
                    info!(game = %game_code, "player joined via lobby, game starting");
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(PlayerId(1));
                    identity.player_token = Some(player_token.clone());

                    let session = mgr.sessions.get(&game_code).unwrap();
                    let host_name = session.display_names[0].clone();
                    let joiner_name = session.display_names[1].clone();
                    let legal_actions = get_legal_actions(&session.state);
                    let actor = server_core::acting_player(&session.state.waiting_for);
                    let p1_legals = if actor == Some(PlayerId(1)) {
                        legal_actions.clone()
                    } else {
                        vec![]
                    };
                    let p0_legals = if actor == Some(PlayerId(0)) {
                        legal_actions
                    } else {
                        vec![]
                    };

                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(PlayerId(1), tx.clone());

                    let joiner_opp_name = if host_name.is_empty() {
                        None
                    } else {
                        Some(host_name)
                    };
                    let msg = ServerMessage::GameStarted {
                        state: filtered_state,
                        your_player: PlayerId(1),
                        opponent_name: joiner_opp_name,
                        legal_actions: p1_legals,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    let p0_state = server_core::filter_state_for_player(
                        &mgr.sessions.get(&game_code).unwrap().state,
                        PlayerId(0),
                    );
                    let host_opp_name = if joiner_name.is_empty() {
                        None
                    } else {
                        Some(joiner_name)
                    };
                    if let Some(p0_sender) = conns.get(&game_code).and_then(|m| m.get(&PlayerId(0)))
                    {
                        info!(game = %game_code, "sending GameStarted to host");
                        let _ = p0_sender.send(ServerMessage::GameStarted {
                            state: p0_state,
                            your_player: PlayerId(0),
                            opponent_name: host_opp_name,
                            legal_actions: p0_legals,
                        });
                    } else {
                        warn!(game = %game_code, "host channel not found in connections");
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
            let opponent = PlayerId(1 - player_id.0);

            let conceded_msg = ServerMessage::Conceded { player: player_id };
            let game_over_msg = ServerMessage::GameOver {
                winner: Some(opponent),
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
            let opponent = PlayerId(1 - player_id.0);
            let msg = ServerMessage::Emote {
                from_player: player_id,
                emote,
            };

            let conns = connections.lock().await;
            if let Some(opp_sender) = conns.get(&game_code).and_then(|m| m.get(&opponent)) {
                let _ = opp_sender.send(msg);
            }
        }
    }
}
