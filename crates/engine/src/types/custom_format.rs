//! Schema for engine-validated custom formats: types, validation, the
//! registration gates, and the bundled Axis-B preset constructors.
//!
//! `IMPLEMENTED_LEGACY_AXES` is still empty, so no `LegacyRuleSet` axis has
//! runtime behavior yet — later phases (2a/2b/2cd) wire them in (mana pool
//! cleanup, combat damage step, etc.). The one exception is
//! [`AntePolicy::Excluded`], whose CR 407.3 deck-construction consequence
//! `game::deck_validation` enforces today; it is a default rather than a
//! declared axis, which is why it needs no entry in that list.
//!
//! `custom_format_registry()` still returns `Vec::new()`, but no longer for
//! want of a preset: [`swedish_old_school`] exists and passes both gates, and
//! is withheld for the sourcing reason its own doc comment gives.

use serde::{Deserialize, Serialize};

use crate::types::format::{
    DeckCopyLimit, DeckSizeRule, FormatConfig, GameFormat, RangeOfInfluenceConfig, SideboardPolicy,
};

/// Lightweight, `Copy`, per-`GameState` transport tag for a custom format.
/// The full ruleset never needs a registry round-trip within one game — see
/// `FormatConfig.custom_rules`, which carries the resolved `CustomFormatRules`
/// value directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CustomFormatId(pub u16);

/// The reserved id every Axis-A "save the current lobby setup as a custom
/// format" definition carries (see [`CustomFormatDef::from_lobby_config`]).
/// A lobby save is ad-hoc and client-persisted — it is never registered in
/// [`custom_format_registry`], so it has no registry-stable id of its own and
/// must not be able to impersonate one. Reserving a single sentinel (rather
/// than letting a lobby save pick an arbitrary id) makes that impersonation
/// unrepresentable, and is enforced in the other direction by
/// [`assert_no_lobby_save_sentinel_collision`]: no bundled preset may ever
/// claim this id.
pub const LOBBY_SAVE_CUSTOM_FORMAT_ID: CustomFormatId = CustomFormatId(0);

/// An MTGJSON-style set code (e.g. "MH3", "LEA"). Distinct from a bare
/// `String` so a card-pool restriction list can't be confused with any other
/// string collection at the type level.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SetCode(pub String);

impl AsRef<str> for SetCode {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A card's English name, as used in banned/restricted lists. A semantic
/// alias, not a newtype: every existing card-name comparison in the engine
/// already operates on plain `String`/`&str`, and wrapping this one field
/// would force `.0`-unwrapping at every pre-existing call site for no
/// behavioral gain.
pub type CardName = String;

/// Mana burn was removed from the rules in the Magic 2010 rules change and
/// has no number in the current Comprehensive Rules (see the "Mana Burn
/// (Obsolete)" glossary entry, `docs/MagicCompRules.txt`). This axis exists
/// so a historically-accurate custom format (e.g. Old School 93/94) can opt
/// back into it. Variant names match `docs/proposals/custom-format-engine/
/// PLAN.md`'s canonical schema exactly. Schema only in this phase — no
/// enforcement exists until a later phase wires it into `types/mana.rs`'s
/// cleanup-step unspent-mana handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ManaBurnPolicy {
    /// No mana burn (removed post-M10).
    #[default]
    Modern,
    /// Life loss for unspent mana at real phase-group boundaries. EC/Swedish
    /// target era.
    Obsolete,
}

/// CR 510 (Combat Damage Step): the modern rules deal all combat damage —
/// first strike and regular — in one unified damage step per combat-damage
/// sub-step, not using the stack (CR 510.2). `OnStack` reproduces the older
/// pre-6th-edition procedure, where assigned combat damage was itself placed
/// on the stack as a stack object rather than a triggered ability, giving
/// players a priority window between assignment and dealing before it
/// resolved. Variant names match `docs/proposals/custom-format-engine/
/// PLAN.md`'s canonical schema exactly. Schema only in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CombatDamageTiming {
    #[default]
    Modern,
    OnStack,
}

/// Scope for "Wish"-style effects that fetch a card from outside the game.
/// No single Comprehensive Rules number governs this generically — each
/// Wish-effect card's own Oracle text defines its behavior, against the
/// general "outside the game" zone concept (CR 400.11: an object is outside
/// the game if it isn't in any of the game's zones; CR 400.11a: sideboard
/// cards are outside the game — one instance of that general concept, not
/// an exhaustive list of every way to be outside the game). Variant names
/// match `docs/proposals/custom-format-engine/PLAN.md`'s canonical schema
/// exactly. Schema only in this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WishOutsideGameScope {
    /// Modern deck-construction/tournament policy (CR 100.4: sideboard
    /// rules and restrictions are set by the Magic: The Gathering
    /// Tournament Rules), not a Comprehensive Rules mandate: in a modern,
    /// sanctioned constructed deck the registered sideboard is the only
    /// legitimate "outside the game" zone a Wish effect can retrieve from,
    /// because no current card creates any other one — older templating
    /// that removed cards from the game (see `PreM10ReachesExile`) has been
    /// replaced by exile.
    #[default]
    PostM10SideboardOnly,
    /// Pre-M10: a Wish could retrieve an owned card that had been removed
    /// from the game (today's exile).
    PreM10ReachesExile,
}

/// CR 704.5j: the "legend rule" state-based action. `PreM14AnyController`
/// reproduces a historical ruling some casual formats use (the Legends
/// 1994 / pre-M14 "both die" form): same-named legendary permanents go to
/// their owners' graveyards across ALL controllers combined, choicelessly,
/// rather than per-controller. Variant names match `docs/proposals/
/// custom-format-engine/PLAN.md`'s canonical schema exactly. Schema only in
/// this phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LegendRuleScope {
    /// Per-controller + choice (post-2013-07 M14). All four EC presets use
    /// this.
    #[default]
    Modern,
    PreM14AnyController,
}

