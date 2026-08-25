use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::database::legality::LegalityFormat;
use crate::types::custom_format::{
    custom_format_registry, CustomFormatId, CustomFormatRules, FormatConfigError,
};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// An engine-validated custom format. Resolves via
    /// `FormatConfig.custom_rules` (see `types::custom_format`) — a bare
    /// `GameFormat::Custom(id)` alone cannot fully answer several of this
    /// enum's methods; see each method's doc comment for how it handles
    /// `Custom`.
    Custom(CustomFormatId),
}

/// Parse error for `GameFormat::from_str` — `GameFormat` has no catch-all
/// variant (unlike `Keyword`), so this is genuinely fallible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameFormatParseError(pub String);

impl std::fmt::Display for GameFormatParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GameFormatParseError {}

impl std::str::FromStr for GameFormat {
    type Err = GameFormatParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("Custom:") {
            return rest
                .parse::<u16>()
                .map(|n| GameFormat::Custom(CustomFormatId(n)))
                .map_err(|_| GameFormatParseError(format!("invalid Custom format id: {rest:?}")));
        }
        match s {
            "Standard" => Ok(GameFormat::Standard),
            "Limited" => Ok(GameFormat::Limited),
            "Commander" => Ok(GameFormat::Commander),
            "Pioneer" => Ok(GameFormat::Pioneer),
            "Modern" => Ok(GameFormat::Modern),
            "Premodern" => Ok(GameFormat::Premodern),
            "Legacy" => Ok(GameFormat::Legacy),
            "Vintage" => Ok(GameFormat::Vintage),
            "Historic" => Ok(GameFormat::Historic),
            "Timeless" => Ok(GameFormat::Timeless),
            "Pauper" => Ok(GameFormat::Pauper),
            "PauperCommander" => Ok(GameFormat::PauperCommander),
            "DuelCommander" => Ok(GameFormat::DuelCommander),
            "TinyLeaders" => Ok(GameFormat::TinyLeaders),
            "Oathbreaker" => Ok(GameFormat::Oathbreaker),
            "Brawl" => Ok(GameFormat::Brawl),
            "HistoricBrawl" => Ok(GameFormat::HistoricBrawl),
            "FreeForAll" => Ok(GameFormat::FreeForAll),
            "TwoHeadedGiant" => Ok(GameFormat::TwoHeadedGiant),
            "Archenemy" => Ok(GameFormat::Archenemy),
            "Planechase" => Ok(GameFormat::Planechase),
            "Momir" => Ok(GameFormat::Momir),
            other => Err(GameFormatParseError(format!(
                "unknown GameFormat: {other:?}"
            ))),
        }
    }
}

impl std::fmt::Display for GameFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameFormat::Custom(id) => write!(f, "Custom:{}", id.0),
            GameFormat::Standard => write!(f, "Standard"),
            GameFormat::Limited => write!(f, "Limited"),
            GameFormat::Commander => write!(f, "Commander"),
            GameFormat::Pioneer => write!(f, "Pioneer"),
            GameFormat::Modern => write!(f, "Modern"),
            GameFormat::Premodern => write!(f, "Premodern"),
            GameFormat::Legacy => write!(f, "Legacy"),
            GameFormat::Vintage => write!(f, "Vintage"),
            GameFormat::Historic => write!(f, "Historic"),
            GameFormat::Timeless => write!(f, "Timeless"),
            GameFormat::Pauper => write!(f, "Pauper"),
            GameFormat::PauperCommander => write!(f, "PauperCommander"),
            GameFormat::DuelCommander => write!(f, "DuelCommander"),
            GameFormat::TinyLeaders => write!(f, "TinyLeaders"),
            GameFormat::Oathbreaker => write!(f, "Oathbreaker"),
            GameFormat::Brawl => write!(f, "Brawl"),
            GameFormat::HistoricBrawl => write!(f, "HistoricBrawl"),
            GameFormat::FreeForAll => write!(f, "FreeForAll"),
            GameFormat::TwoHeadedGiant => write!(f, "TwoHeadedGiant"),
            GameFormat::Archenemy => write!(f, "Archenemy"),
            GameFormat::Planechase => write!(f, "Planechase"),
            GameFormat::Momir => write!(f, "Momir"),
        }
    }
}

