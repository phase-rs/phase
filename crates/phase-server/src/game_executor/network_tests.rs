//! Exercise ordered Full publications through real WebSocket connections.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::format::FormatConfig;
use engine::types::player::PlayerId;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};
use server_core::protocol::{ClientMessage, DeckChoice, DeckData, ServerMessage};
use server_core::session::GameSession;
use server_core::FullSessionKey;

use crate::game_submission_tests::{
    basic_land_test_database, connect_and_hello, reconnect_started_game,
};
use crate::issue_4548_full_create_tests::{recv_server_message, send_test_message, spawn_server};
use crate::socket_test_hooks::run_async_test;

use super::GameExecutor;

type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct NativeTable {
    url: String,
    server: tokio::task::JoinHandle<()>,
    _directory: tempfile::TempDir,
    app: crate::AppState,
    key: FullSessionKey,
    tokens: Vec<String>,
}

impl NativeTable {
    async fn new() -> Self {
        let context = crate::ServerContext {
            game_executor: GameExecutor::new(NonZeroUsize::MIN),
            ..Default::default()
        };
        let (url, server, directory, app) = spawn_server(
            crate::ServerMode::Full,
            crate::draft_pools::DraftPools::default(),
            Arc::new(basic_land_test_database()),
            context,
        )
        .await;
        let (key, tokens) = {
            let names = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let deck = crate::resolve_deck(&app.db, &names).unwrap();
            let mut manager = app.sessions.lock().await;
            let (code, token) = manager
                .create_game_n_players_with_code(
                    deck.clone(),
                    Some(DeckChoice::DeckList(Box::new(names.clone()))),
                    "host".to_string(),
                    None,
                    4,
                    Default::default(),
                    Some(FormatConfig::free_for_all()),
                    None,
                )
                .unwrap();
            let session = manager.session_exclusive(&code).unwrap();
            let mut tokens = Vec::with_capacity(4);
            tokens.push(token);
            for name in ["guest-one", "guest-two", "guest-three"] {
                let (token, _) = session
                    .join_with_reservation(
                        deck.clone(),
                        Some(DeckChoice::DeckList(Box::new(names.clone()))),
                        name.to_string(),
                        None,
                    )
                    .unwrap();
                tokens.push(token);
            }
            session.set_card_names(app.db.card_names());
            session.start_game(&app.db).unwrap();
            let key = crate::bind_full_session_key(&app.game_db, &code).unwrap();
            crate::initialize_full_runtime(&app.game_db, session, key.clone()).unwrap();
            (key, tokens)
        };
        Self {
            url,
            server,
            _directory: directory,
            app,
            key,
            tokens,
        }
    }

    async fn connect(&self, player: PlayerId) -> ClientSocket {
        let mut socket = connect_and_hello(self.url.clone()).await;
        reconnect_started_game(
            &mut socket,
            &self.key.game_code,
            &self.tokens[usize::from(player.0)],
            self.key.clone(),
        )
        .await;
        socket
    }

    async fn keep_all(&self) {
        let session = crate::lock_session(&self.app.sessions, &self.key.game_code)
            .await
            .unwrap();
        let (lease, ()) = self
            .app
            .context
            .game_executor
            .with_session(session, |session| {
                for token in session.player_tokens.clone() {
                    session
                        .handle_action_with_card_db_outcome(
                            &token,
                            GameAction::MulliganDecision {
                                choice: MulliganChoice::Keep,
                            },
                            None,
                        )
                        .unwrap();
                    session.advance_state_revision();
                }
            })
            .await;
        drop(lease);
    }
}

fn enable_ai(session: &mut GameSession, players: impl IntoIterator<Item = PlayerId>) {
    for player in players {
        session.ai_seats.insert(player);
        session.ai_configs.insert(
            player,
            create_config_for_players(AiDifficulty::Easy, Platform::Native, 4),
        );
    }
}

