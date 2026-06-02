//! Size/shape bounds for client-supplied lobby fields.
//!
//! The broker stores client-provided strings and numbers and broadcasts many of
//! them (host name, room name, player counts) to *every* lobby subscriber, so it
//! must bound their size regardless of which shell sits in front of it. The
//! Cloudflare Worker's `name-filter.ts` performs **content moderation** (banned
//! words, URL-like names) on the display/room names, but that runs only in the
//! Worker shell — the native `phase-server` shell and any raw WebSocket client
//! bypass it. This module performs **size/shape validation only**: the
//! resource-safety contract both shells share, enforced once at the protocol
//! boundary. It deliberately does not moderate content; that stays shell policy.
//!
//! Mirrors the existing [`crate::MAX_LOBBY_ENTRIES`] philosophy of bounding
//! attacker-controlled growth at the broker, and reuses the visible-name limits
//! the Worker already advertises so the two shells agree on name length.

/// Max display-name length, in characters. Matches `DISPLAY_NAME_MAX_LENGTH` in
/// the Worker's `name-filter.ts`.
pub const MAX_DISPLAY_NAME_LEN: usize = 20;
/// Max room-name length, in characters. Matches `ROOM_NAME_MAX_LENGTH` in the
/// Worker's `name-filter.ts`.
pub const MAX_ROOM_NAME_LEN: usize = 40;
/// Max lobby password length, in bytes.
pub const MAX_PASSWORD_LEN: usize = 128;
/// Max game-code length, in bytes. Codes are short server-issued handles; this
/// is a generous ceiling that still rejects multi-kilobyte junk.
pub const MAX_GAME_CODE_LEN: usize = 64;
/// Max length, in bytes, of opaque token/identifier fields (reservation tokens,
/// host peer id, build commit, version strings).
pub const MAX_TOKEN_LEN: usize = 128;
/// Max per-turn timer a client may request, in seconds (24h). Beyond this is
/// nonsensical and is rejected before it is stored/broadcast.
pub const MAX_TIMER_SECONDS: u32 = 86_400;
/// Hard ceiling on requested seat count. Exact per-format legality is enforced
/// later by the engine; this just rejects absurd values at the boundary.
pub const MAX_PLAYER_COUNT: u8 = 8;
/// Max number of consumed-reservation tokens accepted in one metadata update.
pub const MAX_CONSUMED_TOKENS: usize = 64;
/// Max draft set-code length, in bytes. Real set codes are much shorter; this
/// leaves room for the synthetic cube sentinel while rejecting stored junk.
pub const MAX_DRAFT_SET_CODE_LEN: usize = 32;
/// Max draft kind label length, in bytes.
pub const MAX_DRAFT_KIND_LEN: usize = 32;

