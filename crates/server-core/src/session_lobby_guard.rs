//! Wire validation for Full-mode game session paths that register lobby entries
//! outside `lobby_broker::Broker::handle`.
//!
//! LobbyOnly mode routes through the broker (and the WASM shell validates at
//! parse time). Full-mode `CreateGameWithSettings` / `JoinGameWithPassword`
//! construct sessions directly and call `LobbyManager::register_game` with
//! client-supplied strings that must be bounded before clone-heavy work.

use engine::starter_decks::DeckData;
use lobby_broker::protocol::LobbyClientMessage;
use lobby_broker::validation::validate_lobby_message;

pub const MAX_MAIN_DECK_ENTRIES: usize = 500;
pub const MAX_SIDEBOARD_ENTRIES: usize = 100;
pub const MAX_COMMANDER_ENTRIES: usize = 4;
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

fn validate_deck_list(field: &str, cards: &[String], max_entries: usize) -> Result<(), String> {
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

fn validate_deck_payload(field: &str, deck: &DeckData) -> Result<(), String> {
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

fn guard_projected(msg: &LobbyClientMessage) -> Result<(), String> {
    validate_lobby_message(msg)?;
    match msg {
        LobbyClientMessage::CreateGameWithSettings { deck, .. }
        | LobbyClientMessage::JoinGameWithPassword { deck, .. } => {
            validate_deck_payload("deck", deck)?;
        }
        _ => {}
    }
    Ok(())
}

/// Validate Full-mode create/join lobby frames before deck resolve and session
/// creation.
pub fn guard_full_lobby_client_message(msg: &LobbyClientMessage) -> Result<(), String> {
    match msg {
        LobbyClientMessage::CreateGameWithSettings { .. }
        | LobbyClientMessage::JoinGameWithPassword { .. } => guard_projected(msg),
        _ => Err("unexpected lobby message for Full-mode wire guard".to_string()),
    }
}
