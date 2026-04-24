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

use crate::types::ability::{AbilityCost, TargetFilter};
use crate::types::card_type::CoreType;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use crate::types::GameState;

use super::filter::{matches_target_filter, FilterContext};

impl AbilityCost {
    /// CR 605.3a + CR 605.3b + CR 601.2h: Payability gate for ACTIVATED MANA
    /// ABILITIES specifically. Unlike [`is_payable`] (which defers mana
    /// affordability to the casting-time `ManaPayment` step per CR 601.2g),
    /// mana abilities resolve immediately and their mana sub-cost must be
    /// debited from the pool at activation — so pool affordability is checked
    /// here. All other cost kinds delegate to [`is_payable`] with no change.
    pub fn is_payable_for_mana_ability(
        &self,
        state: &GameState,
        player: PlayerId,
        source: ObjectId,
    ) -> bool {
        match self {
            AbilityCost::Mana { cost } => mana_cost_payable_from_pool(state, player, cost),
            AbilityCost::Composite { costs } => costs
                .iter()
                .all(|c| c.is_payable_for_mana_ability(state, player, source)),
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
                        .is_some_and(|o| o.zone == Zone::Battlefield);
                }
                super::casting::find_eligible_sacrifice_targets(state, player, source, target).len()
                    >= *count as usize
            }
            // CR 119.4 + CR 119.8 + CR 903.4: Life cost is payable iff life >= amount
            // and "can't lose life" locks do not apply. `amount` is a QuantityExpr
            // so dynamic refs (e.g. commander color identity count) resolve at
            // activation time against the current game state.
            AbilityCost::PayLife { amount } => {
                let resolved =
                    super::quantity::resolve_quantity(state, amount, player, source).max(0) as u32;
                super::life_costs::can_pay_life_cost(state, player, resolved)
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
            // CR 601.2b: Exile requires a choice of card from the specified zone
            // (defaulting to hand per parser convention). Self-ref exile (e.g.,
            // Scavenge: "Exile this card from your graveyard") is payable iff
            // the source is currently in the specified zone.
            AbilityCost::Exile {
                count,
                zone,
                filter,
            } => {
                let zone = zone.unwrap_or(Zone::Hand);
                if matches!(filter, Some(TargetFilter::SelfRef)) {
                    return state.objects.get(&source).is_some_and(|o| o.zone == zone);
                }
                eligible_in_zone_count(state, player, source, zone, filter.as_ref())
                    >= *count as usize
            }
            // CR 701.59b: Can't collect evidence if graveyard total mana value
            // is less than N.
            AbilityCost::CollectEvidence { amount } => {
                super::effects::collect_evidence::can_collect_evidence(state, player, *amount)
            }
            // CR 601.2b: Tapping N creatures requires N untapped creatures
            // matching the filter (excluding the source).
            AbilityCost::TapCreatures { count, filter } => {
                let ctx = FilterContext::from_source(state, source);
                state
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|&id| {
                        if id == source {
                            return false;
                        }
                        state.objects.get(&id).is_some_and(|o| {
                            o.controller == player
                                && !o.tapped
                                && matches_target_filter(state, id, filter, &ctx)
                        })
                    })
                    .count()
                    >= *count as usize
            }
            // CR 601.2b: RemoveCounter requires counters on the implied target.
            // If `target` is None, the source must have the required counters.
            // Otherwise, at least one matching permanent must carry N counters.
            AbilityCost::RemoveCounter {
                count,
                counter_type,
                target,
            } => {
                let counter_kind = crate::types::counter::parse_counter_type(counter_type);
                match target {
                    None => counter_on_object(state, source, &counter_kind) >= *count,
                    Some(tf) => {
                        let ctx = FilterContext::from_source(state, source);
                        state.battlefield.iter().any(|&id| {
                            state.objects.get(&id).is_some_and(|o| {
                                o.controller == player
                                    && matches_target_filter(state, id, tf, &ctx)
                                    && counter_on_object(state, id, &counter_kind) >= *count
                            })
                        })
                    }
                }
            }
            // CR 107.14: A player can pay {E} only if they have enough energy.
            AbilityCost::PayEnergy { amount } => state
                .players
                .get(player.0 as usize)
                .is_some_and(|p| p.energy >= *amount),
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
            // controlled by player matching filter.
            AbilityCost::ReturnToHand { count, filter } => {
                super::casting::find_eligible_return_to_hand_targets(
                    state,
                    player,
                    source,
                    filter.as_ref(),
                )
                .len()
                    >= *count as usize
            }
            // CR 701.13b: A player can mill fewer than N cards if their library
            // has fewer than N; the cost is always payable.
            AbilityCost::Mill { .. } => true,
            // CR 702.38: Exert is applied at attack declaration; the cost itself
            // is always payable — legality is enforced elsewhere in combat.
            AbilityCost::Exert => true,
            // CR 601.2b: Blight requires N creatures controlled by the player.
            AbilityCost::Blight { count } => {
                state
                    .battlefield
                    .iter()
                    .copied()
                    .filter(|&id| {
                        state.objects.get(&id).is_some_and(|o| {
                            o.controller == player
                                && o.card_types.core_types.contains(&CoreType::Creature)
                        })
                    })
                    .count()
                    >= *count as usize
            }
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
            // CR 601.2b: Every sub-cost must be payable.
            AbilityCost::Composite { costs } => {
                costs.iter().all(|c| c.is_payable(state, player, source))
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
            // CR 601.2b: Unimplemented costs are conservatively treated as payable
            // so the existing `Unimplemented` fallback paths are not further gated.
            AbilityCost::Unimplemented { .. } => true,
        }
    }
}

