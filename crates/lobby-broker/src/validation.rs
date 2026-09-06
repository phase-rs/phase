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

use crate::protocol::DraftLobbyMetadata;

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
/// Max length, in bytes, of the draft SOURCE label a lobby listing carries.
///
/// Not one set code: a multi-set draft joins its distinct set codes with `+`
/// into a single label (`draft_core::types::DraftSource::set_code`, e.g.
/// `"ISD+DKA+AVR"`), while a Chaos listing prefixes its candidate intent with
/// `"Chaos:"` so it never exposes the private resolved assignment union. The
/// multiplier is `draft_core`'s `MAX_PACK_COUNT`, inlined because the broker
/// deliberately carries no draft-core dependency — it validates the shape of
/// a listing, never the rules of a draft.
pub const MAX_DRAFT_SET_LABEL_LEN: usize = 6 + 8 * (MAX_DRAFT_SET_CODE_LEN + 1);
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
fn validate_optional_label(field: &str, value: Option<&str>, max: usize) -> Result<(), String> {
    match value {
        Some(value) if !value.trim().is_empty() => validate_required_label(field, value, max),
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
pub fn validate_optional_token(field: &str, value: Option<&str>, max: usize) -> Result<(), String> {
    match value {
        Some(value) => validate_token(field, value, max),
        None => Ok(()),
    }
}

pub struct CreateGameSettingsFields<'a> {
    pub display_name: &'a str,
    pub password: Option<&'a str>,
    pub timer_seconds: Option<u32>,
    pub player_count: u8,
    pub room_name: Option<&'a str>,
    pub host_peer_id: Option<&'a str>,
    pub draft_metadata: Option<&'a DraftLobbyMetadata>,
}

pub fn validate_create_game_settings_fields(
    fields: CreateGameSettingsFields<'_>,
) -> Result<(), String> {
    validate_required_label("display_name", fields.display_name, MAX_DISPLAY_NAME_LEN)?;
    validate_optional_label("room_name", fields.room_name, MAX_ROOM_NAME_LEN)?;
    validate_optional_token("password", fields.password, MAX_PASSWORD_LEN)?;
    validate_optional_token("host_peer_id", fields.host_peer_id, MAX_TOKEN_LEN)?;
    if fields.player_count == 0 || fields.player_count > MAX_PLAYER_COUNT {
        return Err(format!(
            "player_count must be between 1 and {MAX_PLAYER_COUNT}"
        ));
    }
    if let Some(secs) = fields.timer_seconds {
        if secs > MAX_TIMER_SECONDS {
            return Err(format!("timer_seconds must be at most {MAX_TIMER_SECONDS}"));
        }
    }
    if let Some(draft) = fields.draft_metadata {
        validate_token(
            "draft_metadata.set_code",
            &draft.set_code,
            MAX_DRAFT_SET_LABEL_LEN,
        )?;
        validate_token(
            "draft_metadata.draft_kind",
            &draft.draft_kind,
            MAX_DRAFT_KIND_LEN,
        )?;
        validate_optional_label(
            "draft_metadata.cube_name",
            draft.cube_name.as_deref(),
            MAX_ROOM_NAME_LEN,
        )?;
    }
    Ok(())
}

pub struct JoinGameWithPasswordFields<'a> {
    pub game_code: &'a str,
    pub display_name: &'a str,
    pub password: Option<&'a str>,
    pub reservation_token: Option<&'a str>,
}

pub fn validate_join_game_with_password_fields(
    fields: JoinGameWithPasswordFields<'_>,
) -> Result<(), String> {
    validate_token("game_code", fields.game_code, MAX_GAME_CODE_LEN)?;
    validate_required_label("display_name", fields.display_name, MAX_DISPLAY_NAME_LEN)?;
    validate_optional_token("password", fields.password, MAX_PASSWORD_LEN)?;
    validate_optional_token("reservation_token", fields.reservation_token, MAX_TOKEN_LEN)?;
    Ok(())
}

pub struct LookupJoinTargetFields<'a> {
    pub game_code: &'a str,
    pub password: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub release_reservation_token: Option<&'a str>,
}

