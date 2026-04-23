use serde::{Deserialize, Serialize};

use crate::database::legality::LegalityFormat;

/// Supported game formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameFormat {
    Standard,
    Commander,
    Pioneer,
    Historic,
    Pauper,
    Brawl,
    HistoricBrawl,
    FreeForAll,
    TwoHeadedGiant,
}

/// CR 100.4 / CR 100.4a: Per-format sideboard rules.
///
/// - `Forbidden`: the format does not have a sideboard at all (Commander, Brawl,
///   Historic Brawl). Semantically distinct from `Limited(0)` — those formats
///   don't "have" a zero-size sideboard, they have no sideboard concept.
/// - `Limited(n)`: constructed formats cap the sideboard at `n` cards.
///   CR 100.4a sets this at 15 for standard constructed play.
/// - `Unlimited`: casual multiplayer variants (Free-for-All, Two-Headed Giant)
///   impose no size constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SideboardPolicy {
    Forbidden,
    Limited(u32),
    Unlimited,
}

/// Configuration for a game format, describing player counts, starting life, deck rules, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatConfig {
    pub format: GameFormat,
    pub starting_life: i32,
    pub min_players: u8,
    pub max_players: u8,
    pub deck_size: u16,
    pub singleton: bool,
    pub command_zone: bool,
    pub commander_damage_threshold: Option<u8>,
    pub range_of_influence: Option<u8>,
    pub team_based: bool,
}

impl GameFormat {
    /// Maps a playable game format to its corresponding legality format for card pool validation.
    /// Returns `None` for formats that don't restrict card pools (FreeForAll, TwoHeadedGiant).
    pub fn legality_format(self) -> Option<LegalityFormat> {
        match self {
            GameFormat::Standard => Some(LegalityFormat::Standard),
            GameFormat::Commander => Some(LegalityFormat::Commander),
            GameFormat::Pioneer => Some(LegalityFormat::Pioneer),
            GameFormat::Historic => Some(LegalityFormat::Historic),
            GameFormat::Pauper => Some(LegalityFormat::Pauper),
            GameFormat::Brawl => Some(LegalityFormat::StandardBrawl),
            GameFormat::HistoricBrawl => Some(LegalityFormat::Brawl),
            GameFormat::FreeForAll | GameFormat::TwoHeadedGiant => None,
        }
    }

    /// CR 100.4a: Per-format sideboard policy.
    ///
    /// Returns `Forbidden` for Commander/Brawl/Historic Brawl (no sideboard),
    /// `Limited(15)` for constructed formats, and `Unlimited` for casual
    /// multiplayer variants that impose no size cap.
    pub fn sideboard_policy(self) -> SideboardPolicy {
        match self {
            GameFormat::Standard
            | GameFormat::Pioneer
            | GameFormat::Historic
            | GameFormat::Pauper => SideboardPolicy::Limited(15),
            GameFormat::Commander | GameFormat::Brawl | GameFormat::HistoricBrawl => {
                SideboardPolicy::Forbidden
            }
            GameFormat::FreeForAll | GameFormat::TwoHeadedGiant => SideboardPolicy::Unlimited,
        }
    }

    /// Display label for validation error messages (e.g., "Not Pioneer legal").
    pub fn label(self) -> &'static str {
        match self {
            GameFormat::Standard => "Standard",
            GameFormat::Commander => "Commander",
            GameFormat::Pioneer => "Pioneer",
            GameFormat::Historic => "Historic",
            GameFormat::Pauper => "Pauper",
            GameFormat::Brawl => "Brawl",
            GameFormat::HistoricBrawl => "Historic Brawl",
            GameFormat::FreeForAll => "Free-for-All",
            GameFormat::TwoHeadedGiant => "Two-Headed Giant",
        }
    }
}

impl FormatConfig {
    pub fn standard() -> Self {
        FormatConfig {
            format: GameFormat::Standard,
            starting_life: 20,
            min_players: 2,
            max_players: 2,
            deck_size: 60,
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
        }
    }

    pub fn commander() -> Self {
        FormatConfig {
            format: GameFormat::Commander,
            starting_life: 40,
            min_players: 2,
            max_players: 6,
            deck_size: 100,
            singleton: true,
            command_zone: true,
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
        }
    }

    pub fn pioneer() -> Self {
        FormatConfig {
            format: GameFormat::Pioneer,
            ..Self::standard()
        }
    }

    /// Historic: non-rotating constructed using the Arena Historic card pool.
    pub fn historic() -> Self {
        FormatConfig {
            format: GameFormat::Historic,
            ..Self::standard()
        }
    }

    pub fn pauper() -> Self {
        FormatConfig {
            format: GameFormat::Pauper,
            ..Self::standard()
        }
    }

    /// Brawl: 60-card singleton with a commander, 25 starting life.
    /// Uses Standard-legal card pool (CR 903 variant for Brawl).
    pub fn brawl() -> Self {
        FormatConfig {
            format: GameFormat::Brawl,
            starting_life: 25,
            min_players: 2,
            max_players: 2,
            deck_size: 60,
            singleton: true,
            command_zone: true,
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
        }
    }