/// CR 407.1: an ante rule was in "earlier versions of the Magic rules";
/// playing for ante "is now considered an optional variation on the game" and
/// is "strictly forbidden under the Magic: The Gathering Tournament Rules".
/// That makes it the same shape as the other axes here — a rule this engine
/// plays the modern way, which a historically-accurate custom format may opt
/// back out of.
///
/// Unlike its siblings, this axis' DEFAULT carries an enforced consequence
/// rather than merely describing the modern status quo: CR 407.3 says that
/// "when not playing for ante, players can't include these cards in their
/// decks or sideboards", which
/// `game::deck_validation::DeclaredPool::status` enforces today for every
/// custom format. `Enabled` is what remains unimplemented — it promises the
/// CR 407.2 ante zone and the CR 407.4 ante action, which no engine code
/// provides — so it is gated by [`IMPLEMENTED_LEGACY_AXES`] like any other
/// declared-but-unbuilt axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AntePolicy {
    /// CR 407.3: cards bearing "Remove this card from your deck before
    /// playing if you're not playing for ante" may not be in a deck or
    /// sideboard. The modern default, and the only value the engine
    /// implements.
    #[default]
    Excluded,
    /// CR 407.2: each player antes a card after determining who goes first;
    /// the winner takes the ante zone. Schema only — gated until an ante zone
    /// exists.
    Enabled,
}

/// `Default` is every axis at its modern value — the rule set an Axis-A
/// lobby save always declares (it models no historical paper ruleset), and
/// the only one `passes_legacy_axis_gate` accepts while
/// `IMPLEMENTED_LEGACY_AXES` is empty.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LegacyRuleSet {
    pub mana_burn: ManaBurnPolicy,
    pub damage_timing: CombatDamageTiming,
    pub wish_scope: WishOutsideGameScope,
    pub legend_rule_scope: LegendRuleScope,
    /// `#[serde(default)]` because this axis was added after Phase 1c shipped
    /// the Axis-A save path: a `CustomFormatDef` already persisted by a
    /// client carries no `ante` key, and must keep deserializing to the
    /// modern `Excluded` — which is exactly what such a save meant. The
    /// sibling axes need no default, having been present since Phase 1a.
    /// Mirrors `StructuralRules.range_of_influence`'s use of the same
    /// attribute for the same reason.
    #[serde(default)]
    pub ante: AntePolicy,
}

/// CR 903.3 (and the Tiny Leaders / Oathbreaker RC / Brawl deck-construction
/// rules, each layered on top of their own commander-style base format):
/// which commander-eligibility test a custom format modeled after a given
/// built-in commander-style format should apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommanderEligibilityRule {
    Standard,
    TinyLeaders,
    OathbreakerSignatureSpell,
    BrawlColorIdentity,
}

impl CommanderEligibilityRule {
    /// Maps a BUILT-IN source `GameFormat` (the format a custom format is
    /// being modeled after) to the eligibility rule it should reuse.
    /// `Ok(None)` means the built-in genuinely has no commander-eligibility
    /// concept (e.g. Standard, Limited); `Ok(Some(rule))` names the rule a
    /// commander-style built-in uses. `Err` for `GameFormat::Custom`: this
    /// function's contract is that `format` names a built-in a custom format
    /// is modeled after, and a bare `Custom(id)` has no "source format" of
    /// its own to read — that is a distinct condition from "this built-in
    /// has no commander concept," so it is not collapsed into the same
    /// `None` a caller would otherwise have to disambiguate from context.
    pub fn from_source_format(format: GameFormat) -> Result<Option<Self>, FormatConfigError> {
        match format {
            // CR 903.13g: Commander Draft games follow Commander's rules, and
            // CR 903.13f routes its deck construction through CR 903.5 — so
            // CR 903.3's commander eligibility test applies unchanged.
            GameFormat::Commander
            | GameFormat::DuelCommander
            | GameFormat::PauperCommander
            | GameFormat::CommanderDraft => Ok(Some(Self::Standard)),
            GameFormat::TinyLeaders => Ok(Some(Self::TinyLeaders)),
            GameFormat::Oathbreaker => Ok(Some(Self::OathbreakerSignatureSpell)),
            GameFormat::Brawl | GameFormat::HistoricBrawl => Ok(Some(Self::BrawlColorIdentity)),
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
            | GameFormat::Momir => Ok(None),
            GameFormat::Custom(id) => Err(FormatConfigError(format!(
                "from_source_format: source must be a built-in GameFormat, never Custom({})",
                id.0
            ))),
        }
    }
}

/// Whether a custom format uses the command zone (CR 903) and, if so, its
/// commander-damage threshold and eligibility predicate. A single
/// discriminated type instead of three independently-settable fields, so a
/// state like "command zone disabled, but a commander-damage threshold and
/// eligibility rule are set" is unrepresentable — the engine would otherwise
/// have no valid semantic reading for it, and neither registration gate
/// could catch it (serde happily accepts it either way).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandZoneMode {
    Disabled,
    Enabled {
        commander_damage_threshold: Option<u8>,
        eligibility_rule: CommanderEligibilityRule,
    },
}

