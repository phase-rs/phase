use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::database::legality::LegalityFormat;
use crate::types::custom_format::{
    custom_format_registry, passes_legacy_axis_gate, CommandZoneMode, CustomFormatId,
    CustomFormatRules, FormatConfigError,
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
    /// CR 903.13: Commander Draft — a draft (CR 903.13b) followed by a
    /// multiplayer Commander game (CR 903.13a). Deck construction follows
    /// CR 903.5 with CR 903.13f's exceptions: at least 60 cards with no
    /// maximum (f(1)) and no singleton restriction on the drafted pool (f(2)).
    /// CR 903.13g delegates all game rules to CR 903.6-903.11.
    CommanderDraft,
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
            "CommanderDraft" => Ok(GameFormat::CommanderDraft),
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
            GameFormat::CommanderDraft => write!(f, "CommanderDraft"),
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

impl SideboardPolicy {
    /// CR 100.4 / CR 100.4a / CR 903.5e: whether `self` can never admit a
    /// larger sideboard than `ceiling` would. `Forbidden` (no sideboard at
    /// all) permits no more than anything; `Unlimited` permits no more than
    /// `Unlimited` only; `Limited(n)` permits no more than any equal-or-looser
    /// ceiling. A pure permissiveness comparison — never use it to decide
    /// "which format is bigger" in any other sense.
    ///
    /// The single authority `built_in_axes_no_looser_than_rules` uses to
    /// reject a built-in format's payload from declaring a sideboard
    /// allowance looser than `GameFormat::sideboard_policy()` actually
    /// permits. `Forbidden` is the bottom element, which is exactly
    /// `default_sideboard_policy_fallback()` — so a payload serialized
    /// before that field existed is admitted rather than hard-rejected.
    pub fn permits_no_more_than(self, ceiling: Self) -> bool {
        match (self, ceiling) {
            (SideboardPolicy::Forbidden, _) => true,
            (_, SideboardPolicy::Unlimited) => true,
            (SideboardPolicy::Unlimited, _) => false,
            (SideboardPolicy::Limited(_), SideboardPolicy::Forbidden) => false,
            (SideboardPolicy::Limited(n), SideboardPolicy::Limited(m)) => n <= m,
        }
    }
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

impl DeckCopyLimit {
    /// CR 100.2a / CR 100.2b / CR 903.5b: whether `self` can never admit more
    /// copies of a card than `ceiling` would. `Unlimited` permits no more
    /// copies than `Unlimited` only; any `UpTo(n)` permits no more than any
    /// equal-or-looser ceiling. This is a pure permissiveness comparison —
    /// never use it to decide "which format is bigger" in any other sense.
    ///
    /// The single authority `FormatConfig`'s `Deserialize` impl uses to
    /// reject a built-in format's payload from declaring a copy ceiling
    /// looser than `GameFormat::default_deck_copy_limit()` actually allows.
    pub fn permits_no_more_than(self, ceiling: Self) -> bool {
        match (self, ceiling) {
            (DeckCopyLimit::Unlimited, DeckCopyLimit::Unlimited) => true,
            (DeckCopyLimit::UpTo(_), DeckCopyLimit::Unlimited) => true,
            (DeckCopyLimit::Unlimited, DeckCopyLimit::UpTo(_)) => false,
            (DeckCopyLimit::UpTo(n), DeckCopyLimit::UpTo(m)) => n <= m,
        }
    }
}

/// A format's deck-size legality rule: either a floor with no ceiling, or an
/// exact count that is simultaneously the minimum and the maximum.
///
/// - `Minimum(n)`: CR 100.5 — "If a deck must contain at least a certain number
///   of cards, that number is referred to as a minimum deck size. There is no
///   maximum deck size for non-Commander decks." Covers CR 100.2a's 60-card
///   constructed floor and CR 100.2b's 40-card limited floor.
/// - `Exactly(n)`: CR 903.5a — "the minimum deck size and the maximum deck size
///   are both 100" — and the Brawl/Tiny Leaders/Oathbreaker variants that
///   inherit an exact count.
///
/// CR 903.13f(1) is why this axis is typed rather than inferred: Commander
/// Draft is a command-zone format whose deck "must contain at least 60 cards.
/// There is no maximum deck size", so a format's command zone does not predict
/// its deck-size rule. Archenemy (command zone, minimum) and Momir (command
/// zone, exact) already disagreed under the old convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DeckSizeRule {
    Minimum(u16),
    Exactly(u16),
}

impl DeckSizeRule {
    /// CR 100.5 / CR 903.5a: whether `count` satisfies this rule. The single
    /// authority for deck-size legality — callers must not re-derive it with
    /// `<` or `!=` against `min_cards`.
    pub fn accepts(self, count: usize) -> bool {
        match self {
            DeckSizeRule::Minimum(min) => count >= usize::from(min),
            DeckSizeRule::Exactly(exact) => count == usize::from(exact),
        }
    }

    /// The smallest legal deck under this rule. Both variants carry a floor;
    /// `Exactly(n)`'s floor is `n`.
    pub fn min_cards(self) -> u16 {
        match self {
            DeckSizeRule::Minimum(min) => min,
            DeckSizeRule::Exactly(exact) => exact,
        }
    }

    /// Human-readable requirement fragment for validation messages, e.g.
    /// "at least 60" or "exactly 100". Keeps the message honest under
    /// `Minimum`, where the old hardcoded "exactly" would have been false.
    pub fn requirement_phrase(self) -> String {
        match self {
            DeckSizeRule::Minimum(min) => format!("at least {min}"),
            DeckSizeRule::Exactly(exact) => format!("exactly {exact}"),
        }
    }
}

/// Who fixes a format's deck-size MAGNITUDE: the rules, or the table.
///
/// This type carries magnitudes ONLY — never a `DeckSizeRule`. The
/// `Minimum`/`Exactly` DISCRIMINANT is never a table agreement: CR 100.5's
/// "minimum deck size … no maximum deck size for non-Commander decks" and
/// CR 903.5a's "the minimum deck size and the maximum deck size are both 100"
/// are different rules, not looser restatements of each other, and both are
/// live (`DeckSizeRule::accepts`, `DeckSizeRule::min_cards`). Because this
/// type cannot express a discriminant, no registry entry can delegate one even
/// by mistake; the discriminant is pinned structurally by the gate's tuple
/// match instead. The two halves are deliberately separated.
///
/// The magnitude set is CLOSED, never open. `FormatConfig.deck_size` reaches
/// deck admission (`DeckSizeRule::accepts` in `game::deck_validation`) and the
/// between-games sideboard floor (`DeckSizeRule::min_cards` in
/// `game::match_flow`), so an unbounded magnitude would let a host publish a
/// lobby with an arbitrarily low deck floor that every joiner's engine then
/// honors — the same class of hole as a forged `sideboard_policy`.
///
/// Deliberately NOT a `DeckSizeRule` variant and NOT a `FormatConfig` field:
/// this is a registry fact about a format, not part of any format's
/// serialized rules. Keeping it off the wire is what makes it unforgeable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckSizeAuthority {
    /// CR 100.2a / CR 100.2b / CR 903.5a: the magnitude is fixed by the
    /// Comprehensive Rules or by the sanctioned format's own rules.
    RulesFixed,
    /// The magnitude is agreed at the table before play begins; the host picks
    /// one of these. The discriminant stays rules-fixed.
    HostChoiceAmong(&'static [u16]),
}

impl DeckSizeAuthority {
    /// The magnitudes a host may choose. `RulesFixed` returns the empty
    /// slice — the registry value is then the only admissible magnitude, and
    /// any consumer asking "may a host pick here?" can read that as
    /// `options().is_empty()`.
    pub fn options(self) -> &'static [u16] {
        match self {
            DeckSizeAuthority::RulesFixed => &[],
            DeckSizeAuthority::HostChoiceAmong(options) => options,
        }
    }
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

/// CR 100.2a / CR 100.2b / CR 903.5b: Fail-closed default for
/// `FormatConfig.default_deck_copy_limit` on a payload serialized before
/// this field existed. `UpTo(1)` — the same value
/// `GameFormat::Custom(_).default_deck_copy_limit()` already discloses as
/// its own fail-closed fallback — is the tightest possible cap, so a stale
/// payload under-permits rather than silently over-permitting extra copies.
fn default_deck_copy_limit_fallback() -> DeckCopyLimit {
    DeckCopyLimit::UpTo(1)
}

/// Configuration for a game format, describing player counts, starting life, deck rules, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct FormatConfig {
    pub format: GameFormat,
    pub starting_life: i32,
    pub min_players: u8,
    pub max_players: u8,
    /// CR 100.5 / CR 903.5a: the format's deck-size rule. `Minimum(n)` means a
    /// larger deck is legal; `Exactly(n)` means n is both the minimum and the
    /// maximum. The variant is authoritative — never infer exactness from
    /// `command_zone`, which does not predict it (Archenemy is a command-zone
    /// format with a minimum; CR 903.13f(1) makes Commander Draft another).
    /// Compare through `DeckSizeRule::accepts`, never by hand.
    pub deck_size: DeckSizeRule,
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
    /// Engine-derived predicate (mirrors `GameFormat::uses_commander`): true
    /// when the format uses a commander card and the commander-damage
    /// state-based action (CR 903.10a / CR 704.6c) — every format whose
    /// `command_zone` is true and whose `commander_damage_threshold` is
    /// non-`None`. The frontend consumes this directly — it must never
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
    /// Engine-derived, stored per-format default deck-construction copy
    /// ceiling (CR 100.2a / CR 100.2b / CR 903.5b), before per-card printed
    /// overrides and the basic-land exemption — both applied by
    /// `game::deck_validation::max_deck_copies`, the single query authority.
    /// Mirrors `uses_commander`/`supplies_fixed_deck`/`sideboard_policy`'s
    /// stored-field pattern: real consumers must read this field, never
    /// `GameFormat::default_deck_copy_limit()` directly — for a built-in
    /// format the two always agree, but for `GameFormat::Custom` the bare
    /// method has no way to see the real declared limit sitting in
    /// `custom_rules.structural` and would silently discard it.
    /// `#[serde(default)]` fails closed (`UpTo(1)`) for any payload
    /// serialized before this field existed.
    #[serde(default = "default_deck_copy_limit_fallback")]
    pub default_deck_copy_limit: DeckCopyLimit,
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

/// Ceiling on a declared `starting_life` that the built-in-format gate's
/// `HostChoiceWithin` row admits (see that row, below). This is an ENGINE
/// INVARIANT, not a Comprehensive Rules limit — no CR caps how high a
/// variant's starting life total may be, so this constant is never cited
/// with a `CR` annotation. It exists because life-total arithmetic is raw,
/// non-saturating `i32` (e.g. `player.life += (frames - i) as i32;` in
/// `game::engine`'s SBA/effect application), so a `starting_life` admitted
/// at or near `i32::MAX` would overflow the very first time any effect adds
/// to it. The value only bounds the STARTING dial — it leaves the rest of
/// the `i32` range free for in-game life gain/loss to grow into, which is
/// the property that actually matters; it does not re-cap `Player::life`
/// itself. 1,000,000 comfortably clears every real starting total in the
/// registry (Standard 20, Commander/Archenemy 40, Two-Headed Giant 30 shared
/// / 15 per seat) with vast headroom left for house-rule variant play (e.g.
/// a "gigantic life total" casual variant), while leaving over two billion
/// of `i32`'s range for subsequent gameplay-driven life changes.
pub const MAX_STARTING_LIFE: i32 = 1_000_000;

/// CR 100.2a / CR 100.4a / CR 903.5a / CR 903.5b / CR 904.2a: a BUILT-IN
/// format's rules are fixed by the Comprehensive Rules and the engine
/// registry except where the CR itself grants a host a choice — CR 103.4's
/// variant life totals, CR 806's rule-free Free-for-All deck construction,
/// and the seat count no CR fixes (CR 100.1a / CR 100.1b / CR 800.1 fix only
/// that a game begins with two players or with more than two). Re-derive the
/// authoritative config with `FormatConfig::for_format` and check every one
/// of this struct's 17 fields against it under one of six verdicts:
///
/// - Locked: must equal the registry value exactly.
/// - NoLooserThan: must be no more permissive than the registry value,
///   under that axis's own permissiveness order whose BOTTOM element is
///   that axis's `#[serde(default…)]` fallback. That is what keeps legacy
///   payloads (saves, replays, persisted game states) deserializing: they
///   resolve to the bottom, which is never looser.
/// - Derived: a function of Locked fields; re-derived and compared.
/// - HostChoice: a per-session capability orthogonal to format; free.
/// - ShapeLocked(direction): the field's rules-bearing discriminant must
///   match the registry's. `bidirectional` = may neither invent nor delete.
///   `one-directional` = may not invent, may decline. Any value the shape
///   carries is governed by that row's second verdict, stated alongside.
///   Applies to: `deck_size` (bidirectional, the `DeckSizeRule` discriminant),
///   `commander_damage_threshold` (bidirectional, the `Option` discriminant),
///   `archenemy_player` (one-directional, the `Option` discriminant — behavior
///   unchanged, this is a relabel of its existing row).
/// - HostChoiceWithin(set): free inside a stated admissible set; each row
///   names its set and the set's source. Applies to: `max_players` (registry
///   range `min_players..=max_players`), `deck_size`'s magnitude (the
///   registry's closed option list), `commander_damage_threshold`'s magnitude
///   (`>= 1`, playability), `starting_life` (resolved per-seat total `>= 1`
///   for playability; raw declared value `<= MAX_STARTING_LIFE` as an engine
///   overflow-safety invariant, not a rules bound).
///
/// `validate_for_player_count` is the runtime pair of the `max_players` row:
/// that row bounds the format invariant a payload may declare, the other
/// bounds the seat count a session is actually built with.
///
/// The Custom counterpart is the `Some(rules)` arm below, which can demand
/// blanket equality because no Custom payload has ever been accepted at this
/// boundary and there are therefore no legacy Custom payloads to keep
/// compatible with. This function is the built-in half of the same idea.
fn built_in_axes_no_looser_than_rules(config: &FormatConfig) -> Result<(), String> {
    let rules = FormatConfig::for_format(config.format).map_err(|e| e.0)?;

    // format: Locked — discharged by construction. `config.format` is the
    // very key `for_format` was looked up by, so equality is tautological.

    // starting_life: HostChoiceWithin — free, except that every seat must be
    // able to start the game.
    //
    // CR 103.4: "Each player begins the game with a starting life total of
    // 20. Some variant games have different starting life totals." That
    // licenses a VARIANT total; it does not by itself hand the number to a
    // host. What does is the product: the shipped host UI has always exposed
    // `starting_life` for every format, `server-core`'s
    // `create_game_honors_a_configured_starting_life` documents it as
    // supported, and before this gate existed the built-in arm checked only
    // `default_deck_copy_limit` — so an equality row here was a NEW
    // restriction on shipped behavior. No registry ceiling exists to be
    // looser than.
    //
    // The one real bound is playability. CR 704.5a: "If a player has 0 or
    // less life, that player loses the game." A total resolving to 0 or less
    // means every seat loses at the first state-based-action check and no
    // game can start. For a shared-life team format the corresponding rule is
    // CR 810.8c ("If a team's life total is 0 or less, the team loses the
    // game"); CR 810.4 sets Two-Headed Giant's shared total at 30, which this
    // engine represents as a per-seat half (see `starting_life_for_seat`), so
    // a raw `starting_life: 1` resolves to 0 per seat and must be rejected.
    //
    // Checked through `starting_life_for_seat` rather than the raw field, and
    // rather than `starting_life_for_player` (which is what `GameState::new`
    // actually calls), for three reasons. (1) The two agree exactly on
    // `IndividualSeats` and `FixedTeams` — the only topologies where this
    // field is live — so nothing is lost. (2) On `OneVsMany`,
    // `starting_life_for_player` returns CR 904.5's hardcoded 40/20 and
    // ignores this field entirely, so it would validate nothing;
    // `starting_life_for_seat` returns the declared value and is therefore
    // strictly MORE conservative, refusing to admit a nonsense value that
    // would become live if Archenemy ever starts honoring the field. (3) It
    // needs no `PlayerId`; passing an arbitrary seat index would be a
    // question the deserializer has no basis to answer.
    // NOTE: `starting_life_for_seat` has no other production caller — this
    // row is its first. A future edit to it changes this gate; keep the two
    // in step (see the pinning test on `starting_life_for_seat` itself).
    //
    // The upper bound is a sibling engineering invariant, not a rules row:
    // see `MAX_STARTING_LIFE`'s own doc comment for why it exists and why it
    // carries no CR citation. It is checked against the raw declared field,
    // not `starting_life_for_seat`, because the floor's playability concern
    // (a seat that cannot survive the first SBA check) is a per-seat
    // question but the overflow concern is about the field's own magnitude
    // before it is ever divided.
    //
    // Scope: this row is part of `built_in_axes_no_looser_than_rules`, which
    // only runs for built-in formats (the `None` arm of the `custom_rules`
    // match in `FormatConfig::deserialize`). A Custom format's starting life
    // is instead re-derived from `custom_rules.structural.starting_life` and
    // checked by blanket equality in the `Some(rules)` arm, which has no
    // magnitude bound of its own — that arm is out of scope for this change.
    if config.starting_life_for_seat() < 1 {
        return Err(format!(
            "FormatConfig.starting_life is {}, which resolves to {} per seat for {} — every seat \
             must begin above 0 life or the game ends immediately at the first \
             state-based-action check (CR 704.5a; CR 810.8c for shared-life team formats)",
            config.starting_life,
            config.starting_life_for_seat(),
            config.format,
        ));
    }
    if config.starting_life > MAX_STARTING_LIFE {
        return Err(format!(
            "FormatConfig.starting_life is {}, but the engine caps a declared starting life at \
             {MAX_STARTING_LIFE} to keep in-game life-total arithmetic (raw i32) from overflowing \
             — this is an engine invariant, not a Comprehensive Rules limit",
            config.starting_life,
        ));
    }

    // min_players: Locked — no serde default.
    if config.min_players != rules.min_players {
        return Err(format!(
            "FormatConfig.min_players is {}, but {} requires exactly {} — a built-in format's \
             player-count floor is fixed by the engine's format registry",
            config.min_players, config.format, rules.min_players,
        ));
    }

    // max_players: HostChoiceWithin `rules.min_players..=rules.max_players`.
    //
    // The seat count a host opens a table at is a per-session choice, not a
    // rule: CR 100.1a / CR 100.1b and CR 800.1 fix only that a two-player
    // game begins with two players and a multiplayer game with more than two.
    // No CR seats Commander at six — the registry ceiling is an
    // engine/product convention, so the old message's "fixed by the
    // Comprehensive Rules" was false.
    //
    // The admissible set is the registry's own inclusive range. Its floor is
    // `rules.min_players`, which the Locked row above has already forced
    // `config.min_players` to equal, so a payload can never declare a ceiling
    // below its own floor.
    //
    // This field is load-bearing, not decorative: the browser host path
    // submits the chosen seat count through it and reads it straight back out
    // as the wire `player_count`, and `seat_reducer::types::seat_team_info`
    // reads it as the seat-index bound on the Archenemy arm. When
    // `player_count` and this field disagree, `player_count` wins —
    // `create_game_n_players` sizes every per-seat vector from it and
    // `GameState::new` seats exactly that many. That is safe ONLY because
    // `validate_for_player_count` now bounds `player_count` against this same
    // registry range (see that function); the two checks are a pair and must
    // not be separated. This row additionally covers the boundaries that see
    // a `FormatConfig` with no `player_count` beside it — save/replay restore
    // and deck-compatibility requests.
    if !(rules.min_players..=rules.max_players).contains(&config.max_players) {
        return Err(format!(
            "FormatConfig.max_players is {}, but {} seats {}-{} — a built-in format's seat count \
             is a host choice inside the engine registry's own range for that format, never \
             outside it",
            config.max_players, config.format, rules.min_players, rules.max_players,
        ));
    }

    // deck_size: ShapeLocked(bidirectional) on the DeckSizeRule DISCRIMINANT,
    // plus HostChoiceWithin the registry's closed option list on the
    // MAGNITUDE. The two halves are checked separately and neither can
    // substitute for the other.
    //
    // DISCRIMINANT — always locked, in both directions. CR 100.5's "minimum
    // with no maximum" and CR 903.5a's "exactly 100" are not comparable under
    // any sound permissiveness order, and CR 903.13f(1) makes Commander Draft
    // a command-zone format with a minimum, so the discriminant cannot be
    // inferred from any other field either. It is pinned STRUCTURALLY by the
    // tuple match below: the two cross arms return `false` unconditionally
    // and are NEVER routed through the option list. `DeckSizeAuthority`
    // carries bare `u16` magnitudes precisely so that no registry entry can
    // express a discriminant change even by accident. Concretely:
    // Free-for-All's `HostChoiceAmong(&[60, 40])` admits `Minimum(40)` and
    // rejects `Exactly(40)`.
    //
    // MAGNITUDE — locked too, EXCEPT where the registry declares the
    // format's deck size a table agreement
    // (`GameFormat::deck_size_authority`), whose option list is a CLOSED set.
    // Today only Free-for-All delegates, to {60, 40}. The set must stay
    // closed because this field is live in `DeckSizeRule::accepts`
    // (`game::deck_validation`) and `DeckSizeRule::min_cards`
    // (`game::match_flow`).
    let deck_size_ok = match (config.deck_size, rules.deck_size) {
        // Same discriminant: fall through to the magnitude rule.
        (DeckSizeRule::Minimum(declared), DeckSizeRule::Minimum(registry))
        | (DeckSizeRule::Exactly(declared), DeckSizeRule::Exactly(registry)) => {
            declared == registry
                || config
                    .format
                    .deck_size_authority()
                    .options()
                    .contains(&declared)
        }
        // Different discriminant: refused outright, never consulted against
        // the option list. This is the arm that makes the magnitude set safe.
        (DeckSizeRule::Minimum(_), DeckSizeRule::Exactly(_))
        | (DeckSizeRule::Exactly(_), DeckSizeRule::Minimum(_)) => false,
    };
    if !deck_size_ok {
        return Err(format!(
            "FormatConfig.deck_size is {:?}, but {} requires {:?}{} — a built-in format's \
             Minimum/Exactly rule shape is fixed by the Comprehensive Rules and is never a host \
             choice; only the count may vary, only where the format's deck size is a table \
             agreement, and only to a listed option",
            config.deck_size,
            config.format,
            rules.deck_size,
            match config.format.deck_size_authority().options() {
                [] => String::new(),
                options => format!(" (count may also be one of {options:?})"),
            },
        ));
    }

    // singleton: Locked — no serde default.
    if config.singleton != rules.singleton {
        return Err(format!(
            "FormatConfig.singleton is {}, but {} requires exactly {} — a built-in format's \
             singleton rule is fixed by the Comprehensive Rules",
            config.singleton, config.format, rules.singleton,
        ));
    }

    // command_zone: Locked — CR 408.1. No default.
    if config.command_zone != rules.command_zone {
        return Err(format!(
            "FormatConfig.command_zone is {}, but {} requires exactly {} — a built-in format's \
             command-zone usage is fixed by the Comprehensive Rules",
            config.command_zone, config.format, rules.command_zone,
        ));
    }

    // commander_damage_threshold: ShapeLocked(bidirectional) on the Option
    // discriminant, plus HostChoiceWithin `>= 1` on the magnitude.
    //
    // The SHAPE is the rules fact (CR 903.10a / CR 704.6c): either the format
    // has the commander-damage state-based action or it does not. `None` is
    // not a "looser threshold" — it DELETES the SBA (the commander-damage
    // loss collectors both return early on `None`) and flips the Derived
    // `uses_commander` row below, so a payload declaring both `None` and
    // `uses_commander: false` would otherwise be self-consistent and slip
    // past every other row. A payload may neither invent nor delete it.
    //
    // The MAGNITUDE is a house-rule dial, exposed by the shipped host UI
    // whenever the format has a threshold at all and unvalidated on this path
    // before this gate existed. CR 903.10a fixes 21 for sanctioned Commander;
    // the engine treats only the SBA's EXISTENCE as rules-fixed and reads the
    // number straight from the config, so a table playing to 30 is as
    // well-defined as one playing to 21. Zero is not: `damage >= 0` holds for
    // any tracked entry, making `Some(0)` an immediate CR 704.6c loss rather
    // than a threshold.
    match (
        config.commander_damage_threshold,
        rules.commander_damage_threshold,
    ) {
        (Some(declared), None) => {
            return Err(format!(
                "FormatConfig.commander_damage_threshold is Some({declared}), but {} does not \
                 use the commander-damage state-based action at all — only commander-damage \
                 formats may declare this field",
                config.format,
            ));
        }
        (None, Some(registry)) => {
            return Err(format!(
                "FormatConfig.commander_damage_threshold is null, but {} loses a player to \
                 {registry} combat damage from one commander (CR 903.10a / CR 704.6c) — a \
                 payload may set the threshold but never remove it",
                config.format,
            ));
        }
        (Some(0), Some(_)) => {
            return Err(format!(
                "FormatConfig.commander_damage_threshold is Some(0) for {} — a zero threshold \
                 makes any tracked commander damage an immediate loss (CR 704.6c), so the \
                 threshold must be at least 1",
                config.format,
            ));
        }
        (Some(_), Some(_)) | (None, None) => {}
    }

    // range_of_influence: Deferred, NOT checked here — a hard equality row
    // was tried and reverted. It's unsound: legacy payloads don't only omit
    // this field (which would resolve to the `None` default and be safe),
    // they can also carry the OLD LEGACY SCALAR SHAPE that
    // `RangeOfInfluenceConfigWire::Legacy` (this file, ~line 380) exists
    // specifically to deserialize and migrate into `Some(RangeOfInfluenceConfig
    // { .. })` — see `legacy_scalar_range_of_influence_deserializes_and_
    // remains_rejected` in this file's own test module. A hard equality check
    // here would turn that legitimate legacy payload into a `FormatConfig::
    // deserialize` failure instead of letting it deserialize and then be
    // cleanly rejected downstream. Admission for this field is the sole
    // responsibility of the existing single authority,
    // `reject_unimplemented_range_of_influence`, which already runs at every
    // real ingress boundary (engine-wasm, lobby-broker, phase-server,
    // server-core, game_state) — not re-checked here.

    // team_based: Locked — no serde default.
    if config.team_based != rules.team_based {
        return Err(format!(
            "FormatConfig.team_based is {}, but {} requires exactly {} — a built-in format's \
             team structure is fixed by the Comprehensive Rules",
            config.team_based, config.format, rules.team_based,
        ));
    }

    // archenemy_player: ShapeLocked(one-directional) on the Option
    // discriminant — a built-in outside the Archenemy family may not invent
    // one — plus HostChoice on the seat index itself, bound `Deferred` to
    // `FormatConfig::validate_for_player_count`. Behavior and the check below
    // are unchanged; this is a comment-only relabel for taxonomy consistency.
    // CR 904.2a / CR 904.6 only fix that an archenemy exists and takes the
    // first turn — not which numbered seat holds it. Which seat is designated
    // is per-seating-table state (see `custom_format::validate_custom_rules_
    // consistency`'s doc comment on this same field), and the engine already
    // supports a non-zero archenemy seat: `FormatConfig::validate_for_
    // player_count` bounds-checks a *variable* seat index against the actual
    // player count, and both `lobby-broker::inbound_guard` and
    // `phase-server::main` carry tests for the non-zero-seat path. Seat 0
    // being the only reachable value today is a client-side limitation
    // (`client/src/data/formatRegistry.ts` hardcodes it), not an engine
    // invariant this gate should encode as a hard rejection.
    //
    // The one thing still worth rejecting: a built-in format outside the
    // Archenemy family (whose registry value is always `None`) declaring a
    // seat at all, since only Archenemy-family formats use this field.
    if rules.archenemy_player.is_none() && config.archenemy_player.is_some() {
        return Err(format!(
            "FormatConfig.archenemy_player is {:?}, but {} does not use an archenemy \
             designation at all — only Archenemy-family formats may declare this field",
            config.archenemy_player, config.format,
        ));
    }

    // uses_commander: Derived — command_zone && commander_damage_threshold
    // is_some(), both Locked above, so for a built-in the derivation
    // coincides with registry equality. No serde default, so no legacy
    // concern.
    let derived_uses_commander = config.command_zone && config.commander_damage_threshold.is_some();
    if config.uses_commander != derived_uses_commander {
        return Err(format!(
            "FormatConfig.uses_commander is {}, but command_zone ({}) and \
             commander_damage_threshold ({:?}) derive {} — uses_commander must always equal that \
             derivation",
            config.uses_commander,
            config.command_zone,
            config.commander_damage_threshold,
            derived_uses_commander,
        ));
    }

    // supplies_fixed_deck: NoLooserThan over false <= true. true is the
    // permissive value (it bypasses deck-selection gates); false demands
    // more of the player. Fallback false is the bottom, so this is
    // migratable now.
    if config.supplies_fixed_deck && !rules.supplies_fixed_deck {
        return Err(format!(
            "FormatConfig.supplies_fixed_deck is true, but {} does not supply a fixed deck — a \
             built-in format's fixed-deck status is fixed by the Comprehensive Rules; a payload \
             may declare false but never true",
            config.format,
        ));
    }

    // sideboard_policy: NoLooserThan over Forbidden < Limited(n) <=
    // Limited(m>=n) < Unlimited. Fallback Forbidden is the bottom, so this
    // is migratable now. CR 100.4 / CR 100.4a / CR 903.5e.
    if !config
        .sideboard_policy
        .permits_no_more_than(rules.sideboard_policy)
    {
        return Err(format!(
            "FormatConfig.sideboard_policy is {:?}, which is more permissive than {} allows \
             ({:?}) — a built-in format's sideboard policy is fixed by the Comprehensive Rules; \
             a payload may declare an equal-or-stricter value but never a looser one",
            config.sideboard_policy, config.format, rules.sideboard_policy,
        ));
    }

    // default_deck_copy_limit: NoLooserThan via the existing
    // DeckCopyLimit::permits_no_more_than. Fallback UpTo(1) is the bottom.
    // CR 100.2a / CR 100.2b / CR 903.5b.
    if !config
        .default_deck_copy_limit
        .permits_no_more_than(rules.default_deck_copy_limit)
    {
        return Err(format!(
            "FormatConfig.default_deck_copy_limit is {:?}, which is more permissive than {} \
             allows ({:?}) — a built-in format's default copy limit is fixed by the \
             Comprehensive Rules; a payload may declare an equal-or-stricter value but never a \
             looser one",
            config.default_deck_copy_limit, config.format, rules.default_deck_copy_limit,
        ));
    }

    // allow_debug_actions: HostChoice — session capability, orthogonal to
    // format. Free.

    // custom_rules: Locked (None) — the built-in arm is defined by
    // custom_rules == None; the biconditional is established upstream by
    // validate_custom_rules_consistency.

    Ok(())
}

impl<'de> Deserialize<'de> for FormatConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let config = Self::deserialize(deserializer)?;
        crate::types::custom_format::validate_custom_rules_consistency(&config)
            .map_err(serde::de::Error::custom)?;
        // `validate_custom_rules_consistency` has already established the
        // `format == Custom(id) <=> custom_rules == Some(rules with that id)`
        // biconditional, so `Some(_)` here means a Custom config and `None`
        // means a built-in one.
        match &config.custom_rules {
            Some(rules) => {
                // `command_zone`/`commander_damage_threshold`/
                // `uses_commander`/`singleton`/`sideboard_policy`/... are this
                // struct's own independently-serialized runtime fields. For a
                // built-in format they are always consistent because a
                // `FormatConfig::x()` builder derived them together; for
                // `Custom`, `custom_rules.structural` and these bare fields
                // are two independently-writable representations of the same
                // state. Phase 1a rejected every Custom payload outright
                // because no resolver existed to reconcile them. One does
                // now: re-derive the whole config from `custom_rules` and
                // demand equality, so the declaration is authoritative and no
                // payload can smuggle in a runtime field its own
                // `StructuralRules` does not entail.
                if !passes_legacy_axis_gate(&rules.legality.legacy) {
                    return Err(serde::de::Error::custom(
                        "FormatConfig.custom_rules declares a LegacyRuleSet axis the engine does \
                         not implement yet — accepting it would promise historical rules \
                         behavior (mana burn, combat-damage timing, Wish reach, legend-rule \
                         scope) that no engine code enforces",
                    ));
                }
                let mut expected = FormatConfig::for_custom_rules(rules);
                // `allow_debug_actions` is the one field the resolver cannot
                // derive: it is a session capability (sandbox debug actions),
                // orthogonal to format, chosen per game rather than declared
                // by the ruleset. Every other field must match exactly.
                expected.allow_debug_actions = config.allow_debug_actions;
                if config != expected {
                    // Reports the derived target values rather than dumping
                    // both whole structs: `custom_rules.legality`'s
                    // banned/restricted/legal_sets lists are caller-sized and
                    // have no business in an error string.
                    return Err(serde::de::Error::custom(format!(
                        "FormatConfig for {} contradicts its own custom_rules.structural — every \
                         runtime field must be exactly what FormatConfig::for_custom_rules \
                         derives from the declared rules (allow_debug_actions excepted). Derived: \
                         starting_life {}, players {}-{}, deck_size {:?}, singleton {}, \
                         command_zone {}, commander_damage_threshold {:?}, uses_commander {}, \
                         team_based {}, archenemy_player {:?}, supplies_fixed_deck {}, \
                         sideboard_policy {:?}, default_deck_copy_limit {:?}, \
                         range_of_influence set {}",
                        config.format,
                        expected.starting_life,
                        expected.min_players,
                        expected.max_players,
                        expected.deck_size,
                        expected.singleton,
                        expected.command_zone,
                        expected.commander_damage_threshold,
                        expected.uses_commander,
                        expected.team_based,
                        expected.archenemy_player,
                        expected.supplies_fixed_deck,
                        expected.sideboard_policy,
                        expected.default_deck_copy_limit,
                        expected.range_of_influence.is_some(),
                    )));
                }
            }
            // A built-in format's rules are fixed by the Comprehensive Rules
            // and the engine registry, except where the CR itself grants a
            // host a choice (see `built_in_axes_no_looser_than_rules`'s own
            // doc comment for the full verdict list, now six: Locked,
            // NoLooserThan, Derived, HostChoice, ShapeLocked, and
            // HostChoiceWithin). `built_in_axes_no_looser_than_rules`
            // re-derives the authoritative config via `FormatConfig::for_format`
            // and checks every one of this struct's 17 fields against it —
            // absorbing what was previously a single ad hoc
            // `default_deck_copy_limit` check as one of its 17 rows, rather
            // than adding a parallel second check. `range_of_influence`'s row
            // is a documented `Deferred` non-check (see that function's own
            // comment on the field), not an omission.
            None => {
                built_in_axes_no_looser_than_rules(&config).map_err(serde::de::Error::custom)?
            }
        }
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

/// The format a deck-compatibility request is being validated against.
///
/// Wire-Inertness Invariant — the load-bearing security property of this
/// type, stated here as four checkable clauses:
///
/// (1) This type's wire form is the bare `GameFormat` tag in BOTH
///     directions. `Resolved` has no JSON representation. `Serialize`
///     delegates to `self.tag().serialize(s)`; `Deserialize` is
///     `GameFormat::deserialize(d).map(SelectedFormat::Tag)`. Both impls are
///     hand-written, never derived, so serde never emits or accepts an
///     externally-tagged `{"Tag":…}` / `{"Resolved":…}` envelope.
///
/// (2) The complete, exhaustive list of places `SelectedFormat::Resolved` is
///     CONSTRUCTED is exactly one: `validate_name_deck_for_format_full` in
///     `game::deck_validation`, trusted Rust that already holds a
///     `&FormatConfig` supplied by its caller. After any change touching
///     this type, run `grep -rn "SelectedFormat::Resolved" --include=*.rs
///     crates/` and manually triage every hit into one of three buckets: the
///     single production construction site above; a `match`/pattern-match
///     arm that only READS an already-existing value (e.g. `tag()`'s and
///     `rules()`'s own arms in this file, and this doc comment's own prose)
///     — expected and unlimited in count, never a violation; or a
///     `#[cfg(test)] mod tests` block. A naive `grep -v
///     "tests/\|#\[cfg(test)\]"` text filter looks appealing but does NOT
///     work here: `deck_validation.rs` and this very file each have their
///     own `#[cfg(test)] mod tests` block thousands of lines above their
///     in-module test construction sites, and a line-based `grep -v` cannot
///     see "this line lives inside that module" — it can only filter lines
///     that literally contain the attribute text. Do not trust a bare
///     non-test-directory line count from this command; read each hit.
///
/// (3) The gate `built_in_axes_no_looser_than_rules` runs at every
///     `FormatConfig` construction site with no exceptions, because every
///     `FormatConfig` an in-memory `Resolved` can carry was itself produced
///     by `FormatConfig::for_format` (registry-authored) or
///     `FormatConfig::deserialize` (the gated ingress).
///
/// (4) Therefore every untrusted `DeckCompatibilityRequest` WASM boundary
///     can only ever receive `SelectedFormat::Tag` — safe by construction,
///     not by the absence of a test failure. A `Tag(Custom(_))` arriving
///     there yields `rules() == Err`, which every consumer already handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectedFormat {
    /// A format NAME, and nothing more. The only variant any deserializer
    /// can ever produce.
    Tag(GameFormat),
    /// A fully-resolved rule set, constructible ONLY by trusted Rust — see
    /// the Wire-Inertness Invariant above. Boxed for the same
    /// `large_enum_variant` reason `custom_rules` and `range_of_influence`
    /// are (`:463-473` above).
    Resolved(Box<FormatConfig>),
}

