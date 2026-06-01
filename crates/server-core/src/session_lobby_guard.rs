//! Wire validation for Full-mode game session paths that register lobby entries
//! outside `lobby_broker::Broker::handle`.
//!
//! LobbyOnly mode routes through the broker (and the WASM shell validates at
//! parse time). Full-mode `CreateGameWithSettings` / `JoinGameWithPassword`
//! construct sessions directly and call `LobbyManager::register_game` with
//! client-supplied strings that must be bounded before clone-heavy work.

use engine::starter_decks::DeckData;
use engine::types::format::FormatConfig;
use engine::types::match_config::MatchConfig;
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

/// Validate Full-mode `CreateGameWithSettings` lobby fields before deck resolve
/// and session creation.
pub fn guard_full_create_game_with_settings(
    deck: &DeckData,
    display_name: &str,
    public: bool,
    password: &Option<String>,
    timer_seconds: Option<u32>,
    player_count: u8,
    match_config: MatchConfig,
    format_config: &Option<FormatConfig>,
    room_name: &Option<String>,
    start_when_full: bool,
) -> Result<(), String> {
    guard_projected(&LobbyClientMessage::CreateGameWithSettings {
        deck: deck.clone(),
        display_name: display_name.to_string(),
        public,
        password: password.clone(),
        timer_seconds,
        player_count,
        match_config,
        format_config: format_config.clone(),
        room_name: room_name.clone(),
        host_peer_id: None,
        draft_metadata: None,
        start_when_full,
    })
}

/// Validate Full-mode `JoinGameWithPassword` lobby fields before deck resolve.
pub fn guard_full_join_game_with_password(
    game_code: &str,
    deck: &DeckData,
    display_name: &str,
    password: &Option<String>,
    reservation_token: &Option<String>,
) -> Result<(), String> {
    guard_projected(&LobbyClientMessage::JoinGameWithPassword {
        game_code: game_code.to_string(),
        deck: deck.clone(),
        display_name: display_name.to_string(),
        password: password.clone(),
        reservation_token: reservation_token.clone(),
    })
}
