//! CR 601.2b: Cost-payability pre-gate.
//!
//! A single predicate over `AbilityCost` that answers "can this cost be paid
//! right now, given the current game state?" for cost variants where CR 601.2b
//! applies — specifically, costs that require the player to *choose an object*
//! and where no legal object exists.
//!
//! This is the authoritative gate consulted before:
//!   - Offering an `OptionalCostChoice` prompt (if unpayable, the prompt is skipped).
//!   - Paying a `Required` additional cost (if unpayable, the spell cannot be cast).
//!   - Falling through an `AdditionalCost::Choice(A, B)` when A is unpayable.
//!   - Activating an ability whose cost requires a choice-of-object.
//!
//! The predicate is pure: it reads `&GameState` and never mutates. Delegate to
//! existing eligibility helpers in sibling modules rather than reimplementing
//! the enumerations.

use crate::types::ability::{
    AbilityCost, Comparator, FilterProp, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use crate::types::GameState;

use super::filter::{matches_target_filter, matches_target_filter_in_owner_zone, FilterContext};

fn is_pitch_bound_cmc_eq_x_prop(prop: &FilterProp) -> bool {
    matches!(
        prop,
        FilterProp::Cmc {
            comparator: Comparator::EQ,
            value: QuantityExpr::Ref {
                qty: QuantityRef::Variable { name },
            },
        } if name == "X"
    )
}

/// True when a cost filter uses the Shoal pattern: "with mana value X" where X
/// is defined by the card chosen to pay the cost, not by a prior announcement.
pub(crate) fn target_filter_has_pitch_bound_x(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf.properties.iter().any(is_pitch_bound_cmc_eq_x_prop),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_has_pitch_bound_x)
        }
        TargetFilter::Not { filter } | TargetFilter::TrackedSetFiltered { filter, .. } => {
            target_filter_has_pitch_bound_x(filter)
        }
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
}

pub(crate) fn relax_pitch_bound_x_filter(filter: &TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(TypedFilter {
            properties: tf
                .properties
                .iter()
                .filter(|p| !is_pitch_bound_cmc_eq_x_prop(p))
                .cloned()
                .collect(),
            ..tf.clone()
        }),
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.iter().map(relax_pitch_bound_x_filter).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.iter().map(relax_pitch_bound_x_filter).collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(relax_pitch_bound_x_filter(filter)),
        },
        TargetFilter::TrackedSetFiltered { id, filter } => TargetFilter::TrackedSetFiltered {
            id: *id,
            filter: Box::new(relax_pitch_bound_x_filter(filter)),
        },
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => filter.clone(),
    }
}

/// CR 107.3a + CR 118.9: Until the player chooses the pitched card, relax the
/// CMC=X constraint for 601.2b eligibility on Shoal-style exile costs.
pub(crate) fn exile_cost_effective_filter(filter: Option<&TargetFilter>) -> Option<TargetFilter> {
    filter.map(|f| {
        if target_filter_has_pitch_bound_x(f) {
            relax_pitch_bound_x_filter(f)
        } else {
            f.clone()
        }
    })
}

impl AbilityCost {
    /// CR 605.3a + CR 602.2b + CR 601.2g-h: Payability gate for ACTIVATED
    /// MANA ABILITIES specifically. Unlike [`is_payable`] (which defers mana
    /// affordability to the normal spell/ability payment step), a mana ability
    /// resolves immediately after its activation cost is paid. If that cost
    /// includes mana, CR 117.1d and CR 118.2 still allow activating other mana
    /// abilities while paying it, so affordability is checked through the
    /// activation mana-payment building block rather than requiring mana to
    /// already be floating.
    pub fn is_payable_for_mana_ability(
        &self,
        state: &GameState,
        player: PlayerId,
        source: ObjectId,
    ) -> bool {
        match self {
            AbilityCost::Mana { cost } => {
                let excluded_sources = std::collections::HashSet::from([source]);
                super::casting::can_pay_ability_mana_cost_after_auto_tap_excluding(
                    state,
                    player,
                    source,
                    cost,
                    &excluded_sources,
                )
            }
            // Same {T}+TapCreatures source-exclusion logic as `is_payable`'s
            // Composite arm, but Mana sub-costs use the mana-specific check.
            AbilityCost::Composite { costs } => {
                let has_tap = costs.iter().any(|c| matches!(c, AbilityCost::Tap));
                costs.iter().all(|c| match c {
                    AbilityCost::TapCreatures { count, filter } if has_tap => {
                        has_enough_tap_creatures(state, player, source, *count, filter, true)
                    }
                    other => other.is_payable_for_mana_ability(state, player, source),
                })
            }
            // Every other kind has no mana-pool component — defer to the
            // generic 601.2b gate, which already handles it correctly.
            other => other.is_payable(state, player, source),
        }
    }

