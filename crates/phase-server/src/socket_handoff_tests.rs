//! A draft capability can outlive a different Full-game attachment on one socket.

use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::draft_socket_authority_tests::start_draft_match_with_deck;
use crate::game_submission_tests::{basic_land_test_database, connect_and_hello};
use crate::issue_4548_full_create_tests::{
    recv_server_message, send_test_message, spawn_full_mode_server_with_db,
};
use crate::socket_test_hooks::{room_request, run_async_test};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::player::PlayerId;
use server_core::protocol::{ClientMessage, DeckChoice, DeckData, ServerMessage};

#[derive(Clone, Copy)]
enum HandoffScenario {
    Vacant,
    ClosesDuringRetirement,
    EndsDuringRetirement,
    TerminalCommitPending,
    InterveningCloses,
    InterveningLive,
    ValidationFails,
    ValidationFailsWithOwner,
}

#[test]
fn draft_handoff_retires_the_prior_route_before_recapturing_its_target() {
    assert_draft_handoff(HandoffScenario::Vacant);
}

#[test]
fn stale_target_close_during_draft_handoff_cannot_disconnect_the_replacement() {
    assert_draft_handoff(HandoffScenario::ClosesDuringRetirement);
}

#[test]
fn terminal_target_during_draft_handoff_is_refused_without_an_active_binding() {
    assert_draft_handoff(HandoffScenario::EndsDuringRetirement);
}

#[test]
fn terminal_target_pending_commit_is_not_published_as_active() {
    assert_draft_handoff(HandoffScenario::TerminalCommitPending);
}

#[test]
fn intervening_full_reconnect_close_cannot_leave_handoff_disconnected() {
    assert_draft_handoff(HandoffScenario::InterveningCloses);
}

#[test]
fn intervening_live_full_owner_is_not_replaced_by_an_older_handoff() {
    assert_draft_handoff(HandoffScenario::InterveningLive);
}

#[test]
fn draft_handoff_validation_error_cleans_up_provisional_presence() {
    assert_draft_handoff(HandoffScenario::ValidationFails);
}

#[test]
fn draft_handoff_validation_error_preserves_an_intervening_owner() {
    assert_draft_handoff(HandoffScenario::ValidationFailsWithOwner);
}

