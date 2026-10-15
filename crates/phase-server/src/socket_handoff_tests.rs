//! A draft capability can outlive a different Full-game attachment on one socket.

use std::time::Duration;

use crate::draft_socket_authority_tests::start_draft_match_with_deck;
use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
use crate::issue_4548_full_create_tests::{
    recv_server_message, send_test_message, spawn_full_mode_server_with_db,
};
use crate::socket_test_hooks::{room_request, run_async_test};
use engine::types::actions::{GameAction, MulliganChoice};
use server_core::protocol::{ClientMessage, DeckChoice, DeckData, ServerMessage};

#[test]
fn draft_handoff_retires_the_prior_route_before_recapturing_its_target() {
    run_async_test(|| async {
        let (url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            let names = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let deck = crate::resolve_deck(&app.db, &names).unwrap();
            let (draft_code, draft_token, target_key, _) = start_draft_match_with_deck(
                &app,
                deck,
                Some(DeckChoice::DeckList(Box::new(names.clone()))),
            )
            .await;
            let target_token = {
                let mut session = crate::lock_session(&app.sessions, &target_key.game_code)
                    .await
                    .unwrap();
                session.state.format_config = engine::types::format::FormatConfig::freeform();
                session.start_game(&app.db).unwrap();
                session.player_tokens[1].clone()
            };
            // Model the interval before a pairing's Full route is published.
            let match_id = {
                let mut drafts = app.draft_sessions.lock().await;
                let draft = drafts.sessions.get_mut(&draft_code).unwrap();
                let id = draft.active_matches.keys().next().unwrap().clone();
                draft.active_matches.remove(&id);
                id
            };
            let reconnect = ClientMessage::ReconnectDraft {
                draft_code: draft_code.clone(),
                player_token: draft_token,
            };
            let mut moving = connect_and_hello(url.clone()).await;
            send_test_message(&mut moving, &reconnect, false).await;
            loop {
                match recv_server_message(&mut moving).await {
                    ServerMessage::DraftStateUpdate { .. } => break,
                    ServerMessage::DraftMatchStart { .. } => {
                        panic!("the target is not published yet")
                    }
                    ServerMessage::DraftActionRejected { reason } => panic!("draft: {reason}"),
                    _ => {}
                }
            }
            let mut prior_host = connect_and_hello(url.clone()).await;
            send_test_message(
                &mut prior_host,
                &room_request(names.clone(), 2, None),
                false,
            )
            .await;
            let prior_code = loop {
                match recv_server_message(&mut prior_host).await {
                    ServerMessage::GameCreated { game_code, .. } => break game_code,
                    ServerMessage::Error { message, .. } => panic!("prior create: {message}"),
                    _ => {}
                }
            };
            send_test_message(
                &mut moving,
                &ClientMessage::JoinGame {
                    game_code: prior_code.clone(),
                    deck: names,
                },
                false,
            )
            .await;
            loop {
                match recv_server_message(&mut moving).await {
                    ServerMessage::GameStarted {
                        full_key: Some(key),
                        ..
                    } => {
                        assert_eq!(key.game_code, prior_code);
                        break;
                    }
                    ServerMessage::Error { message, .. } => panic!("prior join: {message}"),
                    _ => {}
                }
            }
            let mut target_player = connect_and_hello(url).await;
            send_test_message(
                &mut target_player,
                &ClientMessage::Reconnect {
                    game_code: target_key.game_code.clone(),
                    player_token: target_token,
                    full_key: target_key.clone(),
                },
                false,
            )
            .await;
            loop {
                if let ServerMessage::GameStarted { legal_actions, .. } =
                    recv_server_message(&mut target_player).await
                {
                    assert!(legal_actions.contains(&GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    }));
                    break;
                }
            }
            app.draft_sessions
                .lock()
                .await
                .sessions
                .get_mut(&draft_code)
                .unwrap()
                .active_matches
                .insert(match_id, target_key.game_code.clone());
            let prior_lease = crate::lock_session(&app.sessions, &prior_code)
                .await
                .unwrap();
            let gate = app.context.socket_hooks.arm_cutover();
            send_test_message(&mut moving, &reconnect, false).await;
            gate.entered.notified().await;
            assert!(
                tokio::time::timeout(Duration::from_millis(250), async {
                    loop {
                        match recv_server_message(&mut moving).await {
                            ServerMessage::DraftMatchStart { .. }
                            | ServerMessage::GameStarted { .. }
                            | ServerMessage::StateUpdate { .. } => break,
                            _ => {}
                        }
                    }
                })
                .await
                .is_err(),
                "new-game traffic overtook retirement of the prior route"
            );
            send_test_message(
                &mut target_player,
                &ClientMessage::Action {
                    action: GameAction::MulliganDecision {
                        choice: MulliganChoice::Keep,
                    },
                },
                false,
            )
            .await;
            let target_revision = loop {
                match recv_server_message(&mut target_player).await {
                    ServerMessage::StateUpdate { state_revision, .. } => break state_revision,
                    ServerMessage::ActionRejected { rejection } => panic!("target: {rejection:?}"),
                    _ => {}
                }
            };
            gate.release.notify_one();
            drop(prior_lease);
            loop {
                match recv_server_message(&mut moving).await {
                    ServerMessage::DraftMatchStart { full_key, .. } => {
                        assert_eq!(full_key, target_key);
                        break;
                    }
                    ServerMessage::StateUpdate { .. } => panic!("unbound target update escaped"),
                    _ => {}
                }
            }
            loop {
                match recv_server_message(&mut moving).await {
                    ServerMessage::GameStarted {
                        state_revision,
                        full_key,
                        ..
                    } => {
                        assert_eq!(full_key.as_ref(), Some(&target_key));
                        assert_eq!(state_revision, target_revision);
                        break;
                    }
                    ServerMessage::StateUpdate { .. } => {
                        panic!("update preceded the recaptured target")
                    }
                    _ => {}
                }
            }
        })
        .await;
        server.abort();
        outcome.expect("draft-to-Full handoff did not complete");
    });
}
