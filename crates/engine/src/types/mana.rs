use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

impl ManaColor {
    /// All five colors in canonical WUBRG order.
    pub const ALL: [ManaColor; 5] = [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ];
}

impl FromStr for ManaColor {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "White" => Ok(Self::White),
            "Blue" => Ok(Self::Blue),
            "Black" => Ok(Self::Black),
            "Red" => Ok(Self::Red),
            "Green" => Ok(Self::Green),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaType {
    White,
    Blue,
    Black,
    Red,
    Green,
    Colorless,
}

/// Display-layer projection of `ManaProduction` — typed pip descriptors the
/// frontend renders verbatim. One variant per `ManaProduction` axis so no
/// information is lost on the wire (e.g., colorless producers must surface as
/// a `Colorless` pip rather than an empty `Vec<ManaColor>`).
///
/// The frontend treats this as opaque display data; all derivation lives in
/// the engine. Per the "build for the class" rule, every `ManaProduction`
/// variant maps to a `ManaPip` here so future variants force an exhaustive
/// `match` update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ManaPip {
    /// CR 106.1a: A specific color of mana ({W}, {U}, {B}, {R}, {G}).
    Color(ManaColor),
    /// CR 106.1b: Colorless mana ({C}).
    Colorless,
    /// CR 106.4: Producer chooses one color from the listed set, then yields N
    /// of that color. When the set is all five colors this represents
    /// "any color" (City of Brass); the frontend may special-case the 5-of-5
    /// case visually.
    OneOfColors(Vec<ManaColor>),
    /// CR 106.4: Producer assigns each unit independently across the listed
    /// color set (e.g., Cascading Cataracts). Same axis as `OneOfColors` but
    /// per-unit choice.
    CombinationOfColors(Vec<ManaColor>),
    /// CR 903.4: Producer adds one mana of any color in the controller's
    /// commander color identity (Command Tower, Path of Ancestry). The frontend
    /// resolves the pip set against the controller's `commander_color_identity`.
    AnyInCommandersIdentity,
}

/// Lightweight descriptor of the spell being paid for.
/// Used by `ManaRestriction::allows_spell` to decide whether restricted mana
/// may be spent on a given spell.
#[derive(Debug, Clone, Default)]
pub struct SpellMeta {
    /// Supertype and core type names (e.g., "Legendary", "Creature", "Instant")
    /// used by type-word spend restrictions.
    pub types: Vec<String>,
    /// Subtypes (e.g., "Elf", "Goblin") — case-insensitive matching.
    pub subtypes: Vec<String>,
    /// Effective keyword classes on the spell while being cast.
    pub keyword_kinds: Vec<KeywordKind>,
    /// Zone the spell is being cast from.
    pub cast_from_zone: Option<crate::types::zones::Zone>,
}

