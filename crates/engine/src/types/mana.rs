pub use phase_ast::types::mana::*;

use serde::{Deserialize, Serialize};

use super::ability::{AbilityTag, Comparator, TriggerDefinitionOccurrenceRef};
use super::events::GameEvent;
use super::game_state::ProductionOverride;
use super::identifiers::{ObjectId, ObjectIncarnationRef};
use super::phase::Phase;
use super::player::PlayerId;

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
    /// CR 118.14 + CR 609.4b: preserve whether the concession also covers {C}.
    pub mana_spend_permission: Option<super::ability::ManaSpendPermission>,
    /// CR 119.8 budget: maximum number of 2-life payments this player can
    /// afford for Phyrexian-shape shards in this payment (respects
    /// CantLoseLife → 0). `life_costs::max_phyrexian_life_payments` owns the
    /// floor-division, so this is a payment count, not a life total.
    pub max_life: u32,
    /// CR 107.4f + CR 118.3b: colors for which the player may pay 2 life
    /// rather than 1 colored mana (K'rrik-style grants).
    pub life_colors: LifePaymentColors,
}

/// CR 614.1a + CR 703.4q: Per-unit decision for the CR 703.4q step-end empty
/// event. Each entry in `ProposedEvent::EmptyManaPool::units` describes one
/// `ManaUnit` in the affected player's pool and how the replacement pipeline
/// has chosen to resolve it.
///
/// `pool_index` is the unit's position in pool order (`ManaPool::units`) at the
/// time the event was constructed. The disposition walker (commit 2) iterates in
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

/// CR 605.3b: Semantic classification of an activatable mana source's
/// irreversible consequences. This is part of a submitted mana-source
/// selection because two otherwise-identical rows with different penalties
/// are not the same activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaSourcePenalty {
    None,
    HasIrreversibleContinuation,
    DealsDamageOnResolution { fixed_amount: Option<u16> },
    PaysLifeOnActivation { fixed_amount: Option<u16> },
    Sacrifices,
}

/// Exact identity of one triggered mana ability that augments a land's own
/// production when that land is tapped for mana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TapsForManaSelection {
    pub source: ObjectIncarnationRef,
    pub occurrence: TriggerDefinitionOccurrenceRef,
    pub production_override: ProductionOverride,
}

/// Stable semantic selection for one currently activatable mana-source row.
///
/// The action carries every field that can change legality or resolution, but
/// the reducer never trusts those fields directly: it enumerates current live
/// options and requires one exact semantic match before activating that live
/// option. `ObjectIncarnationRef` prevents a stale choice from rebinding to an
/// object that left and re-entered the battlefield (CR 400.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManaSourceSelection {
    pub source: ObjectIncarnationRef,
    pub ability_index: Option<usize>,
    pub mana_type: ManaType,
    pub output: ManaSourceOutput,
    pub atomic_combination: Option<Vec<ManaType>>,
    pub restrictions: Vec<ManaRestriction>,
    pub penalty: ManaSourcePenalty,
    pub taps_for_mana: Vec<TapsForManaSelection>,
}

impl<'de> Deserialize<'de> for ManaSourceSelection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireSelection {
            source: ObjectIncarnationRef,
            ability_index: Option<usize>,
            mana_type: ManaType,
            #[serde(default)]
            output: Option<ManaSourceOutput>,
            atomic_combination: Option<Vec<ManaType>>,
            restrictions: Vec<ManaRestriction>,
            penalty: ManaSourcePenalty,
            taps_for_mana: Vec<TapsForManaSelection>,
        }

        let wire = WireSelection::deserialize(deserializer)?;
        Ok(Self {
            source: wire.source,
            ability_index: wire.ability_index,
            mana_type: wire.mana_type,
            // `TapLandForMana` actions saved before output provenance existed
            // selected this exact row's `mana_type`; preserve that choice on
            // replay instead of silently turning every old colored row colorless.
            output: wire
                .output
                .unwrap_or(ManaSourceOutput::Concrete(wire.mana_type)),
            atomic_combination: wire.atomic_combination,
            restrictions: wire.restrictions,
            penalty: wire.penalty,
            taps_for_mana: wire.taps_for_mana,
        })
    }
}

impl ManaSourceSelection {
    /// Stable total order for the action wire. The non-`Ord` semantic fields
    /// are compared by explicit closed-enum comparators below; debug strings or
    /// serialized bytes must never become ordering authority.
    pub fn cmp_stable(&self, other: &Self) -> std::cmp::Ordering {
        self.source
            .cmp(&other.source)
            .then_with(|| self.ability_index.cmp(&other.ability_index))
            .then_with(|| self.mana_type.cmp(&other.mana_type))
            .then_with(|| cmp_mana_source_output(self.output, other.output))
            .then_with(|| self.atomic_combination.cmp(&other.atomic_combination))
            .then_with(|| cmp_mana_restriction_slices(&self.restrictions, &other.restrictions))
            .then_with(|| cmp_mana_source_penalty(self.penalty, other.penalty))
            .then_with(|| cmp_taps_for_mana_slices(&self.taps_for_mana, &other.taps_for_mana))
    }
}

fn cmp_mana_source_output(left: ManaSourceOutput, right: ManaSourceOutput) -> std::cmp::Ordering {
    match (left, right) {
        (ManaSourceOutput::Concrete(a), ManaSourceOutput::Concrete(b)) => a.cmp(&b),
        (
            ManaSourceOutput::DeferredColorChoice { quantity: a },
            ManaSourceOutput::DeferredColorChoice { quantity: b },
        ) => a.cmp(&b),
        (ManaSourceOutput::Concrete(_), ManaSourceOutput::DeferredColorChoice { .. }) => {
            std::cmp::Ordering::Less
        }
        (ManaSourceOutput::DeferredColorChoice { .. }, ManaSourceOutput::Concrete(_)) => {
            std::cmp::Ordering::Greater
        }
    }
}

fn cmp_slice_by<T>(
    left: &[T],
    right: &[T],
    cmp: impl Copy + Fn(&T, &T) -> std::cmp::Ordering,
) -> std::cmp::Ordering {
    left.iter()
        .zip(right)
        .map(|(a, b)| cmp(a, b))
        .find(|ordering| !ordering.is_eq())
        .unwrap_or_else(|| left.len().cmp(&right.len()))
}

fn cmp_taps_for_mana_slices(
    left: &[TapsForManaSelection],
    right: &[TapsForManaSelection],
) -> std::cmp::Ordering {
    cmp_slice_by(left, right, |a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.occurrence.cmp(&b.occurrence))
            .then_with(|| cmp_production_override(&a.production_override, &b.production_override))
    })
}

fn cmp_production_override(
    left: &ProductionOverride,
    right: &ProductionOverride,
) -> std::cmp::Ordering {
    use ProductionOverride::{Combination, SingleColor};
    match (left, right) {
        (SingleColor(a), SingleColor(b)) => a.cmp(b),
        (Combination(a), Combination(b)) => a.cmp(b),
        (SingleColor(_), Combination(_)) => std::cmp::Ordering::Less,
        (Combination(_), SingleColor(_)) => std::cmp::Ordering::Greater,
    }
}

fn cmp_mana_source_penalty(
    left: ManaSourcePenalty,
    right: ManaSourcePenalty,
) -> std::cmp::Ordering {
    fn rank(value: ManaSourcePenalty) -> u8 {
        match value {
            ManaSourcePenalty::None => 0,
            ManaSourcePenalty::HasIrreversibleContinuation => 1,
            ManaSourcePenalty::DealsDamageOnResolution { .. } => 2,
            ManaSourcePenalty::PaysLifeOnActivation { .. } => 3,
            ManaSourcePenalty::Sacrifices => 4,
        }
    }
    rank(left)
        .cmp(&rank(right))
        .then_with(|| match (left, right) {
            (
                ManaSourcePenalty::DealsDamageOnResolution { fixed_amount: a },
                ManaSourcePenalty::DealsDamageOnResolution { fixed_amount: b },
            )
            | (
                ManaSourcePenalty::PaysLifeOnActivation { fixed_amount: a },
                ManaSourcePenalty::PaysLifeOnActivation { fixed_amount: b },
            ) => a.cmp(&b),
            _ => std::cmp::Ordering::Equal,
        })
}

