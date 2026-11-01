use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ability::{AbilityTag, Comparator, FilterProp, QuantityExpr, TargetFilter, TypedFilter};
use super::counter::CounterType;
use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};
use super::zones::Zone;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ManaType {
    White,
    Blue,
    Black,
    Red,
    Green,
    Colorless,
}

impl From<ManaColor> for ManaType {
    fn from(color: ManaColor) -> Self {
        match color {
            ManaColor::White => ManaType::White,
            ManaColor::Blue => ManaType::Blue,
            ManaColor::Black => ManaType::Black,
            ManaColor::Red => ManaType::Red,
            ManaColor::Green => ManaType::Green,
        }
    }
}

/// CR 106.1b: a set over the six types of mana, as a bitmask.
///
/// Used to describe which mana a single-mana cost symbol accepts
/// ([`ManaCostShard::single_mana_payment_types`]), which is what lets two costs
/// be compared by what can PAY them rather than by what they are worth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ManaTypeSet(u8);

impl ManaTypeSet {
    pub const EMPTY: Self = Self(0);

    pub const fn of(mana_type: ManaType) -> Self {
        Self(1 << mana_type as u8)
    }

    pub const fn with(self, mana_type: ManaType) -> Self {
        Self(self.0 | (1 << mana_type as u8))
    }

    pub const fn contains(self, mana_type: ManaType) -> bool {
        self.0 & (1 << mana_type as u8) != 0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every mana type in `self` is also in `other` — i.e. any single
    /// mana that satisfies `self` also satisfies `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }
}

impl FromIterator<ManaType> for ManaTypeSet {
    fn from_iter<I: IntoIterator<Item = ManaType>>(it: I) -> Self {
        it.into_iter().fold(Self::EMPTY, Self::with)
    }
}

/// CR 614.1a + CR 703.4q: What happens to an affected unspent-mana unit at the
/// CR 703.4q "any unspent mana left in a player's mana pool empties" event.
///
/// Two leaf-level actions today, both replacement effects on the same step-end
/// drop event:
/// - `Retain`: the mana doesn't empty (CR 614.6 — the loss event is replaced
///   with nothing). Upwelling, Electro, Omnath Locus of Mana, The Last Agni Kai.
/// - `Transform(type)`: the mana becomes `type` instead of emptying (CR 614.1a
///   — the loss event is replaced with a recolor). Horizon Stone, Kruphix,
///   Omnath Locus of All, Ozai.
///
/// **Sibling-cluster trip-trigger:** A third action variant only belongs here
/// if it is also a CR 703.4q step-end-empty replacement. A "Whenever you lose
/// mana, …" pattern is a *triggered* ability on the loss event (CR 603), a
/// different rule domain that warrants its own ability surface rather than
/// extending this enum. Likewise, any effect that fires at a non-step-end
/// time (e.g., on cost payment, on damage) does not belong here.
///
/// Runtime path: handlers are scanned per-player by
/// `game::turns::scan_step_end_mana_handlers` (combining
/// `battlefield_active_statics` with `transient_continuous_effects` keyed on
/// `SpecificPlayer`) and surface as `StepEndManaScanEntry` rows in
/// `state.pending_step_end_mana_handlers`. The replacement pipeline
/// (`empty_mana_pool_matcher` + the Path-A carve-out
/// `apply_empty_mana_pool_replacement` in `game::replacement`) flips
/// per-unit dispositions via the CR 616.1 player-choice surface; the final
/// pool mutation runs in `apply_empty_mana_pool_decisions`. The TCE
/// scan accepts both `Retain` and `Transform` arms — `Transform` is
/// forward-compatible for a future spell-installed transformation rider
/// (today only the printed-static path produces it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StepEndManaAction {
    Retain,
    Transform(ManaType),
}

impl std::fmt::Display for StepEndManaAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StepEndManaAction::Retain => write!(f, "Retain"),
            StepEndManaAction::Transform(t) => write!(f, "Transform({t:?})"),
        }
    }
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
    /// CR 202.3: Mana value of the spell being cast, consulted by mana-value
    /// spend restrictions (`OnlyForSpellWithManaValue`). `None` at payment
    /// sites with no associated spell mana value.
    pub mana_value: Option<u32>,
    /// CR 105.2: Number of colors of the spell being cast, consulted by
    /// color-count spend restrictions (`OnlyForSpellWithColorCount`). `None` at
    /// payment sites with no associated spell.
    pub color_count: Option<u32>,
    /// CR 105.2: The spell's exact colors, consulted by color-specific spend
    /// restrictions. This is distinct from `color_count`: a monocolored spell
    /// must also have the particular required color to satisfy a rider such as
    /// "monocolored spells of that color."
    pub colors: Vec<ManaColor>,
    /// CR 107.3 + CR 202.3e: Whether the spell's printed mana cost contains an
    /// `{X}` symbol. Consulted by the "with {X} in their mana costs" disjunct of
    /// MV/X spend restrictions (`OnlyForSpellMatchingCostCriteria`). `false` at
    /// payment sites with no associated spell, or when the spell has no `{X}`.
    pub has_x_in_cost: bool,
    /// CR 708.4: Whether the spell is being CAST FACE DOWN (morph/disguise/cloak
    /// cast as a 2/2 face-down creature). Consulted by the "cast face-down
    /// spells" spend restriction ([`ManaRestriction::OnlyForFaceDownSpell`],
    /// Tin Street Gossip). `false` at payment sites with no associated spell or
    /// for a normal face-up cast.
    ///
    /// This is "being cast face down", NOT "the object is currently face down" —
    /// the two differ for exile/library concealment. Foretell (CR 702.143a),
    /// hideaway, and similar effects set `obj.face_down = true` while the card
    /// waits in exile, yet that card is CAST FACE UP (CR 702.143c: a foretold card
    /// is cast face up "even if it was cast for a cost other than a foretell
    /// cost"). `game::casting::build_spell_meta` therefore sources this field from
    /// the cast's face-down intent, hardcoded `false` today — never from raw
    /// `obj.face_down` — so a foretold/hideaway cast is correctly reported as
    /// face-up. No engine path casts a spell face down (CR 708.4 face-down play,
    /// `PlayFaceDown` → `game::morph::play_face_down`, enters the battlefield via
    /// the zone pipeline and charges no mana, building no spell-payment context),
    /// so the field is fail-closed: never `true` at a real `PaymentContext::Spell`
    /// site. It is wired for forward compatibility — see
    /// [`ManaRestriction::OnlyForFaceDownSpell`] for the contract.
    pub is_face_down: bool,
    /// CR 601.2g / CR 118.3: the spell carries a `CastingRestriction::CantSpendMana`
    /// ("You can't spend mana to cast this spell" — Hogaak, Arisen Necropolis).
    /// No mana may leave the pool, so the entire mana cost must be met by
    /// alternative payments (convoke/delve). The pool-payment authority
    /// (`game::casting::pay_mana_cost_from_pool_with_choices`) withholds all real
    /// (non-convoke) pool units for the duration of the payment when this is set.
    pub cant_spend_mana: bool,
    /// CR 601.2b / CR 601.2h: "Spend only [colors] mana on X."
    /// When present, `spend_only_on_x_generic_count` of the generic mana pips must be paid
    /// using only the specified `spend_only_on_x_colors`.
    pub spend_only_on_x_colors: Option<Vec<ManaColor>>,
    pub spend_only_on_x_generic_count: u32,
}

/// CR 106.6: Context for a mana-payment decision. Distinguishes "paying for a
/// spell being cast", "paying for an ability being activated", and paying
/// costs during effect resolution so the restriction check can route through
/// the correct rules category.
///
/// Casting-restricted mana (e.g., "creature-spell-only") must reject ability
/// activations; activation-restricted mana (e.g., "activate abilities only")
/// must reject spell casts and resolution-time effect costs. Using the correct
/// variant per payment site is the single authority that enforces this
/// bifurcation.
#[derive(Debug, Clone, Copy)]
pub enum PaymentContext<'a> {
    /// Payment for a spell being cast — consult `allows_spell`.
    Spell(&'a SpellMeta),
    /// Payment for an activated ability — consult `allows_activation` using
    /// the source permanent's core types and subtypes plus the ability's
    /// keyword tag (CR 106.6, for tag-scoped restrictions like Quinjet's).
    Activation {
        source_types: &'a [String],
        source_subtypes: &'a [String],
        ability_tag: Option<AbilityTag>,
        /// CR 106.6: An ability's own activation-cost rider can constrain the
        /// actual mana type paid, independently of what a unit's spend
        /// restriction allows. This is checked before unit restrictions so an
        /// as-though payment permission cannot bypass it.
        mana_color_constraint: ActivationManaColorConstraint,
    },
    /// Payment for a cost during spell or ability resolution. Current
    /// restriction variants name spell-casting or ability-activation use, so
    /// restricted mana is not eligible here.
    Effect,
    /// CR 116.2: Payment for a special action's mana cost (e.g. a Room's
    /// unlock cost, CR 116.2m / CR 709.5e). Special actions don't use the stack
    /// and are neither spell casts nor ability activations, so they need a
    /// distinct context: spell/activation-restricted mana must reject them, but
    /// special-action-restricted mana (`OnlyForSpecialAction`) must accept the
    /// matching action class. Parameterized over [`SpecialAction`] so one
    /// context variant covers every restrictable special-action class rather
    /// than a sibling per action.
    SpecialAction(SpecialAction),
}

impl PaymentContext<'_> {
    /// CR 106.6: Whether a mana unit's actual type may be used in this payment
    /// context before its own spend restrictions are considered. Activation
    /// riders constrain the physical mana paid, so this gate applies equally to
    /// unrestricted and restricted units.
    pub fn permits_actual_mana_type(&self, mana_type: ManaType) -> bool {
        match self {
            Self::Activation {
                mana_color_constraint,
                ..
            } => mana_color_constraint.permits(mana_type),
            Self::Spell(_) | Self::Effect | Self::SpecialAction(_) => true,
        }
    }
}

/// CR 106.6: A color constraint imposed by the activated ability whose cost is
/// being paid (for example, "Spend only mana of the chosen color to activate
/// this ability"). This is an AND gate on the actual `ManaUnit::color`, not a
/// conversion of the mana's type and not an as-though permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivationManaColorConstraint {
    /// The ability has no color-specific activation-payment rider.
    #[default]
    Unrestricted,
    /// Every mana unit spent for this activation must have this actual color.
    Only(ManaColor),
    /// The rider refers to a source choice that is unavailable. Fail closed:
    /// no mana unit may pay the activation cost.
    Impossible,
}

impl ActivationManaColorConstraint {
    /// CR 106.6: Whether this exact mana unit type may pay the activation
    /// before the unit's own spend restrictions are considered.
    pub fn permits(self, mana_type: ManaType) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Only(color) => mana_type == ManaType::from(color),
            Self::Impossible => false,
        }
    }
}

/// CR 116.2: A class of special action whose mana cost can be the subject of a
/// CR 106.6 mana-spend restriction ("Spend this mana only to unlock doors").
///
/// Only special actions that pay a mana cost *through the mana pool* with a
/// restriction-aware payment context belong here. CR 116.2m / CR 709.5e door
/// unlock is one such action (its unlock cost routes through
/// `pay_special_action_mana_cost_with_resume`). CR 116.2b turn-face-up is
/// another: the `GameAction::TurnFaceUp` handler now derives the
/// morph/disguise/manifest cost (`game::morph::turn_face_up_prepare`, CR 702.37e
/// / CR 702.168d / CR 701.40b) and pays it through
/// `PaymentContext::SpecialAction(TurnFaceUp)` before flipping the permanent, so
/// a `TurnFaceUp`-restricted mana's runtime gate
/// ([`ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp)`]) is live
/// there and correctly rejected for every other context. A card whose spend
/// restriction names only production-live branches (Overgrown Zealot; Creeping
/// Peeper; and, since CR 708.4 face-down spell casting, Tin Street Gossip) is
/// therefore absorbed at the `Effect::Mana` seam and supported via
/// `ManaSpendRestriction::is_coverage_supported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialAction {
    /// CR 116.2g + CR 702.139a: Paying {3} to put a revealed companion from
    /// outside the game into its owner's hand. This payment uses the mana pool,
    /// so mana restricted to this exact special action is eligible while mana
    /// restricted to casting, activating, or another special action is not.
    CompanionToHand,
    /// CR 116.2m + CR 709.5e: Paying a locked Room half's unlock cost to give
    /// the permanent the appropriate unlocked designation.
    UnlockDoor,
    /// CR 116.2k + CR 702.170a: Exiling a card from hand and paying its plot
    /// cost to make it a plotted card. The plot ability is synthesized as a
    /// hand-zone activated ability (`synthesize_plot`); this variant lets a
    /// `StaticMode::ReduceActionCost { action: Plot, .. }` (Doc Aurlock) reduce
    /// that plot cost without conflating plot with generic activated-ability
    /// cost reducers.
    Plot,
    /// CR 116.2b + CR 702.37e / CR 702.168d / CR 701.40b: Paying a face-down
    /// permanent's morph/disguise cost (or a manifested creature card's mana cost)
    /// to turn it face up. The `GameAction::TurnFaceUp` handler emits
    /// `PaymentContext::SpecialAction(TurnFaceUp)` when charging that cost, so mana
    /// restricted to this action is spendable there and rejected elsewhere — see
    /// the type-level note above.
    TurnFaceUp,
    /// CR 901.9 / CR 116.2i: Paying the escalating generic cost to roll the
    /// planar die as a Planechase special action.
    RollPlanarDie,
    /// CR 116.2c: Paying a continuous effect's printed termination cost to end
    /// it ("You may pay {W} to end this effect" — the Licid cycle).
    ///
    /// The payment is neither a cast nor an activation, so mana restricted to
    /// spells or to activated abilities MUST be rejected here — that rejection
    /// is the correctness win this variant buys over charging the cost through
    /// an unlabelled payment context.
    EndContinuousEffect,
    /// CR 116.2h + CR 702.143a: Paying {2} to exile a card with foretell from
    /// hand face down as a special action. The payment is not a cast or an
    /// activation, so mana restricted to spells or activated abilities MUST be
    /// rejected here, while special-action-restricted mana is eligible.
    Foretell,
}

/// CR 106.6: The ability-activation half of a "spend only to cast [X] spell or
/// activate …" restriction. Parameterizes *which* ability activations the mana
/// may also be spent on, so the spell-type + ability-activation OR restriction
/// needs a single variant rather than a sibling per ability scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbilityActivationScope {
    /// "… or activate abilities of [the spell's type]" — only abilities on a
    /// permanent whose type matches the restriction's `spell_type`
    /// (e.g. "cast creature spells or activate abilities of creatures").
    OfSpellType,
    /// "… or to activate an ability" — any ability activation is permitted
    /// (e.g. Sage of the Unknowable, "a colorless spell or to activate an ability").
    Any,
}

