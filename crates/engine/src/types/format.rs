pub use phase_ast::types::format::*;

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::database::legality::LegalityFormat;
use crate::types::card::Rarity;
use crate::types::custom_format::{
    custom_format_registry, passes_legacy_axis_gate, CommandZoneMode, CustomFormatId,
    CustomFormatRules, FormatConfigError,
};
use crate::types::match_config::MatchType;
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
    /// `GameFormat::legality_key` of `format`: the key the engine's legality
    /// maps record this format's legality table under, or `None` when the card
    /// data records no table for it.
    pub legality_key: Option<&'static str>,
    pub default_config: FormatConfig,
}

/// Supported game formats.
///
/// `GameFormat::iter()` yields exactly the **built-in** formats, in declaration
/// order: `Custom(CustomFormatId)` is `#[strum(disabled)]` because it is not a
/// member of a fixed set and cannot be enumerated.
///
/// What this iterator is the authority for, at the width it actually holds.
/// Not every test that calls `GameFormat::iter()` is coupled to this exact
/// membership — a test that loops it to assert a per-format property is
/// coupled only to that property.
/// `grep -rn 'GameFormat::iter()' crates/engine/` finds every call site
/// (filter out doc-comment mentions like this one) — that search, not a
/// hand-copied list here, is what enumerates them.
///
/// THREE KINDS of neighbouring guard do NOT work that way, and the difference
/// matters if this comment is read as a promise. The exhaustive `match`
/// statements over the format set — in this file, in `types/custom_format.rs`,
/// in `game/deck_validation.rs` and in `crates/engine-wasm/src/lib.rs` — are
/// held by the COMPILER: a new variant reds them with `E0004`, not against this
/// iterator and not with an assertion name. And
/// the client's `FORMAT_SHAPES` table in
/// `client/src/services/__tests__/deckUrlImport.test.ts` is keyed by
/// `BuiltInGameFormat`, so the TypeScript compiler is what forces its key set;
/// it reaches this iterator only transitively, through
/// `tests::client_builtin_game_format_union_matches_the_engine`. And
/// `tests::client_format_registry_matches_the_engine_registry` compares the
/// client mirror against `registry()`, not against this iterator; it reaches
/// this iterator only transitively, through
/// `tests::registry_lists_every_builtin_format`.
///
/// Exhaustiveness comes from `EnumIter`, not from the length of a hand-written
/// array, which is why no `BUILTINS` constant exists here to drift from the
/// declaration below. The in-crate precedents for an `EnumIter` derive are
/// `parser/oracle_ir/feature.rs` and `types/oracle_diagnostic.rs`;
/// `types/keywords.rs` and `types/statics.rs` carry the same IDIOM — a derived
/// figure rather than a hand-maintained one — but through `strum::EnumCount`,
/// not `EnumIter`.
///
/// Two things are worth knowing before editing this derive. A second
/// payload-carrying variant left without `#[strum(disabled)]` is a COMPILE
/// ERROR only if ITS OWN payload type does not implement `Default` — true of
/// `CustomFormatId` today (a newtype over `u16`, one `#[derive(Default)]` away
/// from no longer gating anything), but the gate is the new variant's payload,
/// not `CustomFormatId` itself. When the payload DOES implement `Default`,
/// `iter()` instead SILENTLY gains a default-valued member — the same failure
/// shape as `#[strum(disabled)]` on a UNIT variant removing it from `iter()`
/// SILENTLY. Both silent cases are caught by
/// `tests::registry_lists_every_builtin_format`, which compares `iter()` to
/// the hand-written `registry()` below (mutation-tested: `#[strum(disabled)]`
/// on `Momir` reds it; `grep -rn 'GameFormat::iter()' crates/engine/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumIter)]
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
    /// Two-Headed Giant Commander: CR 810 (two teams of two sharing life,
    /// poison, and loss; shared team turns) composed with CR 903 (100-card
    /// singleton decks, command zone, 21 commander damage). The CR defines no
    /// combined variant — no CR number below names it — so the two points the
    /// CR leaves open follow played consensus, documented here rather than
    /// cited: each team's shared life total starts at 60 (MTGO and paper
    /// 2HG Commander), and commander damage tracks per player per CR 903.10a,
    /// with the dealt head losing at 21 and its team losing with it per
    /// CR 810.8a (some groups pool damage per team instead; the engine
    /// implements the CR-compositional reading).
    TwoHeadedGiantCommander,
    /// CR 904: Default Archenemy — one archenemy faces a team of heroes using
    /// shared team turns (CR 805), with a single scheme deck (CR 904.3).
    Archenemy,
    /// CR 904.13 (Archenemy Commander option): the archenemy starts at 60
    /// life (CR 904.13b) while the opposing heroes share a 60-life team
    /// total under CR 810.8/CR 810.9; poison is NOT shared — the archenemy
    /// loses at 10+ and each hero loses individually at 10+ (CR 904.13c);
    /// the scheme deck holds at least ten singleton schemes (CR 904.13d).
    /// Player decks, command zone, and 21 commander damage follow CR 903.
    ArchenemyCommander,
    /// CR 901: Planechase using the single communal planar deck option
    /// (CR 901.15a), plus normal 60-card player decks.
    Planechase,
    /// Commander (CR 903: 100 singleton, command zone, 21 commander damage,
    /// 40 life, 2–6 seats) composed with the single communal planar deck
    /// option (CR 901.15a). Like Two-Headed Giant Commander this is a
    /// played-consensus composition — no combined CR rule exists — with
    /// Commander governing construction/life/seating and Planechase adding
    /// the planar deck, starting-plane reveal, and die rolling unchanged.
    PlanechaseCommander,
    /// Momir's Madness: 60 snow basic lands (12 each, no Snow-Covered Wastes),
    /// 20 life, a game-start command-zone emblem granting "{X}, Discard a card:
    /// Create a token that's a copy of a creature card with mana value X chosen
    /// at random."
    Momir,
    /// Horde Magic: a cooperative variant where a team of survivors faces a
    /// single self-piloting "Horde" deck. Casual community format (not
    /// DCI/CR-sanctioned); it reuses CR mechanisms the engine already models —
    /// shared team turns (CR 805) for the survivor team and the one-vs-many
    /// topology used by Archenemy (CR 904). The Horde has no life total, takes
    /// scripted "reveal and resolve" turns, and loses by decking out. Which
    /// concrete deck is played and how each Horde turn reveals cards are carried
    /// by `FormatConfig::horde_ruleset` (`HordeRuleset`), never by sibling
    /// `GameFormat` variants.
    Horde,
    /// CR 903.13: Commander Draft — a draft (CR 903.13b) followed by a
    /// multiplayer Commander game (CR 903.13a). Deck construction follows
    /// CR 903.5 with CR 903.13f's exceptions: at least 60 cards with no
    /// maximum (f(1)) and no singleton restriction on the drafted pool (f(2)).
    /// CR 903.13g delegates all game rules to CR 903.6-903.11.
    CommanderDraft,
    /// Casual 1v1 constructed variant: every set is in the pool, no ban list,
    /// no copy limit and no main-deck minimum. It keeps CR 100.4a's
    /// fifteen-card sideboard.
    Freeform,
    /// Casual multiplayer command-zone variant. Any card that can be CAST may
    /// be designated as its commander — a departure from CR 903.3's legendary
    /// creature / Vehicle / Spacecraft requirement — but a land may not: a
    /// commander is cast from the command zone under an additional cost
    /// (CR 903.8) and a land is played as a special action rather than cast
    /// (CR 305.1), so the tax has nothing to attach to. Every set is in the
    /// pool, there is no ban list, no copy limit and no main-deck minimum.
    /// Two commanders only where partner families admit the pair.
    FreeformCommander,
    /// Dandân (Secret Lair Dandân): two players draw from one 80-card pile (the
    /// Secret Lair list by default, host-supplied otherwise) and share one
    /// graveyard, in a best-of-one match. It departs from CR 400.1
    /// (shared library and graveyard), CR 108.3 / CR 110.2 (a card's owner is the
    /// player who took it into their hand), CR 121.2c (simultaneous draws are dealt
    /// one card at a time) and CR 103.5 (a free mulligan by revealing a hand); its
    /// match is capped at one game (CR 100.6a describes the usual two-win match,
    /// CR 100.4 sideboarding between games). Each departure is a typed axis method
    /// on this enum.
    Dandan,
    /// An engine-validated custom format. Resolves via
    /// `FormatConfig.custom_rules` (see `types::custom_format`) — a bare
    /// `GameFormat::Custom(id)` alone cannot fully answer several of this
    /// enum's methods; see each method's doc comment for how it handles
    /// `Custom`.
    #[strum(disabled)]
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
            "TwoHeadedGiantCommander" => Ok(GameFormat::TwoHeadedGiantCommander),
            "Archenemy" => Ok(GameFormat::Archenemy),
            "ArchenemyCommander" => Ok(GameFormat::ArchenemyCommander),
            "Planechase" => Ok(GameFormat::Planechase),
            "PlanechaseCommander" => Ok(GameFormat::PlanechaseCommander),
            "Momir" => Ok(GameFormat::Momir),
            "Horde" => Ok(GameFormat::Horde),
            "CommanderDraft" => Ok(GameFormat::CommanderDraft),
            "Freeform" => Ok(GameFormat::Freeform),
            "FreeformCommander" => Ok(GameFormat::FreeformCommander),
            "Dandan" => Ok(GameFormat::Dandan),
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
            GameFormat::TwoHeadedGiantCommander => write!(f, "TwoHeadedGiantCommander"),
            GameFormat::Archenemy => write!(f, "Archenemy"),
            GameFormat::ArchenemyCommander => write!(f, "ArchenemyCommander"),
            GameFormat::Planechase => write!(f, "Planechase"),
            GameFormat::PlanechaseCommander => write!(f, "PlanechaseCommander"),
            GameFormat::Momir => write!(f, "Momir"),
            GameFormat::Horde => write!(f, "Horde"),
            GameFormat::CommanderDraft => write!(f, "CommanderDraft"),
            GameFormat::Freeform => write!(f, "Freeform"),
            GameFormat::FreeformCommander => write!(f, "FreeformCommander"),
            GameFormat::Dandan => write!(f, "Dandan"),
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

/// Which authority decides whether a card is in a format's deck-construction
/// card pool.
///
/// CR 100.6 is why this is an engine axis rather than a Comprehensive Rules
/// one: ban and restricted lists live in the tournament rules, not the CR
/// ("These rules may limit the use of some cards, including barring all cards
/// from some older sets"). A format declaring that nothing is restricted is
/// making a statement the CR permits, not overriding one it makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardPool {
    /// The engine's legality table for this `LegalityFormat` answers each card.
    LegalityTable(LegalityFormat),
    /// No legality table answers this format's pool, and the engine makes no
    /// claim that nothing is restricted — that is the whole distinction from
    /// `Unrestricted`. No `CardPoolAuthority` built from this variant ever
    /// refuses a card on pool grounds, but a format may still restrict its
    /// own deck by a construction rule that lives outside `CardPool`
    /// entirely, as `evaluate_momir` does for Momir's five snow basics.
    NoEngineAuthority,
    /// The format's own rules positively declare that no card is restricted.
    /// Silence is not such a declaration: a multiplayer variant that states no
    /// deck-construction rule at all is `NoEngineAuthority`. This variant is
    /// what lets a Constructed-group format decline a legality table without
    /// the registry invariant reading it as an unplaced default.
    Unrestricted,
    /// `GameFormat::Custom` only: the authority is the format's own declared
    /// `LegalityRules`, which a bare `GameFormat` cannot see. The declaration
    /// names the authority, never the answer.
    DeclaredRules,
}

/// CR 702.124a: partner abilities "modify the rules for deck construction in
/// the Commander variant," so whether a format honors them is a per-format
/// deck-construction fact, not a card fact. CR 702.124g caps any partner
/// combination at two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommanderPairing {
    /// No commander is designated from the decklist.
    NoCommander,
    /// Exactly one commander; CR 702.124's partner families are not consulted.
    Solo,
    /// One commander, or two when CR 702.124's partner families permit.
    PartnerFamilies,
}

impl CommanderPairing {
    /// How many designated commanders this rule admits — the single
    /// authority for the ADMISSION decision. Callers deciding admission must
    /// not re-derive it with `is_empty()`, `> 2` or `!= 1`.
    ///
    /// Other `commander.len()` comparisons in the validators are not
    /// admission checks. They guard later slot access and arity branches.
    pub fn admits_count(self, count: usize) -> bool {
        match self {
            CommanderPairing::NoCommander => count == 0,
            CommanderPairing::Solo => count == 1,
            // CR 702.124g: no partner combination exceeds two.
            CommanderPairing::PartnerFamilies => (1..=2).contains(&count),
        }
    }
}

/// CR 100.2a / CR 903.5a: which pile a format's `DeckSizeRule` measures.
///
/// CR 903.5a fixes Commander's 100 "including its commander"; CR 100.2a's
/// constructed 60 has no commander to include. The magnitude alone cannot say
/// which, and `command_zone_holds_decklist_commander()` cannot either — it
/// answers `true` for Oathbreaker, whose subject additionally includes the
/// signature spell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeckSizeSubject {
    /// The main deck alone. The sideboard is a group of ADDITIONAL cards
    /// (CR 100.4) and so is never in this count; its own cap is
    /// `sideboard_policy` (CR 100.4a). No command-zone card is counted.
    MainDeck,
    /// CR 903.5a ("including its commander"): the main deck plus the
    /// designated commanders, netting a commander also listed in the main deck
    /// down to the one physical card it is.
    MainDeckAndCommanders,
    /// Oathbreaker RC: the main deck plus the oathbreaker AND the signature
    /// spell, each netted the same way.
    MainDeckAndCommandZone,
}

/// CR 400.1: whether a per-player zone is one pile for every seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneScope {
    /// CR 400.1: each player has their own zone.
    PerPlayer,
    /// One zone that every seat draws from or puts cards into.
    Shared,
}

/// The zones a format shares between seats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedZones {
    pub library: ZoneScope,
    pub graveyard: ZoneScope,
}

impl SharedZones {
    /// CR 400.1's stock answer: every seat has its own library and graveyard.
    pub const NONE: Self = Self {
        library: ZoneScope::PerPlayer,
        graveyard: ZoneScope::PerPlayer,
    };
    /// One library and one graveyard for every seat.
    pub const LIBRARY_AND_GRAVEYARD: Self = Self {
        library: ZoneScope::Shared,
        graveyard: ZoneScope::Shared,
    };
}

/// CR 121.2c: the order in which simultaneous draws, and the pregame deal, are
/// performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DealOrder {
    /// CR 121.2c: the active player draws every card, then each other player
    /// in turn order.
    PlayerByPlayer,
    /// One card at a time round-robin, the active player first.
    Interleaved,
}

/// CR 103.5: a mulligan a player may take by revealing a qualifying hand,
/// until they take a regular mulligan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeRevealMulligan {
    Unavailable,
    /// The hand qualifies when it holds fewer than `min_lands` lands or fewer
    /// than `min_nonlands` nonland cards.
    WhenHandLacks {
        min_lands: u8,
        min_nonlands: u8,
    },
}

/// CR 108.3 / CR 110.2 / CR 400.3: who owns a card a player takes into hand
/// from a shared zone, casts, or plays as a land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryOwnership {
    /// CR 108.3 / CR 400.3: the card keeps its owner.
    OwnerKept,
    /// The player who takes, casts, or plays the card becomes its owner.
    ActorOwns,
}

/// Who supplies a format's deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeckSupply {
    /// Each player builds or selects their own deck.
    PlayerBuilt,
    /// The engine supplies one fixed deck; a player submits nothing.
    EngineFixed,
    /// One pile is played; the host may submit it, and an empty submission
    /// plays the engine's default pile.
    HostPile,
}

/// Whether a mulligan changes anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningHandEquivalence {
    Distinguishable,
    /// Every opening hand a seat can draw is equivalent to every other.
    Equivalent,
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

/// Which concrete self-piloting Horde deck a `GameFormat::Horde` game uses.
///
/// This is the deck-identity axis of the Horde ruleset. Per-deck rule
/// differences (wave size, whether the Horde's creatures attack, life totals)
/// live on `HordeRuleset`, not here — this enum only names the deck so
/// `deck_loading` can inject the right fixed library. New decks are pure
/// additions: the community/Knudson variants and the three Theros challenge
/// decks (Battle the Horde, Face the Hydra, Defeat a God) each add one variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengeDeck {
    /// Doctor Who "Cyberman Horde" — ~100 real Universes Beyond cards plus ~200
    /// predefined Cyberman/Dalek tokens. The first deck the spine is validated
    /// against.
    CybermanHorde,
    /// D&D Horde (community format, hordemagic.com). An escalating challenge of
    /// three tiered libraries (Ooze → Goblin/Skeleton → Giant/Dragon), all built
    /// from real, already-implemented cards, using the rarity-based wave rule
    /// (`WaveTermination::UntilRarityAtLeast`). This spine slice loads the Level 1
    /// **Ooze** library; multi-tier progression is a follow-up.
    DndHorde,
    /// Zombies Horde (community format, hordemagic.com). 100 nontoken cards plus
    /// 200 Zombie / Zombie Giant tokens. Its signature rule is a *snaking* wave
    /// count — the Horde reveals until one nontoken resolves, then two, then
    /// three, then back down — so pressure ramps up and eases off in a cycle.
    ZombiesHorde,
    /// Slivers Horde (community format, hordemagic.com). 135 nontoken cards plus
    /// 170 Metallic Sliver tokens. Every Sliver lord buffs every other Sliver, so
    /// the board compounds fast — the swarm gets stronger rather than just wider.
    SliversHorde,
    /// Humans and Godzilla Horde (community format, hordemagic.com). 100 nontoken
    /// cards plus 200 Human Soldier tokens: a wide, cheap human army punctuated
    /// by a handful of enormous Godzilla-series titans.
    HumansGodzillaHorde,
    /// The Lord of the Rings "Two Towers" — Sauron, the Dark Lord (community
    /// format, hordemagic.com). A near-all-creature swarm of Orcs, Nazgûl, and
    /// siege beasts, with revealed Orc Army tokens amassing one growing army. Uses
    /// the advanced LOTR rules: the uncommon+ rarity wave and the "milled legendary
    /// enters then phases out" rule. Half of the two-Horde "Two Towers" experience
    /// (Saruman is the other half); playable on its own as a single Horde.
    SauronHorde,
    /// The Lord of the Rings "Two Towers" — Saruman, the White Hand (community
    /// format, hordemagic.com). Uruk-hai and Orcs backed by removal and
    /// enchantments that grow the White Hand's Orc Army; same advanced LOTR rules
    /// as Sauron. The other half of the two-Horde "Two Towers" experience;
    /// playable on its own as a single Horde.
    SarumanHorde,
    /// The Lord of the Rings "Two Towers" — the full TWO-Horde experience: Sauron
    /// and Saruman as two commanders, each with its own library, battlefield, and
    /// graveyard, alternating turns (Reading B). The picker identity that pairs the
    /// two half-decks — its `default_ruleset` has `challenge_deck` = this (seat 0's
    /// library is Sauron's cards) and `co_horde_decks = [SarumanHorde]` (seat 1).
    LotrTwoTowersHorde,
    /// Teenage Mutant Ninja Turtles "Turtle Team-Up" — the co-op boss-deck
    /// game: four 60-card Turtle hero decks face 11 Bosses deployed in three
    /// stages (1 → 2 → 3) sharing one life total per stage, backed by a
    /// 27-card event deck of Ninja creatures and villain Events. The first
    /// boss-deck challenge: the Horde seat keeps a real (per-stage) life
    /// total instead of the classic mill race (see `HordeRuleset::boss_stages`).
    TurtleTeamUpHorde,
}

/// Authoritative display metadata for one selectable Horde challenge deck.
///
/// Produced by [`ChallengeDeck::registry`] and consumed by the frontend's Horde
/// deck picker. The engine is the single source of truth for which decks exist;
/// the client keeps only a synchronous render mirror of this registry
/// (`client/src/data/challengeDeckRegistry.ts`, for pre-WASM picker render)
/// plus a drift test against the `getChallengeDeckRegistry` WASM export — that
/// mirror entry is the only client-side follow-up when adding a deck, never an
/// independent list. Mirrors [`FormatMetadata`] for `GameFormat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeDeckMetadata {
    pub deck: ChallengeDeck,
    /// Full display label, e.g. "Cyberman Horde".
    pub label: &'static str,
    /// Short code for compact badges, e.g. "CYB".
    pub short_label: &'static str,
    /// One-line human description suitable for a card or tooltip.
    pub description: &'static str,
    /// The deck's canonical ruleset, so a client can build the complete
    /// `FormatConfig` for a selected deck without a second call.
    pub default_ruleset: HordeRuleset,
    /// The fully engine-resolved `FormatConfig` for this deck (built by
    /// `FormatConfig::horde`): survivor deck-shape axes (`deck_size`,
    /// `singleton`, commander fields) already derived from the ruleset's
    /// `survivor_deck_format`. The deck picker applies this — never a
    /// ruleset-only patch — so switching decks cannot leave the previous
    /// deck's EDH/constructed shape attached.
    pub resolved_config: FormatConfig,
    /// Human-readable, engine-authored breakdown of how this deck plays — one
    /// labeled line per rules axis (waves, survivor life, setup, Horde combat).
    /// Rendered by [`HordeRuleset::summary`] from the structured ruleset, so the
    /// frontend displays these verbatim and never derives rules prose from the
    /// typed axes itself (that would put game-rule interpretation in the display
    /// layer). Lets the deck picker and any in-game info panel show HOW each deck
    /// plays and how it DIFFERS without a second source of truth.
    pub rules: Vec<RuleSummaryLine>,
}

/// One labeled line of a Horde deck's human-readable rules summary.
///
/// The `label` is a short, stable category name the frontend can style (bold,
/// column header, etc.); the `detail` is the deck-specific behavior for that
/// category, already rendered into prose by the engine. Both are display text —
/// the source of truth for the actual rules is the structured [`HordeRuleset`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSummaryLine {
    /// Short category label, e.g. "Waves", "Survivor life", "Setup".
    pub label: &'static str,
    /// This deck's behavior for that category, human-readable.
    pub detail: String,
}

/// How many NON-token cards a [`WaveTermination::UntilNonToken`] wave must
/// resolve before it ends.
///
/// A plain number would cover the classic rule ("until the first nontoken"), but
/// the count genuinely varies *per Horde turn* in published decks — the Zombies
/// Horde ramps difficulty by snaking the count up and back down. Modelling it as
/// a typed schedule keeps that inside the ruleset instead of leaking a
/// special-case counter into the turn engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WaveCount {
    /// The same number of nontokens every Horde turn. `Fixed(1)` is the classic
    /// "reveal until the first nontoken card" rule.
    Fixed(u32),
    /// Ramp the count up from `min` to `max`, then back down, repeating — a
    /// triangle wave over the Horde's turns. The Zombies Horde is
    /// `Snaking { min: 1, max: 3 }`: "Wave 1, then Wave 2, and so on for Wave 3.
    /// On its next turn, it will descend back down to Wave 2, snaking back and
    /// forth" — i.e. 1, 2, 3, 2, 1, 2, 3, …
    Snaking { min: u32, max: u32 },
}

impl WaveCount {
    /// How many nontokens end the wave on the Horde's `turn_index`-th turn
    /// (0-based).
    ///
    /// `Snaking` is a triangle wave of period `2 * (max - min)`: the phase walks
    /// up to `max` and back down toward `min` without repeating the endpoints,
    /// which is what "snaking back and forth" means. A degenerate range
    /// (`max <= min`) collapses to `min`, so a malformed ruleset still yields a
    /// terminating wave rather than a divide-by-zero.
    pub fn nontokens_for_turn(self, turn_index: u32) -> u32 {
        match self {
            WaveCount::Fixed(n) => n,
            WaveCount::Snaking { min, max } => {
                if max <= min {
                    return min;
                }
                let span = max - min;
                let period = span * 2;
                let phase = turn_index % period;
                min + if phase <= span { phase } else { period - phase }
            }
        }
    }
}

