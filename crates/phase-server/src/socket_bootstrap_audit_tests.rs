//! Native bootstrap ordering for draft-only and spectator sender registries.

use std::time::Duration;

use crate::draft_socket_authority_tests::{start_draft_match_with_deck, test_draft_config};
use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
use crate::issue_4548_full_create_tests::{
    recv_server_message, send_test_message, spawn_full_mode_server, spawn_full_mode_server_with_db,
    spawn_server, tst_draft_pools,
};
use crate::socket_test_hooks::run_async_test;
use draft_core::types::{DraftAction, PodPolicy};
use engine::types::actions::{GameAction, MulliganChoice};
use server_core::protocol::{ClientMessage, DeckChoice, DeckData, ServerMessage};

type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn draft_host(app: &crate::AppState, url: String) -> (String, ClientSocket) {
    let mut config = test_draft_config();
    config.pod_policy = PodPolicy::Casual;
    let (code, token, _) = app
        .draft_sessions
        .lock()
        .await
        .create_draft(config, "host".to_string());
    let mut host = connect_and_hello(url).await;
    send_test_message(
        &mut host,
        &ClientMessage::ReconnectDraft {
            draft_code: code.clone(),
            player_token: token,
        },
        false,
    )
    .await;
    while !matches!(
        recv_server_message(&mut host).await,
        ServerMessage::DraftStateUpdate { .. }
    ) {}
    (code, host)
}

async fn replace_bot(host: &mut ClientSocket, code: &str) {
    send_test_message(
        host,
        &ClientMessage::DraftAction {
            draft_code: code.to_string(),
            action: DraftAction::ReplaceSeatWithBot {
                seat: 7,
                name: Some("after-bootstrap".to_string()),
            },
        },
        false,
    )
    .await;
    loop {
        match recv_server_message(host).await {
            ServerMessage::DraftStateUpdate { view } => {
                assert_eq!(view.seats[7].display_name, "after-bootstrap");
                assert!(view.seats[7].is_bot);
                break;
            }
            ServerMessage::DraftActionRejected { reason } => panic!("host mutation: {reason}"),
            _ => {}
        }
    }
}

#[test]
fn draft_creator_bootstrap_does_not_wait_for_unrelated_lobby_work() {
    run_async_test(|| async {
        let (url, server, _directory, app) = spawn_server(
            crate::ServerMode::Full,
            tst_draft_pools(),
            std::sync::Arc::new(crate::CardDatabase::default()),
            crate::ServerContext::default(),
        )
        .await;
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let mut host = connect_and_hello(url).await;
            let held = app.lobby.lock().await;
            let config = test_draft_config();
            send_test_message(
                &mut host,
                &ClientMessage::CreateDraftWithSettings {
                    display_name: "host".to_string(),
                    source: None,
                    set_codes: Some(vec!["TST".to_string()]),
                    kind: config.kind,
                    public: false,
                    password: None,
                    timer_seconds: None,
                    tournament_format: config.tournament_format,
                    pod_policy: PodPolicy::Casual,
                    pod_size: config.pod_size,
                },
                false,
            )
            .await;
            let code = loop {
                if let Some(code) = app.draft_sessions.lock().await.sessions.keys().next() {
                    break code.clone();
                }
                tokio::task::yield_now().await;
            };
            assert!(matches!(
                recv_server_message(&mut host).await,
                ServerMessage::DraftCreated { draft_code, seat_index: 0, .. } if draft_code == code
            ));
            drop(held);
        })
        .await;
        server.abort();
        outcome.expect("a published draft creator had no bootstrap while the lobby was busy");
    });
}

#[test]
fn draft_join_bootstrap_precedes_a_concurrent_host_mutation() {
    run_async_test(|| async {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let (code, mut host) = draft_host(&app, url.clone()).await;
            let gate = app.context.socket_hooks.arm_cutover();
            let mut guest = connect_and_hello(url).await;
            send_test_message(
                &mut guest,
                &ClientMessage::JoinDraftWithPassword {
                    draft_code: code.clone(),
                    display_name: "guest".to_string(),
                    password: None,
                },
                false,
            )
            .await;
            gate.entered.notified().await;
            replace_bot(&mut host, &code).await;
            loop {
                match recv_server_message(&mut guest).await {
                    ServerMessage::DraftJoined { view, .. } => {
                        assert_ne!(view.seats[7].display_name, "after-bootstrap");
                        break;
                    }
                    ServerMessage::DraftStateUpdate { .. } => {
                        panic!("a draft update overtook the joining seat's binding")
                    }
                    ServerMessage::DraftActionRejected { reason } => panic!("join: {reason}"),
                    _ => {}
                }
            }
            loop {
                if let ServerMessage::DraftStateUpdate { view } =
                    recv_server_message(&mut guest).await
                {
                    assert_eq!(view.seats[7].display_name, "after-bootstrap");
                    break;
                }
            }
            gate.release.notify_one();
        })
        .await;
        server.abort();
        outcome.expect("draft join or concurrent host mutation stalled");
    });
}

