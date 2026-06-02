//! Wire validation for spectator request frames.
//!
//! `SpectatorJoin` and `SpectateDraft` are handled directly in `phase-server`
//! and use client-provided game/draft codes for map lookups and identity state.

use lobby_broker::validation::{validate_token, MAX_GAME_CODE_LEN};

/// Validate a game spectator join request before session lookup.
pub fn guard_spectator_join(game_code: &str) -> Result<(), String> {
    validate_token("game_code", game_code, MAX_GAME_CODE_LEN)
}

/// Validate a draft spectator join request before draft/session lookup.
pub fn guard_spectate_draft(draft_code: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)
}