fn assert_draft_handoff(scenario: HandoffScenario) {
    run_async_test(move || async move {
        let (url, server, directory, app) =
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
            let (target_token, mut target_host_token) = {
                let mut session = crate::lock_session(&app.sessions, &target_key.game_code)
                    .await
                    .unwrap();
                session.state.format_config = engine::types::format::FormatConfig::freeform();
                session.start_game(&app.db).unwrap();
                (
                    session.player_tokens[1].clone(),
                    Some(session.player_tokens[0].clone()),
                )
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
            let mut target_player = connect_and_hello(url.clone()).await;
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
            let mut stale_target = match scenario {
                HandoffScenario::Vacant
                | HandoffScenario::EndsDuringRetirement
                | HandoffScenario::TerminalCommitPending
                | HandoffScenario::InterveningCloses
                | HandoffScenario::InterveningLive
                | HandoffScenario::ValidationFails
                | HandoffScenario::ValidationFailsWithOwner => None,
                HandoffScenario::ClosesDuringRetirement => {
                    let mut stale = connect_and_hello(url.clone()).await;
                    send_test_message(
                        &mut stale,
                        &ClientMessage::Reconnect {
                            game_code: target_key.game_code.clone(),
                            player_token: target_host_token.take().unwrap(),
                            full_key: target_key.clone(),
                        },
                        false,
                    )
                    .await;
                    loop {
                        if matches!(
                            recv_server_message(&mut stale).await,
                            ServerMessage::GameStarted { .. }
                        ) {
                            break;
                        }
                    }
                    Some(stale)
                }
            };
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
            if let Some(stale) = stale_target.as_mut() {
                let connected = app.player_count.load(Ordering::Acquire);
                stale.close(None).await.unwrap();
                // The transport counter is decremented after ordered session
                // cleanup, so this observes a completed stale-close attempt.
                while app.player_count.load(Ordering::Acquire) == connected {
                    tokio::task::yield_now().await;
                }
                let target = crate::lock_session(&app.sessions, &target_key.game_code)
                    .await
                    .unwrap();
                assert!(
                    target.connected[0],
                    "stale target close disconnected an already admitted replacement"
                );
                drop(target);
                assert!(
                    !app.sessions.lock().await.reconnect.is_disconnected(
                        &target_key.game_code,
                        engine::types::player::PlayerId(0),
                    ),
                    "stale target close created a forfeit record during the handoff"
                );
            }
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
            let mut intervening = if matches!(
                scenario,
                HandoffScenario::InterveningCloses
                    | HandoffScenario::InterveningLive
                    | HandoffScenario::ValidationFailsWithOwner
            ) {
                let mut peer = connect_and_hello(url).await;
                send_test_message(
                    &mut peer,
                    &ClientMessage::Reconnect {
                        game_code: target_key.game_code.clone(),
                        player_token: target_host_token.take().unwrap(),
                        full_key: target_key.clone(),
                    },
                    false,
                )
                .await;
                loop {
                    if matches!(
                        recv_server_message(&mut peer).await,
                        ServerMessage::GameStarted { .. }
                    ) {
                        break;
                    }
                }
                if matches!(scenario, HandoffScenario::InterveningCloses) {
                    let connected = app.player_count.load(Ordering::Acquire);
                    peer.close(None).await.unwrap();
                    while app.player_count.load(Ordering::Acquire) == connected {
                        tokio::task::yield_now().await;
                    }
                    assert!(
                        !crate::lock_session(&app.sessions, &target_key.game_code)
                            .await
                            .unwrap()
                            .connected[0],
                        "the intervening owner must really finish disconnecting"
                    );
                    assert!(app
                        .sessions
                        .lock()
                        .await
                        .reconnect
                        .is_disconnected(&target_key.game_code, PlayerId(0),));
                }
                Some(peer)
            } else {
                None
            };
            if matches!(
                scenario,
                HandoffScenario::EndsDuringRetirement | HandoffScenario::TerminalCommitPending
            ) {
                let terminal_gate = matches!(scenario, HandoffScenario::TerminalCommitPending)
                    .then(|| app.context.socket_hooks.arm_terminal_prepare());
                send_test_message(&mut target_player, &ClientMessage::Concede, false).await;
                if let Some(pending) = terminal_gate.as_ref() {
                    pending.entered.notified().await;
                    assert_eq!(
                        app.game_db
                            .load_active_full_key(&target_key.game_code)
                            .unwrap()
                            .as_ref(),
                        Some(&target_key),
                        "Concede must be paused before its terminal transaction"
                    );
                } else {
                    loop {
                        match recv_server_message(&mut target_player).await {
                            ServerMessage::TerminalResult { delivery: Some(_) } => break,
                            ServerMessage::ActionFailed { message } => panic!("concede: {message}"),
                            ServerMessage::ActionRejected { rejection } => {
                                panic!("concede: {rejection:?}")
                            }
                            _ => {}
                        }
                    }
                    assert!(
                        app.game_db
                            .load_active_full_key(&target_key.game_code)
                            .unwrap()
                            .is_none(),
                        "the real concede must durably retire the target before cutover"
                    );
                }
                gate.release.notify_one();
                drop(prior_lease);
                let refusal = async {
                    let mut refused = false;
                    loop {
                        match recv_server_message(&mut moving).await {
                            ServerMessage::DraftActionRejected { .. } => refused = true,
                            ServerMessage::DraftStateUpdate { .. } => {
                                assert!(refused, "terminal handoff must report its refusal");
                                break;
                            }
                            ServerMessage::DraftMatchStart { .. }
                            | ServerMessage::GameStarted { .. }
                            | ServerMessage::StateUpdate { .. } => {
                                panic!("terminal target was published as an active Full binding");
                            }
                            _ => {}
                        }
                    }
                };
                tokio::pin!(refusal);
                let already_refused = if let Some(pending) = terminal_gate.as_ref() {
                    // A retained publication lease may defer the refusal until
                    // terminal commit. Neither lease model may publish an active
                    // binding while the real Concede transaction is paused.
                    let completed = tokio::time::timeout(Duration::from_millis(30), &mut refusal)
                        .await
                        .is_ok();
                    pending.release.notify_one();
                    completed
                } else {
                    false
                };
                if !already_refused {
                    refusal.await;
                }
                if terminal_gate.is_some() {
                    loop {
                        match recv_server_message(&mut target_player).await {
                            ServerMessage::TerminalResult { delivery: Some(_) } => break,
                            ServerMessage::ActionFailed { message } => panic!("concede: {message}"),
                            ServerMessage::ActionRejected { rejection } => {
                                panic!("concede: {rejection:?}")
                            }
                            _ => {}
                        }
                    }
                }
                return;
            }
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
            let mut validation_database = None;
            if matches!(
                scenario,
                HandoffScenario::ValidationFails | HandoffScenario::ValidationFailsWithOwner
            ) {
                // Break the actual SELECT on this server's own temporary DB,
                // rather than replacing the validation result with a mock.
                let database =
                    rusqlite::Connection::open(directory.path().join("games.db")).unwrap();
                database
                    .execute_batch("ALTER TABLE game_sessions RENAME TO held_game_sessions")
                    .unwrap();
                assert!(app
                    .game_db
                    .load_active_full_key(&target_key.game_code)
                    .is_err());
                validation_database = Some(database);
            }
            gate.release.notify_one();
            drop(prior_lease);
            if matches!(
                scenario,
                HandoffScenario::InterveningLive
                    | HandoffScenario::ValidationFails
                    | HandoffScenario::ValidationFailsWithOwner
            ) {
                loop {
                    match recv_server_message(&mut moving).await {
                        ServerMessage::DraftActionRejected { .. } => break,
                        ServerMessage::DraftMatchStart { .. }
                        | ServerMessage::GameStarted { .. }
                        | ServerMessage::StateUpdate { .. } => {
                            panic!("a failed or superseded handoff published an active binding")
                        }
                        _ => {}
                    }
                }
                if matches!(scenario, HandoffScenario::ValidationFails) {
                    let target = crate::lock_session(&app.sessions, &target_key.game_code)
                        .await
                        .unwrap();
                    assert!(
                        !target.connected[0],
                        "query failure left unpublished provisional presence connected"
                    );
                    drop(target);
                    assert!(app
                        .sessions
                        .lock()
                        .await
                        .reconnect
                        .is_disconnected(&target_key.game_code, PlayerId(0),));
                    assert!(!app
                        .connections
                        .lock()
                        .await
                        .get(&target_key.game_code)
                        .is_some_and(|players| players.contains_key(&PlayerId(0))));
                    loop {
                        if let ServerMessage::OpponentDisconnected {
                            full_key,
                            player: Some(PlayerId(0)),
                            ..
                        } = recv_server_message(&mut target_player).await
                        {
                            assert_eq!(full_key.as_ref(), Some(&target_key));
                            break;
                        }
                    }
                    validation_database
                        .take()
                        .unwrap()
                        .execute_batch("ALTER TABLE held_game_sessions RENAME TO game_sessions")
                        .unwrap();
                    // A restored backend must let this same healthy draft
                    // connection reclaim the coherently disconnected seat.
                    send_test_message(&mut moving, &reconnect, false).await;
                } else {
                    if let Some(database) = validation_database.take() {
                        database
                            .execute_batch("ALTER TABLE held_game_sessions RENAME TO game_sessions")
                            .unwrap();
                    }
                    let peer = intervening.as_mut().unwrap();
                    send_test_message(
                        peer,
                        &ClientMessage::Action {
                            action: GameAction::MulliganDecision {
                                choice: MulliganChoice::Keep,
                            },
                        },
                        false,
                    )
                    .await;
                    loop {
                        match recv_server_message(peer).await {
                            ServerMessage::StateUpdate {
                                state_revision,
                                state,
                                ..
                            } if state_revision > target_revision => {
                                assert_eq!(state_revision, target_revision + 1);
                                assert!(matches!(
                                    state.waiting_for,
                                    engine::types::game_state::WaitingFor::Priority { .. }
                                ));
                                break;
                            }
                            ServerMessage::ActionFailed { message } => panic!("owner: {message}"),
                            ServerMessage::ActionRejected { rejection } => {
                                panic!("owner: {rejection:?}")
                            }
                            _ => {}
                        }
                    }
                    assert!(
                        crate::lock_session(&app.sessions, &target_key.game_code)
                            .await
                            .unwrap()
                            .connected[0]
                    );
                    assert!(!app
                        .sessions
                        .lock()
                        .await
                        .reconnect
                        .is_disconnected(&target_key.game_code, PlayerId(0),));
                    return;
                }
            }
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
            if matches!(
                scenario,
                HandoffScenario::ClosesDuringRetirement
                    | HandoffScenario::InterveningCloses
                    | HandoffScenario::ValidationFails
            ) {
                let target = crate::lock_session(&app.sessions, &target_key.game_code)
                    .await
                    .unwrap();
                assert!(
                    target.connected[0],
                    "the installed replacement is disconnected"
                );
                drop(target);
                assert!(!app
                    .sessions
                    .lock()
                    .await
                    .reconnect
                    .is_disconnected(&target_key.game_code, engine::types::player::PlayerId(0),));
                if matches!(
                    scenario,
                    HandoffScenario::InterveningCloses | HandoffScenario::ValidationFails
                ) {
                    loop {
                        if matches!(
                            recv_server_message(&mut target_player).await,
                            ServerMessage::OpponentReconnected {
                                player: Some(PlayerId(0)),
                                ..
                            }
                        ) {
                            break;
                        }
                    }
                }
                send_test_message(
                    &mut moving,
                    &ClientMessage::Action {
                        action: GameAction::MulliganDecision {
                            choice: MulliganChoice::Keep,
                        },
                    },
                    false,
                )
                .await;
                loop {
                    match recv_server_message(&mut moving).await {
                        ServerMessage::StateUpdate {
                            state_revision,
                            state,
                            ..
                        } => {
                            assert_eq!(state_revision, target_revision + 1);
                            assert!(matches!(
                                state.waiting_for,
                                engine::types::game_state::WaitingFor::Priority { .. }
                            ));
                            break;
                        }
                        ServerMessage::ActionFailed { message } => panic!("replacement: {message}"),
                        ServerMessage::ActionRejected { rejection } => {
                            panic!("replacement: {rejection:?}")
                        }
                        _ => {}
                    }
                }
            }
        })
        .await;
        server.abort();
        outcome.expect("draft-to-Full handoff did not complete");
    });
}