/// CR 106.6 + CR 400.7: Whether a zone-gated spend restriction names the zone a
/// spell *must* be cast from or a zone it must *not* be cast from. This is the
/// inclusion/exclusion axis of [`ManaRestriction::OnlyForSpellFromZone`] — "from
/// your graveyard" (`From`) versus "from anywhere other than your hand"
/// (`NotFrom`, Mm'menon, the Right Hand). Parameterizing the existing zone
/// variant over this polarity keeps a single zone-spend variant rather than a
/// `SpellFromZone` / `SpellNotFromZone` sibling pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ZoneSpendPolarity {
    /// "from [your] <zone>" — the spell must be cast from the named zone.
    #[default]
    From,
    /// "from anywhere other than [your] <zone>" — the spell must be cast from
    /// any zone *except* the named one. A spell with no recorded cast-from zone
    /// is treated as not satisfying the restriction (conservative).
    NotFrom,
}

/// CR 106.6 + CR 400.7: parameterized zone-gated spend restriction payload —
/// the named zone plus an inclusion/exclusion [`ZoneSpendPolarity`]. Carried as
/// a newtype payload of [`ManaRestriction::OnlyForSpellFromZone`] (and the
/// parse-time [`crate::types::ability::ManaSpendRestriction::SpellFromZone`]) so
/// the externally-tagged serialized form stays a single value rather than a
/// struct variant. A custom [`Deserialize`] (see [`ZoneSpendPayload`]) accepts
/// both the legacy bare-`Zone` form (`{"OnlyForSpellFromZone":"Graveyard"}`) and
/// the current `{zone, polarity}` form, mapping the legacy form to `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ZoneSpend {
    pub zone: Zone,
    pub polarity: ZoneSpendPolarity,
}

/// Untagged payload bridging legacy bare-`Zone` serialized data and the current
/// `{zone, polarity}` object form. Serde cannot deserialize a bare string into a
/// struct variant, so this enum gives [`ZoneSpend`]'s custom `Deserialize` both
/// shapes; `polarity` defaults to `From` for the current form when absent.
#[derive(Deserialize)]
#[serde(untagged)]
enum ZoneSpendPayload {
    Current {
        zone: Zone,
        #[serde(default)]
        polarity: ZoneSpendPolarity,
    },
    Legacy(Zone),
}

impl<'de> Deserialize<'de> for ZoneSpend {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match ZoneSpendPayload::deserialize(deserializer)? {
            ZoneSpendPayload::Legacy(zone) => Self {
                zone,
                polarity: ZoneSpendPolarity::From,
            },
            ZoneSpendPayload::Current { zone, polarity } => Self { zone, polarity },
        })
    }
}

/// CR 106.6 + CR 107.3 + CR 202.3: A single qualifying criterion on the cost of
/// the spell a restricted mana may be spent on. Used by
/// [`ManaRestriction::OnlyForSpellMatchingCostCriteria`] to express the
/// disjunctive "spells with mana value N or greater **or** spells with {X} in
/// their mana costs" reading (Helga, Skittish Seer; Troyan, Gutsy Explorer)
/// without proliferating one variant per (threshold × X-disjunct) combination.
///
/// Both criteria are properties of the spell's *cost* (CR 202.3 mana value /
/// CR 107.3 X symbol), so they share a single categorical axis and belong to one
/// typed predicate rather than a raw bool flag bolted onto the mana-value variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpellCostCriterion {
    /// CR 202.3: The spell's mana value satisfies `spell_mana_value <cmp> value`.
    ManaValue { comparator: Comparator, value: u32 },
    /// CR 107.3 + CR 202.3e: The spell's printed mana cost contains an `{X}`
    /// symbol. Detected structurally (X contributes 0 to mana value off the
    /// stack), via `SpellMeta.has_x_in_cost`.
    HasXInCost,
}

impl SpellCostCriterion {
    /// CR 106.6: Whether the described spell satisfies this single cost criterion.
    pub fn matches(&self, meta: &SpellMeta) -> bool {
        match self {
            SpellCostCriterion::ManaValue { comparator, value } => meta
                .mana_value
                .is_some_and(|mv| comparator.evaluate(mv as i32, *value as i32)),
            SpellCostCriterion::HasXInCost => meta.has_x_in_cost,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaRestriction {
    /// "Spend this mana only to cast spells."
    OnlyForSpell,
    /// "Spend this mana only to cast creature spells" / "only to cast artifact spells".
    OnlyForSpellType(String),
    /// "Spend this mana only to cast a creature spell of the chosen type."
    /// The `String` is the chosen creature type (e.g., "Elf").
    OnlyForCreatureType(String),
    /// CR 106.6: "Spend this mana only to cast creature spells or activate abilities of creatures."
    /// Allows spending for spells of `spell_type` (checked via `allows_spell`) OR for ability
    /// activations whose scope is described by `ability` (checked via `allows_activation`):
    /// `OfSpellType` restricts to abilities of permanents of `spell_type`; `Any` permits any
    /// ability activation ("… or to activate an ability").
    OnlyForTypeSpellsOrAbilities {
        spell_type: String,
        ability: AbilityActivationScope,
    },
    /// "Spend this mana only to activate abilities."
    /// Cannot be used for casting spells — activation-only.
    OnlyForActivation,
    /// CR 106.6: "Spend this mana only to activate power-up abilities." Keyed on
    /// the activating ability's keyword tag (Quinjet Technician), a distinct axis
    /// from `OnlyForTypeSpellsOrAbilities` (which keys on the source permanent's
    /// type) and `OnlyForActivation` (which permits any activation).
    OnlyForTaggedActivation(AbilityTag),
    /// "Spend this mana only on costs that include {X}."
    /// Only permits spending on spells or abilities with {X} in their cost.
    OnlyForXCosts,
    /// "Spend this mana only to cast spells with flashback."
    OnlyForSpellWithKeywordKind(KeywordKind),
    /// "Spend this mana only to cast spells with flashback from a graveyard."
    OnlyForSpellWithKeywordKindFromZone(KeywordKind, crate::types::zones::Zone),
    /// CR 106.6: "Spend this mana only to cast spells with mana value N or
    /// greater" (or "or less"). `comparator` applies `spell_mana_value <cmp>
    /// value`. Parameterized over [`Comparator`] — one variant per threshold reading.
    OnlyForSpellWithManaValue { comparator: Comparator, value: u32 },
    /// CR 106.6 + CR 107.3 + CR 202.3: "Spend this mana only to cast [creature]
    /// spells with mana value N or greater **or** [creature] spells with {X} in
    /// their mana costs" (Helga, Skittish Seer — creature-narrowed; Troyan, Gutsy
    /// Explorer — any spell). A spell qualifies when it matches **any** listed
    /// [`SpellCostCriterion`] (disjunction) AND, if `spell_type` is `Some`, also
    /// has that type. Parameterized over the optional type narrowing and the
    /// criteria set rather than proliferating one variant per
    /// (type × threshold × X-disjunct) combination.
    OnlyForSpellMatchingCostCriteria {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_type: Option<String>,
        criteria: Vec<SpellCostCriterion>,
    },
    /// CR 105.2 + CR 106.6: "Spend this mana only to cast spells with exactly N
    /// colors" (also "N or more / N or fewer"). `comparator` applies
    /// `spell_color_count <cmp> count`. Colorless spells have color_count 0.
    OnlyForSpellWithColorCount { comparator: Comparator, count: u32 },
    /// CR 105.2 + CR 106.6: "Spend this mana only to cast spells of [color]."
    /// Usually composed with `OnlyForSpellWithColorCount { EQ, 1 }` for the
    /// stricter "monocolored spells of that color" reading.
    OnlyForSpellColor(ManaColor),
    /// CR 106.6 + CR 400.7: "Spend this mana only to cast spells from your
    /// graveyard" / "from exile" ([`ZoneSpendPolarity::From`]) and "from anywhere
    /// other than your hand" ([`ZoneSpendPolarity::NotFrom`], Mm'menon, the Right
    /// Hand). Gates on the spell's cast-from zone, consulting
    /// `SpellMeta.cast_from_zone`. A distinct axis from
    /// `OnlyForSpellWithKeywordKindFromZone` (which also requires a keyword).
    /// Carried as a [`ZoneSpend`] newtype payload whose custom `Deserialize`
    /// accepts the legacy bare-`Zone` form (`{"OnlyForSpellFromZone":"Graveyard"}`)
    /// for backward compatibility, mapping it to the inclusion reading.
    OnlyForSpellFromZone(ZoneSpend),
    /// CR 106.6 + CR 601.2g-h: This mana cannot pay for a spell cast from the
    /// named zone. It remains unrestricted for every non-cast payment context.
    /// A spell whose origin is unknown is rejected conservatively.
    CannotCastSpellFromZone(Zone),
    /// CR 106.6 + CR 708.4: "Spend this mana only to cast face-down spells"
    /// (Tin Street Gossip). Gates spending on whether the spell is being CAST
    /// face down (morph/disguise/cloak), consulting `SpellMeta.is_face_down`.
    /// Rejects normal face-up casts, ability activations, and special actions.
    ///
    /// The gate reads `meta.is_face_down`, which `build_spell_meta` sources from
    /// the cast's face-down intent — NOT from `obj.face_down`. That distinction
    /// matters: exile/library concealment (foretell, hideaway) sets
    /// `obj.face_down = true` for a card that is nonetheless CAST FACE UP
    /// (CR 702.143c), so the gate correctly REJECTS those concealment casts.
    ///
    /// The gate is now live: CR 708.4 morph/megamorph/disguise face-down spell
    /// casting (`AlternativeCastKeyword::FaceDown` → `continue_cast_face_down`)
    /// routes the `{3}` face-down cost (CR 702.37c) through `PaymentContext::Spell`
    /// with `SpellMeta.is_face_down = true`, so this gate is satisfiable there and
    /// correctly rejected for every other context — a normal face-up cast, and an
    /// exile-concealment cast (foretell/hideaway) whose `obj.face_down = true` but
    /// which is cast face up (CR 702.143c), both report `is_face_down = false` —
    /// see [`ManaRestriction::allows_spell`]. A card whose spend restriction
    /// includes this leaf is therefore absorbed at the `Effect::Mana` seam and
    /// coverage-supported via `ManaSpendRestriction::is_coverage_supported`.
    OnlyForFaceDownSpell,
    /// CR 106.6: Disjunctive spend restriction — the mana may be spent on any
    /// payment that satisfies at least one inner restriction. Composition
    /// combinator (each branch is itself a full restriction), not a leaf
    /// parameterization. Models "… to cast a Dragon spell or an Omen spell" and
    /// "… to cast an Assassin spell, a spell that has freerunning, or to activate
    /// an ability of an Assassin source."
    OnlyForAny(Vec<ManaRestriction>),
    /// CR 106.6 + CR 116.2: "Spend this mana only to [unlock doors]" — the
    /// special-action half of a spend restriction. Payable only for a
    /// [`PaymentContext::SpecialAction`] whose [`SpecialAction`] matches.
    /// Rejects spell casts, ability activations, and generic effect payments.
    /// Parameterized over the action class so one variant covers every
    /// restrictable special action (door unlock today; extensible).
    OnlyForSpecialAction(SpecialAction),
    /// A source-dependent template could not resolve a required choice. This
    /// remains attached to the produced mana rather than being dropped so the
    /// resulting unit is unspendable instead of accidentally unrestricted.
    Impossible,
    /// CR 702.51a: Internal marker for a convoke tap that substitutes for
    /// paying mana. The payment algorithm may consume it for the current spell,
    /// but cast-spent metrics and mana-added triggers must ignore it.
    ConvokePayment,
}

/// CR 106.3 + CR 107.1b: The nominal nonnegative base amount of a manually
/// selected flexible mana source.
/// A dynamic amount is resolved after activation costs have been paid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ManaSourceQuantity {
    Fixed(u32),
    Variable,
}

/// CR 106.1a + CR 106.1b: The output choice captured for a mana-source activation.
///
/// `Concrete` is the planner-selected output used by automatic payment. A
/// manually selected flexible producer instead retains `DeferredColorChoice`,
/// so the normal mana-choice interaction chooses its color at activation time
/// rather than silently committing to an arbitrary planner row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ManaSourceOutput {
    Concrete(ManaType),
    DeferredColorChoice { quantity: ManaSourceQuantity },
}

impl ManaRestriction {
    fn matches_required_quality<'a>(
        required: &str,
        qualities: impl IntoIterator<Item = &'a String>,
    ) -> bool {
        let qualities = qualities.into_iter().collect::<Vec<_>>();
        // CR 106.6: A restricted-spend type phrase names the *set* of objects the
        // mana may be spent on. Both connectives — " or " and " and " — enumerate
        // distinct acceptable types, so each is an alternative the object need
        // only satisfy one of. Per the Melek, Izzet Paragon example (CR 601.3e),
        // "instant and sorcery spells" (Tablet of Discovery, issue #1975) lets a
        // spell that is an instant *or* a sorcery qualify; a single object is
        // never required to carry both types. Whitespace within an alternative
        // still ANDs (a compound single quality like "Colorless Eldrazi" must
        // match every word).
        required
            .split(" or ")
            .flat_map(|clause| clause.split(" and "))
            .any(|alternative| {
                alternative.split_whitespace().all(|part| {
                    qualities
                        .iter()
                        .any(|quality| quality.eq_ignore_ascii_case(part))
                })
            })
    }

