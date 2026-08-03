//! Centralized native-shell validation for every [`crate::protocol::ClientMessage`]
//! variant before handler dispatch and broker projection clones.
//!
//! Individual handlers still run their guards for defense in depth; this layer
//! guarantees that every wire policy is declared in an exhaustive, wildcard-free
//! match, so a new `ClientMessage` variant cannot compile until it states one,
//! and broker-projected frames are bounded before `to_lobby_client_message` clones.
//!
//! Three such matches live here, one per policy axis:
//! [`guard_client_message_before_dispatch`] (payload bounding),
//! [`wire_rejection_message`] (which channel a rejection is answered on), and
//! [`guard_broker_projection_inbound`] (broker projection). A new variant must
//! declare a policy in all three.

use lobby_broker::inbound_guard::{
    guard_create_game_settings_inbound, guard_join_game_with_password_inbound,
    guard_lookup_join_target_inbound, CreateGameSettingsInbound, JoinGameWithPasswordInbound,
    LookupJoinTargetInbound,
};
use lobby_broker::validation::{
    validate_unregister_lobby_fields, validate_update_lobby_metadata_fields,
    UpdateLobbyMetadataFields,
};

use crate::ai_seats_wire_guard::guard_create_ai_seats;
use crate::client_hello_guard::guard_client_hello;
use crate::draft_action_payload_guard::guard_draft_action_payload;
use crate::draft_wire_guard::{
    guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
    guard_reconnect_draft,
};
use crate::emote_guard::guard_emote;
use crate::game_action_payload_guard::guard_game_action_payload;
use crate::game_reconnect_guard::guard_game_reconnect;
use crate::interaction_payload_guard::guard_interaction_submission_payload;
use crate::legacy_deck_guard::guard_legacy_deck;
use crate::legacy_join_guard::guard_legacy_join_game;
use crate::protocol::{ClientMessage, ServerMessage, ServerMode};
use crate::seat_mutation_wire_guard::guard_seat_mutation;
use crate::spectator_wire_guard::{guard_spectate_draft, guard_spectator_join};

