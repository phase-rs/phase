use thiserror::Error;

use crate::game::quantity::{
    continuous_modification_uses_unspent_mana, static_condition_uses_unspent_mana,
};
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{GameState, ShardChoice};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{
    ManaCost, ManaCostShard, ManaExpiry, ManaPool, ManaRestriction, ManaSpellGrant, ManaType,
    ManaUnit, PaymentContext,
};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// Color demand array indexed by WUBRG (White=0, Blue=1, Black=2, Red=3, Green=4).
/// CR 107.4a: The five colors are white ({W}), blue ({U}), black ({B}), red ({R}), green ({G}).
pub type ColorDemand = [u32; 5];

/// Units of each mana type kept in a debug "infinite mana" pool. Large enough to
/// cover any single resolution's worth of spends, small enough that the pool's
/// linear spend scan (`ManaPool` is a `Vec<ManaUnit>`) stays cheap.
const INFINITE_MANA_PER_TYPE: usize = 100;

/// The six mana types an infinite-mana pool is seeded with: the five colors
/// (CR 105.1) plus colorless (CR 107.4c).
const INFINITE_MANA_TYPES: [ManaType; 6] = [
    ManaType::White,
    ManaType::Blue,
    ManaType::Black,
    ManaType::Red,
    ManaType::Green,
    ManaType::Colorless,
];

pub(crate) fn has_unspent_mana_continuous_effects(state: &GameState) -> bool {
    state.transient_continuous_effects.iter().any(|effect| {
        effect
            .condition
            .as_ref()
            .is_some_and(static_condition_uses_unspent_mana)
            || effect
                .modifications
                .iter()
                .any(continuous_modification_uses_unspent_mana)
    }) || state.objects.values().any(|obj| {
        obj.static_definitions.iter_all().any(|def| {
            def.mode == StaticMode::Continuous
                && (def
                    .condition
                    .as_ref()
                    .is_some_and(static_condition_uses_unspent_mana)
                    || def
                        .modifications
                        .iter()
                        .any(continuous_modification_uses_unspent_mana))
        })
    })
}

/// Debug-only: top every player in `GameState::debug_infinite_mana` back up to
/// `INFINITE_MANA_PER_TYPE` unrestricted, non-expiring units of each mana type.
///
/// Idempotent — only the shortfall is added — and returns immediately when no
/// player is flagged, so it is cheap to call after every action. Paired with the
/// `UnitDisposition::Keep` override in `turns::drain_pending_phase_transition_progress`
/// (which suppresses the CR 500.4 end-of-step empty for flagged players), this
/// keeps the pool continuously full. Both the affordability check
/// (`reduce_cost_by_pool`) and the real payment path read the same
/// `player.mana_pool`, so a flagged player can pay any cost without divergence
/// between "shows castable" and "actually pays".
///
/// NOT a rules-legal effect — a developer convenience gated behind the same
/// debug-action permission as every other `DebugAction`.
pub fn refill_infinite_mana(state: &mut GameState) {
    if state.debug_infinite_mana.is_empty() {
        return;
    }
    let flagged: Vec<PlayerId> = state.debug_infinite_mana.iter().copied().collect();
    for &player_id in &flagged {
        let Some(player) = state.players.iter_mut().find(|p| p.id == player_id) else {
            continue;
        };
        for color in INFINITE_MANA_TYPES {
            // Count only the units this top-up owns (unrestricted, non-expiring)
            // so card-produced restricted/expiring mana never suppresses a refill.
            let have = player
                .mana_pool
                .mana
                .iter()
                .filter(|u| u.color == color && u.restrictions.is_empty() && u.expiry.is_none())
                .count();
            for _ in have..INFINITE_MANA_PER_TYPE {
                player
                    .mana_pool
                    .add(ManaUnit::new(color, ObjectId(0), false, Vec::new()));
            }
        }
    }
    // Mark display dirty only after the mutable-player borrow above is released.
    for &player_id in &flagged {
        super::public_state::mark_public_state_player_dirty(state, player_id);
    }
    super::public_state::mark_mana_display_dirty(state);
}

fn mana_type_to_demand_index(mt: ManaType) -> Option<usize> {
    match mt {
        ManaType::White => Some(0),
        ManaType::Blue => Some(1),
        ManaType::Black => Some(2),
        ManaType::Red => Some(3),
        ManaType::Green => Some(4),
        ManaType::Colorless => None,
    }
}

/// Accumulate the colored-pip demand of a single cost shard into `demand` (WUBRG).
///
/// CR 107.4b: Generic pips ({1}, {X}) are payable with any mana and so contribute
/// nothing to colored demand. Only colored requirements (Single, Hybrid, the colored
/// half of {2/C}/Phyrexian/{C/color}) increment the per-color counts. Hybrids count
/// both halves because either color could be the one chosen to pay.
fn accumulate_shard_demand(demand: &mut ColorDemand, shard: ManaCostShard) {
    match shard_to_mana_type(shard) {
        ShardRequirement::Single(mt) => {
            if let Some(i) = mana_type_to_demand_index(mt) {
                demand[i] += 1;
            }
        }
        ShardRequirement::Hybrid(a, b) | ShardRequirement::HybridPhyrexian(a, b) => {
            // Both colors count as demanded (either could be needed)
            if let Some(i) = mana_type_to_demand_index(a) {
                demand[i] += 1;
            }
            if let Some(i) = mana_type_to_demand_index(b) {
                demand[i] += 1;
            }
        }
        ShardRequirement::TwoGenericHybrid(mt)
        | ShardRequirement::Phyrexian(mt)
        | ShardRequirement::ColorlessHybrid(mt)
        // CR 107.4f: K'rrik promotion never reaches the
        // demand calc (shard_to_mana_type is the only
        // producer), but the variant must be handled for
        // exhaustiveness. Same demand shape as TwoGenericHybrid.
        | ShardRequirement::TwoGenericHybridPhyrexian(mt) => {
            if let Some(i) = mana_type_to_demand_index(mt) {
                demand[i] += 1;
            }
        }
        ShardRequirement::Snow | ShardRequirement::X => {}
        ShardRequirement::TwoOrMoreColorSource => {}
    }
}

/// Count how many colored pips the other cards in hand demand (WUBRG).
/// Used to decide which hybrid color to spend — spend the least-demanded one.
pub fn compute_hand_color_demand(
    state: &GameState,
    player_id: PlayerId,
    excluding: ObjectId,
) -> ColorDemand {
    let mut demand = [0u32; 5];
    let player = match state.players.iter().find(|p| p.id == player_id) {
        Some(p) => p,
        None => return demand,
    };
    for &obj_id in &player.hand {
        if obj_id == excluding {
            continue;
        }
        if let Some(obj) = state.objects.get(&obj_id) {
            if let ManaCost::Cost { shards, .. } = &obj.mana_cost {
                for shard in shards {
                    accumulate_shard_demand(&mut demand, *shard);
                }
            }
        }
    }
    demand
}

/// Colored-pip demand of an *outer* cost still being paid (WUBRG).
///
/// CR 107.4b: generic pips of the outer cost contribute nothing — they can be paid
/// with any mana, so funding a nested sub-cost from them strands no colored
/// requirement. Only the outer cost's colored shards are "reserved": spending one of
/// those colors on a nested mana-ability sub-cost could leave the outer cost
/// unpayable, so the nested spend deprioritizes them. Empty for NoCost / Self* costs.
pub(crate) fn outer_cost_color_demand(cost: &ManaCost) -> ColorDemand {
    let mut demand = [0u32; 5];
    if let ManaCost::Cost { shards, .. } = cost {
        for &shard in shards {
            accumulate_shard_demand(&mut demand, shard);
        }
    }
    demand
}

#[derive(Debug, Clone, Error, PartialEq)]
pub enum PaymentError {
    #[error("Insufficient mana")]
    InsufficientMana,
    #[error("Invalid cost")]
    InvalidCost,
}

/// Result of a Phyrexian mana payment that used life instead of mana (CR 107.4f).
///
/// CR 107.4f: A Phyrexian mana symbol represents a cost that can be paid either
/// with one mana of its color or by paying 2 life.
#[derive(Debug, Clone, PartialEq)]
pub struct LifePayment {
    pub amount: i32,
}

/// Produce mana and add it to a player's mana pool (CR 106.3 + CR 106.4).
///
/// CR 106.3: Mana is produced by mana abilities. The source of the mana is the
/// source of the ability that produced it (CR 113.7).
/// CR 106.4: When an effect instructs a player to add mana, it goes into their mana pool.
/// CR 614.1a: Before adding, the proposed `ProduceMana` event is routed through
/// the replacement pipeline so static effects (Contamination, Pale Moon, etc.)
/// can rewrite the mana type or prevent production entirely.
pub fn produce_mana(
    state: &mut GameState,
    source_id: ObjectId,
    mana_type: ManaType,
    player_id: PlayerId,
    tapped_for_mana: bool,
    events: &mut Vec<GameEvent>,
) {
    produce_mana_with_attributes(
        state,
        source_id,
        mana_type,
        player_id,
        tapped_for_mana,
        &[],
        &[],
        None,
        events,
    );
}

/// Produce mana and add it to a player's mana pool, carrying spend restrictions,
/// spell grants, and expiry semantics (CR 106.6 + CR 106.4).
///
/// CR 106.6: Some spells or abilities that produce mana restrict how that mana
/// can be spent (e.g., Flamebraider: "Spend this mana only to cast Elemental
/// spells or activate abilities of Elemental sources."). Restrictions attach to
/// each produced `ManaUnit` so the spend-mana payment gate can reject illegal
/// uses via `ManaRestriction::allows_spell` / `allows_activation`.
#[allow(clippy::too_many_arguments)]
pub fn produce_mana_with_attributes(
    state: &mut GameState,
    source_id: ObjectId,
    mana_type: ManaType,
    player_id: PlayerId,
    tapped_for_mana: bool,
    restrictions: &[ManaRestriction],
    grants: &[ManaSpellGrant],
    expiry: Option<ManaExpiry>,
    events: &mut Vec<GameEvent>,
) {
    let source_could_produce_two_or_more_colors =
        super::mana_sources::source_could_produce_two_or_more_colors(state, source_id, player_id);
    produce_mana_with_attributes_from_source_quality(
        state,
        source_id,
        mana_type,
        player_id,
        tapped_for_mana,
        source_could_produce_two_or_more_colors,
        restrictions,
        grants,
        expiry,
        events,
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn produce_mana_with_attributes_from_source_quality(
    state: &mut GameState,
    source_id: ObjectId,
    mana_type: ManaType,
    player_id: PlayerId,
    tapped_for_mana: bool,
    source_could_produce_two_or_more_colors: bool,
    restrictions: &[ManaRestriction],
    grants: &[ManaSpellGrant],
    expiry: Option<ManaExpiry>,
    events: &mut Vec<GameEvent>,
) {
    use crate::game::replacement::{self, ReplacementResult};
    use crate::types::proposed_event::ProposedEvent;

    let proposed =
        ProposedEvent::produce_mana_with_context(source_id, player_id, mana_type, tapped_for_mana);
    let (final_mana_type, final_count) = match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::ProduceMana {
            mana_type: resolved,
            count,
            ..
        }) => (resolved, count),
        // CR 614.1: A fully-prevented mana production produces no mana.
        ReplacementResult::Prevented => return,
        // CR 614.5: Mana-type replacements do not require a player choice; any
        // other outcome (including unexpected pipeline results) falls back to
        // the original type so mana production is never silently dropped.
        _ => (mana_type, 1),
    };

    for _ in 0..final_count {
        let unit = ManaUnit {
            color: final_mana_type,
            source_id,
            supertype: None,
            source_could_produce_two_or_more_colors,
            restrictions: restrictions.to_vec(),
            grants: grants.to_vec(),
            expiry,
        };

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == player_id)
            .expect("player exists");
        player.mana_pool.add(unit);

        events.push(GameEvent::ManaAdded {
            player_id,
            mana_type: final_mana_type,
            source_id,
            tap_state: ManaTapState::from_tap(tapped_for_mana),
        });
    }
    if final_count > 0 && has_unspent_mana_continuous_effects(state) {
        state.layers_dirty.mark_full();
    }
}

/// Check if the mana pool can pay the given cost (CR 202.1a).
///
/// CR 202.1a: Paying a mana cost requires matching the type of any colored or colorless
/// mana symbols as well as paying the generic mana indicated in the cost.
///
/// This convenience wrapper assumes zero Phyrexian-life payments are available. Cost
/// validation paths that know the caster's life total and CantLoseLife status must call
/// [`can_pay_for_spell`] with a computed `max_life_payments` to honor CR 107.4f.
pub fn can_pay(pool: &ManaPool, cost: &ManaCost) -> bool {
    can_pay_for_spell(
        pool,
        cost,
        None,
        crate::types::mana::CostPermissionContext::default(),
    )
}

/// Classification of a mana cost for auto-pay eligibility.
///
/// `Unambiguous` means the cost can be paid without a player-level rules decision:
/// all shards map to a single mana type (after X has been concretized). `pay_mana_cost`
/// can resolve the payment deterministically, and the `WaitingFor::ManaPayment` state
/// adds no information — it is pure ceremony.
///
/// The other variants name which rules decision a player still owes. CR 601.2h requires
/// these to be resolved by the caster before mana is paid, so we must surface the
/// `ManaPayment` UI for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentClassification {
    /// No hybrid or Phyrexian shards remain — `pay_mana_cost` can auto-tap and spend.
    Unambiguous,
    /// Hybrid shard (`{W/U}`, `{2/W}`, `{C/W}`, ...) requires a color choice. CR 107.4e.
    NeedsHybridChoice,
    /// Phyrexian shard (`{W/P}`, `{W/U/P}`, ...) requires a mana-vs-2-life choice. CR 107.4f.
    NeedsPhyrexianChoice,
}

/// Decide whether a concretized mana cost can be paid without any further player decision.
///
/// Inspects each shard through the existing `ShardRequirement` discriminator and flags
/// the first hybrid or Phyrexian requirement found. Generic / `Single(color)` / `Snow`
/// shards are always unambiguous — `pay_mana_cost` already picks sources deterministically
/// and handles auto-tap of free producers.
///
/// CR 601.2h: The player must choose how to pay for hybrid and Phyrexian mana as part
/// of determining total cost. This predicate is the single authority on whether that
/// choice is actually present in a given cost.
pub fn classify_payment(cost: &ManaCost) -> PaymentClassification {
    let ManaCost::Cost { shards, .. } = cost else {
        return PaymentClassification::Unambiguous;
    };
    for shard in shards {
        match shard_to_mana_type(*shard) {
            ShardRequirement::Hybrid(..)
            | ShardRequirement::TwoGenericHybrid(..)
            | ShardRequirement::ColorlessHybrid(..) => {
                return PaymentClassification::NeedsHybridChoice;
            }
            ShardRequirement::Phyrexian(..)
            | ShardRequirement::HybridPhyrexian(..)
            // CR 107.4f: K'rrik-promoted {2/C} carries the Phyrexian-pause
            // shape (mana-vs-life choice). `shard_to_mana_type` never
            // emits this, but if a caller passes a pre-promoted shard
            // through `classify_payment`, surface the right classification.
            | ShardRequirement::TwoGenericHybridPhyrexian(..) => {
                return PaymentClassification::NeedsPhyrexianChoice;
            }
            ShardRequirement::Single(..)
            | ShardRequirement::Snow
            | ShardRequirement::X
            | ShardRequirement::TwoOrMoreColorSource => {}
        }
    }
    PaymentClassification::Unambiguous
}