/// The structural game-parameter snapshot a lobby's "save as custom format"
/// action captures. Every field mirrors an existing `FormatConfig` field 1:1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuralRules {
    pub starting_life: i32,
    pub min_players: u8,
    pub max_players: u8,
    /// CR 100.5 / CR 903.5a: the DECLARED deck-size rule, typed exactly like
    /// the `FormatConfig.deck_size` field it mirrors 1:1. A bare `u16` could
    /// not round-trip which [`DeckSizeRule`] variant a format uses — a saved
    /// Commander-shaped format (`Exactly(100)`) and a saved Commander-Draft-
    /// shaped one (`Minimum(60)`) would both collapse to a number, and the
    /// resolver rebuilding a `FormatConfig` from these rules would have to
    /// guess the missing half of the rule. CR 903.13f(1) is exactly the case
    /// where guessing is wrong (a command-zone format with no maximum), which
    /// is why `FormatConfig` itself stopped inferring it.
    pub deck_size: DeckSizeRule,
    pub singleton: bool,
    pub command_zone_mode: CommandZoneMode,
    #[serde(default)]
    pub range_of_influence: Option<Box<RangeOfInfluenceConfig>>,
    pub team_based: bool,
    /// The DECLARED sideboard policy for this custom format.
    /// `FormatConfig.sideboard_policy` (Phase 1a) is the RESOLVED mirror for
    /// built-in formats today; deriving it for `Custom` from this field via
    /// the real resolver is Phase 1c's widening (see
    /// `docs/proposals/custom-format-engine/IMPLEMENTATION_PLAN.md`).
    pub sideboard_policy: SideboardPolicy,
    /// CR 100.2a / CR 100.2b / CR 903.5b: the DECLARED default
    /// deck-construction copy ceiling, before per-card printed overrides and
    /// the basic-land exemption (both applied by
    /// `game::deck_validation::max_deck_copies`). A direct-copy mirror of
    /// `FormatConfig.default_deck_copy_limit` (Phase 1b), exactly like
    /// `sideboard_policy` above mirrors `FormatConfig.sideboard_policy`:
    /// without it, a lobby save would silently discard the source format's
    /// real ceiling and the resolver would have nothing to rebuild it from
    /// but `GameFormat::Custom(_).default_deck_copy_limit()`'s fail-closed
    /// `UpTo(1)` fallback — the same silent-data-loss bug `sideboard_policy`
    /// exists to prevent.
    pub default_deck_copy_limit: DeckCopyLimit,
}

impl StructuralRules {
    /// Projects the structural half of a resolved [`FormatConfig`], reading
    /// every value from the config's own fields rather than from a bare
    /// `GameFormat` method (see [`CustomFormatDef::from_lobby_config`]'s doc
    /// comment for why that distinction matters).
    ///
    /// `command_zone_mode` is a parameter rather than a derivation because
    /// deriving it is fallible — a source format whose command zone holds
    /// something other than a commander has no representation here — and both
    /// callers already know the answer more directly than this function
    /// could: `from_lobby_config` has just run the fallible match (and owns
    /// the error messages for it), and a bundled constructed-shaped preset
    /// passes [`CommandZoneMode::Disabled`] outright.
    ///
    /// Shared by Axis A (`from_lobby_config`) and the Axis-B preset
    /// constructors so the field-by-field projection exists once. Adding a
    /// field to this struct then has exactly one place to update, instead of
    /// one per preset.
    fn from_format_config(config: &FormatConfig, command_zone_mode: CommandZoneMode) -> Self {
        Self {
            starting_life: config.starting_life,
            min_players: config.min_players,
            max_players: config.max_players,
            deck_size: config.deck_size,
            singleton: config.singleton,
            command_zone_mode,
            range_of_influence: config.range_of_influence.clone(),
            team_based: config.team_based,
            sideboard_policy: config.sideboard_policy,
            default_deck_copy_limit: config.default_deck_copy_limit,
        }
    }
}

