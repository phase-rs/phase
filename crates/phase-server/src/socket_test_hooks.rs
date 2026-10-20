//! Test-only barriers observe real socket delivery without delaying game work.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::extract::ws::Message;
use futures_util::future::BoxFuture;
use futures_util::{FutureExt, Sink};

use parking_lot::Mutex;
use server_core::protocol::ServerMessage;
use tokio::sync::Notify;

#[derive(Clone, Default)]
pub(crate) struct Hooks {
    bootstrap: Arc<Mutex<Option<Arc<BootstrapGate>>>>,
    cutover: Arc<Mutex<Option<Arc<BootstrapGate>>>>,
    terminal_prepare: Arc<Mutex<Option<Arc<BootstrapGate>>>>,
    quota_flush: Arc<Mutex<Option<Arc<BootstrapGate>>>>,
    parser_work: Arc<Mutex<Option<Arc<ParserWork>>>>,
    hello_accept: Arc<Mutex<Option<Arc<BootstrapGate>>>>,
}

/// One connection's real parser calls, with a frozen quota window so wire tests
/// distinguish admission from parser work without relying on machine speed.
pub(crate) struct ParserWork {
    quota_at: std::time::Instant,
    deserializations: std::sync::atomic::AtomicUsize,
    metadata_recoveries: std::sync::atomic::AtomicUsize,
    limited: std::sync::atomic::AtomicUsize,
    changed: Notify,
    probe_at: tokio::time::Instant,
    probes: std::sync::atomic::AtomicUsize,
    completed: std::sync::atomic::AtomicUsize,
}

impl Default for ParserWork {
    fn default() -> Self {
        Self {
            quota_at: std::time::Instant::now(),
            deserializations: Default::default(),
            metadata_recoveries: Default::default(),
            limited: Default::default(),
            changed: Default::default(),
            probe_at: tokio::time::Instant::now(),
            probes: Default::default(),
            completed: Default::default(),
        }
    }
}

impl ParserWork {
    pub(crate) fn quota_at(&self) -> std::time::Instant {
        self.quota_at
    }

    pub(crate) fn counts(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering;
        (
            self.deserializations.load(Ordering::SeqCst),
            self.metadata_recoveries.load(Ordering::SeqCst),
        )
    }

    pub(crate) fn deserialization(&self) {
        self.deserializations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_one();
    }

    pub(crate) fn metadata_recovery(&self) {
        self.metadata_recoveries
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_one();
    }

    pub(crate) fn rate_limited(&self) {
        self.limited
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.changed.notify_one();
    }

    pub(crate) async fn wait_rate_limited(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            if self.limited.load(std::sync::atomic::Ordering::SeqCst) >= count {
                return;
            }
            changed.await;
        }
    }

    pub(crate) async fn wait_counts(&self, expected: (usize, usize)) {
        loop {
            let changed = self.changed.notified();
            let observed = self.counts();
            if observed.0 >= expected.0 && observed.1 >= expected.1 {
                return;
            }
            changed.await;
        }
    }

    pub(crate) fn probe_at(&self) -> tokio::time::Instant {
        self.probe_at
    }

    pub(crate) fn probe_parse(&self) {
        self.probes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn probes(&self) -> usize {
        self.probes.load(std::sync::atomic::Ordering::SeqCst)
    }
}

pub(crate) struct FrameCompletion(Arc<ParserWork>);

impl Drop for FrameCompletion {
    fn drop(&mut self) {
        self.0
            .completed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.0.changed.notify_one();
    }
}

impl ParserWork {
    pub(crate) fn frame_guard(self: &Arc<Self>) -> FrameCompletion {
        FrameCompletion(self.clone())
    }