#[test]
fn fresh_reconnect_never_replays_ai_history_older_than_its_bootstrap() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let mut host = table.connect(PlayerId(0)).await;
            let mut old = table.connect(PlayerId(3)).await;
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let (lease, initial_revision) = table
                .app
                .context
                .game_executor
                .with_session(session, |session| {
                    enable_ai(session, [PlayerId(1), PlayerId(2)]);
                    session.state_revision
                })
                .await;
            drop(lease);
            let gate = table.app.context.game_executor.arm_historical_publication();
            send_test_message(
                &mut host,
                &ClientMessage::Action {
                    action: GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    },
                },
                false,
            )
            .await;
            gate.entered.notified().await;

            let mut returning = connect_and_hello(table.url.clone()).await;
            send_test_message(
                &mut returning,
                &ClientMessage::Reconnect {
                    game_code: table.key.game_code.clone(),
                    player_token: table.tokens[3].clone(),
                    full_key: table.key.clone(),
                },
                false,
            )
            .await;
            send_test_message(
                &mut returning,
                &ClientMessage::Ping { timestamp: 91 },
                false,
            )
            .await;
            let mut bootstrap = None;
            let mut updates = Vec::new();
            // Allow an early bootstrap in the negative control: the defect is
            // the older history delivered AFTER it, not merely the lease gap.
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::Pong { timestamp: 91 } => break,
                    ServerMessage::GameStarted {
                        state_revision,
                        legal_actions,
                        ..
                    } => {
                        bootstrap = Some((state_revision, legal_actions));
                    }
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        updates.push(state_revision)
                    }
                    ServerMessage::Error { message, .. } => panic!("reconnect: {message}"),
                    _ => {}
                }
            }
            gate.release.notify_one();
            while bootstrap.is_none() {
                match recv_server_message(&mut returning).await {
                    ServerMessage::GameStarted {
                        state_revision,
                        legal_actions,
                        ..
                    } => {
                        bootstrap = Some((state_revision, legal_actions));
                    }
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        updates.push(state_revision)
                    }
                    ServerMessage::Error { message, .. } => panic!("reconnect: {message}"),
                    _ => {}
                }
            }
            let (revision, legal_actions) = bootstrap.unwrap();
            assert!(
                revision >= initial_revision + 3,
                "the real human action must drive several AI transitions"
            );
            // Reading the initiator's final frame proves that every historical
            // transition has been enqueued before the drain delimiter below.
            loop {
                match recv_server_message(&mut host).await {
                    ServerMessage::StateUpdate { state_revision, .. }
                        if state_revision >= revision =>
                    {
                        break
                    }
                    ServerMessage::ActionFailed { message } => panic!("human action: {message}"),
                    ServerMessage::ActionRejected { rejection } => {
                        panic!("human action: {rejection:?}")
                    }
                    _ => {}
                }
            }
            send_test_message(&mut returning, &ClientMessage::SubscribeLobby, false).await;
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::LobbyUpdate { .. } => break,
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        updates.push(state_revision)
                    }
                    ServerMessage::Error { message, .. } => panic!("drain: {message}"),
                    _ => {}
                }
            }
            assert!(
                updates.iter().all(|seen| *seen >= revision),
                "a fresh socket rolled back after GameStarted {revision}: {updates:?}"
            );
            old.close(None).await.unwrap();
            let action = legal_actions
                .into_iter()
                .find(|action| {
                    matches!(
                        action,
                        GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep
                        }
                    )
                })
                .expect("the reconnected human still owes its opening decision");
            send_test_message(&mut returning, &ClientMessage::Action { action }, false).await;
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::StateUpdate {
                        state,
                        state_revision,
                        ..
                    } => {
                        assert!(state_revision > revision);
                        if let engine::types::game_state::WaitingFor::MulliganDecision {
                            pending,
                            ..
                        } = state.waiting_for
                        {
                            assert!(pending.iter().all(|entry| entry.player != PlayerId(3)));
                        }
                        break;
                    }
                    ServerMessage::ActionFailed { message } => panic!("new owner: {message}"),
                    ServerMessage::ActionRejected { rejection } => {
                        panic!("new owner: {rejection:?}")
                    }
                    _ => {}
                }
            }
        })
        .await;
        table.server.abort();
        outcome.expect("gated AI publication or fresh reconnect stalled");
    });
}

#[derive(Clone, Copy)]
enum EliminationPublication {
    Concede,
    AiReconnect,
}