/// How a single Horde turn decides how many cards to reveal-and-resolve.
///
/// This is a *policy*, not a scalar count, because the rule genuinely varies by
/// ruleset: the Theros challenge decks reveal a fixed number, the original
/// Knudson rules reveal until a non-token card, and the community format ends a
/// "wave" when a card of at least a given rarity resolves. Modelling it as a
/// typed enum lets every current and future ruleset slot in as a pure addition
/// rather than overloading a number with special-case meanings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WaveTermination {
    /// Reveal-and-resolve a fixed base number of cards each Horde turn. Runtime
    /// bonuses that depend on live game state (one extra per Horde artifact in
    /// play, one extra per additional survivor) are layered on in the turn
    /// engine, not stored here — this is only the base count (e.g. 2 for Battle
    /// the Horde, 1 for Face the Hydra).
    FixedCount(u32),
    /// Reveal-and-resolve cards until `count` NON-token cards have been cast,
    /// which ends the wave. Every token revealed along the way enters the
    /// battlefield; each nontoken is cast (for free), and once the required
    /// number have resolved the wave stops — the next revealed card stays in the
    /// library for the following Horde turn.
    ///
    /// The authentic behavior for token-heavy decks (the original Knudson rules,
    /// and the Doctor Who Cyberman Horde, which is ~2/3 tokens): a small
    /// `FixedCount` would usually reveal only tokens and barely advance the real
    /// threats, so "reveal until N nontokens" keeps the wave pressuring.
    ///
    /// `count` is a [`WaveCount`] rather than a bare number because the count is
    /// not always constant: the Zombies Horde escalates it 1 → 2 → 3 → 2 → 1 …
    /// as a difficulty ramp. `WaveCount::Fixed(1)` is the classic
    /// "until the first nontoken" rule.
    UntilNonToken { count: WaveCount },
    /// Reveal-and-resolve cards until (and including) the first card whose rarity
    /// is at least the given threshold, which ends the wave. Tokens (no rarity)
    /// and nontoken cards *below* the threshold are deployed/cast and the wave
    /// CONTINUES; the first card at or above the threshold is cast and then stops
    /// the wave, leaving the rest of the library for the next Horde turn.
    ///
    /// This is the community "waves" rule used by the D&D Horde: "a wave ends when
    /// an Uncommon, Rare, or Mythic card enters" — i.e. `UntilRarityAtLeast(
    /// Rarity::Uncommon)`. Commons and tokens are the filler that builds the board
    /// up; the first uncommon-or-better is the payoff that caps the wave. Rarity
    /// is a card-data property (`Rarity` derives `Ord`: Common < Uncommon < Rare
    /// < Mythic), not a CR concept — Horde Magic is a casual community format.
    UntilRarityAtLeast(Rarity),
}

/// How a Horde deck treats its own legendary permanent cards when they would be
/// put into the Horde's graveyard from its library (the Horde is damage-milled,
/// see [`crate::game::horde::mill_from_loss`]).
///
/// A typed axis rather than a `bool` because legendary handling has more than two
/// plausible community variants — ordinary graveyard, the "boss recurs"
/// phase-out rule below, or a future "shuffle back into the library" — and a
/// boolean could name at most one of them. Casual community format
/// (hordemagic.com advanced rules); no CR governs the axis itself, though
/// [`EtbThenPhaseOut`](Self::EtbThenPhaseOut) resolves through CR 603.6 (enters-
/// the-battlefield triggers) and CR 702.26 (phasing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HordeLegendaryDeath {
    /// Basic rules: a milled legendary is put into the graveyard like any other
    /// card. The default, and what every currently-shipped deck uses.
    #[default]
    Normal,
    /// Advanced rules (Walking Dead, Stranger Things, …): "any Legendary card
    /// that goes to the Graveyard [when the Horde is damage-milled] instead
    /// enters the battlefield, ETB triggers fire, then immediately Phases Out."
    /// Milling one of the Horde's legendaries therefore DEPLOYS it (its ETB
    /// resolves) rather than removing it, and it phases back in on the Horde's
    /// next untap (CR 702.26c) — a recurring boss instead of a wasted mill.
    EtbThenPhaseOut,
}

/// Whether — and how — the engine-scripted Horde activates its permanents'
/// activated abilities during its post-combat main phase.
///
/// A typed axis rather than a `bool` because the activation cadence has more than
/// two plausible shapes (none / once per permanent / a single best-ability per
/// turn), and a boolean could name only one. Casual community format
/// (hordemagic.com): "Card-activated abilities … occur during the post-combat
/// main phase, and only once per turn"; "Horde has infinite mana (for … activation
/// costs)." The activation itself runs through the normal CR 602 activation path
/// ([`crate::game::casting::handle_activate_ability`]); this axis only gates the
/// scripted beat that drives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HordePostCombatActivation {
    /// Basic rules: the Horde never activates abilities. The default, and what
    /// every currently-shipped deck uses.
    #[default]
    None,
    /// Advanced rules (Walking Dead, Stranger Things, …): during its post-combat
    /// main phase the Horde activates each of its permanents' non-mana activated
    /// abilities once — once per permanent per turn — paying with its infinite
    /// mana (real non-mana costs such as tap/sacrifice are still paid). Per the
    /// rule "Card-activated abilities have summoning sickness", a creature's
    /// `{T}`/`{Q}` ability stays summoning-sick the turn it enters even though the
    /// emblem's Haste lets it attack (CR 302.6) — see
    /// [`crate::game::horde`]'s `tap_ability_summoning_sick_for_horde`.
    OncePerPermanent,
}

/// The deck-construction format each SURVIVOR brings to a Horde game — a per-deck
/// axis, because different Horde decks were designed against different survivor
/// pools. The hordemagic community decks are built around EDH/Commander survivor
/// decks ("Each player chooses an EDH deck"); the Theros challenge decks (Face the
/// Hydra, Battle the Horde, Defeat a God) were built for 60-card Theros-block
/// constructed decks. [`FormatConfig::horde`] derives the survivor deck
/// constraints (size, singleton, commander) from this.
///
/// `Constructed` is the default (and the behavior for the basic community
/// decks). Promoting a community deck to `Commander` is more than a deck-size
/// change — it turns on the commander subsystem for survivors (command-zone
/// commanders via [`FormatConfig::command_zone_holds_decklist_commander`], tax,
/// damage), so that remains a deliberate, per-deck follow-up; this axis is the
/// declaration point for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SurvivorDeckFormat {
    /// 60-card (minimum) constructed: non-singleton, no commander. Theros-block
    /// challenge decks, and the current default for every shipped deck.
    #[default]
    Constructed,
    /// EDH / Commander: 100-card singleton with a commander (command zone) and
    /// the 21-point commander-damage threshold.
    Commander,
}

/// Staged boss-deck configuration (Turtle Team-Up class): the Boss team fights
/// in stages of deployed Bosses sharing one life total per stage, instead of
/// the classic Horde's life-total-less mill race.
///
/// `Some` on [`HordeRuleset::boss_stages`] switches the Horde seat to a real
/// (per-stage) life total: damage and life loss apply normally, and the Boss
/// life hitting zero clears the stage (deployed Bosses leave, life resets, the
/// next stage deploys) until the final stage falls. Turtle Team-Up's life
/// table (per the product booklet): 20 Boss life with 2–3 heroes, 30 with 4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BossStages {
    /// Bosses deployed per stage in order — `[1, 2, 3]` for Turtle Team-Up.
    pub stage_sizes: Vec<u8>,
    /// Boss life at the start of each stage with fewer than four survivors.
    pub life_base: i32,
    /// Boss life at the start of each stage with four or more survivors.
    pub life_at_four_survivors: i32,
}

impl BossStages {
    /// Boss life for a stage given the survivor count.
    pub fn life_for_survivor_count(&self, survivors: usize) -> i32 {
        if survivors >= 4 {
            self.life_at_four_survivors
        } else {
            self.life_base
        }
    }
}

/// The parameters that distinguish one Horde deck's rules from another, carried
/// on `FormatConfig` so a single `GameFormat::Horde` variant covers the whole
/// family without sibling format variants. All fields are typed axes, not
/// booleans-as-config where a richer type exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HordeRuleset {
    /// Which fixed library the Horde seat plays.
    pub challenge_deck: ChallengeDeck,
    /// How each Horde turn sizes its reveal-and-resolve wave.
    pub wave: WaveTermination,
    /// How many turns the survivors take before the Horde's first turn, letting
    /// them establish a board (3 in the published rules).
    pub survivor_setup_turns: u8,
    /// The survivors' shared combined life total with a SINGLE survivor — the
    /// base the per-survivor delta below adjusts from. The hordemagic community
    /// rules set this at 100 ("Player life totals are combined, shared and start
    /// at 100 life"); the Theros challenge decks use a flat shared 20.
    ///
    /// Carried here rather than read from `FormatConfig::starting_life` because
    /// base life is a per-deck-family property (community 100 vs Theros 20), and
    /// it belongs next to `per_extra_survivor_life_delta` — the two halves of the
    /// same combined-total formula. `GameState::new` computes the combined total
    /// as `combined_base_life + per_extra_survivor_life_delta * (survivors - 1)`
    /// and distributes it across the survivor seats (CR 810.9a shared team life).
    pub combined_base_life: i32,
    /// Life added to the survivors' shared combined total for each survivor
    /// beyond the first. `0` for a flat combined total (Theros: shared 20); the
    /// hordemagic community rules use −15 ("Each player reduces {15} life to the
    /// total life beyond the 1st player") — so 1/2/3 survivors → 100/85/70.
    pub per_extra_survivor_life_delta: i32,
    /// Whether the Horde's creatures are forced to attack each combat if able
    /// (true for every published Horde ruleset; a typed axis so a future ruleset
    /// can opt out — e.g. Defeat a God's revelers that only attack when told).
    pub horde_creatures_forced_attackers: bool,
    /// How the Horde's own legendary cards are treated when milled to its
    /// graveyard by damage. `Normal` for the basic community decks; the advanced
    /// decks (Walking Dead, Stranger Things, …) use
    /// [`HordeLegendaryDeath::EtbThenPhaseOut`]. `#[serde(default)]` so Horde
    /// configs serialized before this axis existed still deserialize — they
    /// predate any advanced deck, so the `Normal` back-fill is correct.
    #[serde(default)]
    pub legendary_death: HordeLegendaryDeath,
    /// Whether the Horde activates its permanents' abilities post-combat. `None`
    /// for the basic community decks; the advanced decks use
    /// [`HordePostCombatActivation::OncePerPermanent`], which also grants the
    /// Horde infinite mana (its activation costs are otherwise unpayable).
    /// `#[serde(default)]` so pre-axis serialized Horde configs still deserialize.
    #[serde(default)]
    pub post_combat_activation: HordePostCombatActivation,
    /// The deck-construction format survivors bring (per-deck; see
    /// [`SurvivorDeckFormat`]). `#[serde(default)]` back-fills `Constructed` for
    /// Horde configs serialized before this axis — matching the pre-axis behavior
    /// where `FormatConfig::horde` hardcoded a 60-card non-singleton survivor deck.
    #[serde(default)]
    pub survivor_deck_format: SurvivorDeckFormat,
    /// Additional Horde libraries beyond [`challenge_deck`](Self::challenge_deck),
    /// one per extra Horde seat — the LOTR "Two Towers" two-Horde experience pairs
    /// Sauron (`challenge_deck`) with Saruman here. Empty for every single-Horde
    /// deck. The full per-seat deck list is [`horde_decks`](Self::horde_decks):
    /// `challenge_deck` (seat 0) then each of these in order. `#[serde(default,
    /// skip_serializing_if)]` so single-Horde configs serialize byte-identically
    /// to before this axis existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub co_horde_decks: Vec<ChallengeDeck>,
    /// Staged boss-deck configuration (Turtle Team-Up class) — `None` for the
    /// classic mill-race Hordes. `#[serde(default, skip_serializing_if)]` so
    /// classic configs serialize byte-identically to before this axis existed
    /// and pre-axis payloads back-fill `None` (correct: they predate any boss
    /// deck).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boss_stages: Option<BossStages>,
}

impl HordeRuleset {
    /// The per-seat Horde libraries in seat order: the primary
    /// [`challenge_deck`](Self::challenge_deck) followed by each
    /// [`co_horde_decks`](Self::co_horde_decks) entry. A single-Horde deck yields
    /// one; the LOTR two-Horde deck yields `[Sauron, Saruman]`. The length is the
    /// number of Horde seats the game designates.
    pub fn horde_decks(&self) -> Vec<ChallengeDeck> {
        std::iter::once(self.challenge_deck)
            .chain(self.co_horde_decks.iter().copied())
            .collect()
    }
    /// Render this ruleset's typed axes into human-readable, labeled lines for
    /// the Horde deck picker and any in-game info panel.
    ///
    /// This is the single authority for turning the structured Horde rules into
    /// prose. The frontend renders these lines verbatim; it must never interpret
    /// [`WaveTermination`], the life deltas, or the setup count into its own
    /// wording, because that is game-rule interpretation and belongs in the
    /// engine. Because the summary is produced by matching over the struct's own
    /// fields, every deck's summary is complete by construction — a new
    /// advanced-rules axis added to `HordeRuleset` adds one line here and every
    /// deck picks it up, rather than requiring a hand-authored per-deck string.
    pub fn summary(&self) -> Vec<RuleSummaryLine> {
        // Small pluralizer so counts read naturally without a formatting crate.
        fn plural(n: u32) -> &'static str {
            if n == 1 {
                ""
            } else {
                "s"
            }
        }

        // The wave line is the deck's signature difference, so it leads.
        let waves = match self.wave {
            WaveTermination::FixedCount(n) => {
                format!("Reveals {n} card{} each Horde turn", plural(n))
            }
            WaveTermination::UntilNonToken {
                count: WaveCount::Fixed(1),
            } => "Reveals until the first nontoken card is cast".to_string(),
            WaveTermination::UntilNonToken {
                count: WaveCount::Fixed(n),
            } => format!("Reveals until {n} nontoken cards are cast"),
            WaveTermination::UntilNonToken {
                count: WaveCount::Snaking { min, max },
            } => format!(
                "Reveals until N nontokens are cast — N snakes {min} \u{2192} {max} \u{2192} {min} \
                 each turn"
            ),
            WaveTermination::UntilRarityAtLeast(rarity) => {
                format!(
                    "A wave ends at the first {} or better card",
                    rarity_word(rarity)
                )
            }
        };

        // Survivor life: the combined base plus the per-extra-survivor scaling.
        let life = if self.per_extra_survivor_life_delta == 0 {
            format!("{} life, shared by all survivors", self.combined_base_life)
        } else {
            format!(
                "{} life shared, {:+} per extra survivor",
                self.combined_base_life, self.per_extra_survivor_life_delta
            )
        };

        let setup = format!(
            "Survivors take {} turn{} to set up before the Horde's first turn",
            self.survivor_setup_turns,
            plural(u32::from(self.survivor_setup_turns)),
        );

        let combat = if self.horde_creatures_forced_attackers {
            "The Horde's creatures attack every combat if able, and only its Defenders can block"
                .to_string()
        } else {
            "The Horde's creatures attack only when instructed".to_string()
        };

        // What survivor decks to bring — a per-deck property (community decks are
        // built for EDH; Theros-block decks for 60-card constructed).
        let survivor_decks = match self.survivor_deck_format {
            SurvivorDeckFormat::Constructed => "Survivors bring 60-card constructed decks",
            SurvivorDeckFormat::Commander => {
                "Survivors bring 100-card singleton Commander (EDH) decks"
            }
        }
        .to_string();

        let mut lines = vec![
            RuleSummaryLine {
                label: "Waves",
                detail: waves,
            },
            RuleSummaryLine {
                label: "Survivor life",
                detail: life,
            },
            RuleSummaryLine {
                label: "Survivor decks",
                detail: survivor_decks,
            },
            RuleSummaryLine {
                label: "Setup",
                detail: setup,
            },
            RuleSummaryLine {
                label: "Horde combat",
                detail: combat,
            },
        ];

        // The legendary line is an *advanced-rules* axis: only the decks that opt
        // into the "boss recurs" rule surface it, so a basic deck's summary stays
        // exactly the five base lines above. This is a distinguishing rule, not the
        // vanilla default, so showing "legendaries die normally" for every basic
        // deck would be noise.
        match self.legendary_death {
            HordeLegendaryDeath::Normal => {}
            HordeLegendaryDeath::EtbThenPhaseOut => lines.push(RuleSummaryLine {
                label: "Legendary deaths",
                detail: "A milled legendary enters, triggers its ETB, then phases out — \
                         it returns on the Horde's next untap instead of being removed"
                    .to_string(),
            }),
        }

        // Post-combat activation is likewise an advanced-only axis — surfaced only
        // for the decks that opt in, so basic decks keep their prior line set.
        match self.post_combat_activation {
            HordePostCombatActivation::None => {}
            HordePostCombatActivation::OncePerPermanent => lines.push(RuleSummaryLine {
                label: "Post-combat",
                detail: "After combat the Horde activates each of its permanents' abilities \
                         once, with infinite mana"
                    .to_string(),
            }),
        }

        // Boss stages are likewise surfaced only for the decks that stage
        // Bosses — classic mill-race decks keep their prior line set.
        if let Some(boss) = &self.boss_stages {
            let stages = boss
                .stage_sizes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(" \u{2192} ");
            lines.push(RuleSummaryLine {
                label: "Bosses",
                detail: format!(
                    "Bosses deploy in {} stage{} ({}) sharing {} life per stage ({} with 4+ heroes)",
                    boss.stage_sizes.len(),
                    plural(boss.stage_sizes.len() as u32),
                    stages,
                    boss.life_base,
                    boss.life_at_four_survivors
                ),
            });
        }

        lines
    }
}

/// Lowercase English word for a rarity, for the Horde wave-rule summary
/// ("uncommon or better"). Horde Magic's community wave rule is keyed on rarity
/// order (`Rarity` derives `Ord`); this is display text, not a CR concept.
fn rarity_word(rarity: Rarity) -> &'static str {
    match rarity {
        Rarity::Common => "common",
        Rarity::Uncommon => "uncommon",
        Rarity::Rare => "rare",
        Rarity::Mythic => "mythic",
        Rarity::Special => "special",
        Rarity::Bonus => "bonus",
    }
}

impl ChallengeDeck {
    /// Every selectable challenge deck, in display order.
    ///
    /// New decks must be appended here as well as given a [`Self::metadata`] arm
    /// (that match is exhaustive, so the compiler catches a missing arm; the
    /// `registry_covers_every_challenge_deck` test catches a missing entry here).
    pub const ALL: &'static [ChallengeDeck] = &[
        ChallengeDeck::CybermanHorde,
        ChallengeDeck::DndHorde,
        ChallengeDeck::ZombiesHorde,
        ChallengeDeck::SliversHorde,
        ChallengeDeck::HumansGodzillaHorde,
        ChallengeDeck::SauronHorde,
        ChallengeDeck::SarumanHorde,
        ChallengeDeck::LotrTwoTowersHorde,
        ChallengeDeck::TurtleTeamUpHorde,
    ];

    /// Display metadata for a single deck. Exhaustive match: adding a
    /// `ChallengeDeck` variant fails to compile until it is described here.
    ///
    /// The flavor `label`/`short_label`/`description` are per-deck display text;
    /// the structured `default_ruleset` and its rendered `rules` summary are the
    /// single source of truth for the actual rules, so they are attached once
    /// from [`Self::default_ruleset`] rather than restated per arm.
    pub fn metadata(self) -> ChallengeDeckMetadata {
        let (label, short_label, description) = match self {
            ChallengeDeck::CybermanHorde => (
                "Cyberman Horde",
                "CYB",
                "Doctor Who — Cybermen and Daleks swarm in waves that \
                 run until a nontoken card is cast",
            ),
            ChallengeDeck::DndHorde => (
                "D&D Horde — Oozes",
                "DND",
                "Dungeons & Dragons — a self-replicating Ooze swarm; \
                 each wave ends when an uncommon or better is revealed",
            ),
            ChallengeDeck::ZombiesHorde => (
                "Zombies Horde",
                "ZOM",
                "An undead swarm whose waves ramp 1 → 2 → 3 nontokens \
                 and back down, snaking between pressure and respite",
            ),
            ChallengeDeck::SliversHorde => (
                "Slivers Horde",
                "SLV",
                "Every Sliver buffs every other Sliver — the swarm \
                 compounds, growing stronger as it grows wider",
            ),
            ChallengeDeck::HumansGodzillaHorde => (
                "Humans & Godzilla Horde",
                "HGZ",
                "A wide, cheap human army punctuated by a handful of \
                 enormous Godzilla-series titans",
            ),
            ChallengeDeck::SauronHorde => (
                "Sauron, the Dark Lord Horde",
                "SAU",
                "The Lord of the Rings \u{201c}Two Towers\u{201d} — Sauron's \
                 near-all-creature swarm of Orcs, Nazg\u{fb}l, and siege beasts; \
                 revealed Orc Armies amass one growing army, and milled legendaries \
                 recur by phasing out",
            ),
            ChallengeDeck::SarumanHorde => (
                "Saruman, the White Hand Horde",
                "SAR",
                "The Lord of the Rings \u{201c}Two Towers\u{201d} — Saruman's \
                 Uruk-hai and Orcs backed by removal and enchantments that grow the \
                 White Hand's Orc Army; uncommon+ waves and legendaries that recur \
                 by phasing out",
            ),
            ChallengeDeck::LotrTwoTowersHorde => (
                "LOTR: The Two Towers Horde",
                "2TW",
                "The Lord of the Rings \u{201c}Two Towers\u{201d} — the full \
                 two-Horde experience: Sauron and Saruman as two commanders, each \
                 with its own deck, battlefield, and graveyard, ALTERNATING turns \
                 against the survivor team",
            ),
            ChallengeDeck::TurtleTeamUpHorde => (
                "Turtle Team-Up Horde",
                "TMT",
                "Teenage Mutant Ninja Turtles — staged Bosses sharing one life \
                 total per stage, backed by an event deck of Ninjas and villain \
                 Events",
            ),
        };
        let default_ruleset = self.default_ruleset();
        let rules = default_ruleset.summary();
        let resolved_config = FormatConfig::horde(default_ruleset.clone());
        ChallengeDeckMetadata {
            deck: self,
            label,
            short_label,
            description,
            default_ruleset,
            resolved_config,
            rules,
        }
    }

    /// The full catalog of selectable Horde decks with display metadata. It is
    /// the single source of truth for which decks exist and how they are
    /// labeled; the frontend's Horde deck picker renders the synchronous TS
    /// mirror of this registry pre-WASM, and the drift test pins the two
    /// together.
    pub fn registry() -> Vec<ChallengeDeckMetadata> {
        Self::ALL.iter().copied().map(Self::metadata).collect()
    }

    /// The canonical ruleset for each Horde deck. Centralizes deck-identity →
    /// rules so the registry, `FormatConfig::for_format`, and callers that only
    /// have the deck enum agree on one source of truth.
    pub fn default_ruleset(self) -> HordeRuleset {
        match self {
            ChallengeDeck::CybermanHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::CybermanHorde,
                // The Cyberman deck is ~2/3 tokens, so a small `FixedCount`
                // would usually reveal only tokens and barely advance the real
                // threats. "Reveal until a non-token card" is the authentic
                // token-heavy behavior for this deck — the classic single-
                // nontoken rule, i.e. `Fixed(1)`.
                wave: WaveTermination::UntilNonToken {
                    count: WaveCount::Fixed(1),
                },
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                // All shipped community decks are basic decks — the advanced
                // legendary and post-combat rules arrive with Walking Dead /
                // Stranger Things.
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                // Every shipped deck is a community deck; survivor decks currently
                // load as 60-card constructed. Promoting these to `Commander`
                // (per the basic EDH rule) remains a deliberate per-deck follow-up —
                // the commander seating itself is wired, only the promotion is pending.
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::DndHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::DndHorde,
                // The D&D Horde's defining rule: a wave ends when an Uncommon,
                // Rare, or Mythic card enters (commons and tokens keep it going).
                wave: WaveTermination::UntilRarityAtLeast(Rarity::Uncommon),
                // No published D&D-specific setup/life values; follow the generic
                // hordemagic rules, mirroring the Cyberman defaults (tunable axes).
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                // All shipped community decks are basic decks — the advanced
                // legendary and post-combat rules arrive with Walking Dead /
                // Stranger Things.
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                // Every shipped deck is a community deck; survivor decks currently
                // load as 60-card constructed. Promoting these to `Commander`
                // (per the basic EDH rule) remains a deliberate per-deck follow-up —
                // the commander seating itself is wired, only the promotion is pending.
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::ZombiesHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::ZombiesHorde,
                // The deck's defining rule: "it will Wave 1, putting out Tokens
                // until it flips its first nontoken card. Then ... Wave 2 ... and
                // so on for Wave 3. On its next turn, it will descend back down
                // to Wave 2, snaking back and forth."
                wave: WaveTermination::UntilNonToken {
                    count: WaveCount::Snaking { min: 1, max: 3 },
                },
                // No published Zombies-specific setup/life values; follow the
                // generic hordemagic rules, as the other community decks do.
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                // All shipped community decks are basic decks — the advanced
                // legendary and post-combat rules arrive with Walking Dead /
                // Stranger Things.
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                // Every shipped deck is a community deck; survivor decks currently
                // load as 60-card constructed. Promoting these to `Commander`
                // (per the basic EDH rule) remains a deliberate per-deck follow-up —
                // the commander seating itself is wired, only the promotion is pending.
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::SliversHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::SliversHorde,
                // States no deck-specific rules; takes the hordemagic BASIC rule
                // verbatim: "Waves end when an UNCOMMON, RARE or MYTHIC card is
                // cast."
                wave: WaveTermination::UntilRarityAtLeast(Rarity::Uncommon),
                // Basic rules: "Each player takes {3} consecutive turns to set up
                // their board."
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                // All shipped community decks are basic decks — the advanced
                // legendary and post-combat rules arrive with Walking Dead /
                // Stranger Things.
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                // Every shipped deck is a community deck; survivor decks currently
                // load as 60-card constructed. Promoting these to `Commander`
                // (per the basic EDH rule) remains a deliberate per-deck follow-up —
                // the commander seating itself is wired, only the promotion is pending.
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::HumansGodzillaHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::HumansGodzillaHorde,
                // As above — no deck-specific rules stated, so the basic
                // community wave rule applies.
                wave: WaveTermination::UntilRarityAtLeast(Rarity::Uncommon),
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                // All shipped community decks are basic decks — the advanced
                // legendary and post-combat rules arrive with Walking Dead /
                // Stranger Things.
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                // Every shipped deck is a community deck; survivor decks currently
                // load as 60-card constructed. Promoting these to `Commander`
                // (per the basic EDH rule) remains a deliberate per-deck follow-up —
                // the commander seating itself is wired, only the promotion is pending.
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::SauronHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::SauronHorde,
                // LOTR "Two Towers": "Waves end when an Uncommon, Rare, or Mythic
                // ETB." Per-card rarity is pinned by `sauron_horde::card_rarity`.
                wave: WaveTermination::UntilRarityAtLeast(Rarity::Uncommon),
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                // LOTR advanced rule: a legendary that would be milled to the
                // graveyard by damage instead enters the battlefield and phases out.
                legendary_death: HordeLegendaryDeath::EtbThenPhaseOut,
                post_combat_activation: HordePostCombatActivation::None,
                // The published deck is played against upgraded Commander (EDH)
                // survivor decks (Aragorn, the Uniter) — the first Commander deck.
                survivor_deck_format: SurvivorDeckFormat::Commander,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::SarumanHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::SarumanHorde,
                // LOTR "Two Towers": same advanced rules as Sauron. Per-card rarity
                // is pinned by `saruman_horde::card_rarity`.
                wave: WaveTermination::UntilRarityAtLeast(Rarity::Uncommon),
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                legendary_death: HordeLegendaryDeath::EtbThenPhaseOut,
                post_combat_activation: HordePostCombatActivation::None,
                survivor_deck_format: SurvivorDeckFormat::Commander,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            },
            ChallengeDeck::LotrTwoTowersHorde => HordeRuleset {
                // Seat 0's library is Sauron's (its `load_horde_library` arm loads
                // the Sauron cards); Saruman is the co-horde in seat 1. The registry
                // requires `challenge_deck == deck`, so it is this variant itself.
                challenge_deck: ChallengeDeck::LotrTwoTowersHorde,
                co_horde_decks: vec![ChallengeDeck::SarumanHorde],
                wave: WaveTermination::UntilRarityAtLeast(Rarity::Uncommon),
                survivor_setup_turns: 3,
                combined_base_life: 100,
                per_extra_survivor_life_delta: -15,
                horde_creatures_forced_attackers: true,
                legendary_death: HordeLegendaryDeath::EtbThenPhaseOut,
                post_combat_activation: HordePostCombatActivation::None,
                survivor_deck_format: SurvivorDeckFormat::Commander,
                boss_stages: None,
            },
            ChallengeDeck::TurtleTeamUpHorde => HordeRuleset {
                challenge_deck: ChallengeDeck::TurtleTeamUpHorde,
                // The Boss Action phase plays 2 event cards — 3 with three or
                // four heroes, plus one more while Krang is deployed (those
                // live-state bonuses layer on in `horde::wave_seed`).
                wave: WaveTermination::FixedCount(2),
                // No setup turns: the heroes share the first turn immediately.
                survivor_setup_turns: 0,
                // Shared hero life 20/30/40 for 2/3/4 heroes: base 10 for the
                // first, +10 per extra survivor.
                combined_base_life: 10,
                per_extra_survivor_life_delta: 10,
                horde_creatures_forced_attackers: true,
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                // The four 60-card Turtle hero decks.
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                // Three stages (1 → 2 → 3 Bosses) sharing 20 life per stage
                // (30 with four heroes), per the product's life table.
                boss_stages: Some(BossStages {
                    stage_sizes: vec![1, 2, 3],
                    life_base: 20,
                    life_at_four_survivors: 30,
                }),
            },
        }
    }
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
    /// true when the engine supplies the format's deck or a default for it, so
    /// the player builds nothing (Momir's Madness, Dandân). The frontend
    /// consumes this directly to bypass deck-selection gates — it must never
    /// re-list such formats client-side.
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
    /// Present only for `GameFormat::Horde`: the per-deck Horde rules (which
    /// challenge deck, wave sizing, setup turns, life scaling, forced attackers).
    /// `None` for every other format. Mirrors the optional `archenemy_player`
    /// pattern so existing serialized configs deserialize unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub horde_ruleset: Option<HordeRuleset>,
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
/// of `i32`'s range for subsequent gameplay-driven life changes. This ceiling
/// — and the CR 704.5a / CR 810.8c playability floor beside it — is enforced
/// by `validate_starting_life_bounds`, which is the single shared authority
/// for both bounds: neither a built-in payload's `starting_life` nor a
/// Custom payload's `custom_rules.structural.starting_life` may cross either
/// bound, regardless of which of that function's call sites checks it.
pub const MAX_STARTING_LIFE: i32 = 1_000_000;