    pub(crate) async fn wait_completed(&self, count: usize) {
        loop {
            let changed = self.changed.notified();
            if self.completed.load(std::sync::atomic::Ordering::SeqCst) >= count {
                return;
            }
            changed.await;
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct BootstrapGate {
    pub(crate) entered: Notify,
    pub(crate) release: Notify,
}

impl Hooks {
    pub(crate) fn arm_hello_accept(&self) -> Arc<BootstrapGate> {
        let gate = Arc::new(BootstrapGate::default());
        *self.hello_accept.lock() = Some(gate.clone());
        gate
    }

    pub(crate) async fn before_hello_accept(&self) {
        let gate = self.hello_accept.lock().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }

    pub(crate) fn arm_parser_work(&self) -> Arc<ParserWork> {
        let work = Arc::new(ParserWork::default());
        *self.parser_work.lock() = Some(work.clone());
        work
    }

    pub(crate) fn take_parser_work(&self) -> Option<Arc<ParserWork>> {
        self.parser_work.lock().take()
    }

    pub(crate) fn arm_bootstrap(&self) -> Arc<BootstrapGate> {
        let gate = Arc::new(BootstrapGate::default());
        *self.bootstrap.lock() = Some(gate.clone());
        gate
    }

    pub(crate) fn arm_cutover(&self) -> Arc<BootstrapGate> {
        let gate = Arc::new(BootstrapGate::default());
        *self.cutover.lock() = Some(gate.clone());
        gate
    }

    pub(crate) fn arm_terminal_prepare(&self) -> Arc<BootstrapGate> {
        let gate = Arc::new(BootstrapGate::default());
        *self.terminal_prepare.lock() = Some(gate.clone());
        gate
    }

    pub(crate) fn arm_quota_flush(&self) -> Arc<BootstrapGate> {
        let gate = Arc::new(BootstrapGate::default());
        *self.quota_flush.lock() = Some(gate.clone());
        gate
    }

    pub(crate) fn take_quota_flush(&self) -> Option<Arc<BootstrapGate>> {
        self.quota_flush.lock().take()
    }

    pub(crate) async fn before_bootstrap_cutover(&self) {
        let gate = self.cutover.lock().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
    }

    pub(crate) async fn before_terminal_prepare(&self) {
        let gate = self.terminal_prepare.lock().take();
        if let Some(gate) = gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
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

/// Pause only poll_flush: poll_ready and start_send still reach the real sink.
/// The test therefore holds an encoded refusal in the actual writer path.
pub(crate) struct FlushGateSink<'a, S> {
    inner: &'a mut S,
    waiting: Option<BoxFuture<'static, ()>>,
}

impl<'a, S> FlushGateSink<'a, S> {
    pub(crate) fn new(inner: &'a mut S, gate: Option<Arc<BootstrapGate>>) -> Self {
        Self {
            inner,
            waiting: gate.map(|gate| {
                async move {
                    gate.entered.notify_one();
                    gate.release.notified().await;
                }
                .boxed()
            }),
        }
    }
}

impl<S: Sink<Message> + Unpin> Sink<Message> for FlushGateSink<'_, S> {
    type Error = S::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut *self.get_mut().inner).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
        Pin::new(&mut *self.get_mut().inner).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        if let Some(waiting) = this.waiting.as_mut() {
            std::task::ready!(std::future::Future::poll(waiting.as_mut(), cx));
            this.waiting = None;
        }
        Pin::new(&mut *this.inner).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut *self.get_mut().inner).poll_close(cx)
    }
}

/// Match the production owner and blocking-worker stack without mutating the
/// process environment or imposing a stack requirement on other libtest cases.
/// A current-thread runtime makes the I/O liveness proof strictly single-worker.
pub(crate) fn run_async_test<F: std::future::Future<Output = ()>>(
    test: impl FnOnce() -> F + Send + 'static,
) {
    run_async_test_with_blocking_limit(None, test);
}

pub(crate) fn run_async_test_with_blocking_limit<F: std::future::Future<Output = ()>>(
    blocking_limit: Option<usize>,
    test: impl FnOnce() -> F + Send + 'static,
) {
    std::thread::Builder::new()
        .name("socket-test-runtime".to_string())
        .stack_size(crate::RUNTIME_THREAD_STACK_BYTES)
        .spawn(move || {
            let mut runtime = tokio::runtime::Builder::new_current_thread();
            runtime
                .enable_all()
                .thread_stack_size(crate::RUNTIME_THREAD_STACK_BYTES);
            if let Some(limit) = blocking_limit {
                runtime.max_blocking_threads(limit);
            }
            runtime.build().unwrap().block_on(test());
        })
        .unwrap()
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
}

