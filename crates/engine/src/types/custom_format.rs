//! Schema for engine-validated custom formats. Phase 1a: types +
//! validation + registration gates only. No behavior is wired into the
//! engine yet — `custom_format_registry()` is a stub returning
//! `Vec::new()`, and `IMPLEMENTED_LEGACY_AXES` is empty. Later phases
//! (2a/2b/2cd) populate the registry with real presets and wire
//! `LegacyRuleSet`'s axes into engine behavior (mana pool cleanup, combat
//! damage step, etc.).

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

        let structural = StructuralRules {
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
        };
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
}

/// Axes of `LegacyRuleSet` the engine actually enforces at runtime. Empty in
/// Phase 1a; later phases populate this as each axis's behavior is wired in.
pub const IMPLEMENTED_LEGACY_AXES: &[LegacyAxis] = &[];

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
/// legend rule's scope, Wish reach) that may not be built yet, so declaring
/// one the engine does not implement silently misrepresents how the game will
/// actually play.
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

/// Authoritative list of bundled custom-format presets, filtered through
/// both registration gates. Empty in Phase 1a — no presets exist until a
/// later phase introduces them.
pub fn custom_format_registry() -> Vec<CustomFormatDef> {
    let presets: Vec<CustomFormatDef> = Vec::new();
    assert_no_lobby_save_sentinel_collision(&presets);
    presets
        .into_iter()
        .filter(|def| {
            passes_legacy_axis_gate(&def.rules.legality.legacy) && passes_reprint_fidelity_gate(def)
        })
        .collect()
}