/// CR 704.5a / CR 810.8c playability floor, and the `MAX_STARTING_LIFE`
/// overflow-safety ceiling, on a resolved `FormatConfig`. This is the single
/// shared authority for both bounds. `FormatConfig::deserialize` calls it
/// from both of its arms:
///
/// - the built-in (`None`) arm, via `built_in_axes_no_looser_than_rules`'s
///   `starting_life` `HostChoiceWithin` row;
/// - the Custom (`Some(rules)`) arm, directly, on the config resolved from
///   `custom_rules.structural.starting_life` — that arm's blanket-equality
///   check against `FormatConfig::for_custom_rules` only proves internal
///   self-consistency, not that the declared life total is playable or
///   within the engine's arithmetic-safety ceiling, so this call is what
///   actually bounds it.
///
/// Two more callers bound a `FormatConfig` before it ever reaches
/// `deserialize`, each as defense in depth rather than a closing gate: a
/// config that skipped this earlier check would still be caught by
/// `deserialize` itself, but failing early here gives a readable
/// lobby-level message instead of a raw `deserialize` error at game start.
/// Namely, `engine-wasm`'s `format_config_for_custom_rules` (the resolver
/// the frontend calls when a player selects a saved custom format out of
/// localStorage) and `server-core`'s `SessionManager::create_game_n_players`
/// (defense in depth alongside its existing `validate_for_player_count`/
/// `reject_unimplemented_range_of_influence` calls).
///
/// CR 103.4 licenses a VARIANT starting life total; it does not by itself
/// hand the number to a host. What does is the product: the shipped host UI
/// has always exposed `starting_life` for every format, and
/// `server-core`'s `create_game_honors_a_configured_starting_life` documents
/// it as supported. The one real bound is playability: CR 704.5a ("If a
/// player has 0 or less life, that player loses the game") and, for
/// shared-life team formats, CR 810.8c ("If a team's life total is 0 or
/// less, the team loses the game"). A total resolving to 0 or less per seat
/// means every seat loses at the first state-based-action check and no game
/// can start.
///
/// Checked through `starting_life_for_seat` rather than the raw field, and
/// rather than `starting_life_for_player` (what `GameState::new` actually
/// calls) — see `built_in_axes_no_looser_than_rules`'s history for the full
/// rationale, which still applies unchanged: the two agree on
/// `IndividualSeats`/`FixedTeams` (the only topologies where this field is
/// live), and `starting_life_for_seat` is strictly more conservative on
/// `OneVsMany` for default Archenemy. For Archenemy Commander it reads the
/// shared 60 while hero seats split it per session — still above 0 for
/// every legal hero count (1-5 split 60 into 12..60), so the floor holds.
///
/// The ceiling is a sibling engineering invariant, not a rules row — see
/// `MAX_STARTING_LIFE`'s own doc comment. It is checked against the raw
/// declared field, not `starting_life_for_seat`, because the floor's
/// playability concern (a seat that cannot survive the first SBA check) is a
/// per-seat question but the overflow concern is about the field's own
/// magnitude before it is ever divided.
///
/// `pub` (not `pub(crate)`): both of the two additional callers above are
/// separate crates from `engine`.
pub fn validate_starting_life_bounds(config: &FormatConfig) -> Result<(), String> {
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
    Ok(())
}

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
    // able to start the game (CR 704.5a; CR 810.8c for shared-life team
    // formats) and the declared value must not cross the engine's
    // arithmetic-safety ceiling (`MAX_STARTING_LIFE`, no CR — it is not a
    // rules bound). Both bounds are enforced by `validate_starting_life_
    // bounds`, the single shared authority called from BOTH arms of
    // `FormatConfig::deserialize` — see that function's own doc comment for
    // the full rationale, including why the floor is checked through
    // `starting_life_for_seat` rather than the raw field or
    // `starting_life_for_player`.
    validate_starting_life_bounds(config)?;

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

    // horde_ruleset: ShapeLocked(one-directional) on the Option
    // discriminant — a built-in outside the Horde family may not invent
    // one — plus HostChoice on the ruleset itself. Which challenge deck
    // (Cyberman, Zombie, Sliver, ...) drives the Horde is the table's
    // choice, resolved into the config alongside the seat's deck; the
    // registry value for Horde is only one of several legitimate
    // variants, so value-equality is NOT required here.
    //
    // The one thing still worth rejecting: a built-in format outside the
    // Horde family (whose registry value is always `None`) declaring a
    // ruleset at all, since only Horde-family formats use this field.
    if rules.horde_ruleset.is_none() && config.horde_ruleset.is_some() {
        return Err(format!(
            "FormatConfig.horde_ruleset is {:?}, but {} does not use a Horde ruleset \
             at all — only Horde-family formats may declare this field",
            config.horde_ruleset, config.format,
        ));
    }

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
                         scope, an ante zone) that no engine code enforces",
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
                    // banned/restricted/legal_sets/legal_cards lists are caller-sized and
                    // have no business in an error string.
                    return Err(serde::de::Error::custom(format!(
                        "FormatConfig for {} contradicts its own custom_rules.structural — every \
                         runtime field must be exactly what FormatConfig::for_custom_rules \
                         derives from the declared rules (allow_debug_actions \
                         excepted). Derived: \
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
                // The equality check above only proves `config` is internally
                // self-consistent with its own `custom_rules.structural` — it
                // does not bound the declared `starting_life` itself. Apply
                // the same floor/ceiling a built-in payload gets (see
                // `validate_starting_life_bounds`'s own doc comment), so a
                // Custom format cannot post an unplayable (CR 704.5a / CR
                // 810.8c) or overflow-prone (`MAX_STARTING_LIFE`) starting
                // life just because it is internally consistent about it.
                validate_starting_life_bounds(&config).map_err(serde::de::Error::custom)?;
            }
            // A built-in format's rules are fixed by the Comprehensive Rules
            // and the engine registry, except where the CR itself grants a
            // host a choice (see `built_in_axes_no_looser_than_rules`'s own
            // doc comment for the full verdict list, now six: Locked,
            // NoLooserThan, Derived, HostChoice, ShapeLocked, and
            // HostChoiceWithin). `built_in_axes_no_looser_than_rules`
            // re-derives the authoritative config via `FormatConfig::for_format`
            // and checks every one of this struct's 18 fields against it —
            // absorbing what was previously a single ad hoc
            // `default_deck_copy_limit` check as one of its 18 rows, rather
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
    /// Which authority decides this format's deck-construction card pool —
    /// the single authority: [`Self::legality_format`] is a projection of
    /// this, not a second list.
    pub fn card_pool(self) -> CardPool {
        match self {
            GameFormat::Standard => CardPool::LegalityTable(LegalityFormat::Standard),
            GameFormat::Commander => CardPool::LegalityTable(LegalityFormat::Commander),
            // 2HG Commander decks are Commander decks (CR 903.5) — same card
            // pool and ban list, no separate table.
            GameFormat::TwoHeadedGiantCommander => {
                CardPool::LegalityTable(LegalityFormat::Commander)
            }
            // CR 904.13a: Archenemy Commander decks are Commander decks —
            // same card pool and ban list, no separate table.
            GameFormat::ArchenemyCommander => {
                CardPool::LegalityTable(LegalityFormat::Commander)
            }
            // Planechase Commander decks are Commander decks — same card
            // pool and ban list, no separate table.
            GameFormat::PlanechaseCommander => {
                CardPool::LegalityTable(LegalityFormat::Commander)
            }
            GameFormat::Pioneer => CardPool::LegalityTable(LegalityFormat::Pioneer),
            GameFormat::Modern => CardPool::LegalityTable(LegalityFormat::Modern),
            GameFormat::Premodern => CardPool::LegalityTable(LegalityFormat::Premodern),
            GameFormat::Legacy => CardPool::LegalityTable(LegalityFormat::Legacy),
            GameFormat::Vintage => CardPool::LegalityTable(LegalityFormat::Vintage),
            GameFormat::Historic => CardPool::LegalityTable(LegalityFormat::Historic),
            GameFormat::Timeless => CardPool::LegalityTable(LegalityFormat::Timeless),
            GameFormat::Pauper => CardPool::LegalityTable(LegalityFormat::Pauper),
            GameFormat::PauperCommander => {
                CardPool::LegalityTable(LegalityFormat::PauperCommander)
            }
            GameFormat::DuelCommander => CardPool::LegalityTable(LegalityFormat::DuelCommander),
            GameFormat::Brawl => CardPool::LegalityTable(LegalityFormat::StandardBrawl),
            GameFormat::HistoricBrawl => CardPool::LegalityTable(LegalityFormat::Brawl),
            // Freeform's own rules positively restrict nothing, which is what
            // separates this variant from the silence `NoEngineAuthority`
            // names. (The CR 100.6 rationale for the axis existing at all is
            // on `CardPool` itself; do not restate it here.)
            GameFormat::Freeform => CardPool::Unrestricted,
            // Freeform Commander's own rules make the same positive
            // declaration Freeform's does; it is a separate arm rather than
            // joining Freeform's so a later editor does not merge them and
            // put Freeform's comment over a second format it does not name.
            GameFormat::FreeformCommander => CardPool::Unrestricted,
            GameFormat::TinyLeaders
            | GameFormat::Oathbreaker
            // These state no main-deck card-pool rule at all, so the engine
            // holds no answer — which is not the positive "nothing is
            // restricted" claim `Unrestricted` makes.
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            // CR 903.13e: the drafted cards become the card pool, so no
            // constructed legality table applies — as for Limited.
            | GameFormat::CommanderDraft
            | GameFormat::Limited => CardPool::NoEngineAuthority,
            // Momir's main deck is fixed by the format's own rule, not by a
            // legality table: `evaluate_momir` enforces exactly 12 copies
            // each of the five CR 305.6 snow basic land types.
            GameFormat::Momir => CardPool::NoEngineAuthority,
            // Dandân's deck is the engine-supplied pile, so no legality table
            // applies to a submitted list.
            GameFormat::Dandan => CardPool::NoEngineAuthority,
            // The Horde's own deck is engine-supplied and fixed; survivor
            // decks are validated structurally (Commander shape for EDH
            // hordes, unrestricted otherwise), never against a legality
            // table — the old `legality_format() == None` answer, preserved.
            GameFormat::Horde => CardPool::NoEngineAuthority,
            // A custom format's legality is entirely governed by its own
            // `LegalityRules` (legal_sets/legal_cards/banned/restricted), never by the
            // built-in `LegalityFormat` table.
            GameFormat::Custom(_) => CardPool::DeclaredRules,
        }
    }

    /// The legality table that answers this format's card pool, if the engine
    /// holds one. A projection of [`Self::card_pool`], which is the single
    /// authority: the three pool kinds that answer `None` here are not the
    /// same fact, and only `card_pool` distinguishes them.
    pub fn legality_format(self) -> Option<LegalityFormat> {
        match self.card_pool() {
            CardPool::LegalityTable(format) => Some(format),
            CardPool::NoEngineAuthority | CardPool::Unrestricted | CardPool::DeclaredRules => None,
        }
    }

    /// The legality table the card data records for this format, if any —
    /// whether or not this format's deck validation consults it, which is
    /// `card_pool`'s question.
    pub fn recorded_legality_table(self) -> Option<LegalityFormat> {
        // 2HG Commander, Archenemy Commander, and Planechase Commander
        // share Commander's card pool and ban list — card data records no
        // separate table for any, so Commander's table is their recorded
        // lens. The reverse lookup below cannot express sharing (each table
        // maps to exactly one format), hence the explicit arm.
        if matches!(
            self,
            GameFormat::TwoHeadedGiantCommander
                | GameFormat::ArchenemyCommander
                | GameFormat::PlanechaseCommander
        ) {
            return Some(LegalityFormat::Commander);
        }
        LegalityFormat::ALL
            .into_iter()
            .find(|table| legality_table_format(*table) == self)
    }

    /// [`Self::recorded_legality_table`]'s key in the engine's legality maps.
    pub fn legality_key(self) -> Option<&'static str> {
        self.recorded_legality_table().map(LegalityFormat::as_key)
    }

    /// How many commanders this format's decklist may designate. Whether a
    /// specific count is admitted is `CommanderPairing::admits_count`'s call
    /// — the single authority for that decision. Callers must not re-derive
    /// admission itself.
    ///
    /// `Custom(_)` answers `NoCommander`: reachable via both custom
    /// evaluators (`evaluate_custom_format`, `quick_custom_format_check`),
    /// and correct on either because every command-zone custom format is
    /// already refused upstream by `custom_format_pool`'s
    /// `CUSTOM_FORMAT_COMMAND_ZONE_UNSUPPORTED` gate.
    pub fn commander_pairing(self) -> CommanderPairing {
        match self {
            GameFormat::Commander
            // CR 903.3 + CR 702.124: 2HG Commander decks designate commanders
            // under Commander's own eligibility and partner rules.
            | GameFormat::TwoHeadedGiantCommander
            // CR 904.13a + CR 903.3 + CR 702.124: same for Archenemy
            // Commander decks.
            | GameFormat::ArchenemyCommander
            // CR 903.3 + CR 702.124: same for Planechase Commander decks
            // (planar doesn't change pairing).
            | GameFormat::PlanechaseCommander
            | GameFormat::DuelCommander
            | GameFormat::PauperCommander
            | GameFormat::CommanderDraft
            | GameFormat::TinyLeaders
            // This format widens WHO may be a
            // commander.
            | GameFormat::FreeformCommander => CommanderPairing::PartnerFamilies,
            GameFormat::Brawl | GameFormat::HistoricBrawl | GameFormat::Oathbreaker => {
                CommanderPairing::Solo
            }
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
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            // The Horde deck itself designates no commander. Commander-format
            // hordes (Sauron, Saruman) whose SURVIVORS run commanders are
            // validated by the Horde path in deck_validation, which branches
            // on the challenge deck's survivor-deck format before this
            // format-level pairing is consulted.
            | GameFormat::Horde
            | GameFormat::Dandan
            | GameFormat::Freeform
            | GameFormat::Custom(_) => CommanderPairing::NoCommander,
        }
    }

    /// Whether a card in this game could be an Arena-only card. The
    /// classification keys off the built-in `LegalityFormat` table; a format
    /// outside that table — `Custom(_)` included, whose pool lives in
    /// `custom_rules.legality` — is not classified here and so is not
    /// restricted here.
    pub fn admits_digital_only_cards(self) -> bool {
        self.legality_format()
            .is_none_or(LegalityFormat::admits_digital_only_cards)
    }

    /// Whether this format's venture pool includes Baldur's Gate Wilderness:
    /// the formats whose own rules positively restrict nothing
    /// ([`CardPool::Unrestricted`]) offer it alongside the AFR trio on a
    /// normal venture (CR 701.49a) and as an alternative to Undercity on an
    /// initiative venture (CR 726.2); every other format offers only the
    /// printed pool. Derived from [`Self::card_pool`] — never re-list the
    /// formats here.
    pub fn offers_baldurs_gate_wilderness(self) -> bool {
        matches!(self.card_pool(), CardPool::Unrestricted)
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
            | GameFormat::Pauper
            | GameFormat::Freeform => SideboardPolicy::Limited(15),
            GameFormat::Commander
            // CR 903.5e via the shared commander validator: no sideboard.
            | GameFormat::TwoHeadedGiantCommander
            // CR 904.13a + CR 903.5e: Commander decks, no sideboard.
            | GameFormat::ArchenemyCommander
            // CR 903.5e via the shared commander validator: no sideboard.
            | GameFormat::PlanechaseCommander
            | GameFormat::PauperCommander
            | GameFormat::DuelCommander
            | GameFormat::Oathbreaker
            | GameFormat::Brawl
            // Momir has no sideboard — the deck is exactly 60 snow basic lands.
            | GameFormat::Momir
            // Horde decks are engine-supplied and fixed — no sideboard.
            | GameFormat::Horde
            | GameFormat::Dandan
            // CR 903.13f routes deck construction through CR 903.5, and the
            // Commander family has no sideboard.
            | GameFormat::CommanderDraft
            | GameFormat::HistoricBrawl
            // No deck-compatibility verdict distinguishes this format's
            // sideboard value at all: `request_without_sideboard` strips the
            // sideboard before every commander check. `Forbidden` is the
            // only value under which the engine stays consistent for this
            // format with no edit anywhere else (`load_deck_into_state`'s
            // `drop_sideboard` keys on `Forbidden` alone), and this format
            // has no best-of-three shape for a real sideboard to serve
            // (`evaluate_deck_compatibility` derives `bo3_ready` false for
            // any deck that designates a commander).
            | GameFormat::FreeformCommander => SideboardPolicy::Forbidden,
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
            // CR 903.5b: 2HG Commander decks are Commander singleton decks.
            | GameFormat::TwoHeadedGiantCommander
            // CR 904.13a + CR 903.5b: Commander singleton decks.
            | GameFormat::ArchenemyCommander
            // CR 903.5b: Planechase Commander decks are Commander singleton decks.
            | GameFormat::PlanechaseCommander
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
            | GameFormat::Momir
            // The 80-card list holds ten Dandân, above CR 100.2a's four.
            | GameFormat::Dandan => DeckCopyLimit::Unlimited,
            // Freeform's own rules lift CR 100.2a's four-card limit outright.
            // A card's PRINTED deck-construction limit still binds underneath
            // this default -- see `effective_copy_limit`.
            GameFormat::Freeform => DeckCopyLimit::Unlimited,
            // Freeform Commander's own rules lift CR 100.2a's four-card limit
            // outright, the same as Freeform's — a separate arm for the same
            // reason `card_pool`'s is: a card's PRINTED deck-construction
            // limit still binds underneath this default (MEASURED: 2 copies
            // of a card with a printed singleton limit are refused).
            GameFormat::FreeformCommander => DeckCopyLimit::Unlimited,
            // Constructed-block hordes leave survivor decks unrestricted; the
            // `horde()` constructor tightens this to `UpTo(1)` for
            // Commander-format hordes (a tighter field than the method is
            // allowed — only a looser one is rejected at deserialization).
            GameFormat::Horde => DeckCopyLimit::Unlimited,
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
    /// `RulesFixed` — the magnitude each format's own rules already fix,
    /// whatever that magnitude is.
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
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
            | GameFormat::PlanechaseCommander
            | GameFormat::Momir
            | GameFormat::Dandan
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::Horde
            | GameFormat::FreeformCommander => DeckSizeAuthority::RulesFixed,
            // R6 (review round 3): exhaustive, no wildcard. Unreachable by
            // construction from the admission gate: `for_format` returns Err
            // for Custom before any verdict row runs, and a Custom payload is
            // settled by the `Some(rules)` arm's blanket equality against
            // `FormatConfig::for_custom_rules`. `RulesFixed` is the
            // total-function answer for other callers, not a policy choice.
            GameFormat::Custom(_) => DeckSizeAuthority::RulesFixed,
        }
    }

    /// Which pile this format's `DeckSizeRule` measures — the single
    /// authority.
    ///
    /// `Custom(_)` answers `MainDeck`, disclosed fail-closed: both custom
    /// evaluators (`evaluate_custom_format`, `quick_custom_format_check`) are
    /// constructed-shaped only.
    pub fn deck_size_subject(self) -> DeckSizeSubject {
        match self {
            GameFormat::Commander
            // CR 903.5a: exactly 100 cards, INCLUDING the commander.
            | GameFormat::TwoHeadedGiantCommander
            // CR 904.13a + CR 903.5a: exactly 100, INCLUDING the commander.
            | GameFormat::ArchenemyCommander
            // CR 903.5a: exactly 100, INCLUDING the commander.
            | GameFormat::PlanechaseCommander
            | GameFormat::DuelCommander
            | GameFormat::PauperCommander
            | GameFormat::CommanderDraft
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl
            | GameFormat::TinyLeaders
            // This format's own rules state no deck-size requirement, so
            // the declaration is a pinned property rather than a magnitude
            // any verdict reads.
            | GameFormat::FreeformCommander => DeckSizeSubject::MainDeckAndCommanders,
            GameFormat::Oathbreaker => DeckSizeSubject::MainDeckAndCommandZone,
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
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::Dandan
            | GameFormat::Freeform
            // Default (constructed) survivor decks count the main deck only;
            // Commander-format hordes validate survivors against a Commander
            // config, which answers `MainDeckAndCommanders` instead.
            | GameFormat::Horde
            | GameFormat::Custom(_) => DeckSizeSubject::MainDeck,
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
        match self {
            GameFormat::Commander
            | GameFormat::PauperCommander
            | GameFormat::DuelCommander
            | GameFormat::Oathbreaker
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl
            // CR 904.13a: a two-seat Archenemy Commander game is a duel
            // fought with Commander decks, so the Commander-style grant
            // reaches it like every other Commander-deck duel above.
            // (3+ seats get the free first mulligan via CR 103.5c anyway.)
            | GameFormat::ArchenemyCommander
            // Freeform Commander grants the free first mulligan, matching
            // every other Commander-style duel above.
            | GameFormat::FreeformCommander
            // A two-seat Planechase Commander game is a duel fought with
            // Commander decks, so the Commander-style grant reaches it like
            // Archenemy Commander above. (3+ seats get the free first
            // mulligan via CR 103.5c anyway.)
            | GameFormat::PlanechaseCommander => true,
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
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            // Deliberately false: 2HG Commander never seats a duel (CR 810.1
            // fixes four seats), so this duel-only override is unreachable —
            // the free first mulligan arrives via CR 103.5c's 3+-seat rule.
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            // CR 103.5c's free first mulligan is multiplayer and Brawl only;
            // Dandân's free mulligan is the `free_reveal_mulligan()` axis.
            | GameFormat::Dandan
            | GameFormat::CommanderDraft
            // Plain Freeform is an unrestricted constructed format, not a
            // Commander-style or Brawl variant, so it gets neither
            // CR 103.5c's Brawl clause nor the Commander Rules Committee's
            // supplementary rule.
            | GameFormat::Freeform
            // Like Archenemy: 3+ seat games grant the free mulligan by seat
            // count (`mulligan.rs` ORs that in), so this arm only governs the
            // 1-survivor edge, which mulligans normally.
            | GameFormat::Horde => false,
            // Exhaustive rather than `matches!`, matching `deck_supply`'s
            // and `has_unrepresentable_auxiliary_deck_component`'s style: a
            // future built-in must force a deliberate `true`/`false` choice
            // here.
            GameFormat::Custom(_) => false,
        }
    }

    /// Whether this format is a Two-Headed Giant game: two teams of two
    /// players each (CR 810.1) sharing life, poison, and loss (CR 810.4 /
    /// CR 810.8 / CR 810.9 / CR 810.10) on shared team turns (CR 810.2).
    /// Both the 60-card variant and the Commander-composed variant qualify —
    /// CR 103.8b's first-turn draw skip names Two-Headed Giant specifically
    /// and reaches both, while CR 805 shared-team-turns alone (Archenemy)
    /// does not qualify.
    pub fn is_two_headed_giant_family(self) -> bool {
        matches!(
            self,
            GameFormat::TwoHeadedGiant | GameFormat::TwoHeadedGiantCommander
        )
    }

    /// Whether this format seats one archenemy against a team of heroes on
    /// shared team turns (CR 904.2) with a single scheme deck (CR 904.3 /
    /// CR 904.13d). Both the 60-card variant and the Commander-composed
    /// variant qualify — scheme-deck loading, the scheme upkeep trigger,
    /// and the archenemy-first turn order reach both.
    pub fn is_archenemy_family(self) -> bool {
        matches!(self, GameFormat::Archenemy | GameFormat::ArchenemyCommander)
    }

    /// Whether planar-deck machinery applies: the communal planar deck
    /// (CR 901.15a), starting-plane reveal (CR 901.5), and die rolling
    /// (CR 901.9). Both the 60-card variant and the Commander-composed
    /// variant qualify — planar loading, reveal, and rolling reach both.
    pub fn is_planechase_family(self) -> bool {
        matches!(
            self,
            GameFormat::Planechase | GameFormat::PlanechaseCommander
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::DuelCommander
            | GameFormat::PauperCommander
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl
            | GameFormat::PlanechaseCommander
            // `command_zone: true` and
            // `commander_damage_threshold: Some(21)` travel together for this
            // format — `Derived` in `built_in_axes_no_looser_than_rules`.
            | GameFormat::FreeformCommander
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
            | GameFormat::Momir
            | GameFormat::Dandan
            // Default (constructed) hordes run no commanders; Commander-format
            // hordes set `uses_commander: true` on the resolved config instead.
            | GameFormat::Horde
            | GameFormat::Freeform => Ok(false),
            GameFormat::Custom(id) => Err(FormatConfigError(format!(
                "uses_commander cannot resolve ad-hoc Custom format {} — read \
                 FormatConfig.uses_commander from the resolved config instead",
                id.0
            ))),
        }
    }

    /// Whether this format's command zone is filled from the decklist's
    /// `commander` slot — i.e. the zone holds a card that is part of the deck
    /// proper and is therefore netted against the main deck.
    ///
    /// This is deliberately NOT `uses_commander`, and NOT
    /// `FormatConfig::command_zone`. The three predicates answer different
    /// questions and their answers genuinely differ:
    ///
    /// - `uses_commander` is `command_zone && commander_damage_threshold`, so
    ///   it means "CR 903.10a commander damage applies". Tiny Leaders and
    ///   Oathbreaker put a real card in the command zone but declare no
    ///   damage threshold, so they answer `false` there while answering
    ///   `true` here.
    /// - `command_zone` is true for Archenemy (CR 904.3 + CR 904.4: the
    ///   supplementary scheme deck, which remains in the command zone) and
    ///   Momir (its emblem), whose zones hold no decklist card at all, so
    ///   they answer `true` there while answering `false` here.
    ///
    /// The netting invariant is format-neutral: wherever a format designates a
    /// decklist card for the command zone, that card is COUNTED IN that
    /// format's deck, so a decklist naming it in both the command zone and the
    /// main deck describes ONE physical card, not two. Only the deck size
    /// differs per format, which is why this predicate cannot be phrased in
    /// terms of any single count:
    ///
    /// - CR 903.5a — Commander: exactly 100 cards, INCLUDING its commander.
    /// - CR 903.12d — Brawl: exactly 60 cards, including its commander
    ///   (CR 903.12a makes Brawl a Commander variant, so CR 903.5's identity
    ///   carries over with this size modification).
    /// - CR 903.13f — Commander Draft: at least 60 with no maximum, otherwise
    ///   following CR 903.5 deck construction.
    /// - Tiny Leaders and Oathbreaker appear NOWHERE in the Comprehensive
    ///   Rules — both are community/WPN formats, so their command-zone
    ///   identity comes from their own rules documents (Oathbreaker RC: 60
    ///   cards including the Oathbreaker and signature spell), not from
    ///   CR 903. Do not annotate them with a CR number.
    ///
    /// `deck_validation`'s `CommandZoneNetting::NetAgainstMainDeck` call sites
    /// are exactly the formats that answer `true` here, and `deck_loading`
    /// reads this same predicate for both netting and command-zone placement —
    /// a format that netted without placing would start the game a card short,
    /// and one that placed without netting would start it a card long.
    ///
    /// Returns `Err` for `GameFormat::Custom` for the same reason
    /// `uses_commander` does: a bare `GameFormat` carries no
    /// `CustomFormatRules` to answer from, and `false` would be silently
    /// wrong rather than a safe default.
    pub fn command_zone_holds_decklist_commander(self) -> Result<bool, FormatConfigError> {
        match self {
            GameFormat::Commander
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::DuelCommander
            | GameFormat::PauperCommander
            | GameFormat::Brawl
            | GameFormat::HistoricBrawl
            | GameFormat::CommanderDraft
            | GameFormat::FreeformCommander
            // Tiny Leaders and Oathbreaker seat a decklist card in the command
            // zone without a commander-damage threshold, which is precisely the
            // case `uses_commander` cannot express.
            | GameFormat::TinyLeaders
            | GameFormat::Oathbreaker => Ok(true),
            // Archenemy's scheme deck (CR 904.3: the supplementary deck;
            // CR 904.4: its cards remain in the command zone) and Momir's
            // emblem occupy a
            // command zone that holds no decklist card, so there is nothing to
            // net and nothing to place from the `commander` slot.
            GameFormat::Archenemy
            | GameFormat::Momir
            | GameFormat::Dandan
            | GameFormat::Standard
            | GameFormat::Limited
            | GameFormat::Pioneer
            | GameFormat::Modern
            | GameFormat::Premodern
            | GameFormat::Legacy
            | GameFormat::Vintage
            | GameFormat::Historic
            | GameFormat::Timeless
            | GameFormat::Pauper
            | GameFormat::FreeForAll
            | GameFormat::TwoHeadedGiant
            | GameFormat::Planechase
            // Horde is per-config (Commander-format survivors seat a decklist
            // commander; see the `FormatConfig`-level override), so the
            // format-level axis reports the emblem-only default.
            | GameFormat::Horde
            | GameFormat::Freeform => Ok(false),
            GameFormat::Custom(id) => Err(FormatConfigError(format!(
                "command_zone_holds_decklist_commander cannot resolve ad-hoc Custom format {} — \
                 a bare GameFormat carries no CustomFormatRules; decide from the resolved \
                 FormatConfig (its command_zone and uses_commander fields) instead",
                id.0
            ))),
        }
    }

    /// Whether the engine supplies this format's deck or a default for it, so
    /// the player builds no deck. The frontend consumes the derived
    /// `FormatConfig::supplies_fixed_deck` field to bypass deck-selection gates,
    /// and must never re-list such formats client-side.
    pub fn supplies_fixed_deck(self) -> bool {
        // Horde answers false via `deck_supply` (PlayerBuilt): only the Horde
        // *seat* plays an engine-supplied library (injected like Archenemy's
        // shared scheme deck), while the survivors — the human players —
        // still build and select their own decks.
        self.deck_supply() != DeckSupply::PlayerBuilt
    }

    /// Who supplies this format's deck: Momir's Madness loads the fixed 60-card
    /// snow-basic list (`deck_loading::momir_fixed_deck_names`) for every seat,
    /// and Dandân plays one 80-card pile, the host's submission or the default
    /// list (`deck_loading::dandan_default_pile_names`).
    pub fn deck_supply(self) -> DeckSupply {
        match self {
            GameFormat::Momir => DeckSupply::EngineFixed,
            GameFormat::Dandan => DeckSupply::HostPile,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::Archenemy
            | GameFormat::ArchenemyCommander
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            // CR 903.13e: the drafted cards become the player's card pool and
            // they build a deck from it, so the engine supplies nothing.
            | GameFormat::CommanderDraft
            // Survivors build and select their own decks; only the Horde
            // seat's library is engine-injected (like Archenemy's scheme deck).
            | GameFormat::Horde
            | GameFormat::Planechase
            | GameFormat::PlanechaseCommander => DeckSupply::PlayerBuilt,
            // No custom-format use case for an engine-supplied deck exists
            // today — a real one would need its own design, analogous to
            // Momir's Madness.
            GameFormat::Custom(_) => DeckSupply::PlayerBuilt,
        }
    }

    /// True for a built-in format whose `game::deck_loading` behavior grants
    /// an auxiliary deck or component keyed on this literal `GameFormat`
    /// variant (or its family predicate), with no `StructuralRules` field
    /// able to represent it: a shared communal planar deck (Planechase,
    /// CR 901.15a, `load_shared_planar_deck`), a supplementary scheme deck
    /// (Archenemy, CR 904.3, and Archenemy Commander, CR 904.13d, both via
    /// `load_shared_scheme_deck` behind `is_archenemy_family`), a
    /// game-start emblem (Momir, CR 109.4c / CR 114.1, `grant_emblem`), or
    /// Dandan's shared library and graveyard (the `shared_zones()` axis,
    /// keyed on this literal format). A custom-format definition modeled
    /// after one of these would resolve to a config that looks
    /// structurally sound but never receives the grant, since
    /// `deck_loading.rs` checks the `GameFormat` enum directly rather
    /// than reading any `StructuralRules` field.
    ///
    /// Used by `custom_format::CustomFormatDef::from_lobby_config` to reject
    /// all six as lobby-config sources. Archenemy and Momir both also set
    /// `command_zone: true` with no `CommanderEligibilityRule`, so they are
    /// independently unrepresentable for that reason too; Planechase's
    /// `command_zone` is `false`, so this predicate is the only guard that
    /// reaches it, as it is for Dandan. Archenemy Commander pairs
    /// `command_zone: true` WITH a
    /// `CommanderEligibilityRule::Standard`, so this predicate is likewise
    /// its only guard.
    pub fn has_unrepresentable_auxiliary_deck_component(self) -> bool {
        match self {
            GameFormat::Planechase
            | GameFormat::Archenemy
            // The scheme deck loads through the same format-gated path as
            // default Archenemy (CR 904.13d), so Archenemy Commander is
            // unrepresentable for Custom for the same reason.
            | GameFormat::ArchenemyCommander
            // The planar deck loads through the same format-gated path as
            // default Planechase (CR 901.15a), so Planechase Commander is
            // unrepresentable for Custom for the same reason.
            | GameFormat::PlanechaseCommander
            | GameFormat::Momir
            // The Horde's fixed library and game-start emblem are injected by
            // deck_loading keyed on the literal `GameFormat::Horde`.
            | GameFormat::Horde
            | GameFormat::Dandan => true,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander => false,
            // Exhaustive rather than `matches!`, matching `deck_supply`'s
            // style: a future built-in that grants its own deck_loading.rs
            // auxiliary component must force a deliberate `true`/`false` choice
            // here, not silently default to unrepresented. No custom-format use
            // case for this exists today.
            GameFormat::Custom(_) => false,
        }
    }

    /// CR 400.1: the zones this format shares between seats; every other
    /// format, and every `Custom(_)`, gives each player their own.
    pub fn shared_zones(self) -> SharedZones {
        match self {
            GameFormat::Dandan => SharedZones::LIBRARY_AND_GRAVEYARD,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            | GameFormat::Horde
            | GameFormat::Custom(_) => SharedZones::NONE,
        }
    }

    /// CR 121.2c: the order simultaneous draws and the pregame deal are
    /// performed in.
    pub fn deal_order(self) -> DealOrder {
        match self {
            GameFormat::Dandan => DealOrder::Interleaved,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            | GameFormat::Horde
            | GameFormat::Custom(_) => DealOrder::PlayerByPlayer,
        }
    }

    /// CR 103.5: the mulligan a player may take by revealing a qualifying hand,
    /// if the format offers one.
    pub fn free_reveal_mulligan(self) -> FreeRevealMulligan {
        match self {
            GameFormat::Dandan => FreeRevealMulligan::WhenHandLacks {
                min_lands: 2,
                min_nonlands: 2,
            },
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            | GameFormat::Horde
            | GameFormat::Custom(_) => FreeRevealMulligan::Unavailable,
        }
    }

    /// CR 108.3 / CR 110.2 / CR 400.3: who owns a card a player takes into
    /// hand from a shared zone, casts, or plays as a land.
    pub fn entry_ownership(self) -> EntryOwnership {
        match self {
            GameFormat::Dandan => EntryOwnership::ActorOwns,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            | GameFormat::Horde
            | GameFormat::Custom(_) => EntryOwnership::OwnerKept,
        }
    }

    /// Whether every opening hand a seat can draw is equivalent, so that a
    /// mulligan changes nothing.
    pub fn opening_hand_equivalence(self) -> OpeningHandEquivalence {
        match self {
            GameFormat::Dandan => OpeningHandEquivalence::Distinguishable,
            GameFormat::Momir => OpeningHandEquivalence::Equivalent,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            | GameFormat::Horde
            | GameFormat::Custom(_) => OpeningHandEquivalence::Distinguishable,
        }
    }

    /// CR 100.6a describes the usual two-win match and CR 100.4 sideboarding
    /// between games; this is the longest match the format allows, and a
    /// host-supplied match config never raises it.
    pub fn best_of_three_ceiling(self) -> MatchType {
        match self {
            GameFormat::Dandan => MatchType::Bo1,
            // Single games with no sideboard and no multi-game match shape.
            GameFormat::Horde => MatchType::Bo1,
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
            | GameFormat::TwoHeadedGiantCommander
            | GameFormat::ArchenemyCommander
            | GameFormat::PlanechaseCommander
            | GameFormat::Archenemy
            | GameFormat::Planechase
            | GameFormat::Momir
            | GameFormat::CommanderDraft
            | GameFormat::Freeform
            | GameFormat::FreeformCommander
            | GameFormat::Custom(_) => MatchType::Bo3,
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
            GameFormat::TwoHeadedGiantCommander => Cow::Borrowed("Two-Headed Giant Commander"),
            GameFormat::Archenemy => Cow::Borrowed("Archenemy"),
            GameFormat::ArchenemyCommander => Cow::Borrowed("Archenemy Commander"),
            GameFormat::Planechase => Cow::Borrowed("Planechase"),
            GameFormat::PlanechaseCommander => Cow::Borrowed("Planechase Commander"),
            GameFormat::Momir => Cow::Borrowed("Momir's Madness"),
            GameFormat::Horde => Cow::Borrowed("Horde"),
            GameFormat::CommanderDraft => Cow::Borrowed("Commander Draft"),
            GameFormat::Freeform => Cow::Borrowed("Freeform"),
            GameFormat::FreeformCommander => Cow::Borrowed("Freeform Commander"),
            GameFormat::Dandan => Cow::Borrowed("Dand\u{e2}n"),
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
                legality_key: GameFormat::Standard.legality_key(),
                default_config: FormatConfig::standard(),
            },
            FormatMetadata {
                format: GameFormat::Pioneer,
                label: "Pioneer",
                short_label: "PIO",
                description: "Non-rotating from 2012",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Pioneer.legality_key(),
                default_config: FormatConfig::pioneer(),
            },
            FormatMetadata {
                format: GameFormat::Modern,
                label: "Modern",
                short_label: "MOD",
                description: "Non-rotating from Mirrodin onward",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Modern.legality_key(),
                default_config: FormatConfig::modern(),
            },
            FormatMetadata {
                format: GameFormat::Premodern,
                label: "Premodern",
                short_label: "PRE",
                description: "Old-frame constructed through Scourge",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Premodern.legality_key(),
                default_config: FormatConfig::premodern(),
            },
            FormatMetadata {
                format: GameFormat::Legacy,
                label: "Legacy",
                short_label: "LEG",
                description: "Eternal format, all sets legal",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Legacy.legality_key(),
                default_config: FormatConfig::legacy(),
            },
            FormatMetadata {
                format: GameFormat::Vintage,
                label: "Vintage",
                short_label: "VIN",
                description: "Broadest pool, Power Nine restricted",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Vintage.legality_key(),
                default_config: FormatConfig::vintage(),
            },
            FormatMetadata {
                format: GameFormat::Historic,
                label: "Historic",
                short_label: "HIS",
                description: "Arena's eternal format",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Historic.legality_key(),
                default_config: FormatConfig::historic(),
            },
            FormatMetadata {
                format: GameFormat::Timeless,
                label: "Timeless",
                short_label: "TML",
                description: "Arena's eternal non-rotating format",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Timeless.legality_key(),
                default_config: FormatConfig::timeless(),
            },
            FormatMetadata {
                format: GameFormat::Pauper,
                label: "Pauper",
                short_label: "PAU",
                description: "Commons only",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Pauper.legality_key(),
                default_config: FormatConfig::pauper(),
            },
            FormatMetadata {
                format: GameFormat::Freeform,
                label: "Freeform",
                short_label: "FRF",
                description: "Every set, no bans, no copy limit, no deck minimum",
                group: FormatGroup::Constructed,
                legality_key: GameFormat::Freeform.legality_key(),
                default_config: FormatConfig::freeform(),
            },
            FormatMetadata {
                format: GameFormat::Commander,
                label: "Commander",
                short_label: "CMD",
                description: "100-card singleton, 2\u{2013}4 players",
                group: FormatGroup::Commander,
                legality_key: GameFormat::Commander.legality_key(),
                default_config: FormatConfig::commander(),
            },
            FormatMetadata {
                format: GameFormat::TwoHeadedGiantCommander,
                label: "Two-Headed Giant Commander",
                short_label: "2HGC",
                description: "2v2 Commander, 60 shared life per team",
                group: FormatGroup::Commander,
                legality_key: GameFormat::TwoHeadedGiantCommander.legality_key(),
                default_config: FormatConfig::two_headed_giant_commander(),
            },
            FormatMetadata {
                format: GameFormat::ArchenemyCommander,
                label: "Archenemy Commander",
                short_label: "ARCC",
                description: "1-vs-many Commander, 60 life each side",
                group: FormatGroup::Commander,
                legality_key: GameFormat::ArchenemyCommander.legality_key(),
                default_config: FormatConfig::archenemy_commander(),
            },
            FormatMetadata {
                format: GameFormat::PlanechaseCommander,
                label: "Planechase Commander",
                short_label: "PLCC",
                description: "Commander multiplayer with a communal planar deck",
                group: FormatGroup::Commander,
                legality_key: GameFormat::PlanechaseCommander.legality_key(),
                default_config: FormatConfig::planechase_commander(),
            },
            FormatMetadata {
                format: GameFormat::DuelCommander,
                label: "Duel Commander",
                short_label: "DUC",
                description: "Tournament 1v1 Commander, 30 life",
                group: FormatGroup::Commander,
                legality_key: GameFormat::DuelCommander.legality_key(),
                default_config: FormatConfig::duel_commander(),
            },
            FormatMetadata {
                format: GameFormat::PauperCommander,
                label: "Pauper Commander",
                short_label: "PDH",
                description: "Commons-only singleton Commander",
                group: FormatGroup::Commander,
                legality_key: GameFormat::PauperCommander.legality_key(),
                default_config: FormatConfig::pauper_commander(),
            },
            FormatMetadata {
                format: GameFormat::TinyLeaders,
                label: "Tiny Leaders: Reborn",
                short_label: "TLR",
                description: "50-card Tiny singleton",
                group: FormatGroup::Commander,
                legality_key: GameFormat::TinyLeaders.legality_key(),
                default_config: FormatConfig::tiny_leaders(),
            },
            FormatMetadata {
                format: GameFormat::Oathbreaker,
                label: "Oathbreaker",
                short_label: "OBK",
                description: "60-card singleton, Planeswalker + signature spell",
                group: FormatGroup::Commander,
                legality_key: GameFormat::Oathbreaker.legality_key(),
                default_config: FormatConfig::oathbreaker(),
            },
            FormatMetadata {
                format: GameFormat::Brawl,
                label: "Brawl",
                short_label: "BRL",
                description: "60-card Standard singleton",
                group: FormatGroup::Commander,
                legality_key: GameFormat::Brawl.legality_key(),
                default_config: FormatConfig::brawl(),
            },
            FormatMetadata {
                format: GameFormat::HistoricBrawl,
                label: "Historic Brawl",
                short_label: "HBR",
                description: "100-card eternal singleton",
                group: FormatGroup::Commander,
                legality_key: GameFormat::HistoricBrawl.legality_key(),
                default_config: FormatConfig::historic_brawl(),
            },
            FormatMetadata {
                format: GameFormat::CommanderDraft,
                label: "Commander Draft",
                short_label: "CDR",
                description: "Drafted 60-card minimum Commander, 3\u{2013}8 players",
                group: FormatGroup::Commander,
                legality_key: GameFormat::CommanderDraft.legality_key(),
                default_config: FormatConfig::commander_draft(),
            },
            FormatMetadata {
                format: GameFormat::FreeformCommander,
                label: "Freeform Commander",
                short_label: "FFC",
                description: "Any castable card as your commander, every set, no deck minimum",
                group: FormatGroup::Commander,
                legality_key: GameFormat::FreeformCommander.legality_key(),
                default_config: FormatConfig::freeform_commander(),
            },
            FormatMetadata {
                format: GameFormat::FreeForAll,
                label: "Free-for-All",
                short_label: "FFA",
                description: "3\u{2013}6 player battle royale",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::FreeForAll.legality_key(),
                default_config: FormatConfig::free_for_all(),
            },
            FormatMetadata {
                format: GameFormat::TwoHeadedGiant,
                label: "Two-Headed Giant",
                short_label: "2HG",
                description: "4 players, two teams of two",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::TwoHeadedGiant.legality_key(),
                default_config: FormatConfig::two_headed_giant(),
            },
            FormatMetadata {
                format: GameFormat::Archenemy,
                label: "Archenemy",
                short_label: "ARC",
                description: "One archenemy against a team of heroes",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::Archenemy.legality_key(),
                default_config: FormatConfig::archenemy(),
            },
            FormatMetadata {
                format: GameFormat::Planechase,
                label: "Planechase",
                short_label: "PLC",
                description: "60-card multiplayer with a communal planar deck",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::Planechase.legality_key(),
                default_config: FormatConfig::planechase(),
            },
            FormatMetadata {
                format: GameFormat::Limited,
                label: "Limited",
                short_label: "LIM",
                description: "Draft or sealed, 40-card deck",
                group: FormatGroup::Limited,
                legality_key: GameFormat::Limited.legality_key(),
                default_config: FormatConfig::limited(),
            },
            FormatMetadata {
                format: GameFormat::Momir,
                label: "Momir's Madness",
                short_label: "MOM",
                description: "60 snow basic lands, random creature tokens",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::Momir.legality_key(),
                default_config: FormatConfig::momir(),
            },
            FormatMetadata {
                format: GameFormat::Horde,
                label: "Horde",
                short_label: "HRD",
                description: "Co-op team vs a self-piloting Horde deck",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::Horde.legality_key(),
                default_config: FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            },
            FormatMetadata {
                format: GameFormat::Dandan,
                label: "Dand\u{e2}n",
                short_label: "DAN",
                description: "Shared library and graveyard, fixed 80-card deck",
                group: FormatGroup::Multiplayer,
                legality_key: GameFormat::Dandan.legality_key(),
                default_config: FormatConfig::dandan(),
            },
        ]
    }
}

