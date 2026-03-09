use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use engine::database::CardDatabase;
use engine::types::player::PlayerId;
use server_core::protocol::{ClientMessage, ServerMessage};
use server_core::resolve_deck;
use server_core::session::SessionManager;
use tokio::sync::{mpsc, Mutex};
use tower_http::cors::CorsLayer;

type SharedState = Arc<Mutex<SessionManager>>;
type SharedConnections =
    Arc<Mutex<HashMap<String, HashMap<PlayerId, mpsc::UnboundedSender<ServerMessage>>>>>;
type SharedDb = Arc<CardDatabase>;

/// Per-socket state tracking which game/player this connection belongs to.
struct SocketIdentity {
    game_code: Option<String>,
    player_id: Option<PlayerId>,
    player_token: Option<String>,
}

#[tokio::main]
async fn main() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let cards_dir = std::env::var("FORGE_CARDS_DIR")
        .expect("FORGE_CARDS_DIR environment variable must be set to Forge card files directory");
    let card_db = CardDatabase::load(Path::new(&cards_dir)).expect("Failed to load card database");
    println!("Loaded {} cards", card_db.card_count());
    let db: SharedDb = Arc::new(card_db);

    let state: SharedState = Arc::new(Mutex::new(SessionManager::new()));
    let connections: SharedConnections = Arc::new(Mutex::new(HashMap::new()));

    // Spawn background task for grace period expiry
    let bg_state = state.clone();
    let bg_connections = connections.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let expired = {
                let mut mgr = bg_state.lock().await;
                mgr.reconnect.check_expired()
            };
            for game_code in expired {
                let conns = bg_connections.lock().await;
                if let Some(players) = conns.get(&game_code) {
                    let msg = ServerMessage::GameOver {
                        winner: None,
                        reason: "Opponent disconnected (grace period expired)".to_string(),
                    };
                    for (_, sender) in players {
                        let _ = sender.send(msg.clone());
                    }
                }
            }
        }
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/health", get(health))
        .layer(CorsLayer::permissive())
        .with_state((state, connections, db));

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
        .await
        .expect("failed to bind");
    println!("forge-server listening on port {}", port);
    axum::serve(listener, app).await.expect("server error");
}

async fn health() -> &'static str {
    "ok"
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State((state, connections, db)): State<(SharedState, SharedConnections, SharedDb)>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, connections, db))
}

async fn handle_socket(
    mut socket: WebSocket,
    state: SharedState,
    connections: SharedConnections,
    db: SharedDb,
) {
    // Channel for sending messages to this client from other tasks
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    let mut identity = SocketIdentity {
        game_code: None,
        player_id: None,
        player_token: None,
    };

    loop {
        tokio::select! {
            // Forward outbound messages from channel to WebSocket
            Some(msg) = rx.recv() => {
                if let Ok(json) = serde_json::to_string(&msg) {
                    if socket.send(Message::text(json)).await.is_err() {
                        break;
                    }
                }
            }

            // Read inbound messages from WebSocket
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
    if let (Some(game_code), Some(player_id)) = (&identity.game_code, &identity.player_id) {
        let mut mgr = state.lock().await;
        mgr.handle_disconnect(game_code, *player_id);

        // Notify opponent
        let opponent = PlayerId(1 - player_id.0);
        let conns = connections.lock().await;
        if let Some(opp_sender) = conns.get(game_code).and_then(|m| m.get(&opponent)) {
            let _ = opp_sender.send(ServerMessage::OpponentDisconnected { grace_seconds: 120 });
        }
    }
}

async fn handle_client_message(
    client_msg: ClientMessage,
    socket: &mut WebSocket,
    state: &SharedState,
    connections: &SharedConnections,
    db: &SharedDb,
    tx: &mpsc::UnboundedSender<ServerMessage>,
    identity: &mut SocketIdentity,
) {
    match client_msg {
        ClientMessage::CreateGame { deck } => {
            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                    return;
                }
            };

            let mut mgr = state.lock().await;
            let (game_code, player_token) = mgr.create_game(resolved);

            identity.game_code = Some(game_code.clone());
            identity.player_id = Some(PlayerId(0));
            identity.player_token = Some(player_token.clone());

            // Register in connections
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
            let resolved = match resolve_deck(db, &deck) {
                Ok(entries) => entries,
                Err(e) => {
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
                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(PlayerId(1));
                    identity.player_token = Some(player_token.clone());

                    // Register in connections
                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(PlayerId(1), tx.clone());

                    // Send GameStarted to joiner
                    let msg = ServerMessage::GameStarted {
                        state: filtered_state,
                        your_player: PlayerId(1),
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // Send GameStarted to creator via channel
                    let p0_state = server_core::filter_state_for_player(
                        &mgr.sessions.get(&game_code).unwrap().state,
                        PlayerId(0),
                    );
                    if let Some(p0_sender) = conns.get(&game_code).and_then(|m| m.get(&PlayerId(0)))
                    {
                        let _ = p0_sender.send(ServerMessage::GameStarted {
                            state: p0_state,
                            your_player: PlayerId(0),
                        });
                    }
                }
                Err(e) => {
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

            let mut mgr = state.lock().await;
            match mgr.handle_action(&game_code, &player_token, action) {
                Ok((p0_state, p1_state, events)) => {
                    let conns = connections.lock().await;
                    if let Some(players) = conns.get(&game_code) {
                        if let Some(s) = players.get(&PlayerId(0)) {
                            let _ = s.send(ServerMessage::StateUpdate {
                                state: p0_state,
                                events: events.clone(),
                            });
                        }
                        if let Some(s) = players.get(&PlayerId(1)) {
                            let _ = s.send(ServerMessage::StateUpdate {
                                state: p1_state,
                                events,
                            });
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
            let mut mgr = state.lock().await;
            match mgr.handle_reconnect(&game_code, &player_token) {
                Ok(filtered_state) => {
                    let session = mgr.sessions.get(&game_code).unwrap();
                    let player = session.player_for_token(&player_token).unwrap();

                    identity.game_code = Some(game_code.clone());
                    identity.player_id = Some(player);
                    identity.player_token = Some(player_token);

                    // Re-register in connections
                    let mut conns = connections.lock().await;
                    conns
                        .entry(game_code.clone())
                        .or_default()
                        .insert(player, tx.clone());

                    let msg = ServerMessage::GameStarted {
                        state: filtered_state,
                        your_player: player,
                    };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }

                    // Notify opponent
                    let opponent = PlayerId(1 - player.0);
                    if let Some(opp_sender) = conns.get(&game_code).and_then(|m| m.get(&opponent)) {
                        let _ = opp_sender.send(ServerMessage::OpponentReconnected);
                    }
                }
                Err(e) => {
                    let msg = ServerMessage::Error { message: e };
                    if let Ok(json) = serde_json::to_string(&msg) {
                        let _ = socket.send(Message::text(json)).await;
                    }
                }
            }
        }
    }
}
