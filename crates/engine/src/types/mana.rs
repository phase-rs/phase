use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::ability::{AbilityTag, Comparator};
use super::events::GameEvent;
use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};
use super::player::PlayerId;
use super::zones::Zone;

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

/// CR 107.4f + CR 118.3a + CR 118.3b: Set of mana colors for which a player
/// may substitute 2 life rather than 1 colored mana at payment time, granted
/// by static abilities like K'rrik, Son of Yawgmoth ("For each {B} in a cost,
/// you may pay 2 life rather than pay that mana"). Bitmask over `ManaColor`.
///
/// This is a payment-time *permission*, not a cost rewrite: shards become
/// Phyrexian-shaped only when the paying player has the grant; the printed
/// cost on the spell is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LifePaymentColors(u8);

impl LifePaymentColors {
    pub const EMPTY: Self = Self(0);

    pub const fn contains(self, c: ManaColor) -> bool {
        self.0 & (1 << c as u8) != 0
    }

    pub fn insert(&mut self, c: ManaColor) {
        self.0 |= 1 << c as u8;
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl FromIterator<ManaColor> for LifePaymentColors {
    fn from_iter<I: IntoIterator<Item = ManaColor>>(it: I) -> Self {
        let mut s = Self::EMPTY;
        for c in it {
            s.insert(c);
        }
        s
    }
}

/// CR 118.1: Payment-time permission bundle for the player paying a cost.
///
/// Computed once per cost-payment entry (cast or activate) and threaded
/// through the dry-run, pause-decision, and execution helpers. All three
/// permissions are derived projections of player state at payment time and
/// share the same abstraction layer, so they are bundled to avoid an
/// ever-growing positional-argument list across `can_pay_for_spell`,
/// `compute_phyrexian_shards`, `maybe_pause_for_phyrexian_choice`, etc.
#[derive(Debug, Clone, Copy, Default)]
pub struct CostPermissionContext {
    /// CR 609.4b: `SpendManaAsAnyColor` grants (Chromatic Lantern etc.).
    pub any_color: bool,
    /// CR 119.8 budget: maximum life this player can spend on Phyrexian-shape
    /// shards in this payment (respects CantLoseLife → 0).
    pub max_life: u32,
    /// CR 107.4f + CR 118.3b: colors for which the player may pay 2 life
    /// rather than 1 colored mana (K'rrik-style grants).
    pub life_colors: LifePaymentColors,
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

/// CR 614.1a + CR 703.4q: Per-unit decision for the CR 703.4q step-end empty
/// event. Each entry in `ProposedEvent::EmptyManaPool::units` describes one
/// `ManaUnit` in the affected player's pool and how the replacement pipeline
/// has chosen to resolve it.
///
/// `pool_index` is the unit's position in `ManaPool::mana` at the time the
/// event was constructed. The disposition walker (commit 2) iterates in
/// descending index order so removals don't invalidate later indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitDecision {
    pub pool_index: usize,
    pub color: ManaType,
    pub disposition: UnitDisposition,
}

/// CR 614.1a + CR 614.6 + CR 703.4q: How a single unit in a step-end empty
/// event will be resolved after the replacement pipeline finishes.
///
/// - `Drop`: default — the unit empties per CR 703.4q. A handler matching this
///   unit may flip the disposition to `Keep` (CR 614.6) or `Recolor(_)`
///   (CR 614.1a).
/// - `Keep`: a `StepEndManaAction::Retain` handler has applied; the unit stays
///   in the pool.
/// - `Recolor(_)`: a `StepEndManaAction::Transform(_)` handler has applied; the
///   unit stays in the pool with its color rewritten to the target type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnitDisposition {
    Drop,
    Keep,
    Recolor(ManaType),
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
    /// CR 202.3: Mana value of the spell being cast, consulted by mana-value
    /// spend restrictions (`OnlyForSpellWithManaValue`). `None` at payment
    /// sites with no associated spell mana value.
    pub mana_value: Option<u32>,
    /// CR 105.2: Number of colors of the spell being cast, consulted by
    /// color-count spend restrictions (`OnlyForSpellWithColorCount`). `None` at
    /// payment sites with no associated spell.
    pub color_count: Option<u32>,
    /// CR 107.3 + CR 202.3e: Whether the spell's printed mana cost contains an
    /// `{X}` symbol. Consulted by the "with {X} in their mana costs" disjunct of
    /// MV/X spend restrictions (`OnlyForSpellMatchingCostCriteria`). `false` at
    /// payment sites with no associated spell, or when the spell has no `{X}`.
    pub has_x_in_cost: bool,
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

/// CR 116.2: A class of special action whose mana cost can be the subject of a
/// CR 106.6 mana-spend restriction ("Spend this mana only to unlock doors").
///
/// Only special actions that pay a mana cost *through the mana pool* with a
/// restriction-aware payment context belong here. CR 116.2m / CR 709.5e door
/// unlock is the first such action (its unlock cost routes through
/// `pay_special_action_mana_cost`). CR 116.2b turn-face-up does not yet pay its
/// morph/disguise cost through a restriction-aware pool payment, so it is
/// intentionally absent — its spend restriction is honest-deferred rather than
/// silently over-permitted. New variants are added only once the corresponding
/// special action's payment is routed through `PaymentContext::SpecialAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecialAction {
    /// CR 116.2m + CR 709.5e: Paying a locked Room half's unlock cost to give
    /// the permanent the appropriate unlocked designation.
    UnlockDoor,
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
    /// CR 106.6 + CR 205.3m + CR 903.3: "a creature spell that shares a creature
    /// type with your commander" — relational spend filter for Path of Ancestry.
    /// This is a distinct *relational-to-commander* axis, not a fixed-subtype
    /// parameterization of `OnlyForCreatureType`: the matching subtype set is
    /// computed from live commander state per spend, so it cannot be evaluated
    /// from `SpellMeta` alone. `allows_spell` returns `false` (no commander
    /// context here); the real relational evaluation happens at the
    /// `apply_mana_spell_grants` spend-check site, which has game-state access —
    /// mirroring how `OnlyForXCosts` defers full detection to its call site.
    SharesCreatureTypeWithCommander,
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
    /// CR 702.51a: Internal marker for a convoke tap that substitutes for
    /// paying mana. The payment algorithm may consume it for the current spell,
    /// but cast-spent metrics and mana-added triggers must ignore it.
    ConvokePayment,
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
            // subtypes against the controller's commander's creature types) is done
            // at the `apply_mana_spell_grants` call site, which has `&GameState`.
            // Mirrors the deferral contract of `OnlyForXCosts`.
            ManaRestriction::SharesCreatureTypeWithCommander => false,
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
            // CR 106.6: X-cost restriction — conservatively disallow for spells.
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
            // CR 106.6: Disjunction — the spell is payable if it satisfies any branch.
            ManaRestriction::OnlyForAny(subs) => subs.iter().any(|r| r.allows_spell(meta)),
            // CR 116.2: Special-action-only mana never pays for a spell cast.
            ManaRestriction::OnlyForSpecialAction(_) => false,
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
            | ManaRestriction::SharesCreatureTypeWithCommander
            | ManaRestriction::OnlyForSpellWithKeywordKind(_)
            | ManaRestriction::OnlyForSpellWithKeywordKindFromZone(_, _)
            | ManaRestriction::OnlyForSpellWithManaValue { .. }
            | ManaRestriction::OnlyForSpellMatchingCostCriteria { .. }
            | ManaRestriction::OnlyForSpellWithColorCount { .. }
            | ManaRestriction::OnlyForSpellFromZone(_)
            // CR 116.2: Special-action-only mana never pays for ability activation.
            | ManaRestriction::OnlyForSpecialAction(_) => false,
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
            } => self.allows_activation(source_types, source_subtypes, *ability_tag),
            PaymentContext::Effect => false,
            // CR 116.2: A special-action payment is permitted only by mana that
            // is restricted to that exact special-action class, or by a
            // disjunction that contains such a branch. Spell/activation/generic
            // effect restrictions all reject it.
            PaymentContext::SpecialAction(action) => self.allows_special_action(*action),
        }
    }

    /// CR 106.6 + CR 116.2: Returns `true` if this restriction permits spending
    /// mana on the given special action (e.g. a Room door unlock). Only
    /// [`ManaRestriction::OnlyForSpecialAction`] with a matching action — or a
    /// disjunction containing one — qualifies; every spell/activation/effect
    /// restriction rejects special actions.
    pub fn allows_special_action(&self, action: SpecialAction) -> bool {
        match self {
            ManaRestriction::OnlyForSpecialAction(allowed) => *allowed == action,
            ManaRestriction::OnlyForAny(subs) => {
                subs.iter().any(|r| r.allows_special_action(action))
            }
            _ => false,
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
    /// CR 106.6 + CR 603.3: "When you spend this mana to cast a [filter] spell,
    /// [effect]" — a reflexive trigger riding the produced mana (Lapis Orb of
    /// Dragonkind, Scaled Nurturer, Gilanra). When the mana is spent on a spell
    /// satisfying `restriction` (`None` = any spell), the controller's `ability`
    /// is put on the stack as a triggered ability. The ability's source is the
    /// permanent that produced the mana.
    TriggerOnSpend {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restriction: Option<ManaRestriction>,
        ability: Box<crate::types::ability::AbilityDefinition>,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaUnit {
    pub color: ManaType,
    pub source_id: ObjectId,
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
    pub fn is_snow(&self) -> bool {
        matches!(self.supertype, Some(ManaSupertype::Snow))
    }

    pub fn convoke_payment(color: ManaType, source_id: ObjectId) -> Self {
        Self {
            color,
            source_id,
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

    /// CR 118.9a: Whether this mana cost represents casting without paying mana
    /// (`NoCost`, or a zero `{0}` cost from `ExileWithAltCost` grants).
    pub fn is_without_paying_mana(&self) -> bool {
        match self {
            ManaCost::NoCost => true,
            ManaCost::Cost { shards, generic } => shards.is_empty() && *generic == 0,
            ManaCost::SelfManaCost => false,
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

    /// CR 107.3 + CR 202.3e: Whether this printed mana cost contains an `{X}`
    /// symbol. Independent of mana value (X contributes 0 to mana value off the
    /// stack per CR 202.3e), so "has {X} in its cost" must be detected from the
    /// shards directly — it is a structural property of the cost, not of its
    /// mana value. Consulted by spend restrictions whose Oracle text reads
    /// "spells with {X} in their mana costs" (Helga, Skittish Seer; Troyan,
    /// Gutsy Explorer).
    pub fn has_x(&self) -> bool {
        match self {
            ManaCost::NoCost | ManaCost::SelfManaCost => false,
            ManaCost::Cost { shards, .. } => shards.iter().any(|s| matches!(s, ManaCostShard::X)),
        }
    }

    /// CR 202.3e: X in a mana cost equals the announced value only while the
    /// object is on the stack; in every other zone, X contributes 0.
    pub fn mana_value_with_x(&self, zone: Zone, cost_x_paid: Option<u32>) -> u32 {
        self.mana_value()
            + match zone {
                Zone::Stack => cost_x_paid.unwrap_or(0),
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

    pub fn produced_mana_total(&self) -> usize {
        self.mana
            .iter()
            .filter(|unit| !unit.is_convoke_payment())
            .count()
    }

    pub fn clear(&mut self) {
        self.mana.clear();
    }

    /// CR 500.5 + CR 614.6 + CR 703.4q: Drop only expiry-bound units whose
    /// explicit rule fires on this transition. Runs FIRST during per-player
    /// drain in `drain_pending_phase_transition_progress`, before the
    /// CR 703.4q "empty unspent mana" event is constructed for the
    /// replacement pipeline. Preserves H2 invariant (commit `e92fd3e19`):
    /// expiry-bound mana leaves the pool through its own expiry rule, not
    /// through the step-end empty event — handlers cannot intercept it.
    ///
    /// - `EndOfTurn`: drops at cleanup only.
    /// - `EndOfCombat`: drops when leaving combat (i.e., `in_combat` false).
    /// - `None`: untouched — passed through to the replacement pipeline as
    ///   a `UnitDecision { disposition: Drop }`, where step-end mana
    ///   handlers (Upwelling, Horizon Stone, Kruphix, …) may flip the
    ///   disposition to `Keep` (CR 614.6) or `Recolor(_)` (CR 614.1a). The
    ///   actual emptying / recoloring of `None`-expiry units happens later
    ///   in `apply_empty_mana_pool_decisions` after the pipeline resolves.
    pub fn clear_expiring_at_step_end(&mut self, in_combat: bool, entering_cleanup: bool) {
        self.mana.retain(|u| match u.expiry {
            Some(ManaExpiry::EndOfTurn) => !entering_cleanup,
            Some(ManaExpiry::EndOfCombat) => in_combat,
            None => true,
        });
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

/// CR 614.1a + CR 614.6 + CR 703.4q: Apply per-unit dispositions decided by
/// the replacement pipeline to a player's mana pool. Single authority for
/// the disposition→pool-mutation walk; called by
/// `drain_pending_phase_transition_progress` and by the
/// `EmptyManaPool` resume arm of `handle_replacement_choice`.
///
/// Walks `units` in descending `pool_index` order so removals do not
/// invalidate later indices. Disposition resolution:
/// - `Drop`: remove the unit; emit `GameEvent::ManaPoolEmptied`.
/// - `Keep`: leave the unit in place (a `Retain` handler matched per CR 614.6).
/// - `Recolor(t)`: mutate `unit.color = t`; emit `GameEvent::ManaRecolored`
///   (a `Transform(_)` handler matched per CR 614.1a).
///
/// Pool-position stability across the pipeline is guaranteed by the
/// surrounding drain: no priority is granted between event construction and
/// disposition apply, and per CR 603.2 triggered abilities wait to be put on
/// the stack — they do not fire mid-resolution.
pub fn apply_empty_mana_pool_decisions(
    state: &mut crate::types::game_state::GameState,
    player_id: PlayerId,
    units: &[UnitDecision],
    events: &mut Vec<GameEvent>,
) {
    let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return;
    };
    // Descending pool_index order preserves index validity across removes.
    let mut sorted: Vec<&UnitDecision> = units.iter().collect();
    sorted.sort_by_key(|d| std::cmp::Reverse(d.pool_index));
    let mut changed = false;
    for decision in sorted {
        match decision.disposition {
            UnitDisposition::Drop => {
                if decision.pool_index < player.mana_pool.mana.len() {
                    let removed = player.mana_pool.mana.remove(decision.pool_index);
                    changed = true;
                    events.push(GameEvent::ManaPoolEmptied {
                        player_id,
                        source_id: removed.source_id,
                        color: removed.color,
                    });
                }
            }
            UnitDisposition::Keep => {}
            UnitDisposition::Recolor(to) => {
                if let Some(unit) = player.mana_pool.mana.get_mut(decision.pool_index) {
                    let from = unit.color;
                    unit.color = to;
                    changed = true;
                    events.push(GameEvent::ManaRecolored {
                        player_id,
                        from,
                        to,
                    });
                }
            }
        }
    }
    if changed {
        state.layers_dirty.mark_full();
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

    // CR 500.5 + CR 703.4q: `clear_expiring_at_step_end` is the leading
    // half of step-end mana resolution — it drops only expiry-bound units
    // whose own rule fires on this transition. Handler-driven retention /
    // transformation behavior is exercised end-to-end via the replacement
    // pipeline in `game::turns` runtime tests, not here.
    #[test]
    fn mana_pool_clear_expiring_drops_end_of_turn_only_at_cleanup() {
        let mut pool = ManaPool::default();
        let mut retained = make_unit(ManaType::Green);
        retained.expiry = Some(ManaExpiry::EndOfTurn);
        pool.add(retained);
        pool.add(make_unit(ManaType::Red));

        // Non-cleanup transition: EndOfTurn unit survives; non-expiry unit
        // is left in place (the pipeline drives Drop disposition elsewhere).
        pool.clear_expiring_at_step_end(false, false);
        assert_eq!(pool.count_color(ManaType::Green), 1);
        assert_eq!(pool.count_color(ManaType::Red), 1);

        // Cleanup transition: EndOfTurn unit drops; non-expiry unit remains.
        pool.clear_expiring_at_step_end(false, true);
        assert_eq!(pool.count_color(ManaType::Green), 0);
        assert_eq!(pool.count_color(ManaType::Red), 1);
    }

    #[test]
    fn mana_pool_clear_expiring_drops_end_of_combat_when_leaving_combat() {
        let mut pool = ManaPool::default();
        let mut combat_mana = make_unit(ManaType::Red);
        combat_mana.expiry = Some(ManaExpiry::EndOfCombat);
        pool.add(combat_mana);

        // In-combat transition (e.g., DeclareAttackers → DeclareBlockers):
        // EndOfCombat unit survives.
        pool.clear_expiring_at_step_end(true, false);
        assert_eq!(pool.count_color(ManaType::Red), 1);

        // Leaving combat (EndCombat → PostCombatMain): EndOfCombat unit drops.
        pool.clear_expiring_at_step_end(false, false);
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
        assert!(unit.is_snow());
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
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let instant_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let legendary_spell = SpellMeta {
            types: vec!["Legendary".to_string(), "Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let omen_spell = SpellMeta {
            types: vec!["Enchantment".to_string(), "Omen".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let goblin_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let turtle_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Turtle".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let source_types = vec!["Artifact".to_string()];
        let source_subtypes = Vec::new();

        assert!(restriction.allows(&PaymentContext::Spell(&spell)));
        assert!(!restriction.allows(&PaymentContext::Activation {
            source_types: &source_types,
            source_subtypes: &source_subtypes,
            ability_tag: None,
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
            has_x_in_cost: false,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let elf_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        assert!(restriction.allows_spell(&elf_creature));
        assert!(!restriction.allows_spell(&goblin_creature));
        assert!(!restriction.allows_spell(&elf_instant));
    }

    #[test]
    fn shares_creature_type_with_commander_defers_to_call_site() {
        // CR 106.6 + CR 903.3: The relational commander-subtype filter carries no
        // commander context in `SpellMeta`, so `allows_spell`/`allows_activation`
        // must return `false` for every payment context. The authoritative
        // relational evaluation happens at the `apply_mana_spell_grants` spend
        // site (game-state aware). This documents the deferral contract.
        let restriction = ManaRestriction::SharesCreatureTypeWithCommander;
        let elf_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        assert!(!restriction.allows_spell(&elf_creature));
        let source_types = vec!["Creature".to_string()];
        let source_subtypes = vec!["Elf".to_string()];
        assert!(!restriction.allows_activation(&source_types, &source_subtypes, None));
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
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let tribal_elemental_instant = SpellMeta {
            types: vec!["Tribal".to_string(), "Instant".to_string()],
            subtypes: vec!["Elemental".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let plain_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let colored_eldrazi = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Eldrazi".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let colorless_construct = SpellMeta {
            types: vec!["Artifact".to_string(), "Colorless".to_string()],
            subtypes: vec!["Construct".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let colored_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let artifact_creature_spell = SpellMeta {
            types: vec!["Artifact".to_string(), "Creature".to_string()],
            subtypes: vec!["Golem".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let instant_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Human".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
        }));
        assert!(restriction.allows(&PaymentContext::Activation {
            source_types: &["Land".to_string()],
            source_subtypes: &[],
            ability_tag: None,
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
            has_x_in_cost: false,
        };
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
        }));
        assert!(!restriction.allows(&PaymentContext::Activation {
            source_types: &creature_types,
            source_subtypes: &no_subtypes,
            ability_tag: None,
        }));
        assert!(!restriction.allows(&PaymentContext::Effect));
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
            has_x_in_cost: false,
        };
        let sorcery = SpellMeta {
            types: vec!["Sorcery".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
        };
        let creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
        };
        let normal_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        let mv_four = SpellMeta {
            mana_value: Some(4),
            color_count: None,
            has_x_in_cost: false,
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
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        let mv_four = SpellMeta {
            mana_value: Some(4),
            color_count: None,
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        assert!(restriction.allows_spell(&mv_two));
        assert!(!restriction.allows_spell(&mv_four));
    }

    #[test]
    fn spend_for_enforces_mana_value_restriction() {
        let mut pool = ManaPool::default();
        pool.add(make_restricted_unit(
            ManaType::Green,
            ObjectId(1),
            vec![ManaRestriction::OnlyForSpellWithManaValue {
                comparator: Comparator::GE,
                value: 5,
            }],
        ));

        let mv_four = SpellMeta {
            mana_value: Some(4),
            color_count: None,
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&mv_four))
            .is_none());
        assert_eq!(pool.total(), 1);

        let mv_five = SpellMeta {
            mana_value: Some(5),
            color_count: None,
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&mv_five))
            .is_some());
        assert_eq!(pool.total(), 0);
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
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        let two_colors = SpellMeta {
            color_count: Some(2),
            has_x_in_cost: false,
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
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        let one_color = SpellMeta {
            color_count: Some(1),
            has_x_in_cost: false,
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
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        let one_color = SpellMeta {
            color_count: Some(1),
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        assert!(two_or_more.allows_spell(&three_colors));
        assert!(!two_or_more.allows_spell(&one_color));
        assert!(two_or_fewer.allows_spell(&one_color));
        assert!(!two_or_fewer.allows_spell(&three_colors));
    }

    #[test]
    fn spend_for_enforces_color_count_restriction() {
        let mut pool = ManaPool::default();
        pool.add(make_restricted_unit(
            ManaType::Green,
            ObjectId(1),
            vec![ManaRestriction::OnlyForSpellWithColorCount {
                comparator: Comparator::GE,
                count: 2,
            }],
        ));

        let one_color = SpellMeta {
            color_count: Some(1),
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&one_color))
            .is_none());
        assert_eq!(pool.total(), 1);

        let two_colors = SpellMeta {
            color_count: Some(2),
            has_x_in_cost: false,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&two_colors))
            .is_some());
        assert_eq!(pool.total(), 0);
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
    fn mana_value_with_x_no_x_shard_adds_x_paid() {
        // On the stack, cost_x_paid is the announced X value even when the cost
        // expression has no literal {X} shard.
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Blue],
            generic: 1,
        };
        assert_eq!(cost.mana_value_with_x(Zone::Stack, Some(5)), 8); // 1R+1U+1 generic = 3, +5 = 8
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
}
