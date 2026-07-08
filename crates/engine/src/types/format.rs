use serde::{Deserialize, Serialize};

use crate::database::legality::LegalityFormat;
use crate::types::player::PlayerId;

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
    Premodern,
    Legacy,
    Vintage,
    Historic,
    Timeless,
    Pauper,
    PauperCommander,
    DuelCommander,
    TinyLeaders,
    Oathbreaker,
    Brawl,
    HistoricBrawl,
    FreeForAll,
    TwoHeadedGiant,
    /// CR 904: Default Archenemy — one archenemy faces a team of heroes using
    /// shared team turns (CR 805), with a single scheme deck (CR 904.3).
    Archenemy,
    /// CR 901: Planechase using the single communal planar deck option
    /// (CR 901.15a), plus normal 60-card player decks.
    Planechase,
    /// Momir's Madness: 60 snow basic lands (12 each, no Snow-Covered Wastes),
    /// 20 life, a game-start command-zone emblem granting "{X}, Discard a card:
    /// Create a token that's a copy of a creature card with mana value X chosen
    /// at random."
    Momir,
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

/// Per-card override to the default constructed copy limit.
///
/// CR 100.2a sets the default constructed limit to four of any card with a
/// particular English name (basic lands excepted). A handful of cards print an
/// explicit deck-construction override in their rules text:
///
/// - `Unlimited`: "A deck can have any number of cards named ~." (Relentless
///   Rats, Shadowborn Apostle, etc.) — no upper bound on copies.
/// - `UpTo(n)`: "A deck can have up to <n> cards named ~." (Seven Dwarves → 7,
///   Nazgûl → 9) and the Commander/companion singleton override "Your deck can
///   have only one copy of this card" (Vazal, the Compleat → `UpTo(1)`).
///
/// CR 903.5b's Commander singleton rule exempts basic lands; an `UpTo(n>1)`
/// override likewise raises the cap above the format default for that card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DeckCopyLimit {
    Unlimited,
    UpTo(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStructure {
    IndividualTurns,
    SharedTeamTurns,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatTopology {
    IndividualSeats,
    FixedTeams {
        team_size: u8,
        team_count: u8,
        turn_structure: TurnStructure,
    },
    OneVsMany {
        archenemy: PlayerId,
        turn_structure: TurnStructure,
    },
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
    /// CR 904.2a / CR 904.6: In default Archenemy, the single-player team is
    /// designated as the archenemy and takes the first turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archenemy_player: Option<PlayerId>,
    /// Engine-derived predicate: true when the format uses a commander card
    /// and the commander-damage state-based action (CR 903.10a / CR 704.5u).
    /// Covers Commander, Duel Commander, Pauper Commander, Brawl, and
    /// Historic Brawl. The frontend consumes this directly — it must never
    /// re-list commander-style formats client-side.
    pub uses_commander: bool,
    /// Engine-derived predicate (mirrors `GameFormat::supplies_fixed_deck`):
    /// true when the format's deck is fixed and supplied automatically by the
    /// engine, so the player builds/selects nothing. True only for Momir's
    /// Madness. The frontend consumes this directly to bypass deck-selection
    /// gates — it must never re-list fixed-deck formats client-side.
    #[serde(default)]
    pub supplies_fixed_deck: bool,
    /// Capability flag: when true, the server (and other transport gates)
    /// permit `GameAction::Debug(_)` from any player in this session. Off by
    /// default. Orthogonal to format — a sandbox Commander game plays
    /// exactly like a normal Commander game with one additional permission.
    /// Immutable for the life of the session.
    #[serde(default)]
    pub allow_debug_actions: bool,
}

impl FormatTopology {
    pub fn has_shared_team_turns(self) -> bool {
        matches!(
            self,
            FormatTopology::FixedTeams {
                turn_structure: TurnStructure::SharedTeamTurns,
                ..
            } | FormatTopology::OneVsMany {
                turn_structure: TurnStructure::SharedTeamTurns,
                ..
            }
        )
    }
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
            GameFormat::Premodern => Some(LegalityFormat::Premodern),
            GameFormat::Legacy => Some(LegalityFormat::Legacy),
            GameFormat::Vintage => Some(LegalityFormat::Vintage),
            GameFormat::Historic => Some(LegalityFormat::Historic),
            GameFormat::Timeless => Some(LegalityFormat::Timeless),
            GameFormat::Pauper => Some(LegalityFormat::Pauper),
            GameFormat::PauperCommander => Some(LegalityFormat::PauperCommander),
            GameFormat::DuelCommander => Some(LegalityFormat::DuelCommander),
            GameFormat::Brawl => Some(LegalityFormat::StandardBrawl),
            GameFormat::HistoricBrawl => Some(LegalityFormat::Brawl),
            GameFormat::TinyLeaders
            | GameFormat::Oathbreaker
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            // Momir's pool is the entire creature corpus — no legality restriction.
            | GameFormat::Momir
            | GameFormat::Limited => None,
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
            | GameFormat::Premodern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper => SideboardPolicy::Limited(15),
            GameFormat::Commander
            | GameFormat::PauperCommander
            | GameFormat::DuelCommander
            | GameFormat::Oathbreaker
            | GameFormat::Brawl
            // Momir has no sideboard — the deck is exactly 60 snow basic lands.
            | GameFormat::Momir
            | GameFormat::HistoricBrawl => SideboardPolicy::Forbidden,
            GameFormat::TinyLeaders => SideboardPolicy::Limited(10),
            GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Limited => SideboardPolicy::Unlimited,
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
                | GameFormat::Oathbreaker
                | GameFormat::Brawl
                | GameFormat::HistoricBrawl,
        )
    }

    /// Whether this format uses a commander card and the commander-damage
    /// state-based action (CR 903.10a / CR 704.5u). True for Commander, Duel
    /// Commander, Pauper Commander, Brawl, and Historic Brawl — every format
    /// whose `FormatConfig` has both `command_zone: true` and a non-`None`
    /// `commander_damage_threshold`. The frontend consumes the derived
    /// `FormatConfig::uses_commander` field rather than re-listing the
    /// commander-style variants client-side.
    pub fn uses_commander(self) -> bool {
        matches!(
            self,
            GameFormat::Commander
                | GameFormat::DuelCommander
                | GameFormat::PauperCommander
                | GameFormat::Brawl
                | GameFormat::HistoricBrawl,
        )
    }

    /// Whether this format's deck is fixed by the format rules and supplied
    /// automatically by the engine — the player never builds or selects one.
    /// True only for Momir's Madness, whose deck is the fixed 60-card snow-basic
    /// list (`deck_loading::momir_fixed_deck_names`); `load_and_hydrate_decks`
    /// synthesizes it for every seat. The frontend consumes the derived
    /// `FormatConfig::supplies_fixed_deck` field to bypass deck-selection gates,
    /// and must never re-list fixed-deck formats client-side.
    pub fn supplies_fixed_deck(self) -> bool {
        matches!(self, GameFormat::Momir)
    }

    /// Display label for validation error messages (e.g., "Not Pioneer legal").
    pub fn label(self) -> &'static str {
        match self {
            GameFormat::Standard => "Standard",
            GameFormat::Limited => "Limited",
            GameFormat::Commander => "Commander",
            GameFormat::Pioneer => "Pioneer",
            GameFormat::Modern => "Modern",
            GameFormat::Premodern => "Premodern",
            GameFormat::Legacy => "Legacy",
            GameFormat::Vintage => "Vintage",
            GameFormat::Historic => "Historic",
            GameFormat::Timeless => "Timeless",
            GameFormat::Pauper => "Pauper",
            GameFormat::PauperCommander => "Pauper Commander",
            GameFormat::DuelCommander => "Duel Commander",
            GameFormat::TinyLeaders => "Tiny Leaders: Reborn",
            GameFormat::Oathbreaker => "Oathbreaker",
            GameFormat::Brawl => "Brawl",
            GameFormat::HistoricBrawl => "Historic Brawl",
            GameFormat::FreeForAll => "Free-for-All",
            GameFormat::TwoHeadedGiant => "Two-Headed Giant",
            GameFormat::Archenemy => "Archenemy",
            GameFormat::Planechase => "Planechase",
            GameFormat::Momir => "Momir's Madness",
        }
    }

    /// Authoritative list of user-selectable formats. The frontend consumes
    /// this (via the `get_format_registry` WASM export) to render format
    /// pickers, default configs, and badges. Surface-specific callers may
    /// filter this list when a format is not appropriate for that entry point
    /// (for example deck-construction or solo-AI setup).
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
                format: GameFormat::Premodern,
                label: "Premodern",
                short_label: "PRE",
                description: "Old-frame constructed through Scourge",
                group: FormatGroup::Constructed,
                default_config: FormatConfig::premodern(),
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
                format: GameFormat::TinyLeaders,
                label: "Tiny Leaders: Reborn",
                short_label: "TLR",
                description: "50-card Tiny singleton",
                group: FormatGroup::Commander,
                default_config: FormatConfig::tiny_leaders(),
            },
            FormatMetadata {
                format: GameFormat::Oathbreaker,
                label: "Oathbreaker",
                short_label: "OBK",
                description: "60-card singleton, Planeswalker + signature spell",
                group: FormatGroup::Commander,
                default_config: FormatConfig::oathbreaker(),
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
                format: GameFormat::TwoHeadedGiant,
                label: "Two-Headed Giant",
                short_label: "2HG",
                description: "4 players, two teams of two",
                group: FormatGroup::Multiplayer,
                default_config: FormatConfig::two_headed_giant(),
            },
            FormatMetadata {
                format: GameFormat::Archenemy,
                label: "Archenemy",
                short_label: "ARC",
                description: "One archenemy against a team of heroes",
                group: FormatGroup::Multiplayer,
                default_config: FormatConfig::archenemy(),
            },
            FormatMetadata {
                format: GameFormat::Planechase,
                label: "Planechase",
                short_label: "PLC",
                description: "60-card multiplayer with a communal planar deck",
                group: FormatGroup::Multiplayer,
                default_config: FormatConfig::planechase(),
            },
            FormatMetadata {
                format: GameFormat::Limited,
                label: "Limited",
                short_label: "LIM",
                description: "Draft or sealed, 40-card deck",
                group: FormatGroup::Limited,
                default_config: FormatConfig::limited(),
            },
            FormatMetadata {
                format: GameFormat::Momir,
                label: "Momir's Madness",
                short_label: "MOM",
                description: "60 snow basic lands, random creature tokens",
                group: FormatGroup::Multiplayer,
                default_config: FormatConfig::momir(),
            },
        ]
    }
}

