//! Wire validation for spectator request frames.
//!
//! `SpectatorJoin` and `SpectateDraft` are handled directly in `phase-server`
//! and use client-provided game/draft codes for map lookups and identity state.

use lobby_broker::validation::{validate_token, MAX_GAME_CODE_LEN};

/// Per-game live spectator cap. This bounds fan-out sender storage and the
/// repeated full-state snapshot work performed on SpectatorJoin.
pub const MAX_GAME_SPECTATORS_PER_GAME: usize = 32;

/// Per-draft live spectator cap. This bounds broadcast sender storage and the
/// repeated draft-view snapshot work performed on SpectateDraft.
pub const MAX_DRAFT_SPECTATORS_PER_DRAFT: usize = 32;

/// Validate a game spectator join request before session lookup.
pub fn guard_spectator_join(game_code: &str) -> Result<(), String> {
    validate_token("game_code", game_code, MAX_GAME_CODE_LEN)
}

/// Validate per-game live spectator capacity before registering a sender.
pub fn guard_game_spectator_capacity(current: usize) -> Result<(), String> {
    if current >= MAX_GAME_SPECTATORS_PER_GAME {
        Err(format!(
            "Too many spectators for game: maximum is {MAX_GAME_SPECTATORS_PER_GAME}"
        ))
    } else {
        Ok(())
    }
}

/// Validate per-draft live spectator capacity before registering a sender.
pub fn guard_draft_spectator_capacity(current: usize) -> Result<(), String> {
    if current >= MAX_DRAFT_SPECTATORS_PER_DRAFT {
        Err(format!(
            "Too many spectators for draft: maximum is {MAX_DRAFT_SPECTATORS_PER_DRAFT}"
        ))
    } else {
        Ok(())
    }
}

/// Validate a draft spectator join request before draft/session lookup.
pub fn guard_spectate_draft(draft_code: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)
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
    fn game_spectator_capacity_accepts_slot_below_cap() {
        assert!(guard_game_spectator_capacity(MAX_GAME_SPECTATORS_PER_GAME - 1).is_ok());
    }

    #[test]
    fn game_spectator_capacity_rejects_slot_at_cap() {
        let err = guard_game_spectator_capacity(MAX_GAME_SPECTATORS_PER_GAME).unwrap_err();
        assert!(err.contains("maximum"));
        assert!(err.contains(&MAX_GAME_SPECTATORS_PER_GAME.to_string()));
    }

    #[test]
    fn draft_spectator_capacity_accepts_slot_below_cap() {
        assert!(guard_draft_spectator_capacity(MAX_DRAFT_SPECTATORS_PER_DRAFT - 1).is_ok());
    }

    #[test]
    fn draft_spectator_capacity_rejects_slot_at_cap() {
        let err = guard_draft_spectator_capacity(MAX_DRAFT_SPECTATORS_PER_DRAFT).unwrap_err();
        assert!(err.contains("maximum"));
        assert!(err.contains(&MAX_DRAFT_SPECTATORS_PER_DRAFT.to_string()));
    }

    #[test]
    fn spectate_draft_rejects_oversized_code() {
        let err = guard_spectate_draft(&"x".repeat(MAX_GAME_CODE_LEN + 1)).unwrap_err();
        assert!(err.contains("draft_code"));
    }
}