pub(crate) fn room_request(
    deck: server_core::protocol::DeckData,
    player_count: u8,
    requested_code: Option<String>,
) -> server_core::protocol::ClientMessage {
    server_core::protocol::ClientMessage::CreateGameWithSettings {
        deck,
        display_name: "host".to_string(),
        public: false,
        password: None,
        timer_seconds: None,
        player_count,
        match_config: Default::default(),
        ai_seats: Vec::new(),
        format_config: Some(engine::types::format::FormatConfig::free_for_all()),
        room_name: None,
        host_peer_id: None,
        draft_metadata: None,
        start_when_full: true,
        ranked: false,
        requested_code,
        booster_pack_pool: None,
    }
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

#[test]
fn fresh_join_bootstrap_precedes_a_concurrent_room_start() {
    run_async_test(|| async {
        use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
        use crate::issue_4548_full_create_tests::{
            recv_server_message, send_test_message, spawn_full_mode_server_with_db,
        };
        use engine::types::format::FormatConfig;
        use server_core::protocol::{ClientMessage, DeckData};
        use std::time::Duration;

        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let deck = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let mut host = connect_and_hello(url.clone()).await;
            send_test_message(
                &mut host,
                &ClientMessage::CreateGameWithSettings {
                    deck: deck.clone(),
                    display_name: "host".to_string(),
                    public: false,
                    password: None,
                    timer_seconds: None,
                    player_count: 3,
                    match_config: Default::default(),
                    ai_seats: Vec::new(),
                    format_config: Some(FormatConfig::free_for_all()),
                    room_name: None,
                    host_peer_id: None,
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
                match recv_server_message(&mut host).await {
                    ServerMessage::GameCreated { game_code, .. } => break game_code,
                    ServerMessage::Error { message, .. } => panic!("create: {message}"),
                    _ => {}
                }
            };
            let gate = app.context.socket_hooks.arm_cutover();
            let mut waiting = connect_and_hello(url.clone()).await;
            send_test_message(
                &mut waiting,
                &ClientMessage::JoinGame {
                    game_code: code.clone(),
                    deck: deck.clone(),
                },
                false,
            )
            .await;
            gate.entered.notified().await;
            let mut final_guest = connect_and_hello(url).await;
            send_test_message(
                &mut final_guest,
                &ClientMessage::JoinGame {
                    game_code: code,
                    deck,
                },
                false,
            )
            .await;
            assert!(
                tokio::time::timeout(Duration::from_secs(1), async {
                    loop {
                        if matches!(
                            recv_server_message(&mut host).await,
                            ServerMessage::GameStarted { .. }
                        ) {
                            break;
                        }
                    }
                })
                .await
                .is_err(),
                "the room started before the joining seat's bootstrap cutover"
            );
            gate.release.notify_one();
            let bootstrap_revision = loop {
                match recv_server_message(&mut waiting).await {
                    ServerMessage::StateUpdate { state_revision, .. } => break state_revision,
                    ServerMessage::GameStarted { .. } => panic!("start overtook fresh bootstrap"),
                    ServerMessage::Error { message, .. } => panic!("join: {message}"),
                    _ => {}
                }
            };
            loop {
                if let ServerMessage::GameStarted { state_revision, .. } =
                    recv_server_message(&mut waiting).await
                {
                    assert!(state_revision >= bootstrap_revision);
                    break;
                }
            }
        })
        .await;
        server.abort();
        result.expect("fresh-join bootstrap or room start stalled");
    });
}

#[test]
fn creator_bootstrap_does_not_wait_for_unrelated_lobby_work() {
    run_async_test(|| async {
        use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
        use crate::issue_4548_full_create_tests::{
            recv_server_message, send_test_message, spawn_full_mode_server_with_db,
        };
        use server_core::protocol::DeckData;
        use std::time::Duration;

        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let mut host = connect_and_hello(url).await;
            let held = app.lobby.lock().await;
            send_test_message(
                &mut host,
                &room_request(
                    DeckData {
                        main_deck: vec!["Network Test Land".to_string(); 60],
                        ..Default::default()
                    },
                    2,
                    Some("BOOT01".to_string()),
                ),
                false,
            )
            .await;
            loop {
                if app.sessions.lock().await.contains_game("BOOT01") {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(matches!(
                recv_server_message(&mut host).await,
                ServerMessage::GameCreated { game_code, .. } if game_code == "BOOT01"
            ));
            assert!(matches!(
                recv_server_message(&mut host).await,
                ServerMessage::SessionAttached {
                    player_id: engine::types::player::PlayerId(0),
                    ..
                }
            ));
            drop(held);
        })
        .await;
        server.abort();
        outcome.expect("a published creator had no bootstrap while the lobby was busy");
    });
}