    /// CR 601.2b: Returns true if this cost can be paid given the current game
    /// state. Returns false only when the cost requires a choice of object and
    /// no legal object exists, or a hard resource check fails (e.g., life total).
    ///
    /// Mana affordability is NOT checked here; CR 601.2g handles the mana step
    /// separately through the mana-payment flow.
    pub fn is_payable(&self, state: &GameState, player: PlayerId, source: ObjectId) -> bool {
        match self {
            // CR 601.2g: Mana affordability is checked by the mana payment step,
            // not the 601.2b choice-of-object gate.
            AbilityCost::Mana { .. } => true,
            // CR 118.3: Tap/Untap availability is enforced at payment time
            // (the object must be in the correct state). This gate only concerns
            // choice-of-object eligibility.
            AbilityCost::Tap | AbilityCost::Untap => true,
            // CR 606.4: Positive loyalty is always payable. Negative loyalty
            // requires at least |amount| loyalty counters currently on source.
            AbilityCost::Loyalty { amount } => {
                if *amount >= 0 {
                    true
                } else {
                    let current = state
                        .objects
                        .get(&source)
                        .and_then(|o| o.loyalty)
                        .unwrap_or(0);
                    current as i32 >= -*amount
                }
            }
            // CR 601.2b: Sacrifice requires a choice of permanent; self-sacrifice
            // is always payable so long as the source exists on the battlefield.
            AbilityCost::Sacrifice { target, count } => {
                if matches!(target, TargetFilter::SelfRef) {
                    return state
                        .objects
                        .get(&source)
                        .is_some_and(|o| o.zone == Zone::Battlefield)
                        && !super::static_abilities::player_cant_sacrifice_as_cost(
                            state, player, source,
                        );
                }
                let eligible =
                    super::casting::find_eligible_sacrifice_targets(state, player, source, target);
                let (min_count, _) = super::casting::sacrifice_cost_bounds(*count, eligible.len());
                eligible.len() >= min_count
            }
            // CR 119.4 + CR 119.8 + CR 903.4: Life cost is payable iff life >= amount
            // and "can't lose life" locks do not apply. `amount` is a QuantityExpr
            // so dynamic refs (e.g. commander color identity count) resolve at
            // activation time against the current game state.
            AbilityCost::PayLife { amount } => {
                let resolved =
                    super::quantity::resolve_quantity(state, amount, player, source).max(0) as u32;
                super::life_costs::can_pay_life_cast_or_activation_cost(state, player, resolved)
            }
            // CR 601.2b: Discard requires a choice of card from hand.
            // For `self_ref`, the source card itself must still be in hand.
            AbilityCost::Discard {
                count,
                filter,
                self_ref,
                ..
            } => {
                let Some(p) = state.players.get(player.0 as usize) else {
                    return false;
                };
                if *self_ref {
                    return p.hand.contains(&source);
                }
                let resolved =
                    super::quantity::resolve_quantity(state, count, player, source).max(0) as usize;
                let ctx = FilterContext::from_source(state, source);
                p.hand
                    .iter()
                    .filter(|&&id| {
                        id != source
                            && filter
                                .as_ref()
                                .is_none_or(|f| matches_target_filter(state, id, f, &ctx))
                    })
                    .count()
                    >= resolved
            }
            // CR 601.2b: Exile requires a choice of card from the specified zone.
            // Self-ref exile (e.g., Scavenge: "Exile this card from your
            // graveyard") is payable iff the source is currently in the
            // specified zone. For non-self exile costs, when the parser emits
            // `zone: None` because the filter implies battlefield permanents
            // (CR 117.1: "Exile a creature you control" — Food Chain class),
            // default to `Battlefield`. Otherwise default to `Hand` per the
            // legacy parser convention for non-typed-permanent exile costs.
            AbilityCost::Exile {
                count,
                zone,
                filter,
            } => {
                if matches!(filter, Some(TargetFilter::SelfRef)) {
                    // CR 118.3 + CR 602.1a: "Exile this <self>" as an
                    // activation cost needs the source available to pay that
                    // cost. An explicit zone ("from your graveyard/hand")
                    // gates payability on that zone; a missing zone means the
                    // source's current zone — the ability is only active where
                    // the source functions (e.g. a land's "Exile this land"
                    // is paid from the battlefield), NOT the hand.
                    return match zone {
                        Some(z) => state.objects.get(&source).is_some_and(|o| o.zone == *z),
                        None => state.objects.contains_key(&source),
                    };
                }
                let zone = exile_cost_effective_zone(*zone, filter.as_ref());
                let effective_filter = exile_cost_effective_filter(filter.as_ref());
                eligible_exile_cost_objects(
                    state,
                    player,
                    source,
                    zone,
                    effective_filter.as_ref(),
                    *count,
                )
                .len()
                    >= *count as usize
            }
            // CR 702.167a/b: Craft's materials cost — payable iff enough
            // eligible objects exist across the battlefield/graveyard union
            // (excluding the source, whose self-exile is a separate cost).
            AbilityCost::ExileMaterials { materials, count } => {
                eligible_craft_materials(state, player, source, materials).len()
                    >= count.min_count()
            }
            // CR 701.59b: Can't collect evidence if graveyard total mana value
            // is less than N.
            AbilityCost::CollectEvidence { amount } => {
                super::effects::collect_evidence::can_collect_evidence(state, player, *amount)
            }
            // CR 601.2b: Tapping N creatures requires N untapped creatures
            // matching the filter. The source is excluded only when a {T} cost
            // is also present (handled by the Composite arm); otherwise the
            // source is a valid choice (e.g. Morcant's "Tap three untapped
            // Elves" has no {T}, so Morcant herself is eligible).
            AbilityCost::TapCreatures { count, filter } => {
                has_enough_tap_creatures(state, player, source, *count, filter, false)
            }
            // CR 601.2b: RemoveCounter requires counters on the implied target.
            // If `target` is None, the source must have the required counters.
            // Otherwise, at least one matching permanent must carry N counters.
            // CR 107.2: `u32::MAX` encodes "any number of" — the player chooses
            // how many counters to remove (including zero), so the cost is always
            // payable regardless of the current counter count.
            AbilityCost::RemoveCounter {
                count,
                counter_type,
                target,
            } => {
                if *count == u32::MAX {
                    return true;
                }
                match target {
                    None => counter_on_object(state, source, counter_type) >= *count,
                    Some(tf) => {
                        let ctx = FilterContext::from_source(state, source);
                        state.battlefield.iter().any(|&id| {
                            state.objects.get(&id).is_some_and(|o| {
                                o.controller == player
                                    && matches_target_filter(state, id, tf, &ctx)
                                    && counter_on_object(state, id, counter_type) >= *count
                            })
                        })
                    }
                }
            }
            // CR 107.14: A player can pay {E} only if they have enough energy.
            // CR 107.3c: Resolve the `QuantityExpr` so dynamic amounts read game
            // state. `Variable("X")` resolves to 0 — always payable, which
            // triggers the variable-payment flow (mirrors `PaySpeed` below).
            AbilityCost::PayEnergy { amount } => {
                let resolved =
                    super::quantity::resolve_quantity(state, amount, player, source).max(0);
                state
                    .players
                    .get(player.0 as usize)
                    .is_some_and(|p| (p.energy as i64) >= resolved as i64)
            }
            // CR 702.179f: Pay-speed resolves the quantity, then checks against
            // current speed. `QuantityExpr::Ref(Variable)` resolves to 0, which
            // is always payable and triggers the variable-payment flow.
            AbilityCost::PaySpeed { amount } => {
                let resolved =
                    super::quantity::resolve_quantity(state, amount, player, source).max(0);
                let current = super::speed::effective_speed(state, player) as i32;
                resolved <= current
            }
            // CR 601.2b: Returning N permanents to hand requires N permanents
            // controlled by player matching filter. The `from_zone` axis is
            // only consumed by the unless-payment path, never by activation
            // costs — the standard battlefield-source check is correct here.
            AbilityCost::ReturnToHand {
                count,
                filter,
                from_zone: _,
            } => {
                super::casting::find_eligible_return_to_hand_targets(
                    state,
                    player,
                    source,
                    filter.as_ref(),
                )
                .len()
                    >= *count as usize
            }
            // CR 701.3d: An explicit unattach cost is payable only while the
            // source is an attached battlefield permanent controlled by player.
            AbilityCost::Unattach => state.objects.get(&source).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.controller == player
                    && obj
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == "Equipment")
                    && obj.attached_to.is_some()
            }),
            // CR 701.13b: A player can mill fewer than N cards if their library
            // has fewer than N; the cost is always payable.
            AbilityCost::Mill { .. } => true,
            // CR 701.43b: A permanent can be exerted even if it's not tapped
            // or has already been exerted; the cost itself is always payable.
            // CR 701.43c (off-battlefield) is enforced at payment time.
            AbilityCost::Exert => true,
            // CR 701.68b: Blight is payable iff the player controls >=1 creature.
            // N (the number of -1/-1 counters) is irrelevant to eligibility — the
            // counters all go on the one chosen creature.
            AbilityCost::Blight { .. } => state.battlefield.iter().copied().any(|id| {
                state.objects.get(&id).is_some_and(|o| {
                    o.controller == player && o.card_types.core_types.contains(&CoreType::Creature)
                })
            }),
            // CR 601.2b: Reveal N matching cards requires them to exist in hand.
            // Filter-less reveal (self-reveal) is always payable — you can always
            // reveal the source spell you're casting.
            AbilityCost::Reveal { count, filter } => {
                let Some(p) = state.players.get(player.0 as usize) else {
                    return false;
                };
                match filter {
                    None => true,
                    Some(f) => {
                        let ctx = FilterContext::from_source(state, source);
                        p.hand
                            .iter()
                            .filter(|&&id| matches_target_filter(state, id, f, &ctx))
                            .count()
                            >= *count as usize
                    }
                }
            }
            AbilityCost::Behold { count, filter, .. } => {
                super::casting_costs::eligible_behold_choices(state, player, source, filter).len()
                    >= *count as usize
            }
            // CR 601.2b: Every sub-cost must be payable. When the composite
            // includes {T}, the source is committed to the tap cost and must be
            // excluded from any TapCreatures eligibility count — it will be
            // tapped before TapCreatures is paid.
            AbilityCost::Composite { costs } => {
                let has_tap = costs.iter().any(|c| matches!(c, AbilityCost::Tap));
                costs.iter().all(|c| match c {
                    AbilityCost::TapCreatures { count, filter } if has_tap => {
                        has_enough_tap_creatures(state, player, source, *count, filter, true)
                    }
                    other => other.is_payable(state, player, source),
                })
            }
            // CR 118.12a: Disjunctive — payable if **any** sub-cost is
            // payable. The interactive choice is surfaced at resolution via
            // `WaitingFor::UnlessPaymentChooseCost`; the activation-time
            // gate only needs at least one branch to be reachable.
            AbilityCost::OneOf { costs } => {
                costs.iter().any(|c| c.is_payable(state, player, source))
            }
            // CR 601.2b: Waterbend composes a mana cost with a tap-creature option.
            // Affordability is checked via the standard auto-tap pre-check.
            AbilityCost::Waterbend { cost } => {
                super::casting::can_pay_cost_after_auto_tap(state, player, source, cost)
            }
            // CR 702.49: Ninjutsu requires at least one returnable creature for
            // the variant. Mana affordability is deferred to payment (per CR 601.2g).
            AbilityCost::NinjutsuFamily { variant, .. } => {
                !super::keywords::returnable_creatures_for_variant(state, player, variant)
                    .is_empty()
            }
            // CR 118.3: Effect-as-cost is conservatively treated as payable.
            // Runtime resolution determines actual outcome.
            AbilityCost::EffectCost { .. } => true,
            // CR 601.2b: Unimplemented costs are conservatively treated as payable
            // so the existing `Unimplemented` fallback paths are not further gated.
            AbilityCost::Unimplemented { .. } => true,
            // CR 118.4 + CR 107.3c: Dynamic-generic mana primarily appears in
            // unless-pay contexts. The activation-time payability check
            // resolves the quantity to a fixed amount and treats the cost as
            // mana — same as `AbilityCost::Mana { .. }` (whose mana
            // affordability is delegated to CR 601.2g per the comment above).
            AbilityCost::ManaDynamic { .. } => true,
            // CR 702.24a: `PerCounter` is used today only in unless-payment
            // flows (cumulative upkeep), where Task 6's runtime expansion
            // resolves the multiplier against game state and re-runs payability
            // on the expanded base. Treat as conservatively payable here so
            // the activation-time 601.2b gate doesn't reject the wrapper
            // unseen — actual payability is decided post-expansion.
            AbilityCost::PerCounter { .. } => true,
        }
    }
}