/// Reject ASCII control characters (C0 range and DEL). They corrupt logs, lobby
/// listings, and UI rendering and never belong in a name, code, or token.
fn has_control_char(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

/// Validate a required visible label (e.g. the host display name): it must be
/// non-empty after trimming, within `max` characters, and free of control
/// characters. `field` names the field for the error message.
pub fn validate_required_label(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{field} must be at most {max} characters"));
    }
    if has_control_char(value) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

/// Validate an optional visible label (e.g. room name, optional display name).
/// `None` and `Some("")`/whitespace are accepted — the broker treats blank
/// labels as absent — but a present, non-blank label must satisfy the bounds.
fn validate_optional_label(field: &str, value: &Option<String>, max: usize) -> Result<(), String> {
    match value {
        Some(v) if !v.trim().is_empty() => validate_required_label(field, v, max),
        _ => Ok(()),
    }
}

/// Validate an opaque token/identifier string: bounded byte length and no
/// control characters. Empty is allowed (callers treat empty as absent).
pub fn validate_token(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        return Err(format!("{field} must be at most {max} bytes"));
    }
    if has_control_char(value) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

/// Validate an optional opaque token/identifier string.
pub fn validate_optional_token(
    field: &str,
    value: &Option<String>,
    max: usize,
) -> Result<(), String> {
    match value {
        Some(v) => validate_token(field, v, max),
        None => Ok(()),
    }
}

/// Validate every client-supplied field of a parsed lobby message against the
/// size/shape bounds above. Returns the first violation as a human-readable
/// reason suitable for an `Error` reply. Server-populated reply types
/// (`LobbyServerMessage`) are not validated here — only inbound client frames.
pub fn validate_lobby_message(msg: &crate::protocol::LobbyClientMessage) -> Result<(), String> {
    use crate::protocol::LobbyClientMessage as M;

    match msg {
        M::ClientHello {
            client_version,
            build_commit,
            ..
        } => {
            validate_token("client_version", client_version, MAX_TOKEN_LEN)?;
            validate_token("build_commit", build_commit, MAX_TOKEN_LEN)?;
        }
        M::CreateGameWithSettings {
            display_name,
            password,
            timer_seconds,
            player_count,
            room_name,
            host_peer_id,
            draft_metadata,
            ..
        } => {
            validate_required_label("display_name", display_name, MAX_DISPLAY_NAME_LEN)?;
            validate_optional_label("room_name", room_name, MAX_ROOM_NAME_LEN)?;
            validate_optional_token("password", password, MAX_PASSWORD_LEN)?;
            validate_optional_token("host_peer_id", host_peer_id, MAX_TOKEN_LEN)?;
            if *player_count == 0 || *player_count > MAX_PLAYER_COUNT {
                return Err(format!(
                    "player_count must be between 1 and {MAX_PLAYER_COUNT}"
                ));
            }
            if let Some(secs) = timer_seconds {
                if *secs > MAX_TIMER_SECONDS {
                    return Err(format!("timer_seconds must be at most {MAX_TIMER_SECONDS}"));
                }
            }
            if let Some(draft) = draft_metadata {
                validate_token(
                    "draft_metadata.set_code",
                    &draft.set_code,
                    MAX_DRAFT_SET_CODE_LEN,
                )?;
                validate_token(
                    "draft_metadata.draft_kind",
                    &draft.draft_kind,
                    MAX_DRAFT_KIND_LEN,
                )?;
                validate_optional_label(
                    "draft_metadata.cube_name",
                    &draft.cube_name,
                    MAX_ROOM_NAME_LEN,
                )?;
            }
        }
        M::JoinGameWithPassword {
            game_code,
            display_name,
            password,
            reservation_token,
            ..
        } => {
            validate_token("game_code", game_code, MAX_GAME_CODE_LEN)?;
            validate_required_label("display_name", display_name, MAX_DISPLAY_NAME_LEN)?;
            validate_optional_token("password", password, MAX_PASSWORD_LEN)?;
            validate_optional_token("reservation_token", reservation_token, MAX_TOKEN_LEN)?;
        }
        M::LookupJoinTarget {
            game_code,
            password,
            display_name,
            release_reservation_token,
            ..
        } => {
            validate_token("game_code", game_code, MAX_GAME_CODE_LEN)?;
            validate_optional_token("password", password, MAX_PASSWORD_LEN)?;
            validate_optional_label("display_name", display_name, MAX_DISPLAY_NAME_LEN)?;
            validate_optional_token(
                "release_reservation_token",
                release_reservation_token,
                MAX_TOKEN_LEN,
            )?;
        }
        M::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
            consumed_reservation_tokens,
        } => {
            validate_token("game_code", game_code, MAX_GAME_CODE_LEN)?;
            if *max_players == 0 || *max_players > MAX_PLAYER_COUNT {
                return Err(format!(
                    "max_players must be between 1 and {MAX_PLAYER_COUNT}"
                ));
            }
            if *current_players > MAX_PLAYER_COUNT {
                return Err(format!(
                    "current_players must be at most {MAX_PLAYER_COUNT}"
                ));
            }
            if *current_players > *max_players {
                return Err("current_players must not exceed max_players".to_string());
            }
            if consumed_reservation_tokens.len() > MAX_CONSUMED_TOKENS {
                return Err(format!(
                    "consumed_reservation_tokens must contain at most {MAX_CONSUMED_TOKENS} entries"
                ));
            }
            for token in consumed_reservation_tokens {
                validate_token("consumed_reservation_token", token, MAX_TOKEN_LEN)?;
            }
        }
        M::UnregisterLobby { game_code } => {
            validate_token("game_code", game_code, MAX_GAME_CODE_LEN)?;
        }
        // No client-supplied bounded fields.
        M::SubscribeLobby | M::UnsubscribeLobby | M::Ping { .. } => {}
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbound_guard::{
        guard_inbound, MAX_DECK_CARD_NAME_LEN, MAX_MAIN_DECK_ENTRIES, MAX_SIDEBOARD_ENTRIES,
    };
    use crate::protocol::{DraftLobbyMetadata, LobbyClientMessage as M};
    use engine::starter_decks::DeckData;

    fn empty_deck() -> DeckData {
        serde_json::from_str(r#"{"main_deck":[]}"#).expect("deck fixture")
    }

    fn create_with(display_name: &str) -> M {
        M::CreateGameWithSettings {
            deck: empty_deck(),
            display_name: display_name.to_string(),
            public: true,
            password: None,
            timer_seconds: None,
            player_count: 2,
            match_config: Default::default(),
            format_config: None,
            room_name: None,
            host_peer_id: None,
            draft_metadata: None,
            start_when_full: true,
            ranked: false,
        }
    }

    fn join_with(display_name: &str) -> M {
        M::JoinGameWithPassword {
            game_code: "ABC123".into(),
            deck: empty_deck(),
            display_name: display_name.to_string(),
            password: None,
            reservation_token: None,
        }
    }

    #[test]
    fn required_label_bounds() {
        assert!(validate_required_label("f", "Alice", MAX_DISPLAY_NAME_LEN).is_ok());
        assert!(validate_required_label("f", "", MAX_DISPLAY_NAME_LEN).is_err());
        assert!(validate_required_label("f", "   ", MAX_DISPLAY_NAME_LEN).is_err());
        assert!(validate_required_label("f", &"a".repeat(21), MAX_DISPLAY_NAME_LEN).is_err());
        assert!(validate_required_label("f", &"a".repeat(20), MAX_DISPLAY_NAME_LEN).is_ok());
        assert!(validate_required_label("f", "bad\u{0007}name", MAX_DISPLAY_NAME_LEN).is_err());
    }

    #[test]
    fn multibyte_names_counted_by_character() {
        // 20 multibyte chars is allowed (char count, not byte count); 21 is not.
        assert!(validate_required_label("f", &"é".repeat(20), MAX_DISPLAY_NAME_LEN).is_ok());
        assert!(validate_required_label("f", &"é".repeat(21), MAX_DISPLAY_NAME_LEN).is_err());
    }

    #[test]
    fn optional_label_allows_absent_and_blank() {
        assert!(validate_optional_label("f", &None, MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", &Some(String::new()), MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", &Some("  ".into()), MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", &Some("Room".into()), MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", &Some("r".repeat(41)), MAX_ROOM_NAME_LEN).is_err());
    }

    #[test]
    fn token_bounds() {
        assert!(validate_token("f", "", MAX_TOKEN_LEN).is_ok());
        assert!(validate_token("f", &"x".repeat(MAX_TOKEN_LEN), MAX_TOKEN_LEN).is_ok());
        assert!(validate_token("f", &"x".repeat(MAX_TOKEN_LEN + 1), MAX_TOKEN_LEN).is_err());
        assert!(validate_token("f", "tok\nen", MAX_TOKEN_LEN).is_err());
    }

    #[test]
    fn create_game_accepts_valid() {
        assert!(validate_lobby_message(&create_with("Alice")).is_ok());
    }

    #[test]
    fn create_game_rejects_oversized_display_name() {
        assert!(validate_lobby_message(&create_with(&"a".repeat(21))).is_err());
    }

    #[test]
    fn create_game_rejects_oversized_password() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { password, .. } = &mut msg {
            *password = Some("p".repeat(MAX_PASSWORD_LEN + 1));
        }
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn create_game_rejects_out_of_range_timer() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { timer_seconds, .. } = &mut msg {
            *timer_seconds = Some(MAX_TIMER_SECONDS + 1);
        }
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn create_game_rejects_out_of_range_player_count() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { player_count, .. } = &mut msg {
            *player_count = MAX_PLAYER_COUNT + 1;
        }
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn create_game_rejects_oversized_draft_metadata() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { draft_metadata, .. } = &mut msg {
            *draft_metadata = Some(DraftLobbyMetadata {
                set_code: "a".repeat(MAX_DRAFT_SET_CODE_LEN + 1),
                draft_kind: "Quick".to_string(),
                cube_name: None,
            });
        }
        assert!(validate_lobby_message(&msg).is_err());

        if let M::CreateGameWithSettings { draft_metadata, .. } = &mut msg {
            *draft_metadata = Some(DraftLobbyMetadata {
                set_code: "TDM".to_string(),
                draft_kind: "d".repeat(MAX_DRAFT_KIND_LEN + 1),
                cube_name: None,
            });
        }
        assert!(validate_lobby_message(&msg).is_err());

        if let M::CreateGameWithSettings { draft_metadata, .. } = &mut msg {
            *draft_metadata = Some(DraftLobbyMetadata {
                set_code: "custom-cube".to_string(),
                draft_kind: "Cube".to_string(),
                cube_name: Some("c".repeat(MAX_ROOM_NAME_LEN + 1)),
            });
        }
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn update_metadata_rejects_too_many_tokens() {
        let msg = M::UpdateLobbyMetadata {
            game_code: "ABC123".into(),
            current_players: 1,
            max_players: 2,
            consumed_reservation_tokens: vec![String::from("t"); MAX_CONSUMED_TOKENS + 1],
        };
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn update_metadata_rejects_out_of_range_players() {
        let msg = M::UpdateLobbyMetadata {
            game_code: "ABC123".into(),
            current_players: 1,
            max_players: MAX_PLAYER_COUNT + 1,
            consumed_reservation_tokens: Vec::new(),
        };
        assert!(validate_lobby_message(&msg).is_err());

        let msg = M::UpdateLobbyMetadata {
            game_code: "ABC123".into(),
            current_players: MAX_PLAYER_COUNT + 1,
            max_players: MAX_PLAYER_COUNT,
            consumed_reservation_tokens: Vec::new(),
        };
        assert!(validate_lobby_message(&msg).is_err());

        let msg = M::UpdateLobbyMetadata {
            game_code: "ABC123".into(),
            current_players: 3,
            max_players: 2,
            consumed_reservation_tokens: Vec::new(),
        };
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn ping_has_no_bounded_fields() {
        assert!(validate_lobby_message(&M::Ping { timestamp: 1 }).is_ok());
    }

    #[test]
    fn guard_rejects_oversized_main_deck() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { deck, .. } = &mut msg {
            deck.main_deck = vec!["Card".to_string(); MAX_MAIN_DECK_ENTRIES + 1];
        }
        assert!(guard_inbound(&msg).is_err());
    }

    #[test]
    fn guard_rejects_oversized_card_name_in_deck() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { deck, .. } = &mut msg {
            deck.main_deck = vec!["x".repeat(MAX_DECK_CARD_NAME_LEN + 1)];
        }
        assert!(guard_inbound(&msg).is_err());
    }

    #[test]
    fn guard_rejects_oversized_join_sideboard() {
        let mut msg = join_with("Bob");
        if let M::JoinGameWithPassword { deck, .. } = &mut msg {
            deck.sideboard = vec!["Card".to_string(); MAX_SIDEBOARD_ENTRIES + 1];
        }
        assert!(guard_inbound(&msg).is_err());
    }
}