    /// Historic Brawl: same rules as Brawl but with the broader Historic card pool.
    pub fn historic_brawl() -> Self {
        FormatConfig {
            format: GameFormat::HistoricBrawl,
            ..Self::brawl()
        }
    }

    pub fn free_for_all() -> Self {
        FormatConfig {
            format: GameFormat::FreeForAll,
            starting_life: 20,
            min_players: 2,
            max_players: 6,
            deck_size: 60,
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
        }
    }

    pub fn two_headed_giant() -> Self {
        FormatConfig {
            format: GameFormat::TwoHeadedGiant,
            starting_life: 30,
            min_players: 4,
            max_players: 4,
            deck_size: 60,
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: true,
        }
    }

    /// Default `FormatConfig` for a given `GameFormat`. Used by callers that
    /// only retain the format enum (e.g. the lobby broker) and need a full
    /// config to hand back to clients for deck-legality UX. Customizations a
    /// host may have applied on top of the default (e.g. non-standard player
    /// counts for Commander) are intentionally not recovered — guests use
    /// this purely to filter their local deck picker, and the host's own
    /// FormatConfig remains authoritative once the P2P session is established.
    pub fn for_format(format: GameFormat) -> Self {
        match format {
            GameFormat::Standard => Self::standard(),
            GameFormat::Commander => Self::commander(),
            GameFormat::Pioneer => Self::pioneer(),
            GameFormat::Historic => Self::historic(),
            GameFormat::Pauper => Self::pauper(),
            GameFormat::Brawl => Self::brawl(),
            GameFormat::HistoricBrawl => Self::historic_brawl(),
            GameFormat::FreeForAll => Self::free_for_all(),
            GameFormat::TwoHeadedGiant => Self::two_headed_giant(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_config_standard() {
        let config = FormatConfig::standard();
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, 60);
        assert!(!config.singleton);
        assert!(!config.command_zone);
        assert_eq!(config.commander_damage_threshold, None);
        assert!(!config.team_based);
    }

    #[test]
    fn format_config_commander() {
        let config = FormatConfig::commander();
        assert_eq!(config.starting_life, 40);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 6);
        assert_eq!(config.deck_size, 100);
        assert!(config.singleton);
        assert!(config.command_zone);
        assert_eq!(config.commander_damage_threshold, Some(21));
        assert!(!config.team_based);
    }

    #[test]
    fn format_config_free_for_all() {
        let config = FormatConfig::free_for_all();
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 6);
        assert_eq!(config.deck_size, 60);
        assert!(!config.singleton);
        assert!(!config.command_zone);
    }

    #[test]
    fn format_config_two_headed_giant() {
        let config = FormatConfig::two_headed_giant();
        assert_eq!(config.starting_life, 30);
        assert_eq!(config.min_players, 4);
        assert_eq!(config.max_players, 4);
        assert!(config.team_based);
    }

    #[test]
    fn sideboard_policy_matches_format_semantics() {
        assert_eq!(
            GameFormat::Standard.sideboard_policy(),
            SideboardPolicy::Limited(15)
        );
        assert_eq!(
            GameFormat::Pauper.sideboard_policy(),
            SideboardPolicy::Limited(15)
        );
        assert_eq!(
            GameFormat::Commander.sideboard_policy(),
            SideboardPolicy::Forbidden
        );
        assert_eq!(
            GameFormat::Brawl.sideboard_policy(),
            SideboardPolicy::Forbidden
        );
        assert_eq!(
            GameFormat::HistoricBrawl.sideboard_policy(),
            SideboardPolicy::Forbidden
        );
        assert_eq!(
            GameFormat::FreeForAll.sideboard_policy(),
            SideboardPolicy::Unlimited
        );
        assert_eq!(
            GameFormat::TwoHeadedGiant.sideboard_policy(),
            SideboardPolicy::Unlimited
        );
    }

    #[test]
    fn sideboard_policy_serializes_as_tagged_union() {
        // Unit variants emit {"type": "..."} with no "data" field — the
        // frontend consumer must switch on `.type`, never destructure `.data`
        // unconditionally.
        let forbidden = serde_json::to_string(&SideboardPolicy::Forbidden).unwrap();
        assert_eq!(forbidden, r#"{"type":"Forbidden"}"#);

        let unlimited = serde_json::to_string(&SideboardPolicy::Unlimited).unwrap();
        assert_eq!(unlimited, r#"{"type":"Unlimited"}"#);

        // Tuple variant carries the cap in `data`.
        let limited = serde_json::to_string(&SideboardPolicy::Limited(15)).unwrap();
        assert_eq!(limited, r#"{"type":"Limited","data":15}"#);
    }

    #[test]
    fn format_config_serde_roundtrip() {
        let configs = vec![
            FormatConfig::standard(),
            FormatConfig::commander(),
            FormatConfig::pioneer(),
            FormatConfig::historic(),
            FormatConfig::pauper(),
            FormatConfig::brawl(),
            FormatConfig::historic_brawl(),
            FormatConfig::free_for_all(),
            FormatConfig::two_headed_giant(),
        ];
        for config in configs {
            let json = serde_json::to_string(&config).unwrap();
            let deserialized: FormatConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(config, deserialized);
        }
    }
}