fn has_enough_tap_creatures(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    count: u32,
    filter: &TargetFilter,
    exclude_source: bool,
) -> bool {
    let ctx = FilterContext::from_source(state, source);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            if exclude_source && id == source {
                return false;
            }
            state.objects.get(&id).is_some_and(|o| {
                o.controller == player
                    && !o.tapped
                    && matches_target_filter(state, id, filter, &ctx)
            })
        })
        .count()
        >= count as usize
}

/// CR 117.1 + CR 118.3: Infer the source zone for a non-self
/// `AbilityCost::Exile`. Explicit zones are authoritative. A missing zone
/// keeps the existing parser convention: permanent-implying filters mean
/// battlefield, otherwise hand.
pub(super) fn exile_cost_effective_zone(zone: Option<Zone>, filter: Option<&TargetFilter>) -> Zone {
    zone.unwrap_or_else(|| {
        if filter.is_some_and(filter_implies_battlefield_permanent) {
            Zone::Battlefield
        } else {
            Zone::Hand
        }
    })
}

/// CR 117.1 + CR 118.3: Objects in `zone` controlled/owned by `player` that
/// can be exiled to pay a non-self `AbilityCost::Exile`, excluding `source`.
///
/// `Zone::Library` is deterministic top-of-library payment, not a choice. Only
/// the top `count` cards are eligible, and filtered library exile costs are not
/// surfaced because the existing AST shape represents "exile the top N cards".
pub(super) fn eligible_exile_cost_objects(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    zone: Zone,
    filter: Option<&TargetFilter>,
    count: u32,
) -> Vec<ObjectId> {
    let Some(p) = state.players.get(player.0 as usize) else {
        return Vec::new();
    };
    let ids: Box<dyn Iterator<Item = ObjectId> + '_> = match zone {
        Zone::Hand => Box::new(p.hand.iter().copied()),
        Zone::Graveyard => Box::new(p.graveyard.iter().copied()),
        Zone::Library => {
            if filter.is_some() {
                return Vec::new();
            }
            return p
                .library
                .iter()
                .copied()
                .filter(|id| *id != source)
                .take(count as usize)
                .collect();
        }
        // Battlefield exile/etc. — fall back to iterating the object set by zone.
        _ => {
            let ctx = FilterContext::from_source(state, source);
            return state
                .objects
                .values()
                .filter(|o| {
                    o.zone == zone
                        && o.controller == player
                        && o.id != source
                        && filter.is_none_or(|f| matches_target_filter(state, o.id, f, &ctx))
                })
                .map(|o| o.id)
                .collect();
        }
    };
    let effective_filter = exile_cost_effective_filter(filter);
    let filter_ref = effective_filter.as_ref();
    let ctx = FilterContext::from_source(state, source);
    ids.filter(|&id| {
        id != source
            && filter_ref.is_none_or(|f| matches_target_filter_in_owner_zone(state, id, f, &ctx))
    })
    .collect()
}