#[test]
fn elimination_metadata_survives_native_concession() {
    assert_elimination_publication(EliminationPublication::Concede);
}

#[test]
fn elimination_metadata_survives_ai_reconnect_fanout() {
    assert_elimination_publication(EliminationPublication::AiReconnect);
}

fn assert_elimination_publication(path: EliminationPublication) {
    run_async_test(move || async move {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            table.keep_all().await;
            let (mut viewer, _returning) = match path {
                EliminationPublication::Concede => (table.connect(PlayerId(0)).await, None),
                EliminationPublication::AiReconnect => {
                    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                        .await
                        .unwrap();
                    let (lease, (ai, returning, observer)) = table
                        .app
                        .context
                        .game_executor
                        .with_session(session, |session| {
                            let token = session.player_tokens[2].clone();
                            session
                                .handle_action_with_card_db_outcome(
                                    &token,
                                    GameAction::Concede {
                                        player_id: PlayerId(2),
                                    },
                                    None,
                                )
                                .unwrap();
                            session.advance_state_revision();
                            let ai = server_core::acting_players(&session.state)[0];
                            let mut humans = [PlayerId(0), PlayerId(1), PlayerId(3)]
                                .into_iter()
                                .filter(|player| *player != ai);
                            (ai, humans.next().unwrap(), humans.next().unwrap())
                        })
                        .await;
                    drop(lease);
                    let observer_socket = table.connect(observer).await;
                    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                        .await
                        .unwrap();
                    let (lease, ()) = table
                        .app
                        .context
                        .game_executor
                        .with_session(session, move |session| {
                            enable_ai(session, [ai]);
                        })
                        .await;
                    drop(lease);
                    let returning_socket = table.connect(returning).await;
                    (observer_socket, Some(returning_socket))
                }
            };
            let _conceding = if matches!(path, EliminationPublication::Concede) {
                let mut socket = table.connect(PlayerId(2)).await;
                send_test_message(&mut socket, &ClientMessage::Concede, false).await;
                Some(socket)
            } else {
                None
            };
            loop {
                match recv_server_message(&mut viewer).await {
                    ServerMessage::StateUpdate {
                        eliminated_players, ..
                    } => {
                        assert_eq!(
                            eliminated_players,
                            vec![PlayerId(2)],
                            "the client must see the conceded seat eliminated after publication"
                        );
                        break;
                    }
                    ServerMessage::ActionFailed { message } => panic!("concede: {message}"),
                    ServerMessage::Error { message, .. } => panic!("publication: {message}"),
                    _ => {}
                }
            }
        })
        .await;
        table.server.abort();
        outcome.expect("the real elimination publication stalled");
    });
}

async fn receive_command_frame(socket: &mut ClientSocket) -> serde_json::Value {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
        match socket
            .next()
            .await
            .expect("live command socket")
            .expect("valid frame")
        {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            frame => panic!("command test expected plain JSON, got {frame:?}"),
        }
    }
}

async fn send_command_frame(socket: &mut ClientSocket, action: &GameAction, command_id: u64) {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "Action",
                "command_id": command_id,
                "data": { "action": action },
            })
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

fn owns_command_completion(frame: &serde_json::Value, command_id: u64) -> bool {
    frame.get("command_id").and_then(serde_json::Value::as_u64) == Some(command_id)
}