impl Serialize for GameFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GameFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::String(s) => {
                s.parse::<GameFormat>().map_err(serde::de::Error::custom)
            }
            other => Err(serde::de::Error::custom(format!(
                "expected a string for GameFormat, got {other:?}"
            ))),
        }
    }
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

/// A deck-construction copy ceiling for one card name: either unbounded or
/// capped at `n`. Used at both levels of the rule — the format's default
/// (see [`GameFormat::default_deck_copy_limit`]) and a card's printed override.
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

/// Configuration for the limited range of influence option.
///
/// The engine does not implement limited-range rules yet. This type preserves
/// the full per-seat configuration shape so external boundaries can reject it
/// explicitly until the corresponding game-rule support exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RangeOfInfluenceConfig {
    /// The number of seats away that each player can influence by default.
    /// Zero is valid and means a player can influence only themself.
    pub default_range: u8,
    /// Per-seat exceptions to [`Self::default_range`].
    #[serde(default)]
    pub player_overrides: BTreeMap<PlayerId, u8>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RangeOfInfluenceConfigWire {
    Current(RangeOfInfluenceConfig),
    Legacy(u8),
}

fn deserialize_range_of_influence<'de, D>(
    deserializer: D,
) -> Result<Option<Box<RangeOfInfluenceConfig>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<RangeOfInfluenceConfigWire>::deserialize(deserializer).map(|range| {
        range.map(|range| {
            Box::new(match range {
                RangeOfInfluenceConfigWire::Current(config) => config,
                RangeOfInfluenceConfigWire::Legacy(default_range) => RangeOfInfluenceConfig {
                    default_range,
                    player_overrides: BTreeMap::new(),
                },
            })
        })
    })
}

/// Fail-closed default for `FormatConfig.sideboard_policy` on a payload
/// serialized before that field existed: understating a sideboard
/// allowance is safer than overstating one.
fn default_sideboard_policy_fallback() -> SideboardPolicy {
    SideboardPolicy::Forbidden
}

/// Configuration for a game format, describing player counts, starting life, deck rules, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct FormatConfig {
    pub format: GameFormat,
    pub starting_life: i32,
    pub min_players: u8,
    pub max_players: u8,
    /// CR 100.5: the format's **minimum** deck size. There is no maximum deck
    /// size for non-Commander decks, so a 61-card Standard deck is legal and
    /// this must be compared with `<`, never `==`.
    ///
    /// The command-zone formats are the exception: CR 903.5a makes 100 both the
    /// minimum and the maximum, so an exact comparison is correct there — but
    /// only there, and only when gated on `command_zone`.
    pub deck_size: u16,
    pub singleton: bool,
    pub command_zone: bool,
    pub commander_damage_threshold: Option<u8>,
    #[serde(default, deserialize_with = "deserialize_range_of_influence")]
    pub range_of_influence: Option<Box<RangeOfInfluenceConfig>>,
    pub team_based: bool,
    /// CR 904.2a / CR 904.6: In default Archenemy, the single-player team is
    /// designated as the archenemy and takes the first turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archenemy_player: Option<PlayerId>,
    /// Engine-derived predicate: true when the format uses a commander card
    /// and the commander-damage state-based action (CR 903.10a).
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
    /// Engine-derived, stored per-format sideboard policy (CR 100.4/100.4a).
    /// Mirrors `uses_commander`/`supplies_fixed_deck`'s stored-field
    /// pattern — real consumers (`deck_loading.rs`, `match_flow.rs`,
    /// `companion.rs`) read this field, never `GameFormat::sideboard_policy()`
    /// directly: for a built-in format the two always agree, but for
    /// `GameFormat::Custom` the bare method has no way to see the real
    /// declared policy sitting in `custom_rules.structural.sideboard_policy`
    /// and would silently discard it, which is exactly the bug this field
    /// exists to prevent. `#[serde(default)]` fails closed (`Forbidden`) for
    /// any payload serialized before this field existed.
    #[serde(default = "default_sideboard_policy_fallback")]
    pub sideboard_policy: SideboardPolicy,
    /// Capability flag: when true, the server (and other transport gates)
    /// permit `GameAction::Debug(_)` from any player in this session. Off by
    /// default. Orthogonal to format — a sandbox Commander game plays
    /// exactly like a normal Commander game with one additional permission.
    /// Immutable for the life of the session.
    #[serde(default)]
    pub allow_debug_actions: bool,
    /// Present only when `format == GameFormat::Custom(id)` (and then `id`
    /// must equal `custom_rules.id` — see
    /// `custom_format::validate_custom_rules_consistency`). `None` for every
    /// built-in format. Boxed because `FormatConfig` is embedded directly in
    /// `lobby_broker::protocol::LobbyClientMessage::CreateGameWithSettings`
    /// (and the canonical `server_core` equivalent) — an unboxed
    /// `CustomFormatRules` pushes that enum's largest variant over clippy's
    /// `large_enum_variant` threshold, exactly like `range_of_influence`
    /// above is boxed for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_rules: Option<Box<CustomFormatRules>>,
}

