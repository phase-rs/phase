//! Wire validation for draft session handlers in `phase-server`.
//!
//! Draft create/join/reconnect frames are `ClientMessage` variants handled
//! directly by the server shell. Unlike lobby game frames, they never pass
//! through `lobby_broker::validate_lobby_message`, so client-supplied names,
//! codes, passwords, and tokens must be bounded before clone-heavy work.

use lobby_broker::validation::{
    validate_optional_token, validate_required_label, validate_token, MAX_DISPLAY_NAME_LEN,
    MAX_DRAFT_SET_CODE_LEN, MAX_GAME_CODE_LEN, MAX_PASSWORD_LEN, MAX_PLAYER_COUNT,
    MAX_TIMER_SECONDS, MAX_TOKEN_LEN,
};

/// Validate `CreateDraftWithSettings` wire fields before pool lookup and lobby
/// registration.
pub fn guard_create_draft_with_settings(
    display_name: &str,
    set_code: &str,
    password: &Option<String>,
    timer_seconds: Option<u32>,
    pod_size: u8,
) -> Result<(), String> {
    validate_required_label("display_name", display_name, MAX_DISPLAY_NAME_LEN)?;
    validate_token("set_code", set_code, MAX_DRAFT_SET_CODE_LEN)?;
    validate_optional_token("password", password, MAX_PASSWORD_LEN)?;
    if pod_size == 0 || pod_size > MAX_PLAYER_COUNT {
        return Err(format!("pod_size must be between 1 and {MAX_PLAYER_COUNT}"));
    }
    if let Some(secs) = timer_seconds {
        if secs > MAX_TIMER_SECONDS {
            return Err(format!("timer_seconds must be at most {MAX_TIMER_SECONDS}"));
        }
    }
    Ok(())
}

/// Validate `JoinDraftWithPassword` wire fields before draft session mutation.
pub fn guard_join_draft_with_password(
    draft_code: &str,
    display_name: &str,
    password: &Option<String>,
) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)?;
    validate_required_label("display_name", display_name, MAX_DISPLAY_NAME_LEN)?;
    validate_optional_token("password", password, MAX_PASSWORD_LEN)?;
    Ok(())
}

/// Validate `ReconnectDraft` wire fields before token lookup.
pub fn guard_reconnect_draft(draft_code: &str, player_token: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)?;
    validate_token("player_token", player_token, MAX_TOKEN_LEN)?;
    Ok(())
}