/// CR 601.2h + CR 605.3a: Check whether `cost` can be paid from `player`'s
/// current mana pool. This is the single authority for mana-ability mana
/// payability — auto-tap is NOT considered (mana abilities activate at
/// instant speed without chaining into other mana abilities, CR 605.3c).
fn mana_cost_payable_from_pool(state: &GameState, player: PlayerId, cost: &ManaCost) -> bool {
    let Some(p) = state.players.get(player.0 as usize) else {
        return false;
    };
    super::mana_payment::can_pay(&p.mana_pool, cost)
}

/// Count objects in `zone` controlled by `player` that match `filter`
/// (if provided), excluding `source`.
fn eligible_in_zone_count(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    zone: Zone,
    filter: Option<&TargetFilter>,
) -> usize {
    let Some(p) = state.players.get(player.0 as usize) else {
        return 0;
    };
    let ids: Box<dyn Iterator<Item = ObjectId> + '_> = match zone {
        Zone::Hand => Box::new(p.hand.iter().copied()),
        Zone::Graveyard => Box::new(p.graveyard.iter().copied()),
        Zone::Library => Box::new(p.library.iter().copied()),
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
                .count();
        }
    };
    let ctx = FilterContext::from_source(state, source);
    ids.filter(|&id| {
        id != source && filter.is_none_or(|f| matches_target_filter(state, id, f, &ctx))
    })
    .count()
}

/// Count counters of the given kind on an object.
fn counter_on_object(
    state: &GameState,
    id: ObjectId,
    kind: &crate::types::counter::CounterType,
) -> u32 {
    state
        .objects
        .get(&id)
        .map(|obj| obj.counters.get(kind).copied().unwrap_or(0))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::scenario::GameScenario;
    use crate::types::ability::{QuantityExpr, TargetFilter};
    use crate::types::mana::ManaCost;

    const P0: PlayerId = PlayerId(0);

    fn new_state() -> GameState {
        GameScenario::new().state
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
        assert!(AbilityCost::PayEnergy { amount: 3 }.is_payable(&state, P0, ObjectId(0)));
        assert!(!AbilityCost::PayEnergy { amount: 4 }.is_payable(&state, P0, ObjectId(0)));
    }

    #[test]
    fn blight_requires_creatures() {
        let mut scenario = GameScenario::new();
        // No creatures on battlefield yet.
        assert!(!AbilityCost::Blight { count: 1 }.is_payable(&scenario.state, P0, ObjectId(0)));

        let _id = scenario.add_creature(P0, "Bear", 2, 2).id();
        assert!(AbilityCost::Blight { count: 1 }.is_payable(&scenario.state, P0, ObjectId(0)));
        assert!(!AbilityCost::Blight { count: 2 }.is_payable(&scenario.state, P0, ObjectId(0)));
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
        use crate::types::ability::TypedFilter;
        let mut scenario = GameScenario::new();
        let src = scenario.add_creature(P0, "Source", 0, 1).id();
        // The source itself is excluded from its own eligible set, so with only
        // the source present, sacrifice-a-creature is unpayable.
        let cost = AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature()),
            count: 1,
        };
        assert!(!cost.is_payable(&scenario.state, P0, src));
        scenario.add_creature(P0, "Bear", 2, 2);
        assert!(cost.is_payable(&scenario.state, P0, src));
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
}
