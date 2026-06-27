//! Inbound lobby frame guard — shared validation at the broker dispatch boundary.
//!
//! The Cloudflare Worker shell validates through [`crate::protocol::parse_lobby_client_message`],
//! which calls [`crate::validation::validate_lobby_message`]. The native `phase-server` shell
//! deserializes the wider [`server_core::protocol::ClientMessage`] and projects lobby frames
//! onto [`crate::protocol::LobbyClientMessage`] without re-parsing, so those frames must be
//! checked here before any handler runs. Without this gate, oversized display names, passwords,
//! and deck payloads can be stored, cloned, and broadcast to every lobby subscriber.

use crate::protocol::DraftLobbyMetadata;
use crate::protocol::LobbyClientMessage;
use crate::validation::{
    validate_create_game_settings_fields, validate_join_game_with_password_fields,
    validate_lobby_message, validate_lookup_join_target_fields, CreateGameSettingsFields,
    JoinGameWithPasswordFields, LookupJoinTargetFields, MAX_PLAYER_COUNT,
};
use engine::starter_decks::DeckData;
use engine::types::format::FormatConfig;

/// Generous ceiling on main-deck entries at the wire boundary. Engine deck
/// validation enforces format legality later; this rejects multi-megabyte lists
/// before they are cloned through the native projection path.
pub const MAX_MAIN_DECK_ENTRIES: usize = 500;
/// Max sideboard entries accepted on the wire.
pub const MAX_SIDEBOARD_ENTRIES: usize = 100;
/// Max commander slots accepted on the wire.
pub const MAX_COMMANDER_ENTRIES: usize = 4;
/// Max supplementary Planechase planar-deck entries accepted on the wire.
pub const MAX_PLANAR_DECK_ENTRIES: usize = 200;
/// Max supplementary Archenemy scheme-deck entries accepted on the wire.
pub const MAX_SCHEME_DECK_ENTRIES: usize = 200;
/// Max byte length of a single card name string inside a deck payload.
pub const MAX_DECK_CARD_NAME_LEN: usize = 256;

