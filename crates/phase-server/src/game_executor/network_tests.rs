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
    basic_land_test_database, connect_and_hello, connect_and_hello_with, reconnect_started_game,
};
use crate::issue_4548_full_create_tests::{recv_server_message, send_test_message, spawn_server};
use crate::socket_test_hooks::run_async_test;

use super::GameExecutor;

type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
async fn receive_wire_json(socket: &mut ClientSocket) -> String {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
        let json = match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => text.to_string(),
            // Server snapshots are not subject to the 64 KiB C->S command cap.
            Message::Binary(bytes) => {
                crate::wire::decode_client_envelope(bytes.to_vec(), 16 * 1024 * 1024)
                    .await
                    .unwrap()
            }
            Message::Ping(_) | Message::Pong(_) => continue,
            frame => panic!("expected an application reply on a live socket, got {frame:?}"),
        };
        return json;
    }
}

async fn receive_wire_message(socket: &mut ClientSocket) -> ServerMessage {
    serde_json::from_str(&receive_wire_json(socket).await).unwrap()
}

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
        Self::with_parallelism(NonZeroUsize::MIN).await
    }

    async fn with_parallelism(parallelism: NonZeroUsize) -> Self {
        Self::with_executor(GameExecutor::new(parallelism)).await
    }

    async fn with_executor(game_executor: GameExecutor) -> Self {
        Self::seated(game_executor, 4, FormatConfig::free_for_all()).await
    }

    async fn duel() -> Self {
        Self::seated(
            GameExecutor::new(NonZeroUsize::MIN),
            2,
            FormatConfig::standard(),
        )
        .await
    }

    /// A duel whose encoder writes no application frame over `max_frame_bytes`.
    async fn capped_duel(max_frame_bytes: usize) -> Self {
        let context = crate::ServerContext {
            limits: crate::Limits {
                max_frame_bytes,
                ..Default::default()
            },
            game_executor: GameExecutor::new(NonZeroUsize::MIN),
            ..Default::default()
        };
        Self::in_context(context, 2, FormatConfig::standard()).await
    }

    async fn seated(game_executor: GameExecutor, seats: u8, format: FormatConfig) -> Self {
        let context = crate::ServerContext {
            game_executor,
            ..Default::default()
        };
        Self::in_context(context, seats, format).await
    }

    async fn in_context(context: crate::ServerContext, seats: u8, format: FormatConfig) -> Self {
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
                    seats,
                    Default::default(),
                    Some(format),
                    None,
                )
                .unwrap();
            let session = manager.session_exclusive(&code).unwrap();
            let mut tokens = Vec::with_capacity(usize::from(seats));
            tokens.push(token);
            for name in ["guest-one", "guest-two", "guest-three"]
                .into_iter()
                .take(usize::from(seats) - 1)
            {
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
        self.dial(player, None).await
    }

    /// A seat whose socket takes frames of any size, as the desktop bridge's does.
    async fn connect_uncapped(&self, player: PlayerId) -> ClientSocket {
        let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        self.dial(player, Some(config)).await
    }

    /// A seat whose socket refuses frames over `cap`, as the desktop bridge's does.
    async fn connect_bridged(&self, player: PlayerId, cap: usize) -> ClientSocket {
        self.dial(player, Some(bridge_config(cap))).await
    }

    async fn dial(
        &self,
        player: PlayerId,
        config: Option<tokio_tungstenite::tungstenite::protocol::WebSocketConfig>,
    ) -> ClientSocket {
        let mut socket = connect_and_hello_with(self.url.clone(), config).await;
        reconnect_started_game(
            &mut socket,
            &self.key.game_code,
            &self.tokens[usize::from(player.0)],
            self.key.clone(),
        )
        .await;
        socket
    }

    async fn connect_with_format(
        &self,
        player: PlayerId,
        format: Option<server_core::protocol::WireFormat>,
    ) -> ClientSocket {
        let Some(format) = format else {
            return self.connect(player).await;
        };
        let (mut socket, _) = tokio_tungstenite::connect_async(&self.url).await.unwrap();
        while !matches!(
            recv_server_message(&mut socket).await,
            ServerMessage::ServerHello { .. }
        ) {}
        send_test_message(
            &mut socket,
            &crate::issue_4548_full_create_tests::test_client_hello(vec![format]),
            false,
        )
        .await;
        send_test_message(
            &mut socket,
            &ClientMessage::Reconnect {
                game_code: self.key.game_code.clone(),
                player_token: self.tokens[usize::from(player.0)].clone(),
                full_key: self.key.clone(),
            },
            true,
        )
        .await;
        loop {
            match receive_wire_message(&mut socket).await {
                ServerMessage::GameStarted { your_player, .. } => {
                    assert_eq!(your_player, player);
                    break;
                }
                ServerMessage::Error { message, .. } => {
                    panic!("formatted reconnect failed: {message}")
                }
                _ => {}
            }
        }
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

fn bridge_config(cap: usize) -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(cap))
        .max_frame_size(Some(cap))
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
fn queued_reconnect_before_departure_keeps_contest_presence_causal() {
    run_async_test(|| async {
        use engine::types::contest::{
            ContestConfig, ContestKind, ContestPhase, ContestState, ContestantConnection,
        };
        use engine::types::game_state::WaitingFor;

        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(20), async {
            table.keep_all().await;
            let mut old = table.connect(PlayerId(0)).await;
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let (lease, ()) = table
                .app
                .context
                .game_executor
                .with_session(session, |session| {
                    let config = ContestConfig {
                        countdown_ms: 0,
                        inactivity_timeout_ms: 60_000,
                        reconnect_grace_ms: 100,
                        settle_window_ms: 0,
                        max_active_ms: 60_000,
                    };
                    session.contest.policy.config = config;
                    session.state.phase = engine::types::phase::Phase::PreCombatMain;
                    session.state.active_player = PlayerId(0);
                    let source_id = *session.state.objects.keys().next().unwrap();
                    session.state.waiting_for = WaitingFor::PhysicalContest {
                        contest: Box::new(ContestState::new(
                            91,
                            ContestKind::Staring,
                            PlayerId(0),
                            PlayerId(1),
                            config,
                        )),
                        source_id,
                    };
                    session.tick_contest_at(std::time::Instant::now());
                    session.drain_contest_outbox();
                    // Keep the autonomous driver from consuming either edge
                    // until the replacement has completed its real bootstrap.
                    assert!(session.claim_contest_driver());
                })
                .await;
            drop(lease);

            let work = table.app.context.socket_hooks.arm_parser_work();
            let mut replacement = connect_and_hello(table.url.clone()).await;
            work.wait_completed(1).await;
            let lobby = table.app.lobby.lock().await;
            send_test_message(&mut replacement, &ClientMessage::SubscribeLobby, false).await;
            send_test_message(
                &mut replacement,
                &ClientMessage::Reconnect {
                    game_code: table.key.game_code.clone(),
                    player_token: table.tokens[0].clone(),
                    full_key: table.key.clone(),
                },
                false,
            )
            .await;
            // Decoder completion, not Pong, proves that both ordinary commands
            // have been retained in FIFO order behind the held lobby dispatch.
            work.wait_completed(3).await;
            // Keep the host's millisecond receipt/death stamps distinct without
            // relying on a race between worker scheduling and socket cleanup.
            tokio::time::sleep(Duration::from_millis(5)).await;
            old.close(None).await.unwrap();
            drop(old);
            loop {
                let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let away = !lease.connected[0];
                drop(lease);
                let recorded = table
                    .app
                    .sessions
                    .lock()
                    .await
                    .reconnect
                    .is_disconnected(&table.key.game_code, PlayerId(0));
                let unbound = table
                    .app
                    .connections
                    .lock()
                    .await
                    .get(&table.key.game_code)
                    .is_none_or(|players| !players.contains_key(&PlayerId(0)));
                if away && recorded && unbound {
                    break;
                }
                tokio::task::yield_now().await;
            }
            drop(lobby);
            loop {
                match receive_wire_message(&mut replacement).await {
                    ServerMessage::GameStarted { your_player, .. } => {
                        assert_eq!(your_player, PlayerId(0));
                        break;
                    }
                    ServerMessage::Error { message, .. } => panic!("replacement: {message}"),
                    _ => {}
                }
            }

            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let (lease, before_tick) = table
                .app
                .context
                .game_executor
                .with_session(session, |session| {
                    assert!(session.connected[0]);
                    session.release_contest_driver();
                    session.state_revision
                })
                .await;
            drop(lease);
            crate::ensure_contest_driver(
                &table.app.context.game_executor,
                &table.app.sessions,
                &table.app.connections,
                &table.app.draft_sessions,
                &table.app.game_db,
                &table.app.game_spectators,
                &table.app.lobby,
                &table.app.lobby_subscribers,
                &table.key.game_code,
            )
            .await;
            let mut departure = None;
            let (departed_at, resumed_revision) = loop {
                match receive_wire_message(&mut replacement).await {
                    ServerMessage::StateUpdate {
                        state_revision,
                        state,
                        ..
                    } if state_revision > before_tick => {
                        let WaitingFor::PhysicalContest { contest, .. } = &state.waiting_for else {
                            panic!("the accepted replacement must not forfeit");
                        };
                        assert_eq!(contest.id, 91);
                        match contest.contestants[0].connection {
                            ContestantConnection::Disconnected { since_ms } => {
                                assert_eq!(contest.paused_since_ms, Some(since_ms));
                                departure = Some((state_revision, since_ms));
                            }
                            ContestantConnection::Connected => {
                                let Some((departure_revision, departed_at)) = departure else {
                                    continue;
                                };
                                assert!(state_revision > departure_revision);
                                assert!(contest.clock_ms >= departed_at);
                                assert_eq!(contest.paused_since_ms, None);
                                break (departed_at, state_revision);
                            }
                        }
                    }
                    ServerMessage::Error { message, .. } => {
                        panic!("contest publication: {message}");
                    }
                    _ => {}
                }
            };
            // A revision bump alone could precede the driver. Ordered departure
            // AND return snapshots positively prove that both queued edges ran.
            {
                let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let WaitingFor::PhysicalContest { contest, .. } = &lease.state.waiting_for else {
                    panic!("the published return must remain active");
                };
                assert!(lease.state_revision >= resumed_revision);
                assert!(lease.connected[0]);
                assert!(contest.clock_ms >= departed_at);
                assert_eq!(
                    contest.contestants[0].connection,
                    ContestantConnection::Connected
                );
                assert_eq!(contest.paused_since_ms, None);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
            let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let WaitingFor::PhysicalContest { contest, .. } = &lease.state.waiting_for else {
                panic!("no stale grace may forfeit the replacement later");
            };
            assert_eq!(contest.phase, ContestPhase::Active);
            assert_eq!(
                contest.contestants[0].connection,
                ContestantConnection::Connected
            );
        })
        .await;
        table.server.abort();
        outcome.expect("queued receipt before old-route cleanup must remain a live contest seat");
    });
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

async fn assert_quota_refusal_backlog_is_bounded(payload: serde_json::Value) {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let table = NativeTable::new().await;
    let gate = table.app.context.socket_hooks.arm_bootstrap();
    let outcome = tokio::time::timeout(Duration::from_secs(8), async {
        let mut socket = connect_and_hello(table.url.clone()).await;
        send_test_message(
            &mut socket,
            &ClientMessage::Reconnect {
                game_code: table.key.game_code.clone(),
                player_token: table.tokens[0].clone(),
                full_key: table.key.clone(),
            },
            false,
        )
        .await;
        // This encoder barrier holds ALL application replies before the kernel
        // sink, so the bound does not depend on platform socket-buffer capacity.
        gate.entered.notified().await;
        for _ in 0..crate::RATE_LIMIT_MESSAGES {
            send_test_message(&mut socket, &ClientMessage::SubscribeLobby, false).await;
        }
        for id in 0..crate::socket_mailbox::MAX_PENDING_QUOTA_REFUSALS {
            let mut frame = payload.clone();
            frame["command_id"] = serde_json::json!(100 + id);
            socket
                .send(Message::Text(frame.to_string().into()))
                .await
                .unwrap();
        }
        send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 91 }, false).await;
        loop {
            if matches!(
                recv_server_message(&mut socket).await,
                ServerMessage::Pong { timestamp: 91 }
            ) {
                break;
            }
        }
        let mut frame = payload;
        frame["command_id"] = serde_json::json!(999);
        socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .unwrap();
        loop {
            match socket.next().await.unwrap().unwrap() {
                Message::Close(Some(close)) => {
                    assert_eq!(u16::from(close.code), 1013);
                    break;
                }
                Message::Ping(_) | Message::Pong(_) | Message::Text(_) => {}
                frame => panic!("unexpected refusal transport frame: {frame:?}"),
            }
        }
        assert_eq!(
            table
                .app
                .context
                .metrics
                .ws_disconnect_count(crate::socket_disconnect::Cause::QuotaRefusalOverflow),
            1,
        );
    })
    .await;
    gate.release.notify_one();
    table.server.abort();
    outcome.expect("unflushed quota replies must close with a bounded backlog");
}

#[test]
fn correlated_quota_refusals_bound_the_slow_consumer_backlog() {
    run_async_test(|| async {
        assert_quota_refusal_backlog_is_bounded(serde_json::json!({
            "type": "Action", "data": { "action": { "type": "PassPriority" } },
        }))
        .await;
    });
}

#[test]
fn malformed_correlated_quota_refusals_bound_the_slow_consumer_backlog() {
    run_async_test(|| async {
        assert_quota_refusal_backlog_is_bounded(serde_json::json!({
            "type": "Action", "data": { "action": { "type": "NotAnAction" } },
        }))
        .await;
    });
}