    /// Returns `true` if this restriction permits spending mana on the given spell.
    pub fn allows_spell(&self, meta: &SpellMeta) -> bool {
        match self {
            ManaRestriction::OnlyForSpell => true,
            // CR 106.6: Oracle type phrases in spend restrictions name both core
            // types (Creature, Instant, …) and subtypes (Ninja, Turtle, …). Consult
            // both buckets uniformly, same as `OnlyForTypeSpellsOrAbilities`.
            ManaRestriction::OnlyForSpellType(required_type) => Self::matches_required_quality(
                required_type,
                meta.types.iter().chain(meta.subtypes.iter()),
            ),
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
            // CR 106.6 + CR 903.3: Relational commander-subtype filter. `SpellMeta`
            // carries no commander context, so this restriction can never
            // independently authorize a spend here — it returns `false` and the
            // authoritative relational evaluation (comparing the spell's creature
            // CR 106.6: The spell-casting half of the OR — allows if the spell has the
            // required type, consulting both core card types (Creature, Instant, ...)
            // and subtypes (Elemental, Goblin, ...). Flamebraider's "Elemental" names
            // a creature subtype; "Artifact" would name a core type. The check treats
            // both buckets uniformly because Oracle text doesn't distinguish the two.
            ManaRestriction::OnlyForTypeSpellsOrAbilities { spell_type, .. } => {
                Self::matches_required_quality(
                    spell_type,
                    meta.types.iter().chain(meta.subtypes.iter()),
                )
            }
            // Activation-only mana cannot be used to cast spells.
            ManaRestriction::OnlyForActivation => false,
            // CR 106.6: Tag-scoped activation mana cannot be used to cast spells.
            ManaRestriction::OnlyForTaggedActivation(_) => false,
            // CR 106.6: X-cost restriction — spell must have {X} in its mana cost.
            ManaRestriction::OnlyForXCosts => meta.has_x_in_cost,
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
            // CR 106.6: Mana-value-gated spend. Mirrors the cast-permission
            // mana-value check in game::casting
            // (`comparator.evaluate(obj.mana_cost.mana_value() as i32, value)`).
            // A spell with no known mana value (None) is not eligible.
            ManaRestriction::OnlyForSpellWithManaValue { comparator, value } => meta
                .mana_value
                .is_some_and(|mv| comparator.evaluate(mv as i32, *value as i32)),
            // CR 106.6 + CR 107.3 + CR 202.3: Disjunctive cost-criteria spend.
            // The spell must satisfy the optional type narrowing AND at least one
            // listed criterion ("MV ≥ N" OR "has {X} in cost"). An empty criteria
            // set never authorizes a spend (no criterion can be satisfied).
            ManaRestriction::OnlyForSpellMatchingCostCriteria {
                spell_type,
                criteria,
            } => {
                let type_ok = spell_type.as_deref().is_none_or(|required| {
                    Self::matches_required_quality(
                        required,
                        meta.types.iter().chain(meta.subtypes.iter()),
                    )
                });
                type_ok && criteria.iter().any(|c| c.matches(meta))
            }
            // CR 105.2: Color-count-gated spend. Colorless spells have a color
            // count of 0. A spell with no recorded color count (None) is ineligible.
            ManaRestriction::OnlyForSpellWithColorCount { comparator, count } => meta
                .color_count
                .is_some_and(|cc| comparator.evaluate(cc as i32, *count as i32)),
            ManaRestriction::OnlyForSpellColor(required_color) => {
                meta.colors.contains(required_color)
            }
            // CR 106.6 + CR 400.7: zone-gated spend. `From` requires the spell be
            // cast from the named zone; `NotFrom` requires it be cast from any
            // zone *except* the named one (e.g. "from anywhere other than your
            // hand"). A spell with no recorded cast-from zone satisfies neither
            // reading (conservative — `None == Some(zone)` is false for `From`,
            // and `NotFrom` treats unknown origin as ineligible).
            ManaRestriction::OnlyForSpellFromZone(zs) => match zs.polarity {
                ZoneSpendPolarity::From => meta.cast_from_zone == Some(zs.zone),
                ZoneSpendPolarity::NotFrom => meta
                    .cast_from_zone
                    .is_some_and(|cast_from| cast_from != zs.zone),
            },
            ManaRestriction::CannotCastSpellFromZone(zone) => meta
                .cast_from_zone
                .is_some_and(|cast_from| cast_from != *zone),
            // CR 708.4: Face-down-spell-gated spend. The eligibility predicate is
            // `meta.is_face_down` — a spell qualifies only when it is being CAST
            // face down (morph/disguise/cloak); normal face-up casts are
            // ineligible. `is_face_down` is sourced from the cast's face-down
            // intent (`build_spell_meta`), not from `obj.face_down`, so this arm
            // correctly REJECTS both normal face-up casts AND exile-concealment
            // casts (foretell/hideaway, CR 702.143c) whose `obj.face_down = true`
            // but which are cast face up. It is also fail-closed: no production
            // payment site casts a spell face down (`GameAction::PlayFaceDown` →
            // `game::morph::play_face_down` enters the battlefield via the zone
            // pipeline and charges no mana), so `is_face_down` is never `true` at a
            // real `PaymentContext::Spell` site and the gate never over-permits. It
            // already reads `meta.is_face_down`, so the day a real face-down CAST
            // routes its `{3}` cost through `PaymentContext::Spell` with that flag
            // set, the gate becomes live with no change here. See the variant doc.
            ManaRestriction::OnlyForFaceDownSpell => meta.is_face_down,
            // CR 106.6: Disjunction — the spell is payable if it satisfies any branch.
            ManaRestriction::OnlyForAny(subs) => subs.iter().any(|r| r.allows_spell(meta)),
            // CR 116.2: Special-action-only mana never pays for a spell cast.
            ManaRestriction::OnlyForSpecialAction(_) => false,
            ManaRestriction::Impossible => false,
            ManaRestriction::ConvokePayment => true,
        }
    }

    /// Returns `true` if this restriction permits spending mana to activate an ability
    /// on a permanent whose core types include `source_types` and subtypes include
    /// `source_subtypes`.
    /// CR 106.6: Used for "or activate abilities of creatures" restrictions.
    pub fn allows_activation(
        &self,
        source_types: &[String],
        source_subtypes: &[String],
        ability_tag: Option<AbilityTag>,
    ) -> bool {
        match self {
            // Spell-only restrictions don't permit ability activation.
            ManaRestriction::OnlyForSpell
            | ManaRestriction::OnlyForSpellType(_)
            | ManaRestriction::OnlyForCreatureType(_)
            | ManaRestriction::OnlyForSpellWithKeywordKind(_)
            | ManaRestriction::OnlyForSpellWithKeywordKindFromZone(_, _)
            | ManaRestriction::OnlyForSpellWithManaValue { .. }
            | ManaRestriction::OnlyForSpellMatchingCostCriteria { .. }
            | ManaRestriction::OnlyForSpellWithColorCount { .. }
            | ManaRestriction::OnlyForSpellColor(_)
            | ManaRestriction::OnlyForSpellFromZone(_)
            // CR 708.4: Face-down-spell-only mana never pays for ability activation.
            | ManaRestriction::OnlyForFaceDownSpell
            // CR 116.2: Special-action-only mana never pays for ability activation.
            | ManaRestriction::OnlyForSpecialAction(_)
            | ManaRestriction::Impossible => false,
            ManaRestriction::CannotCastSpellFromZone(_) => true,
            // CR 106.6: The ability-activation half of the OR. `OfSpellType`
            // restricts to abilities of permanents whose type matches the
            // restriction ("Elemental sources" includes creature type Elemental —
            // consult subtypes too); `Any` permits any ability activation.
            ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type,
                ability,
            } => match ability {
                AbilityActivationScope::OfSpellType => Self::matches_required_quality(
                    spell_type,
                    source_types.iter().chain(source_subtypes.iter()),
                ),
                AbilityActivationScope::Any => true,
            },
            // Activation-only mana always allows ability activation.
            ManaRestriction::OnlyForActivation => true,
            // CR 106.6: Tag-scoped mana — payable only for an activation whose
            // ability carries the matching keyword tag (Quinjet → power-up).
            ManaRestriction::OnlyForTaggedActivation(required_tag) => {
                ability_tag == Some(*required_tag)
            }
            // CR 106.6: Disjunction — the activation is payable if any branch allows it.
            ManaRestriction::OnlyForAny(subs) => subs
                .iter()
                .any(|r| r.allows_activation(source_types, source_subtypes, ability_tag)),
            // X-cost mana can be used for abilities with {X} in their cost.
            // TODO: Check if the ability has {X} in its cost once that data is available.
            ManaRestriction::OnlyForXCosts | ManaRestriction::ConvokePayment => false,
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
                ability_tag,
                mana_color_constraint: _,
            } => self.allows_activation(source_types, source_subtypes, *ability_tag),
            PaymentContext::Effect => match self {
                ManaRestriction::CannotCastSpellFromZone(_) => true,
                ManaRestriction::OnlyForAny(subs) => subs.iter().any(|r| r.allows(ctx)),
                _ => false,
            },
            // CR 116.2: Positive "only for" restrictions must authorize this
            // exact special-action class (directly or through a disjunction).
            // A negative restriction naming only one spell-cast class does not
            // restrict special-action payments.
            PaymentContext::SpecialAction(action) => self.allows_special_action(*action),
        }
    }

    /// CR 106.6 + CR 116.2: Returns `true` if this restriction permits spending
    /// mana on the given special action (e.g. a Room door unlock). An exact
    /// [`ManaRestriction::OnlyForSpecialAction`] or a matching disjunction may
    /// authorize one; a restriction that prohibits only a spell-cast class does
    /// not restrict special actions at all.
    pub fn allows_special_action(&self, action: SpecialAction) -> bool {
        match self {
            ManaRestriction::OnlyForSpecialAction(allowed) => *allowed == action,
            ManaRestriction::CannotCastSpellFromZone(_) => true,
            ManaRestriction::OnlyForAny(subs) => {
                subs.iter().any(|r| r.allows_special_action(action))
            }
            ManaRestriction::OnlyForSpell
            | ManaRestriction::OnlyForSpellType(_)
            | ManaRestriction::OnlyForCreatureType(_)
            | ManaRestriction::OnlyForTypeSpellsOrAbilities { .. }
            | ManaRestriction::OnlyForActivation
            | ManaRestriction::OnlyForTaggedActivation(_)
            | ManaRestriction::OnlyForXCosts
            | ManaRestriction::OnlyForSpellWithKeywordKind(_)
            | ManaRestriction::OnlyForSpellWithKeywordKindFromZone(_, _)
            | ManaRestriction::OnlyForSpellWithManaValue { .. }
            | ManaRestriction::OnlyForSpellMatchingCostCriteria { .. }
            | ManaRestriction::OnlyForSpellWithColorCount { .. }
            | ManaRestriction::OnlyForSpellColor(_)
            | ManaRestriction::OnlyForSpellFromZone(_)
            | ManaRestriction::OnlyForFaceDownSpell
            | ManaRestriction::Impossible
            | ManaRestriction::ConvokePayment => false,
        }
    }
}

/// CR 106.6: Additional effect that the mana confers upon the spell it is spent on.
/// E.g., "that spell can't be countered" (Cavern of Souls, Delighted Halfling).
fn default_mana_keyword_grant_duration() -> Box<crate::types::ability::Duration> {
    Box::new(crate::types::ability::Duration::UntilEndOfTurn)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ManaSpellGrant {
    /// CR 106.6: A spell matching `filter` and cast with this mana can't be
    /// countered. `TargetFilter::Any` represents the unqualified wording.
    CantBeCountered { filter: TargetFilter },
    /// CR 106.6a + CR 614.1c: A permanent spell matching `filter` and cast
    /// with this mana enters with `count` `counter_type` counters. Each mana
    /// unit carries a separate replacement effect.
    EntersWithCounters {
        filter: TargetFilter,
        counter_type: CounterType,
        count: QuantityExpr,
    },
    /// CR 106.6 + CR 702.10: If the spell this mana is spent on satisfies
    /// `restriction`, grant it `keyword` for `duration` (subtype lands use
    /// `UntilEndOfTurn`; Hall of the Bandit Lord uses `Permanent`).
    AddKeywordUntilEndOfTurn {
        keyword: Keyword,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restriction: Option<ManaRestriction>,
        #[serde(default = "default_mana_keyword_grant_duration")]
        duration: Box<crate::types::ability::Duration>,
    },
    /// CR 106.6 + CR 603.3: "When you spend this mana to cast a [filter] spell,
    /// [effect]" — a reflexive trigger riding the produced mana (Lapis Orb of
    /// Dragonkind, Scaled Nurturer, Gilanra, Path of Ancestry, Primal Wellspring,
    /// Pyromancer's Goggles). When the mana is spent on a spell matching `filter`,
    /// the controller's `ability` is put on the stack as a triggered ability. The
    /// ability's source is the permanent that produced the mana.
    ///
    /// `filter` is the CR 603.3 TRIGGER EVENT filter — "which spell makes this
    /// fire" — and is deliberately a [`TargetFilter`], NOT a [`ManaRestriction`].
    /// The two answer different questions and coincide only by accident.
    /// `ManaRestriction` is CR 106.6 SPEND legality ("what may this mana pay
    /// for"), and NOT ONE of these cards restricts its mana: Pyromancer's Goggles'
    /// {R} may be spent on anything; it merely TRIGGERS when spent on a red instant
    /// or sorcery. Typing this field as a spend restriction forced its filter
    /// vocabulary to grow along the wrong axis — a color predicate would have had
    /// to be bolted onto `ManaRestriction` purely to serve a trigger — which is the
    /// cross-rule-section conflation the categorical-boundary rule forbids.
    /// `TargetFilter` is the engine's existing "which object matches" vocabulary
    /// (the same one `TriggerDefinition::valid_card` uses), so the whole
    /// type × color × mana-value class is expressible with no new variant.
    ///
    /// Genuine CR 106.6 spend restrictions are unaffected — they live on the
    /// separate [`ManaUnit::restrictions`] field. `TargetFilter::Any` = any spell.
    TriggerOnSpend {
        filter: TargetFilter,
        ability: Box<crate::types::ability::AbilityDefinition>,
    },
}

/// Current wire representation for [`ManaSpellGrant`].
///
/// `CantBeCountered` used to be a unit variant and therefore serialized as the
/// bare string `"CantBeCountered"`. The scoped representation preserves that
/// legacy data as an unqualified (`TargetFilter::Any`) grant.
#[derive(Deserialize)]
enum ManaSpellGrantRepr {
    CantBeCountered {
        filter: TargetFilter,
    },
    EntersWithCounters {
        filter: TargetFilter,
        counter_type: CounterType,
        count: QuantityExpr,
    },
    AddKeywordUntilEndOfTurn {
        keyword: Keyword,
        #[serde(default)]
        restriction: Option<ManaRestriction>,
        #[serde(default = "default_mana_keyword_grant_duration")]
        duration: Box<crate::types::ability::Duration>,
    },
    TriggerOnSpend {
        filter: TargetFilter,
        ability: Box<crate::types::ability::AbilityDefinition>,
    },
}

/// Wire form written before spend-trigger predicates moved from the CR 106.6
/// mana-restriction axis to the CR 603.3 event-filter axis.
#[derive(Deserialize)]
enum LegacyManaSpellGrantRepr {
    TriggerOnSpend {
        #[serde(default)]
        restriction: Option<LegacyManaSpendTriggerRestriction>,
        ability: Box<crate::types::ability::AbilityDefinition>,
    },
}

#[derive(Deserialize)]
enum LegacyManaSpendTriggerRestriction {
    OnlyForSpellWithManaValue { comparator: Comparator, value: u32 },
    OnlyForCreatureType(String),
    SharesCreatureTypeWithCommander,
}

impl LegacyManaSpendTriggerRestriction {
    /// CR 603.3: A historical mana-spend trigger's old restriction selected the
    /// spell event that makes the trigger fire, so preserve it as that event's
    /// object filter rather than as a current CR 106.6 spend restriction.
    fn try_into_event_filter(self) -> Result<TargetFilter, String> {
        Ok(match self {
            Self::OnlyForSpellWithManaValue { comparator, value } => {
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
                    comparator,
                    value: QuantityExpr::Fixed {
                        value: i32::try_from(value).map_err(|_| {
                            "legacy mana spend trigger value exceeds i32 range".to_string()
                        })?,
                    },
                }]))
            }
            Self::OnlyForCreatureType(subtype) => {
                TargetFilter::Typed(TypedFilter::creature().subtype(subtype))
            }
            Self::SharesCreatureTypeWithCommander => TargetFilter::Typed(
                TypedFilter::creature()
                    .properties(vec![FilterProp::SharesCreatureTypeWithCommander]),
            ),
        })
    }
}

impl TryFrom<LegacyManaSpellGrantRepr> for ManaSpellGrant {
    type Error = String;

    fn try_from(value: LegacyManaSpellGrantRepr) -> Result<Self, Self::Error> {
        match value {
            LegacyManaSpellGrantRepr::TriggerOnSpend {
                restriction,
                ability,
            } => Ok(Self::TriggerOnSpend {
                filter: restriction
                    .map_or(Ok(TargetFilter::Any), |value| value.try_into_event_filter())?,
                ability,
            }),
        }
    }
}

