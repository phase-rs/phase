//! CR 601.2f caster-elected cost-reduction ordering.
//!
//! CR 601.2f: "The total cost is the mana cost or alternative cost (as
//! determined in rule 601.2b), plus all additional costs and cost increases,
//! and minus all cost reductions. **If multiple cost reductions apply, the
//! player may apply them in any order.** ... Then the resulting total cost
//! becomes 'locked in.'"
//!
//! Cost reductions stopped commuting once `CostReductionReach` entered the
//! model (CR 118.7b/c/d vs. the printed "This effect reduces only the amount of
//! colored mana you pay" rider): on `{1}{W}`, a `{W}` `ColoredManaOnly`
//! reduction followed by a `{W}` `SpillsToGeneric` reduction locks `{0}`, while
//! the reverse locks `{1}`. CR 601.2f hands that choice to the caster, so the
//! engine models the election rather than silently picking the cheaper order.
//!
//! The snapshot types here are what the election is taken over. They are
//! captured at the lock seam rather than re-derived on the answer, because
//! CR 601.2h's Altar's Reap / Thunderscape Familiar example makes the reducer's
//! continued existence irrelevant once the total cost is locked in — the
//! reduction stays determined even though the Familiar has left the
//! battlefield by the time mana is actually paid.

use serde::{Deserialize, Serialize};

use crate::types::ability::AbilityCost;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaCost, ManaCostShard};
use crate::types::statics::CostReductionReach;

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

/// Where one snapshot reduction came from.
///
/// Typed rather than a bare [`ObjectId`] because two of the five reduction
/// channels have no producing object at all: Affinity (CR 702.41a) and
/// Undaunted (CR 702.125a) are derived from the spell's own keywords, and
/// [`crate::types::game_state::PendingSpellCostReduction`] carries only
/// `player` / `amount` / `spell_filter`. A sentinel id for those would be a lie
/// the UI and the AI would both have to decode.
///
/// RESERVED VARIANTS: `PendingOneShot` / `Affinity` / `Undaunted` name the three
/// spell channels that are structurally generic-only — they reduce generic mana
/// and nothing else — so [`CostReductionEntry::is_order_relevant`] excludes them
/// from the spell permutation set by construction and they never reach a
/// snapshot. They are kept (and wire-tested) because the taxonomy is the honest
/// one: the day a printed Affinity-shaped or one-shot reduction carries a shard,
/// the entry needs a name that is not a fabricated `ObjectId`. Every other
/// variant is constructed: `Static`, `Defiler` and `CastingPermission` by the
/// spell election, `Static`, `AbilityCostRider` and `TransientEffect` by the
/// activation election.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ReductionProvenance {
    /// A `StaticMode::ModifyCost { mode: Reduce }` static. `ordinal`
    /// disambiguates multiple reducing statics printed on one source.
    Static { source: ObjectId, ordinal: u8 },
    /// CR 601.2b: an accepted Defiler-cycle life payment. No source id is
    /// carried because at most one Defiler reduction ever applies to a cast —
    /// `find_defiler_reduction` returns the first matching permanent and stops
    /// — so the bare variant is already unique within one reduction set.
    Defiler,
    /// CR 601.2f: the ELECTED casting permission's own "spells cast this way
    /// cost {N} more/less to cast" rider. No source id is carried because only
    /// ONE permission is elected per cast (`selected_permission_cast_cost_modifier`
    /// consults `casting_permission_index` alone), so the bare variant is
    /// already unique within a reduction set.
    CastingPermission,
    /// RESERVED (see the type-level note): a one-shot
    /// `pending_spell_cost_reductions` entry ("the next spell you cast this
    /// turn costs {2} less"), identified by its index in that vec. Generic-only
    /// today, so it is never snapshotted.
    PendingOneShot { index: usize },
    /// RESERVED (see the type-level note). CR 702.41a: Affinity, derived from
    /// the spell's own keyword. Reduces generic mana only.
    Affinity,
    /// RESERVED (see the type-level note). CR 702.125a: Undaunted, derived from
    /// the spell's own keyword. Reduces generic mana only.
    Undaunted,
    /// CR 602.2b: the activating ability's OWN "this ability costs {N} less to
    /// activate" rider (`AbilityDefinition::cost_reduction`). Like Affinity it
    /// has no producing permanent. An ability carries at most one rider, so the
    /// bare variant is already unique within one activation's reduction set.
    AbilityCostRider,
    /// CR 611.2: a duration-scoped continuous `ReduceAbilityCost` effect (The
    /// Dining Car's "activated abilities of <X> cost {N} less this turn").
    /// `effect` is the installing `TransientContinuousEffect::id`; `ordinal`
    /// is the reducer's index among that effect's `AddStaticMode` reducers.
    TransientEffect { effect: u64, ordinal: u8 },
}

