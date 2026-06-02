//! Wire validation for spectator request frames.
//!
//! `SpectatorJoin` and `SpectateDraft` are handled directly in `phase-server`
//! and use client-provided game/draft codes for map lookups and identity state.

use lobby_broker::validation::{validate_token, MAX_GAME_CODE_LEN};

/// Max live spectator connections per game or draft pod. Generous for real
/// audiences; rejects unbounded fan-out list growth from repeated joins.
pub const MAX_SPECTATORS_PER_SESSION: usize = 64;

/// Validate a game spectator join request before session lookup.
pub fn guard_spectator_join(game_code: &str) -> Result<(), String> {
    validate_token("game_code", game_code, MAX_GAME_CODE_LEN)
}

/// Validate a draft spectator join request before draft/session lookup.
pub fn guard_spectate_draft(draft_code: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)
}

/// Reject spectator registration when the session already has the maximum
/// number of connected spectators.
pub fn guard_spectator_capacity(scope: &str, current: usize) -> Result<(), String> {
    if current >= MAX_SPECTATORS_PER_SESSION {
        return Err(format!(
            "{scope} already has the maximum number of spectators ({MAX_SPECTATORS_PER_SESSION})"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectator_join_accepts_valid_code() {
        assert!(guard_spectator_join("ABC123").is_ok());
    }

    #[test]
    fn spectator_join_rejects_oversized_code() {
        let err = guard_spectator_join(&"x".repeat(MAX_GAME_CODE_LEN + 1)).unwrap_err();
        assert!(err.contains("game_code"));
    }

    #[test]
    fn spectate_draft_rejects_oversized_code() {
        let err = guard_spectate_draft(&"x".repeat(MAX_GAME_CODE_LEN + 1)).unwrap_err();
        assert!(err.contains("draft_code"));
    }

    #[test]
    fn spectator_capacity_accepts_below_limit() {
        assert!(guard_spectator_capacity("game", MAX_SPECTATORS_PER_SESSION - 1).is_ok());
    }

    #[test]
    fn spectator_capacity_rejects_at_limit() {
        let err = guard_spectator_capacity("game", MAX_SPECTATORS_PER_SESSION).unwrap_err();
        assert!(err.contains("maximum"));
    }
}