/// The built-in format each legality table records. Exhaustive, so a new
/// table cannot compile until it names its format.
fn legality_table_format(table: LegalityFormat) -> GameFormat {
    match table {
        LegalityFormat::Standard => GameFormat::Standard,
        LegalityFormat::Commander => GameFormat::Commander,
        LegalityFormat::Modern => GameFormat::Modern,
        LegalityFormat::Premodern => GameFormat::Premodern,
        LegalityFormat::Pioneer => GameFormat::Pioneer,
        LegalityFormat::Legacy => GameFormat::Legacy,
        LegalityFormat::Vintage => GameFormat::Vintage,
        LegalityFormat::Pauper => GameFormat::Pauper,
        LegalityFormat::Historic => GameFormat::Historic,
        LegalityFormat::Brawl => GameFormat::HistoricBrawl,
        LegalityFormat::StandardBrawl => GameFormat::Brawl,
        LegalityFormat::Timeless => GameFormat::Timeless,
        LegalityFormat::PauperCommander => GameFormat::PauperCommander,
        LegalityFormat::DuelCommander => GameFormat::DuelCommander,
        LegalityFormat::Oathbreaker => GameFormat::Oathbreaker,
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
            // CR 810.1 + CR 810.2: 2HG Commander seats two teams of two on
            // shared team turns, exactly like 60-card 2HG. Explicit rather
            // than falling through to the `team_based` arm below so the
            // seat shape stays pinned to the variant, not to a host-tunable
            // axis.
            GameFormat::TwoHeadedGiantCommander => FormatTopology::FixedTeams {
                team_size: 2,
                team_count: 2,
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            GameFormat::ArchenemyCommander => FormatTopology::OneVsMany {
                archenemy: self.archenemy_player.unwrap_or(PlayerId(0)),
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            GameFormat::Archenemy => FormatTopology::OneVsMany {
                archenemy: self.archenemy_player.unwrap_or(PlayerId(0)),
                turn_structure: TurnStructure::SharedTeamTurns,
            },
            // Horde reuses the one-vs-many topology with the Horde as the
            // "archenemy" seat and the survivors as a shared-turn team. The
            // survivors-first turn order and no-life-total handling are applied
            // in `starting_player`/`starting_life_for_player`, not here.
            GameFormat::Horde => FormatTopology::OneVsMany {
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

    /// NOTE: `validate_starting_life_bounds` (shared by both
    /// `FormatConfig::deserialize` arms) depends on this function's
    /// `FixedTeams` division behavior (declared life divided by
    /// `team_size`) to floor every seat above 0 — see that function's own
    /// comment and the pinning test on this function.
    pub fn starting_life_for_seat(&self) -> i32 {
        match self.topology() {
            FormatTopology::IndividualSeats => self.starting_life,
            FormatTopology::FixedTeams { team_size, .. } => {
                self.starting_life / i32::from(team_size)
            }
            FormatTopology::OneVsMany { .. } => self.starting_life,
        }
    }

    /// Per-seat starting life for a session being built with `player_count`
    /// seats. The count only matters for Archenemy Commander heroes (see
    /// below); every other format ignores it, and `GameState::new` — the
    /// sole production caller — always passes the session's seat count.
    pub fn starting_life_for_player(&self, player: PlayerId, player_count: usize) -> i32 {
        match self.topology() {
            FormatTopology::IndividualSeats => self.starting_life,
            FormatTopology::FixedTeams { team_size, .. } => {
                self.starting_life / i32::from(team_size)
            }
            // Horde and Archenemy both map to OneVsMany, so the format must be
            // distinguished here.
            FormatTopology::OneVsMany { archenemy, .. } => {
                if self.format == GameFormat::Horde {
                    // The Horde has no life total (it loses by decking out, not
                    // by life), so its own seat's life value is never consulted.
                    // Every seat is seeded here with the survivors' single-
                    // survivor combined base life, and that combined total is
                    // distributed across the survivor seats in `GameState::new`
                    // once the seat count is known (CR 810.9a reads each seat
                    // through the shared team total). The base lives on the
                    // ruleset (per-deck: community 100, Theros 20); fall back to
                    // the format-wide `starting_life` only if a Horde config was
                    // somehow built without a ruleset. The Horde is NOT given the
                    // archenemy's 40 life.
                    self.horde_ruleset
                        .as_ref()
                        .map_or(self.starting_life, |r| r.combined_base_life)
                } else if self.format == GameFormat::ArchenemyCommander {
                    // CR 904.13b: the archenemy starts at 60; the heroes
                    // split their shared 60 evenly across seats. 60 divides
                    // evenly by every legal hero count (1-5), so the split
                    // is exact and the seats sum back to 60.
                    if player == archenemy {
                        60
                    } else {
                        let heroes = player_count.saturating_sub(1).max(1) as i32;
                        60 / heroes
                    }
                } else if player == archenemy {
                    // CR 904.5: The archenemy starts at 40 life; each other
                    // player starts at 20. This is not a shared life total.
                    40
                } else {
                    20
                }
            }
        }
    }

    /// CR 103.4 + CR 810.4 + CR 904.5 + CR 904.13b: Starting-life quantities
    /// read the format's rules baseline for the referenced player. Individual
    /// formats use the configured total; FixedTeams use the configured shared
    /// team total; OneVsMany uses the selected seat's individual total (40 for
    /// the archenemy, 20 for each hero in default Archenemy) or, in Archenemy
    /// Commander, 60 for the archenemy and the heroes' shared 60 for each
    /// hero — the team total, not the per-seat split.
    pub fn starting_life_total_for_player(&self, player: PlayerId, player_count: usize) -> i32 {
        match self.topology() {
            FormatTopology::IndividualSeats | FormatTopology::FixedTeams { .. } => {
                self.starting_life
            }
            FormatTopology::OneVsMany { .. } => {
                if self.format == GameFormat::ArchenemyCommander {
                    60
                } else {
                    self.starting_life_for_player(player, player_count)
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

    /// The Horde seats a game designates under this config: the primary
    /// (`archenemy_player`) plus one seat per co-Horde deck at consecutive
    /// offsets. UNFILTERED — seats past the table edge are included, so
    /// callers must validate every seat exists (see
    /// `validate_for_player_count`); `GameState::new` filters defensively.
    /// Empty for non-Horde formats or a missing primary, matching the
    /// designation rule in `GameState::new`. Single authority for the
    /// primary+offsets rule so validation and allocation cannot drift.
    pub fn horde_designated_seats(&self) -> Vec<PlayerId> {
        if self.format != GameFormat::Horde {
            return Vec::new();
        }
        let Some(primary) = self.archenemy_player() else {
            return Vec::new();
        };
        let deck_count = self
            .horde_ruleset
            .as_ref()
            .map_or(1, |r| r.horde_decks().len());
        (0..deck_count)
            .map(|offset| PlayerId(primary.0.saturating_add(offset as u8)))
            .collect()
    }

    /// Bounds a runtime seat count against this format's registry range. Owns
    /// the seat-count bound and is the runtime pair of the `max_players`
    /// admission row in `built_in_axes_no_looser_than_rules`: that row bounds
    /// the format invariant a payload may declare, this one bounds the seat
    /// count a session is actually built with.
    ///
    /// That pairing only holds when a payload actually declared a config. An
    /// UNDECLARED config is not a rules choice to bind a seat count against —
    /// both call sites that can build a session from an undeclared config,
    /// `server_core::session::SessionManager::create_game_n_players` and the
    /// WASM `initialize_game_impl`, instead pick a default whose own
    /// registry range already admits the requested seat count via the
    /// shared `FormatConfig::default_for_player_count` (Standard for
    /// `player_count <= 2`, Free-for-All above that) before this function
    /// ever runs, so this check never binds an undeclared config against an
    /// unrelated format's range.
    ///
    /// No protocol-version bump accompanies this: no wire *shape* changed (no
    /// new field on any `Serialize`/`Deserialize` type), but this IS a
    /// behavioral tightening — a `player_count` outside the format's range
    /// that an older server would have accepted is now rejected.
    ///
    /// Five production call sites reach this check, and they split into
    /// four groups by what they are validating:
    ///
    /// - Retryable wire rejections, validating a player_count just supplied
    ///   on an inbound request against a *declared* `FormatConfig` — the
    ///   check is skipped entirely when the request declares none: both
    ///   ingress guards (`lobby_broker::inbound_guard::guard_create_game_
    ///   settings_inbound` for `ServerMode::LobbyOnly`, and `phase-server`'s
    ///   own `guard_full_create_game_settings_inbound` for `ServerMode::Full`,
    ///   which clamps to `MAX_FULL_GAME_PLAYER_COUNT`). The client resubmits
    ///   a corrected request and no state is lost.
    /// - Session creation itself: `server_core::session::SessionManager::
    ///   create_game_n_players` re-checks the same bound the ingress guard
    ///   in front of it already checked, as defense in depth — and, unlike
    ///   the ingress guards, it always runs this check, because it is one of
    ///   the two sites (with the WASM site below) that must supply
    ///   `FormatConfig::default_for_player_count`'s output when the caller
    ///   declared no config at all.
    /// - The WASM `initialize_game_impl`'s `validate_external_format_config`
    ///   is also a retryable wire rejection, but unlike the two ingress
    ///   guards above it does NOT skip this check when the config is
    ///   undeclared: `initialize_game_impl` is the one call site that
    ///   substitutes `FormatConfig::default_for_player_count`'s output for
    ///   an undeclared config and then validates *that* default — passing no
    ///   format config at all is the common case for a local (non-networked)
    ///   game, so this check must run unconditionally here instead of being
    ///   skipped the way the ingress guards skip it.
    /// - Checked against a PERSISTED `player_count` instead — a game that
    ///   already exists, not a fresh request. `server_core::session::
    ///   GameSession::from_persisted` is a hard rejection here: a session
    ///   already saved with a `player_count` outside its format's registry
    ///   range can never be restored again through that path (see
    ///   `from_persisted_rejects_a_persisted_player_count_outside_the_
    ///   format_registry_range`). This is reachable, not merely
    ///   hypothetical — e.g. a `CommanderDraft` session (registry range
    ///   3..=8) persisted with `player_count` 2 from before this bound
    ///   existed on whichever path created it.
    ///
    /// The WASM restore boundary (`decode_and_rehydrate_restored_game_state`,
    /// shared by `restore_game_state` and `resume_multiplayer_host_state`) is
    /// NOT a call site of this function, even though it also handles
    /// persisted, already-exists state. Rejecting there — or repairing by
    /// widening the restored `format_config`'s `min_players`/`max_players`
    /// before calling this validator — was tried and reverted; see that
    /// closure's own comment for why widening is actively unsafe (a widened
    /// `min_players` fails `FormatConfig::deserialize`'s own admission gate
    /// on the very next load, since `min_players` is a Locked row in
    /// `built_in_axes_no_looser_than_rules`). This asymmetry with
    /// `from_persisted` is deliberate and accepted as-is; this paragraph is
    /// disclosure only.
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
        // One-vs-many seat check, phrased on the topology rather than the
        // format enum so every OneVsMany format — default Archenemy,
        // Archenemy Commander (CR 904.13), Horde — inherits this bound
        // without a second edit. The designated special seat (archenemy /
        // Horde) must be a valid seat index.
        if matches!(self.topology(), FormatTopology::OneVsMany { .. }) {
            let archenemy = self.archenemy_player().unwrap_or(PlayerId(0));
            if archenemy.0 >= player_count {
                let seat = if self.format == GameFormat::Horde {
                    "horde"
                } else {
                    "archenemy"
                };
                return Err(format!(
                    "{seat}_player must be less than player_count ({player_count})"
                ));
            }
        }
        // Horde designates the primary plus one seat per co-Horde deck
        // (`horde_designated_seats`); every designated seat must exist, and at
        // least one seat must remain for the survivors. Without this, a
        // two-player Two Towers game silently drops Saruman (seat 2 is past
        // the table edge) and deals a single incoherent Horde, and a
        // primary-0 two-player game leaves no survivors at all. Reject here —
        // `GameState::new` filters defensively but must never silently drop
        // in a validated session.
        if self.format == GameFormat::Horde {
            let designated = self.horde_designated_seats();
            if let Some(&missing) = designated.iter().find(|seat| seat.0 >= player_count) {
                return Err(format!(
                    "horde seat {} (of {} designated) is past the table edge for {player_count} players",
                    missing.0,
                    designated.len(),
                ));
            }
            if designated.len() >= player_count as usize {
                return Err(format!(
                    "horde designates {} of {player_count} seats, leaving no survivors",
                    designated.len(),
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
        // Horde: the survivors set up their board first (the published rules
        // give them several turns before the Horde's first turn), so the first
        // turn goes to a survivor, NOT the Horde: the lowest seat index
        // outside the COMPLETE Horde roster (primary plus every co-Horde —
        // a primary-only check would start a two-Horde game on the secondary
        // commander). `validate_for_player_count` guarantees a survivor seat
        // exists in every admitted session.
        if self.format == GameFormat::Horde {
            let roster = self.horde_designated_seats();
            return (0..=u8::MAX)
                .map(PlayerId)
                .find(|seat| !roster.contains(seat))
                .unwrap_or(PlayerId(0));
        }
        // CR 904.6: The archenemy takes the first turn instead of a randomly
        // determined player. Non-Archenemy formats keep the legacy default.
        self.archenemy_player().unwrap_or(PlayerId(0))
    }

    /// Chooses a registry-compatible default `FormatConfig` for an
    /// UNDECLARED configuration and a given seat count.
    ///
    /// A defaulted config is not a declaration: when the caller does not
    /// specify a format, choose a registry-compatible default for the
    /// REQUESTED seat count rather than always falling back to Standard.
    /// Standard is a two-player constructed format (registry range 2..=2);
    /// a 4-player "Standard" game would be incoherent regardless of which
    /// call site produces it. Free-for-All shares Standard's
    /// `IndividualSeats` topology and admits 2..=6, so the chosen default
    /// stays internally coherent (and survives a later
    /// `to_persisted`/`from_persisted` round-trip) instead of persisting a
    /// Standard config next to a `player_count` its own registry range
    /// excludes — that combination is exactly what `from_persisted` refuses
    /// irreversibly. Do NOT widen Standard via a struct-literal override
    /// (`FormatConfig { max_players: n, ..standard() }`) instead: that
    /// produces a config `FormatConfig::deserialize` itself rejects (the
    /// `min_players`/`max_players` rows in
    /// `built_in_axes_no_looser_than_rules` are Locked for built-in
    /// formats), bricking the very session this default is meant to keep
    /// restorable. Above Free-for-All's 6-seat ceiling, the caller's own
    /// `validate_for_player_count` bound correctly rejects the request —
    /// that is intentional, not a gap for this function to widen.
    ///
    /// This is the single shared authority for "what should an undeclared
    /// config default to for N seats?" — both
    /// `server_core::session::SessionManager::create_game_n_players` and
    /// the WASM `initialize_game_impl` call this function so the answer
    /// never drifts between the two ingresses. It is an engine-policy
    /// default, not a rules claim, so it is deliberately CR-free.
    pub fn default_for_player_count(player_count: u8) -> Self {
        if player_count <= 2 {
            Self::standard()
        } else {
            Self::free_for_all()
        }
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
            horde_ruleset: None,
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
            horde_ruleset: None,
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
            horde_ruleset: None,
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
            horde_ruleset: None,
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
            horde_ruleset: None,
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

    /// Freeform: no main-deck minimum, no copy limit, an unrestricted pool, and
    /// CR 100.4a's fifteen-card sideboard. `DeckSizeRule::Minimum(0)` is the
    /// deck-size rule's own parameter at its smallest value, not a second
    /// shape; `built_in_axes_no_looser_than_rules` locks a payload to exactly
    /// this. `sideboard_policy` and `default_deck_copy_limit` are named
    /// rather than inherited because `standard()` fills them from
    /// `GameFormat::Standard`'s own predicates.
    pub fn freeform() -> Self {
        FormatConfig {
            format: GameFormat::Freeform,
            deck_size: DeckSizeRule::Minimum(0),
            sideboard_policy: GameFormat::Freeform.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Freeform.default_deck_copy_limit(),
            ..Self::standard()
        }
    }

    /// Freeform Commander: a Commander-shaped casual variant. CR 903.7 fixes
    /// the Commander variant's starting life at 40, which this format
    /// declares as its default; the host adjusts it in the lobby.
    pub fn freeform_commander() -> Self {
        FormatConfig {
            format: GameFormat::FreeformCommander,
            // CR 903.7: each player sets their life total to 40.
            starting_life: 40,
            min_players: 2,
            max_players: 4,
            // No main-deck floor: the deck-size MAGNITUDE rule at its
            // degenerate end, not a new shape.
            deck_size: DeckSizeRule::Minimum(0),
            singleton: false,
            command_zone: true,
            // CR 903.10a: 21 combat damage from one commander.
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: true,
            sideboard_policy: GameFormat::FreeformCommander.sideboard_policy(),
            default_deck_copy_limit: GameFormat::FreeformCommander.default_deck_copy_limit(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            horde_ruleset: None,
            custom_rules: None,
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
            horde_ruleset: None,
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
            horde_ruleset: None,
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
            horde_ruleset: None,
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
            horde_ruleset: None,
            custom_rules: None,
        }
    }

    /// Dandân: one 80-card pile, the Secret Lair list by default and
    /// host-supplied otherwise, 20 life (CR 103.4), two players, no sideboard
    /// and no command zone.
    pub fn dandan() -> Self {
        FormatConfig {
            format: GameFormat::Dandan,
            starting_life: 20,
            min_players: 2,
            max_players: 2,
            deck_size: DeckSizeRule::Exactly(80),
            singleton: false,
            command_zone: false,
            commander_damage_threshold: None,
            range_of_influence: None,
            team_based: false,
            archenemy_player: None,
            uses_commander: false,
            sideboard_policy: GameFormat::Dandan.sideboard_policy(),
            default_deck_copy_limit: GameFormat::Dandan.default_deck_copy_limit(),
            supplies_fixed_deck: true,
            allow_debug_actions: false,
            horde_ruleset: None,
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
            horde_ruleset: None,
            custom_rules: None,
        }
    }

    /// Two-Headed Giant Commander: CR 810's team shape with CR 903's deck
    /// construction. The CR defines no combined variant, so the one number
    /// neither parent fixes — the shared starting life total — is 60 per
    /// team by played consensus (MTGO and paper 2HG Commander), not by CR
    /// citation. Everything else composes the parents' verified rules:
    /// CR 810.1 (two teams of two — exactly four seats), CR 810.4/810.8c
    /// (shared life total and loss), CR 810.8d (15 shared poison),
    /// CR 903.5a/903.5b (exactly 100, singleton), CR 903.5e (no sideboard),
    /// CR 903.6 (command zone), CR 903.8 (commander tax), and CR 903.10a
    /// (21 commander damage per player — the dealt head loses, and the team
    /// loses with it per CR 810.8a).
    pub fn two_headed_giant_commander() -> Self {
        FormatConfig {
            format: GameFormat::TwoHeadedGiantCommander,
            starting_life: 60,
            min_players: 4,
            max_players: 4,
            deck_size: DeckSizeRule::Exactly(100),
            singleton: true,
            command_zone: true,
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: true,
            archenemy_player: None,
            uses_commander: true,
            sideboard_policy: GameFormat::TwoHeadedGiantCommander.sideboard_policy(),
            default_deck_copy_limit: GameFormat::TwoHeadedGiantCommander.default_deck_copy_limit(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            horde_ruleset: None,
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
            horde_ruleset: None,
            custom_rules: None,
        }
    }

    /// Commander (CR 903) composed with the communal planar deck option
    /// (CR 901.15a). Commander governs construction (100 singleton),
    /// life (40), seating (2–6), command zone, and 21 commander damage;
    /// Planechase adds the shared planar deck, starting-plane reveal, and
    /// die rolling unchanged.
    pub fn planechase_commander() -> Self {
        FormatConfig {
            format: GameFormat::PlanechaseCommander,
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
            sideboard_policy: GameFormat::PlanechaseCommander.sideboard_policy(),
            default_deck_copy_limit: GameFormat::PlanechaseCommander.default_deck_copy_limit(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            horde_ruleset: None,
            custom_rules: None,
        }
    }

    /// CR 904.13 (Archenemy Commander option): one archenemy at 60 life
    /// against a heroes team sharing 60 (CR 904.13b), poison individual at
    /// 10 each (CR 904.13c), scheme deck of at least ten singleton schemes
    /// (CR 904.13d), and Commander deck construction throughout — exactly
    /// 100 singleton (CR 903.5a/903.5b), no sideboard (CR 903.5e), command
    /// zone (CR 903.6), commander tax (CR 903.8), 21 commander damage per
    /// player (CR 903.10a — a hero reaching 21 loses and the team loses
    /// with them per CR 810.8a-via-904.13b). The archenemy plays first
    /// with no first-turn draw skip (CR 904.6 + CR 103.8c).
    pub fn archenemy_commander() -> Self {
        FormatConfig {
            format: GameFormat::ArchenemyCommander,
            starting_life: 60,
            min_players: 2,
            max_players: 6,
            deck_size: DeckSizeRule::Exactly(100),
            singleton: true,
            command_zone: true,
            commander_damage_threshold: Some(21),
            range_of_influence: None,
            team_based: false,
            archenemy_player: Some(PlayerId(0)),
            uses_commander: true,
            sideboard_policy: GameFormat::ArchenemyCommander.sideboard_policy(),
            default_deck_copy_limit: GameFormat::ArchenemyCommander.default_deck_copy_limit(),
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            horde_ruleset: None,
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
            horde_ruleset: None,
            custom_rules: None,
        }
    }

    /// Horde Magic (casual community variant, not CR-sanctioned). A team of
    /// survivors shares a life total and faces the self-piloting Horde seat,
    /// which occupies the `archenemy_player` seat and reuses the one-vs-many
    /// shared-turn topology. `command_zone: true` so the game-start Horde emblem
    /// (forced attackers + haste on Horde creatures) has a home, mirroring how
    /// Archenemy/Momir install their game-start command-zone objects.
    ///
    /// `starting_life` is the survivors' *combined* starting total (distributed
    /// across survivor seats at game start); the Horde itself has no life total.
    /// `supplies_fixed_deck` is `false` — only the Horde seat's library is
    /// engine-supplied (injected like the Archenemy scheme deck); survivors
    /// build and select their own decks.
    pub fn horde(ruleset: HordeRuleset) -> Self {
        // Survivor deck constraints are a per-deck axis (community decks want EDH,
        // Theros-block decks want 60-card constructed). `command_zone` stays true
        // regardless — the Horde's own game-start emblem lives in the command zone,
        // independent of whether survivors run commanders.
        let (
            deck_size,
            singleton,
            uses_commander,
            commander_damage_threshold,
            default_deck_copy_limit,
        ) = match ruleset.survivor_deck_format {
            SurvivorDeckFormat::Constructed => (
                DeckSizeRule::Minimum(60),
                false,
                false,
                None,
                DeckCopyLimit::Unlimited,
            ),
            SurvivorDeckFormat::Commander => (
                DeckSizeRule::Exactly(100),
                true,
                true,
                Some(21),
                DeckCopyLimit::UpTo(1),
            ),
        };
        FormatConfig {
            format: GameFormat::Horde,
            // Fallback only. The authoritative survivors' combined base life is
            // `HordeRuleset::combined_base_life` (per-deck: community 100, Theros
            // 20); `starting_life_for_player` and `GameState::new` read the
            // ruleset and only fall back to this if a Horde config were built
            // without one. Kept at the community base so that fallback is sane.
            starting_life: 100,
            // 1 survivor + the Horde seat, up to 4 survivors + the Horde.
            min_players: 2,
            max_players: 5,
            deck_size,
            singleton,
            command_zone: true,
            commander_damage_threshold,
            range_of_influence: None,
            team_based: false,
            // The Horde occupies seat 0; survivors take the remaining seats and
            // act first (see `starting_player`).
            archenemy_player: Some(PlayerId(0)),
            uses_commander,
            sideboard_policy: GameFormat::Horde.sideboard_policy(),
            default_deck_copy_limit,
            supplies_fixed_deck: false,
            allow_debug_actions: false,
            horde_ruleset: Some(ruleset),
            custom_rules: None,
        }
    }

    /// Whether this RESOLVED config's command zone is filled from the
    /// decklist's `commander` slot — the netting/placement axis described on
    /// [`GameFormat::command_zone_holds_decklist_commander`].
    ///
    /// This is the method consumers should call. The bare `GameFormat`
    /// predicate cannot answer for `GameFormat::Custom`, because a bare format
    /// carries no `CustomFormatRules`; this one resolves Custom from the rules
    /// the config was actually built with.
    ///
    /// Custom is answered from `CommandZoneMode`, NOT from `uses_commander`.
    /// `for_custom_rules` derives `uses_commander` from the declared
    /// `commander_damage_threshold` alone, so a custom format shaped like
    /// Oathbreaker or Tiny Leaders — `CommandZoneMode::Enabled` with a
    /// `CommanderEligibilityRule` and NO damage threshold — has
    /// `uses_commander: false` while still designating a decklist commander.
    /// Falling back to `uses_commander` would strand that commander in the
    /// library exactly the way the built-in formats did before this predicate
    /// existed. Every `CommandZoneMode::Enabled` carries an
    /// `eligibility_rule`, so an enabled custom command zone always designates
    /// a decklist commander; `Disabled` never does.
    pub fn command_zone_holds_decklist_commander(&self) -> bool {
        match self.format {
            // Horde is per-config, not per-format: Commander-format survivors
            // seat a decklist commander (netted from the library and placed
            // at load); Constructed survivors don't. The enum-level axis
            // below stays `false` — it cannot see the ruleset.
            GameFormat::Horde => self.horde_ruleset.as_ref().is_some_and(|ruleset| {
                ruleset.survivor_deck_format == SurvivorDeckFormat::Commander
            }),
            GameFormat::Custom(_) => match self.custom_rules.as_deref() {
                Some(rules) => matches!(
                    rules.structural.command_zone_mode,
                    CommandZoneMode::Enabled { .. }
                ),
                // A Custom config with no attached rules cannot be resolved;
                // `command_zone` is the closest structural fact it still
                // carries, and it is what `for_custom_rules` would have set.
                None => self.command_zone,
            },
            format => format
                .command_zone_holds_decklist_commander()
                .expect("non-Custom formats always resolve"),
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
            GameFormat::TwoHeadedGiantCommander => Self::two_headed_giant_commander(),
            GameFormat::Archenemy => Self::archenemy(),
            GameFormat::ArchenemyCommander => Self::archenemy_commander(),
            GameFormat::Planechase => Self::planechase(),
            GameFormat::PlanechaseCommander => Self::planechase_commander(),
            GameFormat::Momir => Self::momir(),
            GameFormat::Horde => Self::horde(ChallengeDeck::CybermanHorde.default_ruleset()),
            GameFormat::CommanderDraft => Self::commander_draft(),
            GameFormat::Freeform => Self::freeform(),
            GameFormat::FreeformCommander => Self::freeform_commander(),
            GameFormat::Dandan => Self::dandan(),
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
            horde_ruleset: None,
            custom_rules: Some(Box::new(rules.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            host_choice_count > 0,
            "at least one format must delegate — the building block must not go dead"
        );
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
            horde_ruleset: _,
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

    /// `default_for_player_count` is the single shared authority both
    /// ingresses (`server_core::session::SessionManager::
    /// create_game_n_players` and the WASM `initialize_game_impl`) call to
    /// pick an undeclared config's default. Pin its two-format split and,
    /// separately, that every seat count it can be asked about lands on a
    /// format whose own registry range actually admits that seat count —
    /// the exact property whose absence caused the regression this
    /// function fixes (a 4-player "Standard" default).
    #[test]
    fn default_for_player_count_picks_standard_at_or_below_two_seats() {
        assert_eq!(
            FormatConfig::default_for_player_count(1).format,
            GameFormat::Standard
        );
        assert_eq!(
            FormatConfig::default_for_player_count(2).format,
            GameFormat::Standard
        );
    }

    #[test]
    fn default_for_player_count_picks_free_for_all_above_two_seats() {
        assert_eq!(
            FormatConfig::default_for_player_count(3).format,
            GameFormat::FreeForAll
        );
        assert_eq!(
            FormatConfig::default_for_player_count(6).format,
            GameFormat::FreeForAll
        );
    }

    #[test]
    fn default_for_player_count_is_always_valid_for_the_count_it_was_chosen_for() {
        for n in 2..=6u8 {
            let config = FormatConfig::default_for_player_count(n);
            assert!(
                config.validate_for_player_count(n).is_ok(),
                "default_for_player_count({n}) chose {:?} ({}..={}), which must admit {n} seats",
                config.format,
                config.min_players,
                config.max_players,
            );
        }
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
    fn starting_life_total_for_player_follows_topology() {
        let standard = FormatConfig::standard();
        assert_eq!(standard.starting_life_total_for_player(PlayerId(0), 2), 20);

        let two_headed_giant = FormatConfig::two_headed_giant();
        assert_eq!(
            two_headed_giant.starting_life_total_for_player(PlayerId(0), 4),
            30,
            "FixedTeams uses the shared team starting total, not the per-seat half"
        );

        let mut archenemy = FormatConfig::archenemy();
        archenemy.archenemy_player = Some(PlayerId(2));
        assert_eq!(
            archenemy.starting_life_total_for_player(PlayerId(2), 4),
            40,
            "OneVsMany uses the selected archenemy's rules total"
        );
        assert_eq!(
            archenemy.starting_life_total_for_player(PlayerId(0), 4),
            20,
            "OneVsMany uses a hero's rules total rather than the archenemy's"
        );
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

    /// A two-Horde deck whose co-commander seat is past the table edge must be
    /// rejected, not silently truncated: Two Towers anchored at seat 1 (the
    /// transport remap) with only 2 seats would otherwise drop Saruman (seat
    /// 2) and deal a single incoherent Horde.
    #[test]
    fn validate_for_player_count_rejects_truncated_horde_roster() {
        let mut config = FormatConfig::horde(ChallengeDeck::LotrTwoTowersHorde.default_ruleset());
        config.archenemy_player = Some(PlayerId(1));
        let err = config
            .validate_for_player_count(2)
            .expect_err("seat 2 is past a 2-player table edge");
        assert!(
            err.contains("past the table edge"),
            "unexpected rejection: {err}"
        );
    }

    /// A Horde game with no survivors is incoherent: Two Towers anchored at
    /// seat 0 with 2 seats designates both seats as Horde and must be
    /// rejected rather than starting survivor-less.
    #[test]
    fn validate_for_player_count_rejects_horde_roster_without_survivors() {
        let config = FormatConfig::horde(ChallengeDeck::LotrTwoTowersHorde.default_ruleset());
        assert_eq!(config.archenemy_player, Some(PlayerId(0)));
        let err = config
            .validate_for_player_count(2)
            .expect_err("both seats are Horde, leaving no survivors");
        assert!(err.contains("no survivors"), "unexpected rejection: {err}");
    }

    /// The same Two Towers config is admitted once the table fits the roster:
    /// 3 seats with the seat-1 anchor hold both commanders plus a survivor,
    /// and a single-Horde deck keeps its existing 2-seat admission.
    #[test]
    fn validate_for_player_count_admits_complete_horde_rosters() {
        let mut two_towers =
            FormatConfig::horde(ChallengeDeck::LotrTwoTowersHorde.default_ruleset());
        two_towers.archenemy_player = Some(PlayerId(1));
        assert!(two_towers.validate_for_player_count(3).is_ok());

        let mut cyberman = FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset());
        cyberman.archenemy_player = Some(PlayerId(1));
        assert!(cyberman.validate_for_player_count(2).is_ok());
        cyberman.archenemy_player = Some(PlayerId(0));
        assert!(cyberman.validate_for_player_count(2).is_ok());
    }

    /// Horde survivors act first: for EVERY challenge deck, under both the
    /// engine-default anchor (seat 0) and the transport remap (seat 1), the
    /// starting seat is the lowest seat outside the COMPLETE Horde roster —
    /// never a commander. A primary-only check starts Two Towers on Saruman.
    #[test]
    fn horde_starting_player_is_first_seat_outside_full_roster() {
        for deck in ChallengeDeck::ALL {
            for anchor in [PlayerId(0), PlayerId(1)] {
                let mut config = FormatConfig::horde(deck.default_ruleset());
                config.archenemy_player = Some(anchor);
                let roster = config.horde_designated_seats();
                let start = config.starting_player();
                assert!(
                    !roster.contains(&start),
                    "{deck:?} anchored at {anchor:?} must not start on a Horde seat \
                     (roster {roster:?}, start {start:?})"
                );
                let expected = (0..=u8::MAX)
                    .map(PlayerId)
                    .find(|seat| !roster.contains(seat))
                    .unwrap();
                assert_eq!(
                    start, expected,
                    "{deck:?} anchored at {anchor:?} must start on the lowest survivor seat"
                );
            }
        }
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

    /// The Custom counterpart of `starting_life_admission_gate_pins_both_
    /// bounds`: review-impl round 5 found that the `Some(rules)` arm of
    /// `FormatConfig::deserialize` settled by blanket equality against
    /// `FormatConfig::for_custom_rules` alone, which copies
    /// `structural.starting_life` through unchanged with no magnitude bound
    /// of its own — so a self-consistent Custom payload could declare
    /// `starting_life: 0` or `i32::MAX` and be admitted. Pins that
    /// `validate_starting_life_bounds` is now called from that arm too, via
    /// a payload built entirely from `FormatConfig::for_custom_rules` (so
    /// the blanket-equality check trivially passes and only the bounds check
    /// can fail it).
    #[test]
    fn starting_life_admission_gate_pins_both_bounds_for_custom_format() {
        let def = crate::types::custom_format::CustomFormatDef::from_lobby_config(
            "Test Custom Format".to_string(),
            &FormatConfig::standard(),
        )
        .expect("Standard has no unrepresentable auxiliary deck component");
        let rules = def.rules;

        let build = |starting_life: i32| {
            let mut rules = rules.clone();
            rules.structural.starting_life = starting_life;
            FormatConfig::for_custom_rules(&rules)
        };

        let floor_json = serde_json::to_value(build(0)).unwrap();
        let floor_err = serde_json::from_value::<FormatConfig>(floor_json)
            .expect_err("0 starting life loses every seat at the first SBA check");
        assert!(floor_err.to_string().contains("must begin above 0"));

        let at_ceiling_json = serde_json::to_value(build(MAX_STARTING_LIFE)).unwrap();
        assert!(
            serde_json::from_value::<FormatConfig>(at_ceiling_json).is_ok(),
            "MAX_STARTING_LIFE itself must remain admissible for a Custom format"
        );

        let over_ceiling_json = serde_json::to_value(build(MAX_STARTING_LIFE + 1)).unwrap();
        let over_ceiling_err = serde_json::from_value::<FormatConfig>(over_ceiling_json)
            .expect_err("MAX_STARTING_LIFE + 1 must be rejected for a Custom format");
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

    /// The digital-only partition. `LegalityFormat::ALL` is a hand-written array;
    /// what forces a new variant to be classified is `admits_digital_only_cards`'s
    /// wildcard-free match. A game format outside that table enforces no built-in
    /// pool and is therefore unrestricted.
    #[test]
    fn digital_only_admission_partitions_every_legality_format() {
        let admitting: Vec<LegalityFormat> = LegalityFormat::ALL
            .into_iter()
            .filter(|f| f.admits_digital_only_cards())
            .collect();
        assert_eq!(
            admitting,
            vec![
                LegalityFormat::Historic,
                LegalityFormat::Brawl,
                LegalityFormat::Timeless
            ],
            "only the Arena pools admit digital-only cards"
        );

        assert!(GameFormat::Historic.admits_digital_only_cards());
        assert!(GameFormat::Timeless.admits_digital_only_cards());
        assert!(GameFormat::HistoricBrawl.admits_digital_only_cards());
        assert!(!GameFormat::Standard.admits_digital_only_cards());
        assert!(!GameFormat::Commander.admits_digital_only_cards());
        // `GameFormat::Brawl` maps to the Standard-legal pool.
        assert!(!GameFormat::Brawl.admits_digital_only_cards());

        // No enforced pool ⇒ open, including `Oathbreaker`, whose `GameFormat`
        // maps to `None` even though a `LegalityFormat::Oathbreaker` exists.
        for format in [
            GameFormat::FreeForAll,
            GameFormat::Oathbreaker,
            GameFormat::Limited,
            GameFormat::Momir,
            GameFormat::Custom(CustomFormatId(0)),
        ] {
            assert_eq!(format.legality_format(), None);
            assert!(
                format.admits_digital_only_cards(),
                "{format:?} enforces no built-in pool, so it must stay open"
            );
        }
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
    fn freeform_commander_grants_free_first_mulligan() {
        assert!(GameFormat::FreeformCommander.grants_free_first_mulligan());
    }

    #[test]
    fn freeform_no_free_first_mulligan() {
        assert!(!GameFormat::Freeform.grants_free_first_mulligan());
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

    /// `command_zone_holds_decklist_commander` is a strictly weaker condition
    /// than `uses_commander` (commander damage implies a decklist commander,
    /// never the reverse) and strictly stronger than `command_zone` (a zone
    /// can hold schemes or an emblem instead). Pinning both directions is what
    /// keeps a future variant from being added to only one of the three.
    #[test]
    fn command_zone_holds_decklist_commander_sits_between_uses_commander_and_command_zone() {
        for meta in GameFormat::registry() {
            let holds = meta.format.command_zone_holds_decklist_commander().unwrap();
            let config = &meta.default_config;

            if config.uses_commander {
                assert!(
                    holds,
                    "{:?}: commander damage (CR 903.10a) requires a decklist commander",
                    meta.format
                );
            }
            if holds {
                assert!(
                    config.command_zone,
                    "{:?}: a decklist commander needs a command zone to sit in",
                    meta.format
                );
            }
        }
    }

    /// CR 903.5a: the formats that place a commander from the decklist are
    /// exactly the formats that net that copy out of the main deck. This is the
    /// list `game::deck_loading` gates BOTH netting and command-zone placement
    /// on, and the list `game::deck_validation` passes
    /// `CommandZoneNetting::NetAgainstMainDeck` for. Spelled out literally so
    /// adding a variant to one side without the other fails here rather than
    /// silently starting a game one card short or one card long.
    /// A custom format shaped like Oathbreaker or Tiny Leaders — an enabled
    /// command zone with an eligibility rule but NO commander-damage threshold
    /// — still designates a decklist commander. `for_custom_rules` derives
    /// `uses_commander` from the threshold alone, so reading that field would
    /// answer `false` here and strand the commander in the library, which is
    /// the same class of bug this predicate exists to prevent for the built-in
    /// formats. Resolve Custom from `CommandZoneMode` instead.
    #[test]
    fn custom_command_zone_without_a_damage_threshold_still_holds_a_decklist_commander() {
        use crate::types::custom_format::{
            CommanderEligibilityRule, CustomFormatId, CustomFormatRules, LegacyRuleSet,
            LegalityRules, StructuralRules,
        };

        let build = |command_zone_mode| {
            let rules = CustomFormatRules {
                id: CustomFormatId(4242),
                structural: StructuralRules {
                    starting_life: 20,
                    min_players: 2,
                    max_players: 4,
                    deck_size: DeckSizeRule::Exactly(60),
                    singleton: true,
                    command_zone_mode,
                    range_of_influence: None,
                    team_based: false,
                    sideboard_policy: SideboardPolicy::Forbidden,
                    default_deck_copy_limit: DeckCopyLimit::UpTo(1),
                },
                legality: LegalityRules {
                    legal_sets: None,
                    legal_cards: Vec::new(),
                    banned: Vec::new(),
                    restricted: Vec::new(),
                    legacy: LegacyRuleSet::default(),
                },
            };
            FormatConfig::for_custom_rules(&rules)
        };

        let oathbreaker_shaped = build(CommandZoneMode::Enabled {
            commander_damage_threshold: None,
            eligibility_rule: CommanderEligibilityRule::OathbreakerSignatureSpell,
        });
        // PREMISE: this is genuinely the threshold-less shape, so the assertion
        // below is about the predicate rather than a trivially true case.
        assert!(!oathbreaker_shaped.uses_commander);
        assert!(oathbreaker_shaped.command_zone);
        assert!(
            oathbreaker_shaped.command_zone_holds_decklist_commander(),
            "an enabled custom command zone designates a decklist commander              regardless of the commander-damage threshold"
        );

        // Control: a threshold-bearing custom command zone answers the same,
        // so the predicate is not merely inverting `uses_commander`.
        let commander_shaped = build(CommandZoneMode::Enabled {
            commander_damage_threshold: Some(21),
            eligibility_rule: CommanderEligibilityRule::Standard,
        });
        assert!(commander_shaped.uses_commander);
        assert!(commander_shaped.command_zone_holds_decklist_commander());

        // Control: a disabled custom command zone holds nothing, so nothing may
        // be netted or placed.
        let no_zone = build(CommandZoneMode::Disabled);
        assert!(!no_zone.command_zone);
        assert!(!no_zone.command_zone_holds_decklist_commander());
    }

    /// CR 903.5a: the formats that place a commander from the decklist are
    /// exactly the formats that net that copy out of the main deck.
    /// `game::deck_loading` gates BOTH netting and command-zone placement on
    /// this predicate, and `game::deck_validation` passes
    /// `CommandZoneNetting::NetAgainstMainDeck` for the same set. A format
    /// netted there but not placed here starts the game a card short; one
    /// placed but not netted starts it a card long.
    ///
    /// This SCANS `deck_validation.rs` rather than asserting a hardcoded list.
    /// A hardcoded array cannot fail when a call site changes — flipping
    /// `evaluate_tiny_leaders` to `CountVerbatim` would leave a list-based
    /// test green while the loader and validator silently disagreed, which is
    /// exactly the defect this predicate exists to prevent. Scanning the
    /// source is the same technique `phase-ai`'s `score_contract_lint` uses to
    /// pin a cross-file contract.
    #[test]
    fn command_zone_holds_decklist_commander_matches_the_validator_netting_set() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/game/deck_validation.rs"
        ))
        .expect("deck_validation.rs must be readable");

        // Production code only: `#[cfg(test)]` fixtures call
        // `combined_copy_counts` too, and those calls sit after the last
        // `evaluate_*` function, so including them would attribute a netting
        // mode to whatever helper happened to be declared last.
        const TEST_MODULE_MARKER: &str = "\n#[cfg(test)]\n";
        let production = source
            .split_once(TEST_MODULE_MARKER)
            .map_or(source.as_str(), |(before, _)| before);

        // Map each `evaluate_*`/`quick_*` function to the netting mode its
        // `combined_copy_counts` call passes, by walking the file and
        // remembering the most recent `fn` header.
        let mut current_fn: Option<&str> = None;
        let mut netted: Vec<&str> = Vec::new();
        let mut verbatim: Vec<&str> = Vec::new();
        for line in production.lines() {
            // Accept any visibility/qualifier prefix: `pub fn evaluate_…` and
            // `pub(crate) fn …` are real headers in this file, and a bare
            // `strip_prefix("fn ")` would skip them, silently attributing
            // their call sites to whichever function was declared before.
            if let Some(header) = line
                .split_once("fn ")
                .filter(|(before, _)| {
                    before.is_empty()
                        || before.split_whitespace().all(|w| {
                            matches!(w, "pub" | "async" | "const" | "unsafe" | "extern")
                                || w.starts_with("pub(")
                        })
                })
                .map(|(_, rest)| rest)
            {
                current_fn = header.split('(').next();
            }
            let Some(name) = current_fn else { continue };
            if line.contains("CommandZoneNetting::NetAgainstMainDeck")
                && line.contains("combined_copy_counts(")
            {
                netted.push(name);
            } else if line.contains("CommandZoneNetting::CountVerbatim")
                && line.contains("combined_copy_counts(")
            {
                verbatim.push(name);
            }
        }

        // PREMISE: the scan actually found call sites. Without this the whole
        // test passes vacuously if the call shape is ever reformatted.
        assert!(
            netted.len() >= 5,
            "expected to find the NetAgainstMainDeck call sites, found {netted:?} — \
             has `combined_copy_counts` been reformatted? Update the scan."
        );
        assert!(
            verbatim.len() >= 3,
            "expected to find the CountVerbatim call sites, found {verbatim:?}"
        );

        // Every format whose validator nets must answer `true` here.
        for fn_name in &netted {
            let formats = formats_evaluated_by(fn_name);
            assert!(
                !formats.is_empty(),
                "{fn_name} nets against the main deck but maps to no GameFormat — \
                 add it to `formats_evaluated_by` so this invariant keeps covering it"
            );
            for format in formats {
                assert!(
                    format.command_zone_holds_decklist_commander().unwrap(),
                    "{fn_name} passes NetAgainstMainDeck for {format:?}, so \
                     command_zone_holds_decklist_commander must be true — otherwise \
                     deck_loading leaves that commander in the library while the \
                     validator has already netted it out"
                );
            }
        }

        // And every format counted verbatim must answer `false`, or netting
        // would delete a library card from a format with no decklist commander.
        for fn_name in &verbatim {
            for format in formats_evaluated_by(fn_name) {
                assert!(
                    !format.command_zone_holds_decklist_commander().unwrap(),
                    "{fn_name} counts {format:?} verbatim, so \
                     command_zone_holds_decklist_commander must be false"
                );
            }
        }
    }

    /// The `GameFormat`s each `deck_validation` entry point evaluates. Kept
    /// beside the scan above so a new validator function fails loudly (empty
    /// mapping) rather than being silently skipped.
    fn formats_evaluated_by(fn_name: &str) -> Vec<GameFormat> {
        match fn_name {
            "evaluate_commander_with_format" | "quick_commander_check" => vec![
                GameFormat::Commander,
                GameFormat::DuelCommander,
                GameFormat::PauperCommander,
                GameFormat::CommanderDraft,
                GameFormat::TwoHeadedGiantCommander,
                GameFormat::ArchenemyCommander,
                GameFormat::PlanechaseCommander,
            ],
            "evaluate_brawl" => vec![GameFormat::Brawl, GameFormat::HistoricBrawl],
            "evaluate_tiny_leaders" => vec![GameFormat::TinyLeaders],
            "evaluate_oathbreaker" => vec![GameFormat::Oathbreaker],
            "evaluate_archenemy" => vec![GameFormat::Archenemy],
            "evaluate_planechase" => vec![GameFormat::Planechase],
            "evaluate_constructed" => vec![
                GameFormat::Standard,
                GameFormat::Modern,
                GameFormat::Legacy,
                GameFormat::Vintage,
                GameFormat::Pauper,
            ],
            _ => Vec::new(),
        }
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

    /// `GameFormat::registry()` must carry an entry for every built-in format,
    /// exactly once, with no entry the enum does not carry.
    ///
    /// Compared as SORTED sets, deliberately: `registry()`'s row order is a
    /// separate contract, pinned by
    /// `client_format_registry_matches_the_engine_registry` (mutation-tested:
    /// swapping two `registry()` rows reds that test, not this one).
    /// `premodern_registry_entry_is_ordered_with_constructed_formats`
    /// additionally pins the Modern→Premodern→Legacy run specifically. This
    /// test must not duplicate or contradict either. Sorted-vector equality
    /// still catches a duplicate, because a duplicate changes the length.
    ///
    /// Also the independent authority on `#[strum(disabled)]`: a built-in
    /// disabled out of `iter()` while `registry()` still lists it unbalances
    /// the two sorted sets and reds here (mutation-tested alongside the
    /// enum's own doc comment above).
    #[test]
    fn registry_lists_every_builtin_format() {
        use strum::IntoEnumIterator;

        let mut listed: Vec<String> = GameFormat::registry()
            .iter()
            .map(|meta| meta.format.to_string())
            .collect();
        let mut expected: Vec<String> = GameFormat::iter().map(|f| f.to_string()).collect();
        listed.sort();
        expected.sort();
        assert_eq!(
            listed, expected,
            "GameFormat::registry() must list every built-in format exactly once"
        );
    }

    /// Built-in format names carried on the CODE half of each line in `region`
    /// that begins with `prefix`, taking the first double-quoted token.
    ///
    /// `source_census::code` is this repository's single authority on which
    /// part of a line is code; TypeScript's `//` is lexically the same as
    /// Rust's for that purpose, so it is reused per `source_census`'s
    /// single-authority rule rather than re-invented. Mutation-tested:
    /// bypassing both call sites here leaves both callers below green
    /// against the current file contents — no line depends on it today.
    ///
    /// `region` is a parameter because `types.ts` carries `| "…"` lines
    /// belonging to unions other than `BuiltInGameFormat`, so that union's own
    /// region must be isolated; `formatRegistry.ts` needs no region because
    /// every `format: "…"` line in it is an entry field.
    fn client_format_names(region: &str, prefix: &str) -> Vec<String> {
        region
            .lines()
            .map(crate::source_census::code)
            .filter_map(|line| line.trim().strip_prefix(prefix))
            .filter_map(|rest| rest.split('"').nth(1).map(str::to_owned))
            .collect()
    }

    /// `client/src/adapter/types.ts`'s `BuiltInGameFormat` union must name
    /// exactly the engine's built-in formats.
    ///
    /// This runs in the lane — CI job `rust-test`
    /// step "Run tests", Tilt `test-engine` — and needs no WASM artifact. The
    /// client-side assertion that binds the mirror through the real WASM export
    /// lives in `client/src/data/__tests__/formatRegistry.integration.test.ts`,
    /// which `client/vitest.config.ts` excludes from the default run and which
    /// no workflow, Tiltfile target or verify.sh path invokes. An assertion no
    /// lane executes guards nothing; this one does.
    ///
    /// `include_str!` rather than `read_to_string`: a moved or deleted client
    /// file is then a COMPILE error, not a silently skipped test. Same idiom as
    /// `tests/integration/interaction_contract.rs`.
    ///
    /// SORTED, deliberately: measured, the union's declaration order is neither
    /// the engine's declaration order nor `registry()`'s, and pinning an order
    /// nothing owns would be a test of formatting.
    ///
    /// No separate non-vacuity premise is needed. The comparison is against a
    /// non-empty right side, so an extraction that finds nothing FAILS rather
    /// than passing.
    #[test]
    fn client_builtin_game_format_union_matches_the_engine() {
        use strum::IntoEnumIterator;

        const TYPES_TS: &str = include_str!("../../../../client/src/adapter/types.ts");

        // The union's own span: from its declaration header to the first
        // subsequent line whose code half ends in `;`.
        let mut lines = TYPES_TS
            .lines()
            .skip_while(|line| !line.starts_with("export type BuiltInGameFormat ="));
        let _header = lines
            .next()
            .expect("client/src/adapter/types.ts must declare `export type BuiltInGameFormat =`");
        let mut region = String::new();
        for line in lines {
            region.push_str(line);
            region.push('\n');
            if crate::source_census::code(line).trim_end().ends_with(';') {
                break;
            }
        }

        let mut found = client_format_names(&region, "| ");
        let mut expected: Vec<String> = GameFormat::iter().map(|f| f.to_string()).collect();
        found.sort();
        expected.sort();
        assert_eq!(
            found, expected,
            "client/src/adapter/types.ts's BuiltInGameFormat union must name \
             exactly the engine's built-in formats"
        );
    }

    /// `client/src/data/formatRegistry.ts`'s `FORMAT_REGISTRY` must mirror
    /// `GameFormat::registry()` — the same formats, in the same order, with no
    /// duplicated or half-written entry.
    ///
    /// Each entry writes `format: "X"` TWICE (the entry's own key and its
    /// `default_config.format`), so the entry SEQUENCE is recovered by
    /// collapsing CONSECUTIVE duplicates rather than by matching an
    /// indentation the next reformat would move.
    ///
    /// The collapse alone is NOT enough, and this was measured rather than
    /// reasoned. An adjacent duplicated entry and an entry missing one of its
    /// two `format:` lines both still produce the exact expected sequence with
    /// only the collapse; the multiplicity check below is what reds those two.
    /// An entry whose `default_config.format` disagrees with its own key is
    /// caught by the sequence check itself in the general case; it only
    /// survives the sequence check when the disagreeing value happens to
    /// equal the following entry's own name (the collapse then swallows the
    /// run into that name), and the multiplicity check is what reds that
    /// narrower case. The multiplicity check requires every entry to write
    /// `format:` the same number of times, rather than hardcoding an
    /// assertion that it equals two, so a uniform change to how many times
    /// every entry writes `format:` continues to pass.
    #[test]
    fn client_format_registry_matches_the_engine_registry() {
        const REGISTRY_TS: &str = include_str!("../../../../client/src/data/formatRegistry.ts");

        let raw = client_format_names(REGISTRY_TS, "format: ");

        let mut entries: Vec<String> = Vec::new();
        for name in &raw {
            if entries.last() != Some(name) {
                entries.push(name.clone());
            }
        }
        let expected: Vec<String> = GameFormat::registry()
            .iter()
            .map(|meta| meta.format.to_string())
            .collect();
        assert_eq!(
            entries, expected,
            "client/src/data/formatRegistry.ts must mirror GameFormat::registry(), \
             in the same order"
        );

        let mut counts: Vec<(String, usize)> = expected
            .iter()
            .map(|name| (name.clone(), raw.iter().filter(|n| *n == name).count()))
            .collect();
        counts.sort_by_key(|(_, n)| *n);
        let (lowest, highest) = (counts.first().unwrap(), counts.last().unwrap());
        assert_eq!(
            lowest.1, highest.1,
            "every FORMAT_REGISTRY entry must write `format:` the same number of \
             times; {} writes it {} and {} writes it {}. A duplicated entry, a \
             half-written entry, or a default_config.format that disagrees with \
             its own key all look like this — and all of them survive the \
             sequence check above",
            lowest.0, lowest.1, highest.0, highest.1
        );
    }

    #[test]
    fn recorded_legality_table_agrees_with_every_table_backed_card_pool() {
        use strum::IntoEnumIterator;

        let mut saw_table_backed = false;
        let mut saw_unrestricted = false;
        for format in GameFormat::iter() {
            match format.card_pool() {
                CardPool::LegalityTable(t) => {
                    saw_table_backed = true;
                    assert_eq!(format.recorded_legality_table(), Some(t));
                }
                CardPool::Unrestricted => {
                    saw_unrestricted = true;
                    assert_eq!(format.recorded_legality_table(), None);
                }
                CardPool::NoEngineAuthority | CardPool::DeclaredRules => {}
            }
        }
        assert!(saw_table_backed, "the set must not be empty");
        assert!(saw_unrestricted, "the set must not be empty");
    }

    #[test]
    fn every_legality_table_records_exactly_one_registry_format() {
        let registry = GameFormat::registry();
        for t in LegalityFormat::ALL {
            let format = legality_table_format(t);
            assert!(registry.iter().any(|meta| meta.format == format));
            assert_eq!(format.recorded_legality_table(), Some(t));
        }
    }

    #[test]
    fn registry_entries_publish_their_own_formats_legality_key() {
        for meta in GameFormat::registry() {
            assert_eq!(
                meta.legality_key,
                meta.format.legality_key(),
                "{:?}",
                meta.format
            );
        }
    }

    /// `client/src/data/formatRegistry.ts`'s `legality_key` lines must equal
    /// the engine's, entry by entry. The client names its deck-browser
    /// legality lens from them.
    #[test]
    fn client_format_registry_legality_keys_match_the_engine() {
        const REGISTRY_TS: &str = include_str!("../../../../client/src/data/formatRegistry.ts");

        let found: Vec<Option<String>> = REGISTRY_TS
            .lines()
            .map(crate::source_census::code)
            .filter_map(|line| {
                let trimmed = line.trim();
                trimmed.strip_prefix("legality_key: ")
            })
            .map(|rest| {
                let rest = rest.trim().strip_suffix(',').unwrap_or(rest.trim());
                if rest == "null" {
                    None
                } else if let Some(token) = rest.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                {
                    Some(token.to_owned())
                } else {
                    panic!("unrecognized legality_key line: {rest:?}");
                }
            })
            .collect();

        let expected: Vec<Option<String>> = GameFormat::registry()
            .iter()
            .map(|meta| meta.legality_key.map(str::to_owned))
            .collect();

        assert!(!expected.is_empty());
        assert_eq!(
            found, expected,
            "client/src/data/formatRegistry.ts's legality_key lines must equal \
             the engine's, entry by entry"
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
    fn format_config_horde() {
        let config = FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset());
        assert_eq!(config.format, GameFormat::Horde);
        // `starting_life` is now only the fallback base; the authoritative
        // survivor combined base lives on the ruleset (community rules: 100).
        assert_eq!(config.starting_life, 100);
        assert_eq!(
            config.horde_ruleset.as_ref().unwrap().combined_base_life,
            100
        );
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 5);
        assert!(config.command_zone);
        assert!(!config.team_based);
        assert!(!config.uses_commander);
        // Survivors build their own decks; only the Horde seat is engine-supplied.
        assert!(!config.supplies_fixed_deck);
        assert_eq!(config.archenemy_player(), Some(PlayerId(0)));
        let ruleset = config
            .horde_ruleset
            .as_ref()
            .expect("horde config has ruleset");
        assert_eq!(ruleset.challenge_deck, ChallengeDeck::CybermanHorde);
        assert_eq!(ruleset.survivor_setup_turns, 3);
        assert!(ruleset.horde_creatures_forced_attackers);
        // Every shipped deck loads survivors as 60-card constructed today.
        assert_eq!(
            ruleset.survivor_deck_format,
            SurvivorDeckFormat::Constructed
        );
    }

    /// The per-deck survivor-deck-format axis drives `FormatConfig::horde`'s
    /// survivor deck constraints: `Constructed` is 60-card non-singleton with no
    /// commander; `Commander` is 100-card singleton EDH. `command_zone` is present
    /// either way — the Horde's own emblem lives there.
    #[test]
    fn horde_survivor_deck_format_axis_drives_deck_constraints() {
        let mut ruleset = ChallengeDeck::CybermanHorde.default_ruleset();

        ruleset.survivor_deck_format = SurvivorDeckFormat::Constructed;
        let cfg = FormatConfig::horde(ruleset.clone());
        assert_eq!(cfg.deck_size, DeckSizeRule::Minimum(60));
        assert!(!cfg.singleton);
        assert!(!cfg.uses_commander);
        assert_eq!(cfg.commander_damage_threshold, None);
        assert!(cfg.command_zone);

        ruleset.survivor_deck_format = SurvivorDeckFormat::Commander;
        let cfg = FormatConfig::horde(ruleset);
        assert_eq!(cfg.deck_size, DeckSizeRule::Exactly(100));
        assert!(cfg.singleton);
        assert!(cfg.uses_commander);
        assert_eq!(cfg.commander_damage_threshold, Some(21));
        assert!(cfg.command_zone);
    }

    /// `horde_decks()` is the per-seat library list: the primary `challenge_deck`
    /// then each `co_horde_decks` entry in order. Single-Horde decks list one; the
    /// LOTR two-Horde pairing lists [Sauron, Saruman] (two Horde seats).
    #[test]
    fn horde_decks_lists_primary_then_co_hordes() {
        let single = ChallengeDeck::SauronHorde.default_ruleset();
        assert!(single.co_horde_decks.is_empty());
        assert_eq!(single.horde_decks(), vec![ChallengeDeck::SauronHorde]);

        let mut two = ChallengeDeck::SauronHorde.default_ruleset();
        two.co_horde_decks = vec![ChallengeDeck::SarumanHorde];
        assert_eq!(
            two.horde_decks(),
            vec![ChallengeDeck::SauronHorde, ChallengeDeck::SarumanHorde],
            "primary then co-hordes, in seat order"
        );
    }

    #[test]
    fn two_headed_giant_commander_registry_entry_composes_both_parents() {
        let registry = GameFormat::registry();
        let entry = registry
            .iter()
            .find(|m| m.format == GameFormat::TwoHeadedGiantCommander)
            .expect("TwoHeadedGiantCommander must be in registry");
        assert_eq!(entry.label, "Two-Headed Giant Commander");
        assert_eq!(entry.short_label, "2HGC");
        assert_eq!(entry.group, FormatGroup::Commander);
        // Shared Commander card pool — no separate 2HG Commander table exists.
        assert_eq!(entry.legality_key, Some("commander"));
        let config = FormatConfig::two_headed_giant_commander();
        assert_eq!(entry.default_config, config);
        // CR 810.1: two teams of two — exactly four seats.
        assert_eq!(config.min_players, 4);
        assert_eq!(config.max_players, 4);
        // Played consensus (no CR rule): 60 shared life per team.
        assert_eq!(config.starting_life, 60);
        // CR 903.5a / CR 903.5b / CR 903.5e / CR 903.6 / CR 903.10a.
        assert_eq!(config.deck_size, DeckSizeRule::Exactly(100));
        assert!(config.singleton);
        assert_eq!(config.sideboard_policy, SideboardPolicy::Forbidden);
        assert!(config.command_zone);
        assert_eq!(config.commander_damage_threshold, Some(21));
        assert!(config.uses_commander);
        assert!(config.team_based);
        assert_eq!(config.archenemy_player(), None);
    }

    #[test]
    fn two_headed_giant_commander_round_trips_through_string_and_config() {
        use std::str::FromStr;

        let format = GameFormat::from_str("TwoHeadedGiantCommander").unwrap();
        assert_eq!(format, GameFormat::TwoHeadedGiantCommander);
        assert_eq!(format.to_string(), "TwoHeadedGiantCommander");
        assert_eq!(
            FormatConfig::for_format(format).unwrap(),
            FormatConfig::two_headed_giant_commander()
        );
    }

    #[test]
    fn horde_uses_one_vs_many_shared_turn_topology() {
        let config = FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset());
        assert_eq!(
            config.topology(),
            FormatTopology::OneVsMany {
                archenemy: PlayerId(0),
                turn_structure: TurnStructure::SharedTeamTurns,
            }
        );
    }

    #[test]
    fn horde_survivors_take_the_first_turn_not_the_horde() {
        // Regression guard for the review's B1 blocker: because Horde reuses the
        // Archenemy OneVsMany topology, the naive path would hand the Horde
        // (the archenemy seat) the first turn. Survivors must go first.
        let config = FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset());
        let horde_seat = config.archenemy_player().expect("horde seat");
        let first = config.starting_player();
        assert_ne!(first, horde_seat, "the Horde must not take the first turn");
        assert_eq!(first, PlayerId(1));
    }

    #[test]
    fn horde_seat_is_not_given_archenemy_forty_life() {
        // B1 blocker guard: the Archenemy OneVsMany arm grants 40 life to the
        // special seat. The Horde has no life total and must not inherit 40.
        let ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
        let base = ruleset.combined_base_life;
        let config = FormatConfig::horde(ruleset);
        let horde_seat = config.archenemy_player().expect("horde seat");
        // Load-bearing: NOT the archenemy's 40. Every seat is pre-seeded with the
        // combined base (redistributed across survivors in `GameState::new`);
        // derive it from the ruleset rather than hardcoding, so this stays valid
        // as deck balance changes.
        assert_ne!(config.starting_life_for_player(horde_seat, 4), 40);
        assert_eq!(config.starting_life_for_player(horde_seat, 4), base);
        assert_eq!(config.starting_life_for_player(PlayerId(1), 4), base);
    }

    #[test]
    fn horde_registry_entry_uses_default_config() {
        let registry = GameFormat::registry();
        let entry = registry
            .iter()
            .find(|m| m.format == GameFormat::Horde)
            .expect("Horde must be in registry");
        assert_eq!(entry.group, FormatGroup::Multiplayer);
        assert_eq!(entry.short_label, "HRD");
        assert_eq!(
            entry.default_config,
            FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset())
        );
    }

    #[test]
    fn horde_ruleset_survives_config_serde_round_trip() {
        let config = FormatConfig::horde(ChallengeDeck::CybermanHorde.default_ruleset());
        let json = serde_json::to_string(&config).expect("serialize");
        let restored: FormatConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(config, restored);
    }

    #[test]
    fn non_horde_configs_have_no_horde_ruleset() {
        for meta in GameFormat::registry() {
            let has_ruleset = meta.default_config.horde_ruleset.is_some();
            assert_eq!(
                has_ruleset,
                meta.format == GameFormat::Horde,
                "{:?}: horde_ruleset presence must match the Horde format",
                meta.format
            );
        }
    }

    #[test]
    fn two_headed_giant_commander_topology_and_life_are_team_shaped() {
        let config = FormatConfig::two_headed_giant_commander();
        assert_eq!(
            config.topology(),
            FormatTopology::FixedTeams {
                team_size: 2,
                team_count: 2,
                turn_structure: TurnStructure::SharedTeamTurns,
            }
        );
        // 60 shared per team, divided per seat like 60-card 2HG's 30.
        for seat in 0..4 {
            assert_eq!(config.starting_life_for_player(PlayerId(seat), 4), 30);
            assert_eq!(config.starting_life_total_for_player(PlayerId(seat), 4), 60);
        }
        assert!(config.validate_for_player_count(4).is_ok());
        assert!(config.validate_for_player_count(2).is_err());
        assert!(config.validate_for_player_count(6).is_err());
    }

    #[test]
    fn archenemy_commander_registry_entry_composes_archenemy_and_commander() {
        let registry = GameFormat::registry();
        let entry = registry
            .iter()
            .find(|m| m.format == GameFormat::ArchenemyCommander)
            .expect("ArchenemyCommander must be in registry");
        assert_eq!(entry.label, "Archenemy Commander");
        assert_eq!(entry.short_label, "ARCC");
        assert_eq!(entry.group, FormatGroup::Commander);
        // Shared Commander card pool — no separate table exists.
        assert_eq!(entry.legality_key, Some("commander"));
        let config = FormatConfig::archenemy_commander();
        assert_eq!(entry.default_config, config);
        // CR 904.13a: one archenemy plus 1-5 heroes.
        assert_eq!(config.min_players, 2);
        assert_eq!(config.max_players, 6);
        // CR 904.13b: 60 for the archenemy, shared 60 for the heroes.
        assert_eq!(config.starting_life, 60);
        // CR 903.5a / CR 903.5b / CR 903.5e / CR 903.6 / CR 903.10a.
        assert_eq!(config.deck_size, DeckSizeRule::Exactly(100));
        assert!(config.singleton);
        assert_eq!(config.sideboard_policy, SideboardPolicy::Forbidden);
        assert!(config.command_zone);
        assert_eq!(config.commander_damage_threshold, Some(21));
        assert!(config.uses_commander);
        assert_eq!(config.archenemy_player(), Some(PlayerId(0)));
    }

    #[test]
    fn archenemy_commander_round_trips_through_string_and_config() {
        use std::str::FromStr;

        let format = GameFormat::from_str("ArchenemyCommander").unwrap();
        assert_eq!(format, GameFormat::ArchenemyCommander);
        assert_eq!(format.to_string(), "ArchenemyCommander");
        assert_eq!(
            FormatConfig::for_format(format).unwrap(),
            FormatConfig::archenemy_commander()
        );
    }

    #[test]
    fn archenemy_commander_life_splits_heroes_evenly_by_seat_count() {
        let config = FormatConfig::archenemy_commander();
        assert_eq!(
            config.topology(),
            FormatTopology::OneVsMany {
                archenemy: PlayerId(0),
                turn_structure: TurnStructure::SharedTeamTurns,
            }
        );
        // CR 904.13b: the archenemy starts at 60 at every seat count.
        for count in 2..=6usize {
            assert_eq!(config.starting_life_for_player(PlayerId(0), count), 60);
            assert_eq!(
                config.starting_life_total_for_player(PlayerId(0), count),
                60
            );
        }
        // CR 904.13b: heroes split the shared 60 evenly — exact at every
        // legal hero count (1-5), summing back to 60.
        for (count, per_seat) in [(2usize, 60), (3, 30), (4, 20), (5, 15), (6, 12)] {
            for hero in 1..count {
                assert_eq!(
                    config.starting_life_for_player(PlayerId(hero as u8), count),
                    per_seat,
                    "{count} seats: each hero starts at {per_seat}"
                );
                // The rules baseline is the team total, not the split.
                assert_eq!(
                    config.starting_life_total_for_player(PlayerId(hero as u8), count),
                    60
                );
            }
            let seats: i32 = (1..count)
                .map(|hero| config.starting_life_for_player(PlayerId(hero as u8), count))
                .sum();
            assert_eq!(seats, 60, "{count} seats: hero seats sum to 60");
        }
        assert!(config.validate_for_player_count(2).is_ok());
        assert!(config.validate_for_player_count(6).is_ok());
        assert!(config.validate_for_player_count(1).is_err());
        assert!(config.validate_for_player_count(7).is_err());
    }

    #[test]
    fn archenemy_commander_axes_match_commander_deck_rules() {
        let format = GameFormat::ArchenemyCommander;
        assert!(format.is_archenemy_family());
        assert!(GameFormat::Archenemy.is_archenemy_family());
        assert!(!GameFormat::Commander.is_archenemy_family());
        assert!(!GameFormat::TwoHeadedGiantCommander.is_archenemy_family());
        assert_eq!(
            format.card_pool(),
            CardPool::LegalityTable(LegalityFormat::Commander)
        );
        assert_eq!(
            format.recorded_legality_table(),
            Some(LegalityFormat::Commander)
        );
        assert_eq!(format.legality_format(), Some(LegalityFormat::Commander));
        assert_eq!(
            format.commander_pairing(),
            CommanderPairing::PartnerFamilies
        );
        assert_eq!(format.uses_commander(), Ok(true));
        assert_eq!(format.command_zone_holds_decklist_commander(), Ok(true));
        assert_eq!(
            format.deck_size_subject(),
            DeckSizeSubject::MainDeckAndCommanders
        );
        // Commander-style duel grant — reachable only at 2 seats.
        assert!(format.grants_free_first_mulligan());
        assert!(!format.supplies_fixed_deck());
        // Scheme deck loads through the format-gated path (CR 904.13d).
        assert!(format.has_unrepresentable_auxiliary_deck_component());
    }

    #[test]
    fn two_headed_giant_commander_axes_match_commander_deck_rules() {
        let format = GameFormat::TwoHeadedGiantCommander;
        assert!(format.is_two_headed_giant_family());
        assert!(GameFormat::TwoHeadedGiant.is_two_headed_giant_family());
        assert!(!GameFormat::Commander.is_two_headed_giant_family());
        assert!(!GameFormat::Archenemy.is_two_headed_giant_family());
        assert_eq!(
            format.card_pool(),
            CardPool::LegalityTable(LegalityFormat::Commander)
        );
        assert_eq!(
            format.recorded_legality_table(),
            Some(LegalityFormat::Commander)
        );
        assert_eq!(format.legality_format(), Some(LegalityFormat::Commander));
        assert_eq!(
            format.commander_pairing(),
            CommanderPairing::PartnerFamilies
        );
        assert_eq!(format.uses_commander(), Ok(true));
        assert_eq!(format.command_zone_holds_decklist_commander(), Ok(true));
        assert_eq!(
            format.deck_size_subject(),
            DeckSizeSubject::MainDeckAndCommanders
        );
        assert!(!format.grants_free_first_mulligan());
        assert!(!format.supplies_fixed_deck());
        assert!(!format.has_unrepresentable_auxiliary_deck_component());
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
    fn registry_constructed_formats_declare_a_deck_construction_pool() {
        use strum::IntoEnumIterator;

        // Clause 1: a constructed format may decline a table only by
        // positively declaring that its rules restrict nothing; it may never
        // simply lack an authority.
        for meta in GameFormat::registry()
            .into_iter()
            .filter(|meta| meta.group == FormatGroup::Constructed)
        {
            assert!(
                matches!(
                    meta.format.card_pool(),
                    CardPool::LegalityTable(_) | CardPool::Unrestricted
                ),
                "{:?} is constructed but declares no deck-construction pool",
                meta.format
            );
        }

        // Clause 2: no two built-in formats declare the same legality table —
        // with one declared exception. No separate 2HG Commander or
        // Archenemy Commander ban list exists anywhere (WotC, the Commander
        // Panel, and MTGJSON publish only "Commander"), so both share
        // Commander's table rather than carrying permanently-identical
        // duplicates. The reverse lookup this uniqueness protects
        // (`recorded_legality_table`) answers the shared table for both via
        // its own explicit arm. Any future sharing beyond this trio must
        // amend this clause deliberately, not slide in.
        let mut declarers: std::collections::HashMap<LegalityFormat, Vec<GameFormat>> =
            std::collections::HashMap::new();
        for format in GameFormat::iter() {
            if let CardPool::LegalityTable(table) = format.card_pool() {
                declarers.entry(table).or_default().push(format);
            }
        }
        for (table, mut formats) in declarers {
            formats.sort_by_key(|f| f.to_string());
            if table == LegalityFormat::Commander {
                assert_eq!(
                    formats,
                    vec![
                        GameFormat::ArchenemyCommander,
                        GameFormat::Commander,
                        GameFormat::PlanechaseCommander,
                        GameFormat::TwoHeadedGiantCommander
                    ],
                    "only Commander and its composed variants may share the Commander table"
                );
            } else {
                assert_eq!(
                    formats.len(),
                    1,
                    "{table:?} is declared by {formats:?} — tables are not shared"
                );
            }
        }
    }

    #[test]
    fn legality_format_is_the_card_pool_projection() {
        use strum::IntoEnumIterator;

        for format in
            GameFormat::iter().chain(std::iter::once(GameFormat::Custom(CustomFormatId(0))))
        {
            let projected = match format.card_pool() {
                CardPool::LegalityTable(table) => Some(table),
                CardPool::NoEngineAuthority | CardPool::Unrestricted | CardPool::DeclaredRules => {
                    None
                }
            };
            assert_eq!(
                format.legality_format(),
                projected,
                "{format:?}: legality_format() disagrees with the card_pool() projection"
            );
        }
    }

    /// Every group here pins declared behavior, not a validator literal.
    /// Production admission reads `pairing.admits_count(request.commander.len())`
    /// alone at every call site, and
    /// `no_caller_reintroduces_a_commander_count_literal` in
    /// `format_axis_census.rs` asserts zero surviving per-format
    /// commander-count literals. One group per `CommanderPairing`
    /// variant: `partner_families` admits iff count is 1 or 2 (CR 702.124g),
    /// `solo` admits iff count == 1, and `no_commander_per_count` — every
    /// built-in that never designates a commander — admits iff count == 0.
    /// `no_commander_per_count` also gets the cross-check against the
    /// command-zone axis (`command_zone_holds_decklist_commander() ==
    /// Ok(false)`), a genuine second authority on the same claim rather than
    /// a restatement of the per-count curve.
    #[test]
    fn commander_pairing_admits_exactly_todays_counts() {
        use strum::IntoEnumIterator;

        let partner_families = [
            GameFormat::Commander,
            GameFormat::TwoHeadedGiantCommander,
            GameFormat::ArchenemyCommander,
            GameFormat::PlanechaseCommander,
            GameFormat::PauperCommander,
            GameFormat::DuelCommander,
            GameFormat::CommanderDraft,
            GameFormat::TinyLeaders,
            GameFormat::FreeformCommander,
        ];
        let solo = [
            GameFormat::Brawl,
            GameFormat::HistoricBrawl,
            GameFormat::Oathbreaker,
        ];
        let no_commander_per_count = [
            GameFormat::Standard,
            GameFormat::Pioneer,
            GameFormat::Modern,
            GameFormat::Premodern,
            GameFormat::Legacy,
            GameFormat::Vintage,
            GameFormat::Historic,
            GameFormat::Timeless,
            GameFormat::Pauper,
            GameFormat::Planechase,
            GameFormat::Archenemy,
            GameFormat::Momir,
            GameFormat::FreeForAll,
            GameFormat::TwoHeadedGiant,
            GameFormat::Limited,
            GameFormat::Freeform,
            GameFormat::Dandan,
            // Format-level axis only (per-config Commander survivors are the
            // `FormatConfig`-level override's business).
            GameFormat::Horde,
        ];

        for count in 0usize..=3 {
            for format in partner_families {
                // Pinned expected admission per CR 702.124g: iff count is 1 or 2.
                let expected_admits = (1..=2).contains(&count);
                assert_eq!(
                    format.commander_pairing().admits_count(count),
                    expected_admits,
                    "{format:?} at count {count}"
                );
            }
            for format in solo {
                // Pinned expected admission: iff count == 1.
                let expected_admits = count == 1;
                assert_eq!(
                    format.commander_pairing().admits_count(count),
                    expected_admits,
                    "{format:?} at count {count}"
                );
            }
            for format in no_commander_per_count {
                // Pinned expected admission: iff count == 0.
                let expected_admits = count == 0;
                assert_eq!(
                    format.commander_pairing().admits_count(count),
                    expected_admits,
                    "{format:?} at count {count}"
                );
            }
        }

        for format in no_commander_per_count {
            assert_eq!(format.command_zone_holds_decklist_commander(), Ok(false));
        }

        // Every built-in appears in exactly one of the groups above.
        let mut all: Vec<GameFormat> = partner_families.to_vec();
        all.extend(solo);
        all.extend(no_commander_per_count);
        let mut all_strings: Vec<String> = all.iter().map(GameFormat::to_string).collect();
        let mut expected: Vec<String> = GameFormat::iter().map(|f| f.to_string()).collect();
        all_strings.sort();
        expected.sort();
        assert_eq!(
            all_strings, expected,
            "every built-in format must appear in exactly one group"
        );
    }

    /// Cross-checks `commander_pairing()`'s placement against an existing,
    /// already-pinned axis. Cannot distinguish `Solo` from `PartnerFamilies` —
    /// only what `command_zone_holds_decklist_commander` answers — so it
    /// catches a placement typo the per-count test above would also catch,
    /// from an independent authority.
    #[test]
    fn commander_pairing_agrees_with_the_command_zone_axis() {
        use strum::IntoEnumIterator;

        for format in GameFormat::iter() {
            // Custom answers `Err` here by design; excluded, as its own doc
            // states.
            let Ok(holds_decklist_commander) = format.command_zone_holds_decklist_commander()
            else {
                continue;
            };
            assert_eq!(
                format.commander_pairing() == CommanderPairing::NoCommander,
                !holds_decklist_commander,
                "{format:?}: commander_pairing() disagrees with \
                 command_zone_holds_decklist_commander()"
            );
        }
    }

    /// Cross-checks `deck_size_subject()`'s placement against the same
    /// existing axis. Deliberately partial: it cannot separate
    /// `MainDeckAndCommanders` from `MainDeckAndCommandZone` — no existing
    /// method can make that distinction — so it only catches a format placed
    /// at `MainDeck` for which `command_zone_holds_decklist_commander` is
    /// `Ok(true)`, or vice versa.
    #[test]
    fn deck_size_subject_agrees_with_the_command_zone_axis() {
        use strum::IntoEnumIterator;

        for format in GameFormat::iter() {
            let Ok(holds_decklist_commander) = format.command_zone_holds_decklist_commander()
            else {
                continue;
            };
            assert_eq!(
                format.deck_size_subject() == DeckSizeSubject::MainDeck,
                !holds_decklist_commander,
                "{format:?}: deck_size_subject() disagrees with \
                 command_zone_holds_decklist_commander()"
            );
        }
    }

    /// Dandân is a fixed 80-card, two-player, command-zone-free format, and its
    /// registry entry, constructor and default config agree.
    #[test]
    fn dandan_config_and_registry_entry() {
        let config = FormatConfig::for_format(GameFormat::Dandan).unwrap();
        assert_eq!(config, FormatConfig::dandan());
        assert_eq!(config.starting_life, 20);
        assert_eq!((config.min_players, config.max_players), (2, 2));
        assert_eq!(config.deck_size, DeckSizeRule::Exactly(80));
        assert!(config.supplies_fixed_deck);
        assert!(!config.command_zone && !config.singleton && !config.uses_commander);
        assert_eq!(config.sideboard_policy, SideboardPolicy::Forbidden);
        assert_eq!(config.default_deck_copy_limit, DeckCopyLimit::Unlimited);

        // Positive control: Momir differs from Dandan on exactly the format
        // identity, the deck size and the command zone, so those fields are
        // read and not defaulted.
        assert_ne!(config, FormatConfig::momir());
        assert_eq!(
            config,
            FormatConfig {
                format: GameFormat::Dandan,
                deck_size: DeckSizeRule::Exactly(80),
                command_zone: false,
                ..FormatConfig::momir()
            }
        );

        let registry = GameFormat::registry();
        let entry = registry.last().unwrap();
        assert_eq!(entry.format, GameFormat::Dandan);
        assert_eq!(entry.label, "Dand\u{e2}n");
        assert_eq!(entry.short_label, "DAN");
        assert_eq!(entry.group, FormatGroup::Multiplayer);
        assert_eq!(entry.legality_key, None);
        assert_eq!(entry.default_config, FormatConfig::dandan());
    }

    /// Each Dandân axis departs from the stock answer for exactly the formats
    /// that declare the departure, and `Custom(_)` always answers the stock
    /// value.
    #[test]
    fn format_axes_depart_from_the_stock_answers_only_where_declared() {
        use strum::IntoEnumIterator;

        let all: Vec<GameFormat> = GameFormat::iter()
            .chain([GameFormat::Custom(CustomFormatId(0))])
            .collect();
        // Reach-guard: the census walks every built-in, not an empty set.
        assert!(all.len() >= 26);

        fn splits<T: PartialEq>(
            all: &[GameFormat],
            answer: impl Fn(GameFormat) -> T,
            stock: T,
        ) -> (Vec<GameFormat>, usize) {
            let deviating: Vec<GameFormat> = all
                .iter()
                .copied()
                .filter(|format| answer(*format) != stock)
                .collect();
            let stock_count = all.len() - deviating.len();
            (deviating, stock_count)
        }
        let only_dandan = vec![GameFormat::Dandan];

        let (deviating, stock_count) = splits(&all, GameFormat::shared_zones, SharedZones::NONE);
        assert_eq!(deviating, only_dandan);
        assert!(stock_count > 0);
        assert_eq!(
            GameFormat::Dandan.shared_zones(),
            SharedZones::LIBRARY_AND_GRAVEYARD
        );

        let (deviating, stock_count) =
            splits(&all, GameFormat::deal_order, DealOrder::PlayerByPlayer);
        assert_eq!(deviating, only_dandan);
        assert!(stock_count > 0);
        assert_eq!(GameFormat::Dandan.deal_order(), DealOrder::Interleaved);

        let (deviating, stock_count) = splits(
            &all,
            GameFormat::free_reveal_mulligan,
            FreeRevealMulligan::Unavailable,
        );
        assert_eq!(deviating, only_dandan);
        assert!(stock_count > 0);
        assert_eq!(
            GameFormat::Dandan.free_reveal_mulligan(),
            FreeRevealMulligan::WhenHandLacks {
                min_lands: 2,
                min_nonlands: 2
            }
        );

        let (deviating, stock_count) =
            splits(&all, GameFormat::entry_ownership, EntryOwnership::OwnerKept);
        assert_eq!(deviating, only_dandan);
        assert!(stock_count > 0);
        assert_eq!(
            GameFormat::Dandan.entry_ownership(),
            EntryOwnership::ActorOwns
        );

        let (deviating, stock_count) = splits(
            &all,
            GameFormat::opening_hand_equivalence,
            OpeningHandEquivalence::Distinguishable,
        );
        assert_eq!(deviating, vec![GameFormat::Momir]);
        assert!(stock_count > 0);
        assert_eq!(
            GameFormat::Dandan.opening_hand_equivalence(),
            OpeningHandEquivalence::Distinguishable
        );

        let (deviating, stock_count) =
            splits(&all, GameFormat::best_of_three_ceiling, MatchType::Bo3);
        // Horde joins Dandan off the Bo3 stock: a fixed challenge deck with a
        // Forbidden sideboard has nothing to sideboard between games.
        assert_eq!(deviating, vec![GameFormat::Horde, GameFormat::Dandan]);
        assert!(stock_count > 0);
        assert_eq!(GameFormat::Dandan.best_of_three_ceiling(), MatchType::Bo1);
        assert_eq!(GameFormat::Horde.best_of_three_ceiling(), MatchType::Bo1);
    }

    /// The Horde deck picker renders `ChallengeDeck::registry()` directly, so a
    /// deck missing from `ALL` would be silently unselectable. `metadata` is an
    /// exhaustive match (the compiler catches a missing arm); this catches a
    /// variant that was described but never added to `ALL`.
    #[test]
    fn registry_covers_every_challenge_deck() {
        let registry = ChallengeDeck::registry();
        assert_eq!(registry.len(), ChallengeDeck::ALL.len());

        for deck in ChallengeDeck::ALL {
            // Exhaustive match: adding a variant fails to compile here until it
            // is also appended to `ALL`.
            match deck {
                ChallengeDeck::CybermanHorde
                | ChallengeDeck::DndHorde
                | ChallengeDeck::ZombiesHorde
                | ChallengeDeck::SliversHorde
                | ChallengeDeck::HumansGodzillaHorde
                | ChallengeDeck::SauronHorde
                | ChallengeDeck::SarumanHorde
                | ChallengeDeck::LotrTwoTowersHorde
                | ChallengeDeck::TurtleTeamUpHorde => {}
            }
            assert!(
                registry.iter().any(|meta| meta.deck == *deck),
                "{deck:?} is missing from the challenge-deck registry"
            );
        }
    }

    /// Every entry must be renderable (non-empty labels) and internally
    /// consistent — the bundled ruleset must describe the deck it is attached to,
    /// so a client can build a `FormatConfig` straight from a picked entry.
    #[test]
    fn registry_entries_are_renderable_and_self_consistent() {
        for meta in ChallengeDeck::registry() {
            assert!(!meta.label.is_empty(), "{:?} has no label", meta.deck);
            assert!(
                !meta.short_label.is_empty(),
                "{:?} has no short label",
                meta.deck
            );
            assert!(
                !meta.description.is_empty(),
                "{:?} has no description",
                meta.deck
            );
            assert_eq!(
                meta.default_ruleset.challenge_deck, meta.deck,
                "{:?} bundles a ruleset for a different deck",
                meta.deck
            );
            // Every entry ships a rendered rules summary the picker can display,
            // and it must be exactly what the deck's ruleset renders — the two
            // must not drift.
            assert_eq!(
                meta.rules,
                meta.default_ruleset.summary(),
                "{:?}'s bundled rules summary does not match its ruleset",
                meta.deck
            );
            assert!(
                !meta.rules.is_empty(),
                "{:?} has no rules summary",
                meta.deck
            );
            for line in &meta.rules {
                assert!(
                    !line.label.is_empty(),
                    "{:?} has an unlabeled rule",
                    meta.deck
                );
                assert!(
                    !line.detail.is_empty(),
                    "{:?}'s '{}' rule has no detail",
                    meta.deck,
                    line.label
                );
            }
        }
    }

    /// `HordeRuleset::summary` is the single authority for rules prose, so test it
    /// as a building block across the full `WaveTermination` space — including the
    /// `FixedCount` / `Fixed(n>1)` shapes no shipped deck uses yet (Theros decks
    /// will) — not just the current decks' output.
    #[test]
    fn ruleset_summary_renders_every_wave_shape() {
        let with_wave = |wave: WaveTermination| HordeRuleset {
            challenge_deck: ChallengeDeck::CybermanHorde,
            wave,
            survivor_setup_turns: 3,
            combined_base_life: 100,
            per_extra_survivor_life_delta: -15,
            horde_creatures_forced_attackers: true,
            legendary_death: HordeLegendaryDeath::Normal,
            post_combat_activation: HordePostCombatActivation::None,
            survivor_deck_format: SurvivorDeckFormat::Constructed,
            co_horde_decks: Vec::new(),
            boss_stages: None,
        };
        // Pull the "Waves" line out of the rendered summary.
        let wave_detail = |wave: WaveTermination| -> String {
            with_wave(wave)
                .summary()
                .into_iter()
                .find(|l| l.label == "Waves")
                .expect("summary always has a Waves line")
                .detail
        };

        assert_eq!(
            wave_detail(WaveTermination::UntilNonToken {
                count: WaveCount::Fixed(1)
            }),
            "Reveals until the first nontoken card is cast"
        );
        assert_eq!(
            wave_detail(WaveTermination::UntilNonToken {
                count: WaveCount::Fixed(2)
            }),
            "Reveals until 2 nontoken cards are cast"
        );
        assert_eq!(
            wave_detail(WaveTermination::UntilNonToken {
                count: WaveCount::Snaking { min: 1, max: 3 }
            }),
            "Reveals until N nontokens are cast \u{2014} N snakes 1 \u{2192} 3 \u{2192} 1 each turn"
        );
        assert_eq!(
            wave_detail(WaveTermination::UntilRarityAtLeast(Rarity::Uncommon)),
            "A wave ends at the first uncommon or better card"
        );
        assert_eq!(
            wave_detail(WaveTermination::FixedCount(2)),
            "Reveals 2 cards each Horde turn"
        );
        // Singular grammar for a one-card fixed wave.
        assert_eq!(
            wave_detail(WaveTermination::FixedCount(1)),
            "Reveals 1 card each Horde turn"
        );
    }

    /// The life line must fold both the shared base and the per-extra-survivor
    /// scaling; a flat total (Theros: `delta == 0`) drops the scaling clause.
    #[test]
    fn ruleset_summary_life_line_handles_flat_and_scaled_totals() {
        let line = |base: i32, delta: i32| -> String {
            HordeRuleset {
                challenge_deck: ChallengeDeck::CybermanHorde,
                wave: WaveTermination::FixedCount(1),
                survivor_setup_turns: 3,
                combined_base_life: base,
                per_extra_survivor_life_delta: delta,
                horde_creatures_forced_attackers: true,
                legendary_death: HordeLegendaryDeath::Normal,
                post_combat_activation: HordePostCombatActivation::None,
                survivor_deck_format: SurvivorDeckFormat::Constructed,
                co_horde_decks: Vec::new(),
                boss_stages: None,
            }
            .summary()
            .into_iter()
            .find(|l| l.label == "Survivor life")
            .expect("summary always has a Survivor life line")
            .detail
        };
        assert_eq!(line(100, -15), "100 life shared, -15 per extra survivor");
        assert_eq!(line(20, 0), "20 life, shared by all survivors");
    }

    /// The legendary axis is advanced-only: a `Normal` ruleset renders no
    /// "Legendary deaths" line (basic decks stay their five base lines), while an
    /// `EtbThenPhaseOut` ruleset adds exactly one. This is the axis's contract
    /// with the picker — it surfaces only where the rule actually differs.
    #[test]
    fn ruleset_summary_shows_legendary_line_only_for_advanced_rule() {
        let with_legendary = |rule: HordeLegendaryDeath| HordeRuleset {
            challenge_deck: ChallengeDeck::CybermanHorde,
            wave: WaveTermination::FixedCount(1),
            survivor_setup_turns: 3,
            combined_base_life: 100,
            per_extra_survivor_life_delta: -15,
            horde_creatures_forced_attackers: true,
            legendary_death: rule,
            post_combat_activation: HordePostCombatActivation::None,
            survivor_deck_format: SurvivorDeckFormat::Constructed,
            co_horde_decks: Vec::new(),
            boss_stages: None,
        };

        let basic = with_legendary(HordeLegendaryDeath::Normal).summary();
        assert!(
            !basic.iter().any(|l| l.label == "Legendary deaths"),
            "a Normal ruleset must not advertise the advanced legendary rule"
        );
        assert_eq!(
            basic.len(),
            5,
            "basic decks stay their five base rule lines"
        );

        let advanced = with_legendary(HordeLegendaryDeath::EtbThenPhaseOut).summary();
        let legendary_line = advanced
            .iter()
            .find(|l| l.label == "Legendary deaths")
            .expect("an EtbThenPhaseOut ruleset must surface a Legendary deaths line");
        assert!(
            legendary_line.detail.contains("phases out"),
            "the legendary line must describe the phase-out behavior"
        );
    }

    /// The post-combat-activation axis is advanced-only too: `None` renders no
    /// "Post-combat" line; `OncePerPermanent` adds exactly one.
    #[test]
    fn ruleset_summary_shows_post_combat_line_only_for_advanced_rule() {
        let with_activation = |rule: HordePostCombatActivation| HordeRuleset {
            challenge_deck: ChallengeDeck::CybermanHorde,
            wave: WaveTermination::FixedCount(1),
            survivor_setup_turns: 3,
            combined_base_life: 100,
            per_extra_survivor_life_delta: -15,
            horde_creatures_forced_attackers: true,
            legendary_death: HordeLegendaryDeath::Normal,
            post_combat_activation: rule,
            survivor_deck_format: SurvivorDeckFormat::Constructed,
            co_horde_decks: Vec::new(),
            boss_stages: None,
        };

        let basic = with_activation(HordePostCombatActivation::None).summary();
        assert!(
            !basic.iter().any(|l| l.label == "Post-combat"),
            "a None ruleset must not advertise post-combat activation"
        );
        assert_eq!(
            basic.len(),
            5,
            "basic decks stay their five base rule lines"
        );

        let advanced = with_activation(HordePostCombatActivation::OncePerPermanent).summary();
        let line = advanced
            .iter()
            .find(|l| l.label == "Post-combat")
            .expect("an OncePerPermanent ruleset must surface a Post-combat line");
        assert!(
            line.detail.contains("infinite mana"),
            "the post-combat line must mention the Horde's infinite mana"
        );
    }

    /// The whole point of the summary is to show how decks DIFFER — two decks with
    /// different wave rules must produce different "Waves" lines.
    #[test]
    fn different_decks_produce_different_wave_summaries() {
        let waves = |deck: ChallengeDeck| -> String {
            deck.metadata()
                .rules
                .into_iter()
                .find(|l| l.label == "Waves")
                .unwrap()
                .detail
        };
        assert_ne!(
            waves(ChallengeDeck::CybermanHorde),
            waves(ChallengeDeck::ZombiesHorde)
        );
        assert_ne!(
            waves(ChallengeDeck::CybermanHorde),
            waves(ChallengeDeck::DndHorde)
        );
    }

    /// Pin each deck's defining wave rule — these are what make them play
    /// differently, and a silent swap would change the whole feel of a deck.
    #[test]
    fn challenge_decks_keep_their_defining_wave_rules() {
        assert_eq!(
            ChallengeDeck::CybermanHorde.default_ruleset().wave,
            WaveTermination::UntilNonToken {
                count: WaveCount::Fixed(1)
            }
        );
        assert_eq!(
            ChallengeDeck::DndHorde.default_ruleset().wave,
            WaveTermination::UntilRarityAtLeast(Rarity::Uncommon)
        );
        assert_eq!(
            ChallengeDeck::ZombiesHorde.default_ruleset().wave,
            WaveTermination::UntilNonToken {
                count: WaveCount::Snaking { min: 1, max: 3 }
            }
        );
    }

    /// Pin the published community survivor-life rule (hordemagic basic rules:
    /// "combined, shared and start at 100 life. Each player reduces {15} life ...
    /// beyond the 1st"). The `horde_multiplayer` mechanism tests are deliberately
    /// balance-agnostic (they derive from the ruleset), so this is where the
    /// concrete 100 / −15 → 100·85·70 numbers are actually asserted; a wrong base
    /// or delta slips past those tests but not this one.
    #[test]
    fn community_decks_use_the_published_shared_life_rule() {
        for deck in ChallengeDeck::ALL {
            let r = deck.default_ruleset();
            if *deck == ChallengeDeck::TurtleTeamUpHorde {
                // Turtle Team-Up prints its OWN life table in the product
                // booklet (20/30/40 shared for 2/3/4 heroes), not the community
                // 100/−15 — pin that table deliberately instead.
                assert_eq!(
                    r.combined_base_life, 10,
                    "Turtle Team-Up must use the product's 10-life hero base"
                );
                assert_eq!(
                    r.per_extra_survivor_life_delta, 10,
                    "Turtle Team-Up must add 10 per extra hero"
                );
                // 2 / 3 / 4 heroes → 20 / 30 / 40.
                let combined =
                    |n: i32| r.combined_base_life + r.per_extra_survivor_life_delta * (n - 1);
                assert_eq!((combined(2), combined(3), combined(4)), (20, 30, 40));
                continue;
            }
            assert_eq!(
                r.combined_base_life, 100,
                "{deck:?} must use the 100-life community base"
            );
            assert_eq!(
                r.per_extra_survivor_life_delta, -15,
                "{deck:?} must reduce 15 per extra survivor"
            );
            // 1 / 2 / 3 survivors → 100 / 85 / 70.
            let combined =
                |n: i32| r.combined_base_life + r.per_extra_survivor_life_delta * (n - 1);
            assert_eq!((combined(1), combined(2), combined(3)), (100, 85, 70));
        }
    }

    /// A constant schedule is the classic rule and must not vary by turn.
    #[test]
    fn fixed_wave_count_is_constant_across_turns() {
        let fixed = WaveCount::Fixed(1);
        for turn in 0..10 {
            assert_eq!(fixed.nontokens_for_turn(turn), 1);
        }
    }

    /// The Zombies Horde's published ramp: "Wave 1 ... then Wave 2 ... and so on
    /// for Wave 3. On its next turn, it will descend back down to Wave 2, snaking
    /// back and forth." Pin the exact sequence over two full periods.
    #[test]
    fn snaking_wave_count_ramps_up_and_back_down() {
        let snake = WaveCount::Snaking { min: 1, max: 3 };
        let seq: Vec<u32> = (0..9).map(|t| snake.nontokens_for_turn(t)).collect();
        assert_eq!(seq, vec![1, 2, 3, 2, 1, 2, 3, 2, 1]);
    }

    /// The endpoints must not repeat — a naive "up then reversed" schedule would
    /// emit 1,2,3,3,2,1 and linger a turn too long at each extreme.
    #[test]
    fn snaking_wave_count_does_not_repeat_its_endpoints() {
        let snake = WaveCount::Snaking { min: 2, max: 4 };
        let seq: Vec<u32> = (0..8).map(|t| snake.nontokens_for_turn(t)).collect();
        assert_eq!(seq, vec![2, 3, 4, 3, 2, 3, 4, 3]);
    }

    /// A degenerate range must still yield a terminating wave rather than
    /// dividing by zero on the period.
    #[test]
    fn snaking_wave_count_handles_degenerate_ranges() {
        assert_eq!(
            WaveCount::Snaking { min: 2, max: 2 }.nontokens_for_turn(7),
            2
        );
        assert_eq!(
            WaveCount::Snaking { min: 3, max: 1 }.nontokens_for_turn(7),
            3
        );
    }
}