/// CR 107.4f + CR 118.3: True for shard requirements that carry the Phyrexian
/// mana-vs-life choice — `{C/P}`, hybrid `{C/C/P}`, and the K'rrik-promoted
/// `{2/C/P}`. The payment seams defer these until after strict requirements so
/// each life-vs-mana decision sees the pool that actually remains.
fn is_phyrexian_requirement(req: &ShardRequirement) -> bool {
    matches!(
        req,
        ShardRequirement::Phyrexian(..)
            | ShardRequirement::HybridPhyrexian(..)
            | ShardRequirement::TwoGenericHybridPhyrexian(..)
    )
}

/// CR 107.4f + CR 118.3: Order shard indices so every non-Phyrexian shard is
/// resolved before any Phyrexian-shape shard, mirroring `can_pay_for_spell`'s
/// deferral. Deciding a Phyrexian shard against the pool *after* strict
/// requirements are met stops a contested colored source from being spent on
/// the Phyrexian option and starving a strict shard — e.g. `{B/P}{B}` with one
/// Swamp must spend the black on the strict `{B}` and pay life for `{B/P}`.
/// Phyrexian indices keep their relative order so per-shard `ShardChoice`
/// cursors and `PhyrexianShard` results stay aligned with the printed cost.
fn phyrexian_deferred_order(
    shards: &[ManaCostShard],
    life_colors: crate::types::mana::LifePaymentColors,
) -> Vec<usize> {
    let is_phyrexian = |i: usize| {
        is_phyrexian_requirement(&effective_shard_requirement(
            shard_to_mana_type(shards[i]),
            life_colors,
        ))
    };
    (0..shards.len())
        .filter(|&i| !is_phyrexian(i))
        .chain((0..shards.len()).filter(|&i| is_phyrexian(i)))
        .collect()
}