impl From<ManaSpellGrantRepr> for ManaSpellGrant {
    fn from(value: ManaSpellGrantRepr) -> Self {
        match value {
            ManaSpellGrantRepr::CantBeCountered { filter } => Self::CantBeCountered { filter },
            ManaSpellGrantRepr::EntersWithCounters {
                filter,
                counter_type,
                count,
            } => Self::EntersWithCounters {
                filter,
                counter_type,
                count,
            },
            ManaSpellGrantRepr::AddKeywordUntilEndOfTurn {
                keyword,
                restriction,
                duration,
            } => Self::AddKeywordUntilEndOfTurn {
                keyword,
                restriction,
                duration,
            },
            ManaSpellGrantRepr::TriggerOnSpend { filter, ability } => {
                Self::TriggerOnSpend { filter, ability }
            }
        }
    }
}

impl<'de> Deserialize<'de> for ManaSpellGrant {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value == serde_json::Value::String("CantBeCountered".to_string()) {
            return Ok(Self::CantBeCountered {
                filter: TargetFilter::Any,
            });
        }

        match serde_json::from_value::<ManaSpellGrantRepr>(value.clone()) {
            Ok(value) => Ok(value.into()),
            Err(current_error) => serde_json::from_value::<LegacyManaSpellGrantRepr>(value)
                .map_err(|_| serde::de::Error::custom(current_error))
                .and_then(|value| value.try_into().map_err(serde::de::Error::custom)),
        }
    }
}

/// When mana expires — controls lifecycle beyond the normal CR 106.4 step/phase drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaExpiry {
    /// Mana persists through normal step/phase drains until the turn reaches cleanup.
    /// Used by "Until end of turn, you don't lose this mana as steps and phases end."
    EndOfTurn,
    /// Mana persists through combat steps but drains at EndCombat → PostCombatMain.
    /// Used by Firebending and similar "mana lasts within combat" mechanics.
    EndOfCombat,
    /// Mana persists through the steps of whichever of CR 500.1's five phases is
    /// active, and drains only when the turn crosses into a different one.
    ///
    /// The pre-M10 mana-burn rule, which emptied pools at end of PHASE rather
    /// than end of step (see the "Mana Burn (Obsolete)" glossary entry), and is
    /// opted into per format by `LegacyRuleSet.mana_burn`.
    ///
    /// Generalizes [`ManaExpiry::EndOfCombat`], which is this same "survive my
    /// phase's internal steps, drain at the real boundary" shape hardcoded to
    /// one phase. Like its two siblings this variant names no specific phase —
    /// it resolves against whichever group is active when it is checked.
    EndOfPhaseGroup,
}

/// CR 205.4g: Supertype carried by produced mana (Snow today; extensible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaSupertype {
    Snow,
}

/// Serde adapter for legacy `snow: bool` on `ManaUnit`.
pub mod snow_compat {
    use super::ManaSupertype;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        supertype: &Option<ManaSupertype>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(matches!(supertype, Some(ManaSupertype::Snow)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ManaSupertype>, D::Error> {
        let snow = bool::deserialize(deserializer)?;
        Ok(if snow {
            Some(ManaSupertype::Snow)
        } else {
            None
        })
    }
}

/// Stable per-unit identity for an individual mana pip in a player's pool.
///
/// Minted by `GameState::next_pip_id` when mana enters a real pool so that a
/// player can direct (pin) which specific unit pays a pending cost. The sentinel
/// `ManaPipId(0)` marks an unstamped unit (convoke markers, detached preview
/// pools) and never matches a real pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ManaPipId(pub u64);

#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
pub struct ManaUnit {
    pub color: ManaType,
    pub source_id: ObjectId,
    /// CR 118.3a: stable identity used to direct which pool unit pays a cost.
    /// `ManaPipId(0)` = unstamped sentinel (set by constructors; stamped on pool
    /// entry by `GameState::add_mana_to_pool`).
    #[serde(default = "unstamped_pip_id")]
    pub pip_id: ManaPipId,
    #[serde(default, with = "snow_compat", rename = "snow")]
    pub supertype: Option<ManaSupertype>,
    /// True when this unit was produced by a source that could produce two or
    /// more colors of mana. Used by the Universes Beyond `{Z}` cost symbol.
    #[serde(default, skip_serializing_if = "is_false")]
    pub source_could_produce_two_or_more_colors: bool,
    pub restrictions: Vec<ManaRestriction>,
    /// CR 106.6: Properties granted to the spell this mana is spent on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<ManaSpellGrant>,
    /// When set, this mana survives normal phase-transition drains until the
    /// specified expiry condition is met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<ManaExpiry>,
}

// CR 104.4b: `pip_id` is a runtime/UI identity tag (the pin key used to direct
// which pool unit pays a cost), NOT a game-state-semantic property. It must not
// participate in `ManaUnit` value-equality, otherwise two pool units that differ
// only by their monotonic pip_id would compare unequal and break game-state
// equality — defeating CR 104.4b mandatory-loop detection (a loop that floats
// mana each iteration would never compare equal) and AI-search state dedup.
// `Eq` is derived (it only requires `PartialEq` to exist); ignoring one field
// keeps the relation reflexive/symmetric/transitive.
impl PartialEq for ManaUnit {
    fn eq(&self, other: &Self) -> bool {
        let ManaUnit {
            color,
            source_id,
            pip_id: _,
            supertype,
            source_could_produce_two_or_more_colors,
            restrictions,
            grants,
            expiry,
        } = self;
        let ManaUnit {
            color: other_color,
            source_id: other_source_id,
            pip_id: _,
            supertype: other_supertype,
            source_could_produce_two_or_more_colors: other_two_or_more,
            restrictions: other_restrictions,
            grants: other_grants,
            expiry: other_expiry,
        } = other;
        color == other_color
            && source_id == other_source_id
            && supertype == other_supertype
            && source_could_produce_two_or_more_colors == other_two_or_more
            && restrictions == other_restrictions
            && grants == other_grants
            && expiry == other_expiry
    }
}

/// A mana unit without its pip: everything two pool units must share for no
/// reader to tell them apart, so the pool merges units of one shape.
///
/// CR 106.4: mana is held in a player's pool. CR 106.1b: `color` is one of the
/// six mana types, colorless included. CR 106.3: `source_id` is the producing
/// source. CR 106.6: `restrictions` and `grants` are the spending restrictions
/// and additional effects the producing ability attached. CR 107.4h: the snow
/// supertype marks mana from a snow source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManaShape {
    pub color: ManaType,
    pub source_id: ObjectId,
    pub supertype: Option<ManaSupertype>,
    pub source_could_produce_two_or_more_colors: bool,
    pub restrictions: Vec<ManaRestriction>,
    pub grants: Vec<ManaSpellGrant>,
    pub expiry: Option<ManaExpiry>,
}

impl ManaShape {
    pub fn with_pip(self, pip_id: ManaPipId) -> ManaUnit {
        let ManaShape {
            color,
            source_id,
            supertype,
            source_could_produce_two_or_more_colors,
            restrictions,
            grants,
            expiry,
        } = self;
        ManaUnit {
            color,
            source_id,
            pip_id,
            supertype,
            source_could_produce_two_or_more_colors,
            restrictions,
            grants,
            expiry,
        }
    }

    pub fn is_snow(&self) -> bool {
        matches!(self.supertype, Some(ManaSupertype::Snow))
    }

    pub fn is_convoke_payment(&self) -> bool {
        self.restrictions.contains(&ManaRestriction::ConvokePayment)
    }
}

impl ManaUnit {
    pub fn into_shape_and_pip(self) -> (ManaShape, ManaPipId) {
        let ManaUnit {
            color,
            source_id,
            pip_id,
            supertype,
            source_could_produce_two_or_more_colors,
            restrictions,
            grants,
            expiry,
        } = self;
        (
            ManaShape {
                color,
                source_id,
                supertype,
                source_could_produce_two_or_more_colors,
                restrictions,
                grants,
                expiry,
            },
            pip_id,
        )
    }

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
            pip_id: ManaPipId(0),
            supertype: snow.then_some(ManaSupertype::Snow),
            source_could_produce_two_or_more_colors: false,
            restrictions,
            grants: Vec::new(),
            expiry: None,
        }
    }

    /// Construct a convoke payment marker. This is intentionally not mana
    /// production; it exists only so the shared mana-payment algorithm can
    /// consume a tap as satisfying the selected shard.
    pub fn convoke_payment(color: ManaType, source_id: ObjectId) -> Self {
        Self {
            color,
            source_id,
            pip_id: ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::ConvokePayment],
            grants: Vec::new(),
            expiry: None,
        }
    }

    pub fn is_convoke_payment(&self) -> bool {
        self.restrictions.contains(&ManaRestriction::ConvokePayment)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
    /// `{Z}`: one mana from a source that could produce two or more colors.
    TwoOrMoreColorSource,
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
    /// Canonical printed symbol for this mana-cost component.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::White => "W",
            Self::Blue => "U",
            Self::Black => "B",
            Self::Red => "R",
            Self::Green => "G",
            Self::Colorless => "C",
            Self::Snow => "S",
            Self::X => "X",
            Self::TwoOrMoreColorSource => "Z",
            Self::WhiteBlue => "W/U",
            Self::WhiteBlack => "W/B",
            Self::BlueBlack => "U/B",
            Self::BlueRed => "U/R",
            Self::BlackRed => "B/R",
            Self::BlackGreen => "B/G",
            Self::RedWhite => "R/W",
            Self::RedGreen => "R/G",
            Self::GreenWhite => "G/W",
            Self::GreenBlue => "G/U",
            Self::TwoWhite => "2/W",
            Self::TwoBlue => "2/U",
            Self::TwoBlack => "2/B",
            Self::TwoRed => "2/R",
            Self::TwoGreen => "2/G",
            Self::PhyrexianWhite => "W/P",
            Self::PhyrexianBlue => "U/P",
            Self::PhyrexianBlack => "B/P",
            Self::PhyrexianRed => "R/P",
            Self::PhyrexianGreen => "G/P",
            Self::PhyrexianWhiteBlue => "W/U/P",
            Self::PhyrexianWhiteBlack => "W/B/P",
            Self::PhyrexianBlueBlack => "U/B/P",
            Self::PhyrexianBlueRed => "U/R/P",
            Self::PhyrexianBlackRed => "B/R/P",
            Self::PhyrexianBlackGreen => "B/G/P",
            Self::PhyrexianRedWhite => "R/W/P",
            Self::PhyrexianRedGreen => "R/G/P",
            Self::PhyrexianGreenWhite => "G/W/P",
            Self::PhyrexianGreenBlue => "G/U/P",
            Self::ColorlessWhite => "C/W",
            Self::ColorlessBlue => "C/U",
            Self::ColorlessBlack => "C/B",
            Self::ColorlessRed => "C/R",
            Self::ColorlessGreen => "C/G",
        }
    }

    /// CR 107.4e + CR 601.2b: the two nonhybrid mana symbols this hybrid symbol
    /// can be announced as, when BOTH halves are themselves single mana
    /// symbols.
    ///
    /// CR 601.2b: "If a cost that will be paid as the spell is being cast
    /// includes hybrid mana symbols, the player announces the nonhybrid
    /// equivalent cost they intend to pay." CR 107.4e: "Each one represents a
    /// cost that can be paid in one of two ways, as represented by the two
    /// halves of the symbol."
    ///
    /// `None` for the two hybrid families whose alternative half is NOT a mana
    /// symbol, and which therefore cannot be expressed as a shard substitution:
    ///   * the monocolored hybrids `{2/W}`..`{2/G}` (CR 107.4e), whose other
    ///     half is two generic mana, and
    ///   * every Phyrexian symbol (CR 107.4f), whose other half is 2 life.
    ///
    /// Both of those keep the engine's existing payment-time resolution, where
    /// the same choice is still made; only the shard-substitutable families
    /// participate in the CR 601.2f cost-determination election.
    pub const fn announceable_halves(self) -> Option<[Self; 2]> {
        match self {
            Self::WhiteBlue => Some([Self::White, Self::Blue]),
            Self::WhiteBlack => Some([Self::White, Self::Black]),
            Self::BlueBlack => Some([Self::Blue, Self::Black]),
            Self::BlueRed => Some([Self::Blue, Self::Red]),
            Self::BlackRed => Some([Self::Black, Self::Red]),
            Self::BlackGreen => Some([Self::Black, Self::Green]),
            Self::RedWhite => Some([Self::Red, Self::White]),
            Self::RedGreen => Some([Self::Red, Self::Green]),
            Self::GreenWhite => Some([Self::Green, Self::White]),
            Self::GreenBlue => Some([Self::Green, Self::Blue]),
            // CR 107.4c + CR 107.4e: `{C/W}`'s halves are `{C}` and `{W}`, both
            // single mana symbols, so this family substitutes cleanly too.
            Self::ColorlessWhite => Some([Self::Colorless, Self::White]),
            Self::ColorlessBlue => Some([Self::Colorless, Self::Blue]),
            Self::ColorlessBlack => Some([Self::Colorless, Self::Black]),
            Self::ColorlessRed => Some([Self::Colorless, Self::Red]),
            Self::ColorlessGreen => Some([Self::Colorless, Self::Green]),
            _ => None,
        }
    }

    /// CR 106.1b + CR 107.4: the mana types that can pay this symbol, for the
    /// symbols whose EVERY payment option is exactly one mana of a named type.
    ///
    /// This is the payment model behind [`ManaCost::is_payable_whenever`]: a
    /// symbol with a `Some` answer is paid by one mana, and that mana qualifies
    /// if and only if its type is in the returned set. The set has more than one
    /// member when the SYMBOL accepts more than one type — `{W/U}` takes white
    /// or blue — not because a mana carries several types at once; CR 106.1b
    /// gives each mana exactly one of six.
    ///
    /// `None` — "not modelled here, assume nothing" — for every symbol that can
    /// be paid some other way, or whose qualifying mana is picked out by
    /// something other than its type:
    ///   * `{X}` (CR 107.4b) is not a fixed amount of mana at all.
    ///   * the monocolored hybrids `{2/W}`..`{2/G}` (CR 107.4e) accept TWO
    ///     generic mana as their other half, so paying one is not paying one
    ///     mana of a named type.
    ///   * every Phyrexian symbol (CR 107.4f) accepts 2 life, which is not mana.
    ///   * `{S}` (CR 107.4h) accepts one mana of ANY type, but only from a snow
    ///     source — a restriction on the SOURCE, which a type set cannot
    ///     express.
    ///   * `{Z}` likewise restricts the source ("a source that could produce
    ///     two or more colors"), not the type.
    ///
    /// Callers must treat `None` as "no conclusion", never as "empty set":
    /// every comparison built on this is only as sound as its most conservative
    /// answer.
    pub const fn single_mana_payment_types(self) -> Option<ManaTypeSet> {
        match self {
            // CR 107.4a: one mana of that color.
            Self::White => Some(ManaTypeSet::of(ManaType::White)),
            Self::Blue => Some(ManaTypeSet::of(ManaType::Blue)),
            Self::Black => Some(ManaTypeSet::of(ManaType::Black)),
            Self::Red => Some(ManaTypeSet::of(ManaType::Red)),
            Self::Green => Some(ManaTypeSet::of(ManaType::Green)),
            // CR 107.4c: `{C}` is payable only with one colorless mana.
            Self::Colorless => Some(ManaTypeSet::of(ManaType::Colorless)),
            // CR 107.4e: one mana of either half.
            Self::WhiteBlue => Some(ManaTypeSet::of(ManaType::White).with(ManaType::Blue)),
            Self::WhiteBlack => Some(ManaTypeSet::of(ManaType::White).with(ManaType::Black)),
            Self::BlueBlack => Some(ManaTypeSet::of(ManaType::Blue).with(ManaType::Black)),
            Self::BlueRed => Some(ManaTypeSet::of(ManaType::Blue).with(ManaType::Red)),
            Self::BlackRed => Some(ManaTypeSet::of(ManaType::Black).with(ManaType::Red)),
            Self::BlackGreen => Some(ManaTypeSet::of(ManaType::Black).with(ManaType::Green)),
            Self::RedWhite => Some(ManaTypeSet::of(ManaType::Red).with(ManaType::White)),
            Self::RedGreen => Some(ManaTypeSet::of(ManaType::Red).with(ManaType::Green)),
            Self::GreenWhite => Some(ManaTypeSet::of(ManaType::Green).with(ManaType::White)),
            Self::GreenBlue => Some(ManaTypeSet::of(ManaType::Green).with(ManaType::Blue)),
            // CR 107.4c + CR 107.4e: both halves of `{C/W}` are single mana.
            Self::ColorlessWhite => {
                Some(ManaTypeSet::of(ManaType::Colorless).with(ManaType::White))
            }
            Self::ColorlessBlue => Some(ManaTypeSet::of(ManaType::Colorless).with(ManaType::Blue)),
            Self::ColorlessBlack => {
                Some(ManaTypeSet::of(ManaType::Colorless).with(ManaType::Black))
            }
            Self::ColorlessRed => Some(ManaTypeSet::of(ManaType::Colorless).with(ManaType::Red)),
            Self::ColorlessGreen => {
                Some(ManaTypeSet::of(ManaType::Colorless).with(ManaType::Green))
            }
            Self::Snow
            | Self::X
            | Self::TwoOrMoreColorSource
            | Self::TwoWhite
            | Self::TwoBlue
            | Self::TwoBlack
            | Self::TwoRed
            | Self::TwoGreen
            | Self::PhyrexianWhite
            | Self::PhyrexianBlue
            | Self::PhyrexianBlack
            | Self::PhyrexianRed
            | Self::PhyrexianGreen
            | Self::PhyrexianWhiteBlue
            | Self::PhyrexianWhiteBlack
            | Self::PhyrexianBlueBlack
            | Self::PhyrexianBlueRed
            | Self::PhyrexianBlackRed
            | Self::PhyrexianBlackGreen
            | Self::PhyrexianRedWhite
            | Self::PhyrexianRedGreen
            | Self::PhyrexianGreenWhite
            | Self::PhyrexianGreenBlue => None,
        }
    }

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
            | Self::Colorless | Self::Snow | Self::TwoOrMoreColorSource
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
            "Z" => Ok(ManaCostShard::TwoOrMoreColorSource),
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManaCost {
    NoCost,
    Cost {
        shards: Vec<ManaCostShard>,
        generic: u32,
    },
    /// The card's own mana cost (used for "the flashback cost is equal to its mana cost").
    SelfManaCost,
    /// The card's own mana value (CR 202.3), as generic mana only — used for
    /// "encore {X}, where X is its mana value" (Sliver Gravemother class).
    SelfManaValue,
    /// The card's own mana cost reduced by `reduction` generic mana — used for
    /// granted alt-cost keywords whose cost is "its mana cost reduced by {N}"
    /// (Dream Devourer foretell = MV−2, Aminatou miracle = MV−4).
    ///
    /// INVARIANT: this is a pre-resolution placeholder like `SelfManaCost` /
    /// `SelfManaValue`; it MUST be concretized by `resolve_keyword_mana_cost`
    /// (game/keywords.rs) into a concrete `Cost` before any arithmetic path
    /// (`plus` / `scaled` / `concretize_x`) — it never reaches them unresolved.
    /// CR 601.2f: reduction floors at {0} and only the generic component is reduced.
    SelfManaCostReduced {
        reduction: u32,
    },
}

