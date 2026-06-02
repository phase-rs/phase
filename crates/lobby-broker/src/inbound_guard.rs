//! Inbound lobby frame guard — shared validation at the broker dispatch boundary.
//!
//! The Cloudflare Worker shell validates through [`crate::protocol::parse_lobby_client_message`],
//! which calls [`crate::validation::validate_lobby_message`]. The native `phase-server` shell
//! deserializes the wider [`server_core::protocol::ClientMessage`] and projects lobby frames
//! onto [`crate::protocol::LobbyClientMessage`] without re-parsing, so those frames must be
//! checked here before any handler runs. Without this gate, oversized display names, passwords,
//! and deck payloads can be stored, cloned, and broadcast to every lobby subscriber.

use crate::protocol::LobbyClientMessage;
use crate::validation::validate_lobby_message;
use engine::starter_decks::DeckData;

/// Generous ceiling on main-deck entries at the wire boundary. Engine deck
/// validation enforces format legality later; this rejects multi-megabyte lists
/// before they are cloned through the native projection path.
pub const MAX_MAIN_DECK_ENTRIES: usize = 500;
/// Max sideboard entries accepted on the wire.
pub const MAX_SIDEBOARD_ENTRIES: usize = 100;
/// Max commander slots accepted on the wire.
pub const MAX_COMMANDER_ENTRIES: usize = 4;
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
    Ok(())
}

/// Validate every inbound lobby message before handler dispatch. Applies the
/// string/shape bounds from [`validate_lobby_message`] plus deck payload limits
/// on the two messages that carry a [`DeckData`] body.
pub fn guard_inbound(msg: &LobbyClientMessage) -> Result<(), String> {
    validate_lobby_message(msg)?;
    match msg {
        LobbyClientMessage::CreateGameWithSettings { deck, .. } => {
            validate_deck_payload("deck", deck)?;
        }
        LobbyClientMessage::JoinGameWithPassword { deck, .. } => {
            validate_deck_payload("deck", deck)?;
        }
        _ => {}
    }
    Ok(())
}