/// Check if the pool can pay the cost, respecting mana restrictions when `spell` is provided.
///
/// CR 106.6: Some abilities that produce mana restrict how that mana can be spent.
/// When `spell` is `Some`, restricted mana (e.g., "only for creature spells") is only
/// counted if the restriction permits the given spell. When `None`, all mana is eligible.
///
/// CR 609.4b: When `any_color` is true, colored mana requirements can be paid with
/// mana of any color (e.g., Chromatic Orrery, Joiner Adept).
///
/// CR 107.4f + CR 118.3 + CR 119.8: `max_life_payments` caps the number of
/// Phyrexian shards that can be satisfied by paying 2 life. Callers compute this
/// from the prospective caster's life total and CantLoseLife status (see
/// [`crate::game::life_costs::can_pay_life_cost`]); pool-only contexts pass 0.
/// When a Phyrexian shard's mana option is unavailable, one payment is consumed
/// from the budget; if the budget is exhausted, the cost can't be paid.
pub fn can_pay_for_spell(
    pool: &ManaPool,
    cost: &ManaCost,
    spell: Option<&PaymentContext<'_>>,
    permissions: crate::types::mana::CostPermissionContext,
) -> bool {
    let any_color = permissions.any_color;
    let max_life_payments = permissions.max_life;
    let life_colors = permissions.life_colors;
    match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost | ManaCost::SelfManaValue => true,
        ManaCost::Cost { shards, generic } => {
            // Clone pool to simulate payment
            let mut sim = pool.clone();
            let mut life_budget = max_life_payments;

            // CR 107.4f + CR 118.3: Phyrexian shards are deferred until after
            // non-Phyrexian shards are resolved. A greedy "prefer mana" policy
            // for Phyrexian shards can starve the generic portion (e.g. 3 Islands
            // + cost {3}{U/P}: spending U for the shard leaves only 2 for generic
            // 3, but paying 2 life instead leaves 3U for generic). Deferral lets
            // us see remaining pool capacity before committing mana vs life.
            enum PhyrexianDeferred {
                Single(ManaType),
                Hybrid(ManaType, ManaType),
                // CR 107.4f: K'rrik-promoted {2/C} — pay 1 colored, 2 generic, OR 2 life.
                TwoGeneric(ManaType),
            }
            let mut deferred_phyrexian: Vec<PhyrexianDeferred> = Vec::new();

            // Pay non-Phyrexian colored shards first
            for shard in shards {
                // CR 107.4f: Apply K'rrik-style promotion before dispatch so the
                // post-promotion arms handle life-as-payment uniformly.
                match effective_shard_requirement(shard_to_mana_type(*shard), life_colors) {
                    ShardRequirement::Single(mt) => {
                        // CR 609.4b: When any_color is true, any mana can pay colored costs.
                        if any_color && mt != ManaType::Colorless {
                            if spend_any_for_required_colors(&mut sim, &[mt], spell, None).is_none()
                            {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, mt, spell).is_none() {
                            return false;
                        }
                    }
                    // CR 107.4e: Hybrid mana — can be paid with either color.
                    ShardRequirement::Hybrid(a, b) => {
                        if any_color {
                            if spend_any_for_required_colors(&mut sim, &[a, b], spell, None)
                                .is_none()
                            {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, a, spell).is_none()
                            && spend_eligible(&mut sim, b, spell).is_none()
                        {
                            return false;
                        }
                    }
                    // CR 107.4f: Phyrexian mana — defer decision.
                    ShardRequirement::Phyrexian(color) => {
                        deferred_phyrexian.push(PhyrexianDeferred::Single(color));
                    }
                    // CR 107.4e: Monocolored hybrid {2/C} — pay 1 colored or 2 generic.
                    ShardRequirement::TwoGenericHybrid(color) => {
                        // CR 609.4b: When any_color, any mana satisfies the colored half.
                        if any_color {
                            if spend_any_for_required_colors(&mut sim, &[color], spell, None)
                                .is_none()
                            {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, color, spell).is_none() {
                            if spend_generic_eligible(&mut sim, spell, None).is_none() {
                                return false;
                            }
                            if spend_generic_eligible(&mut sim, spell, None).is_none() {
                                return false;
                            }
                        }
                    }
                    // CR 107.4h: Snow mana {S} — paid with mana from a snow source.
                    ShardRequirement::Snow => {
                        if !spend_snow(&mut sim) {
                            return false;
                        }
                    }
                    ShardRequirement::TwoOrMoreColorSource => {
                        if spend_two_or_more_color_source_eligible(&mut sim, spell).is_none() {
                            return false;
                        }
                    }
                    // CR 107.3: {X} — can be 0, so always satisfiable in a can-pay check.
                    ShardRequirement::X => {}
                    // CR 107.4e: Colorless hybrid {C/color} — pay colorless or colored.
                    ShardRequirement::ColorlessHybrid(color) => {
                        if any_color {
                            if spend_any_for_required_colors(
                                &mut sim,
                                &[ManaType::Colorless, color],
                                spell,
                                None,
                            )
                            .is_none()
                            {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, ManaType::Colorless, spell).is_none()
                            && spend_eligible(&mut sim, color, spell).is_none()
                        {
                            return false;
                        }
                    }
                    // CR 107.4f: Hybrid Phyrexian — defer decision.
                    ShardRequirement::HybridPhyrexian(a, b) => {
                        deferred_phyrexian.push(PhyrexianDeferred::Hybrid(a, b));
                    }
                    // CR 107.4f: K'rrik-promoted {2/C} — defer like other
                    // Phyrexian-shape shards so the life-vs-mana decision sees
                    // the full pool remaining after non-Phyrexian shards.
                    ShardRequirement::TwoGenericHybridPhyrexian(color) => {
                        deferred_phyrexian.push(PhyrexianDeferred::TwoGeneric(color));
                    }
                }
            }

            // CR 107.4f + CR 118.3 + CR 119.8: Resolve deferred Phyrexian shards.
            // For each shard, pay with mana only if the pool will still have enough
            // to cover the generic cost plus remaining Phyrexian shards that might
            // also need mana. Otherwise fall back to life payment.
            let total_pool_after_shards = sim.total();
            let mut mana_spent_on_phyrexian: usize = 0;
            for deferred in &deferred_phyrexian {
                let remaining_after_this =
                    total_pool_after_shards.saturating_sub(mana_spent_on_phyrexian);
                let still_needed_for_generic = *generic as usize;
                let can_spare_mana = remaining_after_this > still_needed_for_generic;

                let mana_ok = if can_spare_mana {
                    match deferred {
                        PhyrexianDeferred::Single(color) => {
                            if any_color {
                                spend_any_for_required_colors(&mut sim, &[*color], spell, None)
                                    .is_some()
                            } else {
                                spend_eligible(&mut sim, *color, spell).is_some()
                            }
                        }
                        PhyrexianDeferred::Hybrid(a, b) => {
                            if any_color {
                                spend_any_for_required_colors(&mut sim, &[*a, *b], spell, None)
                                    .is_some()
                            } else {
                                spend_eligible(&mut sim, *a, spell).is_some()
                                    || spend_eligible(&mut sim, *b, spell).is_some()
                            }
                        }
                        // CR 107.4f + CR 107.4e: {2/C} promoted by K'rrik —
                        // try 1 colored mana first; fall back to 2 generic
                        // (atomic — restore on partial failure); life option
                        // still consumed via the budget arm below.
                        PhyrexianDeferred::TwoGeneric(color) => {
                            if any_color {
                                spend_any_for_required_colors(&mut sim, &[*color], spell, None)
                                    .is_some()
                            } else if spend_eligible(&mut sim, *color, spell).is_some() {
                                true
                            } else {
                                let mut backup = sim.clone();
                                if spend_generic_eligible(&mut backup, spell, None).is_some()
                                    && spend_generic_eligible(&mut backup, spell, None).is_some()
                                {
                                    sim = backup;
                                    true
                                } else {
                                    false
                                }
                            }
                        }
                    }
                } else {
                    false
                };

                if mana_ok {
                    mana_spent_on_phyrexian += 1;
                } else {
                    // CR 118.3 + CR 119.8: Life fallback requires budget.
                    if life_budget == 0 {
                        return false;
                    }
                    life_budget -= 1;
                }
            }

            // Pay generic
            for _ in 0..*generic {
                if spend_generic_eligible(&mut sim, spell, None).is_none() {
                    return false;
                }
            }
            true
        }
    }
}

/// Pay a mana cost from the pool (CR 601.2h).
///
/// CR 601.2h: The player pays the total cost. Partial payments are not allowed.
/// Unpayable costs can't be paid.
///
/// Pool-level arithmetic only — the ability-cost payment authority
/// (`game/costs.rs::pay_cost`, see `.planning/cost-payment-unification/`)
/// sits above this and owns `AbilityCost` dispatch.
pub fn pay_from_pool(
    pool: &mut ManaPool,
    cost: &ManaCost,
) -> Result<(Vec<ManaUnit>, Vec<LifePayment>), PaymentError> {
    pay_cost_with_demand(pool, cost, None, None, false)
}

/// CR 601.2g: Simulate paying `cost` from a clone of `pool` and return the
/// residual cost the pool cannot cover. The auto-tap planner consults this so
/// floating mana (e.g. a pre-tapped Sol Ring) isn't double-counted by tapping
/// additional sources for shards the pool already satisfies.
///
/// This is the dry-run twin of `pay_cost_with_demand_and_choices`: it mirrors
/// that function's shard-by-shard eligibility checks against a scratch pool,
/// but records unmet shards into a new `ManaCost` instead of erroring on
/// shortfall. `spell`/`any_color` gate eligibility exactly as the real payment
/// does — restricted mana the spell can't use stays in the pool and the shard
/// stays in the residual.
///
/// Returns `ManaCost::NoCost` when the pool fully covers the cost so callers
/// can short-circuit.
pub(crate) fn reduce_cost_by_pool(
    pool: &ManaPool,
    cost: &ManaCost,
    spell: Option<&PaymentContext<'_>>,
    any_color: bool,
    demand: Option<&ColorDemand>,
) -> ManaCost {
    let (shards, generic) = match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost | ManaCost::SelfManaValue => return cost.clone(),
        ManaCost::Cost { shards, generic } => (shards, *generic),
    };

    let mut scratch = pool.clone();
    let mut residual_shards: Vec<ManaCostShard> = Vec::new();
    let mut residual_generic = generic;

    for &shard in shards {
        let paid = match shard_to_mana_type(shard) {
            // CR 107.4a/f + CR 609.4b: Exact color required (any_color relaxes to any mana).
            // Phyrexian's life-payment option lives in the real payment path — at the planner
            // layer we only check mana coverage; life-only payments leave the shard in the
            // residual but auto-tap's `needs` then generates zero sources (requires_life
            // ordering handles it downstream).
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                if any_color && color != ManaType::Colorless {
                    spend_any_for_required_colors(&mut scratch, &[color], spell, None).is_some()
                } else {
                    spend_eligible(&mut scratch, color, spell).is_some()
                }
            }
            // CR 107.4e/f: Hybrid pays either half.
            ShardRequirement::Hybrid(a, b) | ShardRequirement::HybridPhyrexian(a, b) => {
                if any_color {
                    spend_any_for_required_colors(&mut scratch, &[a, b], spell, None).is_some()
                } else {
                    spend_eligible(&mut scratch, a, spell).is_some()
                        || spend_eligible(&mut scratch, b, spell).is_some()
                }
            }
            // CR 107.4e: {C/color} — prefer colorless, else the colored half.
            ShardRequirement::ColorlessHybrid(color) => {
                if any_color {
                    spend_any_for_required_colors(
                        &mut scratch,
                        &[ManaType::Colorless, color],
                        spell,
                        None,
                    )
                    .is_some()
                } else {
                    spend_eligible(&mut scratch, ManaType::Colorless, spell).is_some()
                        || spend_eligible(&mut scratch, color, spell).is_some()
                }
            }
            // CR 107.4e: {2/color} — 1 colored is cheaper than 2 generic; try colored first.
            // The 2-generic fallback is atomic: we restore the scratch pool if we can't
            // afford both halves, rather than half-draining it.
            ShardRequirement::TwoGenericHybrid(color) => {
                if any_color {
                    spend_any_for_required_colors(&mut scratch, &[color], spell, None).is_some()
                } else if spend_eligible(&mut scratch, color, spell).is_some() {
                    true
                } else {
                    let mut backup = scratch.clone();
                    if spend_generic_non_demanded(&mut backup, spell, demand).is_some()
                        && spend_generic_non_demanded(&mut backup, spell, demand).is_some()
                    {
                        scratch = backup;
                        true
                    } else {
                        false
                    }
                }
            }
            // CR 107.4h: Snow mana only from snow sources.
            ShardRequirement::Snow => spend_snow_unit(&mut scratch).is_some(),
            ShardRequirement::TwoOrMoreColorSource => {
                spend_two_or_more_color_source_eligible(&mut scratch, spell).is_some()
            }
            // CR 107.1b: `ManaCost::concretize_x` strips `X` shards into generic
            // before auto-tap runs, so this arm is defensive. Keep the shard in
            // the residual so auto-tap's legacy `deferred_generic += 1` path
            // still fires in the edge case where an unconverted X reaches here.
            ShardRequirement::X => false,
            // CR 107.4f: K'rrik-promoted {2/C} is synthesized only by
            // `effective_shard_requirement`; `shard_to_mana_type` never emits
            // it, so this arm is unreachable through `reduce_cost_by_pool`'s
            // direct dispatch path. Pay-mana semantics mirror `TwoGenericHybrid`.
            ShardRequirement::TwoGenericHybridPhyrexian(color) => {
                if any_color {
                    spend_any_for_required_colors(&mut scratch, &[color], spell, None).is_some()
                } else if spend_eligible(&mut scratch, color, spell).is_some() {
                    true
                } else {
                    let mut backup = scratch.clone();
                    if spend_generic_non_demanded(&mut backup, spell, demand).is_some()
                        && spend_generic_non_demanded(&mut backup, spell, demand).is_some()
                    {
                        scratch = backup;
                        true
                    } else {
                        false
                    }
                }
            }
        };
        if !paid {
            residual_shards.push(shard);
        }
    }

    // CR 107.4b: Generic may be paid with any eligible mana. When a nested
    // sub-cost's outer-cost `demand` is supplied, a generic pip is counted
    // covered ONLY if a non-demanded scratch unit can pay it — a demanded unit
    // left over is reserved for the outer cost's colored shard (CR 118.10), so
    // the pip stays in `residual_generic` and auto-tap will tap another source
    // for it. Without `demand` the prior least-available ordering is preserved.
    for _ in 0..generic {
        if spend_generic_non_demanded(&mut scratch, spell, demand).is_some() {
            residual_generic = residual_generic.saturating_sub(1);
        } else {
            break;
        }
    }

    if residual_shards.is_empty() && residual_generic == 0 {
        ManaCost::NoCost
    } else {
        ManaCost::Cost {
            shards: residual_shards,
            generic: residual_generic,
        }
    }
}

/// Pay a mana cost with hand-demand-aware hybrid resolution (CR 601.2f + CR 601.2h).
///
/// CR 601.2f: If a cost includes hybrid mana symbols, the player announces the nonhybrid
/// equivalent cost they intend to pay. If it includes Phyrexian mana symbols, the player
/// announces whether to pay 2 life or the corresponding colored mana for each.
///
/// CR 609.4b: When `any_color` is true, colored mana requirements can be paid with
/// mana of any color (e.g., Chromatic Orrery).
pub fn pay_cost_with_demand(
    pool: &mut ManaPool,
    cost: &ManaCost,
    hand_demand: Option<&ColorDemand>,
    spell: Option<&PaymentContext<'_>>,
    any_color: bool,
) -> Result<(Vec<ManaUnit>, Vec<LifePayment>), PaymentError> {
    pay_cost_with_demand_and_choices(
        pool,
        cost,
        hand_demand,
        spell,
        any_color,
        None,
        crate::types::mana::LifePaymentColors::EMPTY,
    )
}

/// Pay a mana cost with an optional explicit Phyrexian choice vector.
///
/// CR 107.4f + CR 601.2f: When `phyrexian_choices` is `Some`, the caller has pre-resolved
/// the per-shard mana-vs-2-life decision (see `WaitingFor::PhyrexianPayment`). Each
/// Phyrexian shard consumes one choice from the vector in order; `PayLife` produces a
/// `LifePayment`, `PayMana` spends one mana of the shard's color (hybrid-Phyrexian picks
/// via `auto_pay_hybrid`). A `None` choice vector preserves the existing auto-decision
/// behavior: prefer mana when available, fall back to 2 life.
pub fn pay_cost_with_demand_and_choices(
    pool: &mut ManaPool,
    cost: &ManaCost,
    hand_demand: Option<&ColorDemand>,
    spell: Option<&PaymentContext<'_>>,
    any_color: bool,
    phyrexian_choices: Option<&[ShardChoice]>,
    life_colors: crate::types::mana::LifePaymentColors,
) -> Result<(Vec<ManaUnit>, Vec<LifePayment>), PaymentError> {
    match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost | ManaCost::SelfManaValue => {
            Ok((Vec::new(), Vec::new()))
        }
        ManaCost::Cost { shards, generic } => {
            let mut spent = Vec::new();
            let mut life_payments = Vec::new();
            let mut choice_cursor = 0usize;
            let mut preferred_hybrid_colors: Vec<(ManaType, ManaType, ManaType)> = Vec::new();

            // CR 107.4a + CR 107.4f + CR 118.3: Pay non-Phyrexian shards before
            // Phyrexian-shape shards (see `phyrexian_deferred_order`) so each
            // mana-vs-life decision sees the pool remaining after every strict
            // requirement is met. K'rrik promotion is applied per shard so the
            // life-as-payment arms handle the ShardChoice uniformly.
            for idx in phyrexian_deferred_order(shards, life_colors) {
                match effective_shard_requirement(shard_to_mana_type(shards[idx]), life_colors) {
                    ShardRequirement::Single(mt) => {
                        // CR 609.4b: When any_color, any mana can pay colored costs.
                        if any_color && mt != ManaType::Colorless {
                            let unit = spend_any_for_required_colors(pool, &[mt], spell, None)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        } else {
                            let unit = spend_eligible(pool, mt, spell)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        }
                    }
                    // CR 107.4e: Hybrid mana — pay with either half.
                    ShardRequirement::Hybrid(a, b) => {
                        if any_color {
                            let unit = spend_any_for_required_colors(pool, &[a, b], spell, None)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        } else {
                            let remaining_pair_shards =
                                count_remaining_hybrid_shards(shards, idx, a, b);
                            let color = select_hybrid_payment_color(
                                pool,
                                a,
                                b,
                                hand_demand,
                                remaining_pair_shards,
                                &mut preferred_hybrid_colors,
                            );
                            let unit = spend_eligible(pool, color, spell)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        }
                    }
                    // CR 107.4f: Phyrexian mana — pay color or 2 life.
                    ShardRequirement::Phyrexian(color) => {
                        let explicit_choice = phyrexian_choices
                            .and_then(|choices| choices.get(choice_cursor).copied());
                        if explicit_choice.is_some() {
                            choice_cursor += 1;
                        }
                        match explicit_choice {
                            Some(ShardChoice::PayLife) => {
                                life_payments.push(LifePayment { amount: 2 });
                            }
                            Some(ShardChoice::PayMana) => {
                                let unit = if any_color {
                                    spend_any_for_required_colors(pool, &[color], spell, None)
                                } else {
                                    spend_eligible(pool, color, spell)
                                }
                                .ok_or(PaymentError::InsufficientMana)?;
                                spent.push(unit);
                            }
                            None => {
                                // CR 107.4f + CR 118.3: Auto-decide — prefer mana only
                                // when spending it won't starve the generic portion.
                                let can_spare = pool.total() > *generic as usize;
                                let mana_ok = if can_spare {
                                    if any_color {
                                        spend_any_for_required_colors(pool, &[color], spell, None)
                                    } else {
                                        spend_eligible(pool, color, spell)
                                    }
                                } else {
                                    None
                                };
                                if let Some(unit) = mana_ok {
                                    spent.push(unit);
                                } else {
                                    life_payments.push(LifePayment { amount: 2 });
                                }
                            }
                        }
                    }
                    // CR 107.4e: Monocolored hybrid {2/C} — pay 1 colored or 2 generic.
                    ShardRequirement::TwoGenericHybrid(color) => {
                        if any_color {
                            let unit = spend_any_for_required_colors(pool, &[color], spell, None)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        } else if let Some(unit) = spend_eligible(pool, color, spell) {
                            spent.push(unit);
                        } else {
                            for _ in 0..2 {
                                let unit = spend_generic_eligible(pool, spell, None)
                                    .ok_or(PaymentError::InsufficientMana)?;
                                spent.push(unit);
                            }
                        }
                    }
                    // CR 107.4h: Snow mana {S} — paid with mana from a snow source.
                    ShardRequirement::Snow => {
                        let unit = spend_snow_unit(pool).ok_or(PaymentError::InsufficientMana)?;
                        spent.push(unit);
                    }
                    ShardRequirement::TwoOrMoreColorSource => {
                        let unit = spend_two_or_more_color_source_eligible(pool, spell)
                            .ok_or(PaymentError::InsufficientMana)?;
                        spent.push(unit);
                    }
                    // CR 107.3: {X} defaults to 0; caller specifies X value separately.
                    ShardRequirement::X => {}
                    // CR 107.4e: Colorless hybrid {C/color} — prefer colorless, then colored.
                    ShardRequirement::ColorlessHybrid(color) => {
                        if any_color {
                            let unit = spend_any_for_required_colors(
                                pool,
                                &[ManaType::Colorless, color],
                                spell,
                                None,
                            )
                            .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        } else if let Some(unit) = spend_eligible(pool, ManaType::Colorless, spell)
                        {
                            spent.push(unit);
                        } else {
                            let unit = spend_eligible(pool, color, spell)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        }
                    }
                    // CR 107.4f: Hybrid Phyrexian — pay either color or 2 life.
                    ShardRequirement::HybridPhyrexian(a, b) => {
                        let explicit_choice = phyrexian_choices
                            .and_then(|choices| choices.get(choice_cursor).copied());
                        if explicit_choice.is_some() {
                            choice_cursor += 1;
                        }
                        match explicit_choice {
                            Some(ShardChoice::PayLife) => {
                                life_payments.push(LifePayment { amount: 2 });
                            }
                            Some(ShardChoice::PayMana) => {
                                let unit = if any_color {
                                    spend_any_for_required_colors(pool, &[a, b], spell, None)
                                } else {
                                    let remaining_pair_shards =
                                        count_remaining_hybrid_shards(shards, idx, a, b);
                                    let color = select_hybrid_payment_color(
                                        pool,
                                        a,
                                        b,
                                        hand_demand,
                                        remaining_pair_shards,
                                        &mut preferred_hybrid_colors,
                                    );
                                    spend_eligible(pool, color, spell)
                                }
                                .ok_or(PaymentError::InsufficientMana)?;
                                spent.push(unit);
                            }
                            None => {
                                // CR 107.4f + CR 118.3: Auto-decide — prefer mana only
                                // when spending it won't starve the generic portion.
                                let can_spare = pool.total() > *generic as usize;
                                let mana_ok = if can_spare {
                                    if any_color {
                                        spend_any_for_required_colors(pool, &[a, b], spell, None)
                                    } else {
                                        let remaining_pair_shards =
                                            count_remaining_hybrid_shards(shards, idx, a, b);
                                        let color = select_hybrid_payment_color(
                                            pool,
                                            a,
                                            b,
                                            hand_demand,
                                            remaining_pair_shards,
                                            &mut preferred_hybrid_colors,
                                        );
                                        spend_eligible(pool, color, spell)
                                    }
                                } else {
                                    None
                                };
                                if let Some(unit) = mana_ok {
                                    spent.push(unit);
                                } else {
                                    life_payments.push(LifePayment { amount: 2 });
                                }
                            }
                        }
                    }
                    // CR 107.4f + CR 107.4e: K'rrik-promoted {2/C} — pay 1
                    // colored mana, 2 generic mana, or 2 life. Honors explicit
                    // ShardChoice when supplied; otherwise auto-decides via the
                    // same generic-starvation heuristic as plain Phyrexian.
                    ShardRequirement::TwoGenericHybridPhyrexian(color) => {
                        let explicit_choice = phyrexian_choices
                            .and_then(|choices| choices.get(choice_cursor).copied());
                        if explicit_choice.is_some() {
                            choice_cursor += 1;
                        }
                        match explicit_choice {
                            Some(ShardChoice::PayLife) => {
                                life_payments.push(LifePayment { amount: 2 });
                            }
                            Some(ShardChoice::PayMana) => {
                                // Mirror auto-pay preference: 1 colored, then 2 generic.
                                if any_color {
                                    let unit =
                                        spend_any_for_required_colors(pool, &[color], spell, None)
                                            .ok_or(PaymentError::InsufficientMana)?;
                                    spent.push(unit);
                                } else if let Some(unit) = spend_eligible(pool, color, spell) {
                                    spent.push(unit);
                                } else {
                                    for _ in 0..2 {
                                        let unit = spend_generic_eligible(pool, spell, None)
                                            .ok_or(PaymentError::InsufficientMana)?;
                                        spent.push(unit);
                                    }
                                }
                            }
                            None => {
                                // CR 107.4f + CR 118.3: prefer mana only when
                                // spending it won't starve generic.
                                let can_spare = pool.total() > *generic as usize;
                                let mana_paid = if can_spare {
                                    if any_color {
                                        spend_any_for_required_colors(pool, &[color], spell, None)
                                            .map(|u| {
                                                spent.push(u);
                                                true
                                            })
                                            .unwrap_or(false)
                                    } else if let Some(u) = spend_eligible(pool, color, spell) {
                                        spent.push(u);
                                        true
                                    } else {
                                        // 2-generic fallback (atomic).
                                        let mut backup = pool.clone();
                                        let mut tmp_spent: Vec<ManaUnit> = Vec::new();
                                        let ok = (0..2).all(|_| {
                                            if let Some(u) =
                                                spend_generic_eligible(&mut backup, spell, None)
                                            {
                                                tmp_spent.push(u);
                                                true
                                            } else {
                                                false
                                            }
                                        });
                                        if ok {
                                            *pool = backup;
                                            spent.append(&mut tmp_spent);
                                            true
                                        } else {
                                            false
                                        }
                                    }
                                } else {
                                    false
                                };
                                if !mana_paid {
                                    life_payments.push(LifePayment { amount: 2 });
                                }
                            }
                        }
                    }
                }
            }

            // CR 107.4b: Generic mana can be paid with any type of mana.
            // Prefer colorless first, then a non-demanded color, then least-available
            // color to preserve flexibility. `hand_demand` (combined upstream with the
            // outer cost's reserved colors for nested sub-costs) softly deprioritizes
            // a color another cost still needs (CR 118.10) without ever hard-blocking
            // a payable spend (CR 601.2h: partial payments aren't allowed and an
            // unpayable cost can't be paid, so a payable one must never be blocked).
            // Note: this extends the demand signal — previously honored only by the
            // hybrid-color path — to the generic spend, so a normal cast now also
            // deprioritizes a hand-demanded color when filling generic. This only
            // reorders WHICH eligible unit pays a generic pip; it never refuses one.
            for _ in 0..*generic {
                let unit = spend_generic_eligible(pool, spell, hand_demand)
                    .ok_or(PaymentError::InsufficientMana)?;
                spent.push(unit);
            }

            Ok((spent, life_payments))
        }
    }
}