/// CR 601.2b + CR 601.2f: everything the caster elects at the cost-determination
/// seam, as one value.
///
/// The two axes are separate rules steps but a single decision, because they
/// are not independent: CR 601.2b's hybrid announcement fixes WHICH pips exist
/// for CR 601.2f's reductions to cancel, so the reachable locked totals are a
/// function of the PAIR. Carrying them apart would let a caller apply an order
/// against an un-announced cost and lock a total the caster never saw.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReductionElection {
    /// CR 601.2f: the order-relevant reductions, in the order the caster elected
    /// to apply them. Identified by provenance rather than index because the
    /// target-independent and target-dependent collection passes have no shared
    /// index space.
    pub order: Vec<ReductionProvenance>,
    /// CR 601.2b: the announced nonhybrid equivalent for each *announceable*
    /// hybrid symbol in the cost, in cost order — see
    /// [`crate::game::casting::announceable_hybrid_positions`] for which symbols
    /// those are and why. Empty means "announce nothing", which leaves every
    /// hybrid symbol in the locked cost and defers the same choice to payment.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hybrid_announcement: Vec<ManaCostShard>,
}

/// One cost reduction, snapshotted at the CR 601.2f lock seam.
///
/// `amount` × `multiplier` is the *effective* reduction — the dynamic count
/// (`dynamic_count`, Affinity's permanent count, Undaunted's opponent count)
/// has already been resolved, so nothing here needs the game state to be
/// re-read when the caster's answer arrives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReductionEntry {
    /// The per-application reduction amount (CR 118.7).
    pub amount: ManaCost,
    /// How many times `amount` applies.
    pub multiplier: u32,
    /// CR 118.7b/c/d: whether an unmatched unit spills into generic mana.
    #[serde(
        default,
        skip_serializing_if = "crate::types::statics::CostReductionReach::is_spills_to_generic"
    )]
    pub reach: CostReductionReach,
    pub provenance: ReductionProvenance,
    /// Human-readable label for the prompt ("Morophon, the Boundless"). The
    /// frontend renders this verbatim; it computes nothing from it.
    pub display_name: String,
    /// CR 601.2f: the reduction's floor — "This effect can't reduce the mana in
    /// that cost to less than N mana" (Training Grounds, Zirda, Agatha). 0 means
    /// unfloored. Only activated-ability reductions carry one: `ModifyCost` has
    /// no floor, so every spell entry is 0 and, skipped at 0, serializes exactly
    /// as it did before the field existed.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub minimum_mana: u32,
}

impl CostReductionEntry {
    /// CR 601.2f: whether this entry's position in the reduction order can
    /// change the locked cost.
    ///
    /// A reduction whose amount carries no shards only ever decrements the
    /// generic component. Generic decrements are order-independent (the result
    /// is `max(0, generic - k)` for any interleaving), and they can never
    /// change whether a *shard*-bearing reduction finds a matching pip, since
    /// [`crate::game::casting::apply_shard_reduction`] matches against the
    /// shard list alone. So generic-only entries commute with every other
    /// entry and are excluded from the permutation set outright — which is why
    /// the overwhelmingly common cast (Affinity, Undaunted, one-shot "costs
    /// {N} less" reductions, and the 492 generic-only `Reduce` statics) never
    /// reaches a prompt.
    ///
    /// This is the FLOOR-FREE (spell) criterion. It does not hold once floors
    /// differ: an unfloored and a floored generic reduction do not commute
    /// (CR 601.2f "can't reduce ... to less than one mana" binds only when the
    /// floored one runs last). The activated-ability election therefore decides
    /// relevance over the whole set — "do the effective floors differ?" — in
    /// `crate::game::casting`, not per entry here.
    pub fn is_order_relevant(&self) -> bool {
        amount_is_order_relevant(&self.amount)
    }
}

/// CR 601.2f: the amount-level half of [`CostReductionEntry::is_order_relevant`],
/// as a free function so a *prospective* reduction — a `StaticMode::ModifyCost`
/// amount read straight off a static, before any entry is built — can be tested
/// against the same single authority. The cheap pre-gate in
/// `crate::game::casting` uses it to decide whether a cast can produce an
/// ordering election at all without paying for the full collector walks.
pub fn amount_is_order_relevant(amount: &ManaCost) -> bool {
    matches!(amount, ManaCost::Cost { shards, .. } if !shards.is_empty())
}