pub fn validate_lookup_join_target_fields(
    fields: LookupJoinTargetFields<'_>,
) -> Result<(), String> {
    validate_token("game_code", fields.game_code, MAX_GAME_CODE_LEN)?;
    validate_optional_token("password", fields.password, MAX_PASSWORD_LEN)?;
    validate_optional_label("display_name", fields.display_name, MAX_DISPLAY_NAME_LEN)?;
    validate_optional_token(
        "release_reservation_token",
        fields.release_reservation_token,
        MAX_TOKEN_LEN,
    )?;
    Ok(())
}

pub struct UpdateLobbyMetadataFields<'a> {
    pub game_code: &'a str,
    pub current_players: u8,
    pub max_players: u8,
    pub consumed_reservation_tokens: &'a [String],
}

pub fn validate_update_lobby_metadata_fields(
    fields: UpdateLobbyMetadataFields<'_>,
) -> Result<(), String> {
    validate_token("game_code", fields.game_code, MAX_GAME_CODE_LEN)?;
    if fields.max_players == 0 || fields.max_players > MAX_PLAYER_COUNT {
        return Err(format!(
            "max_players must be between 1 and {MAX_PLAYER_COUNT}"
        ));
    }
    if fields.current_players > MAX_PLAYER_COUNT {
        return Err(format!(
            "current_players must be at most {MAX_PLAYER_COUNT}"
        ));
    }
    if fields.current_players > fields.max_players {
        return Err("current_players must not exceed max_players".to_string());
    }
    if fields.consumed_reservation_tokens.len() > MAX_CONSUMED_TOKENS {
        return Err(format!(
            "consumed_reservation_tokens must contain at most {MAX_CONSUMED_TOKENS} entries"
        ));
    }
    for token in fields.consumed_reservation_tokens {
        validate_token("consumed_reservation_token", token, MAX_TOKEN_LEN)?;
    }
    Ok(())
}

pub fn validate_unregister_lobby_fields(game_code: &str) -> Result<(), String> {
    validate_token("game_code", game_code, MAX_GAME_CODE_LEN)
}

// ---------------------------------------------------------------------------
// Tournament organizer
// ---------------------------------------------------------------------------
//
// Size/shape only, exactly like every function above: these bound what a
// client may *send*, never whether the request is legal for the tournament's
// state. Status gating, token authority, duplicate-join rejection and result
// legality all stay in `crate::tournament`, which is the single authority for
// them; duplicating any of that here would create a second gate to drift.

/// Max entries in a [`crate::tournament::PodOutcome::Decisive`] `game_wins`
/// map — one per pod seat.
///
/// `MatchArity::new` caps a pairing at 128 seats (the largest `n` for which
/// the MSTR win-point formula `2n - 1` still fits `u8`), so a client claiming
/// more distinct game-win entries than the largest legal pod is malformed, not
/// merely unusual. Declared here rather than imported because `MatchArity`
/// exposes no public maximum — only `HEAD_TO_HEAD` and `COMMANDER_POD` — and
/// inventing a public `MatchArity::MAX` would be a change to PR1's reviewed
/// surface for the sake of one bound. `matches_match_arity_ceiling` below pins
/// the two together so the derivation cannot silently drift.
///
/// This is a resource bound, not a rules check: `validate_match_result`
/// remains the authority on which keys a legal report may carry (at
/// head-to-head, exactly the two participants).
pub const MAX_GAME_WINS_ENTRIES: usize = 128;

pub struct CreateTournamentFields<'a> {
    pub name: &'a str,
}

pub fn validate_create_tournament_fields(fields: CreateTournamentFields<'_>) -> Result<(), String> {
    // A tournament name is a display label broadcast to every subscriber in
    // `TournamentSummary`, so it gets the same treatment as a room name.
    validate_required_label("name", fields.name, MAX_ROOM_NAME_LEN)?;
    // `total_rounds` is deliberately unbounded here beyond PR1's own
    // `Some(0)` rejection in `TournamentManager::create_tournament`. It is a
    // `u32` (no unbounded allocation to guard) and nothing loops over it: the
    // round ceiling is compared against, never counted to, so even `u32::MAX`
    // costs a comparison. A ceiling would be a tournament-policy judgment,
    // which is `crate::tournament`'s to make, not this module's.
    Ok(())
}