#[test]
fn four_human_mulligans_settle_only_the_originating_commands() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let mut sockets = Vec::with_capacity(4);
            let mut keeps = Vec::with_capacity(4);
            for player in 0..4 {
                let mut socket = connect_and_hello(table.url.clone()).await;
                send_test_message(
                    &mut socket,
                    &ClientMessage::Reconnect {
                        game_code: table.key.game_code.clone(),
                        player_token: table.tokens[player].clone(),
                        full_key: table.key.clone(),
                    },
                    false,
                )
                .await;
                loop {
                    let frame = receive_command_frame(&mut socket).await;
                    if frame["type"] == "GameStarted" {
                        assert_eq!(frame["data"]["your_player"], player);
                        let legal: Vec<GameAction> =
                            serde_json::from_value(frame["data"]["legal_actions"].clone()).unwrap();
                        keeps.push(
                            legal
                                .into_iter()
                                .find(|action| {
                                    matches!(
                                        action,
                                        GameAction::MulliganDecision {
                                            choice: MulliganChoice::Keep
                                        }
                                    )
                                })
                                .expect("each human receives its own opening decision"),
                        );
                        break;
                    }
                }
                sockets.push(socket);
            }

            // All four clients have submitted, but none can have applied yet.
            // The first publication is therefore ambient for three clients.
            let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            for (player, socket) in sockets.iter_mut().enumerate() {
                send_command_frame(socket, &keeps[player], 100 + player as u64).await;
            }
            drop(lease);
            let mut pending = [true; 4];
            let mut own_completions = [0; 4];
            let mut first_revision = None;
            let mut ambient = 0;
            for (player, socket) in sockets.iter_mut().enumerate() {
                loop {
                    let frame = receive_command_frame(socket).await;
                    if frame["type"] != "StateUpdate" {
                        continue;
                    }
                    let revision = frame["data"]["state_revision"].as_u64().unwrap();
                    assert_eq!(*first_revision.get_or_insert(revision), revision);
                    let is_own = frame.get("command_id").is_some();
                    if is_own {
                        assert!(owns_command_completion(&frame, 100 + player as u64));
                        own_completions[player] += 1;
                    } else {
                        ambient += 1;
                    }
                    if owns_command_completion(&frame, 100 + player as u64) {
                        pending[player] = false;
                    }
                    // Model the actual client bug: releasing the slot on an
                    // ambient frame permits its still-legal Keep to be sent a
                    // second time, behind this socket's original command.
                    let legal: Vec<GameAction> =
                        serde_json::from_value(frame["data"]["legal_actions"].clone()).unwrap();
                    if !pending[player] && legal.iter().any(|action| action == &keeps[player]) {
                        send_command_frame(socket, &keeps[player], 200 + player as u64).await;
                    }
                    break;
                }
            }
            assert_eq!(
                ambient, 3,
                "simultaneous decisions must expose the peer-update window"
            );
            let mut rejections = Vec::new();
            for socket in &mut sockets {
                // Application FIFO, not Pong, is the authoritative drain fence.
                send_test_message(socket, &ClientMessage::SubscribeLobby, false).await;
            }
            for (player, socket) in sockets.iter_mut().enumerate() {
                loop {
                    let frame = receive_command_frame(socket).await;
                    match frame["type"].as_str().unwrap() {
                        "LobbyUpdate" => break,
                        "StateUpdate" => {
                            if let Some(id) =
                                frame.get("command_id").and_then(serde_json::Value::as_u64)
                            {
                                assert_eq!(
                                    id,
                                    100 + player as u64,
                                    "a peer must never receive this echo"
                                );
                                pending[player] = false;
                                own_completions[player] += 1;
                            }
                        }
                        "ActionRejected" | "ActionFailed" | "Error" => rejections.push(frame),
                        _ => {}
                    }
                }
            }
            assert!(
                rejections.is_empty(),
                "ambient ACKs replayed opening commands: {rejections:?}"
            );
            assert_eq!(pending, [false; 4]);
            assert_eq!(own_completions, [1; 4]);
            for socket in &mut sockets {
                socket.close(None).await.unwrap();
            }
        })
        .await;
        table.server.abort();
        outcome.expect("four-human command correlation must finish");
    });
}

async fn queue_named_revision(table: &NativeTable, name: String) -> u64 {
    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
        .await
        .unwrap();
    let db = table.app.game_db.clone();
    let executor = table.app.context.game_executor.clone();
    let persistence_executor = executor.clone();
    let (lease, revision) = executor
        .with_session(session, move |session| {
            session.display_names[0] = name;
            let revision = session.advance_state_revision();
            crate::persist_full_session_async(&persistence_executor, &db, session);
            revision
        })
        .await;
    drop(lease);
    revision
}

async fn persistence_pool_probe(table: &NativeTable) -> Arc<str> {
    let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
        .await
        .unwrap();
    let (lease, pools) = table
        .app
        .context
        .game_executor
        .with_session(session, |session| {
            session.full_persist_snapshot().unwrap().deck_pools_json
        })
        .await;
    drop(lease);
    pools
}