/// Deserializing via the derive above, unchecked, would let an external
/// payload construct `format: Custom(id)` with `custom_rules: None` or a
/// mismatched id — `custom_format::validate_custom_rules_consistency` exists
/// precisely to reject that, but a validator nobody calls doesn't protect
/// anything. `#[serde(remote = "Self")]` on `FormatConfig` above generates
/// this type's normal derived field-by-field (de)serialization as plain
/// inherent `FormatConfig::serialize`/`FormatConfig::deserialize` functions
/// (not the `Serialize`/`Deserialize` trait impls, which are instead
/// hand-written here) — the standard serde idiom for "derive, then validate
/// before accepting," with zero duplicated field declarations. `Serialize`
/// needs no validation (an in-memory `FormatConfig` is already guaranteed
/// consistent) and is a pure passthrough; `Deserialize` is the single
/// authoritative `FormatConfig` ingress — every deserialization path (WASM
/// boundary, lobby-broker/server-core protocol payloads, replay/save/restore
/// files) goes through it, since every such path ultimately deserializes a
/// `GameState`/`PersistedGameState` whose own `format_config: FormatConfig`
/// field is a plain derived field with no bypass.
impl Serialize for FormatConfig {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Self::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for FormatConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let config = Self::deserialize(deserializer)?;
        // `command_zone`/`commander_damage_threshold`/`uses_commander`/
        // `singleton`/`sideboard_policy` etc. are this struct's own independently-serialized
        // runtime fields — for a built-in format they're always consistent
        // because a `FormatConfig::x()` builder derived them together. For
        // `Custom` there is no such builder yet: `custom_rules.structural`
        // (a `CommandZoneMode`-discriminated declaration) and these bare
        // runtime fields are two independently-writable representations of
        // the same thing, and nothing here cross-checks them — a
        // matching-`custom_rules.id` payload could still declare
        // `CommandZoneMode::Disabled` while separately setting
        // `command_zone: true`. Rather than accept that inconsistency (or
        // partially validate it in a way a later phase would have to widen
        // anyway), reject `Custom` outright at this single boundary until a
        // real resolver — landing with Axis A/Axis B registration — can
        // derive every runtime field FROM `custom_rules.structural` instead
        // of accepting them independently. `GameFormat::Custom` remains
        // fully constructible in-memory (for schema tests, and for that
        // future resolver to build) — only external deserialization of an
        // ACTIVE `FormatConfig` is refused.
        if matches!(config.format, GameFormat::Custom(_)) {
            return Err(serde::de::Error::custom(
                "GameFormat::Custom cannot be activated via external FormatConfig \
                 deserialization yet — no resolver exists to derive/validate this struct's \
                 runtime fields (command_zone, uses_commander, commander_damage_threshold, \
                 singleton, sideboard_policy, ...) from custom_rules.structural, so two independently-writable \
                 representations of the same state could disagree; this lands in a later phase",
            ));
        }
        crate::types::custom_format::validate_custom_rules_consistency(&config)
            .map_err(serde::de::Error::custom)?;
        Ok(config)
    }
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
            // A custom format's legality is entirely governed by its own
            // `LegalityRules` (legal_sets/banned/restricted), never by the
            // built-in `LegalityFormat` table.
            GameFormat::Custom(_) => None,
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
            // Phase 1a: disclosed, temporary, bare-GameFormat-context
            // fallback — not this custom format's real declared policy
            // (that lives on the resolved FormatConfig/CustomFormatRules,
            // which this method has no access to). Forbidden is the
            // fail-closed answer: understating a sideboard allowance is
            // safer than overstating one. Phase 1b migrates real callers to
            // read FormatConfig's resolved field instead of this method.
            GameFormat::Custom(_) => SideboardPolicy::Forbidden,
        }
    }

    /// CR 100.2a / CR 100.2b / CR 903.5b: Per-format default copy limit for a
    /// single card name, before per-card overrides and the basic-land
    /// exemption are applied.
    ///
    /// - `UpTo(4)` — CR 100.2a: constructed decks may contain no more than four
    ///   of any card with a particular English name. Planechase and Archenemy
    ///   build a constructed deck plus a supplementary deck (CR 100.2d), so
    ///   they inherit the same limit.
    /// - `UpTo(1)` — CR 903.5b: the Commander singleton rule, shared by every
    ///   command-zone singleton variant.
    /// - `Unlimited` — CR 100.2b: a limited deck may contain as many duplicates
    ///   of a card as the product provides. Free-for-All and Two-Headed Giant
    ///   are casual variants that restrict no card pool, and Momir supplies a
    ///   fixed deck (CR 100.2a's limit never applies to a deck players do not
    ///   construct).
    ///
    /// This is the format half of the copy rule only. `max_deck_copies` in
    /// `game::deck_validation` is the single authority that combines it with
    /// the basic-land exemption and a card's printed [`DeckCopyLimit`]
    /// override; callers wanting "how many copies of this card are legal"
    /// must use that, never this method alone.
    pub fn default_deck_copy_limit(self) -> DeckCopyLimit {
        match self {
            GameFormat::Standard
            | GameFormat::Pioneer
            | GameFormat::Modern
            | GameFormat::Premodern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper
            | GameFormat::Planechase
            | GameFormat::Archenemy => DeckCopyLimit::UpTo(4),
            GameFormat::Commander
            | GameFormat::PauperCommander
            | GameFormat::DuelCommander
            | GameFormat::TinyLeaders
            | GameFormat::Oathbreaker
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl => DeckCopyLimit::UpTo(1),
            GameFormat::Limited
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Momir => DeckCopyLimit::Unlimited,
            // Phase 1a: disclosed, temporary, bare-GameFormat-context
            // fallback — not this custom format's real declared limit.
            // UpTo(1) (the same value already used for command-zone
            // singleton formats) is the fail-closed answer: it under-permits
            // rather than silently over-permitting a format whose real
            // rules were never consulted. Phase 1b migrates real callers to
            // read FormatConfig's resolved field instead of this method.
            GameFormat::Custom(_) => DeckCopyLimit::UpTo(1),
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
    /// state-based action (CR 903.10a). True for Commander, Duel
    /// Commander, Pauper Commander, Brawl, and Historic Brawl — every format
    /// whose `FormatConfig` has both `command_zone: true` and a non-`None`
    /// `commander_damage_threshold`. The frontend consumes the derived
    /// `FormatConfig::uses_commander` field rather than re-listing the
    /// commander-style variants client-side.
    ///
    /// Returns `Err` for `GameFormat::Custom` rather than panicking or
    /// guessing `false`: this is a public query, callable with any
    /// `GameFormat` a caller holds — including one parsed straight from
    /// untrusted input, since `GameFormat::from_str` accepts any
    /// `"Custom:<u16>"` string. A bare `GameFormat` carries no
    /// `CustomFormatRules` to answer this from, and a Custom format can
    /// legitimately resolve to a commander-using configuration, so `false`
    /// would be a silently wrong answer, not a safe default. Callers that
    /// might see a Custom format from an external source must handle the
    /// rejection; callers with a resolved `FormatConfig` should read its
    /// `uses_commander` field instead of calling this at all.
    pub fn uses_commander(self) -> Result<bool, FormatConfigError> {
        match self {
            GameFormat::Commander
            | GameFormat::DuelCommander
            | GameFormat::PauperCommander
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl => Ok(true),
            GameFormat::Standard
            | GameFormat::Limited
            | GameFormat::Pioneer
            | GameFormat::Modern
            | GameFormat::Premodern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper
            | GameFormat::TinyLeaders
            | GameFormat::Oathbreaker
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir => Ok(false),
            GameFormat::Custom(id) => Err(FormatConfigError(format!(
                "uses_commander cannot resolve ad-hoc Custom format {} — read \
                 FormatConfig.uses_commander from the resolved config instead",
                id.0
            ))),
        }
    }

    /// Whether this format's deck is fixed by the format rules and supplied
    /// automatically by the engine — the player never builds or selects one.
    /// True only for Momir's Madness, whose deck is the fixed 60-card snow-basic
    /// list (`deck_loading::momir_fixed_deck_names`); `load_and_hydrate_decks`
    /// synthesizes it for every seat. The frontend consumes the derived
    /// `FormatConfig::supplies_fixed_deck` field to bypass deck-selection gates,
    /// and must never re-list fixed-deck formats client-side.
    pub fn supplies_fixed_deck(self) -> bool {
        match self {
            GameFormat::Momir => true,
            GameFormat::Standard
            | GameFormat::Limited
            | GameFormat::Commander
            | GameFormat::Pioneer
            | GameFormat::Modern
            | GameFormat::Premodern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper
            | GameFormat::PauperCommander
            | GameFormat::DuelCommander
            | GameFormat::TinyLeaders
            | GameFormat::Oathbreaker
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase => false,
            // No custom-format use case for an engine-supplied fixed deck
            // exists today — a real one would need its own design, analogous
            // to Momir's Madness.
            GameFormat::Custom(_) => false,
        }
    }

    /// Display label for validation error messages (e.g., "Not Pioneer legal").
    ///
    /// Built-in variants return a static string. `Custom(id)` looks the id up
    /// in `custom_format_registry()`: a hit returns that preset's real label;
    /// a miss returns the fixed fallback `"Custom Format"`. A miss is the
    /// normal case for an ad-hoc lobby-saved format — its player-chosen name
    /// is client-local only and never travels to the engine, so the engine
    /// has no name of its own to report.
    pub fn label(self) -> Cow<'static, str> {
        match self {
            GameFormat::Standard => Cow::Borrowed("Standard"),
            GameFormat::Limited => Cow::Borrowed("Limited"),
            GameFormat::Commander => Cow::Borrowed("Commander"),
            GameFormat::Pioneer => Cow::Borrowed("Pioneer"),
            GameFormat::Modern => Cow::Borrowed("Modern"),
            GameFormat::Premodern => Cow::Borrowed("Premodern"),
            GameFormat::Legacy => Cow::Borrowed("Legacy"),
            GameFormat::Vintage => Cow::Borrowed("Vintage"),
            GameFormat::Historic => Cow::Borrowed("Historic"),
            GameFormat::Timeless => Cow::Borrowed("Timeless"),
            GameFormat::Pauper => Cow::Borrowed("Pauper"),
            GameFormat::PauperCommander => Cow::Borrowed("Pauper Commander"),
            GameFormat::DuelCommander => Cow::Borrowed("Duel Commander"),
            GameFormat::TinyLeaders => Cow::Borrowed("Tiny Leaders: Reborn"),
            GameFormat::Oathbreaker => Cow::Borrowed("Oathbreaker"),
            GameFormat::Brawl => Cow::Borrowed("Brawl"),
            GameFormat::HistoricBrawl => Cow::Borrowed("Historic Brawl"),
            GameFormat::FreeForAll => Cow::Borrowed("Free-for-All"),
            GameFormat::TwoHeadedGiant => Cow::Borrowed("Two-Headed Giant"),
            GameFormat::Archenemy => Cow::Borrowed("Archenemy"),
            GameFormat::Planechase => Cow::Borrowed("Planechase"),
            GameFormat::Momir => Cow::Borrowed("Momir's Madness"),
            GameFormat::Custom(id) => custom_format_registry()
                .into_iter()
                .find(|def| def.rules.id == id)
                .map(|def| Cow::Owned(def.label))
                .unwrap_or(Cow::Borrowed("Custom Format")),
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
                description: "100-card eternal singleton",
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
        if let Some(range) = &self.range_of_influence {
            let max_radius = player_count / 2;
            if range.default_range > max_radius {
                return Err(format!(
                    "range_of_influence.default_range must be at most {max_radius} for {player_count} players"
                ));
            }
            for (&player, &radius) in &range.player_overrides {
                if player.0 >= player_count {
                    return Err(format!(
                        "range_of_influence.player_overrides contains seat {} outside player_count ({player_count})",
                        player.0
                    ));
                }
                if radius > max_radius {
                    return Err(format!(
                        "range_of_influence.player_overrides[{}] must be at most {max_radius} for {player_count} players",
                        player.0
                    ));
                }
            }
        }
        Ok(())
    }

    /// Rejects limited-range configuration until the engine implements its rules.
    pub fn reject_unimplemented_range_of_influence(&self) -> Result<(), String> {
        if self.range_of_influence.is_some() {
            return Err(
                "range_of_influence is not supported until limited-range rules are implemented"
                    .to_string(),
            );
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
            sideboard_policy: GameFormat::Standard.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Commander.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::TinyLeaders.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Oathbreaker.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Brawl.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
        }
    }

    /// Historic Brawl: Brawl's structural rules with the broader Historic card
    /// pool and a 100-card deck (Arena's 100-card Brawl, formerly "Historic
    /// Brawl" — distinct from 60-card Standard Brawl).
    pub fn historic_brawl() -> Self {
        FormatConfig {
            format: GameFormat::HistoricBrawl,
            deck_size: 100,
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
            sideboard_policy: GameFormat::FreeForAll.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Limited.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Momir.sideboard_policy(),
            supplies_fixed_deck: true,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::TwoHeadedGiant.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Planechase.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
            sideboard_policy: GameFormat::Archenemy.sideboard_policy(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
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
    ///
    /// Returns `Err` for `GameFormat::Custom` rather than panicking: this is
    /// a public factory, callable with any `GameFormat` a caller happens to
    /// hold — including one parsed straight from untrusted external input,
    /// since `GameFormat::from_str` accepts any `"Custom:<u16>"` string. A
    /// bare `GameFormat` carries no `CustomFormatRules` to build structural
    /// rules from, so there is no default to fall back to here; callers that
    /// might see a Custom format from an external source must handle the
    /// rejection rather than the function terminating the process.
    pub fn for_format(format: GameFormat) -> Result<Self, FormatConfigError> {
        Ok(match format {
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
            GameFormat::Custom(id) => {
                return Err(FormatConfigError(format!(
                    "for_format cannot resolve ad-hoc Custom format {} structural rules — read custom_rules from the resolved FormatConfig/CustomFormatRules instead",
                    id.0
                )))
            }
        })
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
    fn format_config_brawl_deck_sizes() {
        // Standard Brawl is 60 cards; Historic Brawl (Arena's 100-card Brawl)
        // is 100. Both share the remaining structural rules.
        let brawl = FormatConfig::brawl();
        assert_eq!(brawl.deck_size, 60);
        let historic = FormatConfig::historic_brawl();
        assert_eq!(historic.deck_size, 100);
        assert_eq!(historic.starting_life, brawl.starting_life);
        assert!(historic.singleton);
        assert!(historic.command_zone);
        assert_eq!(historic.commander_damage_threshold, Some(21));
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
        assert!(!GameFormat::Oathbreaker.uses_commander().unwrap());
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
    fn range_of_influence_config_round_trips_with_player_overrides() {
        let config = RangeOfInfluenceConfig {
            default_range: 0,
            player_overrides: BTreeMap::from([(PlayerId(1), 1), (PlayerId(3), 2)]),
        };

        let json = serde_json::to_value(&config).expect("range config serializes");
        assert_eq!(json["default_range"], 0);
        assert_eq!(json["player_overrides"]["1"], 1);
        assert_eq!(json["player_overrides"]["3"], 2);
        assert_eq!(
            serde_json::from_value::<RangeOfInfluenceConfig>(json)
                .expect("range config deserializes"),
            config
        );
    }

    #[test]
    fn range_of_influence_config_defaults_missing_overrides_to_empty() {
        let config: RangeOfInfluenceConfig =
            serde_json::from_str(r#"{"default_range":0}"#).expect("range config deserializes");

        assert_eq!(config.default_range, 0);
        assert!(config.player_overrides.is_empty());
    }

    #[test]
    fn legacy_scalar_range_of_influence_deserializes_and_remains_rejected() {
        let mut serialized = serde_json::to_value(FormatConfig::standard())
            .expect("format config serializes before legacy rewrite");
        serialized["range_of_influence"] = serde_json::json!(1);

        let config: FormatConfig =
            serde_json::from_value(serialized).expect("legacy scalar range config deserializes");

        assert_eq!(
            config.range_of_influence,
            Some(Box::new(RangeOfInfluenceConfig {
                default_range: 1,
                player_overrides: BTreeMap::new(),
            }))
        );
        assert!(config
            .reject_unimplemented_range_of_influence()
            .expect_err("legacy enabled range must reach the normal feature gate")
            .contains("not supported"));
    }

    #[test]
    fn range_of_influence_validation_uses_actual_seating() {
        let mut config = FormatConfig::commander();
        config.range_of_influence = Some(Box::new(RangeOfInfluenceConfig {
            default_range: 0,
            player_overrides: BTreeMap::from([(PlayerId(1), 1), (PlayerId(3), 2)]),
        }));
        assert!(config.validate_for_player_count(4).is_ok());

        config.range_of_influence.as_mut().unwrap().player_overrides =
            BTreeMap::from([(PlayerId(4), 0)]);
        assert!(config
            .validate_for_player_count(4)
            .expect_err("an override must name an occupied seat")
            .contains("outside player_count"));

        config.range_of_influence.as_mut().unwrap().player_overrides =
            BTreeMap::from([(PlayerId(1), 3)]);
        assert!(config
            .validate_for_player_count(4)
            .expect_err("an override cannot exceed the table radius")
            .contains("player_overrides[1]"));

        config.range_of_influence.as_mut().unwrap().player_overrides = BTreeMap::new();
        config.range_of_influence.as_mut().unwrap().default_range = 3;
        assert!(config
            .validate_for_player_count(4)
            .expect_err("a range cannot exceed the table radius")
            .contains("at most 2"));
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
            FormatConfig::for_format(GameFormat::Limited).unwrap(),
            FormatConfig::limited()
        );
    }

    #[test]
    fn premodern_for_format_roundtrip() {
        assert_eq!(
            FormatConfig::for_format(GameFormat::Premodern).unwrap(),
            FormatConfig::premodern()
        );
    }

    #[test]
    fn uses_commander_matches_default_config_and_threshold() {
        // The `GameFormat::uses_commander()` predicate, the derived
        // `FormatConfig::uses_commander` field, and the existence of a
        // commander-damage threshold must all agree for every variant.
        for meta in GameFormat::registry() {
            let expected = meta.format.uses_commander().unwrap();
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
            // The stored `sideboard_policy` field must agree with the bare
            // method for every built-in — real consumers read the stored
            // field precisely so it can diverge safely for Custom, which
            // means it must never silently diverge for a built-in.
            assert_eq!(
                meta.default_config.sideboard_policy,
                meta.format.sideboard_policy(),
                "{:?}: registry default disagrees with sideboard_policy predicate",
                meta.format
            );
        }
        // Variants not in the user-facing registry still respect the invariant.
        for format in [GameFormat::TwoHeadedGiant, GameFormat::Limited] {
            let config = FormatConfig::for_format(format).unwrap();
            assert_eq!(config.uses_commander, format.uses_commander().unwrap());
            assert_eq!(config.supplies_fixed_deck, format.supplies_fixed_deck());
            assert_eq!(config.sideboard_policy, format.sideboard_policy());
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
