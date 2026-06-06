//! Wire validation for draft session handlers in `phase-server`.
//!
//! Draft create/join/reconnect/action frames are `ClientMessage` variants handled
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
    validate_optional_token("password", password.as_deref(), MAX_PASSWORD_LEN)?;
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
    validate_optional_token("password", password.as_deref(), MAX_PASSWORD_LEN)?;
    Ok(())
}

/// Validate `ReconnectDraft` wire fields before token lookup.
pub fn guard_reconnect_draft(draft_code: &str, player_token: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)?;
    validate_token("player_token", player_token, MAX_TOKEN_LEN)?;
    Ok(())
}

/// Validate `DraftAction` wire fields before draft session lookup and mutation.
pub fn guard_draft_action(draft_code: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)
}

#[cfg(test)]
mod tests {
    use lobby_broker::validation::MAX_GAME_CODE_LEN;

    use super::{
        guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
        guard_reconnect_draft,
    };

    #[test]
    fn create_draft_accepts_valid_fields() {
        assert!(guard_create_draft_with_settings("Alice", "TST", &None, None, 4).is_ok());
    }

    #[test]
    fn create_draft_rejects_oversized_display_name() {
        let err =
            guard_create_draft_with_settings(&"a".repeat(21), "TST", &None, None, 4).unwrap_err();
        assert!(err.contains("display_name"));
    }

    #[test]
    fn join_draft_rejects_oversized_draft_code() {
        let err = guard_join_draft_with_password(&"x".repeat(MAX_GAME_CODE_LEN + 1), "Bob", &None)
            .unwrap_err();
        assert!(err.contains("draft_code"));
    }

    #[test]
    fn reconnect_rejects_oversized_player_token() {
        let err = guard_reconnect_draft("ABC123", &"t".repeat(129)).unwrap_err();
        assert!(err.contains("player_token"));
    }

    #[test]
    fn draft_action_accepts_valid_code() {
        assert!(guard_draft_action("ABC123").is_ok());
    }

    #[test]
    fn draft_action_rejects_oversized_code() {
        let err = guard_draft_action(&"x".repeat(MAX_GAME_CODE_LEN + 1)).unwrap_err();
        assert!(err.contains("draft_code"));
    }
}