/// CR 702.167a/b: Objects eligible to be exiled as the materials of a craft
/// ability — the union of (a) permanents on the battlefield the player controls
/// and (b) cards in the player's graveyard, in both cases matching `materials`
/// and excluding `source` (whose self-exile is a separate cost component;
/// excluding it is required for "craft with artifact" on an artifact source).
///
/// `materials` is the dual-zone `TargetFilter::Or` produced by
/// `craft_materials_filter`; the battlefield leg is evaluated with the normal
/// filter evaluator while the graveyard leg uses the owner-zone evaluator so
/// `InZone`/`Owned` predicates resolve against non-battlefield cards. Returns
/// every eligible object; the caller enforces the materials count via
/// `len() >= count`.
pub(crate) fn eligible_craft_materials(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    materials: &TargetFilter,
) -> Vec<ObjectId> {
    let ctx = FilterContext::from_source(state, source);
    let mut out: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            id != source
                && state
                    .objects
                    .get(&id)
                    .is_some_and(|o| o.controller == player)
                && matches_target_filter(state, id, materials, &ctx)
        })
        .collect();
    if let Some(p) = state.players.get(player.0 as usize) {
        out.extend(p.graveyard.iter().copied().filter(|&id| {
            id != source && matches_target_filter_in_owner_zone(state, id, materials, &ctx)
        }));
    }
    out
}