#[test]
fn draft_spectator_bootstrap_precedes_a_concurrent_host_mutation() {
    run_async_test(|| async {
        let (url, server, _directory, app) = spawn_full_mode_server().await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let (code, mut host) = draft_host(&app, url.clone()).await;
            let gate = app.context.socket_hooks.arm_cutover();
            let mut spectator = connect_and_hello(url).await;
            send_test_message(
                &mut spectator,
                &ClientMessage::SpectateDraft {
                    draft_code: code.clone(),
                },
                false,
            )
            .await;
            gate.entered.notified().await;
            replace_bot(&mut host, &code).await;
            let before = loop {
                if let ServerMessage::DraftSpectatorView { view } =
                    recv_server_message(&mut spectator).await
                {
                    break view;
                }
            };
            assert_ne!(
                before.seats[7].display_name, "after-bootstrap",
                "a newer spectator view overtook the captured bootstrap"
            );
            loop {
                if let ServerMessage::DraftSpectatorView { view } =
                    recv_server_message(&mut spectator).await
                {
                    assert_eq!(view.seats[7].display_name, "after-bootstrap");
                    break;
                }
            }
            gate.release.notify_one();
        })
        .await;
        server.abort();
        outcome.expect("draft spectator bootstrap or host mutation stalled");
    });
}

#[test]
fn full_spectator_bootstrap_precedes_a_concurrent_action() {
    run_async_test(|| async {
        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let names = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let deck = crate::resolve_deck(&app.db, &names).unwrap();
            let (_, _, key, _) = start_draft_match_with_deck(
                &app,
                deck,
                Some(DeckChoice::DeckList(Box::new(names))),
            )
            .await;
            let token = {
                let mut session = crate::lock_session(&app.sessions, &key.game_code)
                    .await
                    .unwrap();
                session.state.format_config = engine::types::format::FormatConfig::freeform();
                session.start_game(&app.db).unwrap();
                session.player_tokens[1].clone()
            };
            let mut player = connect_and_hello(url.clone()).await;
            send_test_message(
                &mut player,
                &ClientMessage::Reconnect {
                    game_code: key.game_code.clone(),
                    player_token: token,
                    full_key: key.clone(),
                },
                false,
            )
            .await;
            loop {
                if let ServerMessage::GameStarted { legal_actions, .. } =
                    recv_server_message(&mut player).await
                {
                    assert!(
                        legal_actions.contains(&GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        }),
                        "the player must have a real mulligan action before the race"
                    );
                    break;
                }
            }
            let gate = app.context.socket_hooks.arm_cutover();
            let mut spectator = connect_and_hello(url).await;
            send_test_message(
                &mut spectator,
                &ClientMessage::SpectatorJoin {
                    game_code: key.game_code.clone(),
                },
                false,
            )
            .await;
            gate.entered.notified().await;
            send_test_message(
                &mut player,
                &ClientMessage::Action {
                    action: GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    },
                },
                false,
            )
            .await;
            let action_revision = loop {
                match recv_server_message(&mut player).await {
                    ServerMessage::StateUpdate { state_revision, .. } => break state_revision,
                    ServerMessage::ActionRejected { rejection } => panic!("action: {rejection:?}"),
                    _ => {}
                }
            };
            let bootstrap_revision = loop {
                match recv_server_message(&mut spectator).await {
                    ServerMessage::GameStarted {
                        state_revision,
                        your_player,
                        ..
                    } => {
                        assert_eq!(your_player, crate::SPECTATOR_PLAYER_ID);
                        break state_revision;
                    }
                    ServerMessage::StateUpdate { .. } => {
                        panic!("update overtook spectator binding")
                    }
                    _ => {}
                }
            };
            loop {
                if let ServerMessage::StateUpdate { state_revision, .. } =
                    recv_server_message(&mut spectator).await
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
        outcome.expect("Full spectator bootstrap or concurrent action stalled");
    });
}
