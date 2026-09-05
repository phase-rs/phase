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
    validate_create_tournament_fields, validate_drop_from_tournament_fields,
    validate_end_tournament_fields, validate_get_tournament_fields,
    validate_join_tournament_fields, validate_report_match_result_fields,
    validate_start_tournament_round_fields, validate_unregister_lobby_fields,
    validate_update_lobby_metadata_fields, CreateTournamentFields, DropFromTournamentFields,
    EndTournamentFields, JoinTournamentFields, ReportMatchResultFields, StartTournamentRoundFields,
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
use crate::protocol::{resolve_draft_source_intent_ref, ClientMessage, ServerMessage, ServerMode};
use crate::seat_mutation_wire_guard::guard_seat_mutation;
use crate::spectator_wire_guard::{guard_spectate_draft, guard_spectator_join};
use engine::game::interaction::MAX_INTERACTION_STRING_LEN;
use engine::types::action_rejection::{ActionRejection, ActionRejectionCode};
use engine::types::interaction::{InteractionPreviewRequest, InteractionSubmission};

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
        ClientMessage::PreviewInteraction { request } => {
            guard_interaction_preview_request_payload(request)
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
        | ClientMessage::ExportAuthoritativeState
        | ClientMessage::RequestTakeback(_)
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
        // Tournament variants are lobby-scoped, so they delegate straight to
        // `lobby_broker::validation` exactly as `UpdateLobbyMetadata`/
        // `UnregisterLobby` above do — no `server-core`-local guard module,
        // which is reserved for game-action variants needing engine-type-aware
        // validation the broker crate cannot depend on. The identical calls
        // appear in `guard_broker_projection_inbound`; `both_inbound_guards_agree_
        // on_every_tournament_variant` is what keeps the two from drifting.
        ClientMessage::CreateTournament { name, .. } => {
            validate_create_tournament_fields(CreateTournamentFields { name })
        }
        ClientMessage::JoinTournament {
            code,
            player_key,
            display_name,
        } => validate_join_tournament_fields(JoinTournamentFields {
            code,
            player_key,
            display_name,
        }),
        ClientMessage::GetTournament { code } => validate_get_tournament_fields(code),
        ClientMessage::StartTournamentRound {
            code,
            organizer_token,
        } => validate_start_tournament_round_fields(StartTournamentRoundFields {
            code,
            organizer_token,
        }),
        ClientMessage::ReportMatchResult {
            code,
            player_token,
            outcome,
            ..
        } => validate_report_match_result_fields(ReportMatchResultFields {
            code,
            player_token,
            outcome,
        }),
        ClientMessage::DropFromTournament { code, player_token } => {
            validate_drop_from_tournament_fields(DropFromTournamentFields { code, player_token })
        }
        ClientMessage::EndTournament {
            code,
            organizer_token,
        } => validate_end_tournament_fields(EndTournamentFields {
            code,
            organizer_token,
        }),
        ClientMessage::CreateDraftWithSettings {
            display_name,
            source,
            set_codes,
            password,
            timer_seconds,
            pod_size,
            kind,
            tournament_format,
            ..
        } => {
            // Normalize the new tagged object and legacy root spelling before
            // validating any candidate token or touching a pool map.
            let intent = resolve_draft_source_intent_ref(source.as_ref(), set_codes.as_ref())?;
            guard_create_draft_with_settings(
                display_name,
                intent.set_codes(),
                password,
                *timer_seconds,
                *pod_size,
                *kind,
                *tournament_format,
            )
        }
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
        ClientMessage::Interaction { .. } => {
            let _ = reason;
            ServerMessage::ActionRejected {
                rejection: ActionRejection::new(ActionRejectionCode::InteractionPayloadTooLarge),
            }
        }
        ClientMessage::Action { .. } => ServerMessage::ActionRejected {
            rejection: ActionRejection::new(ActionRejectionCode::InvalidAction),
        },
        ClientMessage::PreviewManaPayment { request_id, .. } => {
            ServerMessage::ManaPaymentPreviewRejected {
                request_id: *request_id,
                rejection: ActionRejection::new(ActionRejectionCode::InvalidAction),
            }
        }
        // Benign like `Interaction`'s arm — a preview carries the same
        // free-form response an ordinary paste can oversize — and correlated
        // like `PreviewManaPayment`'s, so the client's pending promise settles
        // instead of hanging.
        ClientMessage::PreviewInteraction { request } => ServerMessage::InteractionPreviewFailed {
            request_id: request.request_id.clone(),
            message: reason,
        },

        // Every remaining variant keeps the operational-error behavior: a
        // bounds failure on these is a malformed frame, not a rejected game
        // decision.
        ClientMessage::ClientHello { .. }
        | ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::AbandonGame
        | ClientMessage::ExportAuthoritativeState
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
        | ClientMessage::RequestTakeback(_)
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback
        // Tournament variants join the operational-error bucket: none is a
        // game action, so there is no `ActionRejected`/`ManaPaymentPreviewRejected`
        // channel for them to answer on. Their bounds are also not routinely
        // trippable by a non-hostile client — a name over 40 characters or a
        // token over 128 bytes is a malformed frame, not a rejected decision
        // — so the `Error` disposal behavior is the right one here.
        | ClientMessage::CreateTournament { .. }
        | ClientMessage::JoinTournament { .. }
        | ClientMessage::GetTournament { .. }
        | ClientMessage::StartTournamentRound { .. }
        | ClientMessage::ReportMatchResult { .. }
        | ClientMessage::DropFromTournament { .. }
        | ClientMessage::EndTournament { .. } => ServerMessage::error(reason),
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
        // Identical delegation to `guard_client_message_before_dispatch`'s
        // arms, and it must stay identical: this guard runs BEFORE
        // `to_lobby_client_message` clones these strings and the `game_wins`
        // map into the broker, so anything unbounded here is an unbounded
        // clone — the same hazard `UpdateLobbyMetadata`'s arm above exists to
        // prevent.
        ClientMessage::CreateTournament { name, .. } => {
            validate_create_tournament_fields(CreateTournamentFields { name })
        }
        ClientMessage::JoinTournament {
            code,
            player_key,
            display_name,
        } => validate_join_tournament_fields(JoinTournamentFields {
            code,
            player_key,
            display_name,
        }),
        ClientMessage::GetTournament { code } => validate_get_tournament_fields(code),
        ClientMessage::StartTournamentRound {
            code,
            organizer_token,
        } => validate_start_tournament_round_fields(StartTournamentRoundFields {
            code,
            organizer_token,
        }),
        ClientMessage::ReportMatchResult {
            code,
            player_token,
            outcome,
            ..
        } => validate_report_match_result_fields(ReportMatchResultFields {
            code,
            player_token,
            outcome,
        }),
        ClientMessage::DropFromTournament { code, player_token } => {
            validate_drop_from_tournament_fields(DropFromTournamentFields { code, player_token })
        }
        ClientMessage::EndTournament {
            code,
            organizer_token,
        } => validate_end_tournament_fields(EndTournamentFields {
            code,
            organizer_token,
        }),
        ClientMessage::CreateGame { .. }
        | ClientMessage::JoinGame { .. }
        | ClientMessage::Action { .. }
        | ClientMessage::Interaction { .. }
        | ClientMessage::PreviewInteraction { .. }
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::ExportAuthoritativeState
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
        | ClientMessage::RequestTakeback(_)
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback => Ok(()),
    }
}