#[test]
fn pending_persistence_retains_one_snapshot_and_saves_the_latest_revision() {
    run_async_test(|| async {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let pools = persistence_pool_probe(&table).await;
            let baseline = Arc::strong_count(&pools);
            let writes = Arc::new(AtomicUsize::new(0));
            let entered = Arc::new(tokio::sync::Notify::new());
            let completed = Arc::new(tokio::sync::Notify::new());
            let (release, blocked) = std::sync::mpsc::channel();
            let blocked = parking_lot::Mutex::new(Some(blocked));
            table.app.game_db.set_full_save_io_hook(Arc::new({
                let writes = writes.clone();
                let entered = entered.clone();
                let completed = completed.clone();
                move |_, _| {
                    writes.fetch_add(1, Ordering::SeqCst);
                    if let Some(blocked) = blocked.lock().take() {
                        entered.notify_one();
                        blocked.recv().unwrap();
                    } else {
                        completed.notify_one();
                    }
                }
            }));
            queue_named_revision(&table, "first".to_string()).await;
            entered.notified().await;
            let mut latest = 0;
            for revision in 1..=100 {
                latest = queue_named_revision(&table, format!("latest-{revision}")).await;
            }
            // Each eager pending FullPersistSnapshot retains this real shared
            // pool buffer. One prepared write may retain it; 100 queued saves
            // must not retain 100 complete game states behind that write.
            let retained = Arc::strong_count(&pools);
            release.send(()).unwrap();
            completed.notified().await;
            let db = table.app.game_db.clone();
            let loaded =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap();
            let saved = loaded
                .into_iter()
                .find(|saved| saved.key == table.key)
                .unwrap();
            assert!(
                retained <= baseline + 1,
                "pending saves retained {} extra game snapshots behind one writer",
                retained.saturating_sub(baseline),
            );
            assert_eq!(saved.mutation_revision, latest);
            assert_eq!(saved.persisted.display_names[0], "latest-100");
            assert_eq!(
                writes.load(Ordering::SeqCst),
                2,
                "only the in-flight and latest saves should reach SQLite"
            );
        })
        .await;
        table.server.abort();
        outcome.expect("coalesced persistence must drain");
    });
}

#[test]
fn terminal_retirement_cancels_pending_saves_before_serialization() {
    run_async_test(|| async {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let pools = persistence_pool_probe(&table).await;
            let baseline = Arc::strong_count(&pools);
            let serialized = Arc::new(AtomicUsize::new(0));
            table.app.game_db.set_full_save_hook(Arc::new({
                let serialized = serialized.clone();
                move |_| {
                    serialized.fetch_add(1, Ordering::SeqCst);
                }
            }));
            let held_io = table.app.context.game_executor.admit_persistence_io().await;
            for revision in 1..=20 {
                queue_named_revision(&table, format!("pending-{revision}")).await;
            }
            let mut sockets = Vec::with_capacity(4);
            for player in 0..4 {
                sockets.push(table.connect(PlayerId(player)).await);
            }
            for socket in sockets.iter_mut().take(3) {
                send_test_message(socket, &ClientMessage::Concede, false).await;
                loop {
                    let frame = receive_command_frame(socket).await;
                    match frame["type"].as_str().unwrap() {
                        "Conceded" => break,
                        "Error" | "ActionFailed" | "ActionRejected" => {
                            panic!("concede refused: {frame}")
                        }
                        _ => {}
                    }
                }
            }
            loop {
                let frame = receive_command_frame(&mut sockets[3]).await;
                if frame["type"] == "TerminalResult" {
                    break;
                }
            }
            drop(held_io);
            while Arc::strong_count(&pools) > baseline {
                tokio::task::yield_now().await;
            }
            let db = table.app.game_db.clone();
            let active =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap();
            assert!(active.iter().all(|snapshot| snapshot.key != table.key));
            assert_eq!(
                serialized.load(Ordering::SeqCst),
                0,
                "retired pending snapshots must not be serialized and rejected later"
            );
        })
        .await;
        table.server.abort();
        outcome.expect("retirement must cancel the persistence backlog");
    });
}