/// CR 107.4f + CR 601.2f: Compute the per-shard `ShardOptions` for each Phyrexian shard
/// in `cost`, given the caster's post-auto-tap pool, spell context, and life budget.
///
/// Returns `Vec<PhyrexianShard>` aligned with the order of Phyrexian shards in `cost`.
/// Each shard records the colored mana availability (`ManaOnly`, `LifeOnly`, or `ManaOrLife`)
/// so the UI can render only legal choices and the engine can decide whether to pause at
/// `WaitingFor::PhyrexianPayment` before life would be deducted.
///
/// The computation is a simulated dry-run: we spend mana from a cloned pool in order,
/// checking each Phyrexian shard's mana option against the pool state *after* previous
/// non-Phyrexian shards have consumed their mana. This matches the ordering used by
/// `pay_cost_with_demand_and_choices`.
pub fn compute_phyrexian_shards(
    pool: &ManaPool,
    cost: &ManaCost,
    spell: Option<&PaymentContext<'_>>,
    permissions: crate::types::mana::CostPermissionContext,
) -> Vec<crate::types::game_state::PhyrexianShard> {
    use crate::types::game_state::{PhyrexianShard, ShardOptions};

    let any_color = permissions.any_color;
    let max_life_payments = permissions.max_life;
    let life_colors = permissions.life_colors;
    let (shards, generic) = match cost {
        ManaCost::Cost { shards, generic } => (shards, *generic),
        _ => return Vec::new(),
    };

    let mut sim = pool.clone();
    let mut results = Vec::new();
    let mut preferred_hybrid_colors: Vec<(ManaType, ManaType, ManaType)> = Vec::new();
    // CR 107.4f + CR 118.3 + CR 119.8: Mana preference within the dry-run matches
    // `pay_cost_with_demand_and_choices`' auto-decision. `life_budget` tracks how many
    // life payments remain unspent — once exhausted, subsequent shards report `ManaOnly`
    // (or `LifeOnly`/unpayable would have failed `can_pay_for_spell` upstream).
    let mut life_budget = max_life_payments;

    // CR 107.4f + CR 118.3: Resolve non-Phyrexian shards first (consuming their
    // mana from `sim`), then the deferred Phyrexian shards — so each shard's
    // `ShardOptions` reflects the pool after every strict requirement is met
    // (see `phyrexian_deferred_order`). K'rrik promotion is applied per shard.
    for idx in phyrexian_deferred_order(shards, life_colors) {
        match effective_shard_requirement(shard_to_mana_type(shards[idx]), life_colors) {
            ShardRequirement::Single(mt) => {
                if any_color && mt != ManaType::Colorless {
                    let _ = spend_any_for_required_colors(&mut sim, &[mt], spell, None);
                } else {
                    let _ = spend_eligible(&mut sim, mt, spell);
                }
            }
            ShardRequirement::Hybrid(a, b) => {
                if any_color {
                    let _ = spend_any_for_required_colors(&mut sim, &[a, b], spell, None);
                } else {
                    let remaining_pair_shards = count_remaining_hybrid_shards(shards, idx, a, b);
                    let color = select_hybrid_payment_color(
                        &sim,
                        a,
                        b,
                        None,
                        remaining_pair_shards,
                        &mut preferred_hybrid_colors,
                    );
                    let _ = spend_eligible(&mut sim, color, spell);
                }
            }
            ShardRequirement::Phyrexian(color) => {
                let mana_available = sim_phyrexian_mana_available(&sim, spell, any_color, color);
                // CR 107.4f + CR 118.3: Only offer mana when spending it
                // wouldn't starve the generic portion of the cost.
                let can_spare = sim.total() > generic as usize;
                let effective_mana = mana_available && can_spare;
                let life_available = life_budget > 0;
                let options = match (effective_mana, life_available) {
                    (true, true) => ShardOptions::ManaOrLife,
                    (true, false) => ShardOptions::ManaOnly,
                    (false, true) => ShardOptions::LifeOnly,
                    // Unpayable: this should be gated by `can_pay_for_spell` upstream.
                    // If we reach here, treat as ManaOnly — payment will error, surfaced
                    // to the caller as ActionNotAllowed.
                    (false, false) => ShardOptions::ManaOnly,
                };
                results.push(PhyrexianShard {
                    shard_index: idx,
                    color: mana_type_to_color_fallback(color),
                    options,
                });
                // Simulated commit: prefer mana path for later shard availability;
                // if mana is unavailable or would starve generic, consume life budget.
                if effective_mana {
                    let _ = if any_color {
                        spend_any_for_required_colors(&mut sim, &[color], spell, None)
                    } else {
                        spend_eligible(&mut sim, color, spell)
                    };
                } else {
                    life_budget = life_budget.saturating_sub(1);
                }
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                if any_color {
                    let _ = spend_any_for_required_colors(&mut sim, &[color], spell, None);
                } else if spend_eligible(&mut sim, color, spell).is_none() {
                    for _ in 0..2 {
                        let _ = spend_generic_eligible(&mut sim, spell, None);
                    }
                }
            }
            ShardRequirement::Snow => {
                let _ = spend_snow_unit(&mut sim);
            }
            ShardRequirement::TwoOrMoreColorSource => {
                let _ = spend_two_or_more_color_source_eligible(&mut sim, spell);
            }
            ShardRequirement::X => {}
            ShardRequirement::ColorlessHybrid(color) => {
                if any_color {
                    let _ = spend_any_for_required_colors(
                        &mut sim,
                        &[ManaType::Colorless, color],
                        spell,
                        None,
                    );
                } else if spend_eligible(&mut sim, ManaType::Colorless, spell).is_none() {
                    let _ = spend_eligible(&mut sim, color, spell);
                }
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                let mana_available = if any_color {
                    sim_any_for_required_colors_available(&sim, spell, &[a, b])
                } else {
                    sim_color_available(&sim, spell, a) || sim_color_available(&sim, spell, b)
                };
                // CR 107.4f + CR 118.3: Only offer mana when spending it
                // wouldn't starve the generic portion of the cost.
                let can_spare = sim.total() > generic as usize;
                let effective_mana = mana_available && can_spare;
                let life_available = life_budget > 0;
                let options = match (effective_mana, life_available) {
                    (true, true) => ShardOptions::ManaOrLife,
                    (true, false) => ShardOptions::ManaOnly,
                    (false, true) => ShardOptions::LifeOnly,
                    (false, false) => ShardOptions::ManaOnly,
                };
                // CR 107.4f: The printed hybrid-Phyrexian shard shows two colors; surface the
                // first component in `PhyrexianShard.color` for UI display. The payment path
                // chooses the actual spend color via `auto_pay_hybrid`.
                results.push(PhyrexianShard {
                    shard_index: idx,
                    color: mana_type_to_color_fallback(a),
                    options,
                });
                if effective_mana {
                    let _ = if any_color {
                        spend_any_for_required_colors(&mut sim, &[a, b], spell, None)
                    } else {
                        let remaining_pair_shards =
                            count_remaining_hybrid_shards(shards, idx, a, b);
                        let color = select_hybrid_payment_color(
                            &sim,
                            a,
                            b,
                            None,
                            remaining_pair_shards,
                            &mut preferred_hybrid_colors,
                        );
                        spend_eligible(&mut sim, color, spell)
                    };
                } else {
                    life_budget = life_budget.saturating_sub(1);
                }
            }
            // CR 107.4f + CR 107.4e: K'rrik-promoted {2/C} — payable as 1
            // colored mana, 2 generic mana, OR 2 life. Surface the
            // mana-vs-life choice when both are viable.
            ShardRequirement::TwoGenericHybridPhyrexian(color) => {
                // Mana is "available" if either the 1-colored or 2-generic
                // route is satisfiable from the simulated pool.
                let mana_available = {
                    let colored_ok = if any_color {
                        sim_any_for_required_colors_available(&sim, spell, &[color])
                    } else {
                        sim_color_available(&sim, spell, color)
                    };
                    if colored_ok {
                        true
                    } else {
                        let mut probe = sim.clone();
                        spend_generic_eligible(&mut probe, spell, None).is_some()
                            && spend_generic_eligible(&mut probe, spell, None).is_some()
                    }
                };
                // CR 107.4f + CR 118.3: Only offer mana when spending it
                // wouldn't starve the generic portion of the cost.
                let can_spare = sim.total() > generic as usize;
                let effective_mana = mana_available && can_spare;
                let life_available = life_budget > 0;
                let options = match (effective_mana, life_available) {
                    (true, true) => ShardOptions::ManaOrLife,
                    (true, false) => ShardOptions::ManaOnly,
                    (false, true) => ShardOptions::LifeOnly,
                    (false, false) => ShardOptions::ManaOnly,
                };
                results.push(PhyrexianShard {
                    shard_index: idx,
                    color: mana_type_to_color_fallback(color),
                    options,
                });
                if effective_mana {
                    // Mirror `pay_cost_with_demand_and_choices`'s preference:
                    // prefer 1 colored, then atomic 2-generic fallback.
                    if any_color {
                        let _ = spend_any_for_required_colors(&mut sim, &[color], spell, None);
                    } else if spend_eligible(&mut sim, color, spell).is_none() {
                        let mut backup = sim.clone();
                        if spend_generic_eligible(&mut backup, spell, None).is_some()
                            && spend_generic_eligible(&mut backup, spell, None).is_some()
                        {
                            sim = backup;
                        }
                    }
                } else {
                    life_budget = life_budget.saturating_sub(1);
                }
            }
        }
    }

    results
}

fn sim_phyrexian_mana_available(
    pool: &ManaPool,
    spell: Option<&PaymentContext<'_>>,
    any_color: bool,
    color: ManaType,
) -> bool {
    if any_color {
        sim_any_for_required_colors_available(pool, spell, &[color])
    } else {
        sim_color_available(pool, spell, color)
    }
}

fn sim_any_for_required_colors_available(
    pool: &ManaPool,
    spell: Option<&PaymentContext<'_>>,
    required_colors: &[ManaType],
) -> bool {
    let mut clone = pool.clone();
    spend_any_for_required_colors(&mut clone, required_colors, spell, None).is_some()
}

fn sim_color_available(
    pool: &ManaPool,
    spell: Option<&PaymentContext<'_>>,
    color: ManaType,
) -> bool {
    let mut clone = pool.clone();
    spend_eligible(&mut clone, color, spell).is_some()
}

/// CR 107.4a: Phyrexian shards always reference one of the five colors; `Colorless`
/// cannot appear in a `Phyrexian` shard requirement. Default to `White` if we somehow
/// encounter a colorless mapping (defensive fallback; unreachable via `shard_to_mana_type`).
fn mana_type_to_color_fallback(mt: ManaType) -> crate::types::mana::ManaColor {
    use crate::types::mana::ManaColor;
    match mt {
        ManaType::White => ManaColor::White,
        ManaType::Blue => ManaColor::Blue,
        ManaType::Black => ManaColor::Black,
        ManaType::Red => ManaColor::Red,
        ManaType::Green => ManaColor::Green,
        ManaType::Colorless => ManaColor::White,
    }
}

fn canonical_hybrid_key(a: ManaType, b: ManaType) -> (ManaType, ManaType) {
    let a_idx = mana_type_to_demand_index(a).unwrap_or(0);
    let b_idx = mana_type_to_demand_index(b).unwrap_or(0);
    if a_idx <= b_idx {
        (a, b)
    } else {
        (b, a)
    }
}

fn select_hybrid_payment_color(
    pool: &ManaPool,
    a: ManaType,
    b: ManaType,
    hand_demand: Option<&ColorDemand>,
    remaining_hybrid_shards: usize,
    preferred_hybrid_colors: &mut Vec<(ManaType, ManaType, ManaType)>,
) -> ManaType {
    let key = canonical_hybrid_key(a, b);

    for (_, _, color) in preferred_hybrid_colors
        .iter()
        .filter(|(first, second, _)| *first == key.0 && *second == key.1)
    {
        if pool.count_color(*color) > 0 {
            return *color;
        }
    }

    let color = auto_pay_hybrid(pool, a, b, hand_demand, remaining_hybrid_shards);
    if let Some(entry) = preferred_hybrid_colors
        .iter_mut()
        .find(|(first, second, _)| *first == key.0 && *second == key.1)
    {
        *entry = (key.0, key.1, color);
    } else {
        preferred_hybrid_colors.push((key.0, key.1, color));
    }
    color
}

/// For a hybrid shard like W/U, returns the best color to spend.
/// When hand demand is available, spends the color *least needed* by other cards in hand.
/// Falls back to spending whichever color has more in the pool (preserving the scarcer color).
/// If one color can satisfy the remaining identical hybrid shards, use it so repeated
/// hybrid requirements stay on the same color when possible.
fn auto_pay_hybrid(
    pool: &ManaPool,
    a: ManaType,
    b: ManaType,
    hand_demand: Option<&ColorDemand>,
    remaining_hybrid_shards: usize,
) -> ManaType {
    // Only consider colors actually available in pool
    let count_a = pool.count_color(a);
    let count_b = pool.count_color(b);

    if count_a == 0 {
        return b;
    }
    if count_b == 0 {
        return a;
    }

    // If hand demand info is available, spend the less-demanded color
    if let Some(demand) = hand_demand {
        let demand_a = mana_type_to_demand_index(a).map(|i| demand[i]).unwrap_or(0);
        let demand_b = mana_type_to_demand_index(b).map(|i| demand[i]).unwrap_or(0);
        if demand_a != demand_b {
            // Spend the color the hand needs LESS
            return if demand_a < demand_b { a } else { b };
        }
    }

    // If both colors can satisfy all remaining identical hybrid shards, keep shard
    // order preference on ties.
    if count_a >= remaining_hybrid_shards && count_b >= remaining_hybrid_shards {
        return if count_a >= count_b { a } else { b };
    }

    // Prefer a color that can still pay this specific hybrid chain.
    if count_a >= remaining_hybrid_shards {
        return a;
    }

    if count_b >= remaining_hybrid_shards {
        return b;
    }

    // Tiebreaker: spend whichever we have more of (preserve the scarcer color)
    if count_a >= count_b {
        a
    } else {
        b
    }
}

fn count_remaining_hybrid_shards(
    shards: &[ManaCostShard],
    start: usize,
    a: ManaType,
    b: ManaType,
) -> usize {
    if start >= shards.len() {
        return 0;
    }

    let mut total = 0;
    for shard in &shards[start..] {
        match shard_to_mana_type(*shard) {
            ShardRequirement::Hybrid(x, y) | ShardRequirement::HybridPhyrexian(x, y)
                if (x == a && y == b) || (x == b && y == a) =>
            {
                total += 1;
            }
            _ => {}
        }
    }
    total
}

/// Determine mana type for a basic land subtype (CR 305.6).
///
/// CR 305.6: The basic land types are Plains, Island, Swamp, Mountain, and Forest.
/// A land with a basic land type has the intrinsic ability "{T}: Add [mana]" — Plains
/// adds {W}, Islands {U}, Swamps {B}, Mountains {R}, Forests {G}.
pub fn land_subtype_to_mana_type(subtype: &str) -> Option<ManaType> {
    match subtype {
        "Plains" => Some(ManaType::White),
        "Island" => Some(ManaType::Blue),
        "Swamp" => Some(ManaType::Black),
        "Mountain" => Some(ManaType::Red),
        "Forest" => Some(ManaType::Green),
        _ => None,
    }
}

/// Spend one mana of the given color, respecting restrictions if a spell context is provided.
///
/// CR 106.6: Restricted mana can only be spent on spells/abilities that match the restriction.
/// Prefers non-`{Z}`-eligible mana for ordinary colored/colorless requirements
/// so later source-quality-constrained shards are not starved.
fn spend_eligible(
    pool: &mut ManaPool,
    color: ManaType,
    spell: Option<&PaymentContext<'_>>,
) -> Option<ManaUnit> {
    match spell {
        Some(ctx) => spend_color_prefer_non_z(pool, color, |unit| {
            if color == ManaType::Colorless && unit.is_convoke_payment() {
                return false;
            }
            unit.restrictions
                .iter()
                .all(|restriction| restriction.allows(ctx))
        }),
        None => spend_color_prefer_non_z(pool, color, |unit| {
            !(color == ManaType::Colorless && unit.is_convoke_payment())
        }),
    }
}