#[test]
fn starting_join_bootstrap_precedes_an_existing_players_action() {
    run_async_test(|| async {
        use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
        use crate::issue_4548_full_create_tests::{
            recv_server_message, send_test_message, spawn_full_mode_server_with_db,
        };
        use engine::types::actions::{GameAction, MulliganChoice};
        use server_core::protocol::{ClientMessage, DeckData};
        use std::time::Duration;

        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let deck = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let mut host = connect_and_hello(url.clone()).await;
            send_test_message(&mut host, &room_request(deck.clone(), 2, None), false).await;
            let code = loop {
                if let ServerMessage::GameCreated { game_code, .. } =
                    recv_server_message(&mut host).await
                {
                    break game_code;
                }
            };
            let gate = app.context.socket_hooks.arm_cutover();
            let mut guest = connect_and_hello(url).await;
            send_test_message(
                &mut guest,
                &ClientMessage::JoinGame {
                    game_code: code,
                    deck,
                },
                false,
            )
            .await;
            gate.entered.notified().await;
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
            // Startup retains its ordering lease through CPU admission and
            // publication. An existing player cannot advance that unpublished
            // revision, nor may the joining socket receive an update first.
            assert!(
                tokio::time::timeout(Duration::from_millis(250), async {
                    loop {
                        if matches!(
                            recv_server_message(&mut guest).await,
                            ServerMessage::StateUpdate { .. }
                        ) {
                            break;
                        }
                    }
                })
                .await
                .is_err(),
                "an unbootstrapped joining socket received an action update"
            );
            gate.release.notify_one();
            let bootstrap_revision = loop {
                match recv_server_message(&mut guest).await {
                    ServerMessage::GameStarted { state_revision, .. } => break state_revision,
                    ServerMessage::StateUpdate { .. } => panic!("update overtook startup"),
                    _ => {}
                }
            };
            let action_revision = loop {
                match recv_server_message(&mut host).await {
                    ServerMessage::StateUpdate { state_revision, .. } => break state_revision,
                    ServerMessage::ActionRejected { rejection } => panic!("action: {rejection:?}"),
                    _ => {}
                }
            };
            assert!(action_revision > bootstrap_revision);
            loop {
                if let ServerMessage::StateUpdate { state_revision, .. } =
                    recv_server_message(&mut guest).await
                {
                    assert_eq!(state_revision, action_revision);
                    break;
                }
            }
        })
        .await;
        server.abort();
        outcome.expect("starting join or concurrent action did not complete");
    });
}

#[test]
fn pregame_return_bootstrap_precedes_a_concurrent_room_start() {
    run_async_test(|| async {
        use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
        use crate::issue_4548_full_create_tests::{
            recv_server_message, send_test_message, spawn_full_mode_server_with_db,
        };
        use server_core::protocol::{ClientMessage, DeckData};
        use std::time::Duration;

        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let deck = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let mut host = connect_and_hello(url.clone()).await;
            send_test_message(&mut host, &room_request(deck.clone(), 3, None), false).await;
            let (code, token, key) = loop {
                if let ServerMessage::GameCreated {
                    game_code,
                    player_token,
                    full_key: Some(key),
                } = recv_server_message(&mut host).await
                {
                    break (game_code, player_token, key);
                }
            };
            let mut first_guest = connect_and_hello(url.clone()).await;
            send_test_message(
                &mut first_guest,
                &ClientMessage::JoinGame {
                    game_code: code.clone(),
                    deck: deck.clone(),
                },
                false,
            )
            .await;
            while !matches!(
                recv_server_message(&mut first_guest).await,
                ServerMessage::StateUpdate { .. }
            ) {}
            host.close(None).await.unwrap();
            let gate = app.context.socket_hooks.arm_cutover();
            let mut returning = connect_and_hello(url.clone()).await;
            send_test_message(
                &mut returning,
                &ClientMessage::Reconnect {
                    game_code: code.clone(),
                    player_token: token,
                    full_key: key.clone(),
                },
                false,
            )
            .await;
            gate.entered.notified().await;
            let mut final_guest = connect_and_hello(url).await;
            send_test_message(
                &mut final_guest,
                &ClientMessage::JoinGame {
                    game_code: code.clone(),
                    deck,
                },
                false,
            )
            .await;
            while !matches!(
                recv_server_message(&mut final_guest).await,
                ServerMessage::GameStarted { .. }
            ) {}
            loop {
                match recv_server_message(&mut returning).await {
                    ServerMessage::GameCreated {
                        game_code,
                        full_key: Some(actual),
                        ..
                    } => {
                        assert_eq!(game_code, code);
                        assert_eq!(actual, key);
                        break;
                    }
                    ServerMessage::GameStarted { .. } => panic!("start overtook room bootstrap"),
                    _ => {}
                }
            }
            while !matches!(
                recv_server_message(&mut returning).await,
                ServerMessage::GameStarted { .. }
            ) {}
            gate.release.notify_one();
            assert!(
                tokio::time::timeout(Duration::from_millis(250), async {
                    loop {
                        if matches!(
                            recv_server_message(&mut returning).await,
                            ServerMessage::GameCreated { .. }
                        ) {
                            break;
                        }
                    }
                })
                .await
                .is_err(),
                "late duplicate creation acknowledgement could reset the running client"
            );
        })
        .await;
        server.abort();
        outcome.expect("pregame return did not receive its ordered bootstrap");
    });
}