/// CR 106.6: Context for a mana-payment decision. Distinguishes "paying for a
/// spell being cast" from "paying for an ability being activated" so the
/// restriction check can route through `allows_spell` vs `allows_activation`.
///
/// Casting-restricted mana (e.g., "creature-spell-only") must reject ability
/// activations; activation-restricted mana (e.g., "activate abilities only")
/// must reject spell casts. Using the correct variant per payment site is the
/// single authority that enforces this bifurcation.
#[derive(Debug, Clone, Copy)]
pub enum PaymentContext<'a> {
    /// Payment for a spell being cast — consult `allows_spell`.
    Spell(&'a SpellMeta),
    /// Payment for an activated ability — consult `allows_activation` using
    /// the source permanent's core types and subtypes.
    Activation {
        source_types: &'a [String],
        source_subtypes: &'a [String],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaRestriction {
    /// "Spend this mana only to cast creature spells" / "only to cast artifact spells".
    OnlyForSpellType(String),
    /// "Spend this mana only to cast a creature spell of the chosen type."
    /// The `String` is the chosen creature type (e.g., "Elf").
    OnlyForCreatureType(String),
    /// CR 106.6: "Spend this mana only to cast creature spells or activate abilities of creatures."
    /// Allows spending for spells of the type (checked via `allows_spell`) OR for ability
    /// activations on permanents of the type (checked via `allows_activation`).
    OnlyForTypeSpellsOrAbilities(String),
    /// "Spend this mana only to activate abilities."
    /// Cannot be used for casting spells — activation-only.
    OnlyForActivation,
    /// "Spend this mana only on costs that include {X}."
    /// Only permits spending on spells or abilities with {X} in their cost.
    OnlyForXCosts,
    /// "Spend this mana only to cast spells with flashback."
    OnlyForSpellWithKeywordKind(KeywordKind),
    /// "Spend this mana only to cast spells with flashback from a graveyard."
    OnlyForSpellWithKeywordKindFromZone(KeywordKind, crate::types::zones::Zone),
}

impl ManaRestriction {
    /// Returns `true` if this restriction permits spending mana on the given spell.
    pub fn allows_spell(&self, meta: &SpellMeta) -> bool {
        match self {
            ManaRestriction::OnlyForSpellType(required_type) => meta
                .types
                .iter()
                .any(|t| t.eq_ignore_ascii_case(required_type)),
            ManaRestriction::OnlyForCreatureType(required_subtype) => {
                // Must be a creature spell AND have the required subtype
                let is_creature = meta
                    .types
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case("Creature"));
                let has_subtype = meta
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(required_subtype));
                is_creature && has_subtype
            }
            // CR 106.6: The spell-casting half of the OR — allows if the spell has the
            // required type, consulting both core card types (Creature, Instant, ...)
            // and subtypes (Elemental, Goblin, ...). Flamebraider's "Elemental" names
            // a creature subtype; "Artifact" would name a core type. The check treats
            // both buckets uniformly because Oracle text doesn't distinguish the two.
            ManaRestriction::OnlyForTypeSpellsOrAbilities(required_type) => meta
                .types
                .iter()
                .chain(meta.subtypes.iter())
                .any(|t| t.eq_ignore_ascii_case(required_type)),
            // Activation-only mana cannot be used to cast spells.
            ManaRestriction::OnlyForActivation => false,
            // CR 106.12: X-cost restriction — conservatively disallow for spells.
            // Full X-cost detection requires ManaCost inspection at the call site.
            ManaRestriction::OnlyForXCosts => false,
            ManaRestriction::OnlyForSpellWithKeywordKind(required_keyword) => {
                meta.keyword_kinds.contains(required_keyword)
            }
            ManaRestriction::OnlyForSpellWithKeywordKindFromZone(
                required_keyword,
                required_zone,
            ) => {
                meta.keyword_kinds.contains(required_keyword)
                    && meta.cast_from_zone == Some(*required_zone)
            }
        }
    }

    /// Returns `true` if this restriction permits spending mana to activate an ability
    /// on a permanent whose core types include `source_types` and subtypes include
    /// `source_subtypes`.
    /// CR 106.6: Used for "or activate abilities of creatures" restrictions.
    pub fn allows_activation(&self, source_types: &[String], source_subtypes: &[String]) -> bool {
        match self {
            // Spell-only restrictions don't permit ability activation.
            ManaRestriction::OnlyForSpellType(_)
            | ManaRestriction::OnlyForCreatureType(_)
            | ManaRestriction::OnlyForSpellWithKeywordKind(_)
            | ManaRestriction::OnlyForSpellWithKeywordKindFromZone(_, _) => false,
            // CR 106.6: The ability-activation half of the OR. "Elemental sources"
            // includes objects with creature type Elemental — consult subtypes too.
            ManaRestriction::OnlyForTypeSpellsOrAbilities(required_type) => source_types
                .iter()
                .chain(source_subtypes.iter())
                .any(|t| t.eq_ignore_ascii_case(required_type)),
            // Activation-only mana always allows ability activation.
            ManaRestriction::OnlyForActivation => true,
            // X-cost mana can be used for abilities with {X} in their cost.
            // TODO: Check if the ability has {X} in its cost once that data is available.
            ManaRestriction::OnlyForXCosts => false,
        }
    }

    /// CR 106.6: Unified dispatch — use the spell half of a restriction for
    /// spell payments, the activation half for ability payments. Every
    /// runtime payment site must flow through this method so the two halves
    /// stay in lockstep (single authority for restriction enforcement).
    pub fn allows(&self, ctx: &PaymentContext<'_>) -> bool {
        match ctx {
            PaymentContext::Spell(meta) => self.allows_spell(meta),
            PaymentContext::Activation {
                source_types,
                source_subtypes,
            } => self.allows_activation(source_types, source_subtypes),
        }
    }
}

/// CR 106.6: Additional effect that the mana confers upon the spell it is spent on.
/// E.g., "that spell can't be countered" (Cavern of Souls, Delighted Halfling).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaSpellGrant {
    /// The spell cast with this mana can't be countered.
    CantBeCountered,
    /// CR 106.6 + CR 702: If the spell this mana is spent on satisfies
    /// `restriction`, grant it `keyword` until end of turn.
    AddKeywordUntilEndOfTurn {
        keyword: Keyword,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restriction: Option<ManaRestriction>,
    },
}

/// When mana expires — controls lifecycle beyond the normal CR 500.4 phase drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaExpiry {
    /// Mana persists through combat steps but drains at EndCombat → PostCombatMain.
    /// Used by Firebending and similar "mana lasts within combat" mechanics.
    EndOfCombat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaUnit {
    pub color: ManaType,
    pub source_id: ObjectId,
    pub snow: bool,
    pub restrictions: Vec<ManaRestriction>,
    /// CR 106.6: Properties granted to the spell this mana is spent on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<ManaSpellGrant>,
    /// When set, this mana survives normal phase-transition drains until the
    /// specified expiry condition is met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<ManaExpiry>,
}