fn spend_color_prefer_non_z(
    pool: &mut ManaPool,
    color: ManaType,
    allows: impl Fn(&ManaUnit) -> bool,
) -> Option<ManaUnit> {
    if let Some(pos) = pool.mana.iter().position(|unit| {
        unit.color == color && !unit.source_could_produce_two_or_more_colors && allows(unit)
    }) {
        return Some(pool.mana.swap_remove(pos));
    }
    pool.mana
        .iter()
        .position(|unit| unit.color == color && allows(unit))
        .map(|pos| pool.mana.swap_remove(pos))
}

// --- Internal helpers ---

/// Decomposed mana cost shard into its payment requirement (CR 107.4).
///
/// Maps each `ManaCostShard` to the type of payment it requires, per
/// CR 107.4a (colored), CR 107.4b (generic/X), CR 107.4c (colorless),
/// CR 107.4e (hybrid), CR 107.4f (Phyrexian), CR 107.4h (snow).
pub(crate) enum ShardRequirement {
    Single(ManaType),
    Hybrid(ManaType, ManaType),
    Phyrexian(ManaType),
    TwoGenericHybrid(ManaType),
    Snow,
    TwoOrMoreColorSource,
    X,
    ColorlessHybrid(ManaType),
    HybridPhyrexian(ManaType, ManaType),
    /// CR 107.4f: Synthetic fusion of `TwoGenericHybrid({color})` and the
    /// K'rrik-style life-for-color grant — represents a `{2/C}` shard whose
    /// `C` color is in the paying player's `LifePaymentColors`. Payment may
    /// be: 1 mana of the indicated color, 2 generic mana, OR 2 life.
    ///
    /// **Synthesis-only.** This variant is produced exclusively by
    /// `effective_shard_requirement` when promoting a `TwoGenericHybrid` shard
    /// under an active grant. `shard_to_mana_type` does NOT synthesize it
    /// because no printed mana cost symbol corresponds directly to `{2/B/P}`.
    TwoGenericHybridPhyrexian(ManaType),
}

/// CR 107.4f: Promote a `ShardRequirement` to the Phyrexian-shape fusion if
/// the paying player has a `PayLifeAsColoredMana` grant for the shard's color.
/// Returns the input unchanged when no grant applies (or when the shard has
/// no color axis to promote: `Snow`, `X`, `TwoOrMoreColorSource`, etc.).
///
/// Promotion table:
/// - `Single(c)` + grant(c) → `Phyrexian(c)`
/// - `Hybrid(c1, c2)` + grant(c1 or c2) → `HybridPhyrexian(c1, c2)`
/// - `TwoGenericHybrid(c)` + grant(c) → `TwoGenericHybridPhyrexian(c)`
/// - `Phyrexian(_)` / `HybridPhyrexian(_, _)` → unchanged (life already available)
/// - `ColorlessHybrid(_)` / `Snow` / `X` / `TwoOrMoreColorSource` → unchanged
pub(crate) fn effective_shard_requirement(
    req: ShardRequirement,
    life_colors: crate::types::mana::LifePaymentColors,
) -> ShardRequirement {
    if life_colors.is_empty() {
        return req;
    }
    match req {
        ShardRequirement::Single(mt) => {
            if let Some(color) = mana_type_to_color(mt) {
                if life_colors.contains(color) {
                    return ShardRequirement::Phyrexian(mt);
                }
            }
            req
        }
        ShardRequirement::Hybrid(a, b) => {
            let a_in = mana_type_to_color(a).is_some_and(|c| life_colors.contains(c));
            let b_in = mana_type_to_color(b).is_some_and(|c| life_colors.contains(c));
            if a_in || b_in {
                ShardRequirement::HybridPhyrexian(a, b)
            } else {
                req
            }
        }
        ShardRequirement::TwoGenericHybrid(mt) => {
            if let Some(color) = mana_type_to_color(mt) {
                if life_colors.contains(color) {
                    return ShardRequirement::TwoGenericHybridPhyrexian(mt);
                }
            }
            req
        }
        // Already Phyrexian-shape; nothing to promote.
        ShardRequirement::Phyrexian(_)
        | ShardRequirement::HybridPhyrexian(_, _)
        | ShardRequirement::ColorlessHybrid(_)
        | ShardRequirement::Snow
        | ShardRequirement::X
        | ShardRequirement::TwoOrMoreColorSource
        | ShardRequirement::TwoGenericHybridPhyrexian(_) => req,
    }
}

/// Inverse of `From<ManaColor> for ManaType` — `Colorless` has no `ManaColor`
/// counterpart so this returns `None` for it.
fn mana_type_to_color(mt: ManaType) -> Option<crate::types::mana::ManaColor> {
    use crate::types::mana::ManaColor;
    match mt {
        ManaType::White => Some(ManaColor::White),
        ManaType::Blue => Some(ManaColor::Blue),
        ManaType::Black => Some(ManaColor::Black),
        ManaType::Red => Some(ManaColor::Red),
        ManaType::Green => Some(ManaColor::Green),
        ManaType::Colorless => None,
    }
}

/// Map a `ManaCostShard` to its payment requirement (CR 107.4).
pub(crate) fn shard_to_mana_type(shard: ManaCostShard) -> ShardRequirement {
    match shard {
        ManaCostShard::White => ShardRequirement::Single(ManaType::White),
        ManaCostShard::Blue => ShardRequirement::Single(ManaType::Blue),
        ManaCostShard::Black => ShardRequirement::Single(ManaType::Black),
        ManaCostShard::Red => ShardRequirement::Single(ManaType::Red),
        ManaCostShard::Green => ShardRequirement::Single(ManaType::Green),
        ManaCostShard::Colorless => ShardRequirement::Single(ManaType::Colorless),
        ManaCostShard::Snow => ShardRequirement::Snow,
        ManaCostShard::TwoOrMoreColorSource => ShardRequirement::TwoOrMoreColorSource,
        ManaCostShard::X => ShardRequirement::X,
        ManaCostShard::WhiteBlue => ShardRequirement::Hybrid(ManaType::White, ManaType::Blue),
        ManaCostShard::WhiteBlack => ShardRequirement::Hybrid(ManaType::White, ManaType::Black),
        ManaCostShard::BlueBlack => ShardRequirement::Hybrid(ManaType::Blue, ManaType::Black),
        ManaCostShard::BlueRed => ShardRequirement::Hybrid(ManaType::Blue, ManaType::Red),
        ManaCostShard::BlackRed => ShardRequirement::Hybrid(ManaType::Black, ManaType::Red),
        ManaCostShard::BlackGreen => ShardRequirement::Hybrid(ManaType::Black, ManaType::Green),
        ManaCostShard::RedWhite => ShardRequirement::Hybrid(ManaType::Red, ManaType::White),
        ManaCostShard::RedGreen => ShardRequirement::Hybrid(ManaType::Red, ManaType::Green),
        ManaCostShard::GreenWhite => ShardRequirement::Hybrid(ManaType::Green, ManaType::White),
        ManaCostShard::GreenBlue => ShardRequirement::Hybrid(ManaType::Green, ManaType::Blue),
        ManaCostShard::TwoWhite => ShardRequirement::TwoGenericHybrid(ManaType::White),
        ManaCostShard::TwoBlue => ShardRequirement::TwoGenericHybrid(ManaType::Blue),
        ManaCostShard::TwoBlack => ShardRequirement::TwoGenericHybrid(ManaType::Black),
        ManaCostShard::TwoRed => ShardRequirement::TwoGenericHybrid(ManaType::Red),
        ManaCostShard::TwoGreen => ShardRequirement::TwoGenericHybrid(ManaType::Green),
        ManaCostShard::PhyrexianWhite => ShardRequirement::Phyrexian(ManaType::White),
        ManaCostShard::PhyrexianBlue => ShardRequirement::Phyrexian(ManaType::Blue),
        ManaCostShard::PhyrexianBlack => ShardRequirement::Phyrexian(ManaType::Black),
        ManaCostShard::PhyrexianRed => ShardRequirement::Phyrexian(ManaType::Red),
        ManaCostShard::PhyrexianGreen => ShardRequirement::Phyrexian(ManaType::Green),
        ManaCostShard::PhyrexianWhiteBlue => {
            ShardRequirement::HybridPhyrexian(ManaType::White, ManaType::Blue)
        }
        ManaCostShard::PhyrexianWhiteBlack => {
            ShardRequirement::HybridPhyrexian(ManaType::White, ManaType::Black)
        }
        ManaCostShard::PhyrexianBlueBlack => {
            ShardRequirement::HybridPhyrexian(ManaType::Blue, ManaType::Black)
        }
        ManaCostShard::PhyrexianBlueRed => {
            ShardRequirement::HybridPhyrexian(ManaType::Blue, ManaType::Red)
        }
        ManaCostShard::PhyrexianBlackRed => {
            ShardRequirement::HybridPhyrexian(ManaType::Black, ManaType::Red)
        }
        ManaCostShard::PhyrexianBlackGreen => {
            ShardRequirement::HybridPhyrexian(ManaType::Black, ManaType::Green)
        }
        ManaCostShard::PhyrexianRedWhite => {
            ShardRequirement::HybridPhyrexian(ManaType::Red, ManaType::White)
        }
        ManaCostShard::PhyrexianRedGreen => {
            ShardRequirement::HybridPhyrexian(ManaType::Red, ManaType::Green)
        }
        ManaCostShard::PhyrexianGreenWhite => {
            ShardRequirement::HybridPhyrexian(ManaType::Green, ManaType::White)
        }
        ManaCostShard::PhyrexianGreenBlue => {
            ShardRequirement::HybridPhyrexian(ManaType::Green, ManaType::Blue)
        }
        ManaCostShard::ColorlessWhite => ShardRequirement::ColorlessHybrid(ManaType::White),
        ManaCostShard::ColorlessBlue => ShardRequirement::ColorlessHybrid(ManaType::Blue),
        ManaCostShard::ColorlessBlack => ShardRequirement::ColorlessHybrid(ManaType::Black),
        ManaCostShard::ColorlessRed => ShardRequirement::ColorlessHybrid(ManaType::Red),
        ManaCostShard::ColorlessGreen => ShardRequirement::ColorlessHybrid(ManaType::Green),
    }
}

/// Count the units of `color` in `pool` that are eligible to pay a generic pip
/// under the spell `spell` context: never a convoke-payment stand-in, and (when
/// a context is supplied) every restriction must allow it. This is the LIVE
/// eligible count — recomputed per call as the pool shrinks across generic pips,
/// never snapshotted — so a multiplicity-aware "would dip into reserve" check
/// (`count <= demand[i]`) reflects the units actually still spendable.
///
/// Used by both `spend_any_eligible` (real spend) and `spend_generic_non_demanded`
/// (planner dry-run) so the two rank colors identically (CR 118.10: a unit
/// reserved for the outer cost's colored shard can't also fund a sub-cost pip).
fn eligible_color_count(
    pool: &ManaPool,
    color: ManaType,
    spell: Option<&PaymentContext<'_>>,
) -> usize {
    pool.mana
        .iter()
        .filter(|m| {
            m.color == color
                && !m.is_convoke_payment()
                && spell.is_none_or(|ctx| m.restrictions.iter().all(|r| r.allows(ctx)))
        })
        .count()
}

fn spend_any_eligible(
    pool: &mut ManaPool,
    spell: Option<&PaymentContext<'_>>,
    demand: Option<&ColorDemand>,
) -> Option<ManaUnit> {
    match spell {
        Some(ctx) => {
            if let Some(unit) = spend_eligible(pool, ManaType::Colorless, Some(ctx)) {
                return Some(unit);
            }

            let colors = [
                ManaType::White,
                ManaType::Blue,
                ManaType::Black,
                ManaType::Red,
                ManaType::Green,
            ];
            // CR 601.2h + CR 118.10: When a `demand` is supplied, deprioritize
            // colors whose every available unit the outer cost / hand still needs
            // — but only SOFTLY. The check is multiplicity-aware: spending one unit
            // of color `i` would dip into the outer cost's reserve iff the live
            // eligible count is no greater than the demanded count
            // (`count <= demand[i]`); a surplus unit (`count > demand[i]`) is free
            // to spend (CR 118.10: a reserved unit can't also fund this pip, but a
            // surplus one isn't reserved). Colorless / unmapped colors are never
            // reserved. Sort key is `(would_dip_into_reserve, count)`: surplus-safe
            // colors sort first, then least-available within each tier. When EVERY
            // eligible color would dip (no surplus anywhere) all share `(true, count)`
            // and `best` still selects the least-available demanded unit — never
            // returns `None` while payable mana exists (CR 601.2h forbids leaving a
            // payable cost unpaid). `demand == None` => predicate false for all =>
            // byte-identical to the pre-demand least-available ordering.
            let mut best: Option<(ManaType, bool, usize)> = None;
            for &color in &colors {
                let count = eligible_color_count(pool, color, Some(ctx));
                if count > 0 {
                    let would_dip_into_reserve = demand
                        .and_then(|d| {
                            mana_type_to_demand_index(color).map(|i| count <= d[i] as usize)
                        })
                        .unwrap_or(false);
                    let better = match best {
                        None => true,
                        Some((_, best_dip, best_count)) => {
                            (would_dip_into_reserve, count) < (best_dip, best_count)
                        }
                    };
                    if better {
                        best = Some((color, would_dip_into_reserve, count));
                    }
                }
            }
            best.and_then(|(color, _, _)| {
                spend_color_prefer_non_z(pool, color, |unit| {
                    !unit.is_convoke_payment()
                        && unit
                            .restrictions
                            .iter()
                            .all(|restriction| restriction.allows(ctx))
                })
            })
        }
        None => spend_any_unit(pool),
    }
}

fn spend_any_for_required_colors(
    pool: &mut ManaPool,
    required_colors: &[ManaType],
    spell: Option<&PaymentContext<'_>>,
    demand: Option<&ColorDemand>,
) -> Option<ManaUnit> {
    for color in required_colors {
        if let Some(unit) = spend_eligible(pool, *color, spell) {
            return Some(unit);
        }
    }

    spend_any_eligible(pool, spell, demand)
}