#[test]
fn draft_full_bootstrap_precedes_a_concurrent_full_action() {
    run_async_test(|| async {
        use crate::draft_socket_authority_tests::start_draft_match_with_deck;
        use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
        use crate::issue_4548_full_create_tests::{
            recv_server_message, send_test_message, spawn_full_mode_server_with_db,
        };
        use engine::types::actions::{GameAction, MulliganChoice};
        use engine::types::format::FormatConfig;
        use server_core::protocol::{ClientMessage, DeckChoice, DeckData};
        use std::time::Duration;

        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let names = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let deck = crate::resolve_deck(&app.db, &names).unwrap();
            let (draft_code, draft_token, key, _) = start_draft_match_with_deck(
                &app,
                deck,
                Some(DeckChoice::DeckList(Box::new(names))),
            )
            .await;
            let opponent_token = {
                let mut session = crate::lock_session(&app.sessions, &key.game_code)
                    .await
                    .unwrap();
                session.state.format_config = FormatConfig::freeform();
                session.start_game(&app.db).unwrap();
                session.player_tokens[1].clone()
            };
            let mut draft_player = connect_and_hello(url.clone()).await;
            let reconnect = ClientMessage::ReconnectDraft {
                draft_code,
                player_token: draft_token,
            };
            send_test_message(&mut draft_player, &reconnect, false).await;
            while !matches!(
                recv_server_message(&mut draft_player).await,
                ServerMessage::DraftStateUpdate { .. }
            ) {}
            let mut opponent = connect_and_hello(url).await;
            send_test_message(
                &mut opponent,
                &ClientMessage::Reconnect {
                    game_code: key.game_code.clone(),
                    player_token: opponent_token,
                    full_key: key.clone(),
                },
                false,
            )
            .await;
            loop {
                if let ServerMessage::GameStarted { legal_actions, .. } =
                    recv_server_message(&mut opponent).await
                {
                    assert!(
                        legal_actions.contains(&GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        }),
                        "the opponent must have a real mulligan action before the race"
                    );
                    break;
                }
            }
            let gate = app.context.socket_hooks.arm_cutover();
            send_test_message(&mut draft_player, &reconnect, false).await;
            gate.entered.notified().await;
            send_test_message(
                &mut opponent,
                &ClientMessage::Action {
                    action: GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    },
                },
                false,
            )
            .await;
            let action_revision = loop {
                match recv_server_message(&mut opponent).await {
                    ServerMessage::StateUpdate { state_revision, .. } => break state_revision,
                    ServerMessage::ActionRejected { rejection } => panic!("action: {rejection:?}"),
                    _ => {}
                }
            };
            loop {
                match recv_server_message(&mut draft_player).await {
                    ServerMessage::DraftMatchStart { full_key, .. } => {
                        assert_eq!(full_key, key);
                        break;
                    }
                    ServerMessage::StateUpdate { .. } => panic!("update overtook draft binding"),
                    _ => {}
                }
            }
            let bootstrap_revision = loop {
                match recv_server_message(&mut draft_player).await {
                    ServerMessage::GameStarted { state_revision, .. } => break state_revision,
                    ServerMessage::StateUpdate { .. } => {
                        panic!("update overtook draft GameStarted")
                    }
                    _ => {}
                }
            };
            loop {
                if let ServerMessage::StateUpdate { state_revision, .. } =
                    recv_server_message(&mut draft_player).await
                {
                    assert_eq!(state_revision, action_revision);
                    assert!(state_revision > bootstrap_revision);
                    break;
                }
            }
            gate.release.notify_one();
        })
        .await;
        server.abort();
        outcome.expect("draft attachment or concurrent Full action stalled");
    });
}