/// Count counters of the given kind on an object.
/// CR 117.1 + CR 400.6: Decide whether a `TargetFilter` for an `AbilityCost::Exile`
/// without an explicit `zone` implies the battlefield. True when the filter has
/// any `CoreType` typed predicate that names a permanent type (Creature, Artifact,
/// Enchantment, Planeswalker, Land, Battle, Tribal). False for plain "card",
/// "spell", or zone-explicit filters — those keep the legacy hand default.
///
/// Used by Food Chain's "Exile a creature you control: ..." (`zone: None`,
/// `filter: Typed{Creature, You}`) and the broader exile-permanent-cost class.
fn filter_implies_battlefield_permanent(filter: &TargetFilter) -> bool {
    use crate::types::ability::TypeFilter;
    fn type_implies_battlefield(t: &TypeFilter) -> bool {
        match t {
            TypeFilter::Creature
            | TypeFilter::Artifact
            | TypeFilter::Enchantment
            | TypeFilter::Planeswalker
            | TypeFilter::Land
            | TypeFilter::Battle
            | TypeFilter::Permanent => true,
            TypeFilter::Non(inner) => type_implies_battlefield(inner),
            TypeFilter::AnyOf(inners) => inners.iter().any(type_implies_battlefield),
            _ => false,
        }
    }
    match filter {
        TargetFilter::Typed(tf) => tf.type_filters.iter().any(type_implies_battlefield),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_implies_battlefield_permanent)
        }
        _ => false,
    }
}