/// Planner-layer generic spend that respects an outer cost's colored `demand`.
///
/// CR 107.4b + CR 118.10: A generic pip can be paid with any mana, but when an
/// outer cost (still being paid on the call stack) demands specific colors, a
/// nested sub-cost must not consume those colored units — each cost payment
/// applies to only one ability, so a unit reserved for the outer colored shard
/// can't also fund the sub-cost's generic pip. With `demand == Some`, this spends
/// only a SPENDABLE eligible unit: colorless / convoke, an undemanded color
/// (`demand[i] == 0`), or a color held in SURPLUS — its live eligible count
/// exceeds the demanded count (`count > demand[i]`), so consuming one still leaves
/// the outer cost whole. If only reserved (demanded, non-surplus) units remain it
/// returns `None` and the pip is left in the residual so auto-tap taps a different
/// source. The count is multiplicity-aware and shares `eligible_color_count` with
/// the real-spend twin `spend_any_eligible`, so the planner dry-run and real spend
/// rank colors identically. With `demand == None` it is byte-identical to
/// `spend_generic_eligible` — it never falls through to the least-available
/// ordering when reserved units are all that is left (WATCH-ITEM #1: the planner
/// must leave the residual, not coincidentally pay from a reserved unit).
fn spend_generic_non_demanded(
    pool: &mut ManaPool,
    spell: Option<&PaymentContext<'_>>,
    demand: Option<&ColorDemand>,
) -> Option<ManaUnit> {
    let Some(demand) = demand else {
        return spend_generic_eligible(pool, spell, None);
    };

    // Convoke payment units are creature-tap stand-ins, not floated colored mana;
    // they are never reserved for an outer colored shard, so prefer them first
    // (mirrors `spend_generic_eligible`'s convoke-first ordering).
    let convoke_pos = pool.mana.iter().position(|unit| {
        unit.is_convoke_payment()
            && spell.is_none_or(|ctx| unit.restrictions.iter().all(|r| r.allows(ctx)))
    });
    if let Some(pos) = convoke_pos {
        return Some(pool.mana.swap_remove(pos));
    }

    // Among non-convoke units eligible under the spell context, pick one whose
    // color is SPENDABLE without dipping into the outer cost's colored reserve:
    // colorless (never demanded), an undemanded color (`demand[i] == 0`), or a
    // color held in SURPLUS — its live eligible count exceeds the demanded count
    // (`count > demand[i]`), so spending one still leaves enough for the outer
    // cost (CR 118.10). The count is multiplicity-aware and computed LIVE per
    // invocation (the pool shrinks as generic pips are spent), and is independent
    // of the two-tier `Z`-source preference below — it gates spendability, not
    // tier. Prefer non-`Z` units within the spendable set (mirrors
    // `spend_color_prefer_non_z`); if only demanded units remain, return `None`
    // and the pip stays in the residual so auto-tap taps a different source.
    let is_spendable = |unit: &ManaUnit| -> bool {
        if unit.is_convoke_payment() {
            return false;
        }
        if let Some(ctx) = spell {
            if !unit.restrictions.iter().all(|r| r.allows(ctx)) {
                return false;
            }
        }
        match mana_type_to_demand_index(unit.color) {
            // Colorless: index `None`, never demanded.
            None => true,
            Some(i) => {
                demand[i] == 0 || eligible_color_count(pool, unit.color, spell) > demand[i] as usize
            }
        }
    };

    if let Some(pos) = pool
        .mana
        .iter()
        .position(|unit| !unit.source_could_produce_two_or_more_colors && is_spendable(unit))
    {
        return Some(pool.mana.swap_remove(pos));
    }
    pool.mana
        .iter()
        .position(is_spendable)
        .map(|pos| pool.mana.swap_remove(pos))
}

fn spend_generic_eligible(
    pool: &mut ManaPool,
    spell: Option<&PaymentContext<'_>>,
    demand: Option<&ColorDemand>,
) -> Option<ManaUnit> {
    if let Some(ctx) = spell {
        if let Some(pos) = pool.mana.iter().position(|unit| {
            unit.is_convoke_payment()
                && unit
                    .restrictions
                    .iter()
                    .all(|restriction| restriction.allows(ctx))
        }) {
            return Some(pool.mana.swap_remove(pos));
        }
    } else if let Some(pos) = pool.mana.iter().position(|unit| unit.is_convoke_payment()) {
        return Some(pool.mana.swap_remove(pos));
    }

    // CR 601.2h + CR 118.10: Forward the soft color demand so the generic pip is
    // paid from a non-demanded color when one is eligible, still spending a
    // demanded color when it is the only payable mana (CR 601.2h: a payable cost
    // can't be left unpaid — never false-unpayable).
    spend_any_eligible(pool, spell, demand)
}

fn spend_any_unit(pool: &mut ManaPool) -> Option<ManaUnit> {
    if pool.mana.is_empty() {
        return None;
    }

    // Prefer colorless first, then least-available color
    if let Some(unit) = spend_eligible(pool, ManaType::Colorless, None) {
        return Some(unit);
    }

    // Find the color with least available mana and spend it
    let colors = [
        ManaType::White,
        ManaType::Blue,
        ManaType::Black,
        ManaType::Red,
        ManaType::Green,
    ];

    let mut best: Option<(ManaType, usize)> = None;
    for &color in &colors {
        let count = pool
            .mana
            .iter()
            .filter(|unit| unit.color == color && !unit.is_convoke_payment())
            .count();
        if count > 0 {
            match best {
                None => best = Some((color, count)),
                Some((_, best_count)) if count < best_count => best = Some((color, count)),
                _ => {}
            }
        }
    }

    best.and_then(|(color, _)| {
        spend_color_prefer_non_z(pool, color, |unit| !unit.is_convoke_payment())
    })
}

fn spend_snow(pool: &mut ManaPool) -> bool {
    spend_snow_unit(pool).is_some()
}

/// CR 107.4h: Snow mana {S} — paid with one mana of any type from a snow source.
fn spend_snow_unit(pool: &mut ManaPool) -> Option<ManaUnit> {
    if let Some(pos) = pool.mana.iter().position(|m| m.is_snow()) {
        Some(pool.mana.swap_remove(pos))
    } else {
        None
    }
}