/// One augmenting step of the bipartite matching behind
/// [`ManaCost::is_payable_whenever`]: try to match `index` in `mine` to some
/// still-unvisited shard of `theirs` whose payment types it accepts, displacing
/// an earlier match along an augmenting path when that is what it takes.
///
/// `matched[j]` is the `mine` index currently holding `theirs[j]`, if any.
fn match_shard(
    index: usize,
    mine: &[ManaTypeSet],
    theirs: &[ManaTypeSet],
    matched: &mut [Option<usize>],
    visited: &mut [bool],
) -> bool {
    for candidate in 0..theirs.len() {
        if visited[candidate] || !theirs[candidate].is_subset_of(mine[index]) {
            continue;
        }
        visited[candidate] = true;
        let free = match matched[candidate] {
            None => true,
            Some(holder) => match_shard(holder, mine, theirs, matched, visited),
        };
        if free {
            matched[candidate] = Some(index);
            return true;
        }
    }
    false
}

impl ManaCost {
    pub fn zero() -> Self {
        ManaCost::Cost {
            shards: Vec::new(),
            generic: 0,
        }
    }

    /// CR 118.9a: Whether this mana cost represents casting without paying mana
    /// (`NoCost`, or a zero `{0}` cost from `ExileWithAltCost` grants).
    pub fn is_without_paying_mana(&self) -> bool {
        match self {
            ManaCost::NoCost => true,
            ManaCost::Cost { shards, generic } => shards.is_empty() && *generic == 0,
            ManaCost::SelfManaCost
            | ManaCost::SelfManaValue
            | ManaCost::SelfManaCostReduced { .. } => false,
        }
    }

    /// CR 601.2f: reduce ONLY the generic component, saturating at 0; colored/
    /// hybrid/X pips (shards) untouched. An alternative cost's mana component
    /// can't be reduced below {0}.
    pub fn reduced_by_generic(&self, reduction: u32) -> ManaCost {
        match self {
            ManaCost::Cost { shards, generic } => ManaCost::Cost {
                shards: shards.clone(),
                generic: generic.saturating_sub(reduction),
            },
            other => other.clone(),
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
            ManaCost::NoCost
            | ManaCost::SelfManaCost
            | ManaCost::SelfManaValue
            | ManaCost::SelfManaCostReduced { .. } => 0,
            ManaCost::Cost { shards, generic } => {
                let shard_total: u32 = shards.iter().map(|s| s.mana_value_contribution()).sum();
                shard_total + generic
            }
        }
    }

    /// CR 601.2h + CR 107.4e: whether EVERY mana pool that can pay `other` can
    /// also pay `self` — "`self` is never harder to pay than `other`".
    ///
    /// Mana value cannot answer this. `{W}{W/U}` and `{G}{U}` both cost 2, but
    /// a `{G}{U}` pool pays the second and cannot pay the first, so neither
    /// dominates the other. Anything that discards one cost in favour of
    /// another has to compare what PAYS them.
    ///
    /// The test is a sufficient condition, deliberately not a complete one:
    ///
    ///  1. every shard of `self` is matched injectively to a shard of `other`
    ///     whose payment types are a SUBSET of the matched shard's
    ///     ([`ManaCostShard::single_mana_payment_types`]), so the mana that pays
    ///     the `other` shard necessarily also pays the `self` shard; and
    ///  2. `self.mana_value() <= other.mana_value()`, so the mana left over
    ///     after those matches covers `self`'s generic component (CR 107.4b:
    ///     generic mana is payable with mana of any type).
    ///
    /// Soundness: a pool paying `other` assigns one distinct mana to each of
    /// its shards; by (1) each matched `self` shard accepts the same mana, and
    /// the `other.mana_value() - self.shards.len()` mana beyond the matched
    /// ones — every shard of `other` is one mana, since an unmodelled shard
    /// fails the whole test — covers `self`'s generic by (2).
    ///
    /// `false` whenever the answer cannot be PROVEN: a non-`Cost` variant on
    /// either side, a shard whose payment options are not one mana of a named
    /// type (Phyrexian life, `{2/W}`'s two generic, `{S}`/`{Z}`'s source
    /// restriction), or simply no matching. A conservative `false` costs a
    /// caller an option it did not have to offer; a wrong `true` deletes a
    /// legal one.
    pub fn is_payable_whenever(&self, other: &ManaCost) -> bool {
        let (ManaCost::Cost { shards: mine, .. }, ManaCost::Cost { shards: theirs, .. }) =
            (self, other)
        else {
            return false;
        };
        if self.mana_value() > other.mana_value() || mine.len() > theirs.len() {
            return false;
        }
        let Some(mine) = mine
            .iter()
            .map(|shard| shard.single_mana_payment_types())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let Some(theirs) = theirs
            .iter()
            .map(|shard| shard.single_mana_payment_types())
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };

