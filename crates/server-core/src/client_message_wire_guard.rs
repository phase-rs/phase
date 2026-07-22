//! Centralized native-shell validation for every [`crate::protocol::ClientMessage`]
//! variant before handler dispatch and broker projection clones.
//!
//! Individual handlers still run their guards for defense in depth; this layer
//! guarantees a single exhaustive match so new variants must declare wire policy
//! and broker-projected frames are bounded before `to_lobby_client_message` clones.

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
use crate::legacy_deck_guard::guard_legacy_deck;
use crate::legacy_join_guard::guard_legacy_join_game;
use crate::protocol::{ClientMessage, ServerMode};
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
        ClientMessage::Reconnect {
            game_code,
            player_token,
        } => guard_game_reconnect(game_code, player_token),
        ClientMessage::SubscribeLobby
        | ClientMessage::UnsubscribeLobby
        | ClientMessage::Concede
        | ClientMessage::RequestTakeback
        | ClientMessage::RespondTakeback { .. }
        | ClientMessage::CancelTakeback => Ok(()),
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
        | ClientMessage::PreviewManaPayment { .. }
        | ClientMessage::Reconnect { .. }
        | ClientMessage::Concede
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
}