#[test]
fn captured_checkpoints_keep_fifo_admission_during_sustained_mutation() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let executor = table.app.context.game_executor.clone();
            let held_io = executor.admit_persistence_io().await;
            let queued = executor.arm_persistence_admission();
            let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                .await
                .unwrap();
            let db = table.app.game_db.clone();
            let persistence_executor = executor.clone();
            let (lease, captured_revision) = executor
                .with_session(session, move |session| {
                    session.advance_state_revision();
                    crate::persist_full_session_async(&persistence_executor, &db, session);
                    // A revision advance without another save request must not
                    // invalidate the already-requested checkpoint.
                    session.display_names[0] = "current-without-another-request".to_string();
                    session.advance_state_revision()
                })
                .await;
            drop(lease);
            queued.notified().await;

            let observation = executor.admit_persistence_io();
            tokio::pin!(observation);
            tokio::select! {
                biased;
                _ = &mut observation => panic!("the test still holds I/O admission"),
                _ = tokio::task::yield_now() => {}
            }
            for revision in 0..32 {
                queue_named_revision(&table, format!("still-mutating-{revision}")).await;
            }
            drop(held_io);
            // This waiter was queued after the checkpoint. It can acquire only
            // after that checkpoint commits, not after an abandon-and-requeue.
            let io = observation.await;
            let db = table.app.game_db.clone();
            let loaded =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap();
            let saved = loaded
                .into_iter()
                .find(|save| save.key == table.key)
                .unwrap();
            let actual = saved.mutation_revision;
            let names = saved.persisted.display_names;
            drop(io);
            assert_eq!(
                actual, captured_revision,
                "mutations moved the captured checkpoint behind a later I/O waiter",
            );
            assert_eq!(names[0], "current-without-another-request");
        })
        .await;
        table.server.abort();
        outcome.expect("pre-serialization mutation must not starve FIFO checkpoint progress");
    });
}

#[test]
fn a_busy_game_lease_does_not_block_another_games_durable_checkpoint() {
    run_async_test(|| async {
        let executor = GameExecutor::new(NonZeroUsize::MIN);
        let a = NativeTable::with_executor(executor.clone()).await;
        let b = NativeTable::with_executor(executor.clone()).await;
        let session = crate::lock_session(&a.app.sessions, &a.key.game_code)
            .await
            .unwrap();
        let db = a.app.game_db.clone();
        let persistence_executor = executor.clone();
        let lease_wait = executor.arm_persistence_lease();
        let (lease, ()) = executor
            .with_session(session, move |session| {
                session.advance_state_revision();
                crate::persist_full_session_async(&persistence_executor, &db, session);
            })
            .await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            lease_wait.notified().await;
            let revision = queue_named_revision(&b, "other-game-durable".to_string()).await;
            while b.app.game_db.full_saves.pending_games() != 0 {
                tokio::task::yield_now().await;
            }
            let db = b.app.game_db.clone();
            let saved =
                tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|save| save.key == b.key)
                    .unwrap();
            assert_eq!(saved.mutation_revision, revision);
            assert_eq!(saved.persisted.display_names[0], "other-game-durable");
        })
        .await;
        // Also release the lease before reporting a negative-control timeout.
        drop(lease);
        a.server.abort();
        b.server.abort();
        outcome.expect("one game's long ordering lease held global persistence I/O admission");
    });
}

#[test]
fn serialized_checkpoints_commit_during_sustained_mutation() {
    run_async_test(|| async {
        use std::sync::atomic::{AtomicUsize, Ordering};
        const ROUNDS: usize = 3;

        let table = NativeTable::with_parallelism(NonZeroUsize::new(2).unwrap()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(15), async {
            let executor = table.app.context.game_executor.clone();
            let calls = Arc::new(AtomicUsize::new(0));
            let (entered, mut preparations) = tokio::sync::mpsc::unbounded_channel();
            let (release, blocked) = std::sync::mpsc::channel();
            let blocked = parking_lot::Mutex::new(blocked);
            table
                .app
                .game_db
                .set_full_save_hook(Arc::new(move |snapshot| {
                    if calls.fetch_add(1, Ordering::SeqCst) < ROUNDS {
                        entered.send(snapshot.mutation_revision).unwrap();
                        // Dropping the sender also releases this gate on test failure.
                        let _ = blocked.lock().recv();
                    }
                }));
            queue_named_revision(&table, "checkpoint-0".to_string()).await;
            let mut observations = Vec::new();
            for round in 0..ROUNDS {
                let prepared_revision = preparations.recv().await.unwrap();
                queue_named_revision(&table, format!("checkpoint-{}", round + 1)).await;
                let admission = executor.admit_persistence_io();
                tokio::pin!(admission);
                // Enqueue behind this serializer before releasing it; the next
                // checkpoint cannot overtake our durable-read observation.
                tokio::select! {
                    biased;
                    _ = &mut admission => panic!("serialization must retain I/O admission"),
                    _ = tokio::task::yield_now() => {}
                }
                release.send(()).unwrap();
                let io = admission.await;
                let db = table.app.game_db.clone();
                let loaded =
                    tokio::task::spawn_blocking(move || db.load_active_full_sessions().unwrap())
                        .await
                        .unwrap();
                let saved = loaded
                    .into_iter()
                    .find(|save| save.key == table.key)
                    .unwrap();
                observations.push((prepared_revision, saved.mutation_revision));
                drop(io);
            }
            drop(release);
            assert!(
                observations
                    .iter()
                    .all(|(prepared, saved)| saved >= prepared),
                "newer mutations starved already-prepared durable checkpoints: {observations:?}",
            );
        })
        .await;
        table.server.abort();
        outcome.expect("durability must progress without waiting for quiescence");
    });
}

#[test]
fn persistence_preflight_waits_for_io_admission_before_using_blocking_threads() {
    crate::socket_test_hooks::run_async_test_with_blocking_limit(Some(2), || async {
        let table = NativeTable::new().await;
        let io = table.app.context.game_executor.admit_persistence_io().await;
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, blocked) = std::sync::mpsc::channel();
        let db = table.app.game_db.clone();
        let held = tokio::task::spawn_blocking({
            let entered = entered.clone();
            move || db.hold_connection_for_test(entered, blocked)
        });
        entered.notified().await;
        queue_named_revision(&table, "waiting-for-admission".to_string()).await;
        tokio::task::yield_now().await;
        // One blocking worker holds SQLite's real mutex. An unadmitted
        // currency query would consume the other and strand Hello encoding.
        let hello =
            tokio::time::timeout(Duration::from_secs(2), connect_and_hello(table.url.clone()))
                .await;
        drop(release);
        held.await.unwrap();
        drop(io);
        table.server.abort();
        assert!(
            hello.is_ok(),
            "unadmitted persistence preflight starved ServerHello"
        );
    });
}

#[test]
fn removed_session_releases_its_pending_persistence_worker() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let executor = table.app.context.game_executor.clone();
        let io = executor.admit_persistence_io().await;
        queue_named_revision(&table, "removed-before-capture".to_string()).await;
        assert_eq!(table.app.game_db.full_saves.pending_games(), 1);
        table
            .app
            .sessions
            .lock()
            .await
            .remove_game(&table.key.game_code);
        drop(io);
        let cleanup = tokio::time::timeout(Duration::from_secs(5), async {
            while table.app.game_db.full_saves.pending_games() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        table.server.abort();
        cleanup.expect("dead weak session must not strand a worker or sender entry");
    });
}

#[test]
fn retiring_old_persistence_worker_cannot_remove_its_replacement() {
    run_async_test(|| async {
        let table = NativeTable::new().await;
        let executor = table.app.context.game_executor.clone();
        let db = table.app.game_db.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, blocked) = std::sync::mpsc::channel();
        let held = tokio::task::spawn_blocking({
            let db = db.clone();
            let entered = entered.clone();
            move || db.hold_connection_for_test(entered, blocked)
        });
        entered.notified().await;
        queue_named_revision(&table, "cancelled-worker".to_string()).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while executor.persistence_io.available_permits() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old worker must enter preflight before cancellation");
        // Its real DB query cannot finish until the replacement is registered.
        // This makes stale-owner cleanup run after, not before, the cutover.
        table.app.game_db.full_saves.cancel(&table.key);
        let latest = queue_named_revision(&table, "replacement-worker".to_string()).await;
        drop(release);
        held.await.unwrap();
        let key = table.key.clone();
        let progress = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let loaded = tokio::task::spawn_blocking({
                    let db = db.clone();
                    move || db.load_active_full_sessions().unwrap()
                })
                .await
                .unwrap();
                if loaded
                    .iter()
                    .any(|save| save.key == key && save.mutation_revision == latest)
                    && db.full_saves.pending_games() == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        table.server.abort();
        progress.expect("old worker cleanup must preserve the replacement and retire on idle");
    });
}

async fn exhaust_application_quota(socket: &mut ClientSocket) {
    for _ in 0..crate::RATE_LIMIT_MESSAGES {
        send_test_message(socket, &ClientMessage::SubscribeLobby, false).await;
    }
}

async fn receive_quota_refusal(socket: &mut ClientSocket, command_id: u64) {
    loop {
        let frame = receive_command_frame(socket).await;
        if owns_command_completion(&frame, command_id) {
            assert_eq!(
                frame["type"], "ActionFailed",
                "expected quota refusal: {frame}"
            );
            return;
        }
    }
}

#[test]
fn encoded_refusal_keeps_admission_until_actual_socket_flush() {
    run_async_test(|| async {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        let table = NativeTable::new().await;
        let outcome =
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut socket = table.connect(PlayerId(0)).await;
                let count = crate::socket_mailbox::MAX_PENDING_QUOTA_REFUSALS as u64;
                exhaust_application_quota(&mut socket).await;
                let first = table.app.context.socket_hooks.arm_quota_flush();
                send_command_frame(&mut socket, &GameAction::PassPriority, 100).await;
                // The real sink has accepted the encoded frame via start_send and
                // entered poll_flush; this is not a pre-encoding mailbox barrier.
                first.entered.notified().await;
                for id in 101..100 + count {
                    send_command_frame(&mut socket, &GameAction::PassPriority, id).await;
                }
                first.release.notify_one();
                for id in 100..100 + count {
                    receive_quota_refusal(&mut socket, id).await;
                }
                // Reuse of flush-held permits is independent of the bounded
                // inspection window. Start a fresh window before exhausting again.
                tokio::time::sleep(Duration::from_secs(crate::RATE_LIMIT_WINDOW_SECS)).await;
                exhaust_application_quota(&mut socket).await;
                send_command_frame(&mut socket, &GameAction::PassPriority, 100 + count).await;
                receive_quota_refusal(&mut socket, 100 + count).await;
                assert_eq!(
                    table
                        .app
                        .context
                        .metrics
                        .ws_disconnect_count(crate::socket_disconnect::Cause::QuotaRefusalOverflow),
                    0,
                    "a flushed budget must be reusable on the same live socket",
                );

                tokio::time::sleep(Duration::from_secs(crate::RATE_LIMIT_WINDOW_SECS)).await;
                exhaust_application_quota(&mut socket).await;
                let blocked = table.app.context.socket_hooks.arm_quota_flush();
                send_command_frame(&mut socket, &GameAction::PassPriority, 200).await;
                blocked.entered.notified().await;
                for id in 201..200 + count {
                    send_command_frame(&mut socket, &GameAction::PassPriority, id).await;
                }
                send_command_frame(&mut socket, &GameAction::PassPriority, 200 + count).await;
                let overflow =
                    tokio::time::timeout(Duration::from_millis(500), async {
                        while table.app.context.metrics.ws_disconnect_count(
                            crate::socket_disconnect::Cause::QuotaRefusalOverflow,
                        ) == 0
                        {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await;
                // A control Close cannot preempt an in-flight RFC 6455 frame.
                // Release the flush before asserting, including negative controls.
                blocked.release.notify_one();
                overflow.expect("the encoded, unflushed refusal lost its admission permit");
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        Message::Close(Some(close)) => {
                            assert_eq!(u16::from(close.code), 1013);
                            break;
                        }
                        Message::Text(_) | Message::Ping(_) | Message::Pong(_) => {}
                        frame => panic!("unexpected transport frame: {frame:?}"),
                    }
                }
            })
            .await;
        table.server.abort();
        outcome.expect("actual socket flush must own refusal admission");
    });
}