/// Validate wire fields for any inbound `ClientMessage` before handler work.
///
/// `mode` is used for variants whose policy differs between Full and LobbyOnly
/// (currently none reject here — mode gating stays in `reject_if_disabled`).
pub fn guard_client_message_before_dispatch(
    msg: &ClientMessage,
    _mode: ServerMode,
) -> Result<(), String> {
    match msg {
        ClientMessage::ClientHello {
            client_version,
            build_commit,
            ..
        } => guard_client_hello(client_version, build_commit),
        ClientMessage::CreateGame { deck } => guard_legacy_deck(deck),
        ClientMessage::JoinGame { game_code, deck } => guard_legacy_join_game(game_code, deck),
        ClientMessage::Action { action } | ClientMessage::PreviewManaPayment { action, .. } => {
            guard_game_action_payload(action)
        }
        ClientMessage::Interaction { submission } => {
            guard_interaction_submission_payload(submission)
        }
        ClientMessage::Reconnect {
            game_code,
            player_token,
            full_key,
        } => {
            guard_game_reconnect(game_code, player_token)?;
            if full_key.game_code != *game_code || full_key.generation == 0 {
                return Err(
                    "reconnect full_key must match game_code and have a generation".to_string(),
                );
            }
            Ok(())
        }
        ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::Concede
        | ClientMessage::ConcedeMatch
        | ClientMessage::AbandonGame
        | ClientMessage::RequestTakeback
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback => Ok(()),
        ClientMessage::BootstrapTerminalDelivery { request } => {
            if request.key.game_code.is_empty()
                || request.player_token.is_empty()
                || request.request_id.is_empty()
            {
                return Err("terminal bootstrap fields must not be empty".to_string());
            }
            Ok(())
        }
        ClientMessage::ReadTerminalResult { credential } => {
            if credential.0.is_empty() {
                return Err("terminal credential must not be empty".to_string());
            }
            Ok(())
        }
        ClientMessage::AckTerminalDelivery {
            delivery_id,
            credential,
        } => {
            if delivery_id.0.is_empty() || credential.0.is_empty() {
                return Err("terminal acknowledgement fields must not be empty".to_string());
            }
            Ok(())
        }
        ClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            password,
            timer_seconds,
            player_count,
            ai_seats,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            ..
        } => {
            guard_create_game_settings_inbound(CreateGameSettingsInbound {
                deck,
                display_name,
                password: password.as_deref(),
                timer_seconds: *timer_seconds,
                player_count: *player_count,
                format_config: format_config.as_ref(),
                room_name: room_name.as_deref(),
                host_peer_id: host_peer_id.as_deref(),
                draft_metadata: draft_metadata.as_ref(),
            })?;
            guard_create_ai_seats(ai_seats, *player_count)
        }
        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code,
            deck,
            display_name,
            password: password.as_deref(),
            reservation_token: reservation_token.as_deref(),
        }),
        ClientMessage::LookupJoinTarget {
            game_code,
            password,
            display_name,
            release_reservation_token,
            ..
        } => guard_lookup_join_target_inbound(LookupJoinTargetInbound {
            game_code,
            password: password.as_deref(),
            display_name: display_name.as_deref(),
            release_reservation_token: release_reservation_token.as_deref(),
        }),
        ClientMessage::Emote { emote } => guard_emote(emote),
        ClientMessage::SpectatorJoin { game_code } => guard_spectator_join(game_code),
        ClientMessage::Ping { .. } => Ok(()),
        ClientMessage::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
            consumed_reservation_tokens,
        } => validate_update_lobby_metadata_fields(UpdateLobbyMetadataFields {
            game_code,
            current_players: *current_players,
            max_players: *max_players,
            consumed_reservation_tokens,
        }),
        ClientMessage::SeatMutate { mutation } => guard_seat_mutation(mutation),
        ClientMessage::UnregisterLobby { game_code } => validate_unregister_lobby_fields(game_code),
        ClientMessage::CreateDraftWithSettings {
            display_name,
            set_code,
            password,
            timer_seconds,
            pod_size,
            ..
        } => guard_create_draft_with_settings(
            display_name,
            set_code,
            password,
            *timer_seconds,
            *pod_size,
        ),
        ClientMessage::JoinDraftWithPassword {
            draft_code,
            display_name,
            password,
        } => guard_join_draft_with_password(draft_code, display_name, password),
        ClientMessage::DraftAction { draft_code, action } => {
            guard_draft_action(draft_code)?;
            guard_draft_action_payload(action)
        }
        ClientMessage::ReconnectDraft {
            draft_code,
            player_token,
        } => guard_reconnect_draft(draft_code, player_token),
        ClientMessage::SpectateDraft { draft_code } => guard_spectate_draft(draft_code),
    }
}

/// Answer a frame that [`guard_client_message_before_dispatch`] rejected, on
/// the channel that frame's variant declares.
///
/// Exhaustive by design, like the two sibling matches in this module: a new
/// variant must declare not only *which* bounds apply at the wire, but *how a
/// rejection is answered*. The native client disposes its adapter on ANY
/// `ServerMessage::Error`, so any variant whose wire bounds a routine,
/// non-hostile client can trip MUST answer on `ActionRejected`.
pub fn wire_rejection_message(msg: &ClientMessage, reason: String) -> ServerMessage {
    match msg {
        // An oversized interaction response is reachable without hostility:
        // `TextChoiceProjection::allow_arbitrary` accepts free-form text and
        // `MAX_INTERACTION_STRING_LEN` is 256, so a long paste is a rejected
        // decision, not a malformed frame. `ServerMessage::error` here would
        // end the match on a paste.
        ClientMessage::Interaction { .. } => ServerMessage::ActionRejected { reason },

        // Every other variant keeps today's behavior exactly: a bounds failure
        // on these is a malformed frame, not a rejected decision.
        ClientMessage::ClientHello { .. }
        | ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Action { .. }
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::AbandonGame
        | ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::CreateGameWithSettings { .. }
        | ClientMessage::JoinGameWithPassword { .. }
        | ClientMessage::LookupJoinTarget { .. }
        | ClientMessage::Concede
        | ClientMessage::ConcedeMatch
        | ClientMessage::BootstrapTerminalDelivery { .. }
        | ClientMessage::ReadTerminalResult { .. }
        | ClientMessage::AckTerminalDelivery { .. }
        | ClientMessage::Emote { .. }
        | ClientMessage::SpectatorJoin { .. }
        | ClientMessage::Ping { .. }
        | ClientMessage::UpdateLobbyMetadata { .. }
        | ClientMessage::SeatMutate { .. }
        | ClientMessage::UnregisterLobby { .. }
        | ClientMessage::CreateDraftWithSettings { .. }
        | ClientMessage::JoinDraftWithPassword { .. }
        | ClientMessage::DraftAction { .. }
        | ClientMessage::ReconnectDraft { .. }
        | ClientMessage::SpectateDraft { .. }
        | ClientMessage::RequestTakeback
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback => ServerMessage::error(reason),
    }
}