impl FormatConfig {
    pub fn topology(&self) -> FormatTopology {
        match self.format {
            GameFormat::TwoHeadedGiant => FormatTopology::FixedTeams {
                team_size: 2,
                team_count: 2,
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            GameFormat::Archenemy => FormatTopology::OneVsMany {
                archenemy: self.archenemy_player.unwrap_or(PlayerId(0)),
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            _ if self.team_based => FormatTopology::FixedTeams {
                team_size: 2,
                team_count: 2,
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            _ => FormatTopology::IndividualSeats,
        }
    }

    pub fn starting_life_for_seat(&self) -> i32 {
        match self.topology() {
            FormatTopology::IndividualSeats => self.starting_life,
            FormatTopology::FixedTeams { team_size, .. } => {
                self.starting_life / i32::from(team_size)
            }
            FormatTopology::OneVsMany { .. } => self.starting_life,
        }
    }

    pub fn starting_life_for_player(&self, player: PlayerId) -> i32 {
        match self.topology() {
            FormatTopology::IndividualSeats => self.starting_life,
            FormatTopology::FixedTeams { team_size, .. } => {
                self.starting_life / i32::from(team_size)
            }
            // CR 904.5: The archenemy starts at 40 life; each other player
            // starts at 20. This is not a shared life total.
            FormatTopology::OneVsMany { archenemy, .. } => {
                if player == archenemy {
                    40
                } else {
                    20
                }
            }
        }
    }

    pub fn archenemy_player(&self) -> Option<PlayerId> {
        match self.topology() {
            FormatTopology::OneVsMany { archenemy, .. } => Some(archenemy),
            FormatTopology::IndividualSeats | FormatTopology::FixedTeams { .. } => None,
        }
    }

    pub fn validate_for_player_count(&self, player_count: u8) -> Result<(), String> {
        if self.format == GameFormat::Archenemy {
            let archenemy = self.archenemy_player().unwrap_or(PlayerId(0));
            if archenemy.0 >= player_count {
                return Err(format!(
                    "archenemy_player must be less than player_count ({player_count})"
                ));
            }
        }
        Ok(())
    }

    pub fn starting_player(&self) -> PlayerId {
        // CR 904.6: The archenemy takes the first turn instead of a randomly
        // determined player. Non-Archenemy formats keep the legacy default.
        self.archenemy_player().unwrap_or(PlayerId(0))
    }

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
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
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
            archenemy_player: None,
            uses_commander: true,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
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

    /// Premodern: community-maintained old-frame constructed through Scourge.
    pub fn premodern() -> Self {
        FormatConfig {
            format: GameFormat::Premodern,
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

    /// Tiny Leaders: Reborn: 50-card singleton command-zone format, 20 life,
    /// no commander-damage loss threshold, and up to 10 sideboard cards.
    pub fn tiny_leaders() -> Self {
        FormatConfig {
            format: GameFormat::TinyLeaders,
            starting_life: 20,
            min_players: 2,
            max_players: 2,
            deck_size: 50,
            singleton: true,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
        }
    }

    /// Oathbreaker RC: 60-card singleton, one legendary Planeswalker as the
    /// Oathbreaker commander plus one signature spell (instant/sorcery within
    /// color identity), both in the command zone. 20 life, 2–4 players,
    /// no commander-damage threshold.
    pub fn oathbreaker() -> Self {
        FormatConfig {
            format: GameFormat::Oathbreaker,
            starting_life: 20,
            min_players: 2,
            max_players: 4,
            deck_size: 60,
            singleton: true,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
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
            archenemy_player: None,
            uses_commander: true,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
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
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
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
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
        }
    }

    /// Momir's Madness: 60 snow basic lands (12 each of Snow-Covered Plains/
    /// Island/Swamp/Mountain/Forest, no Snow-Covered Wastes), 20 life, 2-player.
    /// A game-start command-zone emblem grants the random-creature-token
    /// activated ability. No sideboard, no commander. `command_zone: true` so
    /// the command-zone activation surface and pool rehydration are enabled.
    pub fn momir() -> Self {
        FormatConfig {
            format: GameFormat::Momir,
            starting_life: 20,
            min_players: 2,
            max_players: 2,
            deck_size: 60,
            singleton: false,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: true,
            allow_debug_actions: false,
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
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
        }
    }

    /// CR 901.15a: Planechase with one communal planar deck. Player decks use
    /// normal 60-card construction; the supplementary planar deck is validated
    /// separately against the actual player count.
    pub fn planechase() -> Self {
        FormatConfig {
            format: GameFormat::Planechase,
            starting_life: 20,
            min_players: 2,
            max_players: 4,
            deck_size: 60,
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
        }
    }

    /// CR 904.1-904.11: Default Archenemy, not Supervillain Rumble (CR 904.12)
    /// and not Archenemy Commander (CR 904.13).
    pub fn archenemy() -> Self {
        FormatConfig {
            format: GameFormat::Archenemy,
            starting_life: 20,
            min_players: 2,
            max_players: 6,
            deck_size: 60,
            singleton: false,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: Some(PlayerId(0)),
            uses_commander: false,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
        }
    }

    /// Return a copy of this config with the sandbox capability enabled.
    /// Pure data transform; the resulting config is otherwise identical and
    /// keeps the same `GameFormat`, deck/seat/life rules, etc. Idempotent.
    pub fn with_sandbox(mut self) -> Self {
        self.allow_debug_actions = true;
        self
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
            GameFormat::Premodern => Self::premodern(),
            GameFormat::Legacy => Self::legacy(),
            GameFormat::Vintage => Self::vintage(),
            GameFormat::Historic => Self::historic(),
            GameFormat::Timeless => Self::timeless(),
            GameFormat::Pauper => Self::pauper(),
            GameFormat::PauperCommander => Self::pauper_commander(),
            GameFormat::DuelCommander => Self::duel_commander(),
            GameFormat::TinyLeaders => Self::tiny_leaders(),
            GameFormat::Oathbreaker => Self::oathbreaker(),
            GameFormat::Brawl => Self::brawl(),
            GameFormat::HistoricBrawl => Self::historic_brawl(),
            GameFormat::FreeForAll => Self::free_for_all(),
            GameFormat::TwoHeadedGiant => Self::two_headed_giant(),
            GameFormat::Archenemy => Self::archenemy(),
            GameFormat::Planechase => Self::planechase(),
            GameFormat::Momir => Self::momir(),
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
    fn format_config_tiny_leaders() {
        let config = FormatConfig::tiny_leaders();
        assert_eq!(config.format, GameFormat::TinyLeaders);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, 50);
        assert!(config.singleton);
        assert!(config.command_zone);
        assert_eq!(config.commander_damage_threshold, None);
        assert!(!config.uses_commander);
        assert!(!config.team_based);
    }

    #[test]
    fn format_config_premodern() {
        let config = FormatConfig::premodern();
        assert_eq!(config.format, GameFormat::Premodern);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, 60);
        assert!(!config.singleton);
        assert!(!config.command_zone);
        assert_eq!(config.commander_damage_threshold, None);
        assert!(!config.uses_commander);
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
        assert_eq!(
            config.topology(),
            FormatTopology::FixedTeams {
                team_size: 2,
                team_count: 2,
                turn_structure: TurnStructure::SharedTeamTurns,
            }
        );
        assert_eq!(config.starting_life_for_seat(), 15);
    }

    #[test]
    fn format_registry_includes_two_headed_giant() {
        let registry = GameFormat::registry();
        let metadata = registry
            .iter()
            .find(|metadata| metadata.format == GameFormat::TwoHeadedGiant)
            .expect("Two-Headed Giant should be user-selectable");

        assert_eq!(metadata.label, "Two-Headed Giant");
        assert_eq!(metadata.short_label, "2HG");
        assert_eq!(metadata.description, "4 players, two teams of two");
        assert_eq!(metadata.group, FormatGroup::Multiplayer);
        assert_eq!(metadata.default_config.min_players, 4);
        assert_eq!(metadata.default_config.max_players, 4);
        assert_eq!(metadata.default_config.starting_life, 30);
        assert!(metadata.default_config.team_based);
        assert!(!metadata.default_config.supplies_fixed_deck);
    }

    #[test]
    fn starting_life_for_seat_preserves_non_team_formats() {
        assert_eq!(FormatConfig::standard().starting_life_for_seat(), 20);
        assert_eq!(FormatConfig::commander().starting_life_for_seat(), 40);
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
            GameFormat::Premodern.sideboard_policy(),
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
            GameFormat::TinyLeaders.sideboard_policy(),
            SideboardPolicy::Limited(10)
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
    fn deck_copy_limit_serializes_as_tagged_union() {
        // Unit variant emits {"type": "..."} with no "data" field; the frontend
        // must switch on `.type`, never destructure `.data` unconditionally.
        let unlimited = serde_json::to_string(&DeckCopyLimit::Unlimited).unwrap();
        assert_eq!(unlimited, r#"{"type":"Unlimited"}"#);

        // Tuple variant carries the cap in `data`.
        let up_to = serde_json::to_string(&DeckCopyLimit::UpTo(7)).unwrap();
        assert_eq!(up_to, r#"{"type":"UpTo","data":7}"#);

        // Round-trips both directions.
        let parsed: DeckCopyLimit = serde_json::from_str(r#"{"type":"Unlimited"}"#).unwrap();
        assert_eq!(parsed, DeckCopyLimit::Unlimited);
        let parsed: DeckCopyLimit = serde_json::from_str(r#"{"type":"UpTo","data":9}"#).unwrap();
        assert_eq!(parsed, DeckCopyLimit::UpTo(9));
    }

    #[test]
    fn format_config_oathbreaker() {
        let config = FormatConfig::oathbreaker();
        assert_eq!(config.format, GameFormat::Oathbreaker);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 4);
        assert_eq!(config.deck_size, 60);
        assert!(config.singleton);
        assert!(config.command_zone);
        assert_eq!(config.commander_damage_threshold, None);
        assert!(!config.uses_commander);
        assert!(!config.team_based);
        assert_eq!(
            GameFormat::Oathbreaker.sideboard_policy(),
            SideboardPolicy::Forbidden
        );
        assert!(GameFormat::Oathbreaker.grants_free_first_mulligan());
        assert!(!GameFormat::Oathbreaker.uses_commander());
        assert_eq!(GameFormat::Oathbreaker.legality_format(), None);
    }

    #[test]
    fn format_config_serde_roundtrip() {
        let configs = vec![
            FormatConfig::standard(),
            FormatConfig::commander(),
            FormatConfig::pioneer(),
            FormatConfig::premodern(),
            FormatConfig::historic(),
            FormatConfig::pauper(),
            FormatConfig::tiny_leaders(),
            FormatConfig::oathbreaker(),
            FormatConfig::brawl(),
            FormatConfig::historic_brawl(),
            FormatConfig::free_for_all(),
            FormatConfig::two_headed_giant(),
            FormatConfig::archenemy(),
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
    fn premodern_uses_normal_constructed_mulligan() {
        assert!(!GameFormat::Modern.grants_free_first_mulligan());
        assert!(!GameFormat::Premodern.grants_free_first_mulligan());
        assert!(!GameFormat::Legacy.grants_free_first_mulligan());
    }

    #[test]
    fn premodern_legality_format() {
        assert_eq!(
            GameFormat::Premodern.legality_format(),
            Some(LegalityFormat::Premodern)
        );
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
    fn premodern_for_format_roundtrip() {
        assert_eq!(
            FormatConfig::for_format(GameFormat::Premodern),
            FormatConfig::premodern()
        );
    }

    #[test]
    fn uses_commander_matches_default_config_and_threshold() {
        // The `GameFormat::uses_commander()` predicate, the derived
        // `FormatConfig::uses_commander` field, and the existence of a
        // commander-damage threshold must all agree for every variant.
        for meta in GameFormat::registry() {
            let expected = meta.format.uses_commander();
            assert_eq!(
                meta.default_config.uses_commander, expected,
                "{:?}: registry default disagrees with predicate",
                meta.format
            );
            assert_eq!(
                meta.default_config.commander_damage_threshold.is_some(),
                expected,
                "{:?}: commander_damage_threshold presence must match uses_commander",
                meta.format
            );
            // The derived `supplies_fixed_deck` field must agree with the
            // predicate for every variant (engine is the single authority for
            // which formats auto-supply their deck).
            assert_eq!(
                meta.default_config.supplies_fixed_deck,
                meta.format.supplies_fixed_deck(),
                "{:?}: registry default disagrees with supplies_fixed_deck predicate",
                meta.format
            );
        }
        // Variants not in the user-facing registry still respect the invariant.
        for format in [GameFormat::TwoHeadedGiant, GameFormat::Limited] {
            let config = FormatConfig::for_format(format);
            assert_eq!(config.uses_commander, format.uses_commander());
            assert_eq!(config.supplies_fixed_deck, format.supplies_fixed_deck());
        }
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

    #[test]
    fn archenemy_registry_entry_uses_default_topology() {
        let registry = GameFormat::registry();
        let entry = registry
            .iter()
            .find(|m| m.format == GameFormat::Archenemy)
            .expect("Archenemy must be in registry");
        assert_eq!(entry.group, FormatGroup::Multiplayer);
        assert_eq!(entry.short_label, "ARC");
        assert_eq!(entry.default_config, FormatConfig::archenemy());
        assert_eq!(entry.default_config.min_players, 2);
        assert_eq!(entry.default_config.max_players, 6);
        assert_eq!(entry.default_config.deck_size, 60);
        assert!(entry.default_config.command_zone);
        assert!(!entry.default_config.team_based);
        assert_eq!(entry.default_config.commander_damage_threshold, None);
        assert_eq!(entry.default_config.archenemy_player(), Some(PlayerId(0)));
    }

    #[test]
    fn premodern_registry_entry_is_ordered_with_constructed_formats() {
        let registry = GameFormat::registry();
        let modern_index = registry
            .iter()
            .position(|m| m.format == GameFormat::Modern)
            .expect("Modern must be in registry");
        let premodern_index = registry
            .iter()
            .position(|m| m.format == GameFormat::Premodern)
            .expect("Premodern must be in registry");
        let legacy_index = registry
            .iter()
            .position(|m| m.format == GameFormat::Legacy)
            .expect("Legacy must be in registry");

        assert_eq!(premodern_index, modern_index + 1);
        assert_eq!(legacy_index, premodern_index + 1);
        assert_eq!(registry[premodern_index].short_label, "PRE");
        assert_eq!(registry[premodern_index].group, FormatGroup::Constructed);
    }

    #[test]
    fn registry_constructed_formats_have_legality_mapping() {
        for meta in GameFormat::registry()
            .into_iter()
            .filter(|meta| meta.group == FormatGroup::Constructed)
        {
            assert!(
                meta.format.legality_format().is_some(),
                "{:?} is constructed but has no legality mapping",
                meta.format
            );
        }
    }
}
