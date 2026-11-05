//! A spectator binding must retire the prior route in the same registry cutover.

use std::collections::HashMap;
use std::time::Duration;

use engine::types::game_state::GameState;
use server_core::protocol::{DeckChoice, DeckData, ServerMessage};
use server_core::FullSessionKey;
use tokio::sync::{mpsc, oneshot};

use crate::draft_socket_authority_tests::start_draft_match_with_deck;
use crate::game_submission_tests::basic_land_test_database;
use crate::issue_4548_full_create_tests::spawn_full_mode_server_with_db;
use crate::socket_test_hooks::run_async_test;

#[test]
fn full_spectator_switch_cannot_publish_a_prior_game_update_after_the_new_bootstrap() {
    run_async_test(|| async {
        let (_url, server, _directory, app) =
            spawn_full_mode_server_with_db(basic_land_test_database()).await;
        let outcome = tokio::time::timeout(Duration::from_secs(5), async {
            let names = DeckData {
                main_deck: vec!["Network Test Land".to_string(); 60],
                ..Default::default()
            };
            let deck = crate::resolve_deck(&app.db, &names).unwrap();
            let (_, _, next_key, _) = start_draft_match_with_deck(
                &app,
                deck,
                Some(DeckChoice::DeckList(Box::new(names))),
            )
            .await;
            let bootstrap = {
                let mut session = crate::lock_session(&app.sessions, &next_key.game_code)
                    .await
                    .unwrap();
                session.state.format_config = engine::types::format::FormatConfig::freeform();
                session.start_game(&app.db).unwrap();
                crate::build_spectator_game_started_message(&session).unwrap()
            };
            let prior_key = FullSessionKey {
                game_code: "OLD001".to_string(),
                generation: 1,
            };
            let (sender, mut messages) = crate::socket_mailbox::channel();
            let mut registry = app.game_spectators.lock().await;
            registry.insert(prior_key.game_code.clone(), vec![sender.clone()]);

            let (switching, queued_switch) = oneshot::channel();
            let switch = tokio::spawn({
                let spectators = app.game_spectators.clone();
                let next_code = next_key.game_code.clone();
                let prior_code = prior_key.game_code.clone();
                let sender = sender.clone();
                async move {
                    switching.send(()).unwrap();
                    crate::switch_game_spectator_slot(
                        &spectators,
                        Some(&prior_code),
                        &next_code,
                        &sender,
                        Some(bootstrap),
                    )
                    .await
                    .unwrap();
                }
            });
            // On this current-thread runtime, the child runs through its pending
            // registry acquisition before the signalled parent can resume.
            queued_switch.await.unwrap();
            let (broadcasting, queued_broadcast) = oneshot::channel();
            let broadcast = tokio::spawn({
                let spectators = app.game_spectators.clone();
                let connections = std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
                let executor = app.context.game_executor.clone();
                async move {
                    broadcasting.send(()).unwrap();
                    // Exercise the real spectator StateUpdate fan-out, queued
                    // behind the same registry mutex as the destination switch.
                    crate::broadcast_takeback_approved(
                        &executor,
                        &connections,
                        &spectators,
                        prior_key,
                        2,
                        17,
                        (
                            GameState::new_two_player(42),
                            Vec::new(),
                            false,
                            HashMap::new(),
                            HashMap::new(),
                        ),
                        None,
                        Vec::new(),
                    )
                    .await;
                }
            });
            queued_broadcast.await.unwrap();
            drop(registry);
            switch.await.unwrap();
            broadcast.await.unwrap();

            match messages.recv().await.unwrap() {
                ServerMessage::GameStarted { full_key, .. } => {
                    assert_eq!(full_key.as_ref(), Some(&next_key));
                }
                other => panic!("destination bootstrap was not first: {other:?}"),
            }
            assert!(
                matches!(messages.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                "a prior-game StateUpdate was published after the destination bootstrap"
            );
            drop(sender);
        })
        .await;
        server.abort();
        outcome.expect("spectator switch and queued broadcaster did not complete");
    });
}