pub struct JoinTournamentFields<'a> {
    pub code: &'a str,
    pub player_key: &'a str,
    pub display_name: &'a str,
}

pub fn validate_join_tournament_fields(fields: JoinTournamentFields<'_>) -> Result<(), String> {
    validate_token("code", fields.code, MAX_GAME_CODE_LEN)?;
    // Client-supplied opaque identity, same treatment as `host_peer_id`.
    validate_token("player_key", fields.player_key, MAX_TOKEN_LEN)?;
    validate_required_label("display_name", fields.display_name, MAX_DISPLAY_NAME_LEN)?;
    Ok(())
}

pub fn validate_get_tournament_fields(code: &str) -> Result<(), String> {
    validate_token("code", code, MAX_GAME_CODE_LEN)
}

pub struct StartTournamentRoundFields<'a> {
    pub code: &'a str,
    pub organizer_token: &'a str,
}

pub fn validate_start_tournament_round_fields(
    fields: StartTournamentRoundFields<'_>,
) -> Result<(), String> {
    validate_token("code", fields.code, MAX_GAME_CODE_LEN)?;
    validate_token("organizer_token", fields.organizer_token, MAX_TOKEN_LEN)?;
    Ok(())
}

pub struct ReportMatchResultFields<'a> {
    pub code: &'a str,
    pub player_token: &'a str,
    pub outcome: &'a crate::tournament::PodOutcome,
}

pub fn validate_report_match_result_fields(
    fields: ReportMatchResultFields<'_>,
) -> Result<(), String> {
    validate_token("code", fields.code, MAX_GAME_CODE_LEN)?;
    validate_token("player_token", fields.player_token, MAX_TOKEN_LEN)?;
    // `PodOutcome::Draw` carries no client-supplied strings at all; only the
    // `Decisive` arm has anything to bound.
    if let crate::tournament::PodOutcome::Decisive { winner, game_wins } = fields.outcome {
        validate_token("outcome.winner", winner, MAX_TOKEN_LEN)?;
        if game_wins.len() > MAX_GAME_WINS_ENTRIES {
            return Err(format!(
                "outcome.game_wins must contain at most {MAX_GAME_WINS_ENTRIES} entries"
            ));
        }
        for player_key in game_wins.keys() {
            validate_token("outcome.game_wins key", player_key, MAX_TOKEN_LEN)?;
        }
    }
    Ok(())
}

pub struct DropFromTournamentFields<'a> {
    pub code: &'a str,
    pub player_token: &'a str,
}

pub fn validate_drop_from_tournament_fields(
    fields: DropFromTournamentFields<'_>,
) -> Result<(), String> {
    validate_token("code", fields.code, MAX_GAME_CODE_LEN)?;
    validate_token("player_token", fields.player_token, MAX_TOKEN_LEN)?;
    Ok(())
}

pub struct EndTournamentFields<'a> {
    pub code: &'a str,
    pub organizer_token: &'a str,
}

