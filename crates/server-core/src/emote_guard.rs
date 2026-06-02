//! Wire validation for in-game emote broadcasts.
//!
//! Emotes are cloned and fan-out to every connected player in the game. Without
//! bounds, a client can amplify memory use across all peers with a multi-kilobyte
//! payload on each send.

/// Max emote length, in characters. Client presets are short phrases; this
/// leaves headroom for custom text while rejecting junk frames.
pub const MAX_EMOTE_LEN: usize = 64;

fn has_control_char(value: &str) -> bool {
    value.chars().any(|c| c.is_control())
}

/// Validate an emote string before broadcast to other players.
pub fn guard_emote(emote: &str) -> Result<(), String> {
    if emote.trim().is_empty() {
        return Err("emote must not be empty".to_string());
    }
    if emote.chars().count() > MAX_EMOTE_LEN {
        return Err(format!("emote must be at most {MAX_EMOTE_LEN} characters"));
    }
    if has_control_char(emote) {
        return Err("emote must not contain control characters".to_string());
    }
    Ok(())
}