fn cmp_mana_restriction_slices(
    left: &[ManaRestriction],
    right: &[ManaRestriction],
) -> std::cmp::Ordering {
    cmp_slice_by(left, right, cmp_mana_restriction)
}

#[allow(clippy::too_many_lines)]
fn cmp_mana_restriction(left: &ManaRestriction, right: &ManaRestriction) -> std::cmp::Ordering {
    fn rank(value: &ManaRestriction) -> u8 {
        match value {
            ManaRestriction::OnlyForSpell => 0,
            ManaRestriction::OnlyForSpellType(_) => 1,
            ManaRestriction::OnlyForCreatureType(_) => 2,
            ManaRestriction::OnlyForTypeSpellsOrAbilities { .. } => 3,
            ManaRestriction::OnlyForActivation => 4,
            ManaRestriction::OnlyForTaggedActivation(_) => 5,
            ManaRestriction::OnlyForXCosts => 6,
            ManaRestriction::OnlyForSpellWithKeywordKind(_) => 7,
            ManaRestriction::OnlyForSpellWithKeywordKindFromZone(_, _) => 8,
            ManaRestriction::OnlyForSpellWithManaValue { .. } => 9,
            ManaRestriction::OnlyForSpellMatchingCostCriteria { .. } => 10,
            ManaRestriction::OnlyForSpellWithColorCount { .. } => 11,
            ManaRestriction::OnlyForSpellColor(_) => 12,
            ManaRestriction::OnlyForSpellFromZone(_) => 13,
            ManaRestriction::CannotCastSpellFromZone(_) => 14,
            ManaRestriction::OnlyForFaceDownSpell => 15,
            ManaRestriction::OnlyForAny(_) => 16,
            ManaRestriction::OnlyForSpecialAction(_) => 17,
            ManaRestriction::Impossible => 18,
            ManaRestriction::ConvokePayment => 19,
        }
    }
    rank(left).cmp(&rank(right)).then_with(|| match (left, right) {
        (ManaRestriction::OnlyForSpellType(a), ManaRestriction::OnlyForSpellType(b))
        | (
            ManaRestriction::OnlyForCreatureType(a),
            ManaRestriction::OnlyForCreatureType(b),
        ) => a.cmp(b),
        (
            ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type: a_type,
                ability: a_ability,
            },
            ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type: b_type,
                ability: b_ability,
            },
        ) => a_type
            .cmp(b_type)
            .then_with(|| ability_activation_scope_rank(*a_ability).cmp(&ability_activation_scope_rank(*b_ability))),
        (
            ManaRestriction::OnlyForTaggedActivation(a),
            ManaRestriction::OnlyForTaggedActivation(b),
        ) => ability_tag_rank(*a).cmp(&ability_tag_rank(*b)),
        (
            ManaRestriction::OnlyForSpellWithKeywordKind(a),
            ManaRestriction::OnlyForSpellWithKeywordKind(b),
        ) => a.cmp(b),
        (
            ManaRestriction::OnlyForSpellWithKeywordKindFromZone(a_kind, a_zone),
            ManaRestriction::OnlyForSpellWithKeywordKindFromZone(b_kind, b_zone),
        ) => a_kind.cmp(b_kind).then_with(|| a_zone.cmp(b_zone)),
        (
            ManaRestriction::OnlyForSpellWithManaValue {
                comparator: a_cmp,
                value: a_value,
            },
            ManaRestriction::OnlyForSpellWithManaValue {
                comparator: b_cmp,
                value: b_value,
            },
        ) => comparator_rank(*a_cmp)
            .cmp(&comparator_rank(*b_cmp))
            .then_with(|| a_value.cmp(b_value)),
        (
            ManaRestriction::OnlyForSpellMatchingCostCriteria {
                spell_type: a_type,
                criteria: a_criteria,
            },
            ManaRestriction::OnlyForSpellMatchingCostCriteria {
                spell_type: b_type,
                criteria: b_criteria,
            },
        ) => a_type
            .cmp(b_type)
            .then_with(|| cmp_slice_by(a_criteria, b_criteria, cmp_spell_cost_criterion)),
        (
            ManaRestriction::OnlyForSpellWithColorCount {
                comparator: a_cmp,
                count: a_count,
            },
            ManaRestriction::OnlyForSpellWithColorCount {
                comparator: b_cmp,
                count: b_count,
            },
        ) => comparator_rank(*a_cmp)
            .cmp(&comparator_rank(*b_cmp))
            .then_with(|| a_count.cmp(b_count)),
        (ManaRestriction::OnlyForSpellColor(a), ManaRestriction::OnlyForSpellColor(b)) => {
            a.cmp(b)
        }
        (
            ManaRestriction::OnlyForSpellFromZone(a),
            ManaRestriction::OnlyForSpellFromZone(b),
        ) => a.zone.cmp(&b.zone).then_with(|| {
            zone_spend_polarity_rank(a.polarity).cmp(&zone_spend_polarity_rank(b.polarity))
        }),
        (
            ManaRestriction::CannotCastSpellFromZone(a),
            ManaRestriction::CannotCastSpellFromZone(b),
        ) => a.cmp(b),
        (ManaRestriction::OnlyForAny(a), ManaRestriction::OnlyForAny(b)) => {
            cmp_mana_restriction_slices(a, b)
        }
        (
            ManaRestriction::OnlyForSpecialAction(a),
            ManaRestriction::OnlyForSpecialAction(b),
        ) => special_action_rank(*a).cmp(&special_action_rank(*b)),
        _ => std::cmp::Ordering::Equal,
    })
}

fn cmp_spell_cost_criterion(
    left: &SpellCostCriterion,
    right: &SpellCostCriterion,
) -> std::cmp::Ordering {
    match (left, right) {
        (
            SpellCostCriterion::ManaValue {
                comparator: a_cmp,
                value: a_value,
            },
            SpellCostCriterion::ManaValue {
                comparator: b_cmp,
                value: b_value,
            },
        ) => comparator_rank(*a_cmp)
            .cmp(&comparator_rank(*b_cmp))
            .then_with(|| a_value.cmp(b_value)),
        (SpellCostCriterion::HasXInCost, SpellCostCriterion::HasXInCost) => {
            std::cmp::Ordering::Equal
        }
        (SpellCostCriterion::ManaValue { .. }, SpellCostCriterion::HasXInCost) => {
            std::cmp::Ordering::Less
        }
        (SpellCostCriterion::HasXInCost, SpellCostCriterion::ManaValue { .. }) => {
            std::cmp::Ordering::Greater
        }
    }
}

fn comparator_rank(value: Comparator) -> u8 {
    match value {
        Comparator::GT => 0,
        Comparator::LT => 1,
        Comparator::GE => 2,
        Comparator::LE => 3,
        Comparator::EQ => 4,
        Comparator::NE => 5,
    }
}

fn ability_activation_scope_rank(value: AbilityActivationScope) -> u8 {
    match value {
        AbilityActivationScope::OfSpellType => 0,
        AbilityActivationScope::Any => 1,
    }
}

fn ability_tag_rank(value: AbilityTag) -> u8 {
    match value {
        AbilityTag::Boast => 0,
        AbilityTag::Evolve => 1,
        AbilityTag::Exhaust => 2,
        AbilityTag::Outlast => 3,
        AbilityTag::Cycling => 4,
        AbilityTag::Backup => 5,
        AbilityTag::PowerUp => 6,
        AbilityTag::Equip => 7,
        AbilityTag::Augment => 8,
    }
}

fn zone_spend_polarity_rank(value: ZoneSpendPolarity) -> u8 {
    match value {
        ZoneSpendPolarity::From => 0,
        ZoneSpendPolarity::NotFrom => 1,
    }
}

fn special_action_rank(value: SpecialAction) -> u8 {
    match value {
        SpecialAction::CompanionToHand => 0,
        SpecialAction::UnlockDoor => 1,
        SpecialAction::Plot => 2,
        SpecialAction::TurnFaceUp => 3,
        SpecialAction::RollPlanarDie => 4,
        SpecialAction::EndContinuousEffect => 5,
        SpecialAction::Foretell => 6,
    }
}