impl ManaUnit {
    /// Construct a standard mana unit with no expiry.
    pub fn new(
        color: ManaType,
        source_id: ObjectId,
        snow: bool,
        restrictions: Vec<ManaRestriction>,
    ) -> Self {
        Self {
            color,
            source_id,
            snow,
            restrictions,
            grants: Vec::new(),
            expiry: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaCostShard {
    // Basic colored
    White,
    Blue,
    Black,
    Red,
    Green,
    // Special
    Colorless,
    Snow,
    X,
    // Hybrid (10 pairs)
    WhiteBlue,
    WhiteBlack,
    BlueBlack,
    BlueRed,
    BlackRed,
    BlackGreen,
    RedWhite,
    RedGreen,
    GreenWhite,
    GreenBlue,
    // Two-generic hybrid (5)
    TwoWhite,
    TwoBlue,
    TwoBlack,
    TwoRed,
    TwoGreen,
    // Phyrexian (5)
    PhyrexianWhite,
    PhyrexianBlue,
    PhyrexianBlack,
    PhyrexianRed,
    PhyrexianGreen,
    // Hybrid phyrexian (10)
    PhyrexianWhiteBlue,
    PhyrexianWhiteBlack,
    PhyrexianBlueBlack,
    PhyrexianBlueRed,
    PhyrexianBlackRed,
    PhyrexianBlackGreen,
    PhyrexianRedWhite,
    PhyrexianRedGreen,
    PhyrexianGreenWhite,
    PhyrexianGreenBlue,
    // Colorless hybrid (5)
    ColorlessWhite,
    ColorlessBlue,
    ColorlessBlack,
    ColorlessRed,
    ColorlessGreen,
}

impl ManaCostShard {
    /// Returns true if this shard contributes to devotion for the given color.
    /// CR 700.5: Each mana symbol that is or contains the color counts.
    /// Hybrid symbols count toward each of their colors. A single hybrid symbol
    /// contributes 1 to multi-color devotion (not once per color).
    pub fn contributes_to(&self, color: ManaColor) -> bool {
        match color {
            ManaColor::White => matches!(
                self,
                Self::White
                    | Self::WhiteBlue
                    | Self::WhiteBlack
                    | Self::RedWhite
                    | Self::GreenWhite
                    | Self::TwoWhite
                    | Self::PhyrexianWhite
                    | Self::PhyrexianWhiteBlue
                    | Self::PhyrexianWhiteBlack
                    | Self::PhyrexianRedWhite
                    | Self::PhyrexianGreenWhite
                    | Self::ColorlessWhite
            ),
            ManaColor::Blue => matches!(
                self,
                Self::Blue
                    | Self::WhiteBlue
                    | Self::BlueBlack
                    | Self::BlueRed
                    | Self::GreenBlue
                    | Self::TwoBlue
                    | Self::PhyrexianBlue
                    | Self::PhyrexianWhiteBlue
                    | Self::PhyrexianBlueBlack
                    | Self::PhyrexianBlueRed
                    | Self::PhyrexianGreenBlue
                    | Self::ColorlessBlue
            ),
            ManaColor::Black => matches!(
                self,
                Self::Black
                    | Self::WhiteBlack
                    | Self::BlueBlack
                    | Self::BlackRed
                    | Self::BlackGreen
                    | Self::TwoBlack
                    | Self::PhyrexianBlack
                    | Self::PhyrexianWhiteBlack
                    | Self::PhyrexianBlueBlack
                    | Self::PhyrexianBlackRed
                    | Self::PhyrexianBlackGreen
                    | Self::ColorlessBlack
            ),
            ManaColor::Red => matches!(
                self,
                Self::Red
                    | Self::BlueRed
                    | Self::BlackRed
                    | Self::RedWhite
                    | Self::RedGreen
                    | Self::TwoRed
                    | Self::PhyrexianRed
                    | Self::PhyrexianBlueRed
                    | Self::PhyrexianBlackRed
                    | Self::PhyrexianRedWhite
                    | Self::PhyrexianRedGreen
                    | Self::ColorlessRed
            ),
            ManaColor::Green => matches!(
                self,
                Self::Green
                    | Self::BlackGreen
                    | Self::RedGreen
                    | Self::GreenWhite
                    | Self::GreenBlue
                    | Self::TwoGreen
                    | Self::PhyrexianGreen
                    | Self::PhyrexianBlackGreen
                    | Self::PhyrexianRedGreen
                    | Self::PhyrexianGreenWhite
                    | Self::PhyrexianGreenBlue
                    | Self::ColorlessGreen
            ),
        }
    }

    /// CR 202.3f: Returns the mana value contribution of this shard.
    /// For hybrid symbols, uses the largest component.
    pub fn mana_value_contribution(&self) -> u32 {
        match self {
            // Two-generic hybrid: max(2, 1) = 2 (CR 202.3f)
            Self::TwoWhite | Self::TwoBlue | Self::TwoBlack
            | Self::TwoRed | Self::TwoGreen => 2,
            // X contributes 0 when not on the stack (CR 202.3e)
            Self::X => 0,
            // All other shards contribute 1:
            // Basic colored (CR 202.3a)
            Self::White | Self::Blue | Self::Black | Self::Red | Self::Green
            // Colorless, Snow
            | Self::Colorless | Self::Snow
            // Two-color hybrid: max(1, 1) = 1 (CR 202.3f)
            | Self::WhiteBlue | Self::WhiteBlack | Self::BlueBlack | Self::BlueRed
            | Self::BlackRed | Self::BlackGreen | Self::RedWhite | Self::RedGreen
            | Self::GreenWhite | Self::GreenBlue
            // Phyrexian: 1 mana or 2 life = mana value 1 (CR 202.3g)
            | Self::PhyrexianWhite | Self::PhyrexianBlue | Self::PhyrexianBlack
            | Self::PhyrexianRed | Self::PhyrexianGreen
            // Phyrexian hybrid: max(1, 1) = 1 (CR 202.3f + CR 202.3g)
            | Self::PhyrexianWhiteBlue | Self::PhyrexianWhiteBlack
            | Self::PhyrexianBlueBlack | Self::PhyrexianBlueRed
            | Self::PhyrexianBlackRed | Self::PhyrexianBlackGreen
            | Self::PhyrexianRedWhite | Self::PhyrexianRedGreen
            | Self::PhyrexianGreenWhite | Self::PhyrexianGreenBlue
            // Colorless hybrid: max(1, 1) = 1 (CR 202.3f)
            | Self::ColorlessWhite | Self::ColorlessBlue | Self::ColorlessBlack
            | Self::ColorlessRed | Self::ColorlessGreen => 1,
        }
    }
}

impl FromStr for ManaCostShard {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "W" => Ok(ManaCostShard::White),
            "U" => Ok(ManaCostShard::Blue),
            "B" => Ok(ManaCostShard::Black),
            "R" => Ok(ManaCostShard::Red),
            "G" => Ok(ManaCostShard::Green),
            "C" => Ok(ManaCostShard::Colorless),
            "S" => Ok(ManaCostShard::Snow),
            "X" => Ok(ManaCostShard::X),
            // Hybrid
            "W/U" => Ok(ManaCostShard::WhiteBlue),
            "W/B" => Ok(ManaCostShard::WhiteBlack),
            "U/B" => Ok(ManaCostShard::BlueBlack),
            "U/R" => Ok(ManaCostShard::BlueRed),
            "B/R" => Ok(ManaCostShard::BlackRed),
            "B/G" => Ok(ManaCostShard::BlackGreen),
            "R/W" => Ok(ManaCostShard::RedWhite),
            "R/G" => Ok(ManaCostShard::RedGreen),
            "G/W" => Ok(ManaCostShard::GreenWhite),
            "G/U" => Ok(ManaCostShard::GreenBlue),
            // Two-generic hybrid
            "2/W" => Ok(ManaCostShard::TwoWhite),
            "2/U" => Ok(ManaCostShard::TwoBlue),
            "2/B" => Ok(ManaCostShard::TwoBlack),
            "2/R" => Ok(ManaCostShard::TwoRed),
            "2/G" => Ok(ManaCostShard::TwoGreen),
            // Phyrexian
            "W/P" => Ok(ManaCostShard::PhyrexianWhite),
            "U/P" => Ok(ManaCostShard::PhyrexianBlue),
            "B/P" => Ok(ManaCostShard::PhyrexianBlack),
            "R/P" => Ok(ManaCostShard::PhyrexianRed),
            "G/P" => Ok(ManaCostShard::PhyrexianGreen),
            // Hybrid phyrexian
            "W/U/P" => Ok(ManaCostShard::PhyrexianWhiteBlue),
            "W/B/P" => Ok(ManaCostShard::PhyrexianWhiteBlack),
            "U/B/P" => Ok(ManaCostShard::PhyrexianBlueBlack),
            "U/R/P" => Ok(ManaCostShard::PhyrexianBlueRed),
            "B/R/P" => Ok(ManaCostShard::PhyrexianBlackRed),
            "B/G/P" => Ok(ManaCostShard::PhyrexianBlackGreen),
            "R/W/P" => Ok(ManaCostShard::PhyrexianRedWhite),
            "R/G/P" => Ok(ManaCostShard::PhyrexianRedGreen),
            "G/W/P" => Ok(ManaCostShard::PhyrexianGreenWhite),
            "G/U/P" => Ok(ManaCostShard::PhyrexianGreenBlue),
            // Colorless hybrid
            "C/W" => Ok(ManaCostShard::ColorlessWhite),
            "C/U" => Ok(ManaCostShard::ColorlessBlue),
            "C/B" => Ok(ManaCostShard::ColorlessBlack),
            "C/R" => Ok(ManaCostShard::ColorlessRed),
            "C/G" => Ok(ManaCostShard::ColorlessGreen),
            _ => Err(format!("Unknown mana cost shard: {}", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManaCost {
    NoCost,
    Cost {
        shards: Vec<ManaCostShard>,
        generic: u32,
    },
    /// The card's own mana cost (used for "the flashback cost is equal to its mana cost").
    SelfManaCost,
}

impl ManaCost {
    pub fn zero() -> Self {
        ManaCost::Cost {
            shards: Vec::new(),
            generic: 0,
        }
    }

    /// Create a cost with only generic mana (e.g., {3}).
    pub fn generic(amount: u32) -> Self {
        ManaCost::Cost {
            shards: Vec::new(),
            generic: amount,
        }
    }

    /// CR 202.3: Calculate the mana value (converted mana cost) of this cost.
    /// CR 202.3e: X in a mana cost contributes 0 when not on the stack.
    /// CR 202.3f: For hybrid symbols, use the largest component.
    pub fn mana_value(&self) -> u32 {
        match self {
            ManaCost::NoCost | ManaCost::SelfManaCost => 0,
            ManaCost::Cost { shards, generic } => {
                let shard_total: u32 = shards.iter().map(|s| s.mana_value_contribution()).sum();
                shard_total + generic
            }
        }
    }

    /// CR 508.1h + CR 509.1d: Aggregate this cost with another cost, producing a
    /// combined "locked in" total. Used for combat-tax aggregation where multiple
    /// UnlessPay static abilities apply to the same attacker/blocker (e.g., two
    /// Ghostly Prisons on the battlefield).
    ///
    /// Semantics: generic mana accumulates, shards are concatenated verbatim. The
    /// result is `NoCost` only when both operands are `NoCost`. `SelfManaCost` is
    /// never produced by combat tax aggregation; if either operand is
    /// `SelfManaCost` the caller is misusing the API, so we treat it as
    /// zero-contribution (no shards, no generic).
    pub fn plus(&self, other: &ManaCost) -> ManaCost {
        let (a_shards, a_generic) = match self {
            ManaCost::Cost { shards, generic } => (shards.as_slice(), *generic),
            _ => (&[] as &[ManaCostShard], 0),
        };
        let (b_shards, b_generic) = match other {
            ManaCost::Cost { shards, generic } => (shards.as_slice(), *generic),
            _ => (&[] as &[ManaCostShard], 0),
        };
        if a_shards.is_empty() && b_shards.is_empty() && a_generic == 0 && b_generic == 0 {
            return ManaCost::zero();
        }
        let mut shards = Vec::with_capacity(a_shards.len() + b_shards.len());
        shards.extend_from_slice(a_shards);
        shards.extend_from_slice(b_shards);
        ManaCost::Cost {
            shards,
            generic: a_generic + b_generic,
        }
    }

    /// CR 508.1h: Scale this cost by an integer multiplier, as used for
    /// "for each of those creatures" per-attacker aggregation on combat taxes.
    /// `factor == 0` produces `ManaCost::zero()`; `factor == 1` returns a clone.
    /// Shards are repeated `factor` times, generic mana is multiplied.
    pub fn scaled(&self, factor: u32) -> ManaCost {
        if factor == 0 {
            return ManaCost::zero();
        }
        match self {
            ManaCost::Cost { shards, generic } => {
                let mut scaled_shards = Vec::with_capacity(shards.len() * factor as usize);
                for _ in 0..factor {
                    scaled_shards.extend_from_slice(shards);
                }
                ManaCost::Cost {
                    shards: scaled_shards,
                    generic: generic * factor,
                }
            }
            other => other.clone(),
        }
    }

    /// CR 107.1b + CR 601.2f: Replace every `ManaCostShard::X` in this cost with
    /// `value * x_count` generic mana. Called after the caster commits to an X
    /// value, so mana payment sees a concrete cost with no symbolic X remaining.
    /// Multiple X shards (e.g. `{X}{X}`) each contribute `value` generic.
    pub fn concretize_x(&mut self, value: u32) {
        if let ManaCost::Cost { shards, generic } = self {
            let x_count = shards
                .iter()
                .filter(|s| matches!(s, ManaCostShard::X))
                .count();
            if x_count == 0 {
                return;
            }
            shards.retain(|s| !matches!(s, ManaCostShard::X));
            *generic += value * x_count as u32;
        }
    }
}

impl Default for ManaCost {
    fn default() -> Self {
        ManaCost::zero()
    }
}

/// CR 601.2h: Per-color tally of mana spent to cast an object.
/// Populated during cost payment (see `casting::pay_mana_cost`) and
/// consumed by trigger conditions like Adamant (CR 207.2c) and any
/// future "if at least N of [color] was spent" checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColoredManaCount {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub white: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub blue: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub black: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub red: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub green: u32,
}

fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl ColoredManaCount {
    pub fn get(&self, color: ManaColor) -> u32 {
        match color {
            ManaColor::White => self.white,
            ManaColor::Blue => self.blue,
            ManaColor::Black => self.black,
            ManaColor::Red => self.red,
            ManaColor::Green => self.green,
        }
    }

    pub fn add(&mut self, color: ManaColor, n: u32) {
        match color {
            ManaColor::White => self.white += n,
            ManaColor::Blue => self.blue += n,
            ManaColor::Black => self.black += n,
            ManaColor::Red => self.red += n,
            ManaColor::Green => self.green += n,
        }
    }

    /// Tally a ManaUnit's color into the count. Colorless mana is ignored
    /// (Adamant and related checks only care about the five colors, per
    /// CR 207.2c's "of [color]" wording).
    pub fn add_unit(&mut self, unit: &ManaUnit) {
        let color = match unit.color {
            ManaType::White => ManaColor::White,
            ManaType::Blue => ManaColor::Blue,
            ManaType::Black => ManaColor::Black,
            ManaType::Red => ManaColor::Red,
            ManaType::Green => ManaColor::Green,
            ManaType::Colorless => return,
        };
        self.add(color, 1);
    }

    pub fn is_empty(&self) -> bool {
        self.white == 0 && self.blue == 0 && self.black == 0 && self.red == 0 && self.green == 0
    }

    /// CR 202.2: Number of distinct colors with a non-zero tally.
    /// Used by self-scoped spent-mana quantities for "X is the number of colors
    /// of mana spent to cast it" patterns (Wildgrowth Archaic family).
    pub fn distinct_colors(&self) -> usize {
        ManaColor::ALL.iter().filter(|c| self.get(**c) > 0).count()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaPool {
    pub mana: Vec<ManaUnit>,
}

impl ManaPool {
    pub fn add(&mut self, unit: ManaUnit) {
        self.mana.push(unit);
    }

    pub fn count_color(&self, color: ManaType) -> usize {
        self.mana.iter().filter(|m| m.color == color).count()
    }

    pub fn total(&self) -> usize {
        self.mana.len()
    }

    pub fn clear(&mut self) {
        self.mana.clear();
    }

    /// CR 500.4: Clear mana on phase transition, retaining combat-expiry mana
    /// while still within combat phases.
    pub fn clear_step_transition(&mut self, in_combat: bool) {
        if in_combat {
            // Retain mana with EndOfCombat expiry; drain everything else
            self.mana
                .retain(|u| u.expiry == Some(ManaExpiry::EndOfCombat));
        } else {
            // Leaving combat or non-combat transition: drain everything
            self.mana.clear();
        }
    }

    /// Remove all mana units produced by the given source.
    /// Returns the number of units removed (zero if mana was already spent).
    pub fn remove_from_source(&mut self, source_id: ObjectId) -> usize {
        let before = self.mana.len();
        self.mana.retain(|u| u.source_id != source_id);
        before - self.mana.len()
    }

    /// CR 702.139a: Remove `count` unrestricted mana of any type from the pool (generic cost).
    /// Skips mana with `ManaRestriction`s since the companion special action is not a spell.
    /// Returns true if enough eligible mana was available and removed, false otherwise.
    pub fn spend_generic(&mut self, count: usize) -> bool {
        let unrestricted_count = self
            .mana
            .iter()
            .filter(|m| m.restrictions.is_empty())
            .count();
        if unrestricted_count < count {
            return false;
        }
        // Remove unrestricted mana, preferring from the end for efficiency
        let mut remaining = count;
        self.mana.retain(|m| {
            if remaining == 0 {
                return true;
            }
            if m.restrictions.is_empty() {
                remaining -= 1;
                false
            } else {
                true
            }
        });
        true
    }

    pub fn spend(&mut self, color: ManaType) -> Option<ManaUnit> {
        if let Some(pos) = self.mana.iter().position(|m| m.color == color) {
            Some(self.mana.swap_remove(pos))
        } else {
            None
        }
    }

    /// Spend one mana of the given color that is eligible for the given payment context.
    ///
    /// CR 106.6: Prefers unrestricted mana first, then falls back to restricted mana
    /// whose restrictions all allow the payment (spell cast or ability activation,
    /// per the `PaymentContext` variant). Mana with restrictions that don't match is
    /// never spent.
    pub fn spend_for(&mut self, color: ManaType, ctx: &PaymentContext<'_>) -> Option<ManaUnit> {
        // First pass: prefer unrestricted mana of this color
        if let Some(pos) = self
            .mana
            .iter()
            .position(|m| m.color == color && m.restrictions.is_empty())
        {
            return Some(self.mana.swap_remove(pos));
        }
        // Second pass: restricted mana that allows this payment context
        if let Some(pos) = self.mana.iter().position(|m| {
            m.color == color
                && !m.restrictions.is_empty()
                && m.restrictions.iter().all(|r| r.allows(ctx))
        }) {
            return Some(self.mana.swap_remove(pos));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_unit(color: ManaType) -> ManaUnit {
        ManaUnit::new(color, ObjectId(1), false, Vec::new())
    }

    fn make_restricted_unit(
        color: ManaType,
        source: ObjectId,
        restrictions: Vec<ManaRestriction>,
    ) -> ManaUnit {
        ManaUnit::new(color, source, false, restrictions)
    }

    #[test]
    fn mana_color_serializes_as_string() {
        let color = ManaColor::White;
        let json = serde_json::to_value(color).unwrap();
        assert_eq!(json, "White");
    }

    #[test]
    fn all_mana_colors_serialize() {
        let colors = [
            (ManaColor::White, "White"),
            (ManaColor::Blue, "Blue"),
            (ManaColor::Black, "Black"),
            (ManaColor::Red, "Red"),
            (ManaColor::Green, "Green"),
        ];
        for (color, expected) in colors {
            let json = serde_json::to_value(color).unwrap();
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn mana_pool_default_is_empty() {
        let pool = ManaPool::default();
        assert_eq!(pool.total(), 0);
    }

    #[test]
    fn mana_pool_add_increases_count() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        pool.add(make_unit(ManaType::Blue));
        pool.add(make_unit(ManaType::Blue));
        assert_eq!(pool.count_color(ManaType::Blue), 3);
        assert_eq!(pool.total(), 3);
    }

    #[test]
    fn mana_pool_add_multiple_colors() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::White));
        pool.add(make_unit(ManaType::White));
        pool.add(make_unit(ManaType::Red));
        pool.add(make_unit(ManaType::Green));
        pool.add(make_unit(ManaType::Green));
        pool.add(make_unit(ManaType::Green));
        assert_eq!(pool.total(), 6);
        assert_eq!(pool.count_color(ManaType::White), 2);
        assert_eq!(pool.count_color(ManaType::Red), 1);
        assert_eq!(pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn mana_pool_total_includes_colorless() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        assert_eq!(pool.total(), 5);
    }

    #[test]
    fn mana_pool_spend_removes_unit() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        pool.add(make_unit(ManaType::Red));

        let spent = pool.spend(ManaType::Blue);
        assert!(spent.is_some());
        assert_eq!(spent.unwrap().color, ManaType::Blue);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.count_color(ManaType::Blue), 0);
    }

    #[test]
    fn mana_pool_spend_returns_none_when_empty() {
        let mut pool = ManaPool::default();
        assert!(pool.spend(ManaType::Black).is_none());
    }

    #[test]
    fn mana_pool_clear_empties_pool() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::White));
        pool.add(make_unit(ManaType::Blue));
        pool.clear();
        assert_eq!(pool.total(), 0);
    }

    #[test]
    fn mana_type_includes_colorless() {
        let types = [
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
            ManaType::Colorless,
        ];
        assert_eq!(types.len(), 6);
    }

    #[test]
    fn mana_unit_tracks_source_and_snow() {
        let unit = ManaUnit::new(
            ManaType::Green,
            ObjectId(42),
            true,
            vec![ManaRestriction::OnlyForSpellType("Creature".to_string())],
        );
        assert_eq!(unit.source_id, ObjectId(42));
        assert!(unit.snow);
        assert_eq!(unit.restrictions.len(), 1);
    }

    #[test]
    fn mana_pool_serializes_and_roundtrips() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        let json = serde_json::to_string(&pool).unwrap();
        let deserialized: ManaPool = serde_json::from_str(&json).unwrap();
        assert_eq!(pool, deserialized);
    }

    #[test]
    fn restriction_allows_matching_spell_type() {
        let restriction = ManaRestriction::OnlyForSpellType("Creature".to_string());
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let instant_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let legendary_spell = SpellMeta {
            types: vec!["Legendary".to_string(), "Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        assert!(restriction.allows_spell(&creature_spell));
        assert!(!restriction.allows_spell(&instant_spell));

        let legendary_restriction = ManaRestriction::OnlyForSpellType("Legendary".to_string());
        assert!(legendary_restriction.allows_spell(&legendary_spell));
        assert!(!legendary_restriction.allows_spell(&creature_spell));
    }

    #[test]
    fn restriction_creature_type_requires_both_type_and_subtype() {
        let restriction = ManaRestriction::OnlyForCreatureType("Elf".to_string());
        let elf_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string(), "Warrior".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let elf_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        assert!(restriction.allows_spell(&elf_creature));
        assert!(!restriction.allows_spell(&goblin_creature));
        assert!(!restriction.allows_spell(&elf_instant));
    }

    #[test]
    fn spend_for_prefers_unrestricted_mana() {
        let mut pool = ManaPool::default();
        // Add restricted green, then unrestricted green
        pool.add(make_restricted_unit(
            ManaType::Green,
            ObjectId(1),
            vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
        ));
        pool.add(make_unit(ManaType::Green));

        let spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let spent = pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&spell))
            .unwrap();
        // Should prefer unrestricted mana first
        assert!(spent.restrictions.is_empty());
        assert_eq!(pool.total(), 1);
    }

    #[test]
    fn spend_for_uses_restricted_mana_when_allowed() {
        let mut pool = ManaPool::default();
        pool.add(make_restricted_unit(
            ManaType::Green,
            ObjectId(1),
            vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
        ));

        let elf_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&elf_spell))
            .is_some());
    }

    #[test]
    fn remove_from_source_removes_matching_units() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(10),
            false,
            Vec::new(),
        ));
        pool.add(ManaUnit::new(
            ManaType::Blue,
            ObjectId(20),
            false,
            Vec::new(),
        ));

        let removed = pool.remove_from_source(ObjectId(10));
        assert_eq!(removed, 2);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.count_color(ManaType::Blue), 1);
    }

    #[test]
    fn remove_from_source_returns_zero_when_no_match() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::White));
        let removed = pool.remove_from_source(ObjectId(99));
        assert_eq!(removed, 0);
        assert_eq!(pool.total(), 1);
    }

    #[test]
    fn spend_for_skips_restricted_mana_when_not_allowed() {
        let mut pool = ManaPool::default();
        pool.add(make_restricted_unit(
            ManaType::Green,
            ObjectId(1),
            vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
        ));

        let goblin_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&goblin_spell))
            .is_none());
        assert_eq!(pool.total(), 1, "Restricted mana should remain in pool");
    }

    // CR 106.6: "Spend this mana only to cast Elemental spells or activate abilities
    // of Elemental sources" — "Elemental" names a creature subtype. The restriction
    // must match against both core types and subtypes on `SpellMeta`.
    #[test]
    fn restriction_type_or_ability_allows_subtype_creature_spell() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities("Elemental".to_string());
        let elemental_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let tribal_elemental_instant = SpellMeta {
            types: vec!["Tribal".to_string(), "Instant".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let plain_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        assert!(restriction.allows_spell(&elemental_creature));
        assert!(restriction.allows_spell(&tribal_elemental_instant));
        assert!(!restriction.allows_spell(&goblin_creature));
        assert!(!restriction.allows_spell(&plain_instant));
    }

    // CR 106.6: The ability-activation half of the OR. An Elemental permanent is a
    // source whose subtypes include "Elemental"; activation must be permitted.
    #[test]
    fn restriction_type_or_ability_allows_subtype_activation() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities("Elemental".to_string());
        let elemental_creature_types = vec!["Creature".to_string()];
        let elemental_subtypes = vec!["Elemental".to_string(), "Shaman".to_string()];
        assert!(restriction.allows_activation(&elemental_creature_types, &elemental_subtypes));

        let goblin_subtypes = vec!["Goblin".to_string()];
        assert!(!restriction.allows_activation(&elemental_creature_types, &goblin_subtypes));

        // Core-type match also satisfies the check (e.g., "Artifact sources").
        let artifact_restriction =
            ManaRestriction::OnlyForTypeSpellsOrAbilities("Artifact".to_string());
        let artifact_types = vec!["Artifact".to_string()];
        let no_subtypes: Vec<String> = vec![];
        assert!(artifact_restriction.allows_activation(&artifact_types, &no_subtypes));
    }

    #[test]
    fn restriction_allows_matching_keyword_kind() {
        let restriction = ManaRestriction::OnlyForSpellWithKeywordKind(KeywordKind::Flashback);
        let flashback_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![KeywordKind::Flashback],
            cast_from_zone: None,
        };
        let normal_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        assert!(restriction.allows_spell(&flashback_spell));
        assert!(!restriction.allows_spell(&normal_spell));
    }

    #[test]
    fn mana_value_two_generic_hybrid() {
        // CR 202.3f: {2/W}{2/W}{2/W} → max(2,1) * 3 = 6
        let cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::TwoWhite,
                ManaCostShard::TwoWhite,
                ManaCostShard::TwoWhite,
            ],
            generic: 0,
        };
        assert_eq!(cost.mana_value(), 6);
    }

    #[test]
    fn mana_value_standard_hybrid() {
        // {1}{W/U}{W/U} → 1 + 1 + 1 = 3
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue, ManaCostShard::WhiteBlue],
            generic: 1,
        };
        assert_eq!(cost.mana_value(), 3);
    }

    #[test]
    fn mana_value_basic_colored() {
        // {W}{U}{B} → 3
        let cost = ManaCost::Cost {
            shards: vec![
                ManaCostShard::White,
                ManaCostShard::Blue,
                ManaCostShard::Black,
            ],
            generic: 0,
        };
        assert_eq!(cost.mana_value(), 3);
    }

    #[test]
    fn mana_value_x_contributes_zero() {
        // CR 202.3e: {X}{R} → 0 + 1 = 1
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        };
        assert_eq!(cost.mana_value(), 1);
    }

    #[test]
    fn mana_value_phyrexian() {
        // CR 202.3g: {W/P}{B/P} → 1 + 1 = 2
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianWhite, ManaCostShard::PhyrexianBlack],
            generic: 0,
        };
        assert_eq!(cost.mana_value(), 2);
    }

    #[test]
    fn test_colored_mana_count_add_unit_ignores_colorless() {
        // CR 207.2c: Adamant checks "of [color]" — colorless mana does not count
        // toward any color tally.
        let mut count = ColoredManaCount::default();
        let source = ObjectId(1);

        count.add_unit(&ManaUnit::new(ManaType::Red, source, false, vec![]));
        count.add_unit(&ManaUnit::new(ManaType::Red, source, false, vec![]));
        count.add_unit(&ManaUnit::new(ManaType::Colorless, source, false, vec![]));
        count.add_unit(&ManaUnit::new(ManaType::Colorless, source, false, vec![]));

        assert_eq!(count.get(ManaColor::Red), 2);
        assert_eq!(count.get(ManaColor::White), 0);
        assert_eq!(count.get(ManaColor::Blue), 0);
        assert_eq!(count.get(ManaColor::Black), 0);
        assert_eq!(count.get(ManaColor::Green), 0);
        assert!(!count.is_empty());

        // An all-colorless tally is considered empty for the "of [color]" check.
        let mut colorless_only = ColoredManaCount::default();
        colorless_only.add_unit(&ManaUnit::new(ManaType::Colorless, source, false, vec![]));
        assert!(colorless_only.is_empty());
    }
}
