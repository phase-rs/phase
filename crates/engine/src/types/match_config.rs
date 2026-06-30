use serde::{Deserialize, Serialize};

use super::game_state::LoopDetectionMode;
use super::player::PlayerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MatchType {
    #[default]
    Bo1,
    Bo3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MatchPhase {
    #[default]
    InGame,
    BetweenGames,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MatchConfig {
    pub match_type: MatchType,
    /// CR 732.2a: combo (infinite-loop) detector opt-in for this match. Chosen at
    /// match creation, immutable during play, whole-table by construction. Projected
    /// onto `GameState::loop_detection` by [`GameState::set_match_config`]. Default
    /// `Off` = exact pre-detector behavior (opt-in invariant, issue #4603).
    ///
    /// Available at every player count (infinite loops are a multiplayer/Commander
    /// staple); unlike `match_type` (Bo3 is inherently 2-player), this carries through
    /// for 3- and 4-player tables. CR 732.5 (no player can be forced to break a loop)
    /// is player-count-agnostic.
    ///
    /// `skip_serializing_if` keeps existing serialized `MatchConfig` bytes unchanged
    /// when the detector is `Off` (mirrors the `unbounded_resources` elision).
    #[serde(default, skip_serializing_if = "LoopDetectionMode::is_off")]
    pub loop_detection: LoopDetectionMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MatchScore {
    pub p0_wins: u8,
    pub p1_wins: u8,
    pub draws: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckCardCount {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BetweenGamesPrompt {
    pub game_number: u8,
    pub score: MatchScore,
    pub player: PlayerId,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 732.2a opt-in byte-stability: a default `MatchConfig` (detector Off) serializes
    /// WITHOUT a `loop_detection` key, so existing serialized configs are byte-unchanged;
    /// a pre-feature config (only `match_type`) deserializes with the detector Off; and an
    /// opted-in config round-trips. This is the #4603 opt-in invariant at the wire layer.
    ///
    /// REVERT-FAIL: drop `skip_serializing_if` ⇒ the `is_none()` assertion flips (the key
    /// is emitted); drop `#[serde(default)]` ⇒ the pre-feature deserialize errors.
    #[test]
    fn match_config_loop_detection_serde_byte_stable() {
        // Default (Off) elides the key — byte-identical to pre-feature configs.
        let json = serde_json::to_value(MatchConfig::default()).expect("serialize default");
        assert!(
            json.get("loop_detection").is_none(),
            "default MatchConfig must elide loop_detection (existing wire bytes unchanged): {json}"
        );

        // A pre-feature config (no loop_detection key) loads with the detector Off.
        let restored: MatchConfig = serde_json::from_str(r#"{"match_type":"Bo3"}"#)
            .expect("deserialize pre-feature config");
        assert_eq!(restored.match_type, MatchType::Bo3);
        assert_eq!(restored.loop_detection, LoopDetectionMode::Off);

        // An opted-in config round-trips and DOES emit the key.
        let on = MatchConfig {
            match_type: MatchType::Bo1,
            loop_detection: LoopDetectionMode::On,
        };
        let on_json = serde_json::to_value(on).expect("serialize On");
        assert!(
            on_json.get("loop_detection").is_some(),
            "an opted-in config must serialize the loop_detection key: {on_json}"
        );
        let round: MatchConfig = serde_json::from_value(on_json).expect("deserialize On");
        assert_eq!(round.loop_detection, LoopDetectionMode::On);
    }
}