/// A unit's place in pool order: slots order units exactly as positions in a
/// `Vec<ManaUnit>` pool would.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PoolSlot(u64);

/// Every unit of one shape in a pool, keyed by slot and by pip.
#[derive(Debug, Clone)]
struct PoolEntry {
    shape: ManaShape,
    slots: im::OrdMap<PoolSlot, ManaPipId>,
    by_pip: im::OrdMap<ManaPipId, im::OrdSet<PoolSlot>>,
}

impl PoolEntry {
    fn first_slot(&self) -> PoolSlot {
        self.slots.get_min().expect("a pool entry is never empty").0
    }

    fn first_slot_of(&self, pip: ManaPipId) -> Option<PoolSlot> {
        self.by_pip
            .get(&pip)
            .and_then(|slots| slots.get_min().copied())
    }

    fn insert(&mut self, slot: PoolSlot, pip: ManaPipId) {
        self.slots.insert(slot, pip);
        self.by_pip.entry(pip).or_default().insert(slot);
    }

    fn remove(&mut self, slot: PoolSlot) -> Option<ManaPipId> {
        let pip = self.slots.remove(&slot)?;
        if let Some(slots) = self.by_pip.get_mut(&pip) {
            slots.remove(&slot);
            if slots.is_empty() {
                self.by_pip.remove(&pip);
            }
        }
        Some(pip)
    }
}

/// Whether every unit's pip is known nonzero, unique in the pool, and journaled
/// as produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum PoolProvenance {
    #[default]
    Verified,
    Unverified,
}

#[derive(Serialize, Deserialize)]
struct ManaPoolWire {
    mana: Vec<ManaUnit>,
}

impl From<ManaPoolWire> for ManaPool {
    fn from(wire: ManaPoolWire) -> Self {
        ManaPool::from_units(wire.mana)
    }
}

impl From<ManaPool> for ManaPoolWire {
    fn from(pool: ManaPool) -> Self {
        ManaPoolWire {
            mana: pool.units().collect(),
        }
    }
}

/// CR 106.4: a player's mana pool, held as one entry per distinct unit shape.
///
/// Pool order is slot order, the order a `Vec<ManaUnit>` pool would present.
/// [`Self::shapes`] yields entries in the order they were created, which is not
/// pool order. A reader whose output is a sequence whose order is used (a zone
/// order, a choice list, an event order, an order-sensitive hash, or a scratch
/// pool handed to an order-dependent function) reads [`Self::units`]. A reader
/// that needs the first unit matching a predicate reads [`Self::first_where`],
/// [`Self::first_pinned_where`] or [`Self::unit_by_pip`], each of which answers
/// at the minimum matching slot. Every other reader (counts, `any`/`all`, pip
/// membership) reads [`Self::shapes`] or an operation built on it.
#[derive(Default, Serialize, Deserialize)]
#[cfg_attr(not(feature = "test-support"), derive(Clone))]
#[serde(from = "ManaPoolWire", into = "ManaPoolWire")]
pub struct ManaPool {
    entries: Vec<PoolEntry>,
    next_slot: u64,
    provenance: PoolProvenance,
}

// Counts the slots a copy does not share with its original.
#[cfg(feature = "test-support")]
impl Clone for ManaPool {
    fn clone(&self) -> Self {
        use crate::game::perf_counters::Unshared;
        let copy = Self {
            entries: self.entries.clone(),
            next_slot: self.next_slot,
            provenance: self.provenance,
        };
        crate::game::perf_counters::record_pool_entries_walked(
            self.entries
                .iter()
                .zip(&copy.entries)
                .map(|(entry, copied)| entry.slots.unshared(&copied.slots))
                .sum(),
        );
        copy
    }
}

impl std::fmt::Debug for ManaPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let units: Vec<ManaUnit> = self
            .ordered()
            .into_iter()
            .map(|(_, entry, pip)| self.entries[entry].shape.clone().with_pip(pip))
            .collect();
        f.debug_struct("ManaPool").field("mana", &units).finish()
    }
}

// Pips are excluded, as `ManaUnit` equality excludes them (CR 104.4b).
impl PartialEq for ManaPool {
    fn eq(&self, other: &Self) -> bool {
        let this = self.ordered();
        let that = other.ordered();
        this.len() == that.len()
            && this
                .iter()
                .zip(&that)
                .all(|(&(_, a, _), &(_, b, _))| self.entries[a].shape == other.entries[b].shape)
    }
}

impl Eq for ManaPool {}

impl FromIterator<ManaUnit> for ManaPool {
    fn from_iter<I: IntoIterator<Item = ManaUnit>>(units: I) -> Self {
        Self::from_units(units)
    }
}

impl ManaPool {
    /// A pool holding `units` in order; a non-empty pool's pips are unverified.
    pub fn from_units(units: impl IntoIterator<Item = ManaUnit>) -> Self {
        let mut pool = Self::default();
        for unit in units {
            pool.push_merged(unit);
        }
        if !pool.is_empty() {
            pool.provenance = PoolProvenance::Unverified;
        }
        pool
    }

    /// Pushes a unit with no journal record, so its pip is unverified.
    pub fn add(&mut self, unit: ManaUnit) {
        self.push_merged(unit);
        self.provenance = PoolProvenance::Unverified;
    }

    /// Pushes a unit whose pip the caller has checked unique and nonzero and
    /// whose producer is in the owning state's journal.
    pub(crate) fn insert_journaled(&mut self, unit: ManaUnit) {
        self.push_merged(unit);
    }

    fn push_merged(&mut self, unit: ManaUnit) -> PoolSlot {
        let slot = PoolSlot(self.next_slot);
        self.next_slot += 1;
        let (shape, pip) = unit.into_shape_and_pip();
        self.insert_at(slot, shape, pip);
        slot
    }

    fn insert_at(&mut self, slot: PoolSlot, shape: ManaShape, pip: ManaPipId) {
        let found = self.walk().position(|entry| entry.shape == shape);
        let index = match found {
            Some(index) => index,
            None => {
                self.entries.push(PoolEntry {
                    shape,
                    slots: im::OrdMap::new(),
                    by_pip: im::OrdMap::new(),
                });
                self.entries.len() - 1
            }
        };
        self.entries[index].insert(slot, pip);
    }