/// One legal CR 601.2b + CR 601.2f outcome: a representative election together
/// with the total cost it locks in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReductionOutcome {
    /// A permutation of indices into the prompt's `reductions` vec. Index 0 is
    /// applied first.
    pub order: Vec<usize>,
    /// CR 601.2b: the announced nonhybrid equivalents this outcome was computed
    /// under, one per entry of the prompt's `hybrid_symbols` vec. Empty means
    /// the outcome announces nothing and every hybrid symbol survives into the
    /// locked cost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hybrid_announcement: Vec<ManaCostShard>,
    /// The total cost this outcome locks in (CR 601.2f), floors included.
    pub locked_cost: ManaCost,
}

/// Whether the analyzer proved it enumerated every reachable locked cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CostReductionCoverage {
    /// Every permutation was explored; `outcomes` is the complete set of
    /// distinct locked costs.
    #[default]
    Exhaustive,
    /// The search budget was hit. Every listed outcome is still a legal
    /// CR 601.2f result, but the list may be incomplete.
    Partial,
}

/// The analyzer's verdict for one cast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostReductionAnalysis {
    /// The order-relevant snapshot entries the permutations range over, in
    /// canonical collection order.
    pub reductions: Vec<CostReductionEntry>,
    /// CR 601.2b: the hybrid symbols this cast announces a nonhybrid equivalent
    /// for, in cost order. Each outcome's `hybrid_announcement` is parallel to
    /// this vec.
    pub hybrid_symbols: Vec<ManaCostShard>,
    /// One representative per distinct locked cost, caster-optimal first.
    pub outcomes: Vec<CostReductionOutcome>,
    pub coverage: CostReductionCoverage,
}

impl CostReductionAnalysis {
    /// CR 601.2f: the caster only gets a choice when two permutations lock in
    /// genuinely different total costs. One outcome (or none) means every legal
    /// order is observationally identical, so electing silently is not a choice
    /// taken away from anyone.
    pub fn needs_election(&self) -> bool {
        self.outcomes.len() > 1
    }
}

/// CR 601.2f + CR 602.2b: every cost modifier that applies to one activation,
/// captured ONCE at the fold, together with where its total cost stands.
///
/// CR 602.2b makes an activation cost the analog of a spell's mana cost for
/// CR 601.2f, so the same "plus all ... cost increases, and minus all cost
/// reductions ... in any order" determination applies. The lock re-applies an
/// order to THIS snapshot rather than re-collecting the board, for the reason
/// CR 601.2h's Altar's Reap example gives: a reduction stays determined even if
/// paying a cost later removes its source.
///
/// Its presence on an in-flight activation IS the fold state: an activation
/// that has passed its fold carries one on every `PendingCast` (and on
/// `WaitingFor::AbilityModeChoice`) until it reaches the stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationCostSnapshot {
    /// The activation cost before any modifier: the printed cost.
    pub base_cost: AbilityCost,
    /// The sum of every applying raise. Raises are generic-only and are all
    /// applied before any reduction (CR 601.2f), so only their sum is
    /// observable; they never enter the election.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub raise_total: u32,
    /// Every applying reduction, dynamic counts already resolved, in canonical
    /// collection order: the ability's own rider, then battlefield statics,
    /// then duration-scoped continuous effects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reductions: Vec<CostReductionEntry>,
    pub lock: ActivationCostLock,
}

/// CR 601.2f: whether an activation's total cost has been "locked in".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ActivationCostLock {
    /// Not locked yet. `point` is where it WILL lock: the pending activation a
    /// `WaitingFor::OrderCostReductions` prompt carries names the continuation
    /// its answer resumes, and a carrier whose lock was deferred past
    /// announcement (a mana `{X}`, announced later — CR 601.2b before
    /// CR 601.2f) names the later point that locks it.
    Open {
        #[serde(default)]
        point: ActivationCostLockPoint,
    },
    /// Locked exactly once, at `point`. `order` is the caster's elected
    /// reduction order; `None` means no order was observable, so the
    /// caster-optimal default governs.
    Locked {
        #[serde(default)]
        point: ActivationCostLockPoint,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        order: Option<Vec<ReductionProvenance>>,
    },
}

/// The fold point after which an activation's cost lock runs, and so where the
/// activation resumes once the caster answers. The contract every point obeys:
/// a lock DEFERS while a later fold can still change the modifiers or the cost
/// they apply to, and runs exactly once, after the final fold and before the
/// first payment. Externally tagged so a later fold point can be added without
/// changing an existing point's bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActivationCostLockPoint {
    /// CR 602.2b + CR 601.2b-f: the activation's announcement, where every
    /// activation cost modifier this engine models is determined — the final
    /// fold unless the cost carries a mana `{X}`.
    #[default]
    Announcement,
    /// CR 601.2b + CR 601.2f: a mana `{X}` is announced before the total cost
    /// is determined, so its reductions (and their floors, which count the
    /// cost's mana) are folded and locked once X is chosen.
    XAnnounced,
}