        // Kuhn's algorithm: grow a maximum bipartite matching one `self` shard
        // at a time, re-routing earlier matches along augmenting paths.
        let mut matched: Vec<Option<usize>> = vec![None; theirs.len()];
        (0..mine.len()).all(|index| {
            let mut visited = vec![false; theirs.len()];
            match_shard(index, &mine, &theirs, &mut matched, &mut visited)
        })
    }

    /// CR 702.143 (foretell-cost reduction) / CR 118.7: return this cost with its
    /// generic component reduced by `reduction`'s mana value (floored at 0),
    /// colored pips preserved. "Its foretell cost is equal to its mana cost
    /// reduced by {2}" applied to `{4}{U}{U}` yields `{2}{U}{U}`; `{U}` yields
    /// `{U}`; `{1}` reduced by `{2}` yields `{0}`. `NoCost` / `SelfManaCost` /
    /// `SelfManaValue` return `self` unchanged defensively — a recipient's printed
    /// cost is always a concrete `Cost`.
    pub fn reduced_generic_by(&self, reduction: &ManaCost) -> ManaCost {
        match self {
            ManaCost::Cost { shards, generic } => ManaCost::Cost {
                shards: shards.clone(),
                generic: generic.saturating_sub(reduction.mana_value()),
            },
            ManaCost::NoCost
            | ManaCost::SelfManaCost
            | ManaCost::SelfManaValue
            | ManaCost::SelfManaCostReduced { .. } => self.clone(),
        }
    }

    /// CR 107.3 + CR 202.3e: Whether this printed mana cost contains an `{X}`
    /// symbol. Independent of mana value (X contributes 0 to mana value off the
    /// stack per CR 202.3e), so "has {X} in its cost" must be detected from the
    /// shards directly — it is a structural property of the cost, not of its
    /// mana value. Consulted by spend restrictions whose Oracle text reads
    /// "spells with {X} in their mana costs" (Helga, Skittish Seer; Troyan,
    /// Gutsy Explorer).
    pub fn has_x(&self) -> bool {
        match self {
            ManaCost::NoCost
            | ManaCost::SelfManaCost
            | ManaCost::SelfManaValue
            | ManaCost::SelfManaCostReduced { .. } => false,
            ManaCost::Cost { shards, .. } => shards.iter().any(|s| matches!(s, ManaCostShard::X)),
        }
    }

    /// CR 107.4 + CR 202.1: Count colored mana symbols in this mana cost.
    /// `Some(c)` counts symbols contributing to color `c` (hybrid /
    /// monocolored-hybrid / Phyrexian symbols included via
    /// `ManaCostShard::contributes_to`). `None` counts each colored shard once
    /// regardless of color — CR 107.4a/107.4e/107.4f: a hybrid or Phyrexian
    /// symbol is a single colored mana symbol even though it is all of its
    /// component colors, so `{G/W}{G/W}` counts as 2 (not 4). Generic, X,
    /// colorless, and snow shards are not colored and never count.
    /// `NoCost`/`SelfManaCost`/`SelfManaValue` have no per-shard cost → 0.
    ///
    /// Single counting authority shared by `QuantityRef::ManaSymbolsInManaCost`
    /// (game/quantity.rs) and `FilterProp::ManaSymbolCount` (game/filter.rs).
    pub fn count_colored_pips(&self, color: Option<ManaColor>) -> i32 {
        match self {
            ManaCost::Cost { shards, .. } => {
                let count = shards
                    .iter()
                    .filter(|shard| match color {
                        Some(c) => shard.contributes_to(c),
                        None => ManaColor::ALL.iter().any(|c| shard.contributes_to(*c)),
                    })
                    .count();
                i32::try_from(count).unwrap_or(i32::MAX)
            }
            ManaCost::NoCost
            | ManaCost::SelfManaCost
            | ManaCost::SelfManaValue
            | ManaCost::SelfManaCostReduced { .. } => 0,
        }
    }

    /// CR 202.3e: X in a mana cost equals the announced value only while the
    /// object is on the stack; in every other zone, X contributes 0.
    ///
    /// CR 202.3e is scoped to "an object **with an {X} in its mana cost**", so the
    /// substitution is gated on [`ManaCost::has_x`]. An object's X (`cost_x_paid`,
    /// the CR 107.3i single-value-per-object channel) is not always a *mana* X: a
    /// spell whose X is defined by its own text rather than by an `{X}` symbol
    /// (Monstrous Onslaught `{4}{R}` — "where X is the greatest power among
    /// creatures you control as you cast this spell") carries an announced X with
    /// nothing in its cost to substitute it into. Adding it here would report that
    /// spell's mana value as 5+X on the stack and corrupt every mana-value-keyed
    /// interaction with it (Spell Pierce, Mana Leak, "spells with mana value 3 or
    /// less").
    pub fn mana_value_with_x(&self, zone: Zone, cost_x_paid: Option<u32>) -> u32 {
        self.mana_value()
            + match zone {
                Zone::Stack if self.has_x() => cost_x_paid.unwrap_or(0),
                _ => 0,
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

/// Parse an MTGJSON mana cost string (e.g. "{2}{W}{U}") into the engine's ManaCost type.
pub fn parse_mtgjson_mana_cost(s: &str) -> ManaCost {
    let s = s.trim();
    if s.is_empty() {
        return ManaCost::NoCost;
    }

    let mut generic: u32 = 0;
    let mut shards = Vec::new();

    // CR 107.4: `ManaCostShard::from_str` is the single authority for the
    // symbol→shard mapping. Routing every symbol through it keeps the loader
    // from drifting out of sync with the rest of the engine — previously this
    // function kept its own hand-maintained list that recognized mono-Phyrexian
    // (`{W/P}`) but not Phyrexian-hybrid (`{G/U/P}`) symbols, silently dropping
    // the pip from compleated planeswalkers and undercounting their cost by 1
    // (issue #1416, same class as #493's `{C/W}` drop). Anything `from_str`
    // doesn't recognize is either a bare number (generic mana) or ignorable.
    for segment in s.split('{').filter(|seg| !seg.is_empty()) {
        let symbol = segment.trim_end_matches('}');
        let symbol = symbol.to_ascii_uppercase();
        match ManaCostShard::from_str(&symbol) {
            Ok(shard) => shards.push(shard),
            Err(_) => {
                // A bare number is generic mana; any other unknown symbol is ignored.
                if let Ok(n) = symbol.parse::<u32>() {
                    generic += n;
                }
            }
        }
    }

    ManaCost::Cost { shards, generic }
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

fn is_false(value: &bool) -> bool {
    !*value
}

/// Serde default for `ManaUnit::pip_id`: legacy saves and freshly built units
/// carry the unstamped sentinel until pool entry stamps a real id.
fn unstamped_pip_id() -> ManaPipId {
    ManaPipId(0)
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

/// Typed failure while applying an already-selected exact pool removal.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum ExactManaRemovalError {
    #[error("duplicate exact mana pip {0:?}")]
    DuplicatePip(ManaPipId),
    #[error("missing exact mana pip {0:?}")]
    MissingPip(ManaPipId),
    #[error("mismatched exact mana unit for pip {0:?}")]
    MismatchedUnit(ManaPipId),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};

    #[test]
    fn mana_spell_grant_accepts_legacy_unqualified_counter_protection() {
        let grant: ManaSpellGrant = serde_json::from_str(r#""CantBeCountered""#)
            .expect("legacy unqualified mana grant deserializes");
        assert_eq!(
            grant,
            ManaSpellGrant::CantBeCountered {
                filter: TargetFilter::Any,
            }
        );

        let serialized = serde_json::to_string(&grant).expect("current grant serializes");
        let round_tripped: ManaSpellGrant =
            serde_json::from_str(&serialized).expect("current grant deserializes");
        assert_eq!(round_tripped, grant);
    }

    #[test]
    fn mana_spell_grant_migrates_legacy_spend_trigger_restrictions() {
        let ability = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        let cases = [
            (
                serde_json::json!("SharesCreatureTypeWithCommander"),
                TargetFilter::Typed(
                    TypedFilter::creature()
                        .properties(vec![FilterProp::SharesCreatureTypeWithCommander]),
                ),
            ),
            (
                serde_json::json!({"OnlyForCreatureType": "Dragon"}),
                TargetFilter::Typed(TypedFilter::creature().subtype("Dragon".to_string())),
            ),
            (
                serde_json::json!({"OnlyForSpellWithManaValue": {"comparator": "GE", "value": 4}}),
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
                    comparator: Comparator::GE,
                    value: QuantityExpr::Fixed { value: 4 },
                }])),
            ),
        ];

        for (restriction, filter) in cases {
            let legacy = serde_json::json!({
                "TriggerOnSpend": {
                    "restriction": restriction,
                    "ability": ability.clone(),
                }
            });
            let grant: ManaSpellGrant =
                serde_json::from_value(legacy).expect("legacy grant deserializes");
            assert_eq!(
                grant,
                ManaSpellGrant::TriggerOnSpend {
                    filter,
                    ability: Box::new(ability.clone()),
                }
            );
        }

        let without_restriction = serde_json::json!({
            "TriggerOnSpend": {
                "ability": ability.clone(),
            }
        });
        let grant: ManaSpellGrant = serde_json::from_value(without_restriction)
            .expect("unrestricted legacy grant deserializes");
        assert_eq!(
            grant,
            ManaSpellGrant::TriggerOnSpend {
                filter: TargetFilter::Any,
                ability: Box::new(ability.clone()),
            }
        );

        let out_of_range = serde_json::json!({
            "TriggerOnSpend": {
                "restriction": {
                    "OnlyForSpellWithManaValue": {
                        "comparator": "GE",
                        "value": 2_147_483_648u64,
                    }
                },
                "ability": ability,
            }
        });
        assert!(
            serde_json::from_value::<ManaSpellGrant>(out_of_range).is_err(),
            "an out-of-range legacy threshold must not wrap into a negative value"
        );
    }

    /// CR 702.143: `reduced_generic_by` reduces only the generic component,
    /// preserving colored pips and flooring at 0.
    #[test]
    fn reduced_generic_by_reduces_generic_and_preserves_pips() {
        // {4}{U}{U} - {2} = {2}{U}{U}
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue; 2],
            generic: 4,
        };
        let reduced = base.reduced_generic_by(&ManaCost::generic(2));
        assert_eq!(
            reduced,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Blue; 2],
                generic: 2,
            }
        );

        // {U} - {2} = {U} (no generic to reduce)
        let mono = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };
        assert_eq!(mono.reduced_generic_by(&ManaCost::generic(2)), mono);

        // {1} - {2} = {0} (floored)
        assert_eq!(
            ManaCost::generic(1).reduced_generic_by(&ManaCost::generic(2)),
            ManaCost::generic(0)
        );

        // Non-Cost variants return self unchanged.
        assert_eq!(
            ManaCost::NoCost.reduced_generic_by(&ManaCost::generic(2)),
            ManaCost::NoCost
        );
    }

    #[test]
    fn mana_color_serializes_as_string() {
        let color = ManaColor::White;
        let json = serde_json::to_value(color).unwrap();
        assert_eq!(json, "White");
    }

    #[test]
    fn deferred_mana_source_quantities_round_trip_with_distinct_wire_shapes() {
        for (quantity, wire) in [
            (
                ManaSourceQuantity::Fixed(3),
                serde_json::json!({
                    "type": "DeferredColorChoice",
                    "data": { "quantity": { "type": "Fixed", "data": 3 } },
                }),
            ),
            (
                ManaSourceQuantity::Variable,
                serde_json::json!({
                    "type": "DeferredColorChoice",
                    "data": { "quantity": { "type": "Variable" } },
                }),
            ),
        ] {
            let output = ManaSourceOutput::DeferredColorChoice { quantity };
            assert_eq!(serde_json::to_value(output).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<ManaSourceOutput>(wire).unwrap(),
                output
            );
        }
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
        assert!(unit.supertype == Some(ManaSupertype::Snow));
        assert_eq!(unit.restrictions.len(), 1);
    }

    #[test]
    fn restriction_allows_matching_spell_type() {
        let restriction = ManaRestriction::OnlyForSpellType("Creature".to_string());
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let instant_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let legendary_spell = SpellMeta {
            types: vec!["Legendary".to_string(), "Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&creature_spell));
        assert!(!restriction.allows_spell(&instant_spell));

        let legendary_restriction = ManaRestriction::OnlyForSpellType("Legendary".to_string());
        assert!(legendary_restriction.allows_spell(&legendary_spell));
        assert!(!legendary_restriction.allows_spell(&creature_spell));
    }

    // CR 106.6: A disjunctive restriction allows a spell if it satisfies ANY
    // inner branch (Maelstrom of the Spirit Dragon: Dragon spell OR Omen spell).
    #[test]
    fn restriction_only_for_any_allows_spell_matching_a_branch() {
        let restriction = ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Dragon".to_string()),
            ManaRestriction::OnlyForSpellType("Omen".to_string()),
        ]);
        let dragon_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Dragon".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let omen_spell = SpellMeta {
            types: vec!["Enchantment".to_string(), "Omen".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let goblin_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        // Matches one branch each.
        assert!(restriction.allows_spell(&dragon_spell));
        assert!(restriction.allows_spell(&omen_spell));
        // Matches no branch.
        assert!(!restriction.allows_spell(&goblin_spell));
    }

    // CR 106.6: A disjunction allows an activation if any branch's activation
    // half allows it (e.g. "… or to activate an ability of an Assassin source").
    #[test]
    fn restriction_only_for_any_allows_activation_via_branch() {
        let restriction = ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Assassin".to_string()),
            ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type: "Assassin".to_string(),
                ability: AbilityActivationScope::OfSpellType,
            },
        ]);
        // An Assassin source's ability is allowed via the second branch.
        assert!(restriction.allows_activation(
            &["Creature".to_string()],
            &["Assassin".to_string()],
            None
        ));
        // A non-Assassin source's ability is allowed by no branch.
        assert!(!restriction.allows_activation(
            &["Creature".to_string()],
            &["Goblin".to_string()],
            None
        ));
    }

    // CR 106.6: "Spend this mana only to cast Ninja spells" (Turtle Lair, issue
    // #3661) names a creature subtype. The restriction must match against
    // `SpellMeta.subtypes`, not only core types.
    #[test]
    fn restriction_spell_type_allows_subtype_spell() {
        let restriction = ManaRestriction::OnlyForSpellType("Ninja".to_string());
        let ninja_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Ninja".to_string(), "Turtle".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let turtle_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Turtle".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&ninja_creature));
        assert!(!restriction.allows_spell(&turtle_creature));
        assert!(!restriction.allows_spell(&goblin_creature));
    }

    #[test]
    fn restriction_spell_only_allows_spells_not_activations_or_effects() {
        let restriction = ManaRestriction::OnlyForSpell;
        let spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let source_types = vec!["Artifact".to_string()];
        let source_subtypes = Vec::new();

        assert!(restriction.allows(&PaymentContext::Spell(&spell)));
        assert!(!restriction.allows(&PaymentContext::Activation {
            source_types: &source_types,
            source_subtypes: &source_subtypes,
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
        assert!(!restriction.allows(&PaymentContext::Effect));
    }

    #[test]
    fn restriction_creature_type_requires_both_type_and_subtype() {
        let restriction = ManaRestriction::OnlyForCreatureType("Elf".to_string());
        let elf_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string(), "Warrior".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let elf_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&elf_creature));
        assert!(!restriction.allows_spell(&goblin_creature));
        assert!(!restriction.allows_spell(&elf_instant));
    }

    // CR 106.6: "Spend this mana only to cast Elemental spells or activate abilities
    // of Elemental sources" — "Elemental" names a creature subtype. The restriction
    // must match against both core types and subtypes on `SpellMeta`.
    #[test]
    fn restriction_type_or_ability_allows_subtype_creature_spell() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Elemental".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let elemental_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let tribal_elemental_instant = SpellMeta {
            types: vec!["Tribal".to_string(), "Instant".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let plain_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&elemental_creature));
        assert!(restriction.allows_spell(&tribal_elemental_instant));
        assert!(!restriction.allows_spell(&goblin_creature));
        assert!(!restriction.allows_spell(&plain_instant));
    }

    // CR 105.2c + CR 106.6: "colorless Eldrazi" is a compound quality phrase.
    // Both the colorless quality and Eldrazi subtype must be true.
    #[test]
    fn restriction_type_or_ability_requires_all_compound_spell_qualities() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Colorless Eldrazi".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let colorless_eldrazi = SpellMeta {
            types: vec!["Creature".to_string(), "Colorless".to_string()],
            subtypes: vec!["Eldrazi".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let colored_eldrazi = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Eldrazi".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let colorless_construct = SpellMeta {
            types: vec!["Artifact".to_string(), "Colorless".to_string()],
            subtypes: vec!["Construct".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&colorless_eldrazi));
        assert!(!restriction.allows_spell(&colored_eldrazi));
        assert!(!restriction.allows_spell(&colorless_construct));
    }

    // CR 106.6: The ability-activation half of the OR. An Elemental permanent is a
    // source whose subtypes include "Elemental"; activation must be permitted.
    #[test]
    fn restriction_type_or_ability_allows_subtype_activation() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Elemental".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let elemental_creature_types = vec!["Creature".to_string()];
        let elemental_subtypes = vec!["Elemental".to_string(), "Shaman".to_string()];
        assert!(restriction.allows_activation(
            &elemental_creature_types,
            &elemental_subtypes,
            None
        ));

        let goblin_subtypes = vec!["Goblin".to_string()];
        assert!(!restriction.allows_activation(&elemental_creature_types, &goblin_subtypes, None));

        // Core-type match also satisfies the check (e.g., "Artifact sources").
        let artifact_restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Artifact".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let artifact_types = vec!["Artifact".to_string()];
        let no_subtypes: Vec<String> = vec![];
        assert!(artifact_restriction.allows_activation(&artifact_types, &no_subtypes, None));
    }

    // CR 106.6: `AbilityActivationScope::Any` — "cast a colorless spell or to
    // activate an ability" (Sage of the Unknowable). The spell half stays
    // type-gated, while the ability half permits *any* activation regardless of
    // the source's types.
    #[test]
    fn restriction_type_or_any_ability_gates_spell_but_allows_any_activation() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Colorless".to_string(),
            ability: AbilityActivationScope::Any,
        };
        let colorless_spell = SpellMeta {
            types: vec!["Artifact".to_string(), "Colorless".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let colored_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        // Spell half: still gated to the named type.
        assert!(restriction.allows_spell(&colorless_spell));
        assert!(!restriction.allows_spell(&colored_spell));
        // Ability half: any activation is permitted, regardless of source type.
        assert!(restriction.allows_activation(&["Goblin".to_string()], &[], None));
        assert!(restriction.allows_activation(
            &["Land".to_string()],
            &["Forest".to_string()],
            None
        ));
    }

    // CR 106.6: Hydraulic Helper — "{T}: Add {U}. This mana can't be spent to
    // cast a nonartifact spell." The negative phrasing lowers to
    // `OnlyForTypeSpellsOrAbilities { spell_type: "Artifact", ability: Any }`:
    // the produced {U} may pay for an artifact spell but not for any nonartifact
    // spell (here, an instant), while leaving EVERY ability activation payable.
    // This is the runtime half of the discriminating coverage — it exercises the
    // single-authority `allows` dispatch (`allows_spell` for spells,
    // `allows_activation` for abilities) every engine spend path funnels through.
    // The discriminating assertion is the activation check: the buggy
    // `OnlyForSpellType("Artifact")` lowering returns `false` from
    // `allows_activation`, wrongly forbidding ability payment.
    #[test]
    fn hydraulic_helper_artifact_mana_casts_artifacts_and_pays_any_ability() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Artifact".to_string(),
            ability: AbilityActivationScope::Any,
        };
        let artifact_spell = SpellMeta {
            types: vec!["Artifact".to_string()],
            subtypes: vec!["Equipment".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let artifact_creature_spell = SpellMeta {
            types: vec!["Artifact".to_string(), "Creature".to_string()],
            subtypes: vec!["Golem".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let instant_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Human".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        // Permitted: any artifact spell (incl. artifact creatures).
        assert!(restriction.allows(&PaymentContext::Spell(&artifact_spell)));
        assert!(restriction.allows(&PaymentContext::Spell(&artifact_creature_spell)));
        // Rejected: nonartifact spells.
        assert!(!restriction.allows(&PaymentContext::Spell(&instant_spell)));
        assert!(!restriction.allows(&PaymentContext::Spell(&creature_spell)));
        // DISCRIMINATING: ability activation stays unrestricted regardless of the
        // source's types — the restriction governs only what spells may be cast.
        assert!(restriction.allows(&PaymentContext::Activation {
            source_types: &["Creature".to_string()],
            source_subtypes: &["Human".to_string()],
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
        assert!(restriction.allows(&PaymentContext::Activation {
            source_types: &["Land".to_string()],
            source_subtypes: &[],
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
    }

    #[test]
    fn restriction_artifact_spell_or_activation_uses_both_payment_contexts() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Artifact".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let artifact_spell = SpellMeta {
            types: vec!["Artifact".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let artifact_types = vec!["Artifact".to_string()];
        let creature_types = vec!["Creature".to_string()];
        let no_subtypes = Vec::new();

        assert!(restriction.allows(&PaymentContext::Spell(&artifact_spell)));
        assert!(!restriction.allows(&PaymentContext::Spell(&creature_spell)));
        assert!(restriction.allows(&PaymentContext::Activation {
            source_types: &artifact_types,
            source_subtypes: &no_subtypes,
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
        assert!(!restriction.allows(&PaymentContext::Activation {
            source_types: &creature_types,
            source_subtypes: &no_subtypes,
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
        assert!(!restriction.allows(&PaymentContext::Effect));
    }

    // CR 708.4: "Spend this mana only to cast face-down spells" (Tin Street
    // Gossip). The restriction permits a face-down cast and rejects a normal
    // face-up cast, ability activation, generic effects, and special actions.
    #[test]
    fn restriction_face_down_spell_only_allows_face_down_cast() {
        let restriction = ManaRestriction::OnlyForFaceDownSpell;
        let face_down_spell = SpellMeta {
            // CR 708.2a: a face-down spell is a typeless 2/2; what matters here
            // is the is_face_down flag, not the (cleared) type list.
            types: vec!["Creature".to_string()],
            is_face_down: true,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let face_up_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        // LEGAL: spending the mana on a face-down cast.
        assert!(restriction.allows(&PaymentContext::Spell(&face_down_spell)));
        // ILLEGAL: a normal face-up cast.
        assert!(!restriction.allows(&PaymentContext::Spell(&face_up_spell)));
        // ILLEGAL: ability activation, generic effect, and special actions.
        assert!(!restriction.allows(&PaymentContext::Activation {
            source_types: &["Creature".to_string()],
            source_subtypes: &[],
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
        assert!(!restriction.allows(&PaymentContext::Effect));
        assert!(!restriction.allows(&PaymentContext::SpecialAction(SpecialAction::UnlockDoor)));
        assert!(!restriction.allows(&PaymentContext::SpecialAction(SpecialAction::TurnFaceUp)));
    }

    // CR 116.2b + CR 702.37e: "turn permanents face up" lowers to a
    // TurnFaceUp special-action restriction. It accepts only the matching
    // special action and rejects every spell cast / activation / effect.
    #[test]
    fn restriction_turn_face_up_special_action_gate() {
        let restriction = ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp);
        // Accepts the matching special action.
        assert!(restriction.allows(&PaymentContext::SpecialAction(SpecialAction::TurnFaceUp)));
        // Rejects the unrelated door-unlock special action.
        assert!(!restriction.allows(&PaymentContext::SpecialAction(SpecialAction::UnlockDoor)));
        // Rejects spell casts (face-down or not), activations, and effects.
        let spell = SpellMeta {
            types: vec!["Creature".to_string()],
            is_face_down: true,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(!restriction.allows(&PaymentContext::Spell(&spell)));
        assert!(!restriction.allows(&PaymentContext::Activation {
            source_types: &["Creature".to_string()],
            source_subtypes: &[],
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Unrestricted,
        }));
        assert!(!restriction.allows(&PaymentContext::Effect));
    }

    // CR 106.6: Creeping Peeper's three-way disjunction
    // `Any([SpellType("Enchantment"), OnlyForSpecialAction(UnlockDoor),
    // OnlyForSpecialAction(TurnFaceUp)])` accepts an enchantment cast and the
    // door-unlock special action, and rejects a non-enchantment cast — the
    // disjunction routes each payment context to the correct branch.
    #[test]
    fn restriction_creeping_peeper_disjunction_routes_each_context() {
        let restriction = ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Enchantment".to_string()),
            ManaRestriction::OnlyForSpecialAction(SpecialAction::UnlockDoor),
            ManaRestriction::OnlyForSpecialAction(SpecialAction::TurnFaceUp),
        ]);
        let enchantment = SpellMeta {
            types: vec!["Enchantment".to_string()],
            ..SpellMeta::default()
        };
        let creature = SpellMeta {
            types: vec!["Creature".to_string()],
            ..SpellMeta::default()
        };
        // LEGAL: enchantment cast (first branch).
        assert!(restriction.allows(&PaymentContext::Spell(&enchantment)));
        // LEGAL: door-unlock special action (second branch).
        assert!(restriction.allows(&PaymentContext::SpecialAction(SpecialAction::UnlockDoor)));
        // ILLEGAL: a non-enchantment (creature) cast satisfies no branch.
        assert!(!restriction.allows(&PaymentContext::Spell(&creature)));
    }

    #[test]
    fn source_chosen_color_mana_restriction_uses_live_spell_colors() {
        let red_only = ManaRestriction::OnlyForSpellColor(ManaColor::Red);
        let red = SpellMeta {
            colors: vec![ManaColor::Red],
            ..SpellMeta::default()
        };
        let blue = SpellMeta {
            colors: vec![ManaColor::Blue],
            ..SpellMeta::default()
        };
        let multicolor = SpellMeta {
            colors: vec![ManaColor::Red, ManaColor::Blue],
            ..SpellMeta::default()
        };
        let colorless = SpellMeta::default();
        assert!(red_only.allows(&PaymentContext::Spell(&red)));
        assert!(red_only.allows(&PaymentContext::Spell(&multicolor)));
        assert!(!red_only.allows(&PaymentContext::Spell(&blue)));
        assert!(!red_only.allows(&PaymentContext::Spell(&colorless)));
        assert!(!ManaRestriction::Impossible.allows(&PaymentContext::Spell(&red)));
    }

    #[test]
    fn activation_color_constraint_rejects_wrong_actual_mana_color() {
        assert!(ActivationManaColorConstraint::Only(ManaColor::Red).permits(ManaType::Red));
        assert!(!ActivationManaColorConstraint::Only(ManaColor::Red).permits(ManaType::Blue));
        assert!(!ActivationManaColorConstraint::Only(ManaColor::Red).permits(ManaType::Colorless));
        assert!(!ActivationManaColorConstraint::Impossible.permits(ManaType::Red));
    }

    // CR 106.6 + CR 601.2g: "Spend this mana only to cast instant and sorcery
    // spells" (Tablet of Discovery, issue #1975) names a union of two distinct
    // spell types. Per the Melek, Izzet Paragon example (CR 601.3e), an "instant
    // and sorcery spells" permission lets a player cast a spell that is an
    // instant OR a sorcery — a single spell never needs to be both. The "and"
    // conjunction therefore distributes across the set of acceptable spells, the
    // same way " or " does, rather than requiring one spell to carry both types.
    #[test]
    fn restriction_instant_and_sorcery_allows_either_type() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Instant and Sorcery".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let sorcery = SpellMeta {
            types: vec!["Sorcery".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        // Manamorphose is an instant — the {R}{R} restricted mana must pay for it.
        assert!(restriction.allows_spell(&instant));
        assert!(restriction.allows_spell(&sorcery));
        assert!(!restriction.allows_spell(&creature));
    }

    // CR 105.2c + CR 106.6: The activation half uses the same compound-quality
    // predicate as spell casting.
    #[test]
    fn restriction_type_or_ability_requires_all_compound_activation_qualities() {
        let restriction = ManaRestriction::OnlyForTypeSpellsOrAbilities {
            spell_type: "Colorless Eldrazi".to_string(),
            ability: AbilityActivationScope::OfSpellType,
        };
        let colorless_creature_types = vec!["Creature".to_string(), "Colorless".to_string()];
        let eldrazi_subtypes = vec!["Eldrazi".to_string()];
        assert!(restriction.allows_activation(&colorless_creature_types, &eldrazi_subtypes, None));

        let colored_creature_types = vec!["Creature".to_string()];
        assert!(!restriction.allows_activation(&colored_creature_types, &eldrazi_subtypes, None));

        let construct_subtypes = vec!["Construct".to_string()];
        assert!(!restriction.allows_activation(
            &colorless_creature_types,
            &construct_subtypes,
            None
        ));
    }

    #[test]
    fn restriction_allows_matching_keyword_kind() {
        let restriction = ManaRestriction::OnlyForSpellWithKeywordKind(KeywordKind::Flashback);
        let flashback_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![KeywordKind::Flashback],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        let normal_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&flashback_spell));
        assert!(!restriction.allows_spell(&normal_spell));
    }

    // CR 106.6 + CR 202.3: "Spend this mana only to cast spells with mana value
    // N or greater" — the GE half of the parameterized mana-value gate.
    #[test]
    fn restriction_allows_spell_with_mana_value_ge_threshold() {
        let restriction = ManaRestriction::OnlyForSpellWithManaValue {
            comparator: Comparator::GE,
            value: 5,
        };
        let mv_six = SpellMeta {
            mana_value: Some(6),
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let mv_four = SpellMeta {
            mana_value: Some(4),
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let no_mv = SpellMeta::default();
        assert!(restriction.allows_spell(&mv_six));
        assert!(!restriction.allows_spell(&mv_four));
        // A spell with no known mana value is not eligible.
        assert!(!restriction.allows_spell(&no_mv));
    }

    // CR 106.6 + CR 202.3: the LE half of the parameterized mana-value gate
    // ("mana value N or less").
    #[test]
    fn restriction_allows_spell_with_mana_value_le_threshold() {
        let restriction = ManaRestriction::OnlyForSpellWithManaValue {
            comparator: Comparator::LE,
            value: 3,
        };
        let mv_two = SpellMeta {
            mana_value: Some(2),
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let mv_four = SpellMeta {
            mana_value: Some(4),
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(restriction.allows_spell(&mv_two));
        assert!(!restriction.allows_spell(&mv_four));
    }

    // CR 106.6: a mana-value gate names spell casting, so it rejects ability
    // activation regardless of comparator.
    #[test]
    fn restriction_mana_value_rejects_activation() {
        let restriction = ManaRestriction::OnlyForSpellWithManaValue {
            comparator: Comparator::GE,
            value: 5,
        };
        let source_types = vec!["Creature".to_string()];
        let source_subtypes: Vec<String> = vec![];
        assert!(!restriction.allows_activation(&source_types, &source_subtypes, None));
    }

    // CR 105.2 + CR 106.6: "Spend this mana only to cast spells with exactly N
    // colors" — the EQ reading of the parameterized color-count gate. A spell
    // with no recorded color count (None) is ineligible.
    #[test]
    fn restriction_allows_spell_with_color_count_eq() {
        let restriction = ManaRestriction::OnlyForSpellWithColorCount {
            comparator: Comparator::EQ,
            count: 3,
        };
        let three_colors = SpellMeta {
            color_count: Some(3),
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let two_colors = SpellMeta {
            color_count: Some(2),
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(restriction.allows_spell(&three_colors));
        assert!(!restriction.allows_spell(&two_colors));
        // No recorded color count → ineligible.
        assert!(!restriction.allows_spell(&SpellMeta::default()));
        // CR 105.2: a color-count gate names spell casting, so it rejects ability
        // activation.
        assert!(!restriction.allows_activation(&["Creature".to_string()], &[], None));
    }

    // CR 105.2: colorless spells have a color count of 0, so "exactly 0 colors"
    // matches colorless spells and rejects colored ones.
    #[test]
    fn restriction_allows_spell_with_color_count_colorless() {
        let restriction = ManaRestriction::OnlyForSpellWithColorCount {
            comparator: Comparator::EQ,
            count: 0,
        };
        let colorless = SpellMeta {
            color_count: Some(0),
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let one_color = SpellMeta {
            color_count: Some(1),
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(restriction.allows_spell(&colorless));
        assert!(!restriction.allows_spell(&one_color));
    }

    // CR 105.2 + CR 106.6: range comparators share the same color-count gate as
    // exact matching.
    #[test]
    fn restriction_allows_spell_with_color_count_ranges() {
        let two_or_more = ManaRestriction::OnlyForSpellWithColorCount {
            comparator: Comparator::GE,
            count: 2,
        };
        let two_or_fewer = ManaRestriction::OnlyForSpellWithColorCount {
            comparator: Comparator::LE,
            count: 2,
        };
        let three_colors = SpellMeta {
            color_count: Some(3),
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        let one_color = SpellMeta {
            color_count: Some(1),
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(two_or_more.allows_spell(&three_colors));
        assert!(!two_or_more.allows_spell(&one_color));
        assert!(two_or_fewer.allows_spell(&one_color));
        assert!(!two_or_fewer.allows_spell(&three_colors));
    }

    // CR 106.6 + CR 400.7: zone-gated spend allows only spells cast from the
    // named zone; a different zone or an unknown (None) origin is ineligible,
    // and the restriction never permits ability activation.
    #[test]
    fn restriction_allows_spell_from_zone() {
        let restriction = ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
            zone: Zone::Graveyard,
            polarity: ZoneSpendPolarity::From,
        });
        let from_gy = SpellMeta {
            cast_from_zone: Some(Zone::Graveyard),
            ..SpellMeta::default()
        };
        let from_exile = SpellMeta {
            cast_from_zone: Some(Zone::Exile),
            ..SpellMeta::default()
        };
        assert!(restriction.allows_spell(&from_gy));
        assert!(!restriction.allows_spell(&from_exile));
        // No recorded cast-from zone → ineligible.
        assert!(!restriction.allows_spell(&SpellMeta::default()));
        // Zone-gated spend is spell-casting only.
        assert!(!restriction.allows_activation(&["Creature".to_string()], &[], None));
    }

    // CR 106.6 + CR 400.7: the `NotFrom` polarity (Mm'menon, the Right Hand —
    // "from anywhere other than your hand") allows any cast-from zone except the
    // named one, and treats an unknown origin as ineligible.
    #[test]
    fn restriction_allows_spell_not_from_zone() {
        let restriction = ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
            zone: Zone::Hand,
            polarity: ZoneSpendPolarity::NotFrom,
        });
        let from_hand = SpellMeta {
            cast_from_zone: Some(Zone::Hand),
            ..SpellMeta::default()
        };
        let from_gy = SpellMeta {
            cast_from_zone: Some(Zone::Graveyard),
            ..SpellMeta::default()
        };
        // Cast from hand → forbidden; cast from any other zone → allowed.
        assert!(!restriction.allows_spell(&from_hand));
        assert!(restriction.allows_spell(&from_gy));
        // No recorded cast-from zone → ineligible (conservative).
        assert!(!restriction.allows_spell(&SpellMeta::default()));
        // Still spell-casting only — never permits ability activation.
        assert!(!restriction.allows_activation(&["Creature".to_string()], &[], None));
    }

    // CR 106.6 + CR 400.7: backward-compat for the zone-spend restriction. The
    // pre-polarity serialized form was a bare-string externally-tagged payload
    // (`{"OnlyForSpellFromZone":"Graveyard"}`); the custom `ZoneSpend`
    // deserializer must still accept it and map it to the inclusion (`From`)
    // reading, while the current `{zone, polarity}` form round-trips. This test
    // fails against a struct-variant encoding — serde cannot deserialize a bare
    // string into a struct variant — so it discriminates the newtype+ZoneSpend
    // backward-compat fix from the struct-variant regression it replaces.
    #[test]
    fn zone_spend_restriction_legacy_and_current_serde() {
        // Legacy bare-`Zone` payload (no `polarity` field existed yet).
        let legacy: ManaRestriction =
            serde_json::from_str(r#"{"OnlyForSpellFromZone":"Graveyard"}"#).unwrap();
        assert_eq!(
            legacy,
            ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
                zone: Zone::Graveyard,
                polarity: ZoneSpendPolarity::From,
            })
        );

        // Current `{zone, polarity}` object form with explicit NotFrom round-trips.
        let current = ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
            zone: Zone::Hand,
            polarity: ZoneSpendPolarity::NotFrom,
        });
        let json = serde_json::to_string(&current).unwrap();
        let round_tripped: ManaRestriction = serde_json::from_str(&json).unwrap();
        assert_eq!(current, round_tripped);

        // Current object form omitting `polarity` defaults to the inclusion reading.
        let defaulted: ManaRestriction =
            serde_json::from_str(r#"{"OnlyForSpellFromZone":{"zone":"Exile"}}"#).unwrap();
        assert_eq!(
            defaulted,
            ManaRestriction::OnlyForSpellFromZone(ZoneSpend {
                zone: Zone::Exile,
                polarity: ZoneSpendPolarity::From,
            })
        );
    }

    #[test]
    fn cannot_cast_from_zone_restriction_serde_round_trips() {
        let restriction = ManaRestriction::CannotCastSpellFromZone(Zone::Hand);
        let json = serde_json::to_string(&restriction).unwrap();
        assert_eq!(json, r#"{"CannotCastSpellFromZone":"Hand"}"#);
        assert_eq!(
            serde_json::from_str::<ManaRestriction>(&json).unwrap(),
            restriction
        );

        // ManaUnit is embedded in GameState and resolved-command journals, so
        // this proves the new externally tagged leaf survives the actual wire
        // carrier rather than only a standalone enum round trip.
        let unit = ManaUnit::new(ManaType::Green, ObjectId(7), false, vec![restriction]);
        let unit_json = serde_json::to_string(&unit).unwrap();
        let decoded: ManaUnit = serde_json::from_str(&unit_json).unwrap();
        assert_eq!(decoded, unit);
        assert!(unit_json.contains(r#""CannotCastSpellFromZone":"Hand""#));
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
        // CR 202.3e: {X}{R} → 0 + 1 = 1 (off-stack, X=0)
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        };
        assert_eq!(cost.mana_value(), 1);
    }

    #[test]
    fn mana_value_with_x_includes_chosen_value() {
        // CR 202.3e: {X}{R}{R} cast with X=4 → 4 + 1 + 1 = 6 while on the stack.
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        };
        assert_eq!(cost.mana_value_with_x(Zone::Stack, Some(4)), 6);
        assert_eq!(cost.mana_value_with_x(Zone::Stack, None), 2);
        assert_eq!(cost.mana_value_with_x(Zone::Stack, Some(0)), 2);
        assert_eq!(cost.mana_value_with_x(Zone::Battlefield, Some(4)), 2);
    }

    #[test]
    fn mana_value_with_x_no_x_shard_ignores_x_paid() {
        // CR 202.3e is conditioned on the object HAVING an {X} in its mana cost:
        // "When calculating the mana value of an object with an {X} in its mana
        // cost, X is treated as ... the number chosen for it while the object is
        // on the stack." A cost with no {X} shard has nothing to substitute, so
        // an object's X (`cost_x_paid`, CR 107.3i) must NOT leak into its mana
        // value.
        //
        // CORRECTED PIN. This previously asserted 8 and its comment asserted the
        // buggy premise as if it were the rule ("cost_x_paid is the announced X
        // value even when the cost expression has no literal {X} shard"). It is
        // not: {1}{R}{U} has mana value 3, full stop. Left uncorrected, any spell
        // whose X is defined by card text rather than by an {X} in its cost
        // (Monstrous Onslaught {4}{R} — "where X is the greatest power among
        // creatures you control as you cast this spell") would read as mana value
        // 5+X on the stack, breaking every mana-value-keyed interaction with it
        // (Spell Pierce, Mana Leak, "spells with mana value 3 or less").
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Blue],
            generic: 1,
        };
        assert_eq!(cost.mana_value_with_x(Zone::Stack, Some(5)), 3);
        assert_eq!(cost.mana_value_with_x(Zone::Stack, None), 3);
        assert_eq!(cost.mana_value_with_x(Zone::Graveyard, Some(5)), 3);
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

    // CR 702.6a: Ronin, Shadow Stalker — the disjunction
    // `OnlyForAny([OnlyForSpellType("Equipment"), OnlyForTaggedActivation(Equip)])`
    // must allow equip-tagged activations and Equipment spell casting, but reject
    // non-equip activated abilities on Equipment permanents.
    #[test]
    fn restriction_equip_tagged_allows_equip_activation() {
        let restriction = ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Equipment".to_string()),
            ManaRestriction::OnlyForTaggedActivation(AbilityTag::Equip),
        ]);
        // Equip-tagged activation on an Equipment source: ALLOWED.
        assert!(restriction.allows_activation(
            &["Artifact".to_string()],
            &["Equipment".to_string()],
            Some(AbilityTag::Equip),
        ));
    }

    // CR 702.6a: Non-equip activated ability on an Equipment permanent must be
    // REJECTED — the restriction is keyword-precise, not source-type-permissive.
    #[test]
    fn restriction_equip_tagged_rejects_non_equip_ability_on_equipment() {
        let restriction = ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Equipment".to_string()),
            ManaRestriction::OnlyForTaggedActivation(AbilityTag::Equip),
        ]);
        // Non-equip activation on an Equipment source: REJECTED.
        assert!(!restriction.allows_activation(
            &["Artifact".to_string()],
            &["Equipment".to_string()],
            None,
        ));
        // Non-equip activation with a different tag: also REJECTED.
        assert!(!restriction.allows_activation(
            &["Artifact".to_string()],
            &["Equipment".to_string()],
            Some(AbilityTag::Boast),
        ));
    }

    // CR 702.6a: Equipment spell casting is still allowed by the spell-type branch.
    #[test]
    fn restriction_equip_tagged_allows_equipment_spell() {
        let restriction = ManaRestriction::OnlyForAny(vec![
            ManaRestriction::OnlyForSpellType("Equipment".to_string()),
            ManaRestriction::OnlyForTaggedActivation(AbilityTag::Equip),
        ]);
        let equipment_spell = SpellMeta {
            types: vec!["Artifact".to_string()],
            subtypes: vec!["Equipment".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(restriction.allows_spell(&equipment_spell));
        // Non-Equipment artifact spell: REJECTED.
        let artifact_spell = SpellMeta {
            types: vec!["Artifact".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(!restriction.allows_spell(&artifact_spell));
    }

    // -----------------------------------------------------------------------
    // CR 601.2h + CR 107.4e: payment-set dominance.
    //
    // `is_payable_whenever` is the discriminator that decides whether one cost
    // can stand in for another. Mana value cannot answer that question, and
    // these rows pin both directions of why.
    // -----------------------------------------------------------------------

    fn cost(shards: &[ManaCostShard], generic: u32) -> ManaCost {
        ManaCost::Cost {
            shards: shards.to_vec(),
            generic,
        }
    }

    /// The Rigo case: `{W}{W/U}` and `{G}{U}` both cost 2, and NEITHER is
    /// payable whenever the other is. A `{G}{U}` pool pays the second and
    /// cannot pay the first; a `{W}{W}` pool pays the first and cannot pay the
    /// second. An equal-mana-value filter calls these interchangeable, which is
    /// exactly the defect this method exists to prevent.
    #[test]
    fn equal_mana_value_does_not_make_two_costs_interchangeable() {
        let hybrid = cost(&[ManaCostShard::White, ManaCostShard::WhiteBlue], 0);
        let announced = cost(&[ManaCostShard::Green, ManaCostShard::Blue], 0);
        assert_eq!(hybrid.mana_value(), announced.mana_value());
        assert!(!hybrid.is_payable_whenever(&announced));
        assert!(!announced.is_payable_whenever(&hybrid));
    }

    /// CR 107.4e: a hybrid symbol IS payable every way either half is, so a
    /// cost that only differs by announcing halves is dominated by the hybrid
    /// form — at equal mana value, which is the case the filter must still
    /// discard.
    #[test]
    fn a_hybrid_cost_is_payable_whenever_its_announced_form_is() {
        let hybrid = cost(&[ManaCostShard::White, ManaCostShard::WhiteBlue], 0);
        for announced in [
            cost(&[ManaCostShard::White, ManaCostShard::Blue], 0),
            cost(&[ManaCostShard::White, ManaCostShard::White], 0),
            // The matching must be injective AND order-insensitive: here the
            // plain `{W}` has to take the second shard so the hybrid can take
            // the first.
            cost(&[ManaCostShard::Blue, ManaCostShard::White], 0),
        ] {
            assert!(
                hybrid.is_payable_whenever(&announced),
                "{hybrid:?} must be payable whenever {announced:?} is"
            );
        }
    }

    /// CR 107.4b: generic mana is payable with mana of any type, so leftover
    /// mana beyond the matched shards covers a generic component — but only as
    /// far as the mana value comparison allows.
    #[test]
    fn generic_is_covered_by_leftover_mana_only_within_the_mana_value() {
        assert!(cost(&[], 1).is_payable_whenever(&cost(&[ManaCostShard::Blue], 1)));
        assert!(cost(&[], 2)
            .is_payable_whenever(&cost(&[ManaCostShard::White, ManaCostShard::Blue], 0)));
        assert!(!cost(&[], 3)
            .is_payable_whenever(&cost(&[ManaCostShard::White, ManaCostShard::Blue], 0)));
        assert!(!cost(&[ManaCostShard::White], 1)
            .is_payable_whenever(&cost(&[ManaCostShard::White], 0)));
    }

    /// A shard whose payment options are not "one mana of a named type" is not
    /// modelled, and an unmodelled shard on EITHER side must abstain rather
    /// than guess. `{W/P}`'s life half and `{2/W}`'s two generic mana would
    /// both make a colour-set comparison unsound.
    #[test]
    fn unmodelled_shards_abstain_rather_than_claim_dominance() {
        for shard in [
            ManaCostShard::PhyrexianWhite,
            ManaCostShard::TwoWhite,
            ManaCostShard::Snow,
            ManaCostShard::TwoOrMoreColorSource,
            ManaCostShard::X,
        ] {
            assert!(
                shard.single_mana_payment_types().is_none(),
                "{shard:?} is not one mana of a named type and must not be modelled"
            );
            assert!(
                !cost(&[ManaCostShard::White], 0).is_payable_whenever(&cost(&[shard], 1)),
                "an unmodelled {shard:?} on the right must abstain"
            );
            assert!(
                !cost(&[shard], 0).is_payable_whenever(&cost(&[ManaCostShard::White], 1)),
                "an unmodelled {shard:?} on the left must abstain"
            );
        }
    }

    /// CR 107.4c: `{C}` is payable only with colorless mana, so it is a type in
    /// the model like any colour — and `{C/W}` is the hybrid of the two.
    #[test]
    fn colorless_participates_in_the_payment_model() {
        assert_eq!(
            ManaCostShard::ColorlessWhite.single_mana_payment_types(),
            Some(ManaTypeSet::of(ManaType::Colorless).with(ManaType::White))
        );
        assert!(cost(&[ManaCostShard::ColorlessWhite], 0)
            .is_payable_whenever(&cost(&[ManaCostShard::Colorless], 0)));
        assert!(!cost(&[ManaCostShard::Colorless], 0)
            .is_payable_whenever(&cost(&[ManaCostShard::White], 0)));
    }

    /// A non-`Cost` variant carries no shards to compare, so the answer is
    /// "not proven" in both directions.
    #[test]
    fn non_concrete_costs_are_never_proven_dominant() {
        for other in [
            ManaCost::NoCost,
            ManaCost::SelfManaCost,
            ManaCost::SelfManaValue,
            ManaCost::SelfManaCostReduced { reduction: 2 },
        ] {
            assert!(!cost(&[], 0).is_payable_whenever(&other));
            assert!(!other.is_payable_whenever(&cost(&[], 0)));
        }
    }

    #[test]
    fn mana_type_sets_compare_as_sets() {
        let white = ManaTypeSet::of(ManaType::White);
        let white_blue = white.with(ManaType::Blue);
        assert!(white.is_subset_of(white_blue));
        assert!(!white_blue.is_subset_of(white));
        assert!(ManaTypeSet::EMPTY.is_subset_of(white));
        assert!(ManaTypeSet::EMPTY.is_empty());
        assert!(white_blue.contains(ManaType::Blue));
        assert!(!white_blue.contains(ManaType::Green));
        assert_eq!(
            [ManaType::White, ManaType::Blue]
                .into_iter()
                .collect::<ManaTypeSet>(),
            white_blue
        );
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_helpers {
    use super::*;

    pub const MANA_TYPES: [ManaType; 6] = [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
        ManaType::Colorless,
    ];

    /// A `Vec<ManaUnit>` pool with the unit-vector semantics the shape pool must reproduce.
    #[derive(Clone, Default)]
    pub struct ReferencePool(pub Vec<ManaUnit>);

    impl ReferencePool {
        pub fn swap_remove_where(&mut self, pred: impl Fn(&ManaShape) -> bool) -> Option<ManaUnit> {
            let pos = self.0.iter().position(|unit| pred(&shape_of(unit)))?;
            Some(self.0.swap_remove(pos))
        }

        pub fn spend_for(&mut self, color: ManaType, ctx: &PaymentContext<'_>) -> Option<ManaUnit> {
            self.swap_remove_where(|m| {
                m.color == color
                    && ctx.permits_actual_mana_type(m.color)
                    && m.restrictions.is_empty()
            })
            .or_else(|| {
                self.swap_remove_where(|m| {
                    m.color == color
                        && ctx.permits_actual_mana_type(m.color)
                        && !m.restrictions.is_empty()
                        && m.restrictions.iter().all(|r| r.allows(ctx))
                })
            })
        }

        pub fn spend_generic(&mut self, count: usize) -> bool {
            if self.0.iter().filter(|m| m.restrictions.is_empty()).count() < count {
                return false;
            }
            let mut remaining = count;
            self.0.retain(|m| {
                if remaining == 0 || !m.restrictions.is_empty() {
                    return true;
                }
                remaining -= 1;
                false
            });
            true
        }

        pub fn remove_exact(&mut self, units: &[ManaUnit]) -> Result<(), ExactManaRemovalError> {
            for unit in units {
                match self
                    .0
                    .iter()
                    .position(|c| c.pip_id == unit.pip_id && c == unit)
                {
                    Some(pos) => {
                        self.0.swap_remove(pos);
                    }
                    None if self.0.iter().any(|c| c.pip_id == unit.pip_id) => {
                        return Err(ExactManaRemovalError::MismatchedUnit(unit.pip_id));
                    }
                    None => return Err(ExactManaRemovalError::MissingPip(unit.pip_id)),
                }
            }
            Ok(())
        }

        pub fn edit_expiry(
            &mut self,
            edit: impl Fn(Option<ManaExpiry>) -> Option<Option<ManaExpiry>>,
        ) {
            for unit in &mut self.0 {
                if let Some(expiry) = edit(unit.expiry) {
                    unit.expiry = expiry;
                }
            }
        }
    }

    pub fn shape_of(unit: &ManaUnit) -> ManaShape {
        unit.clone().into_shape_and_pip().0
    }
}