fn spend_two_or_more_color_source_eligible(
    pool: &mut ManaPool,
    spell: Option<&PaymentContext<'_>>,
) -> Option<ManaUnit> {
    let position = match spell {
        Some(ctx) => pool.mana.iter().position(|unit| {
            unit.source_could_produce_two_or_more_colors
                && unit
                    .restrictions
                    .iter()
                    .all(|restriction| restriction.allows(ctx))
        }),
        None => pool
            .mana
            .iter()
            .position(|unit| unit.source_could_produce_two_or_more_colors),
    }?;
    Some(pool.mana.swap_remove(position))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{
        Comparator, ContinuousModification, Duration, QuantityExpr, QuantityRef, StaticCondition,
        StaticDefinition, TargetFilter,
    };
    use crate::types::game_state::LayersDirty;
    use crate::types::identifiers::CardId;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaRestriction, SpellMeta};
    use crate::types::zones::Zone;

    /// The building-block predicate must classify each shape the parser can produce.
    /// Generic + colored + snow + free `X` (pre-concretization sentinel) are all
    /// resolvable by `pay_mana_cost` without player input; hybrid and Phyrexian
    /// require a rules-level choice per CR 107.4e / 107.4f.
    #[test]
    fn classify_payment_recognizes_each_shard_class() {
        let unambiguous = |shards: Vec<ManaCostShard>| ManaCost::Cost { shards, generic: 0 };

        assert_eq!(
            classify_payment(&ManaCost::NoCost),
            PaymentClassification::Unambiguous
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![
                ManaCostShard::Red,
                ManaCostShard::Red,
                ManaCostShard::Colorless,
            ])),
            PaymentClassification::Unambiguous,
            "pure single-color + colorless is always auto-payable"
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::Snow, ManaCostShard::Blue])),
            PaymentClassification::Unambiguous,
            "snow + single color is auto-payable (pay_mana_cost picks deterministically)"
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::TwoOrMoreColorSource])),
            PaymentClassification::Unambiguous,
            "{{Z}} is source-quality constrained but does not require a player choice"
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::WhiteBlue])),
            PaymentClassification::NeedsHybridChoice,
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::TwoGreen])),
            PaymentClassification::NeedsHybridChoice,
            "{{2/G}} is a hybrid choice: pay 2 generic or 1 green"
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::ColorlessRed])),
            PaymentClassification::NeedsHybridChoice,
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::PhyrexianBlack])),
            PaymentClassification::NeedsPhyrexianChoice,
        );
        assert_eq!(
            classify_payment(&unambiguous(vec![ManaCostShard::PhyrexianWhiteBlue])),
            PaymentClassification::NeedsPhyrexianChoice,
            "hybrid-phyrexian requires a choice (reported as phyrexian since life is an option)"
        );
        // First ambiguity wins — we report phyrexian before hybrid if both appear
        // after a phyrexian shard, which is fine for the auto-pay gate (both paths
        // require input; the variant is informational for future UI improvements).
        assert_eq!(
            classify_payment(&unambiguous(vec![
                ManaCostShard::Red,
                ManaCostShard::WhiteBlue,
                ManaCostShard::PhyrexianBlack,
            ])),
            PaymentClassification::NeedsHybridChoice,
            "scans in order — hybrid is found first"
        );
    }

    fn make_unit(color: ManaType) -> ManaUnit {
        ManaUnit {
            color,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        }
    }

    fn pool_with(units: &[(ManaType, usize)]) -> ManaPool {
        let mut pool = ManaPool::default();
        for (color, count) in units {
            for _ in 0..*count {
                pool.add(make_unit(*color));
            }
        }
        pool
    }

    fn make_two_or_more_color_source_unit(color: ManaType) -> ManaUnit {
        ManaUnit {
            source_could_produce_two_or_more_colors: true,
            ..make_unit(color)
        }
    }

    #[test]
    fn pay_cost_accepts_z_from_eligible_source() {
        let mut pool = ManaPool::default();
        pool.add(make_two_or_more_color_source_unit(ManaType::Green));
        pool.add(make_unit(ManaType::Colorless));
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::TwoOrMoreColorSource],
            generic: 1,
        };

        let (spent, life_payments) = pay_from_pool(&mut pool, &cost).unwrap();

        assert_eq!(spent.len(), 2);
        assert!(spent
            .iter()
            .any(|unit| unit.source_could_produce_two_or_more_colors));
        assert!(life_payments.is_empty());
        assert_eq!(pool.total(), 0);
    }

    #[test]
    fn pay_cost_rejects_z_from_ineligible_source() {
        let mut pool = pool_with(&[(ManaType::Green, 2)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::TwoOrMoreColorSource],
            generic: 0,
        };

        assert_eq!(
            pay_from_pool(&mut pool, &cost),
            Err(PaymentError::InsufficientMana)
        );
        assert_eq!(pool.total(), 2);
    }

    #[test]
    fn pay_cost_preserves_z_eligible_mana_for_z_shard() {
        let mut pool = ManaPool::default();
        pool.add(make_two_or_more_color_source_unit(ManaType::Green));
        pool.add(make_unit(ManaType::Green));
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::TwoOrMoreColorSource],
            generic: 0,
        };

        let (spent, _) = pay_from_pool(&mut pool, &cost).unwrap();

        assert_eq!(spent.len(), 2);
        assert_eq!(pool.total(), 0);
    }

    // --- produce_mana tests ---

    #[test]
    fn produce_mana_adds_to_pool() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(1),
            ManaType::Green,
            PlayerId(0),
            true,
            &mut events,
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 1);
    }

    #[test]
    fn produce_mana_emits_mana_added_event() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(5),
            ManaType::Blue,
            PlayerId(1),
            true,
            &mut events,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GameEvent::ManaAdded {
                player_id: PlayerId(1),
                mana_type: ManaType::Blue,
                source_id: ObjectId(5),
                tap_state: ManaTapState::FromTap,
            }
        ));
    }

    #[test]
    fn produce_mana_without_unspent_mana_static_does_not_dirty_layers() {
        let mut state = GameState::new_two_player(42);
        state.layers_dirty = LayersDirty::Clean;

        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(5),
            ManaType::Blue,
            PlayerId(1),
            true,
            &mut events,
        );

        assert_eq!(state.layers_dirty, LayersDirty::Clean);
    }

    #[test]
    fn produce_mana_with_unspent_mana_static_dirties_layers() {
        let mut state = GameState::new_two_player(42);
        let omnath_static = StaticDefinition::continuous().modifications(vec![
            ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::UnspentMana {
                        color: Some(crate::types::mana::ManaColor::Green),
                    },
                },
            },
        ]);
        let source_id = ObjectId(99);
        let mut source = GameObject::new(
            source_id,
            CardId(1),
            PlayerId(0),
            "Unspent Mana Static".to_string(),
            Zone::Battlefield,
        );
        source.static_definitions.push(omnath_static.clone());
        source.base_static_definitions = Arc::new(vec![omnath_static]);
        state.objects.insert(source_id, source);
        state.battlefield.push_back(source_id);
        state.layers_dirty = LayersDirty::Clean;

        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(5),
            ManaType::Green,
            PlayerId(0),
            true,
            &mut events,
        );

        assert!(state.layers_dirty.is_dirty());
    }

    #[test]
    fn produce_mana_with_unspent_mana_condition_dirties_layers() {
        let mut state = GameState::new_two_player(42);
        let conditional_static = StaticDefinition::continuous()
            .condition(StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::UnspentMana {
                        color: Some(crate::types::mana::ManaColor::Green),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 1 },
            })
            .modifications(vec![ContinuousModification::AddPower { value: 1 }]);
        let source_id = ObjectId(99);
        let mut source = GameObject::new(
            source_id,
            CardId(1),
            PlayerId(0),
            "Unspent Mana Condition".to_string(),
            Zone::Battlefield,
        );
        source.static_definitions.push(conditional_static.clone());
        source.base_static_definitions = Arc::new(vec![conditional_static]);
        state.objects.insert(source_id, source);
        state.battlefield.push_back(source_id);
        state.layers_dirty = LayersDirty::Clean;

        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(5),
            ManaType::Green,
            PlayerId(0),
            true,
            &mut events,
        );

        assert!(state.layers_dirty.is_dirty());
    }

    #[test]
    fn produce_mana_with_unspent_mana_transient_dirties_layers() {
        let mut state = GameState::new_two_player(42);
        state.add_transient_continuous_effect(
            ObjectId(99),
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::Any,
            vec![ContinuousModification::AddDynamicToughness {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::UnspentMana {
                        color: Some(crate::types::mana::ManaColor::Green),
                    },
                },
            }],
            None,
        );
        state.layers_dirty = LayersDirty::Clean;

        let mut events = Vec::new();
        produce_mana(
            &mut state,
            ObjectId(5),
            ManaType::Green,
            PlayerId(0),
            true,
            &mut events,
        );

        assert!(state.layers_dirty.is_dirty());
    }

    #[test]
    fn produce_mana_routes_through_replacement_pipeline() {
        // CR 106.3 + CR 614.1a: A Contamination-style ProduceMana replacement on a
        // battlefield object must rewrite produced mana as it enters the pool.
        use crate::game::game_object::GameObject;
        use crate::types::ability::{ManaModification, ReplacementDefinition};
        use crate::types::identifiers::CardId;
        use crate::types::replacements::ReplacementEvent;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        // Build a Contamination object with a ProduceMana replacement that
        // rewrites to Black.
        let repl = ReplacementDefinition::new(ReplacementEvent::ProduceMana).mana_modification(
            ManaModification::ReplaceWith {
                mana_type: ManaType::Black,
            },
        );
        let contamination_id = ObjectId(99);
        let mut contamination = GameObject::new(
            contamination_id,
            CardId(1),
            PlayerId(0),
            "Contamination".to_string(),
            Zone::Battlefield,
        );
        contamination.replacement_definitions = vec![repl].into();
        state.objects.insert(contamination_id, contamination);
        state.battlefield.push_back(contamination_id);

        // Build a Forest (land) that will "produce" Green.
        let land_id = ObjectId(10);
        let mut forest = GameObject::new(
            land_id,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        forest
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        state.objects.insert(land_id, forest);
        state.battlefield.push_back(land_id);

        let mut events = Vec::new();
        produce_mana(
            &mut state,
            land_id,
            ManaType::Green,
            PlayerId(0),
            true,
            &mut events,
        );

        // Pool should hold Black, not Green.
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 0);
    }

    #[test]
    fn produce_mana_replacement_multiplier_adds_each_unit() {
        // CR 106.12b: A tapped-for-mana replacement modifies the production
        // event while the mana ability resolves, preserving source metadata on
        // each produced mana unit.
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            ControllerRef, ManaModification, ManaReplacementScope, ReplacementDefinition,
            TargetFilter, TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::replacements::ReplacementEvent;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let repl = ReplacementDefinition::new(ReplacementEvent::ProduceMana)
            .mana_modification(ManaModification::Multiply { factor: 3 })
            .mana_replacement_scope(ManaReplacementScope::TappedForMana)
            .valid_card(TargetFilter::Typed(
                TypedFilter::permanent().controller(ControllerRef::You),
            ));
        let nyxbloom_id = ObjectId(99);
        let mut nyxbloom = GameObject::new(
            nyxbloom_id,
            CardId(1),
            PlayerId(0),
            "Nyxbloom Ancient".to_string(),
            Zone::Battlefield,
        );
        nyxbloom.replacement_definitions = vec![repl].into();
        state.objects.insert(nyxbloom_id, nyxbloom);
        state.battlefield.push_back(nyxbloom_id);

        let land_id = ObjectId(10);
        let mut forest = GameObject::new(
            land_id,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        forest.card_types.core_types.push(CoreType::Land);
        state.objects.insert(land_id, forest);
        state.battlefield.push_back(land_id);

        let mut events = Vec::new();
        produce_mana(
            &mut state,
            land_id,
            ManaType::Green,
            PlayerId(0),
            true,
            &mut events,
        );

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Green), 3);
        let mana_added_events: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, GameEvent::ManaAdded { .. }))
            .collect();
        assert_eq!(mana_added_events.len(), 3);
        assert!(mana_added_events.iter().all(|event| matches!(
            event,
            GameEvent::ManaAdded {
                player_id: PlayerId(0),
                mana_type: ManaType::Green,
                source_id,
                tap_state: ManaTapState::FromTap,
            } if *source_id == land_id
        )));
    }

    // --- can_pay tests ---

    #[test]
    fn can_pay_no_cost() {
        let pool = ManaPool::default();
        assert!(can_pay(&pool, &ManaCost::NoCost));
    }

    #[test]
    fn can_pay_zero_cost() {
        let pool = ManaPool::default();
        assert!(can_pay(&pool, &ManaCost::zero()));
    }

    #[test]
    fn can_pay_single_colored() {
        let pool = pool_with(&[(ManaType::White, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_fails_wrong_color() {
        let pool = pool_with(&[(ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(!can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_generic_with_any_color() {
        let pool = pool_with(&[(ManaType::Green, 3)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        assert!(can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_colored_plus_generic() {
        let pool = pool_with(&[(ManaType::Blue, 2), (ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };
        assert!(can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_insufficient_colored() {
        let pool = pool_with(&[(ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
            generic: 0,
        };
        assert!(!can_pay(&pool, &cost));
    }

    #[test]
    fn can_pay_hybrid_either_color() {
        let pool_w = pool_with(&[(ManaType::White, 1)]);
        let pool_u = pool_with(&[(ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        assert!(can_pay(&pool_w, &cost));
        assert!(can_pay(&pool_u, &cost));
    }

    /// CR 107.4f + CR 118.3 + CR 119.8: Phyrexian payability depends on the
    /// caster's life budget. With zero life budget and no mana of the color,
    /// the cost can't be paid; with budget for even one 2-life payment, it can.
    #[test]
    fn can_pay_phyrexian_requires_mana_or_life_budget() {
        let empty_pool = ManaPool::default();
        let white_pool = pool_with(&[(ManaType::White, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianWhite],
            generic: 0,
        };

        // No mana, no life budget → unpayable.
        assert!(!can_pay_for_spell(
            &empty_pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        // No mana, but life budget ≥ 1 → payable with 2 life.
        assert!(can_pay_for_spell(
            &empty_pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        // Mana of the color is present → payable regardless of life budget.
        assert!(can_pay_for_spell(
            &white_pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// CR 107.4f + CR 118.3: Multi-Phyrexian cost requires enough life-or-mana
    /// combined coverage. Two Phyrexian shards with no mana need budget ≥ 2.
    #[test]
    fn can_pay_multi_phyrexian_tracks_life_budget() {
        let pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlack, ManaCostShard::PhyrexianBlack],
            generic: 0,
        };

        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 2,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// CR 107.4f: Hybrid Phyrexian — with neither mana color available and no
    /// life budget, the cost is unpayable.
    #[test]
    fn can_pay_hybrid_phyrexian_requires_mana_or_life() {
        let empty_pool = ManaPool::default();
        let blue_pool = pool_with(&[(ManaType::Blue, 1)]);
        // {W/U/P} — white, blue, or 2 life.
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianWhiteBlue],
            generic: 0,
        };

        assert!(!can_pay_for_spell(
            &empty_pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        assert!(can_pay_for_spell(
            &empty_pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        assert!(can_pay_for_spell(
            &blue_pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    // --- pay_cost tests ---

    #[test]
    fn pay_cost_colored_shards() {
        let mut pool = pool_with(&[(ManaType::White, 2), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 0,
        };
        let (spent, life) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 2);
        assert!(life.is_empty());
        assert_eq!(pool.total(), 1); // 1 white left
    }

    #[test]
    fn pay_cost_generic_from_any() {
        let mut pool = pool_with(&[(ManaType::Green, 3)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        let (spent, _) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 2);
        assert_eq!(pool.total(), 1);
    }

    #[test]
    fn pay_cost_hybrid_prefers_more_available() {
        // 3 white, 1 blue -- should prefer white for W/U hybrid
        let mut pool = pool_with(&[(ManaType::White, 3), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let (spent, _) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].color, ManaType::White);
    }

    #[test]
    fn pay_cost_hybrid_keeps_repeated_pair_consistent_when_possible() {
        // 2 green, 2 blue and two G/U shards should stay on green by default.
        let mut pool = pool_with(&[(ManaType::Green, 2), (ManaType::Blue, 2)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::GreenBlue, ManaCostShard::GreenBlue],
            generic: 0,
        };
        let (spent, _) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 2);
        assert!(spent.iter().all(|unit| unit.color == ManaType::Green));
    }

    #[test]
    fn pay_cost_hybrid_falls_back_when_uniform_not_possible() {
        // 1 green, 1 blue can't pay both G/U shards as the same color.
        let mut pool = pool_with(&[(ManaType::Green, 1), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::GreenBlue, ManaCostShard::GreenBlue],
            generic: 0,
        };
        let (spent, _) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 2);
        assert!(spent.iter().any(|unit| unit.color == ManaType::Green));
        assert!(spent.iter().any(|unit| unit.color == ManaType::Blue));
    }

    #[test]
    fn pay_cost_phyrexian_with_color_available() {
        let mut pool = pool_with(&[(ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianRed],
            generic: 0,
        };
        let (spent, life) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 1);
        assert!(life.is_empty());
    }

    #[test]
    fn pay_cost_phyrexian_pays_life_when_no_color() {
        let mut pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue],
            generic: 0,
        };
        let (spent, life) = pay_from_pool(&mut pool, &cost).unwrap();
        assert!(spent.is_empty());
        assert_eq!(life.len(), 1);
        assert_eq!(life[0].amount, 2);
    }

    #[test]
    fn pay_cost_phyrexian_defers_to_same_color_strict_shard() {
        // CR 107.4f + CR 118.3: {B/P}{B} with a single black source. The strict
        // {B} must claim the lone black; the {B/P} is then paid with 2 life.
        // Printed-order evaluation would spend the only black on {B/P} and then
        // fail the strict {B} with InsufficientMana (regression for #3306 review).
        let mut pool = pool_with(&[(ManaType::Black, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlack, ManaCostShard::Black],
            generic: 0,
        };
        let (spent, life) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 1, "the lone black pays the strict {{B}}");
        assert_eq!(spent[0].color, ManaType::Black);
        assert_eq!(life.len(), 1, "the {{B/P}} is paid with 2 life");
        assert_eq!(life[0].amount, 2);
    }

    #[test]
    fn compute_phyrexian_shards_defers_to_strict_reports_life_only() {
        // CR 107.4f + CR 601.2f: {B/P}{B} with one black source. Because the
        // strict {B} consumes the lone black, the {B/P} shard's only legal option
        // is life — the UI must not surface ManaOrLife (regression for #3306).
        let pool = pool_with(&[(ManaType::Black, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlack, ManaCostShard::Black],
            generic: 0,
        };
        let shards = compute_phyrexian_shards(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY,
            },
        );
        assert_eq!(shards.len(), 1, "one Phyrexian shard ({{B/P}})");
        assert_eq!(
            shards[0].shard_index, 0,
            "shard_index stays the printed position"
        );
        assert_eq!(
            shards[0].options,
            crate::types::game_state::ShardOptions::LifeOnly,
            "the lone black is reserved for the strict {{B}}, so {{B/P}} is life-only"
        );
    }

    #[test]
    fn pay_cost_insufficient_returns_error() {
        let mut pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(pay_from_pool(&mut pool, &cost).is_err());
    }

    #[test]
    fn pay_cost_generic_prefers_colorless() {
        let mut pool = pool_with(&[(ManaType::Colorless, 1), (ManaType::White, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };
        let (spent, _) = pay_from_pool(&mut pool, &cost).unwrap();
        assert_eq!(spent[0].color, ManaType::Colorless);
    }

    // --- hand-demand-aware hybrid tests ---

    #[test]
    fn pay_cost_hybrid_spends_least_demanded_color() {
        // Pool: 2 white, 2 blue. Equal pool counts.
        // Hand demand: blue is needed more (demand[1]=3) than white (demand[0]=1).
        // So we should spend WHITE (the less demanded color) to preserve blue.
        let mut pool = pool_with(&[(ManaType::White, 2), (ManaType::Blue, 2)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let demand: ColorDemand = [1, 3, 0, 0, 0]; // W=1, U=3
        let (spent, _) =
            pay_cost_with_demand(&mut pool, &cost, Some(&demand), None, false).unwrap();
        assert_eq!(spent[0].color, ManaType::White);
    }

    #[test]
    fn pay_cost_hybrid_falls_back_to_pool_on_equal_demand() {
        // Pool: 3 white, 1 blue. Demand is equal.
        // Should fall back to pool-count heuristic: spend white (more available).
        let mut pool = pool_with(&[(ManaType::White, 3), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let demand: ColorDemand = [2, 2, 0, 0, 0]; // Equal
        let (spent, _) =
            pay_cost_with_demand(&mut pool, &cost, Some(&demand), None, false).unwrap();
        assert_eq!(spent[0].color, ManaType::White);
    }

    #[test]
    fn pay_cost_hybrid_skips_unavailable_color() {
        // Pool: 0 white, 2 blue. White is less demanded but unavailable.
        // Should spend blue (only option).
        let mut pool = pool_with(&[(ManaType::Blue, 2)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let demand: ColorDemand = [0, 5, 0, 0, 0]; // Blue highly demanded but only option
        let (spent, _) =
            pay_cost_with_demand(&mut pool, &cost, Some(&demand), None, false).unwrap();
        assert_eq!(spent[0].color, ManaType::Blue);
    }

    // --- land_subtype_to_mana_type tests ---

    #[test]
    fn land_subtypes_map_correctly() {
        assert_eq!(land_subtype_to_mana_type("Plains"), Some(ManaType::White));
        assert_eq!(land_subtype_to_mana_type("Island"), Some(ManaType::Blue));
        assert_eq!(land_subtype_to_mana_type("Swamp"), Some(ManaType::Black));
        assert_eq!(land_subtype_to_mana_type("Mountain"), Some(ManaType::Red));
        assert_eq!(land_subtype_to_mana_type("Forest"), Some(ManaType::Green));
        assert_eq!(land_subtype_to_mana_type("Desert"), None);
    }

    #[test]
    fn can_pay_for_spell_respects_creature_type_restriction() {
        let mut pool = ManaPool::default();
        // One restricted green (Elf only) + one unrestricted green
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
            grants: vec![],
            expiry: None,
        });
        pool.add(make_unit(ManaType::Green));

        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        };

        // Elf creature: both greens usable
        let elf = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let elf_ctx = PaymentContext::Spell(&elf);
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            Some(&elf_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        // Goblin creature: only unrestricted green usable → insufficient
        let goblin = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let goblin_ctx = PaymentContext::Spell(&goblin);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&goblin_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    #[test]
    fn can_pay_colorless_eldrazi_spell_with_eldrazi_temple_restricted_mana() {
        let mut pool = ManaPool::default();
        for _ in 0..2 {
            pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(1),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![ManaRestriction::OnlyForTypeSpellsOrAbilities {
                    spell_type: "Colorless Eldrazi".to_string(),
                    ability: crate::types::mana::AbilityActivationScope::OfSpellType,
                }],
                grants: vec![],
                expiry: None,
            });
        }
        for _ in 0..2 {
            pool.add(make_unit(ManaType::Colorless));
        }

        let thought_knot_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless],
            generic: 3,
        };
        let thought_knot = SpellMeta {
            types: vec!["Creature".to_string(), "Colorless".to_string()],
            subtypes: vec!["Eldrazi".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let thought_knot_ctx = PaymentContext::Spell(&thought_knot);
        assert!(can_pay_for_spell(
            &pool,
            &thought_knot_cost,
            Some(&thought_knot_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        let colored_eldrazi = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Eldrazi".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let colored_eldrazi_ctx = PaymentContext::Spell(&colored_eldrazi);
        assert!(!can_pay_for_spell(
            &pool,
            &thought_knot_cost,
            Some(&colored_eldrazi_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    #[test]
    fn can_pay_any_ability_activation_with_generic_activation_restricted_mana() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForTypeSpellsOrAbilities {
                spell_type: "Colorless".to_string(),
                ability: crate::types::mana::AbilityActivationScope::Any,
            }],
            grants: vec![],
            expiry: None,
        });

        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless],
            generic: 0,
        };
        let source_types = vec!["Creature".to_string()];
        let source_subtypes = vec!["Goblin".to_string()];
        let goblin_activation = PaymentContext::Activation {
            source_types: &source_types,
            source_subtypes: &source_subtypes,
            ability_tag: None,
        };
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            Some(&goblin_activation),
            crate::types::mana::CostPermissionContext::default(),
        ));

        let colored_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let colored_spell_ctx = PaymentContext::Spell(&colored_spell);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&colored_spell_ctx),
            crate::types::mana::CostPermissionContext::default(),
        ));
    }

    /// CR 106.6: Hydraulic Helper — "{T}: Add {U}. This mana can't be spent to
    /// cast a nonartifact spell." End-to-end through the production payment gate:
    /// the real Oracle phrasing is parsed, lowered through `resolve_restrictions`,
    /// loaded into a `ManaPool`, and spent via `can_pay_for_spell` (which funnels
    /// through `ManaRestriction::allows`). The restriction governs only which
    /// SPELLS the mana may cast; ability activation must stay UNRESTRICTED.
    ///
    /// Discriminating: with the buggy `SpellType("Artifact")` lowering
    /// (`OnlyForSpellType`), `allows_activation` returns false and assertion (b)
    /// fails. Only the fix's `SpellTypeOrAbilityActivation { ability: Any }`
    /// (→ `OnlyForTypeSpellsOrAbilities { ability: Any }`) lets the {U} pay for an
    /// ability while still rejecting a nonartifact spell.
    #[test]
    fn hydraulic_helper_restricted_mana_pays_artifacts_and_any_ability() {
        use crate::types::ability::ManaSpendRestriction;
        use crate::types::mana::AbilityActivationScope;

        // Parser fix under test: the negative phrasing must lower to the OR variant.
        let (ast, _grants) = crate::parser::oracle_effect::mana::parse_mana_spend_restriction(
            "this mana can't be spent to cast a nonartifact spell",
        )
        .expect("Hydraulic Helper's spend restriction must parse");
        assert_eq!(
            ast,
            ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: "Artifact".to_string(),
                ability: AbilityActivationScope::Any,
            },
            "negative nonartifact restriction must keep ability activation unrestricted"
        );

        // Lower through the real runtime resolver (state-independent for this variant).
        let state = GameState::new_two_player(42);
        let runtime = crate::game::effects::mana::resolve_restrictions(
            std::slice::from_ref(&ast),
            &state,
            ObjectId(1),
        );

        // The produced {U} carries the lowered restriction.
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: runtime,
            grants: vec![],
            expiry: None,
        });

        let one_blue = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };

        // (a) Casting a nonartifact spell with this mana is rejected.
        let instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        assert!(
            !can_pay_for_spell(
                &pool,
                &one_blue,
                Some(&PaymentContext::Spell(&instant)),
                crate::types::mana::CostPermissionContext::default(),
            ),
            "a nonartifact (instant) spell must not be payable with artifact-restricted mana"
        );

        // (b) DISCRIMINATING: activating an ability with this mana is allowed,
        //     regardless of the activating permanent's types.
        let creature_types = vec!["Creature".to_string()];
        let no_subtypes: Vec<String> = vec![];
        assert!(
            can_pay_for_spell(
                &pool,
                &one_blue,
                Some(&PaymentContext::Activation {
                    source_types: &creature_types,
                    source_subtypes: &no_subtypes,
                    ability_tag: None,
                }),
                crate::types::mana::CostPermissionContext::default(),
            ),
            "ability activation must remain payable — the restriction governs spells only"
        );

        // Sanity: an artifact spell IS payable.
        let artifact = SpellMeta {
            types: vec!["Artifact".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: None,
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        assert!(
            can_pay_for_spell(
                &pool,
                &one_blue,
                Some(&PaymentContext::Spell(&artifact)),
                crate::types::mana::CostPermissionContext::default(),
            ),
            "an artifact spell must be payable with the artifact-restricted mana"
        );
    }

    /// CR 106.6: Hydraulic Helper end-to-end through GameScenario + GameRunner —
    /// the restricted `{U}` ("can't be spent to cast a nonartifact spell") MUST
    /// remain spendable to ACTIVATE AN ABILITY. The restricted mana is seeded
    /// into the pool and a `{U}`-cost activated ability is driven through the real
    /// activation pipeline: the `AbilityActivation` driver finalizes the cost at
    /// `WaitingFor::ManaPayment` via `PassPriority`, which the engine accepts only
    /// if the pool can pay — i.e. only if the restriction permits ability
    /// activation.
    ///
    /// Discriminating: with the buggy `OnlyForSpellType("Artifact")` lowering,
    /// `allows_activation` returns false, `PassPriority` errors, and the driver's
    /// `.expect("finalizing the ability's mana payment must be accepted")` panics
    /// — the test fails. Only the fix's `OnlyForTypeSpellsOrAbilities { ability:
    /// Any }` lets the ability resolve and gain the life this asserts.
    #[test]
    fn hydraulic_helper_restricted_mana_activates_ability_through_pipeline() {
        use crate::game::scenario::GameScenario;
        use crate::types::mana::AbilityActivationScope;
        use crate::types::player::PlayerId;

        let p0 = PlayerId(0);
        let mut scenario = GameScenario::new_n_player(2, 42);
        // A permanent with a {U}-cost activated ability (no tap, no targets).
        let sink = scenario
            .add_creature_from_oracle(p0, "Mana Sink", 1, 1, "{U}: You gain 1 life.")
            .id();
        // Seed Hydraulic Helper's restricted {U} into P0's pool (the lowered form
        // of "this mana can't be spent to cast a nonartifact spell").
        scenario.with_mana_pool(
            p0,
            vec![ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(9999),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: vec![ManaRestriction::OnlyForTypeSpellsOrAbilities {
                    spell_type: "Artifact".to_string(),
                    ability: AbilityActivationScope::Any,
                }],
                grants: vec![],
                expiry: None,
            }],
        );

        let mut runner = scenario.build();
        let life_before = runner.life(p0);
        // Drives announce → ManaPayment (PassPriority pays the {U} from the
        // restricted pool) → resolve. Panics if the restricted mana cannot pay.
        runner.activate(sink, 0).resolve();
        assert_eq!(
            runner.life(p0),
            life_before + 1,
            "restricted {{U}} must remain spendable to activate an ability (CR 106.6); \
             the ability resolved only because the mana paid its cost"
        );
    }

    #[test]
    fn can_pay_for_spell_respects_flashback_keyword_restriction() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForSpellWithKeywordKind(
                crate::types::keywords::KeywordKind::Flashback,
            )],
            grants: vec![],
            expiry: None,
        });

        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };

        let flashback_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![crate::types::keywords::KeywordKind::Flashback],
            cast_from_zone: Some(crate::types::zones::Zone::Graveyard),
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let flashback_ctx = PaymentContext::Spell(&flashback_spell);
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            Some(&flashback_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        let normal_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: Some(crate::types::zones::Zone::Hand),
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let normal_ctx = PaymentContext::Spell(&normal_spell);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&normal_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    #[test]
    fn can_pay_for_spell_respects_flashback_zone_restriction() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForSpellWithKeywordKindFromZone(
                crate::types::keywords::KeywordKind::Flashback,
                crate::types::zones::Zone::Graveyard,
            )],
            grants: vec![],
            expiry: None,
        });

        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };

        let graveyard_flashback_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![crate::types::keywords::KeywordKind::Flashback],
            cast_from_zone: Some(crate::types::zones::Zone::Graveyard),
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let gy_ctx = PaymentContext::Spell(&graveyard_flashback_spell);
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            Some(&gy_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        let hand_flashback_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![crate::types::keywords::KeywordKind::Flashback],
            cast_from_zone: Some(crate::types::zones::Zone::Hand),
            mana_value: None,
            color_count: None,
            has_x_in_cost: false,
            is_face_down: false,
        };
        let hand_ctx = PaymentContext::Spell(&hand_flashback_spell);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&hand_ctx),
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    #[test]
    fn can_pay_any_color_allows_wrong_color_mana() {
        // CR 609.4b: With any_color=true, green mana can pay for a white cost.
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
        });
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        // Without any_color, can't pay white with green
        assert!(!can_pay(&pool, &cost));
        // With any_color, can pay white with green
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: true,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    #[test]
    fn pay_cost_any_color_spends_available_mana() {
        // CR 609.4b: pay_cost_with_demand with any_color uses available mana for colored costs.
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(1),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![],
            grants: vec![],
            expiry: None,
        });
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };
        let result = pay_cost_with_demand(&mut pool, &cost, None, None, true);
        assert!(result.is_ok());
        let (spent, _) = result.unwrap();
        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].color, ManaType::Red);
    }

    #[test]
    fn any_color_does_not_let_generic_convoke_pay_colorless_symbol() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::convoke_payment(ManaType::Colorless, ObjectId(1)));
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Colorless],
            generic: 0,
        };

        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: true,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        assert!(pay_cost_with_demand(&mut pool, &cost, None, None, true).is_err());
    }

    #[test]
    fn generic_convoke_payment_still_pays_generic_cost() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit::convoke_payment(ManaType::Colorless, ObjectId(1)));
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };

        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: true,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        let (spent, _) = pay_cost_with_demand(&mut pool, &cost, None, None, true).unwrap();
        assert_eq!(spent.len(), 1);
        assert!(spent[0].is_convoke_payment());
    }

    #[test]
    fn any_color_does_not_change_colored_convoke_payment_color() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 0,
        };

        let mut only_convoke = ManaPool::default();
        only_convoke.add(ManaUnit::convoke_payment(ManaType::Green, ObjectId(1)));
        assert!(!can_pay_for_spell(
            &only_convoke,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: true,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));

        let mut pool = ManaPool::default();
        pool.add(ManaUnit::convoke_payment(ManaType::Green, ObjectId(1)));
        pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(2),
            false,
            Vec::new(),
        ));

        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: true,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        let (spent, _) = pay_cost_with_demand(&mut pool, &cost, None, None, true).unwrap();
        assert_eq!(spent.len(), 1);
        assert!(!spent[0].is_convoke_payment());
        assert!(pool.mana.iter().any(ManaUnit::is_convoke_payment));
    }

    /// CR 107.4f + CR 118.3: Phyrexian Metamorph scenario — {3}{U/P} with only
    /// 3 Blue available. Greedy mana-first for the Phyrexian shard would spend 1U
    /// leaving only 2U for generic 3 (fail). The deferred approach recognizes that
    /// paying life for {U/P} leaves the full 3U for generic (success).
    #[test]
    fn can_pay_phyrexian_defers_to_life_when_mana_needed_for_generic() {
        let pool = pool_with(&[(ManaType::Blue, 3)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue],
            generic: 3,
        };
        // With life budget, payable: 3U covers generic, 2 life covers {U/P}.
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        // Without life budget and only 3 mana for a 4-mana effective cost: unpayable.
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// CR 107.4f: When the pool has surplus mana beyond generic, prefer mana for
    /// Phyrexian shards (preserves life).
    #[test]
    fn can_pay_phyrexian_prefers_mana_when_pool_has_surplus() {
        let pool = pool_with(&[(ManaType::Blue, 4)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue],
            generic: 3,
        };
        // 4U covers both: 1U for {U/P} + 3U for generic. Life not needed.
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// CR 107.4f: Dismember scenario — {1}{B/P}{B/P} with 2 Swamps (2B).
    /// One Phyrexian shard can be paid with mana (surplus: 2 > 1), the second
    /// must use life (remaining 1 = generic 1, no surplus).
    #[test]
    fn can_pay_multi_phyrexian_defers_second_shard_to_life() {
        let pool = pool_with(&[(ManaType::Black, 2)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlack, ManaCostShard::PhyrexianBlack],
            generic: 1,
        };
        // 2B + 2 life: 1B for first {B/P}, life for second {B/P}, 1B for generic.
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        // 2B + 0 life: 1B for first {B/P}, no life for second → still needs 1B
        // for generic but only 1B left → can't cover both second shard and generic.
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// CR 107.4f: Dismember with 0 mana — needs at least 1 mana for generic
    /// regardless of life budget.
    #[test]
    fn can_pay_multi_phyrexian_still_requires_generic_mana() {
        let pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlack, ManaCostShard::PhyrexianBlack],
            generic: 1,
        };
        // Even with enough life for both Phyrexian shards, generic 1 is unpayable.
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 5,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// CR 107.4f: Gitaxian Probe {U/P} with 0 mana is payable with life alone.
    #[test]
    fn can_pay_phyrexian_no_generic_life_only() {
        let pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianBlue],
            generic: 0,
        };
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 1,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            None,
            crate::types::mana::CostPermissionContext {
                any_color: false,
                max_life: 0,
                life_colors: crate::types::mana::LifePaymentColors::EMPTY
            }
        ));
    }

    /// `refill_infinite_mana` is the debug "infinite mana" building block. It must
    /// seed every flagged player's pool to `INFINITE_MANA_PER_TYPE` units of each
    /// of the six mana types, restore that cap after a spend without unbounded
    /// growth (idempotent), no-op when no player is flagged, and never touch an
    /// unflagged player.
    #[test]
    fn refill_infinite_mana_seeds_tops_up_and_isolates_players() {
        let mut state = GameState::new_two_player(0);
        let p1 = state.players[1].id;

        // No player flagged → cheap no-op.
        refill_infinite_mana(&mut state);
        assert!(state.players[0].mana_pool.mana.is_empty());

        // Flag P0 → seeded to the cap for each of the six types; P1 untouched.
        state.debug_infinite_mana.insert(state.players[0].id);
        refill_infinite_mana(&mut state);
        for color in INFINITE_MANA_TYPES {
            let n = state.players[0]
                .mana_pool
                .mana
                .iter()
                .filter(|u| u.color == color)
                .count();
            assert_eq!(n, INFINITE_MANA_PER_TYPE, "{color:?} seeded to cap");
        }
        let p1_pool = state.players.iter().find(|p| p.id == p1).unwrap();
        assert!(
            p1_pool.mana_pool.mana.is_empty(),
            "unflagged player untouched"
        );

        // Spend two units, refill restores to the cap — idempotent, no growth.
        assert!(state.players[0].mana_pool.spend(ManaType::White).is_some());
        assert!(state.players[0].mana_pool.spend(ManaType::Green).is_some());
        refill_infinite_mana(&mut state);
        let total: usize = INFINITE_MANA_TYPES
            .iter()
            .map(|&c| {
                state.players[0]
                    .mana_pool
                    .mana
                    .iter()
                    .filter(|u| u.color == c)
                    .count()
            })
            .sum();
        assert_eq!(
            total,
            INFINITE_MANA_PER_TYPE * INFINITE_MANA_TYPES.len(),
            "topped back up to cap with no unbounded growth"
        );
    }

    /// The `SetInfiniteMana` debug handler must add the player to
    /// `debug_infinite_mana` and seed the pool immediately on enable (so the next
    /// affordability probe reads full), and remove the flag on disable.
    #[test]
    fn set_infinite_mana_handler_toggles_flag_and_seeds() {
        use crate::game::engine_debug::apply_debug_action;
        use crate::types::actions::DebugAction;

        let mut state = GameState::new_two_player(0);
        let p0 = state.players[0].id;
        let mut events = Vec::new();

        apply_debug_action(
            &mut state,
            p0,
            DebugAction::SetInfiniteMana {
                player_id: p0,
                enabled: true,
            },
            &mut events,
        )
        .expect("enable infinite mana");
        assert!(state.debug_infinite_mana.contains(&p0));
        for color in INFINITE_MANA_TYPES {
            assert!(
                state.players[0]
                    .mana_pool
                    .mana
                    .iter()
                    .any(|u| u.color == color),
                "{color:?} seeded on enable"
            );
        }

        apply_debug_action(
            &mut state,
            p0,
            DebugAction::SetInfiniteMana {
                player_id: p0,
                enabled: false,
            },
            &mut events,
        )
        .expect("disable infinite mana");
        assert!(!state.debug_infinite_mana.contains(&p0));
    }
}
