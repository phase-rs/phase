//! Schema for engine-validated custom formats. Phase 1a: types +
//! validation + registration gates only. No behavior is wired into the
//! engine yet — `custom_format_registry()` is a stub returning
//! `Vec::new()`, and `IMPLEMENTED_LEGACY_AXES` is empty. Later phases
//! (2a/2b/2cd) populate the registry with real presets and wire
//! `LegacyRuleSet`'s axes into engine behavior (mana pool cleanup, combat
//! damage step, etc.).

use serde::{Deserialize, Serialize};

use crate::types::format::{GameFormat, RangeOfInfluenceConfig, SideboardPolicy};

/// Lightweight, `Copy`, per-`GameState` transport tag for a custom format.
/// The full ruleset never needs a registry round-trip within one game — see
/// `FormatConfig.custom_rules`, which carries the resolved `CustomFormatRules`
/// value directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CustomFormatId(pub u16);

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
            GameFormat::Commander | GameFormat::DuelCommander | GameFormat::PauperCommander => {
                Ok(Some(Self::Standard))
            }
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
    pub deck_size: u16,
    pub singleton: bool,
    pub command_zone_mode: CommandZoneMode,
    #[serde(default)]
    pub range_of_influence: Option<Box<RangeOfInfluenceConfig>>,
    pub team_based: bool,
    /// The DECLARED sideboard policy for this custom format. Not yet mirrored
    /// by a RESOLVED `FormatConfig.sideboard_policy` field — that's a later
    /// phase's widening.
    pub sideboard_policy: SideboardPolicy,
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

/// Registration gate (a): every axis a def declares as non-default must be
/// in `IMPLEMENTED_LEGACY_AXES`, or the def is rejected.
pub fn passes_legacy_axis_gate(def: &CustomFormatDef) -> bool {
    declared_legacy_axes(&def.rules.legality.legacy)
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

/// Authoritative list of bundled custom-format presets, filtered through
/// both registration gates. Empty in Phase 1a — no presets exist until a
/// later phase introduces them.
pub fn custom_format_registry() -> Vec<CustomFormatDef> {
    let presets: Vec<CustomFormatDef> = Vec::new();
    presets
        .into_iter()
        .filter(|def| passes_legacy_axis_gate(def) && passes_reprint_fidelity_gate(def))
        .collect()
}
