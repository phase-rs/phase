//! Wire validation for `DraftAction` frames in `phase-server`.
//!
//! Draft create/join/reconnect validate codes via `draft_wire_guard`, but
//! `DraftAction` is handled separately and previously accepted unbounded
//! `draft_code` strings before session lookup and action dispatch.

use lobby_broker::validation::{validate_token, MAX_GAME_CODE_LEN};

/// Validate `DraftAction` wire fields before draft session mutation.
pub fn guard_draft_action(draft_code: &str) -> Result<(), String> {
    validate_token("draft_code", draft_code, MAX_GAME_CODE_LEN)
}