#[test]
fn legitimate_forty_click_bursts_keep_plain_and_gzip_contests_running() {
    run_async_test(|| async {
        use engine::types::contest::{ContestConfig, ContestKind, ContestPhase, ContestState};
        use engine::types::game_state::WaitingFor;
        use futures_util::SinkExt;
        use server_core::protocol::WireFormat;
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let table = NativeTable::new().await;
            let work = table.app.context.socket_hooks.arm_parser_work();
            let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                table.keep_all().await;
                let mut socket = table.connect_with_format(PlayerId(0), format).await;
                let _opponent = table.connect(PlayerId(1)).await;
                let session = crate::lock_session(&table.app.sessions, &table.key.game_code).await.unwrap();
                let (lease, ()) = table.app.context.game_executor.with_session(session, |session| {
                    let config = ContestConfig {
                        countdown_ms: 1,
                        inactivity_timeout_ms: 60_000,
                        reconnect_grace_ms: 60_000,
                        max_active_ms: 60_000,
                        ..Default::default()
                    };
                    session.contest.policy.config = config;
                    session.state.phase = engine::types::phase::Phase::PreCombatMain;
                    let source_id = *session.state.objects.keys().next().unwrap();
                    session.state.waiting_for = WaitingFor::PhysicalContest {
                        contest: Box::new(ContestState::new(71, ContestKind::Staring, PlayerId(0), PlayerId(1), config)),
                        source_id,
                    };
                }).await;
                drop(lease);
                crate::ensure_contest_driver(
                    &table.app.context.game_executor, &table.app.sessions, &table.app.connections,
                    &table.app.draft_sessions, &table.app.game_db, &table.app.game_spectators,
                    &table.app.lobby, &table.app.lobby_subscribers, &table.key.game_code,
                ).await;
                loop {
                    let lease = crate::lock_session(&table.app.sessions, &table.key.game_code).await.unwrap();
                    let active = matches!(&lease.state.waiting_for, WaitingFor::PhysicalContest { contest, .. } if contest.phase == ContestPhase::Active);
                    drop(lease);
                    if active { break; }
                    tokio::task::yield_now().await;
                }
                for seq in 1..=40 {
                    send_test_message(&mut socket, &ClientMessage::ContestClick { contest: 71, seq }, format.is_some()).await;
                }
                // Hello + Reconnect spent two of thirty slots. The frozen quota
                // clock proves all forty clicks arrive in the same work window.
                work.wait_rate_limited(12).await;
                let mut command = serde_json::to_value(ClientMessage::Action { action: GameAction::PassPriority }).unwrap();
                command["command_id"] = serde_json::json!(719_001);
                command["padding"] = serde_json::json!("x".repeat(4096));
                let json = command.to_string();
                assert!(json.len() > 1024, "exercise a command beyond the old compact recovery cap");
                let frame = match crate::wire::encode_json_message_sync(json, format.is_some()).unwrap() {
                    axum::extract::ws::Message::Text(text) => WsMessage::Text(text.to_string().into()),
                    axum::extract::ws::Message::Binary(bytes) => WsMessage::Binary(bytes),
                    _ => unreachable!("an application command is text or binary"),
                };
                socket.send(frame).await.unwrap();
                work.wait_rate_limited(13).await;
                send_test_message(&mut socket, &ClientMessage::Ping { timestamp: 719 }, format.is_some()).await;
                let mut pong = false;
                let mut progress = false;
                let mut refusal = false;
                while !pong || !progress || !refusal {
                    let json = receive_wire_json(&mut socket).await;
                    match serde_json::from_str::<ServerMessage>(&json).unwrap() {
                        ServerMessage::Pong { timestamp: 719 } => pong = true,
                        ServerMessage::ActionFailed { .. } => {
                            let header: serde_json::Value = serde_json::from_str(&json).unwrap();
                            assert_eq!(header["command_id"], 719_001);
                            refusal = true;
                        }
                        ServerMessage::ContestProgress { contest: 71, clicks, .. } => {
                            for (player, count) in clicks {
                                if player == PlayerId(0) {
                                    assert!(count <= 40, "a click was applied twice");
                                    progress |= count == 40;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                assert_eq!(work.counts(), (crate::RATE_LIMIT_MESSAGES as usize + 12, 13),
                    "only twelve recovery-admitted compact clicks get an extra typed parse");
                assert_eq!(table.app.context.metrics.ws_quota_discard_count(), 0);
            }).await;
            table.server.abort();
            outcome.unwrap_or_else(|error| {
                panic!("{format:?} forty-click burst stalled or closed its socket: {error}")
            });
        }
    });
}

#[test]
fn policy_compliant_click_refill_preserves_correlated_completion() {
    run_async_test(|| async {
        use engine::types::contest::{ContestConfig, ContestKind, ContestPhase, ContestState};
        use engine::types::game_state::WaitingFor;
        use futures_util::{SinkExt, StreamExt};
        use server_core::protocol::WireFormat;
        use tokio_tungstenite::tungstenite::Message as WsMessage;

        for format in [None, Some(WireFormat::GzipEnvelopeV1)] {
            let table = NativeTable::new().await;
            let work = table.app.context.socket_hooks.arm_parser_work();
            let outcome = tokio::time::timeout(Duration::from_secs(20), async {
                table.keep_all().await;
                let mut socket = table.connect_with_format(PlayerId(0), format).await;
                let _opponent = table.connect(PlayerId(1)).await;
                let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let (lease, burst) = table
                    .app
                    .context
                    .game_executor
                    .with_session(session, |session| {
                        let config = ContestConfig {
                            countdown_ms: 0,
                            inactivity_timeout_ms: 60_000,
                            reconnect_grace_ms: 60_000,
                            max_active_ms: 60_000,
                            ..Default::default()
                        };
                        session.contest.policy.config = config;
                        session.state.phase = engine::types::phase::Phase::PreCombatMain;
                        let source_id = *session.state.objects.keys().next().unwrap();
                        session.state.waiting_for = WaitingFor::PhysicalContest {
                            contest: Box::new(ContestState::new(
                                72,
                                ContestKind::Staring,
                                PlayerId(0),
                                PlayerId(1),
                                config,
                            )),
                            source_id,
                        };
                        session.contest.policy.click_burst
                    })
                    .await;
                drop(lease);
                let clicks = burst + 20;
                assert!(clicks >= 60, "exercise the old exhausted-refusal boundary");
                crate::ensure_contest_driver(
                    &table.app.context.game_executor,
                    &table.app.sessions,
                    &table.app.connections,
                    &table.app.draft_sessions,
                    &table.app.game_db,
                    &table.app.game_spectators,
                    &table.app.lobby,
                    &table.app.lobby_subscribers,
                    &table.key.game_code,
                )
                .await;
                loop {
                    let lease = crate::lock_session(&table.app.sessions, &table.key.game_code)
                        .await
                        .unwrap();
                    let active = matches!(&lease.state.waiting_for,
                        WaitingFor::PhysicalContest { contest, .. }
                            if contest.phase == ContestPhase::Active);
                    drop(lease);
                    if active {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                for seq in 1..=clicks {
                    if seq > burst {
                        // Twenty refill clicks over ~700 ms stay below the
                        // policy's 30/s rate after its initial forty-click burst.
                        tokio::time::sleep(Duration::from_millis(35)).await;
                    }
                    send_test_message(
                        &mut socket,
                        &ClientMessage::ContestClick { contest: 72, seq },
                        format.is_some(),
                    )
                    .await;
                    // One frozen decoder window, without letting ingress
                    // capacity conceal the distinct work-allowance regression.
                    work.wait_completed(2 + seq as usize).await;
                }
                let mut command = serde_json::to_value(ClientMessage::Action {
                    action: GameAction::PassPriority,
                })
                .unwrap();
                command["command_id"] = serde_json::json!(720_001);
                command["padding"] = serde_json::json!("x".repeat(4096));
                let frame =
                    crate::wire::encode_json_message_sync(command.to_string(), format.is_some())
                        .unwrap();
                let frame = match frame {
                    axum::extract::ws::Message::Text(text) => {
                        WsMessage::Text(text.to_string().into())
                    }
                    axum::extract::ws::Message::Binary(bytes) => WsMessage::Binary(bytes),
                    _ => unreachable!("application command encoding"),
                };
                socket.send(frame).await.unwrap();
                work.wait_completed(3 + clicks as usize).await;
                loop {
                    let json = receive_wire_json(&mut socket).await;
                    if matches!(
                        serde_json::from_str::<ServerMessage>(&json).unwrap(),
                        ServerMessage::ActionFailed { .. }
                    ) {
                        let header: serde_json::Value = serde_json::from_str(&json).unwrap();
                        assert_eq!(header["command_id"], 720_001);
                        break;
                    }
                }
                // The frozen probe window may be spent by legitimate clicks;
                // native control liveness does not depend on JSON probe credit.
                socket
                    .send(WsMessage::Ping(vec![72, 1].into()))
                    .await
                    .unwrap();
                loop {
                    match socket.next().await.unwrap().unwrap() {
                        WsMessage::Pong(bytes) if bytes.as_ref() == [72, 1] => break,
                        WsMessage::Text(_)
                        | WsMessage::Binary(_)
                        | WsMessage::Ping(_)
                        | WsMessage::Pong(_) => {}
                        frame => panic!("legitimate policy traffic closed the socket: {frame:?}"),
                    }
                }
                let quota = crate::RATE_LIMIT_MESSAGES as usize;
                let over_quota = clicks as usize - (quota - 2);
                assert_eq!(work.counts(), (quota * 2, over_quota + 1));
                assert_eq!(
                    table.app.context.metrics.ws_quota_discard_count(),
                    (over_quota - quota) as u64,
                );
                assert_eq!(
                    table
                        .app
                        .context
                        .metrics
                        .ws_disconnect_count(crate::socket_disconnect::Cause::QuotaDiscardOverflow),
                    0,
                );
            })
            .await;
            table.server.abort();
            outcome.unwrap_or_else(|error| {
                panic!("{format:?} maximal contest stream lost owned completion: {error}")
            });
        }
    });
}

async fn age_hosted_listing(
    app: &crate::AppState,
    code: &str,
) -> Vec<lobby_broker::LobbyRegistration> {
    let mut broker = app.lobby.lock().await;
    // Preserve the registration generation while advancing its fixture age.
    // No production clock override or five-minute wall-clock sleep is needed.
    let mut snapshot = serde_json::to_value(broker.lobby()).unwrap();
    let game = &mut snapshot["games"][code];
    game["created_at"] = serde_json::json!(0);
    game["last_seen"] = serde_json::json!(0);
    *broker.lobby_mut() = serde_json::from_value(snapshot).unwrap();
    let reported = broker.lobby().check_expired(300, &crate::SysEnv);
    assert!(reported
        .iter()
        .any(|registration| registration.game_code() == code));
    reported
}

#[test]
fn socket_hosted_json_pings_keep_the_listing_alive() {
    run_async_test(|| async {
        for mode in [crate::ServerMode::Full, crate::ServerMode::LobbyOnly] {
            let (url, server, _directory, app) = spawn_server(
                mode,
                crate::draft_pools::DraftPools::default(),
                Arc::new(basic_land_test_database()),
                crate::ServerContext::default(),
            )
            .await;
            let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                let mut socket = connect_and_hello(url).await;
                send_test_message(
                    &mut socket,
                    &ClientMessage::CreateGameWithSettings {
                        deck: DeckData {
                            main_deck: vec!["Network Test Land".to_string(); 60],
                            ..Default::default()
                        },
                        display_name: "Heartbeat Host".to_string(),
                        public: true,
                        password: None,
                        timer_seconds: None,
                        player_count: 2,
                        match_config: Default::default(),
                        ai_seats: Vec::new(),
                        format_config: Some(FormatConfig::free_for_all()),
                        room_name: None,
                        host_peer_id: matches!(mode, crate::ServerMode::LobbyOnly)
                            .then(|| "heartbeat-peer".to_string()),
                        draft_metadata: None,
                        start_when_full: true,
                        ranked: false,
                        requested_code: None,
                        booster_pack_pool: None,
                    },
                    false,
                )
                .await;
                let code = loop {
                    match recv_server_message(&mut socket).await {
                        ServerMessage::GameCreated { game_code, .. } => break game_code,
                        ServerMessage::Error { message, .. } => {
                            panic!("{mode:?} host creation failed: {message}")
                        }
                        _ => {}
                    }
                };
                // Delimit creation through the application FIFO before aging.
                // Only broker-hosted rooms publish a heartbeat registration.
                send_test_message(&mut socket, &ClientMessage::SubscribeLobby, false).await;
                loop {
                    if matches!(
                        recv_server_message(&mut socket).await,
                        ServerMessage::LobbyUpdate { .. }
                    ) {
                        break;
                    }
                }
                for timestamp in 1..=3 {
                    if matches!(mode, crate::ServerMode::LobbyOnly) {
                        let _ = age_hosted_listing(&app, &code).await;
                    }
                    send_test_message(&mut socket, &ClientMessage::Ping { timestamp }, false).await;
                    loop {
                        if matches!(
                            recv_server_message(&mut socket).await,
                            ServerMessage::Pong { timestamp: echoed } if echoed == timestamp
                        ) {
                            break;
                        }
                    }
                    match mode {
                        crate::ServerMode::Full => {
                            // Full rooms are session-owned, not broker-owned.
                            // Force a stale report AFTER the Pong, then drive
                            // the same session/draft and lobby phases as main's
                            // production expiry task. A live host must keep its
                            // room even without a broker heartbeat stamp.
                            let mut reported = age_hosted_listing(&app, &code).await;
                            tokio::time::timeout(Duration::from_secs(5), async {
                                loop {
                                    let drafts =
                                        crate::live_draft_codes(&app.draft_sessions, &reported)
                                            .await;
                                    let sweep = {
                                        let mut sessions = app.sessions.lock().await;
                                        crate::handle_expired_lobby_games(
                                            &mut sessions,
                                            &app.game_db,
                                            &reported,
                                            &drafts,
                                        )
                                    };
                                    let routes = crate::ExpiredLobbyRoutes::capture(
                                        &sweep,
                                        &app.connections,
                                        &app.game_spectators,
                                    )
                                    .await;
                                    crate::finish_lobby_expiry_sweep(
                                        &app.lobby,
                                        &app.lobby_subscribers,
                                        &app.connections,
                                        &app.game_spectators,
                                        &sweep,
                                        routes,
                                    )
                                    .await;
                                    assert!(
                                        app.sessions.lock().await.contains_game(&code),
                                        "lobby age retired a connected Full host's room",
                                    );
                                    let broker = app.lobby.lock().await;
                                    assert!(broker.lobby().public_game(&code).is_some());
                                    reported = broker.lobby().check_expired(300, &crate::SysEnv);
                                    if !reported
                                        .iter()
                                        .any(|registration| registration.game_code() == code)
                                    {
                                        break;
                                    }
                                    drop(broker);
                                    // A busy session is legitimately deferred
                                    // by the sweep; model its next tick.
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("the production sweep failed to keep a live Full room");
                        }
                        crate::ServerMode::LobbyOnly => {
                            // Pong and HostRefresh are independent. Do not send
                            // another broker command that could mask a missing
                            // refresh from this actual socket heartbeat.
                            tokio::time::timeout(Duration::from_secs(5), async {
                                loop {
                                    let broker = app.lobby.lock().await;
                                    assert!(broker.lobby().public_game(&code).is_some());
                                    if !broker
                                        .lobby()
                                        .check_expired(300, &crate::SysEnv)
                                        .iter()
                                        .any(|registration| registration.game_code() == code)
                                    {
                                        break;
                                    }
                                    drop(broker);
                                    tokio::task::yield_now().await;
                                }
                            })
                            .await
                            .expect("a real JSON Ping failed to refresh its broker-hosted listing");
                        }
                    }
                }
            })
            .await;
            server.abort();
            outcome.unwrap_or_else(|error| {
                panic!("{mode:?} hosted socket lifecycle failed: {error}");
            });
        }
    });
}

mod oversized_publication {
    use super::*;
    use engine::ai_support::legal_actions;
    use engine::analysis::decision_template::IterationCount;
    use engine::analysis::loop_check::ShortcutResponse;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::scenario_db::GameScenarioDbExt;
    use engine::types::ability::{AbilityKind, TargetRef};
    use engine::types::events::GameEvent;
    use engine::types::game_state::{CastPaymentMode, GameState, LoopDetectionMode, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::log::{GameLogEntry, LogImportance, LogSegment};
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use server_core::game_state_snapshot_wire_guard::{
        MAX_SNAPSHOT_EVENTS, MAX_SNAPSHOT_SPELL_COSTS,
    };

    const SMAUG: &str = "Smaug the Impenetrable";
    const TAKE: u32 = 120;

    fn act(runner: &mut GameRunner, action: GameAction) {
        let shown = format!("{action:?}");
        runner
            .act(action)
            .unwrap_or_else(|error| panic!("{shown}: {error:?}"));
    }

    /// Resolves pending choices, preferring actions that name the combo pieces.
    fn settle(runner: &mut GameRunner, score: &dyn Fn(&GameAction) -> i32) {
        for _ in 0..400 {
            match &runner.state().waiting_for {
                WaitingFor::LoopShortcut { .. } | WaitingFor::GameOver { .. } => return,
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
                WaitingFor::Priority { .. } => act(runner, GameAction::PassPriority),
                _ => {
                    let best = legal_actions(runner.state())
                        .into_iter()
                        .filter(|action| {
                            !matches!(
                                action,
                                GameAction::PassPriority
                                    | GameAction::CancelCast
                                    | GameAction::DeclineShortcut
                                    | GameAction::DeclareShortcut { .. }
                                    | GameAction::UntapLandForMana { .. }
                                    | GameAction::UnspendPoolMana { .. }
                                    | GameAction::BackToManaPayment
                                    | GameAction::Concede { .. }
                            )
                        })
                        .enumerate()
                        .max_by_key(|(at, action)| (score(action), std::cmp::Reverse(*at)))
                        .map(|(_, action)| action)
                        .unwrap_or(GameAction::PassPriority);
                    act(runner, best);
                }
            }
        }
        panic!("the Smaug board did not settle");
    }

    fn card_db() -> engine::database::card_db::CardDatabase {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../engine/tests/fixtures/integration_cards.json.gz"
        );
        engine::database::card_db::CardDatabase::from_export_reader(flate2::read::GzDecoder::new(
            std::io::BufReader::new(std::fs::File::open(path).unwrap()),
        ))
        .unwrap()
    }

    /// Smaug + Mayhem Devil with Treasure mana: each Treasure sacrificed for
    /// mana deals Smaug 1 damage, which creates a Treasure, until the engine
    /// offers a loop shortcut.
    fn smaug_offer() -> GameState {
        let db = card_db();
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let smaug = scenario.add_real_card(P0, SMAUG, Zone::Battlefield, &db);
        let devil = scenario.add_real_card(P0, "Mayhem Devil", Zone::Battlefield, &db);
        let breaker =
            scenario.add_real_card(P0, "Professional Face-Breaker", Zone::Battlefield, &db);
        scenario.add_real_card(P0, "Mountain", Zone::Battlefield, &db);
        let shock = scenario.add_real_card(P0, "Shock", Zone::Hand, &db);
        for _ in 0..60 {
            scenario.add_real_card(P0, "Swamp", Zone::Library, &db);
            scenario.add_real_card(P1, "Swamp", Zone::Library, &db);
        }
        let mut runner = scenario.build();
        runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
        let score = move |action: &GameAction| {
            let related = action.related_object_ids();
            2 * i32::from(related.contains(&smaug))
                + i32::from(related.contains(&devil))
                + i32::from(related.contains(&breaker))
        };
        let card_id = runner.state().objects[&shock].card_id;
        act(
            &mut runner,
            GameAction::CastSpell {
                object_id: shock,
                card_id,
                targets: vec![smaug],
                payment_mode: CastPaymentMode::Auto,
            },
        );
        settle(&mut runner, &score);
        for _ in 0..8 {
            let state = runner.state();
            let treasure = state
                .battlefield
                .iter()
                .copied()
                .find(|id| state.objects[id].name == "Treasure" && !state.objects[id].tapped)
                .expect("an untapped Treasure");
            let ability_index = mana_ability(state, treasure);
            act(
                &mut runner,
                GameAction::ActivateAbility {
                    source_id: treasure,
                    ability_index,
                    payment_mode: CastPaymentMode::Auto,
                },
            );
            settle(&mut runner, &score);
            if matches!(runner.state().waiting_for, WaitingFor::LoopShortcut { .. }) {
                return runner.state().clone();
            }
        }
        panic!("no loop shortcut offer: {:?}", runner.state().waiting_for);
    }

    fn mana_ability(state: &GameState, id: ObjectId) -> usize {
        state.objects[&id]
            .abilities
            .iter()
            .position(|ability| {
                ability.kind == AbilityKind::Activated
                    && engine::game::mana_abilities::is_mana_ability(ability)
            })
            .expect("a Treasure mana ability")
    }

    fn smaug_damage_in(state: &GameState) -> u64 {
        state
            .objects
            .values()
            .find(|object| object.name == SMAUG)
            .map(|object| u64::from(object.damage_marked))
            .unwrap()
    }

    fn objects_named<'a>(update: &'a serde_json::Value, name: &str) -> Vec<&'a serde_json::Value> {
        let objects = &update["data"]["state"]["objects"];
        let mut all = objects
            .as_object()
            .map(|map| map.values().collect::<Vec<_>>())
            .or_else(|| objects.as_array().map(|list| list.iter().collect()))
            .unwrap();
        all.retain(|object| object["name"] == name);
        all
    }

    fn smaug_damage(update: &serde_json::Value) -> u64 {
        objects_named(update, SMAUG)[0]["damage_marked"]
            .as_u64()
            .unwrap()
    }

    static SERVER_LOG: std::sync::Mutex<Vec<u8>> = std::sync::Mutex::new(Vec::new());

    struct ServerLog;

    impl std::io::Write for ServerLog {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            SERVER_LOG.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Sends this process's tracing output to [`SERVER_LOG`]; the server's worker threads
    /// only see a global subscriber.
    fn capture_server_log() {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            tracing_subscriber::fmt()
                .with_writer(|| ServerLog)
                .with_ansi(false)
                .init();
        });
    }

    fn trim_warned(game_code: &str) -> bool {
        String::from_utf8_lossy(&SERVER_LOG.lock().unwrap())
            .lines()
            .any(|line| {
                line.contains("events/log trimmed to broadcast bounds") && line.contains(game_code)
            })
    }

    /// Ravenous Trap resolved: P1's whole graveyard is in exile and the update carries only the
    /// newest MAX_SNAPSHOT_EVENTS events.
    fn assert_trap_resolved(update: &serde_json::Value) {
        assert_eq!(update["type"], "StateUpdate", "{update}");
        assert_eq!(
            update["data"]["events"].as_array().unwrap().len(),
            MAX_SNAPSHOT_EVENTS
        );
        let exiled = objects_named(update, "Grizzly Bears");
        assert_eq!(exiled.len(), GRAVEYARD);
        assert!(exiled.iter().all(|card| card["zone"] == "Exile"));
    }

    /// Enough cards in P1's graveyard that exiling it, and P1's concession (CR 104.3a), each
    /// emit more than MAX_SNAPSHOT_EVENTS events.
    const GRAVEYARD: usize = 2_000;

    /// P0 has cast Ravenous Trap ("Exile target player's graveyard.") at P1's graveyard of
    /// [`GRAVEYARD`] Grizzly Bears, and P1 holds priority: P1's pass resolves it (CR 117.4).
    fn trap_on_the_stack() -> GameState {
        trap_held_by(GameScenario::new(), P1)
    }

    /// Ravenous Trap cast by P0 at P1's graveyard; every seat before `holder` has passed, so
    /// `holder`'s pass resolves it (CR 117.4).
    fn trap_held_by(mut scenario: GameScenario, holder: PlayerId) -> GameState {
        let db = card_db();
        scenario.at_phase(Phase::PreCombatMain);
        let trap = scenario.add_real_card(P0, "Ravenous Trap", Zone::Hand, &db);
        for _ in 0..4 {
            scenario.add_real_card(P0, "Swamp", Zone::Battlefield, &db);
        }
        for _ in 0..GRAVEYARD {
            scenario.add_real_card(P1, "Grizzly Bears", Zone::Graveyard, &db);
        }
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&trap].card_id;
        act(
            &mut runner,
            GameAction::CastSpell {
                object_id: trap,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            },
        );
        act(
            &mut runner,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(P1)),
            },
        );
        while runner.state().waiting_for != (WaitingFor::Priority { player: holder }) {
            act(&mut runner, GameAction::PassPriority);
            assert_eq!(runner.state().stack.len(), 1);
        }
        runner.state().clone()
    }

    async fn install(table: &NativeTable, state: GameState, ai: &[PlayerId]) {
        let ai = ai.to_vec();
        let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
            .await
            .unwrap();
        let (lease, ()) = table
            .app
            .context
            .game_executor
            .with_session(session, move |session| {
                session.state = state;
                enable_ai(session, ai);
                session.advance_state_revision();
            })
            .await;
        drop(lease);
    }

    async fn completion(socket: &mut ClientSocket, command_id: u64) -> serde_json::Value {
        loop {
            let frame = receive_command_frame(socket).await;
            if owns_command_completion(&frame, command_id) {
                return frame;
            }
        }
    }

    async fn next_decision(socket: &mut ClientSocket) -> serde_json::Value {
        loop {
            let frame = receive_command_frame(socket).await;
            if frame["type"] == "StateUpdate"
                && frame["data"]["legal_actions"]
                    .as_array()
                    .is_some_and(|actions| !actions.is_empty())
            {
                return frame;
            }
        }
    }

    /// Repetition elision keeps the take under MAX_SNAPSHOT_EVENTS, first and last marker included.
    fn assert_whole_take(update: &serde_json::Value, before: u64) {
        assert_eq!(update["type"], "StateUpdate", "{update}");
        assert!(LegReport::events(update).len() < MAX_SNAPSHOT_EVENTS);
        assert_eq!(LegReport::markers(update), [(P0, 1), (P0, TAKE)]);
        assert_eq!(smaug_damage(update), before + u64::from(TAKE));
    }

    async fn declare(declarer: &mut ClientSocket) {
        send_command_frame(
            declarer,
            &GameAction::DeclareShortcut {
                count: IterationCount::Fixed(TAKE),
                template: None,
            },
            1,
        )
        .await;
        assert_eq!(completion(declarer, 1).await["type"], "StateUpdate");
    }

    const ACCEPT: GameAction = GameAction::RespondToShortcut {
        response: ShortcutResponse::Accept,
    };

    #[test]
    fn human_accept_reaches_both_humans_whole() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let table = NativeTable::duel().await;
            install(&table, offer, &[]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut declarer = table.connect(P0).await;
                let mut accepter = table.connect(P1).await;
                declare(&mut declarer).await;
                send_command_frame(&mut accepter, &ACCEPT, 7).await;
                assert_whole_take(&completion(&mut accepter, 7).await, before);
                let decision = next_decision(&mut declarer).await;
                assert_eq!(smaug_damage(&decision), before + u64::from(TAKE));
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized human take was not published");
        });
    }

    #[test]
    fn ai_accept_reaches_the_declarer_whole() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let table = NativeTable::duel().await;
            install(&table, offer, &[P1]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut declarer = table.connect(P0).await;
                declare(&mut declarer).await;
                assert_whole_take(&next_decision(&mut declarer).await, before);
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized AI take was not published");
        });
    }

    #[test]
    fn human_accept_with_an_ai_followup_is_published_whole() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let table = NativeTable::duel().await;
            install(&table, offer, &[]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut declarer = table.connect(P0).await;
                let mut accepter = table.connect(P1).await;
                declare(&mut declarer).await;
                let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let (lease, ()) = table
                    .app
                    .context
                    .game_executor
                    .with_session(session, |session| enable_ai(session, [P0]))
                    .await;
                drop(lease);
                send_command_frame(&mut accepter, &ACCEPT, 7).await;
                let accepted = completion(&mut accepter, 7).await;
                assert_whole_take(&accepted, before);
                assert_eq!(accepted["data"]["legal_actions"], serde_json::json!([]));
                let revision = accepted["data"]["state_revision"].as_u64().unwrap();
                loop {
                    let frame = receive_command_frame(&mut accepter).await;
                    if frame["type"] == "StateUpdate" {
                        assert!(frame["data"]["state_revision"].as_u64().unwrap() > revision);
                        break;
                    }
                }
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized take with an AI follow-up was not published");
        });
    }

    #[test]
    fn oversized_exile_reaches_both_humans_trimmed() {
        run_async_test(|| async {
            capture_server_log();
            let table = NativeTable::duel().await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut caster = table.connect(P0).await;
                let mut passer = table.connect(P1).await;
                install(&table, trap_on_the_stack(), &[]).await;
                send_command_frame(&mut passer, &GameAction::PassPriority, 7).await;
                let resolved = completion(&mut passer, 7).await;
                assert_trap_resolved(&resolved);
                let revision = &resolved["data"]["state_revision"];
                let seen = loop {
                    let frame = receive_command_frame(&mut caster).await;
                    if frame["type"] == "StateUpdate"
                        && frame["data"]["state_revision"] == *revision
                    {
                        break frame;
                    }
                };
                assert_trap_resolved(&seen);
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized exile was not published");
            assert!(trim_warned(&table.key.game_code));
        });
    }

    #[test]
    fn oversized_exile_with_an_ai_followup_is_published_trimmed() {
        run_async_test(|| async {
            capture_server_log();
            let table = NativeTable::duel().await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut passer = table.connect(P1).await;
                install(&table, trap_on_the_stack(), &[P0]).await;
                send_command_frame(&mut passer, &GameAction::PassPriority, 7).await;
                let resolved = completion(&mut passer, 7).await;
                assert_trap_resolved(&resolved);
                assert_eq!(resolved["data"]["legal_actions"], serde_json::json!([]));
                let revision = resolved["data"]["state_revision"].as_u64().unwrap();
                loop {
                    let frame = receive_command_frame(&mut passer).await;
                    if frame["type"] == "StateUpdate" {
                        assert!(frame["data"]["state_revision"].as_u64().unwrap() > revision);
                        break;
                    }
                }
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized exile with an AI follow-up was not published");
            assert!(trim_warned(&table.key.game_code));
        });
    }

    #[test]
    fn oversized_concession_reaches_the_opponent_trimmed() {
        run_async_test(|| async {
            capture_server_log();
            let table = NativeTable::duel().await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut opponent = table.connect(P0).await;
                let mut conceder = table.connect(P1).await;
                install(&table, trap_on_the_stack(), &[]).await;
                send_test_message(&mut conceder, &ClientMessage::Concede, false).await;
                let update = loop {
                    let frame = receive_command_frame(&mut opponent).await;
                    match frame["type"].as_str() {
                        Some("StateUpdate")
                            if frame["data"]["eliminated_players"] == serde_json::json!([P1]) =>
                        {
                            break frame;
                        }
                        Some("Conceded" | "TerminalResult" | "ActionFailed" | "Error") => {
                            panic!("no concession update before {frame}")
                        }
                        _ => {}
                    }
                };
                assert_eq!(
                    update["data"]["events"].as_array().unwrap().len(),
                    MAX_SNAPSHOT_EVENTS
                );
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized concession was not published");
            assert!(trim_warned(&table.key.game_code));
        });
    }

    #[test]
    fn oversized_ai_pass_on_reconnect_reaches_the_observer_trimmed() {
        run_async_test(|| async {
            capture_server_log();
            const AI: PlayerId = PlayerId(2);
            let table = NativeTable::seated(
                GameExecutor::new(NonZeroUsize::MIN),
                3,
                FormatConfig::free_for_all(),
            )
            .await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut observer = table.connect(P0).await;
                install(
                    &table,
                    trap_held_by(GameScenario::new_n_player(3, 42), AI),
                    &[AI],
                )
                .await;
                // P1's reconnect runs the AI seat, whose pass resolves the Trap; this handshake
                // skips the keep-alive Ping (sent after 10 s) that `connect` panics on.
                let _returning = table
                    .connect_with_format(
                        P1,
                        Some(server_core::protocol::WireFormat::GzipEnvelopeV1),
                    )
                    .await;
                let update = loop {
                    let frame = receive_command_frame(&mut observer).await;
                    if frame["type"] == "StateUpdate" {
                        break frame;
                    }
                };
                assert_trap_resolved(&update);
            })
            .await;
            table.server.abort();
            outcome.expect("the oversized reconnect AI update was not published");
            assert!(trim_warned(&table.key.game_code));
        });
    }

    #[test]
    fn post_ai_gate_ignores_the_decision_the_followup_replaces() {
        run_async_test(|| async {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let bolts: Vec<_> = (0..=MAX_SNAPSHOT_SPELL_COSTS)
                .map(|_| scenario.add_bolt_to_hand(P0))
                .collect();
            let mut runner = scenario.build();
            let state = runner.state_mut();
            for bolt in bolts {
                state.objects.get_mut(&bolt).unwrap().mana_cost = ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 0,
                };
            }
            state.active_player = P1;
            state.priority_player = P1;
            state.waiting_for = WaitingFor::Priority { player: P1 };
            let state = runner.state().clone();
            let table = NativeTable::duel().await;
            install(&table, state, &[P0]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(60), async {
                let mut human = table.connect(P1).await;
                send_command_frame(&mut human, &GameAction::PassPriority, 3).await;
                let passed = completion(&mut human, 3).await;
                assert_eq!(passed["type"], "StateUpdate", "{passed}");
                let revision = passed["data"]["state_revision"].as_u64().unwrap();
                loop {
                    let frame = receive_command_frame(&mut human).await;
                    if frame["type"] == "StateUpdate" {
                        assert!(frame["data"]["state_revision"].as_u64().unwrap() > revision);
                        break;
                    }
                }
            })
            .await;
            table.server.abort();
            outcome.expect("the human pass was not published");
        });
    }

    #[test]
    fn a_blocked_game_call_refuses_overflow_without_closing_the_socket() {
        run_async_test(|| async {
            use futures_util::SinkExt;
            use tokio_tungstenite::tungstenite::Message;

            let (entered_tx, entered) = tokio::sync::oneshot::channel();
            let entered_tx = parking_lot::Mutex::new(Some(entered_tx));
            let (release, released) = std::sync::mpsc::channel::<()>();
            let released = parking_lot::Mutex::new(released);
            let executor = GameExecutor::new(NonZeroUsize::MIN).with_ai_step_hook(move |_| {
                if let Some(entered) = entered_tx.lock().take() {
                    let _ = entered.send(());
                    released
                        .lock()
                        .recv_timeout(Duration::from_secs(60))
                        .expect("the test opens the gate");
                }
            });
            let table = NativeTable::with_executor(executor).await;
            let outcome = tokio::time::timeout(Duration::from_secs(60), async {
                // Connect first: a reconnect may itself run pending AI decisions.
                let mut host = table.connect(PlayerId(0)).await;
                let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
                    .await
                    .unwrap();
                let (lease, ()) = table
                    .app
                    .context
                    .game_executor
                    .with_session(session, |session| {
                        enable_ai(session, [PlayerId(1), PlayerId(2), PlayerId(3)])
                    })
                    .await;
                drop(lease);
                let keep = GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                };
                send_command_frame(&mut host, &keep, 5).await;
                entered.await.unwrap();
                let emote = serde_json::to_string(&ClientMessage::Emote {
                    emote: "Hello".to_string(),
                })
                .unwrap();
                for _ in 0..70 {
                    host.send(Message::Text(emote.clone().into()))
                        .await
                        .unwrap();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                send_command_frame(&mut host, &GameAction::PassPriority, 6).await;
                let refused = completion(&mut host, 6).await;
                assert_eq!(refused["type"], "ActionFailed", "{refused}");
                assert_eq!(
                    refused["data"]["message"],
                    crate::socket_mailbox::RefusalReason::GameBusy.message()
                );
                send_test_message(&mut host, &ClientMessage::Ping { timestamp: 9 }, false).await;
                while receive_command_frame(&mut host).await["type"] != "Pong" {}
                release.send(()).unwrap();
                assert_eq!(completion(&mut host, 5).await["type"], "StateUpdate");
            })
            .await;
            table.server.abort();
            outcome.expect("a blocked game call closed or stalled its socket");
        });
    }

    /// What one full-length take did to its sockets and what it published.
    #[derive(Debug, Default)]
    struct LegReport {
        count: u32,
        declare_hold: Option<Duration>,
        /// Each accept's hold, in order; the last runs the take.
        accept_holds: Vec<(PlayerId, Duration)>,
        /// Each Ping: its seat, when it was sent after the declaration, and its Pong's delay.
        pings: Vec<(PlayerId, Duration, Option<Duration>)>,
        /// Each Close, read error or end of stream, with its seat and time.
        broken: Vec<String>,
        /// The answer to the correlated action sent after the burst, and when it arrived.
        refusal: Option<(Duration, serde_json::Value)>,
        /// When the held command was sent and when its answer arrived.
        held: Option<(Duration, Option<Duration>)>,
        export: Option<Duration>,
        /// The completion of the command the game held.
        completion: Option<serde_json::Value>,
        /// The declarer's first StateUpdate with legal actions after the hold began.
        decision: Option<(Duration, serde_json::Value)>,
    }

    impl LegReport {
        fn events(update: &serde_json::Value) -> Vec<GameEvent> {
            serde_json::from_value(update["data"]["events"].clone()).unwrap()
        }

        fn markers(update: &serde_json::Value) -> Vec<(PlayerId, u32)> {
            Self::events(update)
                .into_iter()
                .filter_map(|event| match event {
                    GameEvent::ShortcutRepetition {
                        controller,
                        repetition,
                    } => Some((controller, repetition)),
                    _ => None,
                })
                .collect()
        }

        /// The entries the game log's default view lists, as text.
        fn timeline(update: &serde_json::Value) -> Vec<String> {
            let entries: Vec<GameLogEntry> =
                serde_json::from_value(update["data"]["log_entries"].clone()).unwrap_or_default();
            entries
                .iter()
                .filter(|entry| {
                    matches!(
                        entry.presentation.importance,
                        LogImportance::Essential | LogImportance::Context
                    )
                })
                .map(|entry| {
                    entry
                        .segments
                        .iter()
                        .map(|segment| match segment {
                            LogSegment::Text(text) => text.clone(),
                            LogSegment::CardName { name, .. }
                            | LogSegment::PlayerName { name, .. } => name.clone(),
                            LogSegment::Number(number) => number.to_string(),
                            other => format!("{other:?}"),
                        })
                        .collect()
                })
                .collect()
        }

        fn unanswered_pings(&self) -> usize {
            self.pings
                .iter()
                .filter(|(_, _, pong)| !pong.is_some_and(|delay| delay <= PONG_BUDGET))
                .count()
        }

        /// Whether each Ping sent during the hold, until `PONG_RACE` before its answer, had its
        /// Pong before that answer.
        fn pongs_during_hold(&self) -> Vec<bool> {
            let Some((sent, Some(answered))) = self.held else {
                return Vec::new();
            };
            self.pings
                .iter()
                .filter(|(_, at, _)| *at >= sent && *at + PONG_RACE < answered)
                .map(|(_, at, pong)| pong.is_some_and(|delay| *at + delay < answered))
                .collect()
        }

        fn line(&self) -> String {
            let pings: Vec<String> = self
                .pings
                .iter()
                .map(|(seat, sent, pong)| format!("P{}@{:.1?}:{pong:.1?}", seat.0, sent))
                .collect();
            let decision = self.decision.as_ref().map(|(at, update)| {
                format!(
                    "at {at:.1?}: markers {:?}, {} events, {} log entries, timeline {:?}",
                    Self::markers(update),
                    update["data"]["events"].as_array().map_or(0, Vec::len),
                    update["data"]["log_entries"].as_array().map_or(0, Vec::len),
                    Self::timeline(update),
                )
            });
            format!(
                "LegReport count={} declare_hold={:.1?} accept_holds={:.1?} pings=[{}] \
                 unanswered={} broken={:?} refusal={} held={:.1?} export={:.1?} decision={}",
                self.count,
                self.declare_hold,
                self.accept_holds,
                pings.join(" "),
                self.unanswered_pings(),
                self.broken,
                self.refusal
                    .as_ref()
                    .map_or("none".to_string(), |(at, frame)| format!(
                        "{} at {at:.1?}",
                        frame["data"]["message"]
                    )),
                self.held,
                self.export,
                decision.unwrap_or_else(|| "none".to_string()),
            )
        }
    }

    /// The client's silence budget: a socket whose Ping goes unanswered this long is dropped.
    const PONG_BUDGET: Duration = Duration::from_secs(45);
    /// A Ping sent this close to the held answer may see its Pong cross that answer on the wire.
    const PONG_RACE: Duration = Duration::from_secs(1);
    const FULL_LENGTH_BUDGET: Duration = Duration::from_secs(30 * 60);
    const BURST: usize = 70;
    const HELD_ACCEPT: u64 = 7;
    const AFTER_BURST: u64 = 8;

    /// A socket frame, or why the socket stopped.
    async fn next_frame(socket: &mut ClientSocket) -> Result<serde_json::Value, String> {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => return Ok(serde_json::from_str(&text).unwrap()),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Close(frame))) => return Err(format!("close {frame:?}")),
                Some(Ok(frame)) => return Err(format!("unexpected frame {frame:?}")),
                Some(Err(error)) => return Err(format!("read error {error}")),
                None => return Err("end of stream".to_string()),
            }
        }
    }

    /// The seat a StateUpdate asks to answer the shortcut, and whether others answer after it.
    fn responder(update: &serde_json::Value) -> Option<(PlayerId, bool)> {
        let waiting = &update["data"]["state"]["waiting_for"];
        (waiting["type"] == "RespondToShortcut").then(|| {
            let seat = waiting["data"]["player"].as_u64().unwrap();
            let others = waiting["data"]["remaining_players"]
                .as_array()
                .is_some_and(|rest| !rest.is_empty());
            (PlayerId(u8::try_from(seat).unwrap()), others)
        })
    }

    /// The sockets of one full-length leg and what they have seen.
    struct Leg {
        seats: Vec<(PlayerId, ClientSocket)>,
        report: LegReport,
        started: tokio::time::Instant,
        deadline: tokio::time::Instant,
        /// When the held command was sent; the export, burst and decision belong to it.
        hold: Option<tokio::time::Instant>,
        /// Each correlated answer, by command id, with when it arrived.
        answers: std::collections::HashMap<u64, (Duration, serde_json::Value)>,
        /// Each unanswered Ping's index in the report, by timestamp.
        pending: std::collections::HashMap<u64, usize>,
        timestamp: u64,
    }

    impl Leg {
        fn socket(&mut self, seat: PlayerId) -> &mut ClientSocket {
            &mut self
                .seats
                .iter_mut()
                .find(|(connected, _)| *connected == seat)
                .expect("a connected seat")
                .1
        }

        /// Sends `action` as `command` from `seat`; when it was sent.
        async fn send(&mut self, seat: PlayerId, action: &GameAction, command: u64) -> Duration {
            send_command_frame(self.socket(seat), action, command).await;
            self.started.elapsed()
        }

        /// The answer to `command` and how long after `sent` it arrived.
        fn answer(&self, command: u64, sent: Duration) -> Option<(Duration, serde_json::Value)> {
            self.answers
                .get(&command)
                .map(|(at, frame)| (*at - sent, frame.clone()))
        }

        /// Whether the held command, the burst, the export and the declarer's decision are all
        /// answered, and no Ping is still within its budget.
        fn settled(&self, held: u64) -> bool {
            let now = self.started.elapsed();
            self.answers.contains_key(&held)
                && self.answers.contains_key(&AFTER_BURST)
                && self.report.export.is_some()
                && self.report.decision.is_some()
                && self
                    .pending
                    .values()
                    .all(|index| now > self.report.pings[*index].1 + PONG_BUDGET)
        }

        /// Reads every socket, and pings each every 5 s, until `done` holds, a socket breaks or
        /// the budget runs out; `burst` names the holder and its held command, and the holder
        /// sends the burst and then the correlated action.
        async fn pump(&mut self, burst: Option<(PlayerId, u64)>, done: impl Fn(&Self) -> bool) {
            use futures_util::SinkExt;
            use tokio_tungstenite::tungstenite::Message;

            let emote = serde_json::to_string(&ClientMessage::Emote {
                emote: "Hello".to_string(),
            })
            .unwrap();
            let mut pings = tokio::time::interval(Duration::from_secs(5));
            // Only frames within the rate quota are retained toward the command budget, and the
            // held command spends one; pacing two under the quota keeps every window inside it.
            let mut tick = tokio::time::interval(
                Duration::from_secs(crate::RATE_LIMIT_WINDOW_SECS)
                    / (crate::RATE_LIMIT_MESSAGES - 2),
            );
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut sent = 0;
            while !done(self) && self.report.broken.is_empty() {
                tokio::select! {
                    () = tokio::time::sleep_until(self.deadline) => break,
                    _ = pings.tick() => {
                        let held = burst.is_some_and(|(_, held)| self.answers.contains_key(&held));
                        if !held {
                            self.ping().await;
                        }
                    }
                    _ = tick.tick(), if burst.is_some() && sent <= BURST => {
                        let (seat, _) = burst.unwrap();
                        let socket = self.socket(seat);
                        if sent < BURST {
                            socket.send(Message::Text(emote.clone().into())).await.unwrap();
                        } else {
                            send_command_frame(socket, &GameAction::PassPriority, AFTER_BURST)
                                .await;
                        }
                        sent += 1;
                    }
                    (seat, frame) = async {
                        let reads = self.seats.iter_mut().map(|(seat, socket)| {
                            Box::pin(async move { (*seat, next_frame(socket).await) })
                        });
                        futures_util::future::select_all(reads).await.0
                    } => self.read(seat, frame),
                }
            }
        }

        async fn ping(&mut self) {
            let at = self.started.elapsed();
            for (seat, socket) in &mut self.seats {
                self.timestamp += 1;
                let ping = ClientMessage::Ping {
                    timestamp: self.timestamp,
                };
                send_test_message(socket, &ping, false).await;
                self.pending.insert(self.timestamp, self.report.pings.len());
                self.report.pings.push((*seat, at, None));
            }
        }

        fn read(&mut self, seat: PlayerId, frame: Result<serde_json::Value, String>) {
            let at = self.started.elapsed();
            let frame = match frame {
                Ok(frame) => frame,
                Err(why) => {
                    self.report
                        .broken
                        .push(format!("P{} at {at:.1?}: {why}", seat.0));
                    return;
                }
            };
            if frame["type"] == "Pong" {
                let stamp = frame["data"]["timestamp"].as_u64().unwrap();
                if let Some(index) = self.pending.remove(&stamp) {
                    let ping = &mut self.report.pings[index];
                    ping.2 = Some(at - ping.1);
                }
                return;
            }
            if let Some(command) = frame.get("command_id").and_then(serde_json::Value::as_u64) {
                self.answers.insert(command, (at, frame.clone()));
            }
            let Some(hold) = self.hold.filter(|_| seat == P0) else {
                return;
            };
            if frame["type"] == "AuthoritativeStateExport" {
                self.report.export = Some(hold.elapsed());
            } else if self.report.decision.is_none()
                && frame["type"] == "StateUpdate"
                && frame["data"]["legal_actions"]
                    .as_array()
                    .is_some_and(|actions| !actions.is_empty())
            {
                self.report.decision = Some((hold.elapsed(), frame));
            }
        }

        /// Sends the held command, then P0's export, and reads until everything is answered.
        async fn run_held(
            &mut self,
            seat: PlayerId,
            action: &GameAction,
            command: u64,
        ) -> Duration {
            let sent = self.send(seat, action, command).await;
            self.hold = Some(tokio::time::Instant::now());
            send_test_message(
                self.socket(P0),
                &ClientMessage::ExportAuthoritativeState,
                false,
            )
            .await;
            self.pump(Some((seat, command)), |leg| leg.settled(command))
                .await;
            self.report.refusal = self.answers.get(&AFTER_BURST).cloned();
            self.report.held = Some((sent, self.answers.get(&command).map(|(at, _)| *at)));
            sent
        }
    }

    /// CR 732.2a + CR 732.2c: P0 declares the take of `offer` at `count` and every human seat
    /// accepts it, while every connected seat reads its socket and pings each 5 s. During the
    /// command that runs the take (the declaration when no human answers it, else the last
    /// accept) P0 asks for an export and the holder sends a burst then a correlated action. The
    /// report records what the sockets saw until each answer arrived or `budget` ran out.
    async fn full_length_take(
        table: &NativeTable,
        offer: GameState,
        ai: &[PlayerId],
        count: u32,
        budget: Duration,
    ) -> LegReport {
        let seats = offer.players.len();
        install(table, offer, ai).await;
        let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
            .await
            .unwrap();
        let (lease, ()) = table
            .app
            .context
            .game_executor
            .with_session(session, |session| {
                session.set_hosting(server_core::session::HostingMode::SingleUser);
            })
            .await;
        drop(lease);

        let mut connected = vec![(P0, table.connect_uncapped(P0).await)];
        for seat in (1..seats).map(|seat| PlayerId(u8::try_from(seat).unwrap())) {
            if !ai.contains(&seat) {
                connected.push((seat, table.connect_uncapped(seat).await));
            }
        }
        let started = tokio::time::Instant::now();
        let mut leg = Leg {
            seats: connected,
            report: LegReport {
                count,
                ..Default::default()
            },
            started,
            deadline: started + budget,
            hold: None,
            answers: Default::default(),
            pending: Default::default(),
            timestamp: 0,
        };
        let declare = GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        };
        let held = if leg.seats.len() == 1 {
            let sent = leg.run_held(P0, &declare, 1).await;
            leg.report.declare_hold = leg.answer(1, sent).map(|(hold, _)| hold);
            1
        } else {
            let sent = leg.send(P0, &declare, 1).await;
            leg.pump(None, |leg| leg.answers.contains_key(&1)).await;
            let Some((hold, mut update)) = leg.answer(1, sent) else {
                return leg.report;
            };
            leg.report.declare_hold = Some(hold);
            let mut command = 100;
            loop {
                let (seat, others) = responder(&update).expect("a seat answers the shortcut");
                if !others {
                    let sent = leg.run_held(seat, &ACCEPT, HELD_ACCEPT).await;
                    if let Some((hold, _)) = leg.answer(HELD_ACCEPT, sent) {
                        leg.report.accept_holds.push((seat, hold));
                    }
                    break HELD_ACCEPT;
                }
                command += 1;
                let sent = leg.send(seat, &ACCEPT, command).await;
                leg.pump(None, |leg| leg.answers.contains_key(&command))
                    .await;
                let Some((hold, answer)) = leg.answer(command, sent) else {
                    return leg.report;
                };
                leg.report.accept_holds.push((seat, hold));
                update = answer;
            }
        };
        leg.report.completion = leg.answers.remove(&held).map(|(_, frame)| frame);
        leg.report
    }

    /// The shared assertions of a full-length leg: no socket dropped, every Ping answered in
    /// time and each Pong during the hold before the held answer, the burst refused rather than
    /// closed, the export answered, and the decision made.
    fn assert_full_length(report: &LegReport) {
        assert!(report.broken.is_empty(), "{}", report.line());
        assert!(!report.pings.is_empty(), "{}", report.line());
        assert_eq!(report.unanswered_pings(), 0, "{}", report.line());
        let pongs = report.pongs_during_hold();
        assert!(
            !pongs.is_empty(),
            "no Ping during the hold: {}",
            report.line()
        );
        assert!(
            pongs.iter().all(|before| *before),
            "a Pong waited for the held answer: {}",
            report.line()
        );
        let (_, held) = report.held.expect("a command is held");
        let held = held.expect("the held command is answered");
        let (refused, refusal) = report.refusal.as_ref().expect("the burst is answered");
        assert!(
            *refused < held,
            "precondition: the hold must outlast the burst; {}",
            report.line()
        );
        assert_eq!(refusal["type"], "ActionFailed", "{}", report.line());
        assert_eq!(
            refusal["data"]["message"],
            crate::socket_mailbox::RefusalReason::GameBusy.message()
        );
        assert!(report.export.is_some(), "{}", report.line());
        assert!(report.decision.is_some(), "{}", report.line());
    }

    /// The offer's own count: what a player taking the default declares.
    fn default_count(offer: &GameState) -> u32 {
        let WaitingFor::LoopShortcut { schema, .. } = &offer.waiting_for else {
            panic!("an offer");
        };
        let IterationCount::Fixed(count) = schema.iteration_count else {
            panic!("a fixed default: {:?}", schema.iteration_count);
        };
        count
    }

    /// CR 732.2a: the default count of the Smaug the Impenetrable + Mayhem Devil loop, 1000,
    /// taken in full by a human accept over real sockets. Each repetition is Mayhem Devil's 1
    /// damage to Smaug (CR 120.3e), so Smaug's damage rises by 1000.
    /// Run: `cargo nextest run -p phase-server --cargo-profile server-release --run-ignored only
    /// --no-capture -E 'test(/full_length/)'`.
    #[test]
    #[ignore = "full length: minutes per leg"]
    fn full_length_human_accept_keeps_every_socket() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let count = default_count(&offer);
            assert_eq!(count, 1000);
            let table = NativeTable::duel().await;
            let report = full_length_take(&table, offer, &[], count, FULL_LENGTH_BUDGET).await;
            table.server.abort();
            println!("{}", report.line());
            assert_full_length(&report);
            let accepted = report.completion.as_ref().expect("the accept completes");
            assert_eq!(accepted["type"], "StateUpdate", "{}", report.line());
            assert_eq!(LegReport::markers(accepted), [(P0, 1), (P0, count)]);
            assert!(LegReport::events(accepted).len() < MAX_SNAPSHOT_EVENTS);
            assert_eq!(smaug_damage(accepted), before + u64::from(count));
        });
    }

    /// The same take accepted by an AI seat: the game holds P0's declaration while the AI accepts.
    /// Run as [`full_length_human_accept_keeps_every_socket`].
    #[test]
    #[ignore = "full length: minutes per leg"]
    fn full_length_ai_accept_keeps_every_socket() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let count = default_count(&offer);
            assert_eq!(count, 1000);
            let table = NativeTable::duel().await;
            let report = full_length_take(&table, offer, &[P1], count, FULL_LENGTH_BUDGET).await;
            table.server.abort();
            println!("{}", report.line());
            assert_full_length(&report);
            let (_, decision) = report.decision.as_ref().expect("the declarer decides");
            assert_eq!(LegReport::markers(decision), [(P0, 1), (P0, count)]);
            assert_eq!(smaug_damage(decision), before + u64::from(count));
        });
    }

    /// The frame bound the Smaug legs run at: the offer's frames fit under it, every frame of
    /// the turn after the take does not.
    const SMAUG_CAP: usize = 512 * 1024;

    async fn session_read<T: Send + 'static>(
        table: &NativeTable,
        read: impl FnOnce(&mut crate::game_executor::SessionJob<'_>) -> T + Send + 'static,
    ) -> T {
        let session = crate::lock_session(&table.app.sessions, &table.key.game_code)
            .await
            .unwrap();
        let (lease, value) = table
            .app
            .context
            .game_executor
            .with_session(session, read)
            .await;
        drop(lease);
        value
    }

    async fn server_revision(table: &NativeTable) -> u64 {
        session_read(table, |session| session.state_revision).await
    }

    async fn server_smaug_damage(table: &NativeTable) -> u64 {
        session_read(table, |session| smaug_damage_in(&session.state)).await
    }

    async fn until_type(socket: &mut ClientSocket, kind: &str) -> serde_json::Value {
        loop {
            let frame = receive_command_frame(socket).await;
            if frame["type"] == kind {
                return frame;
            }
        }
    }

    /// Reads `socket` until a frame of `kind` at `revision` arrives.
    async fn at_revision(
        socket: &mut ClientSocket,
        kind: &str,
        revision: u64,
    ) -> serde_json::Value {
        loop {
            let frame = until_type(socket, kind).await;
            if frame["data"]["state_revision"] == revision {
                return frame;
            }
        }
    }

    async fn ping_pong(socket: &mut ClientSocket, timestamp: u64) {
        send_test_message(socket, &ClientMessage::Ping { timestamp }, false).await;
        while until_type(socket, "Pong").await["data"]["timestamp"] != timestamp {}
    }

    /// The revision of a withheld-`StateUpdate` notice.
    fn withheld_update(frame: &serde_json::Value) -> u64 {
        assert_eq!(frame["type"], "StateWithheld", "{frame}");
        assert_eq!(
            frame["data"]["withheld"],
            serde_json::json!({ "type": "StateUpdate" }),
            "{frame}"
        );
        assert_eq!(
            frame["data"]["reason"],
            crate::socket_mailbox::STATE_UPDATE_WITHHELD
        );
        frame["data"]["state_revision"].as_u64().unwrap()
    }

    async fn request_takeback(requester: &mut ClientSocket) {
        send_test_message(requester, &ClientMessage::RequestTakeback(None), false).await;
    }

    async fn approve_takeback(approver: &mut ClientSocket) {
        send_test_message(
            approver,
            &ClientMessage::RespondTakeback { approve: true },
            false,
        )
        .await;
    }

    async fn assert_requested(socket: &mut ClientSocket, requester: PlayerId) {
        let requested = until_type(socket, "TakebackRequested").await;
        assert_eq!(requested["data"]["requester"], requester.0, "{requested}");
    }

    /// Plain text length and value of the next application frame.
    async fn receive_sized(socket: &mut ClientSocket) -> (usize, serde_json::Value) {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        loop {
            match socket.next().await.expect("live").expect("valid frame") {
                Message::Text(text) => return (text.len(), serde_json::from_str(&text).unwrap()),
                Message::Ping(_) | Message::Pong(_) => {}
                frame => panic!("expected plain JSON, got {frame:?}"),
            }
        }
    }

    fn stripped_len(update: &serde_json::Value) -> usize {
        let mut update = update.clone();
        update["data"]["events"] = serde_json::json!([]);
        update["data"]
            .as_object_mut()
            .unwrap()
            .remove("log_entries");
        serde_json::to_string(&update).unwrap().len()
    }

    async fn server_exiled_bears(table: &NativeTable) -> usize {
        session_read(table, |session| {
            session
                .state
                .objects
                .values()
                .filter(|object| object.name == "Grizzly Bears" && object.zone == Zone::Exile)
                .count()
        })
        .await
    }

    /// Ravenous Trap's resolution arrives whole but stripped of events and log.
    fn assert_trap_stripped(update: &serde_json::Value, revision: u64, exiled: usize) {
        assert_eq!(update["type"], "StateUpdate", "{update}");
        assert_eq!(update["data"]["state_revision"], revision);
        assert_eq!(update["data"]["events"], serde_json::json!([]));
        assert!(update["data"].get("log_entries").is_none());
        let bears = objects_named(update, "Grizzly Bears");
        assert_eq!(bears.len(), GRAVEYARD);
        assert!(bears.iter().all(|card| card["zone"] == "Exile"));
        assert_eq!(bears.len(), exiled);
    }

    /// CR 732.2a + CR 732.2c: the take an accept runs, at a bound its frames exceed.
    async fn declare_and_accept_withheld(
        table: &NativeTable,
        declarer: &mut ClientSocket,
        accepter: &mut ClientSocket,
        before: u64,
    ) -> u64 {
        declare(declarer).await;
        send_command_frame(accepter, &ACCEPT, 7).await;
        let revision = withheld_update(&completion(accepter, 7).await);
        assert_eq!(revision, server_revision(table).await);
        assert_eq!(server_smaug_damage(table).await, before + u64::from(TAKE));
        let seen = at_revision(declarer, "StateWithheld", revision).await;
        assert_eq!(withheld_update(&seen), revision);
        assert!(seen.get("command_id").is_none());
        revision
    }

    async fn pass_withheld(
        table: &NativeTable,
        passer: &mut ClientSocket,
        other: &mut ClientSocket,
        command: u64,
    ) {
        send_command_frame(passer, &GameAction::PassPriority, command).await;
        let revision = withheld_update(&completion(passer, command).await);
        assert_eq!(revision, server_revision(table).await);
        withheld_update(&at_revision(other, "StateWithheld", revision).await);
    }

    #[test]
    fn over_bound_export_is_refused_and_the_socket_kept() {
        run_async_test(|| async {
            const CAP: usize = 2 * 1024 * 1024;
            let table = NativeTable::capped_duel(CAP).await;
            install(&table, smaug_offer(), &[]).await;
            session_read(&table, |session| {
                session.set_hosting(server_core::session::HostingMode::SingleUser);
            })
            .await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut host = table.connect_bridged(P0, CAP).await;
                let mut accepter = table.connect_bridged(P1, CAP).await;
                let export = ClientMessage::ExportAuthoritativeState;
                send_test_message(&mut host, &export, false).await;
                until_type(&mut host, "AuthoritativeStateExport").await;
                declare(&mut host).await;
                send_command_frame(&mut accepter, &ACCEPT, 7).await;
                completion(&mut accepter, 7).await;
                for _ in 0..2 {
                    send_test_message(&mut host, &export, false).await;
                    let refused = until_type(&mut host, "AuthoritativeStateExportFailed").await;
                    let message = refused["data"]["message"].as_str().unwrap();
                    assert!(message.starts_with("too large to send: "), "{message}");
                    assert!(message.contains("at most 2097152 allowed"), "{message}");
                }
                ping_pong(&mut host, 11).await;
            })
            .await;
            table.server.abort();
            outcome.expect("the over-bound export closed or stalled its socket");
        });
    }

    #[test]
    fn over_bound_update_arrives_stripped_of_events_and_log() {
        run_async_test(|| async {
            let measured = NativeTable::duel().await;
            let sizes = tokio::time::timeout(Duration::from_secs(120), async {
                let mut caster = measured.connect_uncapped(P0).await;
                let mut passer = measured.connect_uncapped(P1).await;
                install(&measured, trap_on_the_stack(), &[]).await;
                send_command_frame(&mut passer, &GameAction::PassPriority, 7).await;
                let (passer_len, resolved) = loop {
                    let (len, frame) = receive_sized(&mut passer).await;
                    if owns_command_completion(&frame, 7) {
                        break (len, frame);
                    }
                };
                let revision = resolved["data"]["state_revision"].as_u64().unwrap();
                let (caster_len, seen) = loop {
                    let (len, frame) = receive_sized(&mut caster).await;
                    if frame["type"] == "StateUpdate" && frame["data"]["state_revision"] == revision
                    {
                        break (len, frame);
                    }
                };
                let full = passer_len.min(caster_len);
                let stripped = stripped_len(&resolved).max(stripped_len(&seen));
                assert!(stripped < full, "stripped {stripped} vs full {full}");
                (stripped + full) / 2
            })
            .await;
            measured.server.abort();
            let cap = sizes.expect("the measuring table did not publish");

            let table = NativeTable::capped_duel(cap).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut caster = table.connect_bridged(P0, cap).await;
                let mut passer = table.connect_bridged(P1, cap).await;
                install(&table, trap_on_the_stack(), &[]).await;
                send_command_frame(&mut passer, &GameAction::PassPriority, 7).await;
                let resolved = completion(&mut passer, 7).await;
                let revision = server_revision(&table).await;
                let exiled = server_exiled_bears(&table).await;
                assert_trap_stripped(&resolved, revision, exiled);
                let seen = at_revision(&mut caster, "StateUpdate", revision).await;
                assert_trap_stripped(&seen, revision, exiled);
                ping_pong(&mut passer, 21).await;
                ping_pong(&mut caster, 22).await;
            })
            .await;
            table.server.abort();
            outcome.expect("the stripped update was not delivered");
        });
    }

    #[test]
    fn consecutive_over_bound_updates_are_withheld_until_the_turn_passes() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let table = NativeTable::capped_duel(SMAUG_CAP).await;
            install(&table, offer, &[]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut p0 = table.connect_bridged(P0, SMAUG_CAP).await;
                let mut p1 = table.connect_bridged(P1, SMAUG_CAP).await;
                let mut previous =
                    declare_and_accept_withheld(&table, &mut p0, &mut p1, before).await;
                let start_turn = session_read(&table, |session| session.state.turn_number).await;
                let mut withheld = 0;
                let mut timestamp = 30;
                let mut sockets = [p0, p1];
                for command in 100.. {
                    assert!(command < 150, "the turn did not pass");
                    let waiting =
                        session_read(&table, |session| session.state.waiting_for.clone()).await;
                    let (actor, action) = match waiting {
                        WaitingFor::Priority { player } => (player, GameAction::PassPriority),
                        WaitingFor::DeclareAttackers { player, .. } => (
                            player,
                            GameAction::DeclareAttackers {
                                attacks: vec![],
                                bands: vec![],
                            },
                        ),
                        other => panic!("unexpected decision {other:?}"),
                    };
                    let [s0, s1] = &mut sockets;
                    let (mine, other) = if actor == P0 { (s0, s1) } else { (s1, s0) };
                    send_command_frame(mine, &action, command).await;
                    let answer = completion(mine, command).await;
                    let revision = answer["data"]["state_revision"].as_u64().unwrap();
                    assert_eq!(revision, server_revision(&table).await);
                    assert_eq!(revision, previous + 1);
                    previous = revision;
                    if answer["type"] == "StateUpdate" {
                        assert_eq!(answer["data"]["state"]["turn_number"], start_turn + 1);
                        at_revision(other, "StateUpdate", revision).await;
                        break;
                    }
                    withheld_update(&answer);
                    withheld_update(&at_revision(other, "StateWithheld", revision).await);
                    withheld += 1;
                    if withheld <= 2 {
                        for socket in [&mut *mine, &mut *other] {
                            timestamp += 1;
                            ping_pong(socket, timestamp).await;
                        }
                    }
                }
                assert!(withheld >= 2, "only {withheld} withheld updates");
            })
            .await;
            table.server.abort();
            outcome.expect("the withheld turn closed or stalled a socket");
        });
    }

    #[test]
    fn over_bound_reconnect_stays_attached_and_recovers_by_takeback() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let before = smaug_damage_in(&offer);
            let table = NativeTable::capped_duel(SMAUG_CAP).await;
            install(&table, offer, &[]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut p0 = table.connect_bridged(P0, SMAUG_CAP).await;
                let mut p1 = table.connect_bridged(P1, SMAUG_CAP).await;
                declare_and_accept_withheld(&table, &mut p0, &mut p1, before).await;
                let mut fresh =
                    connect_and_hello_with(table.url.clone(), Some(bridge_config(SMAUG_CAP))).await;
                send_test_message(
                    &mut fresh,
                    &ClientMessage::Reconnect {
                        game_code: table.key.game_code.clone(),
                        player_token: table.tokens[0].clone(),
                        full_key: table.key.clone(),
                    },
                    false,
                )
                .await;
                let notice = loop {
                    let frame = receive_command_frame(&mut fresh).await;
                    assert_ne!(frame["type"], "GameStarted", "{frame}");
                    if frame["type"] == "StateWithheld" {
                        break frame;
                    }
                };
                assert_eq!(
                    notice["data"]["withheld"],
                    serde_json::json!({ "type": "GameStarted", "data": { "your_player": P0 } }),
                    "{notice}"
                );
                assert_eq!(
                    notice["data"]["reason"],
                    crate::socket_mailbox::GAME_STARTED_WITHHELD
                );
                assert_eq!(
                    notice["data"]["state_revision"],
                    server_revision(&table).await
                );
                request_takeback(&mut fresh).await;
                assert_requested(&mut p1, P0).await;
                approve_takeback(&mut p1).await;
                let restored = until_type(&mut fresh, "StateUpdate").await;
                assert_eq!(
                    restored["data"]["state_revision"],
                    server_revision(&table).await
                );
                assert_eq!(smaug_damage(&restored), before);
                assert_eq!(server_smaug_damage(&table).await, before);
                ping_pong(&mut fresh, 41).await;
                drop(p0);
            })
            .await;
            table.server.abort();
            outcome.expect("the withheld reconnect did not recover");
        });
    }

    #[test]
    fn takeback_recovers_from_consecutive_withheld_updates() {
        run_async_test(|| async {
            let offer = smaug_offer();
            let pre = smaug_damage_in(&offer);
            let table = NativeTable::capped_duel(SMAUG_CAP).await;
            install(&table, offer, &[]).await;
            let outcome = tokio::time::timeout(Duration::from_secs(120), async {
                let mut p0 = table.connect_bridged(P0, SMAUG_CAP).await;
                let mut p1 = table.connect_bridged(P1, SMAUG_CAP).await;
                declare_and_accept_withheld(&table, &mut p0, &mut p1, pre).await;
                assert_eq!(
                    session_read(&table, |session| session.state.waiting_for.clone()).await,
                    WaitingFor::Priority { player: P0 }
                );
                pass_withheld(&table, &mut p0, &mut p1, 8).await;

                request_takeback(&mut p1).await;
                assert_requested(&mut p1, P1).await;
                assert_requested(&mut p0, P1).await;
                approve_takeback(&mut p0).await;
                for socket in [&mut p0, &mut p1] {
                    let restored = until_type(socket, "StateUpdate").await;
                    assert!(restored.get("command_id").is_none());
                    assert_eq!(restored["data"]["events"], serde_json::json!([]));
                    assert_eq!(
                        restored["data"]["state_revision"],
                        server_revision(&table).await
                    );
                    assert_eq!(smaug_damage(&restored), pre);
                    let resolved = until_type(socket, "TakebackResolved").await;
                    assert_eq!(
                        resolved["data"],
                        serde_json::json!({ "resolution": "approved", "resolved_by": 0 })
                    );
                }
                assert_eq!(server_smaug_damage(&table).await, pre);
                ping_pong(&mut p0, 51).await;
                ping_pong(&mut p1, 52).await;

                // P0's own last action is then the post-take pass: its first rollback is still
                // over the bound, its second reaches its proposal.
                send_command_frame(&mut p1, &ACCEPT, 9).await;
                let revision = withheld_update(&completion(&mut p1, 9).await);
                withheld_update(&at_revision(&mut p0, "StateWithheld", revision).await);
                pass_withheld(&table, &mut p0, &mut p1, 10).await;
                request_takeback(&mut p0).await;
                assert_requested(&mut p1, P0).await;
                approve_takeback(&mut p1).await;
                for socket in [&mut p0, &mut p1] {
                    let notice = until_type(socket, "StateWithheld").await;
                    assert!(notice.get("command_id").is_none());
                    assert_eq!(withheld_update(&notice), server_revision(&table).await);
                    until_type(socket, "TakebackResolved").await;
                }
                request_takeback(&mut p0).await;
                assert_requested(&mut p1, P0).await;
                approve_takeback(&mut p1).await;
                for socket in [&mut p0, &mut p1] {
                    let restored = until_type(socket, "StateUpdate").await;
                    assert_eq!(
                        restored["data"]["state_revision"],
                        server_revision(&table).await
                    );
                    assert_eq!(smaug_damage(&restored), pre);
                }
                assert_eq!(server_smaug_damage(&table).await, pre);
            })
            .await;
            table.server.abort();
            outcome.expect("the takeback did not recover the withheld game");
        });
    }

    /// Concessions while a takeback vote is open, through real sockets.
    mod concession_takeback {
        use super::*;
        use engine::types::mana::ManaColor;
        use engine::types::match_config::MatchType;

        const P2: PlayerId = PlayerId(2);
        const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";

        fn add_bolt(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
            scenario
                .add_spell_to_hand_from_oracle(owner, "Lightning Bolt", true, LIGHTNING_BOLT)
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 0,
                })
                .id()
        }

        fn cast_bolt(runner: &mut GameRunner, bolt: ObjectId, target: PlayerId) {
            let card_id = runner.state().objects[&bolt].card_id;
            act(
                runner,
                GameAction::CastSpell {
                    object_id: bolt,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                },
            );
            act(
                runner,
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(target)),
                },
            );
        }

        /// Three seats at P0's precombat main: P0 passed, P1 cast Lightning Bolt
        /// at P0, and P1 and P2 passed, so P0 holds priority with the Bolt on
        /// the stack.
        fn bolt_at_p0() -> GameState {
            let mut scenario = GameScenario::new_n_player(3, 29);
            scenario.at_phase(Phase::PreCombatMain);
            scenario.add_basic_land(P1, ManaColor::Red);
            let bolt = add_bolt(&mut scenario, P1);
            let mut runner = scenario.build();
            act(&mut runner, GameAction::PassPriority);
            cast_bolt(&mut runner, bolt, P0);
            act(&mut runner, GameAction::PassPriority);
            act(&mut runner, GameAction::PassPriority);
            assert_eq!(
                runner.state().waiting_for,
                WaitingFor::Priority { player: P0 }
            );
            assert_eq!(runner.state().stack.len(), 1);
            runner.state().clone()
        }

        /// Bolt-AI: three seats at P0's precombat main. P0 holds a Mountain on
        /// the battlefield and one in hand and has cast Lightning Bolt at P2 (3
        /// life); P1 is to be an AI seat. P0 holds priority with the Bolt on
        /// the stack.
        fn bolt_ai() -> GameState {
            let mut scenario = GameScenario::new_n_player(3, 31);
            scenario.at_phase(Phase::PreCombatMain);
            scenario.with_life(P2, 3);
            scenario.add_basic_land(P0, ManaColor::Red);
            scenario.add_land_to_hand(P0, "Mountain");
            let bolt = add_bolt(&mut scenario, P0);
            let mut runner = scenario.build();
            cast_bolt(&mut runner, bolt, P2);
            assert_eq!(runner.state().stack.len(), 1);
            runner.state().clone()
        }

        /// A two-seat Bo3 game 1 at P0's precombat main, P0 holding priority.
        fn bo3_duel() -> GameState {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let mut state = scenario.build().state().clone();
            state.match_config.match_type = MatchType::Bo3;
            state.game_number = 1;
            state
        }

        /// Reads `socket` until the takeback withdrawal attributed to
        /// `conceding`, refusing a concession `StateUpdate` before it, then
        /// reads and returns the concession's `StateUpdate`.
        async fn withdrawal_then_state(
            socket: &mut ClientSocket,
            conceding: PlayerId,
        ) -> serde_json::Value {
            loop {
                let frame = receive_command_frame(socket).await;
                match frame["type"].as_str() {
                    Some("TakebackResolved") => {
                        assert_eq!(
                            frame["data"],
                            serde_json::json!({ "resolution": "withdrawn", "resolved_by": conceding.0 })
                        );
                        break;
                    }
                    Some("StateUpdate") | Some("ActionRejected") | Some("RequestRejected") => {
                        panic!("expected the withdrawal first, got {frame}")
                    }
                    _ => {}
                }
            }
            let update = until_type(socket, "StateUpdate").await;
            assert_eq!(
                update["data"]["eliminated_players"],
                serde_json::json!([conceding.0]),
                "{update}"
            );
            update
        }

        async fn three_human_vote(table: &NativeTable) -> [ClientSocket; 3] {
            let mut p0 = table.connect(P0).await;
            let mut p1 = table.connect(P1).await;
            let mut p2 = table.connect(P2).await;
            send_command_frame(&mut p0, &GameAction::PassPriority, 1).await;
            completion(&mut p0, 1).await;
            request_takeback(&mut p0).await;
            for socket in [&mut p0, &mut p1, &mut p2] {
                assert_requested(socket, P0).await;
            }
            [p0, p1, p2]
        }

        async fn three_human_table() -> NativeTable {
            let table = NativeTable::seated(
                GameExecutor::new(NonZeroUsize::MIN),
                3,
                FormatConfig::free_for_all(),
            )
            .await;
            install(&table, bolt_at_p0(), &[]).await;
            table
        }

        /// N1. `ClientMessage::Concede` during a vote: every seat gets the
        /// withdrawal before the concession's state.
        #[test]
        fn a_concede_message_during_a_vote_reaches_every_seat_after_the_withdrawal() {
            run_async_test(|| async {
                let table = three_human_table().await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let [mut p0, mut p1, mut p2] = three_human_vote(&table).await;
                    send_test_message(&mut p2, &ClientMessage::Concede, false).await;
                    for socket in [&mut p2, &mut p0, &mut p1] {
                        withdrawal_then_state(socket, P2).await;
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the withdrawal or the concession never arrived");
            });
        }

        /// N2. The same through a `GameAction::Concede` command frame.
        #[test]
        fn a_concede_action_during_a_vote_reaches_every_seat_after_the_withdrawal() {
            run_async_test(|| async {
                let table = three_human_table().await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let [mut p0, mut p1, mut p2] = three_human_vote(&table).await;
                    send_command_frame(&mut p2, &GameAction::Concede { player_id: P2 }, 2).await;
                    for socket in [&mut p2, &mut p0, &mut p1] {
                        withdrawal_then_state(socket, P2).await;
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the withdrawal or the concession never arrived");
            });
        }

        /// N3. `ConcedeMatch` during a vote: the withdrawal precedes the
        /// concession's state.
        #[test]
        fn a_match_concession_during_a_vote_follows_the_withdrawal() {
            run_async_test(|| async {
                let table = NativeTable::duel().await;
                install(&table, bo3_duel(), &[]).await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut p0 = table.connect(P0).await;
                    let mut p1 = table.connect(P1).await;
                    send_command_frame(&mut p0, &GameAction::PassPriority, 1).await;
                    completion(&mut p0, 1).await;
                    request_takeback(&mut p0).await;
                    assert_requested(&mut p0, P0).await;
                    assert_requested(&mut p1, P0).await;
                    send_test_message(&mut p1, &ClientMessage::ConcedeMatch, false).await;
                    for socket in [&mut p1, &mut p0] {
                        loop {
                            let frame = receive_command_frame(socket).await;
                            match frame["type"].as_str() {
                                Some("TakebackResolved") => {
                                    assert_eq!(
                                        frame["data"],
                                        serde_json::json!({ "resolution": "withdrawn", "resolved_by": 1 })
                                    );
                                    break;
                                }
                                Some("StateUpdate") | Some("RequestRejected") => {
                                    panic!("expected the withdrawal first, got {frame}")
                                }
                                _ => {}
                            }
                        }
                        let update = until_type(socket, "StateUpdate").await;
                        assert_eq!(
                            update["data"]["state"]["waiting_for"],
                            serde_json::json!({ "type": "GameOver", "data": { "winner": 0 } }),
                            "{update}"
                        );
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the withdrawal or the match concession never arrived");
            });
        }

        /// N4. A target-branch voter's `ClientMessage::Concede` approves the
        /// vote: the restored state, then the approval.
        #[test]
        fn a_voters_concede_message_publishes_the_restored_state_then_the_approval() {
            run_async_test(|| async {
                let table = NativeTable::seated(
                    GameExecutor::new(NonZeroUsize::MIN),
                    3,
                    FormatConfig::free_for_all(),
                )
                .await;
                install(&table, bolt_ai(), &[P1]).await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut p0 = table.connect(P0).await;
                    let mut p2 = table.connect(P2).await;
                    send_command_frame(&mut p0, &GameAction::PassPriority, 1).await;
                    completion(&mut p0, 1).await;
                    send_command_frame(&mut p2, &GameAction::PassPriority, 2).await;
                    completion(&mut p2, 2).await;
                    assert_eq!(
                        session_read(&table, |session| session.state.eliminated_players.clone())
                            .await,
                        vec![P2]
                    );
                    request_takeback(&mut p0).await;
                    assert_requested(&mut p0, P0).await;
                    assert_requested(&mut p2, P0).await;
                    send_test_message(&mut p2, &ClientMessage::Concede, false).await;
                    for (seat, socket) in [(P2, &mut p2), (P0, &mut p0)] {
                        let restored = loop {
                            let frame = receive_command_frame(socket).await;
                            match frame["type"].as_str() {
                                Some("StateUpdate") => break frame,
                                Some("TakebackResolved") | Some("ActionRejected") => {
                                    panic!("expected the restored state first, got {frame}")
                                }
                                _ => {}
                            }
                        };
                        assert_eq!(
                            restored["data"]["state"]["stack"].as_array().map(Vec::len),
                            Some(1),
                            "the Bolt is back on the stack: {restored}"
                        );
                        assert_eq!(
                            restored["data"]["eliminated_players"],
                            serde_json::json!([])
                        );
                        if seat == P0 {
                            let actions = restored["data"]["legal_actions"]
                                .as_array()
                                .expect("legal actions");
                            assert!(!actions.is_empty(), "{restored}");
                            assert!(
                                actions.iter().all(|action| action["type"] != "PlayLand"),
                                "{restored}"
                            );
                        }
                        let resolved = until_type(socket, "TakebackResolved").await;
                        assert_eq!(
                            resolved["data"],
                            serde_json::json!({ "resolution": "approved", "resolved_by": 2 })
                        );
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the restored state or the approval never arrived");
            });
        }

        /// N5. A spectator receives the concession's state.
        #[test]
        fn a_spectator_receives_the_concessions_state() {
            run_async_test(|| async {
                let table = three_human_table().await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut p0 = table.connect(P0).await;
                    let mut p2 = table.connect(P2).await;
                    let mut spectator = connect_and_hello(table.url.clone()).await;
                    send_test_message(
                        &mut spectator,
                        &ClientMessage::SpectatorJoin {
                            game_code: table.key.game_code.clone(),
                        },
                        false,
                    )
                    .await;
                    until_type(&mut spectator, "GameStarted").await;
                    send_test_message(&mut p2, &ClientMessage::Concede, false).await;
                    for socket in [&mut p0, &mut spectator] {
                        loop {
                            let update = until_type(socket, "StateUpdate").await;
                            if update["data"]["eliminated_players"] == serde_json::json!([2]) {
                                break;
                            }
                        }
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the concession's state never reached the spectator");
            });
        }

        /// N6. A `ClientMessage::Concede` that hands priority to an AI seat
        /// runs that seat's decision.
        #[test]
        fn a_concede_message_that_passes_priority_to_an_ai_seat_runs_the_ai() {
            run_async_test(|| async {
                let table = NativeTable::seated(
                    GameExecutor::new(NonZeroUsize::MIN),
                    3,
                    FormatConfig::free_for_all(),
                )
                .await;
                let mut scenario = GameScenario::new_n_player(3, 37);
                scenario.at_phase(Phase::PreCombatMain);
                install(&table, scenario.build().state().clone(), &[P1]).await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut p0 = table.connect(P0).await;
                    let mut p2 = table.connect(P2).await;
                    send_test_message(&mut p0, &ClientMessage::Concede, false).await;
                    loop {
                        let update = until_type(&mut p2, "StateUpdate").await;
                        if update["data"]["state"]["waiting_for"]
                            == serde_json::json!({ "type": "Priority", "data": { "player": 2 } })
                        {
                            assert_eq!(
                                update["data"]["eliminated_players"],
                                serde_json::json!([0]),
                                "{update}"
                            );
                            break;
                        }
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the AI seat never acted after the concession");
            });
        }

        /// N7. A voter's `ClientMessage::Concede` that hands priority to an AI
        /// seat: every human gets the withdrawal, then the concession's state,
        /// then the AI's decision.
        #[test]
        fn a_voters_concede_message_that_passes_priority_to_an_ai_seat_follows_the_withdrawal() {
            run_async_test(|| async {
                let table = NativeTable::seated(
                    GameExecutor::new(NonZeroUsize::MIN),
                    3,
                    FormatConfig::free_for_all(),
                )
                .await;
                let mut scenario = GameScenario::new_n_player(3, 41);
                scenario.at_phase(Phase::PreCombatMain);
                install(&table, scenario.build().state().clone(), &[P2]).await;
                let outcome = tokio::time::timeout(Duration::from_secs(30), async {
                    let mut p0 = table.connect(P0).await;
                    let mut p1 = table.connect(P1).await;
                    send_command_frame(&mut p0, &GameAction::PassPriority, 1).await;
                    completion(&mut p0, 1).await;
                    request_takeback(&mut p0).await;
                    assert_requested(&mut p0, P0).await;
                    assert_requested(&mut p1, P0).await;
                    send_test_message(&mut p1, &ClientMessage::Concede, false).await;
                    let ai_priority =
                        serde_json::json!({ "type": "Priority", "data": { "player": 2 } });
                    for socket in [&mut p1, &mut p0] {
                        // CR 800.4a: P1 left while holding priority, so it passes to P2.
                        let conceded = withdrawal_then_state(socket, P1).await;
                        assert_eq!(
                            conceded["data"]["state"]["waiting_for"], ai_priority,
                            "{conceded}"
                        );
                        let acted = until_type(socket, "StateUpdate").await;
                        assert_ne!(
                            acted["data"]["state"]["waiting_for"], ai_priority,
                            "{acted}"
                        );
                        assert_eq!(acted["data"]["eliminated_players"], serde_json::json!([1]));
                    }
                })
                .await;
                table.server.abort();
                outcome.expect("the withdrawal, the concession, or the AI decision never arrived");
            });
        }
    }
}
