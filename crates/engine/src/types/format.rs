use serde::{Deserialize, Serialize};

use crate::database::legality::LegalityFormat;

/// Broad grouping used by the UI to visually cluster related formats
/// (constructed, commander-style, multiplayer). Frontends may key color
/// treatments off the group so they don't have to maintain a per-format
/// styling table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormatGroup {
    Constructed,
    Commander,
    Multiplayer,
    Limited,
}

/// Authoritative metadata for a single user-selectable format. Produced by
/// `GameFormat::registry()` and consumed by the frontend so that adding a new
/// format requires touching the engine only — no mirrored maps on the client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatMetadata {
    pub format: GameFormat,
    /// Full display label, e.g. "Historic Brawl".
    pub label: &'static str,
    /// Short three-letter code for compact badges, e.g. "HBR".
    pub short_label: &'static str,
    /// One-line human description suitable for a card or tooltip.
    pub description: &'static str,
    pub group: FormatGroup,
    pub default_config: FormatConfig,
}

/// Supported game formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GameFormat {
    Standard,
    Limited,
    Commander,
    Pioneer,
    Modern,
    Legacy,
    Vintage,
    Historic,
    Timeless,
    Pauper,
    PauperCommander,
    DuelCommander,
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
            GameFormat::Modern => Some(LegalityFormat::Modern),
            GameFormat::Legacy => Some(LegalityFormat::Legacy),
            GameFormat::Vintage => Some(LegalityFormat::Vintage),
            GameFormat::Historic => Some(LegalityFormat::Historic),
            GameFormat::Timeless => Some(LegalityFormat::Timeless),
            GameFormat::Pauper => Some(LegalityFormat::Pauper),
            GameFormat::PauperCommander => Some(LegalityFormat::PauperCommander),
            GameFormat::DuelCommander => Some(LegalityFormat::DuelCommander),
            GameFormat::Brawl => Some(LegalityFormat::StandardBrawl),
            GameFormat::HistoricBrawl => Some(LegalityFormat::Brawl),
            GameFormat::FreeForAll | GameFormat::TwoHeadedGiant | GameFormat::Limited => None,
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
            | GameFormat::Modern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper => SideboardPolicy::Limited(15),
            GameFormat::Commander
            | GameFormat::PauperCommander
            | GameFormat::DuelCommander
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl => SideboardPolicy::Forbidden,
            GameFormat::FreeForAll | GameFormat::TwoHeadedGiant | GameFormat::Limited => {
                SideboardPolicy::Unlimited
            }
        }
    }

    /// Whether this format grants a free first mulligan in duels (2-player
    /// games). Combines CR 103.5c (which covers Brawl and all multiplayer
    /// games) with the Commander Rules Committee's supplementary rule (which
    /// extends free-first-mulligan to Commander and Historic Brawl duels).
    ///
    /// Multiplayer games (3+ seats) always get the free first mulligan per
    /// CR 103.5c regardless of format; this predicate is the *duel* override.
    pub fn grants_free_first_mulligan(self) -> bool {
        matches!(
            self,
            GameFormat::Commander
                | GameFormat::PauperCommander
                | GameFormat::DuelCommander
                | GameFormat::Brawl
                | GameFormat::HistoricBrawl,
        )
    }

    /// Display label for validation error messages (e.g., "Not Pioneer legal").
    pub fn label(self) -> &'static str {
        match self {
            GameFormat::Standard => "Standard",
            GameFormat::Limited => "Limited",
            GameFormat::Commander => "Commander",
            GameFormat::Pioneer => "Pioneer",
            GameFormat::Modern => "Modern",
            GameFormat::Legacy => "Legacy",
            GameFormat::Vintage => "Vintage",
            GameFormat::Historic => "Historic",
            GameFormat::Timeless => "Timeless",
            GameFormat::Pauper => "Pauper",
            GameFormat::PauperCommander => "Pauper Commander",
            GameFormat::DuelCommander => "Duel Commander",
            GameFormat::Brawl => "Brawl",
            GameFormat::HistoricBrawl => "Historic Brawl",
            GameFormat::FreeForAll => "Free-for-All",
            GameFormat::TwoHeadedGiant => "Two-Headed Giant",
        }
    }

    /// Authoritative list of user-selectable formats. The frontend consumes
    /// this (via the `get_format_registry` WASM export) to render format
    /// pickers, default configs, and badges. `TwoHeadedGiant` is intentionally
    /// omitted — the enum variant exists but the engine does not yet support
    /// teamed play, so it is not exposed to end users.
    pub fn registry() -> Vec<FormatMetadata> {
        vec![
            FormatMetadata {
                format: GameFormat::Standard,
                label: "Standard",
                short_label: "STD",
                description: "Rotating card pool",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::standard(),
            },
            FormatMetadata {
                format: GameFormat::Pioneer,
                label: "Pioneer",
                short_label: "PIO",
                description: "Non-rotating from 2012",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::pioneer(),
            },
            FormatMetadata {
                format: GameFormat::Modern,
                label: "Modern",
                short_label: "MOD",
                description: "Non-rotating from Mirrodin onward",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::modern(),
            },
            FormatMetadata {
                format: GameFormat::Legacy,
                label: "Legacy",
                short_label: "LEG",
                description: "Eternal format, all sets legal",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::legacy(),
            },
            FormatMetadata {
                format: GameFormat::Vintage,
                label: "Vintage",
                short_label: "VIN",
                description: "Broadest pool, Power Nine restricted",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::vintage(),
            },
            FormatMetadata {
                format: GameFormat::Historic,
                label: "Historic",
                short_label: "HIS",
                description: "Arena's eternal format",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::historic(),
            },
            FormatMetadata {
                format: GameFormat::Timeless,
                label: "Timeless",
                short_label: "TML",
                description: "Arena's eternal non-rotating format",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::timeless(),
            },
            FormatMetadata {
                format: GameFormat::Pauper,
                label: "Pauper",
                short_label: "PAU",
                description: "Commons only",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::pauper(),
            },
            FormatMetadata {
                format: GameFormat::Commander,
                label: "Commander",
                short_label: "CMD",
                description: "100-card singleton, 2\u{2013}4 players",
                group: FormatGroup::Commander,
                default_config: FormatConfig::commander(),
            },
            FormatMetadata {
                format: GameFormat::DuelCommander,
                label: "Duel Commander",
                short_label: "DUC",
                description: "Tournament 1v1 Commander, 30 life",
                group: FormatGroup::Commander,
                default_config: FormatConfig::duel_commander(),
            },
            FormatMetadata {
                format: GameFormat::PauperCommander,
                label: "Pauper Commander",
                short_label: "PDH",
                description: "Commons-only singleton Commander",
                group: FormatGroup::Commander,
                default_config: FormatConfig::pauper_commander(),
            },
            FormatMetadata {
                format: GameFormat::Brawl,
                label: "Brawl",
                short_label: "BRL",
                description: "60-card Standard singleton",
                group: FormatGroup::Commander,
                default_config: FormatConfig::brawl(),
            },
            FormatMetadata {
                format: GameFormat::HistoricBrawl,
                label: "Historic Brawl",
                short_label: "HBR",
                description: "60-card eternal singleton",
                group: FormatGroup::Commander,
                default_config: FormatConfig::historic_brawl(),
            },
            FormatMetadata {
                format: GameFormat::FreeForAll,
                label: "Free-for-All",
                short_label: "FFA",
                description: "3\u{2013}6 player battle royale",
                group: FormatGroup::Multiplayer,
                default_config: FormatConfig::free_for_all(),
            },
            FormatMetadata {
                format: GameFormat::Limited,
                label: "Limited",
                short_label: "LIM",
                description: "Draft or sealed, 40-card deck",
                group: FormatGroup::Limited,
                default_config: FormatConfig::limited(),
            },
        ]
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

    /// Modern: non-rotating constructed from Mirrodin (2003) onward.
    pub fn modern() -> Self {
        FormatConfig {
            format: GameFormat::Modern,
            ..Self::standard()
        }
    }

    /// Legacy: non-rotating constructed spanning the full Magic card pool,
    /// minus the Legacy banned list.
    pub fn legacy() -> Self {
        FormatConfig {
            format: GameFormat::Legacy,
            ..Self::standard()
        }
    }

    /// Vintage: non-rotating constructed with the broadest legal pool,
    /// restricted rather than fully banned for Power Nine and similar.
    pub fn vintage() -> Self {
        FormatConfig {
            format: GameFormat::Vintage,
            ..Self::standard()
        }
    }

    /// Timeless: Arena's eternal non-rotating format, 60-card constructed.
    pub fn timeless() -> Self {
        FormatConfig {
            format: GameFormat::Timeless,
            ..Self::standard()
        }
    }

    /// Pauper Commander: 100-card singleton commander format restricted to
    /// commons (with an uncommon creature/planeswalker commander). Shares
    /// Commander's structural rules (life, command zone, damage threshold).
    pub fn pauper_commander() -> Self {
        FormatConfig {
            format: GameFormat::PauperCommander,
            ..Self::commander()
        }
    }

    /// Duel Commander: tournament 1v1 commander. 100-card singleton but 30
    /// life, strict duel cap, distinct banned list from regular Commander.
    pub fn duel_commander() -> Self {
        FormatConfig {
            format: GameFormat::DuelCommander,
            starting_life: 30,
            max_players: 2,
            ..Self::commander()
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

    /// Limited: 40-card minimum, 20 starting life, 2-player, no singleton,
    /// no command zone. Used by all Draft variants.
    pub fn limited() -> Self {
        FormatConfig {
            format: GameFormat::Limited,
            starting_life: 20,
            min_players: 2,
            max_players: 2,
            deck_size: 40,
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
            GameFormat::Limited => Self::limited(),
            GameFormat::Commander => Self::commander(),
            GameFormat::Pioneer => Self::pioneer(),
            GameFormat::Modern => Self::modern(),
            GameFormat::Legacy => Self::legacy(),
            GameFormat::Vintage => Self::vintage(),
            GameFormat::Historic => Self::historic(),
            GameFormat::Timeless => Self::timeless(),
            GameFormat::Pauper => Self::pauper(),
            GameFormat::PauperCommander => Self::pauper_commander(),
            GameFormat::DuelCommander => Self::duel_commander(),
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
            FormatConfig::limited(),
        ];
        for config in configs {
            let json = serde_json::to_string(&config).unwrap();
            let deserialized: FormatConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(config, deserialized);
        }
    }

    #[test]
    fn format_config_limited() {
        let config = FormatConfig::limited();
        assert_eq!(config.format, GameFormat::Limited);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, 40);
        assert!(!config.singleton);
        assert!(!config.command_zone);
        assert_eq!(config.commander_damage_threshold, None);
        assert!(!config.team_based);
    }

    #[test]
    fn limited_legality_format_is_none() {
        assert_eq!(GameFormat::Limited.legality_format(), None);
    }

    #[test]
    fn limited_sideboard_policy_is_unlimited() {
        assert_eq!(
            GameFormat::Limited.sideboard_policy(),
            SideboardPolicy::Unlimited
        );
    }

    #[test]
    fn limited_no_free_first_mulligan() {
        assert!(!GameFormat::Limited.grants_free_first_mulligan());
    }

    #[test]
    fn limited_label() {
        assert_eq!(GameFormat::Limited.label(), "Limited");
    }

    #[test]
    fn limited_for_format_roundtrip() {
        assert_eq!(
            FormatConfig::for_format(GameFormat::Limited),
            FormatConfig::limited()
        );
    }

    #[test]
    fn limited_in_registry() {
        let registry = GameFormat::registry();
        let entry = registry
            .iter()
            .find(|m| m.format == GameFormat::Limited)
            .expect("Limited must be in registry");
        assert_eq!(entry.group, FormatGroup::Limited);
        assert_eq!(entry.short_label, "LIM");
    }
}
