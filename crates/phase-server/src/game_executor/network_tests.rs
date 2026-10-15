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
