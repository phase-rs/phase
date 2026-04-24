use thiserror::Error;

use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ShardChoice};
use crate::types::identifiers::ObjectId;
use crate::types::mana::{
    ManaCost, ManaCostShard, ManaExpiry, ManaPool, ManaRestriction, ManaSpellGrant, ManaType,
    ManaUnit, PaymentContext,
};
use crate::types::player::PlayerId;

/// Color demand array indexed by WUBRG (White=0, Blue=1, Black=2, Red=3, Green=4).
/// CR 107.4a: The five colors are white ({W}), blue ({U}), black ({B}), red ({R}), green ({G}).
pub type ColorDemand = [u32; 5];

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
                    match shard_to_mana_type(*shard) {
                        ShardRequirement::Single(mt) => {
                            if let Some(i) = mana_type_to_demand_index(mt) {
                                demand[i] += 1;
                            }
                        }
                        ShardRequirement::Hybrid(a, b)
                        | ShardRequirement::HybridPhyrexian(a, b) => {
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
                        | ShardRequirement::ColorlessHybrid(mt) => {
                            if let Some(i) = mana_type_to_demand_index(mt) {
                                demand[i] += 1;
                            }
                        }
                        ShardRequirement::Snow | ShardRequirement::X => {}
                    }
                }
            }
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
    pub player_id: PlayerId,
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
    use crate::game::replacement::{self, ReplacementResult};
    use crate::types::proposed_event::ProposedEvent;

    let proposed = ProposedEvent::produce_mana(source_id, player_id, mana_type);
    let final_mana_type = match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::ProduceMana {
            mana_type: resolved,
            ..
        }) => resolved,
        // CR 614.1: A fully-prevented mana production produces no mana.
        ReplacementResult::Prevented => return,
        // CR 614.5: Mana-type replacements do not require a player choice; any
        // other outcome (including unexpected pipeline results) falls back to
        // the original type so mana production is never silently dropped.
        _ => mana_type,
    };

    let unit = ManaUnit {
        color: final_mana_type,
        source_id,
        snow: false,
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
        tapped_for_mana,
    });
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
    can_pay_for_spell(pool, cost, None, false, 0)
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
            ShardRequirement::Phyrexian(..) | ShardRequirement::HybridPhyrexian(..) => {
                return PaymentClassification::NeedsPhyrexianChoice;
            }
            ShardRequirement::Single(..) | ShardRequirement::Snow | ShardRequirement::X => {}
        }
    }
    PaymentClassification::Unambiguous
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
    any_color: bool,
    max_life_payments: u32,
) -> bool {
    match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost => true,
        ManaCost::Cost { shards, generic } => {
            // Clone pool to simulate payment
            let mut sim = pool.clone();
            let mut life_budget = max_life_payments;
            // Pay colored shards first
            for shard in shards {
                match shard_to_mana_type(*shard) {
                    ShardRequirement::Single(mt) => {
                        // CR 609.4b: When any_color is true, any mana can pay colored costs.
                        if any_color {
                            if spend_any_eligible(&mut sim, spell).is_none() {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, mt, spell).is_none() {
                            return false;
                        }
                    }
                    // CR 107.4e: Hybrid mana — can be paid with either color.
                    ShardRequirement::Hybrid(a, b) => {
                        if any_color {
                            if spend_any_eligible(&mut sim, spell).is_none() {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, a, spell).is_none()
                            && spend_eligible(&mut sim, b, spell).is_none()
                        {
                            return false;
                        }
                    }
                    // CR 107.4f: Phyrexian mana — pay one mana of indicated color or 2 life.
                    // Prefer mana when available (matches `pay_cost_with_demand`);
                    // otherwise consume a life payment from the budget.
                    ShardRequirement::Phyrexian(color) => {
                        let mana_ok = if any_color {
                            spend_any_eligible(&mut sim, spell).is_some()
                        } else {
                            spend_eligible(&mut sim, color, spell).is_some()
                        };
                        if !mana_ok {
                            // CR 118.3 + CR 119.8: Life fallback requires budget.
                            if life_budget == 0 {
                                return false;
                            }
                            life_budget -= 1;
                        }
                    }
                    // CR 107.4e: Monocolored hybrid {2/C} — pay 1 colored or 2 generic.
                    ShardRequirement::TwoGenericHybrid(color) => {
                        // CR 609.4b: When any_color, any mana satisfies the colored half.
                        if any_color {
                            if spend_any_eligible(&mut sim, spell).is_none() {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, color, spell).is_none() {
                            if spend_any_eligible(&mut sim, spell).is_none() {
                                return false;
                            }
                            if spend_any_eligible(&mut sim, spell).is_none() {
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
                    // CR 107.3: {X} — can be 0, so always satisfiable in a can-pay check.
                    ShardRequirement::X => {}
                    // CR 107.4e: Colorless hybrid {C/color} — pay colorless or colored.
                    ShardRequirement::ColorlessHybrid(color) => {
                        if any_color {
                            if spend_any_eligible(&mut sim, spell).is_none() {
                                return false;
                            }
                        } else if spend_eligible(&mut sim, ManaType::Colorless, spell).is_none()
                            && spend_eligible(&mut sim, color, spell).is_none()
                        {
                            return false;
                        }
                    }
                    // CR 107.4f: Hybrid Phyrexian — pay either component color or 2 life.
                    ShardRequirement::HybridPhyrexian(a, b) => {
                        let mana_ok = if any_color {
                            spend_any_eligible(&mut sim, spell).is_some()
                        } else {
                            spend_eligible(&mut sim, a, spell).is_some()
                                || spend_eligible(&mut sim, b, spell).is_some()
                        };
                        if !mana_ok {
                            // CR 118.3 + CR 119.8: Life fallback requires budget.
                            if life_budget == 0 {
                                return false;
                            }
                            life_budget -= 1;
                        }
                    }
                }
            }
            // Pay generic
            for _ in 0..*generic {
                if spend_any_eligible(&mut sim, spell).is_none() {
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
pub fn pay_cost(
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
) -> ManaCost {
    let (shards, generic) = match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost => return cost.clone(),
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
                if any_color {
                    spend_any_eligible(&mut scratch, spell).is_some()
                } else {
                    spend_eligible(&mut scratch, color, spell).is_some()
                }
            }
            // CR 107.4e/f: Hybrid pays either half.
            ShardRequirement::Hybrid(a, b) | ShardRequirement::HybridPhyrexian(a, b) => {
                if any_color {
                    spend_any_eligible(&mut scratch, spell).is_some()
                } else {
                    spend_eligible(&mut scratch, a, spell).is_some()
                        || spend_eligible(&mut scratch, b, spell).is_some()
                }
            }
            // CR 107.4e: {C/color} — prefer colorless, else the colored half.
            ShardRequirement::ColorlessHybrid(color) => {
                if any_color {
                    spend_any_eligible(&mut scratch, spell).is_some()
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
                    spend_any_eligible(&mut scratch, spell).is_some()
                } else if spend_eligible(&mut scratch, color, spell).is_some() {
                    true
                } else {
                    let mut backup = scratch.clone();
                    if spend_any_eligible(&mut backup, spell).is_some()
                        && spend_any_eligible(&mut backup, spell).is_some()
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
            // CR 107.1b: `ManaCost::concretize_x` strips `X` shards into generic
            // before auto-tap runs, so this arm is defensive. Keep the shard in
            // the residual so auto-tap's legacy `deferred_generic += 1` path
            // still fires in the edge case where an unconverted X reaches here.
            ShardRequirement::X => false,
        };
        if !paid {
            residual_shards.push(shard);
        }
    }

    // CR 107.4b: Generic may be paid with any eligible mana.
    for _ in 0..generic {
        if spend_any_eligible(&mut scratch, spell).is_some() {
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
    pay_cost_with_demand_and_choices(pool, cost, hand_demand, spell, any_color, None)
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
) -> Result<(Vec<ManaUnit>, Vec<LifePayment>), PaymentError> {
    match cost {
        ManaCost::NoCost | ManaCost::SelfManaCost => Ok((Vec::new(), Vec::new())),
        ManaCost::Cost { shards, generic } => {
            let mut spent = Vec::new();
            let mut life_payments = Vec::new();
            let mut choice_cursor = 0usize;

            // CR 107.4a: Pay colored shards first (exact color match required).
            for shard in shards {
                match shard_to_mana_type(*shard) {
                    ShardRequirement::Single(mt) => {
                        // CR 609.4b: When any_color, any mana can pay colored costs.
                        if any_color {
                            let unit = spend_any_eligible(pool, spell)
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
                            let unit = spend_any_eligible(pool, spell)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        } else {
                            let color = auto_pay_hybrid(pool, a, b, hand_demand);
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
                                life_payments.push(LifePayment {
                                    player_id: PlayerId(0),
                                    amount: 2,
                                });
                            }
                            Some(ShardChoice::PayMana) => {
                                let unit = if any_color {
                                    spend_any_eligible(pool, spell)
                                } else {
                                    spend_eligible(pool, color, spell)
                                }
                                .ok_or(PaymentError::InsufficientMana)?;
                                spent.push(unit);
                            }
                            None => {
                                // CR 107.4f: Auto-decide — prefer mana when available.
                                if any_color {
                                    if let Some(unit) = spend_any_eligible(pool, spell) {
                                        spent.push(unit);
                                    } else {
                                        life_payments.push(LifePayment {
                                            player_id: PlayerId(0),
                                            amount: 2,
                                        });
                                    }
                                } else if let Some(unit) = spend_eligible(pool, color, spell) {
                                    spent.push(unit);
                                } else {
                                    life_payments.push(LifePayment {
                                        player_id: PlayerId(0),
                                        amount: 2,
                                    });
                                }
                            }
                        }
                    }
                    // CR 107.4e: Monocolored hybrid {2/C} — pay 1 colored or 2 generic.
                    ShardRequirement::TwoGenericHybrid(color) => {
                        if any_color {
                            let unit = spend_any_eligible(pool, spell)
                                .ok_or(PaymentError::InsufficientMana)?;
                            spent.push(unit);
                        } else if let Some(unit) = spend_eligible(pool, color, spell) {
                            spent.push(unit);
                        } else {
                            for _ in 0..2 {
                                let unit = spend_any_eligible(pool, spell)
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
                    // CR 107.3: {X} defaults to 0; caller specifies X value separately.
                    ShardRequirement::X => {}
                    // CR 107.4e: Colorless hybrid {C/color} — prefer colorless, then colored.
                    ShardRequirement::ColorlessHybrid(color) => {
                        if any_color {
                            let unit = spend_any_eligible(pool, spell)
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
                                life_payments.push(LifePayment {
                                    player_id: PlayerId(0),
                                    amount: 2,
                                });
                            }
                            Some(ShardChoice::PayMana) => {
                                let unit = if any_color {
                                    spend_any_eligible(pool, spell)
                                } else {
                                    let color = auto_pay_hybrid(pool, a, b, hand_demand);
                                    spend_eligible(pool, color, spell)
                                }
                                .ok_or(PaymentError::InsufficientMana)?;
                                spent.push(unit);
                            }
                            None => {
                                if any_color {
                                    if let Some(unit) = spend_any_eligible(pool, spell) {
                                        spent.push(unit);
                                    } else {
                                        life_payments.push(LifePayment {
                                            player_id: PlayerId(0),
                                            amount: 2,
                                        });
                                    }
                                } else {
                                    let color = auto_pay_hybrid(pool, a, b, hand_demand);
                                    if let Some(unit) = spend_eligible(pool, color, spell) {
                                        spent.push(unit);
                                    } else {
                                        life_payments.push(LifePayment {
                                            player_id: PlayerId(0),
                                            amount: 2,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // CR 107.4b: Generic mana can be paid with any type of mana.
            // Prefer colorless first, then least-available color to preserve flexibility.
            for _ in 0..*generic {
                let unit = spend_any_eligible(pool, spell).ok_or(PaymentError::InsufficientMana)?;
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
/// `WaitingFor::PhyrexianPayment` (pause iff any shard has `ShardOptions::ManaOrLife`).
///
/// The computation is a simulated dry-run: we spend mana from a cloned pool in order,
/// checking each Phyrexian shard's mana option against the pool state *after* previous
/// non-Phyrexian shards have consumed their mana. This matches the ordering used by
/// `pay_cost_with_demand_and_choices`.
pub fn compute_phyrexian_shards(
    pool: &ManaPool,
    cost: &ManaCost,
    spell: Option<&PaymentContext<'_>>,
    any_color: bool,
    max_life_payments: u32,
) -> Vec<crate::types::game_state::PhyrexianShard> {
    use crate::types::game_state::{PhyrexianShard, ShardOptions};

    let shards = match cost {
        ManaCost::Cost { shards, .. } => shards,
        _ => return Vec::new(),
    };

    let mut sim = pool.clone();
    let mut results = Vec::new();
    // CR 107.4f + CR 118.3 + CR 119.8: Mana preference within the dry-run matches
    // `pay_cost_with_demand_and_choices`' auto-decision. `life_budget` tracks how many
    // life payments remain unspent — once exhausted, subsequent shards report `ManaOnly`
    // (or `LifeOnly`/unpayable would have failed `can_pay_for_spell` upstream).
    let mut life_budget = max_life_payments;

    for (idx, shard) in shards.iter().enumerate() {
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(mt) => {
                if any_color {
                    let _ = spend_any_eligible(&mut sim, spell);
                } else {
                    let _ = spend_eligible(&mut sim, mt, spell);
                }
            }
            ShardRequirement::Hybrid(a, b) => {
                if any_color {
                    let _ = spend_any_eligible(&mut sim, spell);
                } else {
                    let color = auto_pay_hybrid(&sim, a, b, None);
                    let _ = spend_eligible(&mut sim, color, spell);
                }
            }
            ShardRequirement::Phyrexian(color) => {
                let mana_available = sim_phyrexian_mana_available(&sim, spell, any_color, color);
                let life_available = life_budget > 0;
                let options = match (mana_available, life_available) {
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
                // if mana is unavailable, consume one life payment from budget.
                if mana_available {
                    let _ = if any_color {
                        spend_any_eligible(&mut sim, spell)
                    } else {
                        spend_eligible(&mut sim, color, spell)
                    };
                } else {
                    life_budget = life_budget.saturating_sub(1);
                }
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                if any_color {
                    let _ = spend_any_eligible(&mut sim, spell);
                } else if spend_eligible(&mut sim, color, spell).is_none() {
                    for _ in 0..2 {
                        let _ = spend_any_eligible(&mut sim, spell);
                    }
                }
            }
            ShardRequirement::Snow => {
                let _ = spend_snow_unit(&mut sim);
            }
            ShardRequirement::X => {}
            ShardRequirement::ColorlessHybrid(color) => {
                if any_color {
                    let _ = spend_any_eligible(&mut sim, spell);
                } else if spend_eligible(&mut sim, ManaType::Colorless, spell).is_none() {
                    let _ = spend_eligible(&mut sim, color, spell);
                }
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                let mana_available = if any_color {
                    sim_any_color_available(&sim, spell)
                } else {
                    sim_color_available(&sim, spell, a) || sim_color_available(&sim, spell, b)
                };
                let life_available = life_budget > 0;
                let options = match (mana_available, life_available) {
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
                if mana_available {
                    let _ = if any_color {
                        spend_any_eligible(&mut sim, spell)
                    } else {
                        let color = auto_pay_hybrid(&sim, a, b, None);
                        spend_eligible(&mut sim, color, spell)
                    };
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
        sim_any_color_available(pool, spell)
    } else {
        sim_color_available(pool, spell, color)
    }
}

fn sim_any_color_available(pool: &ManaPool, spell: Option<&PaymentContext<'_>>) -> bool {
    let mut clone = pool.clone();
    spend_any_eligible(&mut clone, spell).is_some()
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

/// For a hybrid shard like W/U, returns the best color to spend.
/// When hand demand is available, spends the color *least needed* by other cards in hand.
/// Falls back to spending whichever color has more in the pool (preserving the scarcer color).
fn auto_pay_hybrid(
    pool: &ManaPool,
    a: ManaType,
    b: ManaType,
    hand_demand: Option<&ColorDemand>,
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

    // Tiebreaker: spend whichever we have more of (preserve the scarcer color)
    if count_a >= count_b {
        a
    } else {
        b
    }
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
/// When `spell` is `Some`, delegates to `ManaPool::spend_for` (restriction-aware).
/// When `spell` is `None`, delegates to `ManaPool::spend` (unrestricted).
fn spend_eligible(
    pool: &mut ManaPool,
    color: ManaType,
    spell: Option<&PaymentContext<'_>>,
) -> Option<ManaUnit> {
    match spell {
        Some(meta) => pool.spend_for(color, meta),
        None => pool.spend(color),
    }
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
    X,
    ColorlessHybrid(ManaType),
    HybridPhyrexian(ManaType, ManaType),
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

fn spend_any_eligible(pool: &mut ManaPool, spell: Option<&PaymentContext<'_>>) -> Option<ManaUnit> {
    match spell {
        Some(ctx) => {
            if let Some(unit) = pool.spend_for(ManaType::Colorless, ctx) {
                return Some(unit);
            }

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
                    .filter(|m| m.color == color && m.restrictions.iter().all(|r| r.allows(ctx)))
                    .count();
                if count > 0 {
                    match best {
                        None => best = Some((color, count)),
                        Some((_, best_count)) if count < best_count => best = Some((color, count)),
                        _ => {}
                    }
                }
            }
            best.and_then(|(color, _)| pool.spend_for(color, ctx))
        }
        None => spend_any_unit(pool),
    }
}

fn spend_any_unit(pool: &mut ManaPool) -> Option<ManaUnit> {
    if pool.mana.is_empty() {
        return None;
    }

    // Prefer colorless first, then least-available color
    if let Some(unit) = pool.spend(ManaType::Colorless) {
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
        let count = pool.count_color(color);
        if count > 0 {
            match best {
                None => best = Some((color, count)),
                Some((_, best_count)) if count < best_count => best = Some((color, count)),
                _ => {}
            }
        }
    }

    best.and_then(|(color, _)| pool.spend(color))
}

fn spend_snow(pool: &mut ManaPool) -> bool {
    spend_snow_unit(pool).is_some()
}

/// CR 107.4h: Snow mana {S} — paid with one mana of any type from a snow source.
fn spend_snow_unit(pool: &mut ManaPool) -> Option<ManaUnit> {
    if let Some(pos) = pool.mana.iter().position(|m| m.snow) {
        Some(pool.mana.swap_remove(pos))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaRestriction, SpellMeta};

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
            snow: false,
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
                tapped_for_mana: true,
            }
        ));
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
        assert!(!can_pay_for_spell(&empty_pool, &cost, None, false, 0));

        // No mana, but life budget ≥ 1 → payable with 2 life.
        assert!(can_pay_for_spell(&empty_pool, &cost, None, false, 1));

        // Mana of the color is present → payable regardless of life budget.
        assert!(can_pay_for_spell(&white_pool, &cost, None, false, 0));
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

        assert!(!can_pay_for_spell(&pool, &cost, None, false, 0));
        assert!(!can_pay_for_spell(&pool, &cost, None, false, 1));
        assert!(can_pay_for_spell(&pool, &cost, None, false, 2));
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

        assert!(!can_pay_for_spell(&empty_pool, &cost, None, false, 0));
        assert!(can_pay_for_spell(&empty_pool, &cost, None, false, 1));
        assert!(can_pay_for_spell(&blue_pool, &cost, None, false, 0));
    }

    // --- pay_cost tests ---

    #[test]
    fn pay_cost_colored_shards() {
        let mut pool = pool_with(&[(ManaType::White, 2), (ManaType::Blue, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 0,
        };
        let (spent, life) = pay_cost(&mut pool, &cost).unwrap();
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
        let (spent, _) = pay_cost(&mut pool, &cost).unwrap();
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
        let (spent, _) = pay_cost(&mut pool, &cost).unwrap();
        assert_eq!(spent.len(), 1);
        assert_eq!(spent[0].color, ManaType::White);
    }

    #[test]
    fn pay_cost_phyrexian_with_color_available() {
        let mut pool = pool_with(&[(ManaType::Red, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::PhyrexianRed],
            generic: 0,
        };
        let (spent, life) = pay_cost(&mut pool, &cost).unwrap();
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
        let (spent, life) = pay_cost(&mut pool, &cost).unwrap();
        assert!(spent.is_empty());
        assert_eq!(life.len(), 1);
        assert_eq!(life[0].amount, 2);
    }

    #[test]
    fn pay_cost_insufficient_returns_error() {
        let mut pool = ManaPool::default();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        };
        assert!(pay_cost(&mut pool, &cost).is_err());
    }

    #[test]
    fn pay_cost_generic_prefers_colorless() {
        let mut pool = pool_with(&[(ManaType::Colorless, 1), (ManaType::White, 1)]);
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };
        let (spent, _) = pay_cost(&mut pool, &cost).unwrap();
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
            snow: false,
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
        };
        let elf_ctx = PaymentContext::Spell(&elf);
        assert!(can_pay_for_spell(&pool, &cost, Some(&elf_ctx), false, 0));

        // Goblin creature: only unrestricted green usable → insufficient
        let goblin = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
            keyword_kinds: vec![],
            cast_from_zone: None,
        };
        let goblin_ctx = PaymentContext::Spell(&goblin);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&goblin_ctx),
            false,
            0
        ));
    }

    #[test]
    fn can_pay_for_spell_respects_flashback_keyword_restriction() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(1),
            snow: false,
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
        };
        let flashback_ctx = PaymentContext::Spell(&flashback_spell);
        assert!(can_pay_for_spell(
            &pool,
            &cost,
            Some(&flashback_ctx),
            false,
            0,
        ));

        let normal_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![],
            cast_from_zone: Some(crate::types::zones::Zone::Hand),
        };
        let normal_ctx = PaymentContext::Spell(&normal_spell);
        assert!(!can_pay_for_spell(
            &pool,
            &cost,
            Some(&normal_ctx),
            false,
            0
        ));
    }

    #[test]
    fn can_pay_for_spell_respects_flashback_zone_restriction() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(1),
            snow: false,
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
        };
        let gy_ctx = PaymentContext::Spell(&graveyard_flashback_spell);
        assert!(can_pay_for_spell(&pool, &cost, Some(&gy_ctx), false, 0,));

        let hand_flashback_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
            keyword_kinds: vec![crate::types::keywords::KeywordKind::Flashback],
            cast_from_zone: Some(crate::types::zones::Zone::Hand),
        };
        let hand_ctx = PaymentContext::Spell(&hand_flashback_spell);
        assert!(!can_pay_for_spell(&pool, &cost, Some(&hand_ctx), false, 0,));
    }

    #[test]
    fn can_pay_any_color_allows_wrong_color_mana() {
        // CR 609.4b: With any_color=true, green mana can pay for a white cost.
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            snow: false,
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
        assert!(can_pay_for_spell(&pool, &cost, None, true, 0));
    }

    #[test]
    fn pay_cost_any_color_spends_available_mana() {
        // CR 609.4b: pay_cost_with_demand with any_color uses available mana for colored costs.
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(1),
            snow: false,
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
}