/// Wire bounds for an inbound `InteractionPreviewRequest`.
///
/// `InteractionPreviewRequest` differs from `InteractionSubmission` only by its
/// correlation key, so the two shared members are charged through the same
/// engine validator the submission path uses — the same cumulative budget, not
/// a restatement of it — and the key is charged against the engine's own
/// per-string ceiling. The rejection string is therefore byte-identical to the
/// submission's at every layer.
///
/// Private: the preview route has no second caller. The handler deliberately
/// re-guards nothing, because `preview_interaction` re-charges every member and
/// answers its own `PayloadTooLarge` on the preview's own correlated channel —
/// a third charge would buy a strictly worse frame.
///
/// ponytail: clones the two shared members to present them as a submission,
/// because `bound_interaction_submission` takes a borrowed `InteractionSubmission`
/// and no borrowed-view constructor exists. The clone is of an already-materialized
/// frame and is dropped at the end of this function, so it never enters a
/// long-lived structure. Upgrade path: a borrowed submission view in the engine.
fn guard_interaction_preview_request_payload(
    request: &InteractionPreviewRequest,
) -> Result<(), String> {
    if request.request_id.as_str().len() > MAX_INTERACTION_STRING_LEN {
        return Err(format!(
            "Engine error: {:?}",
            engine::types::interaction::InteractionReasonCode::PayloadTooLarge
        ));
    }
    guard_interaction_submission_payload(&InteractionSubmission {
        interaction_id: request.interaction_id.clone(),
        response: request.response.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_action_payload_guard::MAX_ACTION_LIST_LEN;
    use engine::game::interaction::MAX_INTERACTION_STRING_LEN;
    use engine::types::ability::{TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef};
    use engine::types::game_state::ProductionOverride;
    use engine::types::identifiers::ObjectIncarnationRef;
    use engine::types::interaction::{
        AmountAssignment, InteractionChoiceId, InteractionId, InteractionResponse,
        InteractionShortcutDecision, InteractionShortcutPin, InteractionSubmission,
        PreviewRequestId, MAX_INTERACTION_LIST_LEN,
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
        let _reason =
            guard_client_message_before_dispatch(&interaction, ServerMode::Full).unwrap_err();

        match wire_rejection_message(&interaction, _reason) {
            ServerMessage::ActionRejected { rejection } => {
                assert_eq!(
                    rejection.code,
                    ActionRejectionCode::InteractionPayloadTooLarge
                );
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
                ServerMessage::ActionRejected {
                    rejection: ActionRejection {
                        code: ActionRejectionCode::InvalidAction,
                        ..
                    }
                }
            ),
            "an oversized action is a typed invalid action"
        );
    }

    // -- Tournament organizer ----------------------------------------------

    use crate::protocol::{BracketShape, MatchArity, PodOutcome, ScoringPolicy};
    use lobby_broker::validation::{
        MAX_DISPLAY_NAME_LEN, MAX_GAME_CODE_LEN, MAX_GAME_WINS_ENTRIES, MAX_ROOM_NAME_LEN,
        MAX_TOKEN_LEN,
    };
    use std::collections::HashMap;

    /// One frame per tournament variant, each with exactly one oversized
    /// field, paired with the substring its rejection must name. Driving both
    /// guards from ONE table is the point: a variant that reached only one of
    /// them would have to be listed twice to slip through.
    fn oversized_tournament_frames() -> Vec<(&'static str, ClientMessage)> {
        let long_token = "t".repeat(MAX_TOKEN_LEN + 1);
        let long_code = "c".repeat(MAX_GAME_CODE_LEN + 1);
        vec![
            (
                "name",
                ClientMessage::CreateTournament {
                    name: "n".repeat(MAX_ROOM_NAME_LEN + 1),
                    arity: MatchArity::HEAD_TO_HEAD,
                    scoring: ScoringPolicy::default(),
                    bracket: BracketShape::Swiss,
                    total_rounds: None,
                },
            ),
            (
                "display_name",
                ClientMessage::JoinTournament {
                    code: "TOUR01".into(),
                    player_key: "key-a".into(),
                    display_name: "d".repeat(MAX_DISPLAY_NAME_LEN + 1),
                },
            ),
            (
                "code",
                ClientMessage::GetTournament {
                    code: long_code.clone(),
                },
            ),
            (
                "organizer_token",
                ClientMessage::StartTournamentRound {
                    code: "TOUR01".into(),
                    organizer_token: long_token.clone(),
                },
            ),
            (
                "player_token",
                ClientMessage::ReportMatchResult {
                    code: "TOUR01".into(),
                    pairing_id: 0,
                    player_token: long_token.clone(),
                    outcome: PodOutcome::Draw,
                },
            ),
            (
                "player_token",
                ClientMessage::DropFromTournament {
                    code: "TOUR01".into(),
                    player_token: long_token.clone(),
                },
            ),
            (
                "organizer_token",
                ClientMessage::EndTournament {
                    code: "TOUR01".into(),
                    organizer_token: long_token,
                },
            ),
        ]
    }

    fn valid_tournament_frames() -> Vec<ClientMessage> {
        vec![
            ClientMessage::CreateTournament {
                name: "Friday Night".into(),
                arity: MatchArity::HEAD_TO_HEAD,
                scoring: ScoringPolicy::default(),
                bracket: BracketShape::Swiss,
                total_rounds: Some(3),
            },
            ClientMessage::JoinTournament {
                code: "TOUR01".into(),
                player_key: "key-a".into(),
                display_name: "Alice".into(),
            },
            ClientMessage::GetTournament {
                code: "TOUR01".into(),
            },
            ClientMessage::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: "tok".into(),
            },
            ClientMessage::ReportMatchResult {
                code: "TOUR01".into(),
                pairing_id: 0,
                player_token: "tok".into(),
                outcome: PodOutcome::Draw,
            },
            ClientMessage::DropFromTournament {
                code: "TOUR01".into(),
                player_token: "tok".into(),
            },
            ClientMessage::EndTournament {
                code: "TOUR01".into(),
                organizer_token: "tok".into(),
            },
        ]
    }

    /// Verification Matrix row 13's core assertion. Both guards must reject
    /// the SAME oversized field for every variant — a variant bounded in one
    /// guard but not the other is exactly the silent divergence this test
    /// exists to rule out, and it is not a compile error (both matches would
    /// still be exhaustive with an `Ok(())` arm).
    #[test]
    fn both_inbound_guards_agree_on_every_tournament_variant() {
        for (field, msg) in oversized_tournament_frames() {
            let dispatch = guard_client_message_before_dispatch(&msg, ServerMode::Full)
                .expect_err(&format!("dispatch guard accepted an oversized {field}"));
            let projection = guard_broker_projection_inbound(&msg)
                .expect_err(&format!("projection guard accepted an oversized {field}"));

            assert!(dispatch.contains(field), "unexpected reason: {dispatch}");
            assert_eq!(
                dispatch, projection,
                "the two guards must delegate to the same validation function"
            );
        }
    }

    /// Non-vacuity for the test above: both guards ACCEPT well-formed frames,
    /// so neither is passing by rejecting everything. Also proves the mode
    /// argument does not gate tournaments — that stays `reject_if_disabled`'s
    /// job.
    #[test]
    fn both_inbound_guards_accept_valid_tournament_frames() {
        for msg in valid_tournament_frames() {
            assert!(
                guard_client_message_before_dispatch(&msg, ServerMode::Full).is_ok(),
                "Full-mode dispatch guard rejected {msg:?}"
            );
            assert!(
                guard_client_message_before_dispatch(&msg, ServerMode::LobbyOnly).is_ok(),
                "LobbyOnly dispatch guard rejected {msg:?}"
            );
            assert!(
                guard_broker_projection_inbound(&msg).is_ok(),
                "projection guard rejected {msg:?}"
            );
        }
    }

    /// The `ReportMatchResult` collection bound specifically, at the
    /// projection boundary — the direct sibling of
    /// `broker_projection_rejects_too_many_consumed_tokens`, for the one
    /// tournament payload that carries an unbounded-by-shape map a client
    /// controls, and which `to_lobby_client_message` would otherwise clone.
    #[test]
    fn broker_projection_rejects_an_oversized_game_wins_map_before_clone() {
        let game_wins: HashMap<String, u8> = (0..=MAX_GAME_WINS_ENTRIES)
            .map(|i| (format!("key-{i}"), 1u8))
            .collect();
        let msg = ClientMessage::ReportMatchResult {
            code: "TOUR01".into(),
            pairing_id: 0,
            player_token: "tok".into(),
            outcome: PodOutcome::Decisive {
                winner: "key-0".into(),
                game_wins,
            },
        };

        let err = guard_broker_projection_inbound(&msg).unwrap_err();
        assert!(err.contains("game_wins"), "unexpected reason: {err}");
        assert_eq!(
            guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err(),
            err
        );
    }

    /// The declared rejection channel: a tournament frame is not a game
    /// action, so it answers on the operational-error channel, never
    /// `ActionRejected`.
    #[test]
    fn tournament_wire_rejections_answer_on_the_operational_error_channel() {
        for (_, msg) in oversized_tournament_frames() {
            let reason = guard_client_message_before_dispatch(&msg, ServerMode::Full).unwrap_err();
            match wire_rejection_message(&msg, reason.clone()) {
                ServerMessage::Error { message, code } => {
                    assert_eq!(message, reason);
                    assert!(code.is_none());
                }
                other => panic!("a tournament frame must not answer as {other:?}"),
            }
        }
    }

    // ── Row 4: the payload ceiling on the preview path ────────────────────

    fn preview_frame(request_id: &str, response: InteractionResponse) -> ClientMessage {
        ClientMessage::PreviewInteraction {
            request: engine::types::interaction::InteractionPreviewRequest {
                request_id: PreviewRequestId(request_id.to_string()),
                interaction_id: InteractionId("interaction-1".to_string()),
                response,
            },
        }
    }

    /// Pins whose `amounts` are each individually under the list ceiling and
    /// whose sum is not — the cumulative branch a naive per-field guard misses.
    fn cumulative_shortcut(pin_count: u32) -> InteractionResponse {
        let per_pin = MAX_INTERACTION_LIST_LEN / 2;
        InteractionResponse::Shortcut {
            decision: InteractionShortcutDecision::AcceptSuggested,
            pins: (0..pin_count)
                .map(|group| InteractionShortcutPin {
                    group,
                    choice_ids: vec![InteractionChoiceId("a".to_string())],
                    amounts: vec![
                        AmountAssignment {
                            choice_id: InteractionChoiceId("a".to_string()),
                            amount: 1,
                        };
                        per_pin
                    ],
                })
                .collect(),
        }
    }

    /// Row 4. Sited at the WIRE GUARD, not at the engine: the engine charges the
    /// same triple independently before its `state.clone()`, so an engine-sited
    /// assertion would still pass with this arm reverted to `Ok(())` — vacuous.
    /// Replace the `PreviewInteraction` arm with `Ok(())` and the first
    /// assertion below accepts an oversized frame.
    #[test]
    fn dispatch_guard_rejects_an_oversized_preview_before_handler_work() {
        let three_pins = preview_frame("req-1", cumulative_shortcut(3));
        // Reach guard on the fixture itself: every pin is individually legal,
        // so the refusal below is attributable to the cumulative charge.
        match &three_pins {
            ClientMessage::PreviewInteraction { request } => {
                let InteractionResponse::Shortcut { pins, .. } = &request.response else {
                    panic!("fixture must carry a shortcut response");
                };
                assert!(pins.len() > 1, "the cumulative branch needs several pins");
                for pin in pins {
                    assert!(pin.amounts.len() <= MAX_INTERACTION_LIST_LEN);
                    assert!(pin.choice_ids.len() <= MAX_INTERACTION_LIST_LEN);
                }
            }
            other => panic!("wrong fixture: {other:?}"),
        }

        let err = guard_client_message_before_dispatch(&three_pins, ServerMode::Full)
            .expect_err("the cumulative budget is charged at the wire");
        assert!(err.contains("PayloadTooLarge"), "unexpected reason: {err}");

        // Discriminating positive: ONE pin of the identical shape passes the
        // bound, so the refusal above is the cumulative charge and not the pin
        // shape.
        assert_eq!(
            guard_client_message_before_dispatch(
                &preview_frame("req-1", cumulative_shortcut(1)),
                ServerMode::Full
            ),
            Ok(())
        );

        // The second member this helper charges: the correlation key, against
        // the engine's own per-string ceiling.
        let long_key = "x".repeat(MAX_INTERACTION_STRING_LEN + 1);
        let key_err = guard_client_message_before_dispatch(
            &preview_frame(
                &long_key,
                InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("a".to_string()),
                },
            ),
            ServerMode::Full,
        )
        .expect_err("an oversized request_id is refused");
        assert!(key_err.contains("PayloadTooLarge"), "unexpected: {key_err}");

        // The bound sits at the constant, not at "any long key".
        assert_eq!(
            guard_client_message_before_dispatch(
                &preview_frame(
                    &"x".repeat(MAX_INTERACTION_STRING_LEN),
                    InteractionResponse::Choose {
                        choice_id: InteractionChoiceId("a".to_string()),
                    },
                ),
                ServerMode::Full
            ),
            Ok(())
        );

        // And the interaction_id half still reaches the shared engine validator.
        let long_id = ClientMessage::PreviewInteraction {
            request: engine::types::interaction::InteractionPreviewRequest {
                request_id: PreviewRequestId("req-1".to_string()),
                interaction_id: InteractionId("x".repeat(MAX_INTERACTION_STRING_LEN + 1)),
                response: InteractionResponse::Choose {
                    choice_id: InteractionChoiceId("a".to_string()),
                },
            },
        };
        assert!(guard_client_message_before_dispatch(&long_id, ServerMode::Full).is_err());
    }

    /// The refusal string is byte-identical to the submission's, because both
    /// charge the same engine validator rather than restating its bounds.
    #[test]
    fn preview_and_submission_refuse_the_same_payload_identically() {
        let response = cumulative_shortcut(3);
        assert_eq!(
            guard_client_message_before_dispatch(
                &preview_frame("req-1", response.clone()),
                ServerMode::Full
            ),
            guard_client_message_before_dispatch(
                &interaction_frame(response.clone()),
                ServerMode::Full
            )
        );
        // Non-vacuity: both sides are `Err`, not both `Ok`.
        assert!(guard_client_message_before_dispatch(
            &interaction_frame(response),
            ServerMode::Full
        )
        .is_err());
    }

    /// Row 5's wire-guard legs. The preview is classified exactly as
    /// `Interaction` is at the projection boundary, and its rejection travels
    /// on a CORRELATED channel where `Interaction`'s does not — an uncorrelated
    /// answer would leave the client's pending promise hanging forever.
    #[test]
    fn preview_wire_rejection_is_benign_and_correlated() {
        let frame = preview_frame("req-1", cumulative_shortcut(3));
        let reason = guard_client_message_before_dispatch(&frame, ServerMode::Full).unwrap_err();

        match wire_rejection_message(&frame, reason.clone()) {
            ServerMessage::InteractionPreviewFailed {
                request_id,
                message,
            } => {
                assert_eq!(request_id, PreviewRequestId("req-1".to_string()));
                assert_eq!(message, reason);
            }
            other => panic!("a preview rejection must not tear the session down: {other:?}"),
        }

        // Same projection policy as its sibling: accepted without bounding,
        // safe only because `to_lobby_client_message` never clones it (the
        // paired half lives in `phase-server`).
        assert!(guard_broker_projection_inbound(&frame).is_ok());
    }
}