pub fn validate_end_tournament_fields(fields: EndTournamentFields<'_>) -> Result<(), String> {
    validate_token("code", fields.code, MAX_GAME_CODE_LEN)?;
    validate_token("organizer_token", fields.organizer_token, MAX_TOKEN_LEN)?;
    Ok(())
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
            validate_create_game_settings_fields(CreateGameSettingsFields {
                display_name,
                password: password.as_deref(),
                timer_seconds: *timer_seconds,
                player_count: *player_count,
                room_name: room_name.as_deref(),
                host_peer_id: host_peer_id.as_deref(),
                draft_metadata: draft_metadata.as_ref(),
            })?;
        }
        M::JoinGameWithPassword {
            game_code,
            display_name,
            password,
            reservation_token,
            ..
        } => {
            validate_join_game_with_password_fields(JoinGameWithPasswordFields {
                game_code,
                display_name,
                password: password.as_deref(),
                reservation_token: reservation_token.as_deref(),
            })?;
        }
        M::LookupJoinTarget {
            game_code,
            password,
            display_name,
            release_reservation_token,
            ..
        } => {
            validate_lookup_join_target_fields(LookupJoinTargetFields {
                game_code,
                password: password.as_deref(),
                display_name: display_name.as_deref(),
                release_reservation_token: release_reservation_token.as_deref(),
            })?;
        }
        M::UpdateLobbyMetadata {
            game_code,
            current_players,
            max_players,
            consumed_reservation_tokens,
        } => {
            validate_update_lobby_metadata_fields(UpdateLobbyMetadataFields {
                game_code,
                current_players: *current_players,
                max_players: *max_players,
                consumed_reservation_tokens,
            })?;
        }
        M::UnregisterLobby { game_code } => {
            validate_unregister_lobby_fields(game_code)?;
        }
        M::CreateTournament { name, .. } => {
            validate_create_tournament_fields(CreateTournamentFields { name })?;
        }
        M::JoinTournament {
            code,
            player_key,
            display_name,
        } => {
            validate_join_tournament_fields(JoinTournamentFields {
                code,
                player_key,
                display_name,
            })?;
        }
        M::GetTournament { code } => {
            validate_get_tournament_fields(code)?;
        }
        // `request_id: _` on all four: the correlator is an opaque integer the
        // broker echoes and never stores, so it has no bounds to check. Binding
        // it explicitly is what shows the next reader it was considered here
        // rather than overlooked.
        //
        // `ReportMatchResult` gives up its `..` rest pattern to say that: a
        // rest pattern silently absorbs every future field, so this arm was the
        // one place a newly-added bounded field could slip past validation
        // without a compile error. Naming all five fields makes the next
        // addition break here, which is the behavior the other three arms
        // already had for free.
        M::StartTournamentRound {
            code,
            organizer_token,
            request_id: _,
        } => {
            validate_start_tournament_round_fields(StartTournamentRoundFields {
                code,
                organizer_token,
            })?;
        }
        M::ReportMatchResult {
            code,
            pairing_id: _,
            player_token,
            outcome,
            request_id: _,
        } => {
            validate_report_match_result_fields(ReportMatchResultFields {
                code,
                player_token,
                outcome,
            })?;
        }
        M::DropFromTournament {
            code,
            player_token,
            request_id: _,
        } => {
            validate_drop_from_tournament_fields(DropFromTournamentFields { code, player_token })?;
        }
        M::EndTournament {
            code,
            organizer_token,
            request_id: _,
        } => {
            validate_end_tournament_fields(EndTournamentFields {
                code,
                organizer_token,
            })?;
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
        guard_inbound, MAX_DECK_CARD_NAME_LEN, MAX_MAIN_DECK_ENTRIES, MAX_PLANAR_DECK_ENTRIES,
        MAX_SCHEME_DECK_ENTRIES, MAX_SIDEBOARD_ENTRIES,
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
        assert!(validate_optional_label("f", None, MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", Some(""), MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", Some("  "), MAX_ROOM_NAME_LEN).is_ok());
        assert!(validate_optional_label("f", Some("Room"), MAX_ROOM_NAME_LEN).is_ok());
        let long = "r".repeat(41);
        assert!(validate_optional_label("f", Some(&long), MAX_ROOM_NAME_LEN).is_err());
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
                set_code: "a".repeat(MAX_DRAFT_SET_LABEL_LEN + 1),
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

    #[test]
    fn guard_rejects_oversized_planar_deck() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { deck, .. } = &mut msg {
            deck.planar_deck = vec!["Plane".to_string(); MAX_PLANAR_DECK_ENTRIES + 1];
        }
        let err = guard_inbound(&msg).unwrap_err();
        assert!(err.contains("planar_deck"));
    }

    #[test]
    fn guard_rejects_invalid_planar_deck_card_name() {
        let mut msg = join_with("Bob");
        if let M::JoinGameWithPassword { deck, .. } = &mut msg {
            deck.planar_deck = vec!["Bad\u{0007}Plane".to_string()];
        }
        let err = guard_inbound(&msg).unwrap_err();
        assert!(err.contains("planar_deck[0]"));
    }

    #[test]
    fn guard_rejects_oversized_scheme_deck() {
        let mut msg = create_with("Alice");
        if let M::CreateGameWithSettings { deck, .. } = &mut msg {
            deck.scheme_deck = vec!["Scheme".to_string(); MAX_SCHEME_DECK_ENTRIES + 1];
        }
        let err = guard_inbound(&msg).unwrap_err();
        assert!(err.contains("scheme_deck"));
    }

    #[test]
    fn guard_rejects_invalid_scheme_deck_card_name() {
        let mut msg = join_with("Bob");
        if let M::JoinGameWithPassword { deck, .. } = &mut msg {
            deck.scheme_deck = vec!["Bad\u{0007}Scheme".to_string()];
        }
        let err = guard_inbound(&msg).unwrap_err();
        assert!(err.contains("scheme_deck[0]"));
    }

    // -- Tournament organizer ----------------------------------------------

    use crate::tournament::{BracketShape, MatchArity, PodOutcome, ScoringPolicy};
    use std::collections::HashMap;

    fn create_tournament_with(name: &str) -> M {
        M::CreateTournament {
            name: name.to_string(),
            arity: MatchArity::HEAD_TO_HEAD,
            scoring: ScoringPolicy::default(),
            bracket: BracketShape::Swiss,
            total_rounds: None,
        }
    }

    fn join_tournament_with(code: &str, player_key: &str, display_name: &str) -> M {
        M::JoinTournament {
            code: code.to_string(),
            player_key: player_key.to_string(),
            display_name: display_name.to_string(),
        }
    }

    /// Every fixture in this module is UNCORRELATED (`request_id: None`).
    /// That is the honest shape here: validation is a field-bounds verdict and
    /// the correlator is opaque to it, so a correlated frame is validated
    /// identically and an uncorrelated literal keeps these tests measuring
    /// bounds rather than correlation.
    fn report_with(code: &str, player_token: &str, outcome: PodOutcome) -> M {
        M::ReportMatchResult {
            code: code.to_string(),
            pairing_id: 0,
            player_token: player_token.to_string(),
            outcome,
            request_id: None,
        }
    }

    fn decisive(winner: &str, game_wins: HashMap<String, u8>) -> PodOutcome {
        PodOutcome::Decisive {
            winner: winner.to_string(),
            game_wins,
        }
    }

    /// The bound's own derivation, pinned. `MatchArity` caps a pairing at 128
    /// seats; if that ceiling ever moves, this assertion fails rather than
    /// letting `MAX_GAME_WINS_ENTRIES` silently mean something else.
    #[test]
    fn max_game_wins_entries_matches_match_arity_ceiling() {
        assert!(MatchArity::new(128).is_ok());
        assert!(MatchArity::new(129).is_err());
        assert_eq!(MAX_GAME_WINS_ENTRIES, 128);
    }

    /// Acceptance half of Verification Matrix row 12 — one valid message per
    /// new variant. Without this, every rejection test below could be
    /// satisfied by a function that rejected everything.
    #[test]
    fn tournament_messages_accept_valid() {
        let valid = [
            create_tournament_with("Friday Night"),
            join_tournament_with("TOUR01", "key-a", "Alice"),
            M::GetTournament {
                code: "TOUR01".into(),
            },
            M::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: "tok".into(),
                request_id: None,
            },
            report_with(
                "TOUR01",
                "tok",
                decisive("key-a", [("key-a".to_string(), 2u8)].into_iter().collect()),
            ),
            report_with("TOUR01", "tok", PodOutcome::Draw),
            M::DropFromTournament {
                code: "TOUR01".into(),
                player_token: "tok".into(),
                request_id: None,
            },
            M::EndTournament {
                code: "TOUR01".into(),
                organizer_token: "tok".into(),
                request_id: None,
            },
        ];
        for msg in valid {
            assert!(
                validate_lobby_message(&msg).is_ok(),
                "valid message rejected: {msg:?}"
            );
        }
    }

    #[test]
    fn create_tournament_rejects_oversized_name() {
        let msg = create_tournament_with(&"a".repeat(MAX_ROOM_NAME_LEN + 1));
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn create_tournament_rejects_blank_name() {
        assert!(validate_lobby_message(&create_tournament_with("   ")).is_err());
    }

    #[test]
    fn join_tournament_rejects_oversized_code() {
        let msg = join_tournament_with(&"c".repeat(MAX_GAME_CODE_LEN + 1), "key-a", "Alice");
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn join_tournament_rejects_oversized_player_key() {
        let msg = join_tournament_with("TOUR01", &"k".repeat(MAX_TOKEN_LEN + 1), "Alice");
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn join_tournament_rejects_oversized_display_name() {
        let msg = join_tournament_with("TOUR01", "key-a", &"a".repeat(MAX_DISPLAY_NAME_LEN + 1));
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn get_tournament_rejects_oversized_code() {
        let msg = M::GetTournament {
            code: "c".repeat(MAX_GAME_CODE_LEN + 1),
        };
        assert!(validate_lobby_message(&msg).is_err());
    }

    #[test]
    fn organizer_gated_messages_reject_oversized_organizer_token() {
        let long = "t".repeat(MAX_TOKEN_LEN + 1);
        for msg in [
            M::StartTournamentRound {
                code: "TOUR01".into(),
                organizer_token: long.clone(),
                request_id: None,
            },
            M::EndTournament {
                code: "TOUR01".into(),
                organizer_token: long.clone(),
                request_id: None,
            },
        ] {
            assert!(validate_lobby_message(&msg).is_err(), "{msg:?}");
        }
    }

    #[test]
    fn player_gated_messages_reject_oversized_player_token() {
        let long = "t".repeat(MAX_TOKEN_LEN + 1);
        for msg in [
            report_with("TOUR01", &long, PodOutcome::Draw),
            M::DropFromTournament {
                code: "TOUR01".into(),
                player_token: long.clone(),
                request_id: None,
            },
        ] {
            assert!(validate_lobby_message(&msg).is_err(), "{msg:?}");
        }
    }

    #[test]
    fn report_match_result_rejects_oversized_winner() {
        let msg = report_with(
            "TOUR01",
            "tok",
            decisive(&"w".repeat(MAX_TOKEN_LEN + 1), HashMap::new()),
        );
        assert!(validate_lobby_message(&msg).is_err());
    }

    /// The collection-bound hostile fixture, mirroring
    /// `update_metadata_rejects_too_many_tokens` for a different oversized
    /// collection: more distinct `game_wins` entries than the largest legal
    /// pod could ever have seats.
    #[test]
    fn report_match_result_rejects_oversized_game_wins_map() {
        let game_wins: HashMap<String, u8> = (0..=MAX_GAME_WINS_ENTRIES)
            .map(|i| (format!("key-{i}"), 1u8))
            .collect();
        assert_eq!(game_wins.len(), MAX_GAME_WINS_ENTRIES + 1);
        let msg = report_with("TOUR01", "tok", decisive("key-0", game_wins));
        let err = validate_lobby_message(&msg).unwrap_err();
        assert!(err.contains("game_wins"), "unexpected reason: {err}");

        // Exactly at the ceiling is accepted (off-by-one guard).
        let at_limit: HashMap<String, u8> = (0..MAX_GAME_WINS_ENTRIES)
            .map(|i| (format!("key-{i}"), 1u8))
            .collect();
        assert!(
            validate_lobby_message(&report_with("TOUR01", "tok", decisive("key-0", at_limit)))
                .is_ok()
        );
    }

    #[test]
    fn report_match_result_rejects_oversized_game_wins_key() {
        let game_wins: HashMap<String, u8> =
            [("k".repeat(MAX_TOKEN_LEN + 1), 1u8)].into_iter().collect();
        let msg = report_with("TOUR01", "tok", decisive("key-a", game_wins));
        let err = validate_lobby_message(&msg).unwrap_err();
        assert!(err.contains("game_wins key"), "unexpected reason: {err}");
    }

    /// Control characters are rejected on tournament fields too — the same
    /// primitive every other variant uses, not a parallel check.
    #[test]
    fn tournament_fields_reject_control_characters() {
        assert!(validate_lobby_message(&create_tournament_with("Bad\u{0007}Name")).is_err());
        assert!(
            validate_lobby_message(&join_tournament_with("TOUR01", "key\u{0007}a", "Alice"))
                .is_err()
        );
        assert!(validate_lobby_message(&M::GetTournament {
            code: "TOUR\n01".into()
        })
        .is_err());
    }
}
