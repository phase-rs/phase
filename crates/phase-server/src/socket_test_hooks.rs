//! Test-only barriers observe real socket delivery without delaying game work.

use std::sync::Arc;

use parking_lot::Mutex;
use server_core::protocol::ServerMessage;
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub(crate) struct Hooks {
    bootstrap: Arc<Mutex<Option<Arc<BootstrapGate>>>>,
}

#[derive(Default)]
pub(crate) struct BootstrapGate {
    entered: Notify,
    release: Notify,
}

impl Hooks {
    pub(crate) fn arm_bootstrap(&self) -> Arc<BootstrapGate> {
        let gate = Arc::new(BootstrapGate::default());
        *self.bootstrap.lock() = Some(gate.clone());
        gate
    }

    pub(crate) async fn before_encode(&self, message: &ServerMessage) {
        if matches!(message, ServerMessage::GameStarted { .. }) {
            let gate = self.bootstrap.lock().take();
            if let Some(gate) = gate {
                gate.entered.notify_one();
                gate.release.notified().await;
            }
        }
    }
}

/// Match the production owner and blocking-worker stack without mutating the
/// process environment or imposing a stack requirement on other libtest cases.
/// A current-thread runtime makes the I/O liveness proof strictly single-worker.
pub(crate) fn run_async_test<F: std::future::Future<Output = ()>>(
    test: impl FnOnce() -> F + Send + 'static,
) {
    std::thread::Builder::new()
        .name("socket-test-runtime".to_string())
        .stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

#[test]
fn reconnect_bootstrap_precedes_a_concurrent_newer_action() {
    run_async_test(|| async {
        use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
        use crate::issue_4548_full_create_tests::{
            recv_server_message, spawn_full_mode_server_with_db,
        };
        use engine::types::actions::{GameAction, MulliganChoice};
        use engine::types::format::FormatConfig;
        use futures_util::SinkExt;
        use server_core::protocol::{ClientMessage, DeckData};
        use std::time::Duration;
        use tokio_tungstenite::tungstenite::Message;

        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let result = tokio::time::timeout(Duration::from_secs(8), async {
            let deck = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let mut host = connect_and_hello(url.clone()).await;
            let create = ClientMessage::CreateGameWithSettings {
                deck: deck.clone(),
                display_name: "host".to_string(),
                public: false,
                password: None,
                timer_seconds: None,
                player_count: 2,
                match_config: Default::default(),
                ai_seats: Vec::new(),
                format_config: Some(FormatConfig::freeform()),
                room_name: None,
                host_peer_id: None,
                draft_metadata: None,
                start_when_full: true,
                ranked: false,
                requested_code: None,
                booster_pack_pool: None,
            };
            host.send(Message::Text(
                serde_json::to_string(&create).unwrap().into(),
            ))
            .await
            .unwrap();
            let (code, token, key) = loop {
                match recv_server_message(&mut host).await {
                    ServerMessage::GameCreated {
                        game_code,
                        player_token,
                        full_key: Some(key),
                    } => break (game_code, player_token, key),
                    ServerMessage::Error { message, .. } => panic!("create: {message}"),
                    _ => {}
                }
            };
            let mut guest = connect_and_hello(url.clone()).await;
            guest
                .send(Message::Text(
                    serde_json::to_string(&ClientMessage::JoinGame {
                        game_code: code.clone(),
                        deck,
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            for socket in [&mut host, &mut guest] {
                loop {
                    match recv_server_message(socket).await {
                        ServerMessage::GameStarted { .. } => break,
                        ServerMessage::Error { message, .. } => panic!("start: {message}"),
                        _ => {}
                    }
                }
            }
            host.close(None).await.unwrap();
            let gate = app.context.socket_hooks.arm_bootstrap();
            let mut returning = connect_and_hello(url).await;
            returning
                .send(Message::Text(
                    serde_json::to_string(&ClientMessage::Reconnect {
                        game_code: code,
                        player_token: token,
                        full_key: key,
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            gate.entered.notified().await;
            guest
                .send(Message::Text(
                    serde_json::to_string(&ClientMessage::Action {
                        action: GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        },
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            let action_revision = loop {
                match recv_server_message(&mut guest).await {
                    ServerMessage::StateUpdate { state_revision, .. } => break state_revision,
                    ServerMessage::ActionRejected { rejection } => panic!("action: {rejection:?}"),
                    _ => {}
                }
            };
            // The newer action has completed, but its update must remain behind the
            // bootstrap whose encode is paused. This fails with the direct-send lane.
            assert!(
                tokio::time::timeout(Duration::from_millis(500), async {
                    loop {
                        if matches!(
                            recv_server_message(&mut returning).await,
                            ServerMessage::GameStarted { .. } | ServerMessage::StateUpdate { .. }
                        ) {
                            break;
                        }
                    }
                })
                .await
                .is_err(),
                "a newer state overtook the reconnect bootstrap"
            );
            gate.release.notify_one();
            let bootstrap_revision = loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::GameStarted { state_revision, .. } => break state_revision,
                    ServerMessage::StateUpdate { state_revision, .. } => {
                        panic!("update {state_revision} arrived before reconnect bootstrap")
                    }
                    _ => {}
                }
            };
            let next_revision = loop {
                if let ServerMessage::StateUpdate { state_revision, .. } =
                    recv_server_message(&mut returning).await
                {
                    break state_revision;
                }
            };
            assert_eq!(next_revision, action_revision);
            assert!(
                next_revision > bootstrap_revision,
                "the reconnect rolled state backward"
            );
        })
        .await;
        server.abort();
        result.expect("reconnect/action barrier did not complete");
    });
}