/// Legality/era rules. `legal_sets: None` means unrestricted (every card
/// passes the pool check); `Some(list)` restricts to exactly that list. This
/// `Option` (not a bare possibly-empty `Vec`) is required to distinguish "no
/// restriction" from "restricted to nothing."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalityRules {
    pub legal_sets: Option<Vec<SetCode>>,
    pub banned: Vec<CardName>,
    pub restricted: Vec<CardName>,
    pub legacy: LegacyRuleSet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFormatRules {
    pub id: CustomFormatId,
    pub structural: StructuralRules,
    pub legality: LegalityRules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReprintPolicy {
    OriginalPrintingsOnly,
    AllowSpecialReprintSets,
    AllowAnyPrinting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrintingFidelity {
    NotApplicable,
    SetCodeApproximation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFormatDef {
    pub rules: CustomFormatRules,
    pub label: String,
    pub short_label: String,
    pub description: String,
    pub reprint_policy: Option<ReprintPolicy>,
    pub printing_fidelity: PrintingFidelity,
}

/// A malformed-`FormatConfig` rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatConfigError(pub String);

impl std::fmt::Display for FormatConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for FormatConfigError {}

/// How many characters `short_label_from_name` keeps. `FormatMetadata`'s
/// hand-curated `short_label`s ("STD", "CMD", "2HG") are all exactly three,
/// and the frontend's own unrecognized-format fallback is
/// `format.slice(0, 3).toUpperCase()` — this is that same derivation, moved
/// into the engine so an Axis-A save carries a real engine-supplied value
/// instead of the display layer computing one.
const SHORT_LABEL_LEN: usize = 3;

/// Derives a compact badge code from an arbitrary user-supplied format name:
/// the first [`SHORT_LABEL_LEN`] alphanumeric characters of the trimmed name,
/// uppercased. A name with fewer than that many alphanumeric characters
/// yields a shorter code — a deliberate, documented deviation from the
/// "always exactly three" convention every hand-curated built-in happens to
/// satisfy, because there is no meaningful three-character abbreviation to
/// invent for a two-character name. `from_lobby_config` rejects an entirely
/// empty trimmed name outright, so this never returns an empty string on its
/// production path.
fn short_label_from_name(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(SHORT_LABEL_LEN)
        .collect::<String>()
        .to_uppercase()
}

/// Builds the one-line human description an Axis-A save has no human curator
/// to write, from the structural rules' own field values — so two different
/// `StructuralRules` describe themselves differently rather than sharing a
/// static placeholder. Mirrors the built-in phrasing style
/// (`"100-card singleton, 2–4 players"`, `"Tournament 1v1 Commander, 30
/// life"`): short comma-joined structural fragments, no terminal punctuation.
///
/// The contributing fields are deck size (with its [`DeckSizeRule`] variant
/// preserved — "at least N" is not the same claim as "exactly N"), singleton,
/// the player-count range, starting life, and the command zone / team-based
/// flags when set. `sideboard_policy`/`default_deck_copy_limit` are
/// deliberately omitted: they are deck-construction validation inputs, not
/// table-shape facts, and the built-in descriptions this mirrors never
/// mention them either.
fn derive_structural_description(structural: &StructuralRules) -> String {
    let mut parts = Vec::new();

    // CR 100.5 vs CR 903.5a: an exact-size rule and a floor are different
    // claims, so the description must not flatten them into one phrasing.
    let deck = match structural.deck_size {
        DeckSizeRule::Exactly(n) => format!("{n}-card"),
        DeckSizeRule::Minimum(n) => format!("{n}-card minimum"),
    };
    parts.push(if structural.singleton {
        format!("{deck} singleton")
    } else {
        deck
    });

    parts.push(if structural.min_players == structural.max_players {
        format!("{}-player", structural.min_players)
    } else {
        format!(
            "{}\u{2013}{} players",
            structural.min_players, structural.max_players
        )
    });

    parts.push(format!("{} life", structural.starting_life));

    // CR 408.1: the command zone is a distinct game area, so its presence is
    // a table-shape fact worth surfacing.
    if matches!(
        structural.command_zone_mode,
        CommandZoneMode::Enabled { .. }
    ) {
        parts.push("command zone".to_string());
    }
    if structural.team_based {
        parts.push("team-based".to_string());
    }

    parts.join(", ")
}

impl CustomFormatDef {
    /// Axis A: captures a lobby's live, fully-resolved built-in
    /// `FormatConfig` as a saved custom-format DEFINITION (never an active
    /// `FormatConfig` — [`crate::types::format::FormatConfig::for_custom_rules`]
    /// is the reverse direction, applied only when a player later selects
    /// this definition to start a game).
    ///
    /// Every structural field is read from `config`'s own RESOLVED, stored
    /// fields — never from a bare `GameFormat` method. `sideboard_policy()`
    /// and `default_deck_copy_limit()` both return a disclosed fail-closed
    /// fallback for `GameFormat::Custom`, and more importantly a lobby host
    /// may have tuned a field away from its format default; reading the
    /// method would silently save something the host never configured.
    ///
    /// `legality` is left at defaults (`legal_sets: None`, empty
    /// banned/restricted, default `LegacyRuleSet`): a lobby save models no
    /// published paper ruleset, so it has no card-pool or era intent to
    /// declare. `reprint_policy: None` / `printing_fidelity: NotApplicable`
    /// for the same reason.
    ///
    /// Returns `Err` rather than silently dropping data whenever `config` is
    /// a state this conversion cannot faithfully represent.
    ///
    /// Two `FormatConfig` fields are deliberately NOT captured, per the
    /// charter's own accounting: `archenemy_player` is per-seating table
    /// state, not a format rule (and the only format that sets it is
    /// rejected below anyway), and `supplies_fixed_deck` is always `false`
    /// for every custom format — no custom-format use case for an
    /// engine-supplied fixed deck exists, and the only built-in that sets it
    /// (Momir) is likewise rejected below.
    pub fn from_lobby_config(
        name: String,
        config: &FormatConfig,
    ) -> Result<Self, FormatConfigError> {
        // Re-saving an already-custom format is out of scope for Axis A: the
        // source's `legality` (legal_sets/banned/restricted/legacy) has no
        // home in this conversion, which always writes defaults, so the save
        // would silently drop it. `from_source_format` below would reject
        // `Custom` too, but only when the command-zone branch is reached —
        // check it up front so the rejection does not depend on the source's
        // command-zone flag.
        if let GameFormat::Custom(id) = config.format {
            return Err(FormatConfigError(format!(
                "from_lobby_config cannot save Custom({}) as a new custom format — the source's \
                 own legality rules (legal_sets/banned/restricted/legacy) have no representation \
                 in a lobby save and would be silently dropped",
                id.0
            )));
        }

        if name.trim().is_empty() {
            return Err(FormatConfigError(
                "from_lobby_config requires a non-empty format name — there is nothing to label \
                 the saved format with"
                    .to_string(),
            ));
        }
        // Normalize once, right after validating: the emptiness check above
        // already treats leading/trailing whitespace as insignificant, so the
        // stored `label` should match that judgment rather than preserving
        // whitespace the validation itself ignored.
        let name = name.trim().to_string();

        // Closes the general defect class documented on
        // `GameFormat::has_unrepresentable_auxiliary_deck_component`: Planechase
        // (CR 901.15a, shared planar deck), Archenemy (CR 904.3, scheme deck),
        // and Momir (CR 109.4c / CR 114.1, game-start emblem) each get an
        // auxiliary deck/component from `deck_loading.rs` keyed on this exact
        // `GameFormat` literal, with no `StructuralRules` field able to carry
        // it forward. Checked ahead of the command-zone/eligibility match
        // below because Planechase's `command_zone` is `false` — it would
        // otherwise fall straight through to `CommandZoneMode::Disabled` and
        // save "successfully," silently losing the planar deck. Archenemy and
        // Momir are also caught here now (previously only by the `(true,
        // None)` arm below, which this predicate makes unreachable for them —
        // left in place as a defensive fallback for any future built-in that
        // sets `command_zone: true` without a commander concept).
        if config.format.has_unrepresentable_auxiliary_deck_component() {
            return Err(FormatConfigError(format!(
                "from_lobby_config cannot save {} as a custom format — its deck_loading.rs \
                 behavior grants an auxiliary deck or component (a shared planar deck, a scheme \
                 deck, or a game-start emblem) keyed on this literal format, and StructuralRules \
                 has no representation for it",
                config.format
            )));
        }

        let eligibility_rule = CommanderEligibilityRule::from_source_format(config.format)?;
        let command_zone_mode = match (config.command_zone, eligibility_rule) {
            (true, Some(eligibility_rule)) => CommandZoneMode::Enabled {
                commander_damage_threshold: config.commander_damage_threshold,
                eligibility_rule,
            },
            // Defensive fallback: among today's built-ins, only Archenemy and
            // Momir reach this arm (both `command_zone: true` with no
            // eligibility rule), and both are already rejected above by
            // `has_unrepresentable_auxiliary_deck_component`. Kept so a future
            // command-zone format added to `CommanderEligibilityRule::from_source_format`'s
            // `Ok(None)` bucket without also being added to that predicate
            // still fails closed here instead of silently resolving to
            // `CommandZoneMode::Disabled`.
            (true, None) => {
                return Err(FormatConfigError(format!(
                    "from_lobby_config cannot save {} as a custom format — its command zone holds \
                     format-specific objects rather than a commander, and StructuralRules has no \
                     representation for them",
                    config.format
                )))
            }
            // No command zone: `eligibility_rule` (if the source format even
            // has one) is meaningless without one, so nothing is dropped.
            (false, _) => CommandZoneMode::Disabled,
        };

        let structural = StructuralRules::from_format_config(config, command_zone_mode);
        let description = derive_structural_description(&structural);
        let short_label = short_label_from_name(&name);

        Ok(CustomFormatDef {
            rules: CustomFormatRules {
                id: LOBBY_SAVE_CUSTOM_FORMAT_ID,
                structural,
                legality: LegalityRules {
                    legal_sets: None,
                    banned: Vec::new(),
                    restricted: Vec::new(),
                    legacy: LegacyRuleSet::default(),
                },
            },
            label: name,
            short_label,
            description,
            reprint_policy: None,
            printing_fidelity: PrintingFidelity::NotApplicable,
        })
    }
}

/// Engine-consistency invariant: `format == GameFormat::Custom(id) ⟺
/// custom_rules == Some(rules) && rules.id == id`. Phase 1a checks only this
/// id-consistency (both directions); later phases widen this function as
/// more derived `FormatConfig` fields are added.
pub fn validate_custom_rules_consistency(
    config: &crate::types::format::FormatConfig,
) -> Result<(), FormatConfigError> {
    match (config.format, &config.custom_rules) {
        (GameFormat::Custom(id), Some(rules)) if rules.id == id => Ok(()),
        (GameFormat::Custom(id), Some(rules)) => Err(FormatConfigError(format!(
            "FormatConfig.format is Custom({}) but custom_rules.id is {:?}",
            id.0, rules.id
        ))),
        (GameFormat::Custom(id), None) => Err(FormatConfigError(format!(
            "FormatConfig.format is Custom({}) but custom_rules is None",
            id.0
        ))),
        (_, None) => Ok(()),
        (other, Some(_)) => Err(FormatConfigError(format!(
            "FormatConfig.format is {other:?} (a built-in format) but custom_rules is Some(_) — \
             built-in formats must not carry custom_rules"
        ))),
    }
}

/// One axis of `LegacyRuleSet` behavior. Engine-internal only — never
/// serialized, never part of the wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyAxis {
    ManaBurn,
    CombatDamageTiming,
    WishOutsideGameScope,
    LegendRuleScope,
    Ante,
}

/// Axes of `LegacyRuleSet` the engine actually enforces at runtime. Empty in
/// Phase 1a; later phases populate this as each axis's behavior is wired in.
///
/// `ManaBurn` joined in Phase 2b, which is what makes the Eternal Central Old
/// School presets selectable — they were listed in `custom_format_registry`
/// and rejected here from the moment they existed, and this one entry is the
/// whole of what changed for them. See `game::mana_burn`.
pub const IMPLEMENTED_LEGACY_AXES: &[LegacyAxis] = &[LegacyAxis::ManaBurn];

fn declared_legacy_axes(rules: &LegacyRuleSet) -> Vec<LegacyAxis> {
    let mut axes = Vec::new();
    if rules.mana_burn != ManaBurnPolicy::default() {
        axes.push(LegacyAxis::ManaBurn);
    }
    if rules.damage_timing != CombatDamageTiming::default() {
        axes.push(LegacyAxis::CombatDamageTiming);
    }
    if rules.wish_scope != WishOutsideGameScope::default() {
        axes.push(LegacyAxis::WishOutsideGameScope);
    }
    if rules.legend_rule_scope != LegendRuleScope::default() {
        axes.push(LegacyAxis::LegendRuleScope);
    }
    // Only the non-default `AntePolicy::Enabled` is a declared axis. The
    // default `Excluded` is already enforced (CR 407.3, at
    // `DeclaredPool::status`), so gating it would reject every custom format
    // in existence — including the Axis-A lobby saves whose whole
    // `LegacyRuleSet` is `Default`.
    if rules.ante != AntePolicy::default() {
        axes.push(LegacyAxis::Ante);
    }
    axes
}

/// Registration gate (a): every axis a rule set declares as non-default must
/// be in `IMPLEMENTED_LEGACY_AXES`, or it is rejected.
///
/// Takes the `LegacyRuleSet` rather than the whole `CustomFormatDef` because
/// that is all it has ever read, and because it has a second caller that
/// holds no `CustomFormatDef` at all: `FormatConfig`'s `Deserialize` impl,
/// which sees only a `CustomFormatRules` (display metadata never travels on
/// an active config). Both callers must apply the identical gate — a
/// deserialized custom format that declares an unimplemented axis would
/// otherwise get behavior the engine silently does not enforce.
///
/// Deliberately asymmetric with `legal_sets`/`banned`/`restricted`, which are
/// NOT gated: those are declarative card-pool data the evaluator either
/// applies in full or not at all, so there is no partial-implementation risk.
/// A `LegacyRuleSet` axis instead promises runtime behavior (mana burn, the
/// legend rule's scope, Wish reach, an ante zone) that may not be built yet,
/// so declaring one the engine does not implement silently misrepresents how
/// the game will actually play.
///
/// Note the asymmetry inside [`AntePolicy`] itself, which
/// [`declared_legacy_axes`] documents: only `Enabled` is a declared axis.
/// `Excluded`'s CR 407.3 deck-construction consequence is enforced today and
/// is the default every custom format carries, so it is never gated.
pub fn passes_legacy_axis_gate(rules: &LegacyRuleSet) -> bool {
    declared_legacy_axes(rules)
        .into_iter()
        .all(|axis| IMPLEMENTED_LEGACY_AXES.contains(&axis))
}

/// Registration gate (b): `reprint_policy` presence must agree with
/// `printing_fidelity`.
pub fn passes_reprint_fidelity_gate(def: &CustomFormatDef) -> bool {
    def.reprint_policy.is_some()
        == matches!(
            def.printing_fidelity,
            PrintingFidelity::SetCodeApproximation
        )
}

/// Registration gate (c): no bundled preset may claim
/// [`LOBBY_SAVE_CUSTOM_FORMAT_ID`], which is reserved for Axis-A lobby saves.
/// A collision would make a client-persisted ad-hoc save indistinguishable
/// from a registry-stable preset — `GameFormat::label()` would report the
/// preset's name for someone else's save, and (once Phase 1d's evaluator
/// lands) a save could inherit a preset's banned/restricted lists.
///
/// A real `assert!`, not a `debug_assert!`: neither the `release` nor the
/// `server-release` profile in the workspace `Cargo.toml` overrides
/// `debug-assertions`, so a `debug_assert!` here would be compiled out of
/// every shipped binary — precisely the builds where a preset added later
/// must not be able to silently shadow the sentinel. The preset list is a
/// hardcoded, developer-authored constant, so this can only fire on a
/// programming error, never on user input.
pub fn assert_no_lobby_save_sentinel_collision(presets: &[CustomFormatDef]) {
    for def in presets {
        assert!(
            def.rules.id != LOBBY_SAVE_CUSTOM_FORMAT_ID,
            "custom-format preset {:?} (short_label {:?}) claims CustomFormatId({}), which is \
             reserved as LOBBY_SAVE_CUSTOM_FORMAT_ID for Axis-A lobby saves — give the preset a \
             different id",
            def.label,
            def.short_label,
            LOBBY_SAVE_CUSTOM_FORMAT_ID.0,
        );
    }
}

/// Registry id for [`swedish_old_school`]. The first id after
/// [`LOBBY_SAVE_CUSTOM_FORMAT_ID`]'s reserved `0`; ids are registry-stable
/// and must never be reused or renumbered, since a persisted
/// `FormatConfig`/`GameFormat::Custom(id)` refers to a format by this number.
pub const SWEDISH_OLD_SCHOOL_ID: CustomFormatId = CustomFormatId(1);

/// Swedish Old School 93/94, per the primary source
/// (`oldschool-mtg.blogspot.com/p/banrestriction.html`, re-fetched
/// 2026-09-07 and matching `docs/proposals/custom-format-engine/CONTEXT.md`'s
/// captured lists verbatim): the Alpha-through-The-Dark card pool plus
/// "Summer Magic", no banned cards at all, 25 restricted cards, and fully
/// modern rules.
///
/// **Constructed but deliberately NOT registered.** `custom_format_registry`
/// does not list this def, per PLAN.md §7/§8: the format's reprint policy is
/// CONTEXT.md Open item 6, unresolved. Re-fetching the primary source
/// confirmed every other list here but yielded only "Only English versions
/// are allowed in Oldschool" on reprints — the secondary "no Revised-or-later
/// reprints" claim remains unconfirmed — so `reprint_policy` stays `None`
/// ("no confirmed authored intent to declare", distinct from a lobby save's
/// permanent `None`), `printing_fidelity` stays `NotApplicable` per the §1
/// pairing rule, and the def stays out of the selectable list rather than
/// shipping a label a future maintainer would inherit as fact.
///
/// The empty `banned` list is real data, not a placeholder: Swedish Old
/// School bans nothing, restricting instead. `legal_sets` is `Some(_)` — this
/// format genuinely restricts its pool, and `None` would mean "unrestricted".
///
/// The seven ante cards the source carves out ("must be removed before play
/// unless the tournament is specifically played for ante") need no entry
/// here: `legality.legacy.ante` is [`AntePolicy::Excluded`] by default, and
/// CR 407.3 identifies that class by the cards' own printed text, so
/// `DeclaredPool` excludes them without a name list. Three of the seven
/// (Contract from Below, Darkpact, Tempest Efreet) also appear on the
/// restricted list below, exactly as the source spells it.
///
/// The structural half is `FormatConfig::standard()`'s — 20 life, two
/// players, a 60-card minimum deck, a 15-card sideboard, four copies. The
/// primary source states pool and restriction rules only, so rather than
/// invent structural values this reads them from the shape every built-in
/// 60-card constructed format already spreads (`premodern()`, `legacy()`,
/// `vintage()` and `timeless()` are each literally `..Self::standard()`).
pub fn swedish_old_school() -> CustomFormatDef {
    CustomFormatDef {
        rules: CustomFormatRules {
            id: SWEDISH_OLD_SCHOOL_ID,
            structural: StructuralRules::from_format_config(
                &FormatConfig::standard(),
                CommandZoneMode::Disabled,
            ),
            legality: LegalityRules {
                legal_sets: Some(
                    ["LEA", "LEB", "2ED", "ARN", "ATQ", "LEG", "DRK", "SUM"]
                        .into_iter()
                        .map(|code| SetCode(code.to_string()))
                        .collect(),
                ),
                banned: Vec::new(),
                restricted: [
                    "Ancestral Recall",
                    "Balance",
                    "Black Lotus",
                    "Braingeyser",
                    "Channel",
                    "Chaos Orb",
                    "Contract from Below",
                    "Darkpact",
                    "Demonic Tutor",
                    "Library of Alexandria",
                    "Mana Drain",
                    "Mind Twist",
                    "Mishra's Workshop",
                    "Mox Emerald",
                    "Mox Jet",
                    "Mox Pearl",
                    "Mox Ruby",
                    "Mox Sapphire",
                    "Regrowth",
                    "Sol Ring",
                    "Strip Mine",
                    "Tempest Efreet",
                    "Time Walk",
                    "Timetwister",
                    "Wheel of Fortune",
                ]
                .into_iter()
                .map(CardName::from)
                .collect(),
                // The source mentions no mana burn, no damage on the stack, no
                // pre-M10 Wish templating and no modified legend rule: Swedish
                // Old School is an old card pool played under modern rules.
                legacy: LegacyRuleSet::default(),
            },
        },
        label: "Swedish Old School 93/94".to_string(),
        short_label: "OSS".to_string(),
        description: "Alpha through The Dark (plus Summer Magic), nothing banned, 25 restricted \
                      cards, modern rules"
            .to_string(),
        reprint_policy: None,
        printing_fidelity: PrintingFidelity::NotApplicable,
    }
}

/// Registry id for [`old_school_93_94`]. See [`SWEDISH_OLD_SCHOOL_ID`] on why
/// these are stable and never renumbered.
pub const OLD_SCHOOL_93_94_ID: CustomFormatId = CustomFormatId(2);

/// Registry id for [`old_school_95`].
pub const OLD_SCHOOL_95_ID: CustomFormatId = CustomFormatId(3);

/// The reprint-fidelity disclosure every `SetCodeApproximation` preset's
/// `description` must carry, per PLAN.md §1's pairing rule.
///
/// Both Eternal Central Old School rulesets define legality partly by
/// PRINTING — "all non-foil cards from the sets above, that were reprinted in
/// any language with the original frame and original art" — while this engine
/// knows only set-code membership (`printed_in_any_set`). Two concrete
/// divergences, both verified against Scryfall at implementation time rather
/// than asserted:
///
/// - **Frame/foil is not enforced.** A foil or modern-frame copy of a legal
///   card passes here and would not pass in paper. Over-permissive.
/// - **The promo carve-outs are not included.** Both rulesets name specific
///   legal promos — Arena, Sewers of Estark and Nalathni Dragon for 93/94,
///   plus Giant Badger, Windseeker Centaur and Mana Crypt for 95 — and their
///   sets are NOT in `legal_sets`, so those cards are rejected. Under-
///   permissive, and not fixable at set-code granularity for 93/94: four of
///   those six live in `PHPR` (HarperPrism Book Promos, 5 cards), of which
///   only Arena and Sewers of Estark are 93/94-legal, so admitting the set
///   would admit three cards the format does not allow — one of them Mana
///   Crypt. See this phase's PR discussion.
const SET_CODE_APPROXIMATION_DISCLOSURE: &str =
    "Legality is approximated at the set-code level; original-printing frame/foil and the \
     ruleset's named promo cards are not enforced.";

fn set_codes(codes: &[&str]) -> Vec<SetCode> {
    codes.iter().map(|code| SetCode(code.to_string())).collect()
}

fn card_names(names: &[&str]) -> Vec<CardName> {
    names.iter().map(|name| CardName::from(*name)).collect()
}

/// Eternal Central's Old School 93/94, per the primary source
/// (`raw.githubusercontent.com/northern-information/lordsofthepit.com/main/src/pages/formats.md`,
/// re-fetched 2026-09-09 and matching RESEARCH.md §1 verbatim: 11 legal sets,
/// 22 restricted, 7 banned, mana burn as the only legacy exception).
///
/// **A different ruleset from [`swedish_old_school`], not a duplicate** —
/// different legal sets (this one includes Revised, Fallen Empires and the
/// Collectors' Editions but not Summer Magic), a different restricted list,
/// and a real banned list where the Swedish rules ban nothing.
///
/// Its seven banned cards are exactly the CR 407.3 ante class within this
/// era's pool. `game::ante` already bars them from every deck; the entries are
/// kept because the source states them, and because they must survive a future
/// format that legitimately plays for ante.
///
/// **Selectable as of Phase 2b.** `mana_burn: Obsolete` held this preset out
/// of the registry from the moment it existed; adding `LegacyAxis::ManaBurn`
/// to [`IMPLEMENTED_LEGACY_AXES`] is the entire change that released it — this
/// constructor was not touched. See `game::mana_burn`.
pub fn old_school_93_94() -> CustomFormatDef {
    CustomFormatDef {
        rules: CustomFormatRules {
            id: OLD_SCHOOL_93_94_ID,
            structural: StructuralRules::from_format_config(
                &FormatConfig::standard(),
                CommandZoneMode::Disabled,
            ),
            legality: LegalityRules {
                // Alpha, Beta, Unlimited, Collectors' Edition, Intl.
                // Collectors' Edition, Arabian Nights, Antiquities, Revised,
                // Legends, The Dark, Fallen Empires. Every code verified
                // against Scryfall's live set list at implementation time.
                legal_sets: Some(set_codes(&[
                    "LEA", "LEB", "2ED", "CED", "CEI", "ARN", "ATQ", "3ED", "LEG", "DRK", "FEM",
                ])),
                banned: card_names(&[
                    "Bronze Tablet",
                    "Contract from Below",
                    "Darkpact",
                    "Demonic Attorney",
                    "Jeweled Bird",
                    "Rebirth",
                    "Tempest Efreet",
                ]),
                restricted: card_names(&[
                    "Ancestral Recall",
                    "Balance",
                    "Black Lotus",
                    "Braingeyser",
                    "Chaos Orb",
                    "Channel",
                    "Demonic Tutor",
                    "Library of Alexandria",
                    "Mana Drain",
                    "Mind Twist",
                    "Mox Emerald",
                    "Mox Jet",
                    "Mox Pearl",
                    "Mox Ruby",
                    "Mox Sapphire",
                    "Recall",
                    "Regrowth",
                    "Sol Ring",
                    "Time Vault",
                    "Time Walk",
                    "Timetwister",
                    "Wheel of Fortune",
                ]),
                // The source states mana burn as this format's ONLY legacy
                // exception — no damage on the stack, no pre-M10 Wish
                // templating, no legend-rule reversion, and it is not played
                // for ante.
                legacy: LegacyRuleSet {
                    mana_burn: ManaBurnPolicy::Obsolete,
                    ..LegacyRuleSet::default()
                },
            },
        },
        label: "Old School 93/94".to_string(),
        short_label: "O94".to_string(),
        description: format!(
            "Alpha through Fallen Empires, 22 restricted, 7 banned, mana burn. \
             {SET_CODE_APPROXIMATION_DISCLOSURE}"
        ),
        // The source's reprint rule admits reprints in any language with the
        // original frame and art, which includes the Collectors' Editions
        // already present in `legal_sets`.
        reprint_policy: Some(ReprintPolicy::AllowSpecialReprintSets),
        printing_fidelity: PrintingFidelity::SetCodeApproximation,
    }
}

/// Eternal Central's Old School 95 — published on the same page as an
/// incremental extension of 93/94's own lists, and built here the same way, so
/// the shared base can never drift between the two.
///
/// Adds five sets (Fourth Edition, Ice Age, Chronicles, Renaissance,
/// Homelands), two restricted cards (Demonic Consultation, Mana Crypt) and two
/// banned cards (Amulet of Quoz, Timmerian Fiends) — the last two being the
/// remaining CR 407.3 ante cards, which this era's pool newly contains.
///
/// Everything else is inherited verbatim, including `mana_burn: Obsolete`, so
/// this preset became selectable alongside its base in Phase 2b.
pub fn old_school_95() -> CustomFormatDef {
    let mut def = old_school_93_94();

    // A registry-stable id of its own — inheriting the base's would make two
    // presets indistinguishable to `GameFormat::Custom(id)`.
    def.rules.id = OLD_SCHOOL_95_ID;

    // `get_or_insert_with` rather than unwrapping: `legal_sets` is
    // `Option<Vec<_>>`, and while the base always sets `Some(..)` (a
    // pool-restricting preset never leaves it `None`), extending in place stays
    // correct if that invariant ever changes rather than assuming it silently.
    def.rules
        .legality
        .legal_sets
        .get_or_insert_with(Vec::new)
        .extend(set_codes(&["4ED", "ICE", "CHR", "REN", "HML"]));
    def.rules
        .legality
        .restricted
        .extend(card_names(&["Demonic Consultation", "Mana Crypt"]));
    def.rules
        .legality
        .banned
        .extend(card_names(&["Amulet of Quoz", "Timmerian Fiends"]));

    def.label = "Old School 95".to_string();
    def.short_label = "O95".to_string();
    def.description = format!(
        "Old School 93/94 plus Fourth Edition through Homelands, 24 restricted, 9 banned, \
         mana burn. {SET_CODE_APPROXIMATION_DISCLOSURE}"
    );
    def
}

/// Every bundled preset CONSIDERED for registration, before either gate runs.
///
/// Split out from [`custom_format_registry`] so the two reasons a preset can
/// be absent from the registry stay distinguishable — from the outside they
/// look identical, and only one of them is the gates doing their job:
///
/// - **Listed here and filtered out** — it declares something the engine does
///   not implement yet. The gate is the mechanism, and the preset registers
///   itself the moment that changes.
/// - **Not listed here at all** — it would PASS the gates, so being listed
///   would register it. This is the only way to express a blocker that is not
///   about engine capability, which is [`swedish_old_school`]'s situation.
///
/// A test asserting only that the registry is empty cannot tell those apart,
/// and would keep passing if a preset were quietly dropped from this list.
pub fn bundled_presets() -> Vec<CustomFormatDef> {
    vec![old_school_93_94(), old_school_95()]
}

/// Authoritative list of bundled custom-format presets, filtered through
/// both registration gates.
///
/// **As of Phase 2b this returns the two Eternal Central presets** — the first
/// custom formats the engine actually offers. They were listed here from the
/// start and rejected by `passes_legacy_axis_gate` for declaring
/// `mana_burn: Obsolete`; adding `LegacyAxis::ManaBurn` to
/// [`IMPLEMENTED_LEGACY_AXES`] released both without touching either
/// constructor, which is exactly what listing-then-filtering was for.
///
/// [`swedish_old_school`] is the exception, and is deliberately NOT in this
/// list: it PASSES both gates, so listing it would register it, and CONTEXT.md
/// Open item 6 (its unconfirmed reprint-policy metadata) blocks that
/// separately. A documentation blocker has no gate to express it, so omission
/// is the only mechanism — see that constructor.
pub fn custom_format_registry() -> Vec<CustomFormatDef> {
    let presets = bundled_presets();
    assert_no_lobby_save_sentinel_collision(&presets);
    presets
        .into_iter()
        .filter(|def| {
            passes_legacy_axis_gate(&def.rules.legality.legacy) && passes_reprint_fidelity_gate(def)
        })
        .collect()
}