/// CR 122.1 + CR 118.3: Count counters on `id` matching `kind`. `Any` sums
/// across every counter type currently on the object (Loch Mare's untyped
/// "remove a counter" cost — CR 118.3: the ability is payable iff the object
/// has at least one counter of any kind); `OfType(t)` reads the specific
/// entry. CR 122.1: counters of the same kind are interchangeable.
fn counter_on_object(
    state: &GameState,
    id: ObjectId,
    kind: &crate::types::counter::CounterMatch,
) -> u32 {
    let Some(obj) = state.objects.get(&id) else {
        return 0;
    };
    match kind {
        crate::types::counter::CounterMatch::Any => obj.counters.values().copied().sum(),
        crate::types::counter::CounterMatch::OfType(t) => obj.counters.get(t).copied().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{
        ControllerRef, FilterProp, QuantityExpr, TargetFilter, TypeFilter, TypedFilter,
    };
    use crate::types::mana::ManaCost;

    const P0: PlayerId = PlayerId(0);

    fn new_state() -> GameState {
        GameScenario::new().state
    }

    fn mark_elf(state: &mut GameState, id: ObjectId) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .subtypes
            .push("Elf".to_string());
    }

    fn elf_filter() -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::creature()
                .with_type(TypeFilter::Subtype("Elf".to_string()))
                .controller(ControllerRef::You),
        )
    }

    #[test]
    fn mana_cost_always_payable_at_this_layer() {
        let state = new_state();
        let cost = AbilityCost::Mana {
            cost: ManaCost::NoCost,
        };
        assert!(cost.is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn tap_untap_always_payable() {
        let state = new_state();
        assert!(AbilityCost::Tap.is_payable(&state, P0, ObjectId(0)));
        assert!(AbilityCost::Untap.is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn pay_life_requires_sufficient_life() {
        let mut state = new_state();
        state.players[0].life = 5;
        assert!(AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 5 }
        }
        .is_payable(&state, P0, ObjectId(0)));
        assert!(!AbilityCost::PayLife {
            amount: QuantityExpr::Fixed { value: 6 }
        }
        .is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn pay_energy_requires_sufficient_energy() {
        let mut state = new_state();
        state.players[0].energy = 3;
        assert!(AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 3 }
        }
        .is_payable(&state, P0, ObjectId(0)));
        assert!(!AbilityCost::PayEnergy {
            amount: QuantityExpr::Fixed { value: 4 }
        }
        .is_payable(&state, P0, ObjectId(0)));
    }

    /// CR 118.3 + CR 602.1a: a self-exile cost with no explicit zone ("Exile
    /// this land") is paid from the source's current zone — the battlefield —
    /// not the hand. This previously defaulted to `Zone::Hand`, so a
    /// permanent's "Exile this <self>" activated-ability cost was wrongly
    /// reported unpayable from play.
    #[test]
    fn self_exile_cost_without_zone_payable_from_battlefield() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Ominous Cemetery", 0, 0).id();
        let self_exile = AbilityCost::Exile {
            count: 1,
            zone: None,
            filter: Some(TargetFilter::SelfRef),
        };
        assert!(
            self_exile.is_payable(&scenario.state, P0, src),
            "self-exile cost with no zone must be payable from the battlefield"
        );
        // Within the Ominous Cemetery composite ({5}, {T}, Exile this land) the
        // exile component stays payable.
        assert!(AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, self_exile],
        }
        .is_payable(&scenario.state, P0, src));
        // An EXPLICIT zone still gates: a battlefield source cannot pay a
        // "from your graveyard" self-exile cost (Scavenge class).
        assert!(!AbilityCost::Exile {
            count: 1,
            zone: Some(Zone::Graveyard),
            filter: Some(TargetFilter::SelfRef),
        }
        .is_payable(&scenario.state, P0, src));
    }

    /// CR 601.2b: Standalone TapCreatures (no {T}) includes the source itself
    /// in the eligible count. Morcant shape: "Tap three untapped Elves you control"
    /// — the card itself counts as one of the three.
    #[test]
    fn tap_creatures_standalone_includes_source() {
        let mut scenario = GameScenario::new();
        let cost = AbilityCost::TapCreatures {
            count: 3,
            filter: elf_filter(),
        };
        // Place exactly 3 Elves controlled by P0 — including the source.
        let src = scenario.add_creature(P0, "Morcant", 4, 4).id();
        mark_elf(&mut scenario.state, src);
        let elf_a = scenario.add_creature(P0, "Elf A", 1, 1).id();
        mark_elf(&mut scenario.state, elf_a);
        let elf_b = scenario.add_creature(P0, "Elf B", 1, 1).id();
        mark_elf(&mut scenario.state, elf_b);
        // 3 Elves total including source → payable.
        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "source counts among the 3 Elves"
        );
        // With 2 OTHER Elves + source, must still be payable (source is the 3rd).
        // Remove elf_b — now only source + elf_a = 2 Elves → unpayable.
        scenario.state.battlefield.retain(|id| *id != elf_b);
        scenario.state.objects.remove(&elf_b);
        assert!(
            !cost.is_payable(&scenario.state, P0, src),
            "only 2 Elves (source + elf_a) < 3"
        );
    }

    /// CR 601.2b: Composite({T}, TapCreatures) still excludes the source from
    /// TapCreatures eligibility — source is committed to {T}.
    #[test]
    fn tap_creatures_composite_with_tap_excludes_source() {
        let mut scenario = GameScenario::new();
        let cost = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::TapCreatures {
                    count: 2,
                    filter: elf_filter(),
                },
            ],
        };
        let src = scenario.add_creature(P0, "Lathril", 2, 2).id();
        mark_elf(&mut scenario.state, src);
        let elf_a = scenario.add_creature(P0, "Elf A", 1, 1).id();
        mark_elf(&mut scenario.state, elf_a);
        let elf_b = scenario.add_creature(P0, "Elf B", 1, 1).id();
        mark_elf(&mut scenario.state, elf_b);
        // Source committed to {T} — 2 OTHER Elves available → payable.
        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "2 other Elves satisfy TapCreatures(2)"
        );
        // Remove elf_b — only 1 other Elf → unpayable.
        scenario.state.battlefield.retain(|id| *id != elf_b);
        scenario.state.objects.remove(&elf_b);
        assert!(
            !cost.is_payable(&scenario.state, P0, src),
            "only 1 other Elf < 2"
        );
    }

    #[test]
    fn blight_requires_creatures() {
        let mut scenario = GameScenario::new();
        // No creatures on battlefield yet.
        assert!(!AbilityCost::Blight { count: 1 }.is_payable(&scenario.state, P0, ObjectId(0)));

        let _id = scenario.add_creature(P0, "Bear", 2, 2).id();
        assert!(AbilityCost::Blight { count: 1 }.is_payable(&scenario.state, P0, ObjectId(0)));
        // CR 701.68b: N > 1 no longer requires N creatures — all N counters go on
        // the single chosen creature, so one controlled creature suffices.
        assert!(AbilityCost::Blight { count: 2 }.is_payable(&scenario.state, P0, ObjectId(0)));
        assert!(AbilityCost::Blight { count: 3 }.is_payable(&scenario.state, P0, ObjectId(0)));
    }

    #[test]
    fn discard_requires_cards_in_hand() {
        let mut state = new_state();
        state.players[0].hand.clear();
        assert!(!AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: false,
            self_ref: false,
        }
        .is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn sacrifice_self_ref_requires_battlefield() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Bear", 2, 2).id();
        let cost = AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef,
            count: 1,
        };
        assert!(cost.is_payable(&scenario.state, P0, src));
        // Move source off battlefield.
        scenario.state.objects.get_mut(&src).unwrap().zone = Zone::Graveyard;
        assert!(!cost.is_payable(&scenario.state, P0, src));
    }

    #[test]
    fn sacrifice_non_self_requires_eligible_permanent() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Source", 0, 1).id();
        let cost = AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: 1,
        };
        assert!(cost.is_payable(&scenario.state, P0, src));

        let another_cost = AbilityCost::Sacrifice {
            target: TargetFilter::Typed(
                TypedFilter::creature().properties(vec![FilterProp::Another]),
            ),
            count: 1,
        };
        assert!(!another_cost.is_payable(&scenario.state, P0, src));

        scenario.add_creature(P0, "Bear", 2, 2);
        assert!(cost.is_payable(&scenario.state, P0, src));
        assert!(another_cost.is_payable(&scenario.state, P0, src));
    }

    #[test]
    fn variable_sacrifice_cost_is_payable_with_zero_or_more_matches() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Chatterfang", 3, 3).id();
        let cost = AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype("Squirrel".into()))),
            count: u32::MAX,
        };

        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "X sacrifice costs should be payable at X=0 even with no eligible permanents"
        );

        scenario
            .add_creature(P0, "Squirrel Token", 1, 1)
            .with_subtypes(vec!["Squirrel"]);
        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "X sacrifice costs should stay payable once eligible permanents exist"
        );
    }

    #[test]
    fn loyalty_positive_is_always_payable() {
        let state = new_state();
        assert!(AbilityCost::Loyalty { amount: 1 }.is_payable(&state, P0, ObjectId(0)));
        assert!(AbilityCost::Loyalty { amount: 0 }.is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn loyalty_negative_requires_counters() {
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "PW", 0, 0).id();
        scenario.state.objects.get_mut(&src).unwrap().loyalty = Some(3);
        assert!(AbilityCost::Loyalty { amount: -3 }.is_payable(&scenario.state, P0, src));
        assert!(!AbilityCost::Loyalty { amount: -4 }.is_payable(&scenario.state, P0, src));
    }

    #[test]
    fn composite_all_must_be_payable() {
        let mut state = new_state();
        state.players[0].life = 3;
        let payable = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                },
            ],
        };
        assert!(payable.is_payable(&state, P0, ObjectId(0)));
        let unpayable = AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 10 },
                },
            ],
        };
        assert!(!unpayable.is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn mill_exert_always_payable() {
        let state = new_state();
        assert!(AbilityCost::Mill { count: 5 }.is_payable(&state, P0, ObjectId(0)));
        assert!(AbilityCost::Exert.is_payable(&state, P0, ObjectId(0)));
    }

    /// CR 118.3: #542 Loch Mare — `RemoveCounter` with `CounterMatch::Any`
    /// (the untyped "remove a counter" form) must be payable iff the object
    /// has at least one counter of ANY type. Without summing across kinds,
    /// Loch Mare's `{1}{U}, Remove a counter from ~: Draw a card.` reads zero
    /// counters and the activation is forever grayed out.
    #[test]
    fn remove_counter_untyped_any_sums_all_kinds() {
        use crate::types::counter::CounterType;
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Loch Mare", 0, 0).id();
        scenario
            .state
            .objects
            .get_mut(&src)
            .unwrap()
            .counters
            .insert(CounterType::Minus1Minus1, 3);
        let cost = AbilityCost::RemoveCounter {
            count: 1,
            counter_type: crate::types::counter::CounterMatch::Any,
            target: None,
        };
        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "untyped 'remove a counter' must be payable when the object has -1/-1 counters",
        );

        // With no counters at all, the cost must be unpayable.
        scenario
            .state
            .objects
            .get_mut(&src)
            .unwrap()
            .counters
            .clear();
        assert!(
            !cost.is_payable(&scenario.state, P0, src),
            "untyped 'remove a counter' must be unpayable when no counters of any kind are present",
        );
    }

    /// CR 107.2: "Remove any number of" counters is always payable — the
    /// player may choose zero, so no minimum counter count is required.
    #[test]
    fn remove_counter_any_number_always_payable() {
        use crate::types::counter::CounterType;
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Mage-Ring Network", 0, 0).id();
        let cost = AbilityCost::RemoveCounter {
            count: u32::MAX,
            counter_type: crate::types::counter::CounterMatch::OfType(CounterType::Generic(
                "storage".to_string(),
            )),
            target: None,
        };
        // Payable even with zero counters.
        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "'remove any number of' must be payable even with zero counters",
        );
        // Still payable with some counters.
        scenario
            .state
            .objects
            .get_mut(&src)
            .unwrap()
            .counters
            .insert(CounterType::Generic("storage".to_string()), 3);
        assert!(
            cost.is_payable(&scenario.state, P0, src),
            "'remove any number of' must be payable with counters present",
        );
    }

    /// Issue #2372 — Nourishing Shoal: CMC=X is defined by the pitched card, so
    /// payability must not require a pre-announced X.
    #[test]
    fn shoal_pitch_exile_cost_payable_with_any_green_hand_card() {
        use crate::game::zones::create_object;
        use crate::parser::oracle_cost::parse_oracle_cost;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::mana::{ManaColor, ManaCost};

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let shoal = create_object(
            &mut state,
            CardId(700),
            caster,
            "Nourishing Shoal".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&shoal).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![
                    crate::types::mana::ManaCostShard::X,
                    crate::types::mana::ManaCostShard::Green,
                    crate::types::mana::ManaCostShard::Green,
                ],
                generic: 0,
            };
        }

        let green_two_drop = create_object(
            &mut state,
            CardId(701),
            caster,
            "Green Two Drop".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&green_two_drop).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.color.push(ManaColor::Green);
            obj.mana_cost = ManaCost::generic(2);
        }

        let cost = parse_oracle_cost("exile a green card with mana value X from your hand");
        assert!(
            cost.is_payable(&state, caster, shoal),
            "Shoal pitch cost must be payable when any green card can set X"
        );

        let AbilityCost::Exile { filter, .. } = cost else {
            panic!("expected Exile cost");
        };
        let eligible = super::eligible_exile_cost_objects(
            &state,
            caster,
            shoal,
            Zone::Hand,
            filter.as_ref(),
            1,
        );
        assert!(
            eligible.contains(&green_two_drop),
            "green hand card must be eligible regardless of CMC before X is chosen: {eligible:?}"
        );
        assert!(
            !eligible.contains(&shoal),
            "cast source must be excluded from pitch eligibility"
        );
    }
}