impl SelectedFormat {
    /// The format name, regardless of whether this is a bare tag or a fully
    /// resolved config.
    pub fn tag(&self) -> GameFormat {
        match self {
            SelectedFormat::Tag(format) => *format,
            SelectedFormat::Resolved(config) => config.format,
        }
    }

    /// `Err` only for `Tag(Custom(_))`: a bare tag cannot resolve a custom
    /// format's rules. `Resolved` is always `Ok`, INCLUDING for Custom —
    /// that is the entire point of the variant.
    pub fn rules(&self) -> Result<Cow<'_, FormatConfig>, FormatConfigError> {
        match self {
            SelectedFormat::Tag(format) => FormatConfig::for_format(*format).map(Cow::Owned),
            SelectedFormat::Resolved(config) => Ok(Cow::Borrowed(config)),
        }
    }
}

impl Serialize for SelectedFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.tag().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SelectedFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        GameFormat::deserialize(deserializer).map(SelectedFormat::Tag)
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
            // CR 903.13e: the drafted cards become the card pool, so no
            // constructed legality table applies — as for Limited.
            | GameFormat::CommanderDraft
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
            // CR 903.13f routes deck construction through CR 903.5, and the
            // Commander family has no sideboard.
            | GameFormat::CommanderDraft
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
            // CR 903.13f(2): a Commander Draft deck "may include any number of
            // cards from that player's card pool with the same name", so
            // CR 903.5b's singleton rule does NOT apply — this deliberately
            // does not join the `UpTo(1)` Commander group.
            | GameFormat::CommanderDraft
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

    /// Whether this format's deck-size MAGNITUDE is rules-fixed or agreed at
    /// the table, and if agreed, the closed set a host may choose from. The
    /// `Minimum`/`Exactly` discriminant is always rules-fixed — see
    /// [`DeckSizeAuthority`], which cannot express one.
    ///
    /// `RulesFixed` for every sanctioned format: CR 100.2a's 60-card
    /// constructed minimum, CR 100.2b's 40-card limited minimum, CR 903.5a's
    /// exact 100, and the supplementary-deck formats that build a constructed
    /// deck (CR 100.2d).
    ///
    /// `HostChoiceAmong(&[60, 40])` for Free-for-All alone. CR 806
    /// ("Free-for-All Variant") specifies seating, attack options and range of
    /// influence, and CR 806.2 states that "any multiplayer options used are
    /// determined before play begins" — it imposes NO deck-construction rule,
    /// so the registry's `Minimum(60)` is a default convention, not a rule.
    /// The engine already treats Free-for-All as non-constructed elsewhere:
    /// `default_deck_copy_limit` returns `Unlimited` for it, deliberately not
    /// CR 100.2a's `UpTo(4)`.
    ///
    /// This list is the SINGLE place these magnitudes exist in the repository.
    pub fn deck_size_authority(self) -> DeckSizeAuthority {
        match self {
            GameFormat::FreeForAll => DeckSizeAuthority::HostChoiceAmong(&[60, 40]),
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
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::CommanderDraft => DeckSizeAuthority::RulesFixed,
            // R6 (review round 3): exhaustive, no wildcard. Unreachable by
            // construction from the admission gate: `for_format` returns Err
            // for Custom before any verdict row runs, and a Custom payload is
            // settled by the `Some(rules)` arm's blanket equality against
            // `FormatConfig::for_custom_rules`. `RulesFixed` is the
            // total-function answer for other callers, not a policy choice.
            GameFormat::Custom(_) => DeckSizeAuthority::RulesFixed,
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
    /// state-based action (CR 903.10a / CR 704.6c). True for every format
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
            | GameFormat::HistoricBrawl
            // CR 903.13g: Commander Draft games follow the same rules as
            // Commander games, so CR 903.10a's commander-damage SBA applies.
            | GameFormat::CommanderDraft => Ok(true),
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
            // CR 903.13e: the drafted cards become the player's card pool and
            // they build a deck from it, so the engine supplies nothing.
            | GameFormat::CommanderDraft
            | GameFormat::Planechase => false,
            // No custom-format use case for an engine-supplied fixed deck
            // exists today — a real one would need its own design, analogous
            // to Momir's Madness.
            GameFormat::Custom(_) => false,
        }
    }

    /// True for a built-in format whose `game::deck_loading` behavior grants
    /// an auxiliary deck or component keyed on this literal `GameFormat`
    /// variant, with no `StructuralRules` field able to represent it: a
    /// shared communal planar deck (Planechase, CR 901.15a,
    /// `load_shared_planar_deck`), a supplementary scheme deck (Archenemy,
    /// CR 904.3, `load_shared_scheme_deck`), or a game-start emblem (Momir,
    /// CR 109.4c / CR 114.1, `grant_emblem`). A custom-format definition
    /// modeled after one of these would resolve to a config that looks
    /// structurally sound but never receives the grant, since
    /// `deck_loading.rs` checks `state.format_config.format ==
    /// GameFormat::X` directly rather than reading any `StructuralRules`
    /// field.
    ///
    /// Used by `custom_format::CustomFormatDef::from_lobby_config` to reject
    /// all three as lobby-config sources. Archenemy and Momir both also set
    /// `command_zone: true` with no `CommanderEligibilityRule`, so they are
    /// independently unrepresentable for that reason too; Planechase's
    /// `command_zone` is `false`, so this predicate is the only guard that
    /// reaches it.
    pub fn has_unrepresentable_auxiliary_deck_component(self) -> bool {
        match self {
            GameFormat::Planechase | GameFormat::Archenemy | GameFormat::Momir => true,
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
            | GameFormat::CommanderDraft => false,
            // Exhaustive rather than `matches!`, matching `supplies_fixed_deck`'s
            // style: a future built-in that grants its own deck_loading.rs
            // auxiliary component must force a deliberate `true`/`false` choice
            // here, not silently default to unrepresented. No custom-format use
            // case for this exists today.
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
            GameFormat::CommanderDraft => Cow::Borrowed("Commander Draft"),
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
                format: GameFormat::CommanderDraft,
                label: "Commander Draft",
                short_label: "CDR",
                description: "Drafted 60-card minimum Commander, 3\u{2013}8 players",
                group: FormatGroup::Commander,
                default_config: FormatConfig::commander_draft(),
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

    /// NOTE: `built_in_axes_no_looser_than_rules`'s `starting_life` admission
    /// row now depends on this function's `FixedTeams` division behavior
    /// (declared life divided by `team_size`) to floor every seat above 0 —
    /// see that row's own comment and the pinning test on this function.
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

    /// Bounds a runtime seat count against this format's registry range. Owns
    /// the seat-count bound and is the runtime pair of the `max_players`
    /// admission row in `built_in_axes_no_looser_than_rules`: that row bounds
    /// the format invariant a payload may declare, this one bounds the seat
    /// count a session is actually built with.
    ///
    /// No protocol-version bump accompanies this: no wire *shape* changed (no
    /// new field on any `Serialize`/`Deserialize` type), but this IS a
    /// behavioral tightening — a `player_count` outside the format's range
    /// that an older server would have accepted is now rejected.
    ///
    /// At most call sites (both ingress guards, the WASM session boundary,
    /// session creation) that rejection is a retryable wire rejection: the
    /// client resubmits a corrected request and no state is lost. It is NOT
    /// retryable at `server_core::session::GameSession::from_persisted` — the
    /// one call site checked against a PERSISTED `player_count` rather than
    /// one just supplied on an inbound request. There, this same rejection
    /// aborts the restore permanently: a session already saved with a
    /// `player_count` outside its format's registry range can never be
    /// restored again. This is reachable, not merely hypothetical — a
    /// Commander session (registry range 2..=6) persisted while
    /// `player_count` was 8 (`lobby-broker::inbound_guard`'s ingress clamp
    /// admits up to `MAX_PLAYER_COUNT` = 8, independent of the format's own
    /// range, at the time the session was created) becomes permanently
    /// unrestorable the moment this bound is enforced. Whether to repair or
    /// clamp such a persisted blob at the restore boundary is tracked
    /// separately from this comment; this paragraph is disclosure only.
    pub fn validate_for_player_count(&self, player_count: u8) -> Result<(), String> {
        // CR 100.1a / CR 100.1b / CR 800.1: a two-player game begins with two
        // players and a multiplayer game with more than two; the exact seat
        // count a format admits is the engine registry's, not the
        // Comprehensive Rules'. This is the runtime half of the `max_players`
        // admission row in `built_in_axes_no_looser_than_rules`, and the two
        // are a PAIR: that row bounds the format invariant a payload may
        // declare, this one bounds the seat count a session is actually built
        // with. Without this check, loosening `max_players` from Locked to a
        // range would leave the seat-count axis unenforced — a payload could
        // declare `max_players: 2` (admissible) alongside `player_count: 8`
        // (previously bounded only by a global clamp that is not
        // format-relative), and session creation would allocate eight seats
        // for a six-seat format.
        //
        // This function is the single authority already called at every site
        // where a seat count meets a `FormatConfig` — both ingress guards,
        // the WASM session boundary, session creation, and `from_persisted`
        // — which is why the bound belongs here rather than in a new
        // per-caller check.
        //
        // A format whose floor exceeds two (Commander Draft's three,
        // Two-Headed Giant's four per CR 810.1's "two teams of two players
        // each") will now reject a request that a global clamp previously
        // floored at two. That is a correct rejection: such a session was
        // structurally broken before (a 2-seat "Two-Headed Giant" game is not
        // a Two-Headed Giant game). The ingress `clamp(2, MAX_PLAYER_COUNT)`
        // guards only ever RAISE a sub-2 value — they cannot produce a
        // sub-`min_players` `player_count` from otherwise-legitimate traffic,
        // so they are not the source of this rejection; a request naming a
        // seat count below a format's own floor is what triggers it. If a
        // caller is found to depend on the old behavior, that caller must
        // pass the format's own `min_players` — this bound must not be
        // weakened.
        if player_count < self.min_players || player_count > self.max_players {
            return Err(format!(
                "player_count {player_count} is outside {}'s seat range {}-{}",
                self.format, self.min_players, self.max_players,
            ));
        }
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
            deck_size: DeckSizeRule::Minimum(60),
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::Standard.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Standard.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Exactly(100),
            singleton: true,
            command_zone: true,
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: true,
            sideboard_policy: GameFormat::Commander.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Commander.default_deck_copy_limit(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            custom_rules: None,
        }
    }

    /// CR 903.13: Commander Draft. A Commander game (CR 903.13g -> CR 903.6-903.11)
    /// whose deck construction takes CR 903.13f's exceptions.
    pub fn commander_draft() -> Self {
        FormatConfig {
            format: GameFormat::CommanderDraft,
            // CR 903.7: each player sets their life total to 40.
            starting_life: 40,
            // CR 903.13a + CR 800.1: "a draft ... followed by a multiplayer
            // game", and a multiplayer game "begins with more than two
            // players" - so three seats is the floor. Matches
            // DraftProcedure::min_pod_size for DraftKind::CommanderDraft.
            min_players: 3,
            // The draft pod becomes the game; draft_wire_guard admits pods up
            // to MAX_PLAYER_COUNT (8), and seat-reducer rejects any seat index
            // >= max_players, so a narrower ceiling here would reject seats the
            // draft already seated. CR 903.13 fixes no pod size.
            max_players: 8,
            // CR 903.13f(1): "must contain at least 60 cards. There is no
            // maximum deck size."
            deck_size: DeckSizeRule::Minimum(60),
            // CR 903.13f(2): the deck "may include any number of cards from
            // that player's card pool with the same name" - CR 903.5b's
            // singleton rule does not apply.
            singleton: false,
            command_zone: true,
            // CR 903.10a (via CR 903.13g): 21 combat damage from one commander.
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: true,
            sideboard_policy: GameFormat::CommanderDraft.sideboard_policy(),
            default_deck_copy_limit: GameFormat::CommanderDraft.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Exactly(50),
            singleton: true,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::TinyLeaders.sideboard_policy(),
            default_deck_copy_limit: GameFormat::TinyLeaders.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Exactly(60),
            singleton: true,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::Oathbreaker.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Oathbreaker.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Exactly(60),
            singleton: true,
            command_zone: true,
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: true,
            sideboard_policy: GameFormat::Brawl.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Brawl.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Exactly(100),
            ..Self::brawl()
        }
    }

    pub fn free_for_all() -> Self {
        FormatConfig {
            format: GameFormat::FreeForAll,
            starting_life: 20,
            min_players: 2,
            max_players: 6,
            deck_size: DeckSizeRule::Minimum(60),
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::FreeForAll.sideboard_policy(),
            default_deck_copy_limit: GameFormat::FreeForAll.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Minimum(40),
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::Limited.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Limited.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Exactly(60),
            singleton: false,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::Momir.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Momir.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Minimum(60),
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: true,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::TwoHeadedGiant.sideboard_policy(),
            default_deck_copy_limit: GameFormat::TwoHeadedGiant.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Minimum(60),
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::Planechase.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Planechase.default_deck_copy_limit(),
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
            deck_size: DeckSizeRule::Minimum(60),
            singleton: false,
            command_zone: true,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: Some(PlayerId(0)),
            uses_commander: false,
            sideboard_policy: GameFormat::Archenemy.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Archenemy.default_deck_copy_limit(),
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
            GameFormat::CommanderDraft => Self::commander_draft(),
            GameFormat::Custom(id) => {
                return Err(FormatConfigError(format!(
                    "for_format cannot resolve ad-hoc Custom format {} structural rules — read custom_rules from the resolved FormatConfig/CustomFormatRules instead",
                    id.0
                )))
            }
        })
    }

    /// The single authoritative `CustomFormatRules -> FormatConfig` resolver:
    /// turns a saved definition (an Axis-A lobby save, or an Axis-B registry
    /// preset once those exist) into the live, active config a game actually
    /// runs on. The counterpart to `for_format` for custom formats, and the
    /// inverse of `custom_format::CustomFormatDef::from_lobby_config`.
    ///
    /// Total and infallible, unlike `for_format`: a `CustomFormatRules` value
    /// carries every structural field this needs, so there is no
    /// "unresolvable" input to report. That totality is load-bearing — the
    /// `Deserialize` impl below re-derives an expected config with this
    /// function and demands equality, which only works if every ingress
    /// produces exactly one config per rule set.
    ///
    /// Field mapping:
    /// - Direct copies from `rules.structural`: `starting_life`,
    ///   `min_players`, `max_players`, `deck_size`, `singleton`,
    ///   `range_of_influence`, `team_based`, `sideboard_policy`,
    ///   `default_deck_copy_limit`.
    /// - `CommandZoneMode`-derived (CR 408.1 / CR 903.10a): `Disabled` gives
    ///   `command_zone: false`, `commander_damage_threshold: None`,
    ///   `uses_commander: false`. `Enabled` gives `command_zone: true`, the
    ///   declared threshold unchanged, and `uses_commander:
    ///   threshold.is_some()` — NOT unconditionally `true`, because a command
    ///   zone without a commander-damage threshold is a real, supported
    ///   format class (Tiny Leaders, Oathbreaker), and `uses_commander`'s own
    ///   contract is `command_zone && commander_damage_threshold.is_some()`.
    ///   `eligibility_rule` is not mirrored at all: `FormatConfig` has no
    ///   such field, and the commander-eligibility check reads it from
    ///   `custom_rules.structural` directly.
    /// - Fixed: `format`/`custom_rules` are set consistently (satisfying
    ///   `validate_custom_rules_consistency` by construction);
    ///   `supplies_fixed_deck: false` (no custom-format use case for an
    ///   engine-supplied fixed deck exists — a deliberate exclusion, not an
    ///   oversight); `archenemy_player: None` (per-seating table state, not a
    ///   format rule); `allow_debug_actions: false` (a session capability
    ///   orthogonal to format — apply `with_sandbox()` afterwards).
    pub fn for_custom_rules(rules: &CustomFormatRules) -> FormatConfig {
        let structural = &rules.structural;
        let (command_zone, commander_damage_threshold) = match structural.command_zone_mode {
            CommandZoneMode::Disabled => (false, None),
            CommandZoneMode::Enabled {
                commander_damage_threshold,
                ..
            } => (true, commander_damage_threshold),
        };
        FormatConfig {
            format: GameFormat::Custom(rules.id),
            starting_life: structural.starting_life,
            min_players: structural.min_players,
            max_players: structural.max_players,
            deck_size: structural.deck_size,
            singleton: structural.singleton,
            command_zone,
            commander_damage_threshold,
            range_of_influence: structural.range_of_influence.clone(),
            team_based: structural.team_based,
            archenemy_player: None,
            // CR 903.10a / CR 704.6c: the commander-damage SBA exists only
            // when a threshold does.
            uses_commander: command_zone && commander_damage_threshold.is_some(),
            supplies_fixed_deck: false,
            sideboard_policy: structural.sideboard_policy,
            default_deck_copy_limit: structural.default_deck_copy_limit,
            allow_debug_actions: false,
            custom_rules: Some(Box::new(rules.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deck_copy_limit_permits_no_more_than_is_a_sound_permissiveness_order() {
        use DeckCopyLimit::*;
        assert!(Unlimited.permits_no_more_than(Unlimited));
        assert!(!Unlimited.permits_no_more_than(UpTo(4)));
        assert!(UpTo(4).permits_no_more_than(Unlimited));
        assert!(UpTo(1).permits_no_more_than(UpTo(4)));
        assert!(UpTo(4).permits_no_more_than(UpTo(4)));
        assert!(!UpTo(5).permits_no_more_than(UpTo(4)));
    }

    #[test]
    fn sideboard_policy_permits_no_more_than_is_a_sound_permissiveness_order() {
        use SideboardPolicy::*;
        assert!(Forbidden.permits_no_more_than(Forbidden));
        assert!(Forbidden.permits_no_more_than(Limited(15)));
        assert!(Forbidden.permits_no_more_than(Unlimited));
        assert!(!Limited(15).permits_no_more_than(Forbidden));
        assert!(Limited(1).permits_no_more_than(Limited(15)));
        assert!(Limited(15).permits_no_more_than(Limited(15)));
        assert!(!Limited(16).permits_no_more_than(Limited(15)));
        assert!(Limited(15).permits_no_more_than(Unlimited));
        assert!(Unlimited.permits_no_more_than(Unlimited));
        assert!(!Unlimited.permits_no_more_than(Limited(15)));
        assert!(!Unlimited.permits_no_more_than(Forbidden));
    }

    /// Registry-completeness guard for `DeckSizeAuthority`: every
    /// `HostChoiceAmong` option list contains that format's OWN registry
    /// magnitude (so `for_format`'s own output always passes its own gate),
    /// `RulesFixed.options()` is empty, and the set is neither empty nor
    /// all-`HostChoiceAmong` — a real building block, not a one-format
    /// special case masquerading as one.
    ///
    /// Deliberately does NOT pin which format(s) or how many delegate their
    /// deck-size magnitude to the table (formerly `GameFormat::FreeForAll`
    /// and exactly one) — that was a frozen-count/frozen-identity assertion
    /// on a set this phase's own registry is designed to grow (e.g. a future
    /// `HostChoiceAmong` preset), not a property the building block actually
    /// requires. See the R4 review note on this test.
    #[test]
    fn deck_size_authority_registry_completeness() {
        let mut host_choice_count = 0;
        let mut rules_fixed_count = 0;
        for meta in GameFormat::registry() {
            let authority = meta.format.deck_size_authority();
            match authority {
                DeckSizeAuthority::HostChoiceAmong(options) => {
                    host_choice_count += 1;
                    let registry_magnitude = meta.default_config.deck_size.min_cards();
                    assert!(
                        options.contains(&registry_magnitude),
                        "{:?}'s own registry magnitude {registry_magnitude} must be in its own \
                         option list {options:?}, or for_format's own output would fail this gate",
                        meta.format
                    );
                }
                DeckSizeAuthority::RulesFixed => {
                    rules_fixed_count += 1;
                    assert!(authority.options().is_empty());
                }
            }
        }
        assert!(rules_fixed_count > 0, "the set must not be empty");
        assert!(
            host_choice_count < GameFormat::registry().len(),
            "the set must not be all-HostChoiceAmong"
        );

        // Unreachable-by-construction from the admission gate (see S2/R6):
        // `for_format` returns `Err` for `Custom` before any verdict row
        // runs, and a Custom payload is settled by the `Some(rules)` arm's
        // blanket equality against `FormatConfig::for_custom_rules`.
        // `RulesFixed` is the total-function answer for other callers, not a
        // policy choice — not a behavior claim about any reachable path.
        assert_eq!(
            GameFormat::Custom(CustomFormatId(0)).deck_size_authority(),
            DeckSizeAuthority::RulesFixed
        );
    }

    /// Compiler-enforced completeness guard for `built_in_axes_no_looser_
    /// than_rules`: adding a field to `FormatConfig` without updating this
    /// exhaustive destructure is a compile error, which forces the author to
    /// also confirm the new field is classified (Locked / NoLooserThan /
    /// HostChoice / Derived / ShapeLocked / HostChoiceWithin) in that gate.
    /// Replaces a prior arithmetic guard (`serialized_json_object.len() + 2
    /// == 17`) that a future field sharing `archenemy_player`/`custom_rules`'s
    /// `skip_serializing_if`-when-`None` shape could silently defeat without
    /// moving the count.
    #[test]
    fn format_config_field_destructure_is_exhaustive() {
        let config = FormatConfig::standard();
        let FormatConfig {
            format: _,
            starting_life: _,
            min_players: _,
            max_players: _,
            deck_size: _,
            singleton: _,
            command_zone: _,
            commander_damage_threshold: _,
            range_of_influence: _,
            team_based: _,
            archenemy_player: _,
            uses_commander: _,
            supplies_fixed_deck: _,
            sideboard_policy: _,
            default_deck_copy_limit: _,
            allow_debug_actions: _,
            custom_rules: _,
        } = config;
    }

    /// V1 (Verification Matrix): `SelectedFormat`'s wire form is the bare
    /// `GameFormat` tag in BOTH directions.
    #[test]
    fn selected_format_tag_round_trips_as_a_bare_game_format_string() {
        let from_wire: SelectedFormat =
            serde_json::from_value(serde_json::json!("Standard")).unwrap();
        assert_eq!(from_wire, SelectedFormat::Tag(GameFormat::Standard));

        let to_wire = serde_json::to_value(SelectedFormat::Tag(GameFormat::Standard)).unwrap();
        assert_eq!(to_wire, serde_json::json!("Standard"));

        let custom: SelectedFormat = serde_json::from_value(serde_json::json!("Custom:7")).unwrap();
        assert_eq!(
            custom,
            SelectedFormat::Tag(GameFormat::Custom(
                crate::types::custom_format::CustomFormatId(7)
            ))
        );

        assert!(serde_json::from_value::<SelectedFormat>(serde_json::json!("Nonsense")).is_err());
    }

    /// V2: `Resolved` has NO JSON representation — it serializes to exactly
    /// the bare tag, and round-tripping it back always yields `Tag`, never
    /// `Resolved`. This is the structural half of the Wire-Inertness
    /// Invariant: the resolved payload cannot survive a wire round-trip.
    #[test]
    fn selected_format_resolved_serializes_to_the_bare_tag_and_returns_as_tag() {
        let mut config = FormatConfig::standard();
        config.default_deck_copy_limit = DeckCopyLimit::UpTo(1);
        let resolved = SelectedFormat::Resolved(Box::new(config));

        let wire = serde_json::to_value(&resolved).unwrap();
        assert_eq!(wire, serde_json::json!("Standard"));

        let round_tripped: SelectedFormat = serde_json::from_value(wire).unwrap();
        assert_eq!(round_tripped, SelectedFormat::Tag(GameFormat::Standard));
    }

    /// V3: neither externally-tagged envelope shape deserializes.
    #[test]
    fn selected_format_rejects_both_externally_tagged_envelopes() {
        assert!(
            serde_json::from_value::<SelectedFormat>(serde_json::json!({"Tag": "Standard"}))
                .is_err()
        );
        assert!(serde_json::from_value::<SelectedFormat>(serde_json::json!({
            "Resolved": serde_json::to_value(FormatConfig::standard()).unwrap()
        }))
        .is_err());
    }

    #[test]
    fn format_config_standard() {
        let config = FormatConfig::standard();
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, DeckSizeRule::Minimum(60));
        assert!(!config.singleton);
        assert!(!config.command_zone);
        assert_eq!(config.commander_damage_threshold, None);
        assert!(!config.team_based);
        assert_eq!(config.default_deck_copy_limit, DeckCopyLimit::UpTo(4));
    }

    #[test]
    fn format_config_commander() {
        let config = FormatConfig::commander();
        assert_eq!(config.starting_life, 40);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 6);
        assert_eq!(config.deck_size, DeckSizeRule::Exactly(100));
        assert!(config.singleton);
        assert!(config.command_zone);
        assert_eq!(config.commander_damage_threshold, Some(21));
        assert!(!config.team_based);
        assert_eq!(config.default_deck_copy_limit, DeckCopyLimit::UpTo(1));
    }

    /// CR 903.5a vs CR 903.13f(1): the two command-zone deck-size rules are
    /// different rules, and `DeckSizeRule` is what keeps them apart. Both
    /// directions are required - the positive half alone would be satisfied by
    /// deleting the exactness check.
    #[test]
    fn commander_deck_is_exactly_100_but_commander_draft_is_min_60() {
        let draft = FormatConfig::for_format(GameFormat::CommanderDraft)
            .unwrap()
            .deck_size;
        let commander = FormatConfig::for_format(GameFormat::Commander)
            .unwrap()
            .deck_size;

        // CR 903.13f(1): "at least 60 cards. There is no maximum deck size."
        assert!(draft.accepts(60), "60 cards is the CR 903.13f(1) floor");
        assert!(draft.accepts(61), "a 61-card Commander Draft deck is legal");
        assert!(
            !draft.accepts(59),
            "59 cards is below the CR 903.13f(1) floor"
        );

        // CR 903.5a: 100 is both the minimum and the maximum.
        assert!(
            commander.accepts(100),
            "100 cards is a legal Commander deck"
        );
        assert!(
            !commander.accepts(101),
            "CR 903.5a caps Commander at 100 - a Minimum rule here would pass"
        );
        assert!(
            !commander.accepts(99),
            "99 cards is below the CR 903.5a minimum"
        );
    }

    /// CR 903.13: the Commander Draft preset, per subrule.
    #[test]
    fn commander_draft_format_config_matches_cr() {
        let config = FormatConfig::for_format(GameFormat::CommanderDraft).unwrap();
        assert_eq!(config.starting_life, 40, "CR 903.7");
        assert!(config.uses_commander, "CR 903.13g -> CR 903.6-903.11");
        assert_eq!(config.commander_damage_threshold, Some(21), "CR 903.10a");
        assert!(
            !config.singleton,
            "CR 903.13f(2): any number of same-name cards"
        );
        assert_eq!(
            config.deck_size,
            DeckSizeRule::Minimum(60),
            "CR 903.13f(1): at least 60 cards, no maximum"
        );
        assert_ne!(
            config.deck_size,
            DeckSizeRule::Exactly(60),
            "CR 903.13f(1) forbids the exact-size rule the old inference would have produced"
        );
        assert!(config.command_zone);
        assert_eq!(config.min_players, 3, "CR 903.13a + CR 800.1");
    }

    #[test]
    fn format_config_tiny_leaders() {
        let config = FormatConfig::tiny_leaders();
        assert_eq!(config.format, GameFormat::TinyLeaders);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, DeckSizeRule::Exactly(50));
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
        assert_eq!(config.deck_size, DeckSizeRule::Minimum(60));
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
        assert_eq!(brawl.deck_size, DeckSizeRule::Exactly(60));
        let historic = FormatConfig::historic_brawl();
        assert_eq!(historic.deck_size, DeckSizeRule::Exactly(100));
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
        assert_eq!(config.deck_size, DeckSizeRule::Minimum(60));
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
    fn deck_size_rule_serializes_as_tagged_union() {
        // Both variants carry their count in `data`; the frontend must switch
        // on `.type`, never assume a minimum. Mirrored by hand in
        // client/src/adapter/types.ts and by both adapter-contract fixtures.
        let minimum = serde_json::to_string(&DeckSizeRule::Minimum(60)).unwrap();
        assert_eq!(minimum, r#"{"type":"Minimum","data":60}"#);

        let exactly = serde_json::to_string(&DeckSizeRule::Exactly(100)).unwrap();
        assert_eq!(exactly, r#"{"type":"Exactly","data":100}"#);

        // Round-trips both directions.
        let parsed: DeckSizeRule = serde_json::from_str(r#"{"type":"Minimum","data":60}"#).unwrap();
        assert_eq!(parsed, DeckSizeRule::Minimum(60));
        let parsed: DeckSizeRule =
            serde_json::from_str(r#"{"type":"Exactly","data":100}"#).unwrap();
        assert_eq!(parsed, DeckSizeRule::Exactly(100));
    }

    #[test]
    fn format_config_oathbreaker() {
        let config = FormatConfig::oathbreaker();
        assert_eq!(config.format, GameFormat::Oathbreaker);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 4);
        assert_eq!(config.deck_size, DeckSizeRule::Exactly(60));
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

    /// The new S7 seat-count bound: every registry format admits every seat
    /// count in its own `min_players..=max_players` range.
    #[test]
    fn validate_for_player_count_accepts_every_seat_in_the_registry_range() {
        for meta in GameFormat::registry() {
            for n in meta.default_config.min_players..=meta.default_config.max_players {
                assert!(
                    meta.default_config.validate_for_player_count(n).is_ok(),
                    "{:?}: {n} is within {}..={} and must be accepted",
                    meta.format,
                    meta.default_config.min_players,
                    meta.default_config.max_players,
                );
            }
        }
    }

    /// Commander's own registry range (2-6) rejects outside it.
    #[test]
    fn validate_for_player_count_rejects_outside_commander_range() {
        let config = FormatConfig::commander();
        assert!(config.validate_for_player_count(7).is_err());
        assert!(config.validate_for_player_count(1).is_err());
    }

    /// The residual-hazard case named in the plan's S7 comment: Two-Headed
    /// Giant's registry range (CR 810.1's "two teams of two players each") is
    /// the single point 4..=4, so both a below-floor 2-seat request and an
    /// above-ceiling 5-seat request must be rejected. Neither rejection
    /// traces to the ingress `clamp(2, MAX_PLAYER_COUNT)` guards: those only
    /// ever RAISE a sub-2 value, which cannot explain the 5-seat rejection,
    /// and produces exactly 2 for a sub-2 input — never reaching this
    /// format's own floor check on a value the clamp already handled. Both
    /// rejections are deliberate consequences of the format's own registry
    /// range, not an ingress-clamp artifact.
    #[test]
    fn validate_for_player_count_two_headed_giant_rejects_outside_its_registry_range() {
        let config = FormatConfig::two_headed_giant();
        assert!(config.validate_for_player_count(4).is_ok());
        assert!(config.validate_for_player_count(2).is_err());
        assert!(config.validate_for_player_count(5).is_err());
    }

    /// The admission gate and the runtime seat-count validator are
    /// INDEPENDENT layers: the gate admits a config that is internally
    /// consistent with the registry, but a caller can still hand that config
    /// an inconsistent runtime `player_count` — this is exactly what S7
    /// closes and this test proves the two checks are not the same check.
    #[test]
    fn deserialize_admission_and_validate_for_player_count_are_independent_layers() {
        let mut host_config = FormatConfig::commander();
        host_config.max_players = 2;
        let json = serde_json::to_value(&host_config).unwrap();
        let config: FormatConfig =
            serde_json::from_value(json).expect("max_players: 2 is within Commander's own range");

        assert!(config.validate_for_player_count(2).is_ok());
        assert!(
            config.validate_for_player_count(8).is_err(),
            "the deserialize gate admitting the config must not imply every runtime \
             player_count is admitted too"
        );
    }

    /// Pins both ends of the `starting_life` admission row: the existing
    /// floor (a per-seat total that cannot survive the first SBA check) and
    /// the new `MAX_STARTING_LIFE` ceiling (an engine overflow-safety
    /// invariant, not a rules bound). Exercised through the same
    /// `serde_json` round trip as `deserialize_admission_and_validate_for_
    /// player_count_are_independent_layers`, since `built_in_axes_no_looser_
    /// than_rules` runs inside `FormatConfig::deserialize`.
    #[test]
    fn starting_life_admission_gate_pins_both_bounds() {
        let mut floor_config = FormatConfig::standard();
        floor_config.starting_life = 0;
        let floor_json = serde_json::to_value(&floor_config).unwrap();
        let floor_err = serde_json::from_value::<FormatConfig>(floor_json)
            .expect_err("0 starting life loses every seat at the first SBA check");
        assert!(floor_err.to_string().contains("must begin above 0"));

        let mut at_ceiling = FormatConfig::standard();
        at_ceiling.starting_life = MAX_STARTING_LIFE;
        let at_ceiling_json = serde_json::to_value(&at_ceiling).unwrap();
        assert!(
            serde_json::from_value::<FormatConfig>(at_ceiling_json).is_ok(),
            "MAX_STARTING_LIFE itself must remain admissible"
        );

        let mut over_ceiling = FormatConfig::standard();
        over_ceiling.starting_life = MAX_STARTING_LIFE + 1;
        let over_ceiling_json = serde_json::to_value(&over_ceiling).unwrap();
        let over_ceiling_err = serde_json::from_value::<FormatConfig>(over_ceiling_json)
            .expect_err("MAX_STARTING_LIFE + 1 must be rejected");
        assert!(over_ceiling_err.to_string().contains("engine caps"));
    }

    /// Pin `starting_life_for_seat`'s `FixedTeams` division behavior: the
    /// `starting_life` admission gate row depends on it dividing the declared
    /// total evenly across `team_size`, and a future change to that behavior
    /// must break this test loudly rather than silently changing the gate.
    #[test]
    fn starting_life_for_seat_pins_fixed_teams_division() {
        let config = FormatConfig::two_headed_giant();
        assert_eq!(config.starting_life, 30);
        assert_eq!(
            config.topology(),
            FormatTopology::FixedTeams {
                team_size: 2,
                team_count: 2,
                turn_structure: TurnStructure::SharedTeamTurns,
            }
        );
        assert_eq!(
            config.starting_life_for_seat(),
            config.starting_life / 2,
            "FixedTeams must divide the declared total evenly by team_size"
        );
        assert_eq!(config.starting_life_for_seat(), 15);
    }

    #[test]
    fn format_config_limited() {
        let config = FormatConfig::limited();
        assert_eq!(config.format, GameFormat::Limited);
        assert_eq!(config.starting_life, 20);
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 2);
        assert_eq!(config.deck_size, DeckSizeRule::Minimum(40));
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
            assert_eq!(
                meta.default_config.default_deck_copy_limit,
                meta.format.default_deck_copy_limit(),
                "{:?}: registry default disagrees with default_deck_copy_limit predicate",
                meta.format
            );
        }
        // Variants not in the user-facing registry still respect the invariant.
        for format in [GameFormat::TwoHeadedGiant, GameFormat::Limited] {
            let config = FormatConfig::for_format(format).unwrap();
            assert_eq!(config.uses_commander, format.uses_commander().unwrap());
            assert_eq!(config.supplies_fixed_deck, format.supplies_fixed_deck());
            assert_eq!(config.sideboard_policy, format.sideboard_policy());
            assert_eq!(
                config.default_deck_copy_limit,
                format.default_deck_copy_limit()
            );
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
        assert_eq!(entry.default_config.deck_size, DeckSizeRule::Minimum(60));
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