fn validate_card_name(field: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if name.len() > MAX_DECK_CARD_NAME_LEN {
        return Err(format!(
            "{field} must be at most {MAX_DECK_CARD_NAME_LEN} bytes"
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

pub fn validate_deck_list(field: &str, cards: &[String], max_entries: usize) -> Result<(), String> {
    if cards.len() > max_entries {
        return Err(format!(
            "{field} must contain at most {max_entries} entries"
        ));
    }
    for (index, name) in cards.iter().enumerate() {
        validate_card_name(&format!("{field}[{index}]"), name)?;
    }
    Ok(())
}

/// Bound the deck half of Create/Join lobby messages. Lobby-only mode ignores
/// deck contents for matchmaking, but the native shell still deserializes and
/// clones the full structure on every frame.
pub fn validate_deck_payload(field: &str, deck: &DeckData) -> Result<(), String> {
    validate_deck_list(
        &format!("{field}.main_deck"),
        &deck.main_deck,
        MAX_MAIN_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.sideboard"),
        &deck.sideboard,
        MAX_SIDEBOARD_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.commander"),
        &deck.commander,
        MAX_COMMANDER_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.planar_deck"),
        &deck.planar_deck,
        MAX_PLANAR_DECK_ENTRIES,
    )?;
    validate_deck_list(
        &format!("{field}.scheme_deck"),
        &deck.scheme_deck,
        MAX_SCHEME_DECK_ENTRIES,
    )?;
    Ok(())
}

pub struct CreateGameSettingsInbound<'a> {
    pub deck: &'a DeckData,
    pub display_name: &'a str,
    pub password: Option<&'a str>,
    pub timer_seconds: Option<u32>,
    pub player_count: u8,
    pub format_config: Option<&'a FormatConfig>,
    pub room_name: Option<&'a str>,
    pub host_peer_id: Option<&'a str>,
    pub draft_metadata: Option<&'a DraftLobbyMetadata>,
}

/// Validate a settings-create frame without constructing the owned broker enum.
pub fn validate_create_game_settings_inbound_fields(
    fields: &CreateGameSettingsInbound<'_>,
) -> Result<(), String> {
    validate_create_game_settings_fields(CreateGameSettingsFields {
        display_name: fields.display_name,
        password: fields.password,
        timer_seconds: fields.timer_seconds,
        player_count: fields.player_count,
        room_name: fields.room_name,
        host_peer_id: fields.host_peer_id,
        draft_metadata: fields.draft_metadata,
    })
}

/// Validate a settings-create frame without constructing the owned broker enum.
pub fn guard_create_game_settings_inbound(
    fields: CreateGameSettingsInbound<'_>,
) -> Result<(), String> {
    validate_create_game_settings_inbound_fields(&fields)?;
    if let Some(format_config) = fields.format_config {
        format_config.validate_for_player_count(fields.player_count.clamp(2, MAX_PLAYER_COUNT))?;
    }
    validate_deck_payload("deck", fields.deck)
}

pub struct JoinGameWithPasswordInbound<'a> {
    pub game_code: &'a str,
    pub deck: &'a DeckData,
    pub display_name: &'a str,
    pub password: Option<&'a str>,
    pub reservation_token: Option<&'a str>,
}

/// Validate a settings-join frame without constructing the owned broker enum.
pub fn guard_join_game_with_password_inbound(
    fields: JoinGameWithPasswordInbound<'_>,
) -> Result<(), String> {
    validate_join_game_with_password_fields(JoinGameWithPasswordFields {
        game_code: fields.game_code,
        display_name: fields.display_name,
        password: fields.password,
        reservation_token: fields.reservation_token,
    })?;
    validate_deck_payload("deck", fields.deck)
}

pub type LookupJoinTargetInbound<'a> = LookupJoinTargetFields<'a>;

/// Validate a lookup frame without constructing the owned broker enum.
pub fn guard_lookup_join_target_inbound(fields: LookupJoinTargetInbound<'_>) -> Result<(), String> {
    validate_lookup_join_target_fields(fields)
}

/// Validate every inbound lobby message before handler dispatch. Applies the
/// string/shape bounds from [`validate_lobby_message`] plus deck payload limits
/// on the two messages that carry a [`DeckData`] body.
pub fn guard_inbound(msg: &LobbyClientMessage) -> Result<(), String> {
    match msg {
        LobbyClientMessage::CreateGameWithSettings {
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
        })?,
        LobbyClientMessage::JoinGameWithPassword {
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
        })?,
        _ => validate_lobby_message(msg)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(main: usize, sideboard: usize) -> DeckData {
        DeckData {
            main_deck: vec!["Forest".to_string(); main],
            sideboard: vec!["Forest".to_string(); sideboard],
            ..Default::default()
        }
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_deck_without_owned_message() {
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(MAX_MAIN_DECK_ENTRIES + 1, 0),
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
        })
        .unwrap_err();

        assert!(err.contains("main_deck"));
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_lobby_field() {
        let display_name = "a".repeat(crate::validation::MAX_DISPLAY_NAME_LEN + 1);
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(1, 0),
            display_name: &display_name,
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
        })
        .unwrap_err();

        assert!(err.contains("display_name"));
    }

    #[test]
    fn borrowed_create_guard_rejects_archenemy_seat_outside_player_count() {
        let mut format_config = FormatConfig::archenemy();
        format_config.archenemy_player = Some(engine::types::player::PlayerId(3));

        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck(1, 0),
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: Some(&format_config),
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
        })
        .unwrap_err();

        assert!(err.contains("archenemy_player"));
    }

    #[test]
    fn borrowed_join_guard_rejects_oversized_deck_without_owned_message() {
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck: &deck(1, MAX_SIDEBOARD_ENTRIES + 1),
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("sideboard"));
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_planar_deck() {
        let mut deck = deck(1, 0);
        deck.planar_deck = vec!["Plane".to_string(); MAX_PLANAR_DECK_ENTRIES + 1];
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck,
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
        })
        .unwrap_err();

        assert!(err.contains("planar_deck"));
    }

    #[test]
    fn borrowed_join_guard_rejects_invalid_planar_deck_entry() {
        let mut deck = deck(1, 0);
        deck.planar_deck = vec!["Bad\nPlane".to_string()];
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck: &deck,
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("planar_deck[0]"));
    }

    #[test]
    fn borrowed_create_guard_rejects_oversized_scheme_deck() {
        let mut deck = deck(1, 0);
        deck.scheme_deck = vec!["Scheme".to_string(); MAX_SCHEME_DECK_ENTRIES + 1];
        let err = guard_create_game_settings_inbound(CreateGameSettingsInbound {
            deck: &deck,
            display_name: "Host",
            password: None,
            timer_seconds: None,
            player_count: 2,
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
        })
        .unwrap_err();

        assert!(err.contains("scheme_deck"));
    }

    #[test]
    fn borrowed_join_guard_rejects_invalid_scheme_deck_entry() {
        let mut deck = deck(1, 0);
        deck.scheme_deck = vec!["Bad\nScheme".to_string()];
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: "GAME01",
            deck: &deck,
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("scheme_deck[0]"));
    }

    #[test]
    fn borrowed_join_guard_rejects_oversized_lobby_field() {
        let game_code = "G".repeat(crate::validation::MAX_GAME_CODE_LEN + 1);
        let err = guard_join_game_with_password_inbound(JoinGameWithPasswordInbound {
            game_code: &game_code,
            deck: &deck(1, 0),
            display_name: "Guest",
            password: None,
            reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("game_code"));
    }

    #[test]
    fn borrowed_lookup_guard_rejects_oversized_lobby_field() {
        let game_code = "G".repeat(crate::validation::MAX_GAME_CODE_LEN + 1);
        let err = guard_lookup_join_target_inbound(LookupJoinTargetInbound {
            game_code: &game_code,
            password: None,
            display_name: None,
            release_reservation_token: None,
        })
        .unwrap_err();

        assert!(err.contains("game_code"));
    }
}
