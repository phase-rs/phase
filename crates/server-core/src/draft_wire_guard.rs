//! Wire validation for draft session handlers in `phase-server`.
//!
//! Draft create/join/reconnect/action frames are `ClientMessage` variants handled
//! directly by the server shell. Unlike lobby game frames, they never pass
//! through `lobby_broker::validate_lobby_message`, so client-supplied names,
//! codes, passwords, and tokens must be bounded before clone-heavy work.

use draft_core::types::{DraftKind, TournamentFormat, MAX_PACK_COUNT};
use lobby_broker::validation::{
    validate_optional_token, validate_required_label, validate_token, MAX_DISPLAY_NAME_LEN,
    MAX_DRAFT_SET_CODE_LEN, MAX_GAME_CODE_LEN, MAX_PASSWORD_LEN, MAX_TIMER_SECONDS, MAX_TOKEN_LEN,
};

/// Validate `CreateDraftWithSettings` wire fields before pool lookup and lobby
/// registration.
pub fn guard_create_draft_with_settings(
    display_name: &str,
    set_codes: &[String],
    password: &Option<String>,
    timer_seconds: Option<u32>,
    pod_size: u8,
    kind: DraftKind,
    tournament_format: TournamentFormat,
) -> Result<(), String> {
    validate_required_label("display_name", display_name, MAX_DISPLAY_NAME_LEN)?;
    // Bound the source's set tokens before any pool lookup: each code costs a
    // map probe and a pool clone on the accept path. Chaos carries candidates
    // here, never the server-owned assignment matrix.
    if !(1..=usize::from(MAX_PACK_COUNT)).contains(&set_codes.len()) {
        return Err(format!(
            "set_codes must name between 1 and {MAX_PACK_COUNT} packs"
        ));
    }
    for code in set_codes {
        validate_token("set_code", code, MAX_DRAFT_SET_CODE_LEN)?;
    }
    validate_optional_token("password", password.as_deref(), MAX_PASSWORD_LEN)?;
    let procedure = kind.procedure();
    if !procedure.allows_pod_size(tournament_format, pod_size) {
        let allowed = procedure.allowed_pod_size_range(tournament_format);
        return Err(format!(
            "pod_size must be between {} and {}",
            allowed.start(),
            allowed.end(),
        ));
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
    use draft_core::types::{DraftKind, TournamentFormat, MAX_PACK_COUNT};
    use lobby_broker::validation::{MAX_DRAFT_SET_CODE_LEN, MAX_GAME_CODE_LEN};

    use super::{
        guard_create_draft_with_settings, guard_draft_action, guard_join_draft_with_password,
        guard_reconnect_draft,
    };

    #[test]
    fn create_draft_accepts_valid_fields() {
        assert!(guard_create_draft_with_settings(
            "Alice",
            &["TST".to_string()],
            &None,
            None,
            4,
            DraftKind::Premier,
            TournamentFormat::Swiss,
        )
        .is_ok());
    }

    /// CR 903.13a + CR 800.1: a Commander pod's floor is three seats — the
    /// smallest pod that can still deliver the multiplayer game the format is
    /// defined as — so the four-seat product default must pass the wire guard.
    ///
    /// This is the REACH-GUARD half of the pod-floor claim: the guard reads
    /// `kind.procedure().min_pod_size`, so this test fails if that derivation
    /// is ever replaced by a literal 8 (the four older kinds' pod size).
    #[test]
    fn wire_guard_admits_four_seat_commander_pod() {
        assert!(guard_create_draft_with_settings(
            "Alice",
            &["CMM".to_string()],
            &None,
            None,
            4,
            DraftKind::CommanderDraft,
            TournamentFormat::Swiss,
        )
        .is_ok());
    }

    /// The paired negative: two seats is not a multiplayer game (CR 800.1), so
    /// the floor must refuse it — and must say so by naming `pod_size`, not by
    /// failing on some unrelated field.
    #[test]
    fn wire_guard_refuses_two_seat_commander_pod() {
        let err = guard_create_draft_with_settings(
            "Alice",
            &["CMM".to_string()],
            &None,
            None,
            2,
            DraftKind::CommanderDraft,
            TournamentFormat::Swiss,
        )
        .unwrap_err();
        assert!(err.contains("pod_size"), "unexpected rejection: {err}");
        assert!(
            err.contains('3'),
            "the floor must name the CR 800.1 minimum: {err}"
        );
    }

    #[test]
    fn wire_guard_keeps_remote_quick_drafts_inside_the_public_range() {
        for pod_size in [1, 9] {
            let err = guard_create_draft_with_settings(
                "Alice",
                &["TST".to_string()],
                &None,
                None,
                pod_size,
                DraftKind::Quick,
                TournamentFormat::Swiss,
            )
            .expect_err("remote Quick Draft must use the public 2..=8 range");
            assert!(err.contains("pod_size"), "unexpected rejection: {err}");
            assert!(
                err.contains('2') && err.contains('8'),
                "unexpected range: {err}"
            );
        }
    }

    /// The multi-set claim at the guard: an ordered, repeating sequence is a
    /// legal pod pool, so the guard must admit one rather than only the
    /// single-code shape it was written for.
    #[test]
    fn create_draft_accepts_an_ordered_multi_set_sequence() {
        assert!(guard_create_draft_with_settings(
            "Alice",
            &["ISD".to_string(), "DKA".to_string(), "ISD".to_string(),],
            &None,
            None,
            8,
            DraftKind::Premier,
            TournamentFormat::Swiss,
        )
        .is_ok());
    }

    /// Both ends of the sequence bound, checked BEFORE any pool lookup: an
    /// empty sequence names no booster at all, and an unbounded one would cost
    /// a map probe and a pool clone per entry on the accept path.
    #[test]
    fn create_draft_rejects_an_empty_or_oversized_sequence() {
        for codes in [
            Vec::new(),
            vec!["TST".to_string(); usize::from(MAX_PACK_COUNT) + 1],
        ] {
            let err = guard_create_draft_with_settings(
                "Alice",
                &codes,
                &None,
                None,
                8,
                DraftKind::Premier,
                TournamentFormat::Swiss,
            )
            .unwrap_err();
            assert!(err.contains("set_codes"), "unexpected rejection: {err}");
        }
    }

    /// Every entry is bounded, not just the first — a sequence whose LAST code
    /// is junk must be refused as loudly as one whose first is.
    #[test]
    fn create_draft_rejects_an_oversized_code_anywhere_in_the_sequence() {
        let err = guard_create_draft_with_settings(
            "Alice",
            &["TST".to_string(), "z".repeat(MAX_DRAFT_SET_CODE_LEN + 1)],
            &None,
            None,
            8,
            DraftKind::Premier,
            TournamentFormat::Swiss,
        )
        .unwrap_err();
        assert!(err.contains("set_code"), "unexpected rejection: {err}");
    }

    #[test]
    fn create_draft_validates_chaos_candidates_before_pool_lookup() {
        let accepted = ["AAA".to_string(), "BBB".to_string()];
        assert!(guard_create_draft_with_settings(
            "Alice",
            &accepted,
            &None,
            None,
            4,
            DraftKind::Premier,
            TournamentFormat::Swiss,
        )
        .is_ok());

        let rejected = ["A".repeat(MAX_DRAFT_SET_CODE_LEN + 1)];
        assert!(guard_create_draft_with_settings(
            "Alice",
            &rejected,
            &None,
            None,
            4,
            DraftKind::Premier,
            TournamentFormat::Swiss,
        )
        .is_err());
    }

    #[test]
    fn create_draft_rejects_oversized_display_name() {
        let err = guard_create_draft_with_settings(
            &"a".repeat(21),
            &["TST".to_string()],
            &None,
            None,
            4,
            DraftKind::Premier,
            TournamentFormat::Swiss,
        )
        .unwrap_err();
        assert!(err.contains("display_name"));
    }

    #[test]
    fn wire_guard_uses_the_procedure_for_ceiling_and_pairing_bracket() {
        assert!(guard_create_draft_with_settings(
            "Alice",
            &["CMM".to_string()],
            &None,
            None,
            3,
            DraftKind::CommanderDraft,
            TournamentFormat::SingleElimination,
        )
        .is_ok());

        for (pod_size, tournament_format) in [
            (9, TournamentFormat::Swiss),
            (7, TournamentFormat::SingleElimination),
        ] {
            let err = guard_create_draft_with_settings(
                "Alice",
                &["TST".to_string()],
                &None,
                None,
                pod_size,
                DraftKind::Premier,
                tournament_format,
            )
            .unwrap_err();
            assert!(err.contains("pod_size"), "unexpected rejection: {err}");
        }

        assert!(guard_create_draft_with_settings(
            "Alice",
            &["TST".to_string()],
            &None,
            None,
            1,
            DraftKind::Quick,
            TournamentFormat::Swiss,
        )
        .is_err());
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