    /// The pool's single shape walk.
    fn walk(&self) -> impl Iterator<Item = &PoolEntry> + '_ {
        self.entries.iter().inspect(|_| {
            #[cfg(feature = "test-support")]
            crate::game::perf_counters::record_pool_entries_walked(1);
        })
    }

    /// Every (slot, entry index, pip) in slot order.
    fn ordered(&self) -> Vec<(PoolSlot, usize, ManaPipId)> {
        let mut ordered: Vec<(PoolSlot, usize, ManaPipId)> = self
            .entries
            .iter()
            .enumerate()
            .flat_map(|(index, entry)| {
                entry
                    .slots
                    .iter()
                    .map(move |(slot, pip)| (*slot, index, *pip))
            })
            .collect();
        ordered.sort_unstable_by_key(|(slot, _, _)| *slot);
        #[cfg(feature = "test-support")]
        crate::game::perf_counters::record_pool_entries_walked(ordered.len() as u64);
        ordered
    }

    fn entry_of_slot(&self, slot: PoolSlot) -> Option<usize> {
        self.walk()
            .position(|entry| entry.slots.contains_key(&slot))
    }

    fn nth_slot(&self, index: usize) -> Option<PoolSlot> {
        self.ordered().get(index).map(|(slot, _, _)| *slot)
    }

    /// Deletes `slot`, dropping its entry once empty, and returns its unit.
    fn take_slot(&mut self, slot: PoolSlot) -> Option<ManaUnit> {
        let index = self.entry_of_slot(slot)?;
        let pip = self.entries[index].remove(slot)?;
        let shape = if self.entries[index].slots.is_empty() {
            self.entries.remove(index).shape
        } else {
            self.entries[index].shape.clone()
        };
        Some(shape.with_pip(pip))
    }

    /// Every unit in pool order.
    pub fn units(&self) -> impl Iterator<Item = ManaUnit> + '_ {
        self.slotted_units().map(|(_, unit)| unit)
    }

    /// Every unit in pool order, with its slot.
    pub fn slotted_units(&self) -> impl Iterator<Item = (PoolSlot, ManaUnit)> + '_ {
        self.ordered()
            .into_iter()
            .map(move |(slot, index, pip)| (slot, self.entries[index].shape.clone().with_pip(pip)))
    }

    /// Each distinct shape with its unit count, in entry-creation order, which
    /// is not pool order.
    pub fn shapes(&self) -> impl Iterator<Item = (&ManaShape, usize)> + '_ {
        self.walk().map(|entry| (&entry.shape, entry.slots.len()))
    }

    pub fn count_where(&self, pred: impl Fn(&ManaShape) -> bool) -> usize {
        self.shapes()
            .filter(|(shape, _)| pred(shape))
            .map(|(_, count)| count)
            .sum()
    }

    pub fn count_color(&self, color: ManaType) -> usize {
        self.count_where(|shape| shape.color == color)
    }

    pub fn total(&self) -> usize {
        self.count_where(|_| true)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn produced_mana_total(&self) -> usize {
        self.count_where(|shape| !shape.is_convoke_payment())
    }

    pub fn contains_pip(&self, pip: ManaPipId) -> bool {
        self.walk().any(|entry| entry.by_pip.contains_key(&pip))
    }

    /// The unit holding `pip`, at its minimum slot when the pip is duplicated.
    pub fn unit_by_pip(&self, pip: ManaPipId) -> Option<ManaUnit> {
        self.shape_of_pip(pip)
            .map(|shape| shape.clone().with_pip(pip))
    }

    /// The shape of the unit holding `pip`, at its minimum slot.
    pub fn shape_of_pip(&self, pip: ManaPipId) -> Option<&ManaShape> {
        self.walk()
            .filter_map(|entry| entry.first_slot_of(pip).map(|slot| (slot, &entry.shape)))
            .min_by_key(|(slot, _)| *slot)
            .map(|(_, shape)| shape)
    }

    /// The minimum slot whose unit's shape satisfies `pred`.
    pub fn first_where(&self, pred: impl Fn(&ManaShape) -> bool) -> Option<(PoolSlot, &ManaShape)> {
        self.walk()
            .filter(|entry| pred(&entry.shape))
            .map(|entry| (entry.first_slot(), &entry.shape))
            .min_by_key(|(slot, _)| *slot)
    }

    /// The minimum slot holding one of `pins` whose unit's shape satisfies `pred`.
    pub fn first_pinned_where(
        &self,
        pins: &[ManaPipId],
        pred: impl Fn(&ManaShape) -> bool,
    ) -> Option<(PoolSlot, &ManaShape)> {
        if pins.is_empty() {
            return None;
        }
        self.walk()
            .filter(|entry| pred(&entry.shape))
            .filter_map(|entry| {
                pins.iter()
                    .filter_map(|pin| entry.first_slot_of(*pin))
                    .min()
                    .map(|slot| (slot, &entry.shape))
            })
            .min_by_key(|(slot, _)| *slot)
    }

    /// Removes the unit at `slot` as `Vec::swap_remove` would: the unit at the
    /// last slot takes its place.
    pub fn swap_remove(&mut self, slot: PoolSlot) -> ManaUnit {
        let last = self
            .walk()
            .map(|entry| {
                entry
                    .slots
                    .get_max()
                    .expect("a pool entry is never empty")
                    .0
            })
            .max()
            .expect("swap_remove on an empty pool");
        let unit = self
            .take_slot(slot)
            .expect("swap_remove of a slot the pool holds");
        if last != slot {
            let index = self
                .entry_of_slot(last)
                .expect("the last slot is held by an entry");
            let pip = self.entries[index]
                .remove(last)
                .expect("the last slot is held by its entry");
            self.entries[index].insert(slot, pip);
        }
        unit
    }

    pub fn unit_at(&self, index: usize) -> Option<ManaUnit> {
        self.ordered()
            .get(index)
            .map(|&(_, entry, pip)| self.entries[entry].shape.clone().with_pip(pip))
    }

    /// Removes the unit at `index` in pool order, as `Vec::remove` would.
    pub fn remove_at(&mut self, index: usize) -> Option<ManaUnit> {
        let slot = self.nth_slot(index)?;
        self.take_slot(slot)
    }

    /// Recolors the unit at `index` in pool order, keeping its slot, and
    /// returns its previous color.
    pub fn recolor_at(&mut self, index: usize, color: ManaType) -> Option<ManaType> {
        let slot = self.nth_slot(index)?;
        let (mut shape, pip) = self.take_slot(slot)?.into_shape_and_pip();
        let from = std::mem::replace(&mut shape.color, color);
        self.insert_at(slot, shape, pip);
        Some(from)
    }

    /// Rewrites the pip at `slot`; a rewritten pip has no proven producer.
    pub fn set_pip_at(&mut self, slot: PoolSlot, pip: ManaPipId) {
        let index = self
            .entry_of_slot(slot)
            .expect("set_pip_at of a slot the pool holds");
        self.entries[index].remove(slot);
        self.entries[index].insert(slot, pip);
        self.provenance = PoolProvenance::Unverified;
    }

    /// Replaces each entry's shape by `edit`, merging entries that become equal
    /// and keeping every slot and pip.
    fn edit_shapes(&mut self, edit: impl Fn(&ManaShape) -> Option<ManaShape>) {
        if !self.walk().any(|entry| edit(&entry.shape).is_some()) {
            return;
        }
        let mut merged: Vec<PoolEntry> = Vec::with_capacity(self.entries.len());
        for mut entry in std::mem::take(&mut self.entries) {
            if let Some(shape) = edit(&entry.shape) {
                entry.shape = shape;
            }
            match merged.iter_mut().find(|kept| kept.shape == entry.shape) {
                Some(kept) => {
                    for (slot, pip) in entry.slots {
                        kept.insert(slot, pip);
                    }
                }
                None => merged.push(entry),
            }
        }
        self.entries = merged;
    }

    /// Keeps only entries whose shape satisfies `keep`.
    pub fn retain_shapes(&mut self, keep: impl Fn(&ManaShape) -> bool) {
        self.entries.retain(|entry| keep(&entry.shape));
    }

    /// Removes each of `units` by pip and shape, at the pip's minimum slot.
    pub(crate) fn remove_exact(&mut self, units: &[ManaUnit]) -> Result<(), ExactManaRemovalError> {
        for unit in units {
            let (shape, pip) = unit.clone().into_shape_and_pip();
            let slot = self
                .walk()
                .filter(|entry| entry.shape == shape)
                .find_map(|entry| entry.first_slot_of(pip));
            match slot {
                Some(slot) => {
                    self.swap_remove(slot);
                }
                None if self.contains_pip(pip) => {
                    return Err(ExactManaRemovalError::MismatchedUnit(pip));
                }
                None => return Err(ExactManaRemovalError::MissingPip(pip)),
            }
        }
        Ok(())
    }

    pub fn is_verified(&self) -> bool {
        self.provenance == PoolProvenance::Verified
    }

    pub(crate) fn mark_unverified(&mut self) {
        self.provenance = PoolProvenance::Unverified;
    }

    pub(crate) fn mark_verified(&mut self) {
        self.provenance = PoolProvenance::Verified;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// CR 500.5 + CR 703.4q: End-of-combat retention expires before constructing
    /// the ordinary "empty unspent mana" event. CR 500.5 orders these operations:
    /// effects lasting until this boundary expire first, then unspent mana empties.
    /// Clearing the reached marker makes the still-unspent unit eligible for the
    /// normal empty-pool pipeline, where another active retention or transformation
    /// effect may still apply.
    ///
    /// - `EndOfCombat`: marker clears when leaving combat (CR 500.5a).
    /// - `EndOfPhaseGroup`: marker clears when the turn crosses from one of
    ///   CR 500.1's five phases into another.
    /// - `EndOfTurn`: marker remains until the cleanup action (CR 514.2).
    /// - `None`: already eligible for the ordinary empty-pool event.
    ///
    /// `from` is the PRE-transition phase and is `None` only for a `GameState`
    /// saved before it was recorded. A phase-group crossing cannot be computed
    /// without it, so such a resume leaves `EndOfPhaseGroup` units retained —
    /// the conservative direction, since the alternative would burn a player
    /// for mana at a boundary the engine cannot confirm they crossed.
    ///
    /// `EndOfCombat` deliberately still keys off the DESTINATION alone rather
    /// than the crossing, preserving its existing behavior exactly: a stray
    /// combat-retained unit drains at the next non-combat step whether or not
    /// that step is a phase-group boundary.
    pub fn clear_expired_retention_markers(&mut self, from: Option<Phase>, to: Phase) {
        let leaving_phase_group = from.is_some_and(|from| from.group() != to.group());
        self.edit_shapes(|shape| {
            let expired = match shape.expiry {
                // CR 500.5a
                Some(ManaExpiry::EndOfCombat) => !to.is_combat(),
                // CR 500.1
                Some(ManaExpiry::EndOfPhaseGroup) => leaving_phase_group,
                // CR 514.2: cleared by the cleanup action, not here.
                Some(ManaExpiry::EndOfTurn) => false,
                None => false,
            };
            expired.then(|| ManaShape {
                expiry: None,
                ..shape.clone()
            })
        });
    }

    /// Mana burn: hold unspent mana across the steps INSIDE one of CR 500.1's
    /// five phases, so it empties only at the phase boundary — where the
    /// emptied count is the life the player loses.
    ///
    /// Marks ordinary units with [`ManaExpiry::EndOfPhaseGroup`], reusing the
    /// same retention mechanism Firebending's `EndOfCombat` already rides on,
    /// rather than suppressing the drain: retention is what keeps a unit out of
    /// the `Drop` set, and a second way to do that could disagree with the
    /// first.
    ///
    /// **Marks here, not at each `ManaUnit` construction.** A unit cannot know
    /// the format it was produced under — `ManaUnit::new` takes no state, and
    /// there are well over a hundred construction sites, most of them tests.
    /// The phase transition is the one place that has both the pool and the
    /// resolved rules, and it is also the only place the mark is ever read.
    ///
    /// At a real crossing this deliberately does nothing, leaving the units
    /// ordinary so [`Self::clear_expired_retention_markers`] and the
    /// empty-pool pipeline treat them exactly as they treat any other unspent
    /// mana. That is what makes the burn amount fall out of the existing drop
    /// count instead of needing a second, separately-computed tally.
    pub fn retain_across_phase_group_steps(&mut self, from: Option<Phase>, to: Phase) {
        if from.is_some_and(|from| from.group() != to.group()) {
            return;
        }
        self.edit_shapes(|shape| {
            shape.expiry.is_none().then(|| ManaShape {
                expiry: Some(ManaExpiry::EndOfPhaseGroup),
                ..shape.clone()
            })
        });
    }

    /// CR 514.2: “Until end of turn” effects end during the cleanup action.
    /// Clearing only the marker leaves the unit for the next ordinary
    /// CR 500.5 / CR 703.4q empty-pool boundary.
    pub fn clear_expired_end_of_turn_retention_markers(&mut self) {
        self.edit_shapes(|shape| {
            matches!(shape.expiry, Some(ManaExpiry::EndOfTurn)).then(|| ManaShape {
                expiry: None,
                ..shape.clone()
            })
        });
    }

    /// Remove all mana units produced by the given source.
    /// Returns the number of units removed (zero if mana was already spent).
    pub fn remove_from_source(&mut self, source_id: ObjectId) -> usize {
        let before = self.total();
        self.retain_shapes(|shape| shape.source_id != source_id);
        before - self.total()
    }

    /// CR 702.139a: Remove `count` unrestricted mana of any type from the pool (generic cost).
    /// Skips mana with `ManaRestriction`s since the companion special action is not a spell.
    /// Returns true if enough eligible mana was available and removed, false otherwise.
    pub fn spend_generic(&mut self, count: usize) -> bool {
        if self.count_where(|shape| shape.restrictions.is_empty()) < count {
            return false;
        }
        let mut unrestricted: Vec<PoolSlot> = self
            .walk()
            .filter(|entry| entry.shape.restrictions.is_empty())
            .flat_map(|entry| entry.slots.keys().copied())
            .collect();
        unrestricted.sort_unstable();
        for slot in unrestricted.into_iter().take(count) {
            self.take_slot(slot);
        }
        true
    }

    pub fn spend(&mut self, color: ManaType) -> Option<ManaUnit> {
        let (slot, _) = self.first_where(|shape| shape.color == color)?;
        Some(self.swap_remove(slot))
    }

    /// Spend one mana of the given color that is eligible for the given payment context.
    ///
    /// CR 106.6: Prefers unrestricted mana first, then falls back to restricted mana
    /// whose restrictions all allow the payment (spell cast or ability activation,
    /// per the `PaymentContext` variant). Mana with restrictions that don't match is
    /// never spent.
    pub fn spend_for(&mut self, color: ManaType, ctx: &PaymentContext<'_>) -> Option<ManaUnit> {
        // First pass: prefer unrestricted mana of this color
        let slot = self
            .first_where(|m| {
                m.color == color
                    && ctx.permits_actual_mana_type(m.color)
                    && m.restrictions.is_empty()
            })
            // Second pass: restricted mana that allows this payment context
            .or_else(|| {
                self.first_where(|m| {
                    m.color == color
                        && ctx.permits_actual_mana_type(m.color)
                        && !m.restrictions.is_empty()
                        && m.restrictions.iter().all(|r| r.allows(ctx))
                })
            })
            .map(|(slot, _)| slot)?;
        Some(self.swap_remove(slot))
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
) -> u32 {
    let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
        return 0;
    };
    // Descending pool_index order preserves index validity across removes.
    let mut sorted: Vec<&UnitDecision> = units.iter().collect();
    sorted.sort_by_key(|d| std::cmp::Reverse(d.pool_index));
    let mut changed = false;
    let mut removed_count = 0_u32;
    let mut seen_indices = std::collections::HashSet::new();
    for decision in sorted {
        // The action wire is hostile input. A duplicate index or a stale
        // color/index pair must not rebound to a different mana unit after a
        // prior descending removal.
        if !seen_indices.insert(decision.pool_index)
            || player
                .mana_pool
                .unit_at(decision.pool_index)
                .is_none_or(|unit| unit.color != decision.color)
        {
            continue;
        }
        match decision.disposition {
            UnitDisposition::Drop => {
                let Some(removed) = player.mana_pool.remove_at(decision.pool_index) else {
                    continue;
                };
                removed_count += 1;
                changed = true;
                events.push(GameEvent::ManaPoolEmptied {
                    player_id,
                    source_id: removed.source_id,
                    color: removed.color,
                });
            }
            UnitDisposition::Keep => {}
            UnitDisposition::Recolor(to) => {
                if let Some(from) = player.mana_pool.recolor_at(decision.pool_index, to) {
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
    removed_count
}

#[cfg(test)]
mod tests {
    use super::test_helpers::{shape_of, ReferencePool, MANA_TYPES};
    use super::*;
    use crate::types::ability::Effect;
    use crate::types::zones::Zone;

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
    fn empty_mana_pool_decisions_count_only_valid_unique_drops() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        state.players[0].mana_pool.add(make_unit(ManaType::Black));
        state.players[0].mana_pool.add(make_unit(ManaType::Red));
        let decisions = vec![
            UnitDecision {
                pool_index: 1,
                color: ManaType::Red,
                disposition: UnitDisposition::Drop,
            },
            // Hostile duplicate: must not rebound after the first removal.
            UnitDecision {
                pool_index: 1,
                color: ManaType::Red,
                disposition: UnitDisposition::Drop,
            },
            // Hostile stale color/index pair: must not remove the black unit.
            UnitDecision {
                pool_index: 0,
                color: ManaType::Blue,
                disposition: UnitDisposition::Drop,
            },
        ];
        let mut events = Vec::new();

        let removed =
            apply_empty_mana_pool_decisions(&mut state, PlayerId(0), &decisions, &mut events);

        assert_eq!(removed, 1);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert_eq!(
            state.players[0].mana_pool.unit_at(0).unwrap().color,
            ManaType::Black
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::ManaPoolEmptied { .. }))
                .count(),
            1
        );
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

    // CR 500.5 + CR 703.4q: retention durations end before the ordinary
    // empty-pool event. The helper clears only the reached marker; pool
    // removal and any still-active retention / transformation effects are
    // exercised end-to-end through `game::turns`.
    #[test]
    fn mana_pool_clears_end_of_turn_marker_only_at_cleanup() {
        let mut pool = ManaPool::default();
        let mut retained = make_unit(ManaType::Green);
        retained.expiry = Some(ManaExpiry::EndOfTurn);
        pool.add(retained);
        pool.add(make_unit(ManaType::Red));

        // Non-cleanup transition: EndOfTurn unit survives; non-expiry unit
        // is left in place (the pipeline drives Drop disposition elsewhere).
        pool.clear_expired_retention_markers(Some(Phase::Upkeep), Phase::Draw);
        assert_eq!(pool.count_color(ManaType::Green), 1);
        assert_eq!(pool.count_color(ManaType::Red), 1);
        assert_eq!(pool.unit_at(0).unwrap().expiry, Some(ManaExpiry::EndOfTurn));

        // Cleanup transition: the duration ends, but the still-unspent unit
        // remains for the ordinary empty-pool pipeline.
        pool.clear_expired_end_of_turn_retention_markers();
        assert_eq!(pool.count_color(ManaType::Green), 1);
        assert_eq!(pool.count_color(ManaType::Red), 1);
        assert_eq!(pool.unit_at(0).unwrap().expiry, None);
    }

    #[test]
    fn mana_pool_clears_end_of_combat_marker_when_leaving_combat() {
        let mut pool = ManaPool::default();
        let mut combat_mana = make_unit(ManaType::Red);
        combat_mana.expiry = Some(ManaExpiry::EndOfCombat);
        pool.add(combat_mana);

        // In-combat transition (e.g., DeclareAttackers → DeclareBlockers):
        // EndOfCombat unit survives.
        pool.clear_expired_retention_markers(Some(Phase::DeclareAttackers), Phase::DeclareBlockers);
        assert_eq!(pool.count_color(ManaType::Red), 1);
        assert_eq!(
            pool.unit_at(0).unwrap().expiry,
            Some(ManaExpiry::EndOfCombat)
        );

        // Leaving combat ends the retention duration; ordinary empty-pool
        // processing decides the unit's final disposition.
        pool.clear_expired_retention_markers(Some(Phase::EndCombat), Phase::PostCombatMain);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.unit_at(0).unwrap().expiry, None);
    }

    /// CR 500.1: the pre-M10 boundary is the PHASE, not the step. A unit must
    /// survive every intra-phase step and clear only on a real crossing.
    #[test]
    fn mana_pool_clears_end_of_phase_group_marker_only_when_crossing_phases() {
        let mut pool = ManaPool::default();
        let mut burn_mana = make_unit(ManaType::Red);
        burn_mana.expiry = Some(ManaExpiry::EndOfPhaseGroup);
        pool.add(burn_mana);

        // Intra-group steps, across three different groups, all retain. The
        // combat pair is the one the old `EndOfCombat` rule already handled;
        // the beginning-phase pair is the one it never could.
        for (from, to) in [
            (Phase::Untap, Phase::Upkeep),
            (Phase::Upkeep, Phase::Draw),
            (Phase::DeclareAttackers, Phase::DeclareBlockers),
            (Phase::End, Phase::Cleanup),
        ] {
            pool.clear_expired_retention_markers(Some(from), to);
            assert_eq!(
                pool.unit_at(0).unwrap().expiry,
                Some(ManaExpiry::EndOfPhaseGroup),
                "{from:?} -> {to:?} stays inside one phase and must retain"
            );
        }

        // An unknown pre-transition phase (a save predating the field) also
        // retains — the crossing cannot be confirmed, so it is not assumed.
        pool.clear_expired_retention_markers(None, Phase::PreCombatMain);
        assert_eq!(
            pool.unit_at(0).unwrap().expiry,
            Some(ManaExpiry::EndOfPhaseGroup)
        );

        // A real crossing clears the marker, handing the unit to the ordinary
        // empty-pool pipeline.
        pool.clear_expired_retention_markers(Some(Phase::Draw), Phase::PreCombatMain);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.unit_at(0).unwrap().expiry, None);
    }

    #[test]
    fn mana_pool_serializes_and_roundtrips() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        let json = serde_json::to_string(&pool).unwrap();
        let deserialized: ManaPool = serde_json::from_str(&json).unwrap();
        assert_eq!(pool, deserialized);
    }

    // NOTE: `shares_creature_type_with_commander_defers_to_call_site` was DELETED with
    // the `ManaRestriction::SharesCreatureTypeWithCommander` variant it documented. That
    // variant was never a CR 106.6 spend restriction — Path of Ancestry's mana may be
    // spent on anything — so it carried a permanently-`false` `allows_spell`, and the
    // test existed only to pin that hole. The predicate now lives where it belongs, as
    // `FilterProp::SharesCreatureTypeWithCommander`, and its behavior is pinned by the
    // RUNTIME test `mana_spend_trigger_shares_creature_type_with_commander`
    // (game/casting_tests.rs), which is the only layer that can prove it.

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
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
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
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
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
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&goblin_spell))
            .is_none());
        assert_eq!(pool.total(), 1, "Restricted mana should remain in pool");
    }

    #[test]
    fn spend_for_enforces_activation_color_constraint_for_both_passes() {
        let source_types = vec!["Artifact".to_string()];
        let source_subtypes = Vec::new();
        let context = PaymentContext::Activation {
            source_types: &source_types,
            source_subtypes: &source_subtypes,
            ability_tag: None,
            mana_color_constraint: ActivationManaColorConstraint::Only(ManaColor::Red),
        };
        let mut pool = ManaPool::from_units(vec![
            // First pass would take this unrestricted blue unit without
            // checking the activation's actual-color rider.
            make_unit(ManaType::Blue),
            // Second pass must reject it too, even though its restriction
            // otherwise permits any activation.
            make_restricted_unit(
                ManaType::Blue,
                ObjectId(2),
                vec![ManaRestriction::OnlyForActivation],
            ),
            make_unit(ManaType::Red),
        ]);

        assert!(pool.spend_for(ManaType::Blue, &context).is_none());
        assert_eq!(pool.total(), 3);
        assert_eq!(
            pool.spend_for(ManaType::Red, &context)
                .expect("matching actual mana color should remain spendable")
                .color,
            ManaType::Red
        );
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
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&mv_four))
            .is_none());
        assert_eq!(pool.total(), 1);

        let mv_five = SpellMeta {
            mana_value: Some(5),
            color_count: None,
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&mv_five))
            .is_some());
        assert_eq!(pool.total(), 0);
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
            colors: vec![],
            has_x_in_cost: false,
            is_face_down: false,
            cant_spend_mana: false,
            spend_only_on_x_colors: None,
            spend_only_on_x_generic_count: 0,
            ..SpellMeta::default()
        };
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&one_color))
            .is_none());
        assert_eq!(pool.total(), 1);

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
        assert!(pool
            .spend_for(ManaType::Green, &PaymentContext::Spell(&two_colors))
            .is_some());
        assert_eq!(pool.total(), 0);
    }

    type ShapePredicate = Box<dyn Fn(&ManaShape) -> bool>;

    fn predicates() -> Vec<ShapePredicate> {
        let mut preds: Vec<ShapePredicate> = vec![
            Box::new(|_| true),
            Box::new(ManaShape::is_snow),
            Box::new(ManaShape::is_convoke_payment),
            Box::new(|s| s.source_could_produce_two_or_more_colors),
            Box::new(|s| !s.restrictions.is_empty()),
            Box::new(|s| s.expiry.is_some()),
        ];
        for color in MANA_TYPES {
            preds.push(Box::new(move |s| s.color == color));
        }
        preds
    }

    /// Asserts the shape pool answers every read exactly as the reference unit pool does.
    fn assert_answers_as(pool: &ManaPool, reference: &ReferencePool, step: &str) {
        let units: Vec<ManaUnit> = pool.units().collect();
        assert_eq!(units, reference.0, "{step}: units in pool order");
        assert_eq!(
            units.iter().map(|u| u.pip_id).collect::<Vec<_>>(),
            reference.0.iter().map(|u| u.pip_id).collect::<Vec<_>>(),
            "{step}: pips in pool order"
        );
        assert_eq!(pool.total(), reference.0.len(), "{step}: total");
        assert_eq!(pool.is_empty(), reference.0.is_empty(), "{step}: is_empty");
        assert_eq!(
            pool.produced_mana_total(),
            reference
                .0
                .iter()
                .filter(|u| !u.is_convoke_payment())
                .count(),
            "{step}: produced_mana_total"
        );
        for color in MANA_TYPES {
            assert_eq!(
                pool.count_color(color),
                reference.0.iter().filter(|u| u.color == color).count(),
                "{step}: count_color({color:?})"
            );
        }
        let slots: Vec<PoolSlot> = pool.slotted_units().map(|(slot, _)| slot).collect();
        let position_of = |slot: PoolSlot| slots.iter().position(|s| *s == slot);
        let pips: Vec<ManaPipId> = reference.0.iter().map(|u| u.pip_id).collect();
        let pin_sets = [
            pips.iter().rev().take(2).copied().collect::<Vec<_>>(),
            pips.iter().step_by(2).copied().collect(),
            vec![ManaPipId(9_999)],
        ];
        for (index, pred) in predicates().iter().enumerate() {
            assert_eq!(
                pool.count_where(pred),
                reference.0.iter().filter(|u| pred(&shape_of(u))).count(),
                "{step}: count_where #{index}"
            );
            assert_eq!(
                pool.first_where(pred)
                    .and_then(|(slot, _)| position_of(slot)),
                reference.0.iter().position(|u| pred(&shape_of(u))),
                "{step}: first_where #{index}"
            );
            for pins in &pin_sets {
                assert_eq!(
                    pool.first_pinned_where(pins, pred)
                        .and_then(|(slot, _)| position_of(slot)),
                    reference
                        .0
                        .iter()
                        .position(|u| pins.contains(&u.pip_id) && pred(&shape_of(u))),
                    "{step}: first_pinned_where #{index} {pins:?}"
                );
            }
        }
        for pip in pips.iter().copied().chain([ManaPipId(9_999)]) {
            let expected = reference.0.iter().find(|u| u.pip_id == pip);
            assert_eq!(
                pool.contains_pip(pip),
                expected.is_some(),
                "{step}: contains_pip"
            );
            assert_eq!(
                pool.unit_by_pip(pip).as_ref(),
                expected,
                "{step}: unit_by_pip"
            );
            assert_eq!(
                pool.unit_by_pip(pip).map(|u| u.pip_id),
                expected.map(|u| u.pip_id),
                "{step}: unit_by_pip pip"
            );
            assert_eq!(
                pool.shape_of_pip(pip).cloned(),
                expected.map(shape_of),
                "{step}: shape_of_pip"
            );
        }
        for index in 0..=reference.0.len() {
            assert_eq!(
                pool.unit_at(index).as_ref(),
                reference.0.get(index),
                "{step}: unit_at"
            );
        }
        assert_eq!(
            serde_json::to_value(pool).unwrap(),
            serde_json::json!({ "mana": reference.0 }),
            "{step}: serialized pool"
        );
        assert_eq!(
            *pool,
            ManaPool::from_units(reference.0.clone()),
            "{step}: equality"
        );
        let mut distinct: Vec<(ManaUnit, usize)> = Vec::new();
        for unit in &reference.0 {
            match distinct.iter_mut().find(|(seen, _)| seen == unit) {
                Some((_, count)) => *count += 1,
                None => distinct.push((unit.clone(), 1)),
            }
        }
        let shapes: Vec<(ManaShape, usize)> = pool
            .shapes()
            .map(|(shape, count)| (shape.clone(), count))
            .collect();
        assert_eq!(
            shapes.len(),
            distinct.len(),
            "{step}: one entry per distinct shape"
        );
        for (unit, count) in &distinct {
            assert!(
                shapes.contains(&(shape_of(unit), *count)),
                "{step}: shape {unit:?} held {count} times"
            );
        }
    }

    /// Each card's mana ability resolved onto the battlefield, one unit per card.
    fn real_card_units() -> Vec<(&'static str, ManaUnit)> {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::scenario_db::GameScenarioDbExt;
        use crate::types::ability::ResolvedAbility;

        let db = crate::test_support::shared_card_db();
        let names = [
            "Island",
            "Snow-Covered Island",
            "Basalt Monolith",
            "Grand Architect",
            "Relic of Legends",
        ];
        let mut scenario = GameScenario::new();
        let ids: Vec<ObjectId> = names
            .iter()
            .map(|name| scenario.add_real_card(P0, name, Zone::Battlefield, db))
            .collect();
        let mut runner = scenario.build();
        names
            .iter()
            .zip(ids)
            .map(|(name, id)| {
                let state = runner.state_mut();
                let effect = state.objects[&id]
                    .abilities
                    .iter()
                    .find(|ability| matches!(*ability.effect, Effect::Mana { .. }))
                    .map(|ability| (*ability.effect).clone())
                    .unwrap_or_else(|| panic!("{name} has a mana ability"));
                let before = state.players[0].mana_pool.total();
                crate::game::effects::mana::resolve(
                    state,
                    &ResolvedAbility::new(effect, vec![], id, P0),
                    &mut Vec::new(),
                )
                .unwrap_or_else(|error| panic!("{name}'s mana ability resolves: {error:?}"));
                if matches!(
                    runner.state().waiting_for,
                    crate::types::game_state::WaitingFor::ChooseManaColor { .. }
                ) {
                    runner
                        .act(crate::types::actions::GameAction::ChooseManaColor {
                            choice: crate::types::game_state::ManaChoice::SingleColor(
                                ManaType::Red,
                            ),
                            count: 1,
                        })
                        .unwrap_or_else(|error| panic!("{name}'s color choice: {error:?}"));
                }
                let unit = runner.state().players[0]
                    .mana_pool
                    .unit_at(before)
                    .unwrap_or_else(|| panic!("{name} produced mana"));
                (*name, unit)
            })
            .collect()
    }

    #[test]
    fn shape_pool_answers_as_a_unit_pool_on_a_real_board() {
        let cards = real_card_units();
        let unit = |name: &str, pip: u64| ManaUnit {
            pip_id: ManaPipId(pip),
            ..cards
                .iter()
                .find(|(card, _)| *card == name)
                .map(|(_, unit)| unit.clone())
                .unwrap()
        };
        let island = |pip| unit("Island", pip);
        let snow = |pip| unit("Snow-Covered Island", pip);
        let basalt = |pip| unit("Basalt Monolith", pip);
        let architect = |pip| unit("Grand Architect", pip);
        let relic = |pip| unit("Relic of Legends", pip);
        assert!(
            shape_of(&snow(0)).is_snow(),
            "reach: Snow-Covered Island's mana is snow"
        );
        assert!(
            !architect(0).restrictions.is_empty(),
            "reach: Grand Architect's mana is restricted"
        );
        assert!(
            relic(0).source_could_produce_two_or_more_colors,
            "reach: Relic of Legends could produce two or more colors"
        );
        assert_eq!(
            [island(0), snow(0), basalt(0), architect(0), relic(0)]
                .iter()
                .map(|u| u.color)
                .collect::<Vec<_>>()[..3],
            [ManaType::Blue, ManaType::Blue, ManaType::Colorless],
            "reach: the basic and Basalt colors"
        );

        let mut pool = ManaPool::from_units([]);
        let mut reference = ReferencePool::default();
        assert!(pool.is_verified(), "an empty pool is verified");
        assert_answers_as(&pool, &reference, "empty");

        for unit in [
            island(1),
            basalt(2),
            island(3),
            architect(4),
            snow(5),
            relic(6),
            basalt(7),
            island(8),
            // A duplicate pip in another shape.
            basalt(3),
            architect(9),
            basalt(10),
        ] {
            pool.add(unit.clone());
            reference.0.push(unit);
            assert_answers_as(&pool, &reference, "add");
        }
        assert!(!pool.is_verified(), "a raw push leaves the pool unverified");
        pool.insert_journaled(relic(15));
        reference.0.push(relic(15));
        assert_answers_as(&pool, &reference, "insert_journaled");

        let (slot, _) = pool
            .first_where(|s| s.color == ManaType::Blue && !s.is_snow())
            .unwrap();
        assert_eq!(
            pool.swap_remove(slot),
            reference
                .swap_remove_where(|s| s.color == ManaType::Blue && !s.is_snow())
                .unwrap()
        );
        assert_answers_as(&pool, &reference, "swap_remove");

        assert_eq!(
            pool.spend(ManaType::Colorless),
            reference.swap_remove_where(|s| s.color == ManaType::Colorless)
        );
        assert_answers_as(&pool, &reference, "spend");
        assert_eq!(pool.spend(ManaType::Green), None);
        assert_answers_as(&pool, &reference, "spend declines");

        let meta = SpellMeta::default();
        let ctx = PaymentContext::Spell(&meta);
        for color in [ManaType::Colorless, ManaType::Colorless, ManaType::Green] {
            assert_eq!(
                pool.spend_for(color, &ctx),
                reference.spend_for(color, &ctx)
            );
            assert_answers_as(&pool, &reference, "spend_for");
        }

        let pins = [ManaPipId(10), ManaPipId(5)];
        let (slot, _) = pool.first_pinned_where(&pins, |_| true).unwrap();
        assert_eq!(
            pool.swap_remove(slot).pip_id,
            reference
                .0
                .swap_remove(
                    reference
                        .0
                        .iter()
                        .position(|u| pins.contains(&u.pip_id))
                        .unwrap()
                )
                .pip_id
        );
        assert_answers_as(&pool, &reference, "pinned swap_remove");

        // The same shape added before and after a recolor that leaves and rejoins it.
        pool.add(island(11));
        reference.0.push(island(11));
        let index = reference
            .0
            .iter()
            .position(|u| u.pip_id == ManaPipId(11))
            .unwrap();
        for color in [ManaType::Black, ManaType::Blue] {
            let from = reference.0[index].color;
            reference.0[index].color = color;
            assert_eq!(pool.recolor_at(index, color), Some(from));
            assert_answers_as(&pool, &reference, "recolor_at");
        }
        pool.add(island(12));
        reference.0.push(island(12));
        assert_answers_as(&pool, &reference, "add after recolor");
        assert_eq!(pool.recolor_at(reference.0.len(), ManaType::Red), None);

        // Retention markers that make one entry equal to another.
        let retained = ManaUnit {
            expiry: Some(ManaExpiry::EndOfPhaseGroup),
            ..island(13)
        };
        let end_of_turn = ManaUnit {
            expiry: Some(ManaExpiry::EndOfTurn),
            ..basalt(14)
        };
        for unit in [retained, end_of_turn] {
            pool.add(unit.clone());
            reference.0.push(unit);
        }
        assert_answers_as(&pool, &reference, "marked units");
        pool.retain_across_phase_group_steps(Some(Phase::Upkeep), Phase::Draw);
        reference.edit_expiry(|e| e.is_none().then_some(Some(ManaExpiry::EndOfPhaseGroup)));
        assert_answers_as(&pool, &reference, "retain_across_phase_group_steps");
        pool.clear_expired_end_of_turn_retention_markers();
        reference.edit_expiry(|e| matches!(e, Some(ManaExpiry::EndOfTurn)).then_some(None));
        assert_answers_as(
            &pool,
            &reference,
            "clear_expired_end_of_turn_retention_markers",
        );
        pool.clear_expired_retention_markers(Some(Phase::Draw), Phase::PreCombatMain);
        reference.edit_expiry(|e| matches!(e, Some(ManaExpiry::EndOfPhaseGroup)).then_some(None));
        assert_answers_as(&pool, &reference, "clear_expired_retention_markers");

        let exact = [reference.0[1].clone(), reference.0[4].clone()];
        assert_eq!(pool.remove_exact(&exact), reference.remove_exact(&exact));
        assert_answers_as(&pool, &reference, "remove_exact");
        let mismatched = [ManaUnit {
            color: ManaType::Green,
            ..reference.0[0].clone()
        }];
        let missing = [island(9_998)];
        for units in [
            &mismatched[..],
            &missing[..],
            &[reference.0[0].clone(), island(9_998)][..],
        ] {
            let expected = reference.remove_exact(units);
            assert!(expected.is_err(), "reach: the removal declines");
            assert_eq!(pool.remove_exact(units), expected);
            assert_answers_as(&pool, &reference, "remove_exact declines");
        }

        assert!(!pool.spend_generic(reference.0.len() + 1));
        assert!(!reference.spend_generic(reference.0.len() + 1));
        assert_answers_as(&pool, &reference, "spend_generic declines");
        assert_eq!(pool.spend_generic(2), reference.spend_generic(2));
        assert_answers_as(&pool, &reference, "spend_generic");

        assert_eq!(pool.remove_at(1), Some(reference.0.remove(1)));
        assert_answers_as(&pool, &reference, "remove_at");
        assert_eq!(pool.remove_at(reference.0.len()), None);

        pool.retain_shapes(|s| !s.is_snow());
        reference.0.retain(|u| !shape_of(u).is_snow());
        assert_answers_as(&pool, &reference, "retain_shapes");
        let source = basalt(0).source_id;
        let before = reference.0.len();
        reference.0.retain(|u| u.source_id != source);
        assert_eq!(pool.remove_from_source(source), before - reference.0.len());
        assert_answers_as(&pool, &reference, "remove_from_source");

        let collected: ManaPool = reference.0.iter().cloned().collect();
        assert_answers_as(&collected, &reference, "from_iter");
        let decoded: ManaPool =
            serde_json::from_value(serde_json::json!({ "mana": reference.0 })).unwrap();
        assert_answers_as(&decoded, &reference, "deserialize");
        assert!(
            !decoded.is_verified(),
            "a loaded non-empty pool is unverified"
        );

        pool.clear();
        reference.0.clear();
        assert_answers_as(&pool, &reference, "clear");

        let units = [ManaPipId(1), ManaPipId(1), ManaPipId(2)].map(|pip_id| ManaUnit {
            pip_id,
            ..ManaUnit::new(ManaType::Green, ObjectId(7), false, vec![])
        });
        let mut pool = ManaPool::from_units(units.clone());
        assert!(!pool.is_verified(), "a pool built from units is unverified");
        pool.mark_verified();
        assert!(pool.is_verified(), "reach: the pool is verified");
        pool.insert_journaled(ManaUnit {
            pip_id: ManaPipId(5),
            ..units[0].clone()
        });
        assert!(
            pool.is_verified(),
            "a journaled insert keeps the pool verified"
        );

        let (slot, _) = pool.slotted_units().nth(1).unwrap();
        pool.set_pip_at(slot, ManaPipId(3));
        assert!(
            !pool.is_verified(),
            "a rewritten pip has no proven producer"
        );
        assert_eq!(
            pool.units().map(|u| u.pip_id).collect::<Vec<_>>(),
            [ManaPipId(1), ManaPipId(3), ManaPipId(2), ManaPipId(5)]
        );
        assert_eq!(
            pool.unit_by_pip(ManaPipId(3)).map(|u| u.pip_id),
            Some(ManaPipId(3))
        );
        assert!(pool.contains_pip(ManaPipId(1)));
        let (first, _) = pool.slotted_units().next().unwrap();
        pool.set_pip_at(first, ManaPipId(4));
        assert!(
            !pool.contains_pip(ManaPipId(1)),
            "the old pip leaves the lookup"
        );
        assert_eq!(pool.unit_by_pip(ManaPipId(1)), None);
        assert_eq!(
            pool.unit_by_pip(ManaPipId(4)).map(|u| u.pip_id),
            Some(ManaPipId(4))
        );
    }
}