/// Validate broker-projected lobby frames without constructing `LobbyClientMessage`.
///
/// Used by `dispatch_broker` before `to_lobby_client_message` clones strings and
/// token vectors.
pub fn guard_broker_projection_inbound(msg: &ClientMessage) -> Result<(), String> {
    match msg {
        ClientMessage::ClientHello {
            client_version,
            build_commit,
            ..
        } => guard_client_hello(client_version, build_commit),
        ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::Ping { .. } => Ok(()),
        ClientMessage::CreateGameWithSettings {
            deck,
            display_name,
            password,
            timer_seconds,
            player_count,
            format_config,
            room_name,
            host_peer_id,
            draft_metadata,
            ..
        } => guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck,
            display_name,
            password: password.as_deref(),
            timer_seconds: *timer_seconds,
            player_count: *player_count,
            format_config: format_config.as_ref(),
            room_name: room_name.as_deref(),
            host_peer_id: host_peer_id.as_deref(),
            draft_metadata: draft_metadata.as_ref(),
        }),
        ClientMessage::JoinGameWithPassword {
            game_code,
            deck,
            display_name,
            password,
            reservation_token,
        } => guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code,
            deck,
            display_name,
            password: password.as_deref(),
            reservation_token: reservation_token.as_deref(),
        }),
        ClientMessage::LookupJoinTarget {
            game_code,
            password,
            display_name,
            release_reservation_token,
            ..
        } => guard_lookup_join_target_inbound(LookupJoinTargetInbound {
            game_code,
            password: password.as_deref(),
            display_name: display_name.as_deref(),
            release_reservation_token: release_reservation_token.as_deref(),
        }),
        ClientMessage::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
            consumed_reservation_tokens,
        } => validate_update_lobby_metadata_fields(UpdateLobbyMetadataFields {
            game_code,
            current_players: *current_players,
            max_players: *max_players,
            consumed_reservation_tokens,
        }),
        ClientMessage::UnregisterLobby { game_code } => validate_unregister_lobby_fields(game_code),
        ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Action { .. }
        | ClientMessage::Interaction { .. }
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::AbandonGame
        | ClientMessage::Concede
        | ClientMessage::ConcedeMatch
        | ClientMessage::BootstrapTerminalDelivery { .. }
        | ClientMessage::ReadTerminalResult { .. }
        | ClientMessage::AckTerminalDelivery { .. }
        | ClientMessage::Emote { .. }
        | ClientMessage::SpectatorJoin { .. }
        | ClientMessage::SeatMutate { .. }
        | ClientMessage::CreateDraftWithSettings { .. }
        | ClientMessage::JoinDraftWithPassword { .. }
        | ClientMessage::DraftAction { .. }
        | ClientMessage::ReconnectDraft { .. }
        | ClientMessage::SpectateDraft { .. }
        | ClientMessage::RequestTakeback
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_action_payload_guard::MAX_ACTION_LIST_LEN;
    use engine::types::ability::{TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef};
    use engine::types::game_state::ProductionOverride;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::interaction::{
        InteractionChoiceId, InteractionId, InteractionResponse, InteractionSubmission,
        MAX_INTERACTION_LIST_LEN,
    };
    use engine::types::mana::{
        ManaRestriction, ManaSourcePenalty, ManaSourceSelection, ManaType, TapsForManaSelection,
    };
    use engine::types::{GameAction, ObjectId};
    use lobby_broker::validation::MAX_CONSUMED_TOKENS;

    #[test]
    fn dispatch_guard_accepts_subscribe_lobby() {
        assert!(guard_client_message_before_dispatch(
            &ClientMessage::SubscribeLobby,
            ServerMode::Full
        )
        .is_ok());
    }

    #[test]
    fn dispatch_guard_rejects_oversized_emote() {
        let msg = ClientMessage::Emote {
            emote: "x".repeat(129),
        };
        let err = guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err();
        assert!(err.contains("emote"));
    }

    #[test]
    fn dispatch_guard_rejects_oversized_game_action_before_handler_work() {
        let msg = ClientMessage::Action {
            action: GameAction::ReorderHand {
                order: vec![ObjectId(1); MAX_ACTION_LIST_LEN + 1],
            },
        };

        let err = guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err();
        assert!(err.contains("ReorderHand.order"));
    }

    #[test]
    fn dispatch_guard_rejects_hostile_tap_land_restrictions_at_action_boundary() {
        let msg = ClientMessage::Action {
            action: GameAction::TapLandForMana {
                selection: ManaSourceSelection {
                    source: ObjectIncarnationRef::of(ObjectId(1), 1),
                    ability_index: None,
                    mana_type: ManaType::Green,
                    output: engine::types::mana::ManaSourceOutput::Concrete(ManaType::Green),
                    atomic_combination: None,
                    restrictions: vec![ManaRestriction::OnlyForAny(vec![
                        ManaRestriction::OnlyForSpell;
                        MAX_ACTION_LIST_LEN + 1
                    ])],
                    penalty: ManaSourcePenalty::None,
                    taps_for_mana: Vec::new(),
                },
            },
        };

        let err = guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err();
        assert!(err.contains("TapLandForMana.selection.restrictions.OnlyForAny"));
    }

    #[test]
    fn dispatch_guard_rejects_hostile_tap_land_trigger_production_at_preview_boundary() {
        let msg = ClientMessage::PreviewManaPayment {
            request_id: 7,
            action: GameAction::TapLandForMana {
                selection: ManaSourceSelection {
                    source: ObjectIncarnationRef::of(ObjectId(1), 1),
                    ability_index: None,
                    mana_type: ManaType::Green,
                    output: engine::types::mana::ManaSourceOutput::Concrete(ManaType::Green),
                    atomic_combination: None,
                    restrictions: Vec::new(),
                    penalty: ManaSourcePenalty::None,
                    taps_for_mana: vec![TapsForManaSelection {
                        source: ObjectIncarnationRef::of(ObjectId(2), 1),
                        occurrence: TriggerDefinitionOccurrenceRef::Printed {
                            base_set: TriggerBaseSetInstanceRef::INITIAL,
                            printed_index: 0,
                        },
                        production_override: ProductionOverride::Combination(vec![
                            ManaType::Red;
                            MAX_ACTION_LIST_LEN
                                + 1
                        ]),
                    }],
                },
            },
        };

        let err = guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err();
        assert!(err.contains("production_override.Combination"));
    }

    #[test]
    fn broker_projection_rejects_oversized_metadata_tokens_before_clone() {
        let msg = ClientMessage::UpdateLobbyMetadata {
            game_code: "GAME01".to_string(),
            current_players: 1,
            max_players: 2,
            consumed_reservation_tokens: vec!["t".repeat(129)],
        };
        let err = guard_broker_projection_inbound(&msg).unwrap_err();
        assert!(err.contains("consumed_reservation_token"));
    }

    #[test]
    fn broker_projection_rejects_too_many_consumed_tokens() {
        let msg = ClientMessage::UpdateLobbyMetadata {
            game_code: "GAME01".to_string(),
            current_players: 1,
            max_players: 2,
            consumed_reservation_tokens: vec!["ok".to_string(); MAX_CONSUMED_TOKENS + 1],
        };
        let err = guard_broker_projection_inbound(&msg).unwrap_err();
        assert!(err.contains("consumed_reservation_tokens"));
    }

    #[test]
    fn dispatch_guard_rejects_oversized_lookup_game_code() {
        let msg = ClientMessage::LookupJoinTarget {
            game_code: "x".repeat(65),
            password: None,
            reserve: false,
            display_name: None,
            release_reservation_token: None,
        };
        let err = guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err();
        assert!(err.contains("game_code"));
    }

    fn interaction_frame(response: InteractionResponse) -> ClientMessage {
        ClientMessage::Interaction {
            submission: InteractionSubmission {
                interaction_id: InteractionId("interaction-1".to_string()),
                response,
            },
        }
    }

    fn oversized_interaction_frame() -> ClientMessage {
        interaction_frame(InteractionResponse::Select {
            choice_ids: vec![InteractionChoiceId("a".to_string()); MAX_INTERACTION_LIST_LEN + 1],
        })
    }

    /// Direct sibling of `dispatch_guard_rejects_oversized_game_action_before_handler_work`.
    #[test]
    fn dispatch_guard_rejects_oversized_interaction_before_handler_work() {
        let err =
            guard_client_message_before_dispatch(&oversized_interaction_frame(), ServerMode::Full)
                .unwrap_err();

        assert!(err.contains("PayloadTooLarge"), "unexpected reason: {err}");
    }

    /// Non-vacuity guard for the test above, and a statement that the dispatch
    /// guard does not double as the mode gate — that is `reject_if_disabled`'s
    /// job.
    #[test]
    fn dispatch_guard_accepts_a_bounded_interaction() {
        let msg = interaction_frame(InteractionResponse::Choose {
            choice_id: InteractionChoiceId("a".to_string()),
        });

        assert!(guard_client_message_before_dispatch(&msg, ServerMode::Full).is_ok());
        assert!(guard_client_message_before_dispatch(&msg, ServerMode::LobbyOnly).is_ok());
    }

    /// Declared wire policy for the projection boundary: an interaction is a
    /// game frame, so `to_lobby_client_message` returns `None` for it and
    /// nothing is ever cloned into the broker. Unbounded is safe here *only*
    /// because nothing is cloned — which is why this test is meaningless
    /// without its pair, `interaction_is_never_projected_into_the_lobby_broker`
    /// in `phase-server`.
    #[test]
    fn broker_projection_accepts_an_interaction_without_bounding_it() {
        assert!(guard_broker_projection_inbound(&oversized_interaction_frame()).is_ok());
    }

    /// Both halves are required: the `Interaction` half alone would pass for a
    /// function that answered `ActionRejected` for everything, which would
    /// change `Action`'s behavior.
    #[test]
    fn interaction_wire_rejection_answers_on_the_benign_channel() {
        let interaction = oversized_interaction_frame();
        let reason =
            guard_client_message_before_dispatch(&interaction, ServerMode::Full).unwrap_err();

        match wire_rejection_message(&interaction, reason.clone()) {
            ServerMessage::ActionRejected { reason: answered } => {
                assert_eq!(answered, reason);
            }
            other => panic!("an interaction rejection must not tear the session down: {other:?}"),
        }

        let action = ClientMessage::Action {
            action: GameAction::ReorderHand {
                order: vec![ObjectId(1); MAX_ACTION_LIST_LEN + 1],
            },
        };
        let action_reason =
            guard_client_message_before_dispatch(&action, ServerMode::Full).unwrap_err();

        assert!(
            matches!(
                wire_rejection_message(&action, action_reason),
                ServerMessage::Error { .. }
            ),
            "an oversized action stays a malformed frame"
        );
    }
}
