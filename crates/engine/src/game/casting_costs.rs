use std::collections::HashSet;

use crate::types::ability::{
    is_chosen_remove_counter_cost_count, AbilityCondition, AbilityCost, AbilityDefinition,
    AbilityKind, AdditionalCost, AdditionalCostInstance, AdditionalCostOrigin, BeholdCostAction,
    CastTimingPermission, CostPaidObjectSnapshot, CounterCostSelection, Effect, KickerVariant,
    QuantityExpr, QuantityRef, ReplacementDefinition, ResolvedAbility, SacrificeCost,
    SacrificeRequirement, SpellCastingOptionKind, StaticCondition, TargetFilter, TypeFilter,
    TypedFilter, EXILE_COST_X,
};
use crate::types::events::{GameEvent, ManaTapState};
use crate::types::game_state::{
    AssistState, CastPaymentMode, CastingVariant, ConvokeMode, CostResume, CounterCostChoice,
    DistributionUnit, GameState, PayCostKind, PendingCast, PendingDiscardForCostResume,
    SpellCostSource, StackEntry, StackEntryKind, StackPaidSnapshot, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaCost, ManaCostShard, ManaType, PaymentContext};
use crate::types::player::PlayerId;
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::{CostModifyMode, StaticMode};
use crate::types::zones::{ExileCostSourceZone, Zone};

use super::casting::emit_targeting_events;
use super::effects::counters::add_counter_with_replacement;
use super::engine::EngineError;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources::{self, ManaSourceOption};
use super::restrictions;
use super::stack;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets_for_ability, begin_target_selection_for_ability,
    build_target_slots, build_target_slots_labelled, flatten_targets_in_chain,
    modal_choice_for_player, random_select_targets_for_ability, target_constraints_from_modal,
};
use super::life_costs::PayLifeCostResult;

fn stamp_controller_controlled_as_cast(
    state: &GameState,
    ability: &mut ResolvedAbility,
    player: PlayerId,
    source_id: ObjectId,
) {
    let mut filters = Vec::new();
    collect_controller_controlled_as_cast_filters(ability, &mut filters);
    let mut unique_filters = Vec::new();
    for filter in filters {
        if !unique_filters.contains(&filter) {
            unique_filters.push(filter);
        }
    }
    ability.context.controller_controlled_as_cast = unique_filters
        .into_iter()
        .filter(|filter| {
            super::quantity::resolve_quantity(
                state,
                &QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: filter.clone(),
                    },
                },
                player,
                source_id,
            ) > 0
        })
        .collect();
}

fn collect_controller_controlled_as_cast_filters(
    ability: &ResolvedAbility,
    filters: &mut Vec<TargetFilter>,
) {
    if let Some(condition) = &ability.condition {
        collect_controller_controlled_as_cast_filters_from_condition(condition, filters);
    }
    if let Some(sub_ability) = &ability.sub_ability {
        collect_controller_controlled_as_cast_filters(sub_ability, filters);
    }
    if let Some(else_ability) = &ability.else_ability {
        collect_controller_controlled_as_cast_filters(else_ability, filters);
    }
}

fn collect_controller_controlled_as_cast_filters_from_condition(
    condition: &AbilityCondition,
    filters: &mut Vec<TargetFilter>,
) {
    match condition {
        AbilityCondition::ControllerControlledMatchingAsCast { filter } => {
            filters.push(filter.clone());
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for condition in conditions {
                collect_controller_controlled_as_cast_filters_from_condition(condition, filters);
            }
        }
        AbilityCondition::Not { condition }
        | AbilityCondition::ConditionInstead { inner: condition } => {
            collect_controller_controlled_as_cast_filters_from_condition(condition, filters);
        }
        _ => {}
    }
}

fn recompute_pending_cast_cost_after_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: &mut PendingCast,
) {
    let object_id = pending.object_id;
    let prior_pending = state.pending_cast.take();
    state.pending_cast = Some(Box::new(pending.clone()));
    let recomputed = super::casting::recompute_pending_cast_cost(state, player, object_id);
    state.pending_cast = prior_pending;
    if let Some(cost) = recomputed {
        pending.cost = cost;
    }
}

/// Handle the player's decision on an additional cost (kicker, blight, "or pay").
///
/// For `Optional`: `pay=true` pays the cost and sets `additional_cost_paid`, `pay=false` skips.
/// For `Choice`: `pay=true` pays the first cost, `pay=false` pays the second cost.
pub(crate) fn handle_decide_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    additional_cost: &AdditionalCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if pending
        .additional_cost_queue
        .first()
        .is_some_and(|instance| {
            matches!(
                instance.cost,
                AdditionalCost::Optional {
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                    ..
                }
            )
        })
    {
        return handle_decide_repeatable_additional_cost(state, player, pending, pay, events);
    }

    match (pending.additional_cost_flow.as_ref(), additional_cost) {
        (Some(AdditionalCost::Kicker { .. }), _) => {
            return handle_decide_kicker_cost(state, player, pending, pay, events);
        }
        (
            Some(AdditionalCost::Optional {
                repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                ..
            }),
            _,
        ) => {
            return handle_decide_repeatable_additional_cost(state, player, pending, pay, events);
        }
        (None, AdditionalCost::Kicker { .. }) => {
            let mut pending = pending;
            pending.additional_cost_flow = Some(additional_cost.clone());
            return handle_decide_kicker_cost(state, player, pending, pay, events);
        }
        (
            None,
            AdditionalCost::Optional {
                repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                ..
            },
        ) => {
            let mut pending = pending;
            pending.additional_cost_flow = Some(additional_cost.clone());
            return handle_decide_repeatable_additional_cost(state, player, pending, pay, events);
        }
        _ => {}
    }

    let cost_source = pending.additional_cost_source;
    let current_instance = pending.additional_cost_queue.first().cloned();
    let mut ability = pending.ability;

    // CR 702.166a: Track whether this decision paid an optional additional cost
    // (Bargain), so the self-spell cost-modifier passes can be re-run afterward —
    // a `ReduceCost { condition: AdditionalCostPaid }` static only applies once
    // `additional_cost_paid` is set.
    let mut optional_cost_paid = false;

    let cost_to_pay = match additional_cost {
        // CR 702.33a: Kicker is an optional additional cost.
        AdditionalCost::Optional {
            cost,
            repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
        } => {
            if pay {
                if let Some(instance) = current_instance.as_ref() {
                    ability.context.record_additional_cost_instance_payment(
                        instance.origin,
                        instance.origin_ordinal,
                        1,
                    );
                } else {
                    ability
                        .context
                        .record_additional_cost_payment(AdditionalCostOrigin::Other, 1);
                }
                optional_cost_paid = true;
                Some(cost.clone())
            } else {
                None
            }
        }
        AdditionalCost::Optional {
            repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
            ..
        } => {
            unreachable!("repeatable optional costs are handled before generic optional costs")
        }
        AdditionalCost::Kicker { .. } => {
            unreachable!("kicker costs are handled before generic optional costs")
        }
        AdditionalCost::Choice(preferred, fallback) => {
            if pay {
                let is_card_additional_cost_choice = state
                    .objects
                    .get(&pending.object_id)
                    .and_then(|obj| obj.additional_cost.as_ref())
                    .is_some_and(|cost| matches!(cost, AdditionalCost::Choice(_, _)));
                if is_card_additional_cost_choice {
                    // CR 601.2b: Optional/additional `Choice` costs (e.g. casualty).
                    ability
                        .context
                        .record_additional_cost_payment(AdditionalCostOrigin::Other, 1);
                } else if matches!(preferred, AbilityCost::Mana { .. }) {
                    // CR 118.9: Spellcasting-option alternative mana costs are not
                    // additional costs; gate riders via `alternative_mana_cost_paid`.
                    ability.context.alternative_mana_cost_paid = true;
                }
                Some(preferred.clone())
            } else {
                Some(fallback.clone())
            }
        }
        AdditionalCost::Required(cost) => {
            // Required costs are always paid — the choice prompt should not be reached,
            // but handle defensively by always paying.
            if let Some(instance) = current_instance.as_ref() {
                ability.context.record_additional_cost_instance_payment(
                    instance.origin,
                    instance.origin_ordinal,
                    1,
                );
            } else {
                ability
                    .context
                    .record_additional_cost_payment(AdditionalCostOrigin::Other, 1);
            }
            Some(cost.clone())
        }
    };

    let mut updated_pending = PendingCast { ability, ..pending };
    if current_instance.is_some() {
        updated_pending.additional_cost_queue.remove(0);
        if updated_pending.additional_cost_queue.is_empty() {
            updated_pending.additional_cost_decided = true;
        }
    }
    updated_pending.additional_cost_source = SpellCostSource::Other;

    // CR 601.2b: When an optional additional cost (e.g. Casualty) was declared
    // before targets (deferred_target_selection = true), clear the flow after
    // the decision so finish_pending_cost_or_cast proceeds to target selection
    // instead of re-presenting the optional choice. Mark additional_cost_decided
    // so finish_pending_cast_cost_or_pay skips re-detecting the cost after
    // the player selects targets.
    if updated_pending.deferred_target_selection
        && matches!(
            updated_pending.additional_cost_flow,
            Some(AdditionalCost::Optional {
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                ..
            })
        )
    {
        updated_pending.additional_cost_flow = None;
        updated_pending.additional_cost_decided = true;
    }

    // CR 601.2f + CR 601.2g: Now that the optional additional cost (Bargain) has
    // been declared and `additional_cost_paid` is set, re-derive the total mana
    // cost before mana payment begins. The recompute reads the in-flight cast's
    // flag via `state.pending_cast`, so publish `updated_pending` there for the
    // duration of the recompute, then restore the prior value.
    if optional_cost_paid {
        recompute_pending_cast_cost_after_additional_cost(state, player, &mut updated_pending);
    }

    if let Some(cost) = cost_to_pay {
        pay_additional_cost_with_source(state, player, cost, cost_source, updated_pending, events)
    } else {
        finish_pending_cost_or_cast(state, player, updated_pending, events)
    }
}

pub(crate) fn payable_spell_alternative_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<AbilityCost> {
    payable_spell_alternative_cost_details(state, player, object_id).map(|details| details.cost)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PayableSpellAlternativeCost {
    pub(crate) cost: AbilityCost,
    pub(crate) timing_permission: Option<CastTimingPermission>,
}

pub(crate) fn payable_spell_alternative_cost_details(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<PayableSpellAlternativeCost> {
    let obj = state.objects.get(&object_id)?;
    if obj.zone != Zone::Hand || obj.controller != player {
        return None;
    }
    // This prompt reuses `AdditionalCost::Choice`, so keep it to pure
    // alternative/free-cast cards until the pending-cast flow can compose
    // alternative and additional costs in one CR 601.2f total-cost pass.
    if obj.additional_cost.is_some() {
        return None;
    }

    // CR 118.9a: only one alternative cost is applied to a spell and the
    // controller chooses which. The pipeline currently exposes a single
    // alternative-vs-printed choice, so when a spell carries BOTH a
    // self-referential casting option and a permanent grant it cannot offer
    // both — it deterministically prefers the spell's own printed option. This
    // is not a CR-mandated precedence; honoring full controller choice across a
    // self-option and one or more grants needs a multi-alternative choice
    // surface and is a known limitation tracked for follow-up.
    let self_option = obj.casting_options.iter().find_map(|option| {
        if option.condition.as_ref().is_some_and(|condition| {
            !restrictions::evaluate_condition(state, player, object_id, condition)
        }) {
            return None;
        }
        let cost = match option.kind {
            SpellCastingOptionKind::AlternativeCost => option.cost.clone()?,
            SpellCastingOptionKind::CastWithoutManaCost => AbilityCost::Mana {
                cost: ManaCost::NoCost,
            },
            SpellCastingOptionKind::AsThoughHadFlash | SpellCastingOptionKind::CastAdventure => {
                return None;
            }
        };
        if spell_alternative_cost_is_payable(state, player, object_id, &cost) {
            Some(PayableSpellAlternativeCost {
                cost,
                timing_permission: None,
            })
        } else {
            None
        }
    });
    if self_option.is_some() {
        return self_option;
    }

    // CR 118.9 + CR 601.2f: A permanent-granted alternative MANA cost (Rooftop
    // Storm, Fist of Suns, Jodah) applies when no self-referential option does.
    let granted = super::casting::granted_spell_alternative_cost(state, player, object_id)?;
    spell_alternative_cost_is_payable(state, player, object_id, &granted.cost).then_some(
        PayableSpellAlternativeCost {
            cost: granted.cost,
            timing_permission: granted.timing_permission,
        },
    )
}

pub(crate) fn payable_spell_alternative_cost_for_timing(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    timing_permission: CastTimingPermission,
) -> Option<PayableSpellAlternativeCost> {
    let obj = state.objects.get(&object_id)?;
    if obj.zone != Zone::Hand || obj.controller != player || obj.additional_cost.is_some() {
        return None;
    }

    let granted = super::casting::granted_spell_alternative_cost(state, player, object_id)?;
    if granted.timing_permission != Some(timing_permission) {
        return None;
    }
    spell_alternative_cost_is_payable(state, player, object_id, &granted.cost).then_some(
        PayableSpellAlternativeCost {
            cost: granted.cost,
            timing_permission: granted.timing_permission,
        },
    )
}

fn spell_alternative_cost_is_payable(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    match cost {
        AbilityCost::Mana { cost } => {
            super::casting::can_pay_cost_after_auto_tap(state, player, object_id, cost)
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .all(|sub_cost| spell_alternative_cost_is_payable(state, player, object_id, sub_cost)),
        other => other.is_payable(state, player, object_id),
    }
}

pub(crate) fn eligible_behold_choices(
    state: &GameState,
    player: PlayerId,
    source: ObjectId,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    let ctx = super::filter::FilterContext::from_source(state, source);
    let mut choices: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player
                    && super::filter::matches_target_filter(state, id, filter, &ctx)
            })
        })
        .collect();

    if let Some(player_state) = state.players.get(player.0 as usize) {
        choices.extend(player_state.hand.iter().copied().filter(|&id| {
            id != source && super::filter::matches_target_filter(state, id, filter, &ctx)
        }));
    }

    choices
}

fn handle_decide_kicker_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some((variant, cost, repeatability)) = next_kicker_option(state, player, &pending) else {
        pending.additional_cost_flow = None;
        return finish_pending_cost_or_cast(state, player, pending, events);
    };

    if !pay {
        if repeatability.is_repeatable() {
            pending.additional_cost_flow = None;
        } else if !pending.declined_kickers.contains(&variant) {
            pending.declined_kickers.push(variant);
        }
        return finish_pending_cost_or_cast(state, player, pending, events);
    }

    pending.ability.context.additional_cost_paid = true;
    pending.ability.context.kickers_paid.push(variant);
    // CR 601.2b + CR 601.2f + CR 702.33d: Kicker is declared before total cost is
    // locked in. Recompute now so "kicked spell" cost reducers see the paid kicker
    // through `state.pending_cast` before mana payment.
    recompute_pending_cast_cost_after_additional_cost(state, player, &mut pending);
    if pending.deferred_modal_choice.is_some() || pending.deferred_target_selection {
        pending.declared_kickers_to_pay.push(variant);
        return finish_pending_cost_or_cast(state, player, pending, events);
    }
    pay_additional_cost(state, player, cost, pending, events)
}

fn next_kicker_option(
    state: &GameState,
    player: PlayerId,
    pending: &PendingCast,
) -> Option<(
    KickerVariant,
    AbilityCost,
    crate::types::ability::AdditionalCostRepeatability,
)> {
    let Some(AdditionalCost::Kicker {
        costs,
        repeatability,
    }) = &pending.additional_cost_flow
    else {
        return None;
    };

    if repeatability.is_repeatable() {
        let cost = costs.first()?.clone();
        return cost
            .is_payable(state, player, pending.object_id)
            .then_some((
                KickerVariant::First,
                cost,
                crate::types::ability::AdditionalCostRepeatability::Repeatable,
            ));
    }

    for (index, cost) in costs.iter().enumerate() {
        let variant = match index {
            0 => KickerVariant::First,
            1 => KickerVariant::Second,
            _ => break,
        };
        if pending.ability.context.kickers_paid.contains(&variant)
            || pending.declined_kickers.contains(&variant)
        {
            continue;
        }
        if cost.is_payable(state, player, pending.object_id) {
            return Some((
                variant,
                cost.clone(),
                crate::types::ability::AdditionalCostRepeatability::Once,
            ));
        }
    }

    None
}

fn handle_decide_repeatable_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let queued_instance = pending.additional_cost_queue.first().cloned();
    let queued_origin = queued_instance.as_ref().map(|instance| instance.origin);
    let queued_origin_ordinal = queued_instance
        .as_ref()
        .map(|instance| instance.origin_ordinal);
    let Some(cost) = next_repeatable_additional_cost(state, player, &pending) else {
        if queued_origin.is_some() {
            pending.additional_cost_queue.remove(0);
        } else {
            pending.additional_cost_flow = None;
        }
        return finish_pending_cost_or_cast(state, player, pending, events);
    };

    if !pay {
        if queued_origin.is_some() {
            pending.additional_cost_queue.remove(0);
        } else {
            pending.additional_cost_flow = None;
        }
        return finish_pending_cost_or_cast(state, player, pending, events);
    }

    if let (Some(origin), Some(origin_ordinal)) = (queued_origin, queued_origin_ordinal) {
        pending
            .ability
            .context
            .record_additional_cost_instance_payment(origin, origin_ordinal, 1);
    } else {
        pending
            .ability
            .context
            .record_additional_cost_payment(AdditionalCostOrigin::Other, 1);
    }
    pay_additional_cost(state, player, cost, pending, events)
}

fn next_repeatable_additional_cost(
    state: &GameState,
    player: PlayerId,
    pending: &PendingCast,
) -> Option<AbilityCost> {
    if let Some(AdditionalCostInstance {
        cost:
            AdditionalCost::Optional {
                cost,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
            },
        ..
    }) = pending.additional_cost_queue.first()
    {
        return cost
            .is_payable(state, player, pending.object_id)
            .then_some(cost.clone());
    }

    let Some(AdditionalCost::Optional {
        cost,
        repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
    }) = &pending.additional_cost_flow
    else {
        return None;
    };

    cost.is_payable(state, player, pending.object_id)
        .then_some(cost.clone())
}

fn finish_pending_cost_or_cast(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let Some(instance) = pending.additional_cost_queue.first().cloned() {
        match instance.cost {
            AdditionalCost::Required(cost) => {
                pending.additional_cost_queue.remove(0);
                return pay_additional_cost_with_source(
                    state,
                    player,
                    cost,
                    SpellCostSource::Other,
                    pending,
                    events,
                );
            }
            AdditionalCost::Optional {
                cost,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            } => {
                if !cost.is_payable(state, player, pending.object_id) {
                    pending.additional_cost_queue.remove(0);
                    return finish_pending_cost_or_cast(state, player, pending, events);
                }
                return Ok(WaitingFor::OptionalCostChoice {
                    player,
                    cost: AdditionalCost::Optional {
                        cost,
                        repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                    },
                    times_kicked: 0,
                    pending_cast: Box::new(pending),
                });
            }
            AdditionalCost::Optional {
                cost,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
            } => {
                if cost.is_payable(state, player, pending.object_id) {
                    let times_kicked = pending.ability.context.instance_payment_count_for_ordinal(
                        instance.origin,
                        instance.origin_ordinal,
                    );
                    return Ok(WaitingFor::OptionalCostChoice {
                        player,
                        cost: AdditionalCost::Optional {
                            cost,
                            repeatability:
                                crate::types::ability::AdditionalCostRepeatability::Repeatable,
                        },
                        times_kicked,
                        pending_cast: Box::new(pending),
                    });
                }
                pending.additional_cost_queue.remove(0);
                return finish_pending_cost_or_cast(state, player, pending, events);
            }
            AdditionalCost::Kicker { .. } | AdditionalCost::Choice(_, _) => {
                pending.additional_cost_queue.remove(0);
                return finish_pending_cost_or_cast(state, player, pending, events);
            }
        }
    }

    if matches!(
        pending.additional_cost_flow,
        Some(AdditionalCost::Required(_))
    ) {
        if let Some(AdditionalCost::Required(cost)) = pending.additional_cost_flow.take() {
            let cost_source = pending.additional_cost_source;
            pending.additional_cost_source = SpellCostSource::Other;
            return pay_additional_cost_with_source(
                state,
                player,
                cost,
                cost_source,
                pending,
                events,
            );
        }
    }

    if matches!(
        pending.additional_cost_flow,
        Some(AdditionalCost::Optional {
            repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
            ..
        })
    ) {
        if let Some(current_cost) = next_repeatable_additional_cost(state, player, &pending) {
            let times_kicked = pending.ability.context.additional_cost_payment_count;
            return Ok(WaitingFor::OptionalCostChoice {
                player,
                cost: AdditionalCost::Optional {
                    cost: current_cost,
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                },
                times_kicked,
                pending_cast: Box::new(pending),
            });
        }
        pending.additional_cost_flow = None;
    }

    if matches!(
        pending.additional_cost_flow,
        Some(AdditionalCost::Kicker { .. })
    ) {
        if pending.deferred_target_selection {
            if let Some((_, current_cost, repeatability)) =
                next_kicker_option(state, player, &pending)
            {
                // CR 702.33c/d: present the live Kicker cost (not a laundered
                // Optional) so the frontend can render a kicker-aware modal and
                // know whether the kicker is repeatable.
                let times_kicked = pending.ability.context.kickers_paid.len() as u32;
                return Ok(WaitingFor::OptionalCostChoice {
                    player,
                    cost: AdditionalCost::Kicker {
                        costs: vec![current_cost],
                        repeatability,
                    },
                    times_kicked,
                    pending_cast: Box::new(pending),
                });
            }
            return begin_deferred_target_selection(state, player, pending, events);
        }
        if pending.deferred_modal_choice.is_none() {
            if let Some(cost) = next_declared_kicker_cost(&mut pending) {
                return pay_additional_cost(state, player, cost, pending, events);
            }
        }
        if let Some((_, current_cost, repeatability)) = next_kicker_option(state, player, &pending)
        {
            // CR 702.33c/d: present the live Kicker cost (not a laundered Optional)
            // so the frontend renders the kicker re-prompt with the running kick count.
            let times_kicked = pending.ability.context.kickers_paid.len() as u32;
            return Ok(WaitingFor::OptionalCostChoice {
                player,
                cost: AdditionalCost::Kicker {
                    costs: vec![current_cost],
                    repeatability,
                },
                times_kicked,
                pending_cast: Box::new(pending),
            });
        }
        if pending.deferred_modal_choice.is_none() {
            pending.additional_cost_flow = None;
        }
    }

    if pending.additional_cost_flow.is_none() {
        if let Some(req_cost) = pending.deferred_required_additional_cost.take() {
            if !req_cost.is_payable(state, player, pending.object_id) {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot pay required additional cost".to_string(),
                ));
            }
            let cost_source = pending.additional_cost_source;
            pending.additional_cost_source = SpellCostSource::Other;
            return pay_additional_cost_with_source(
                state,
                player,
                req_cost,
                cost_source,
                pending,
                events,
            );
        }
    }

    // CR 601.2b: Optional additional costs (Casualty) that must be declared before
    // targets. When deferred_target_selection is true, present the choice first.
    // After the choice resolves, additional_cost_flow is cleared by
    // handle_decide_additional_cost so the general deferred path below fires.
    if let Some(AdditionalCost::Optional {
        cost: ref optional_cost,
        repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
    }) = pending.additional_cost_flow
    {
        if pending.deferred_target_selection {
            let optional_cost = AdditionalCost::Optional {
                cost: optional_cost.clone(),
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            };
            return Ok(WaitingFor::OptionalCostChoice {
                player,
                cost: optional_cost,
                times_kicked: 0,
                pending_cast: Box::new(pending),
            });
        }
    }

    // CR 601.2b/c: General deferred target selection — fires after an optional
    // additional cost (e.g. Casualty sacrifice) has been decided and
    // additional_cost_flow cleared, so targets are chosen after the cost.
    if pending.deferred_target_selection
        && !matches!(
            pending.additional_cost_flow,
            Some(
                AdditionalCost::Kicker { .. }
                    | AdditionalCost::Optional {
                        repeatability:
                            crate::types::ability::AdditionalCostRepeatability::Repeatable,
                        ..
                    }
            )
        )
    {
        return begin_deferred_target_selection(state, player, pending, events);
    }

    if let Some(modal) = pending.deferred_modal_choice.take() {
        let mut capped = modal_choice_for_player(
            state,
            player,
            pending.object_id,
            &modal,
            &pending.ability.context,
        );
        capped.max_choices = capped.max_choices.min(capped.mode_count);
        pending.target_constraints = target_constraints_from_modal(&capped);
        let mode_abilities = state
            .objects
            .get(&pending.object_id)
            .map(super::ability_utils::modal_spell_mode_abilities)
            .unwrap_or_default();
        let unavailable_modes = super::ability_utils::spell_modal_unavailable_modes(
            state,
            pending.object_id,
            player,
            &capped,
            &mode_abilities,
        );
        return Ok(WaitingFor::ModeChoice {
            player,
            modal: capped,
            pending_cast: Box::new(pending),
            unavailable_modes,
        });
    }

    // CR 601.2b: If a Required additional cost was deferred while an optional cost
    // (e.g., Casualty) was offered first (Village Rites + Casualty), pay it now.
    if let Some(AdditionalCost::Required(req_cost)) = pending.additional_cost_flow.take() {
        if !req_cost.is_payable(state, player, pending.object_id) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay required additional cost".to_string(),
            ));
        }
        let cost_source = pending.additional_cost_source;
        pending.additional_cost_source = SpellCostSource::Other;
        return pay_additional_cost_with_source(
            state,
            player,
            req_cost,
            cost_source,
            pending,
            events,
        );
    }

    if pending.activation_ability_index.is_some()
        && !matches!(pending.cost, ManaCost::NoCost | ManaCost::SelfManaCost)
    {
        state.pending_cast = Some(Box::new(pending));
        return enter_payment_step(state, player, None, events);
    }

    if let Some(ability_index) = pending.activation_ability_index {
        let waiting_for = push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            pending.x_residual_activation,
            events,
        )?;
        return Ok(drain_deferred_triggers_after_stack_object_announcement(
            state,
            events,
            waiting_for,
        ));
    }

    let base_cost = pending.base_cost.clone();
    // CR 601.2f: Cost floors are the last effects applied to the final locked
    // spell cost. Additional-cost payments can reduce `pending.cost` after the
    // prepare/targeting floor passes, so re-run the floor idempotently here.
    if !cost_has_x(&pending.cost) {
        super::casting::apply_cost_floor(state, player, pending.object_id, &mut pending.cost);
        super::casting::apply_cost_floor_with_selected_targets(
            state,
            player,
            pending.object_id,
            &pending.ability,
            &mut pending.cost,
        );
    }
    let waiting_for = pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        base_cost,
        pending.casting_variant,
        pending.cast_timing_permission,
        pending.distribute,
        pending.origin_zone,
        pending.payment_mode,
        events,
    )?;
    Ok(drain_deferred_triggers_after_stack_object_announcement(
        state,
        events,
        waiting_for,
    ))
}

pub(super) fn drain_deferred_triggers_after_stack_object_announcement(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    waiting_for: WaitingFor,
) -> WaitingFor {
    if !matches!(waiting_for, WaitingFor::Priority { .. }) {
        return waiting_for;
    }
    crate::game::triggers::drain_deferred_triggers_after_stack_object_announcement(state, events)
        .unwrap_or(waiting_for)
}

pub(crate) fn begin_deferred_target_selection(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    pending.deferred_target_selection = false;
    // CR 700.2 + CR 601.2b: For modal casts whose target legality depended on
    // X (or any deferred cost), the mode-choice step recorded the chosen mode
    // indices on `pending.chosen_modes`. Rebuild slots with the labelled
    // builder so the per-mode banner survives the X round-trip — passing
    // `pending.ability.chosen_x` so per-mode legality filters that reference
    // `X` (e.g. Kozilek's Command mode 2: "mana value X or less") resolve
    // against the announced value. Non-modal casts fall back to the unlabelled
    // builder.
    // CR 601.2b + CR 601.2c: modes/X are announced (601.2b) before targets are
    // chosen (601.2c), since target legality (e.g. "mana value X or less") can
    // depend on the chosen X.
    let (mut target_slots, mode_labels) = if pending.chosen_modes.is_empty() {
        (build_target_slots(state, &pending.ability)?, Vec::new())
    } else {
        let obj = state.objects.get(&pending.object_id).ok_or_else(|| {
            EngineError::InvalidAction(
                "Modal spell object missing for deferred target labels".into(),
            )
        })?;
        let (abilities, mode_descriptions) =
            if let Some(ability_index) = pending.activation_ability_index {
                let def = obj.abilities.get(ability_index).ok_or_else(|| {
                    EngineError::InvalidAction(
                        "Modal activated ability missing for deferred target labels".into(),
                    )
                })?;
                (
                    def.mode_abilities.clone(),
                    def.modal
                        .as_ref()
                        .map(|m| m.mode_descriptions.clone())
                        .unwrap_or_default(),
                )
            } else {
                (
                    obj.abilities.to_vec(),
                    obj.modal
                        .as_ref()
                        .map(|m| m.mode_descriptions.clone())
                        .unwrap_or_default(),
                )
            };
        debug_assert!(
            !mode_descriptions.is_empty(),
            "begin_deferred_target_selection: chosen_modes is non-empty but the source object has no modal descriptions (object {:?}); per-mode target labels would silently degrade",
            pending.object_id,
        );
        build_target_slots_labelled(
            state,
            &abilities,
            &pending.chosen_modes,
            &mode_descriptions,
            pending.object_id,
            pending.ability.controller,
            &pending.ability.context,
            pending.ability.chosen_x,
        )?
    };
    // CR 601.2c + CR 601.2d: X is now known (deferred selection runs after the
    // ChooseXValue round-trip), so a divided spell's slot count can be clamped to
    // its divisible pool — each target needs ≥1, so picking more targets than the
    // pool can never be legally divided (Shatterskull Smashing X=1, issue #2856).
    super::ability_utils::cap_distribution_target_slots(
        state,
        &pending.ability,
        pending.distribute.as_ref(),
        &mut target_slots,
    );
    if target_slots.is_empty() {
        return finish_pending_cost_or_cast(state, player, pending, events);
    }
    // CR 115.1 + CR 701.9b: Random-target abilities short-circuit to RNG-driven
    // selection here too. The deferred-selection path is reached after additional
    // costs are paid; the random pick still uses `state.rng`.
    if matches!(
        pending.ability.target_selection_mode,
        crate::types::ability::TargetSelectionMode::Random
    ) {
        let targets =
            random_select_targets_for_ability(state, &target_slots, &pending.target_constraints)?;
        let mut ability = pending.ability.clone();
        assign_targets_in_chain(state, &mut ability, &targets)?;
        pending.ability = ability;
        return finish_pending_cost_or_cast(state, player, pending, events);
    }
    if let Some(targets) = auto_select_targets_for_ability(
        state,
        &pending.ability,
        &target_slots,
        &pending.target_constraints,
    )? {
        let mut ability = pending.ability.clone();
        assign_targets_in_chain(state, &mut ability, &targets)?;
        pending.ability = ability;
        return finish_pending_cost_or_cast(state, player, pending, events);
    }

    let selection = begin_target_selection_for_ability(
        state,
        &pending.ability,
        &target_slots,
        &pending.target_constraints,
    )?;
    Ok(WaitingFor::TargetSelection {
        player,
        pending_cast: Box::new(pending),
        target_slots,
        mode_labels,
        selection,
    })
}

fn next_declared_kicker_cost(pending: &mut PendingCast) -> Option<AbilityCost> {
    let additional = pending.additional_cost_flow.as_ref()?;
    let AdditionalCost::Kicker {
        costs,
        repeatability,
    } = additional
    else {
        return None;
    };
    let variant = pending.declared_kickers_to_pay.pop()?;
    if repeatability.is_repeatable() {
        return costs.first().cloned();
    }
    let index = match variant {
        KickerVariant::First => 0,
        KickerVariant::Second => 1,
    };
    costs.get(index).cloned()
}

/// Complete the discard-for-cost flow: discard selected cards, then continue casting.
pub(crate) fn handle_discard_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    expected: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != expected {
        return Err(EngineError::InvalidAction(format!(
            "Must discard exactly {} card(s), got {}",
            expected,
            chosen.len()
        )));
    }
    for card_id in chosen {
        if !legal_cards.contains(card_id) {
            return Err(EngineError::InvalidAction(
                "Selected card not in hand".to_string(),
            ));
        }
    }

    // CR 117.1 + CR 400.7j + CR 608.2k: Capture the discarded card's public
    // characteristics BEFORE it leaves the hand, so cost-paid-object property
    // references can resolve at ability resolution.
    if let Some(&first) = chosen.first() {
        if let Some(obj) = state.objects.get(&first) {
            pending
                .ability
                .set_cost_paid_object_recursive(CostPaidObjectSnapshot {
                    object_id: first,
                    lki: obj.snapshot_for_mana_spent(),
                });
        }
    }

    // CR 601.2h + CR 616.1: Discard each chosen card through the replacement pipeline
    // so Madness (CR 702.35) etc. can intercept.
    for (index, &card_id) in chosen.iter().enumerate() {
        match super::effects::discard::discard_as_cost(state, card_id, player, events) {
            super::effects::discard::DiscardOutcome::Complete => {}
            super::effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) => {
                state.pending_discard_for_cost = Some(PendingDiscardForCostResume {
                    player,
                    pending: pending.clone(),
                    chosen: chosen.to_vec(),
                    paused_at_index: index,
                });
                super::casting::pause_cost_payment_for_replacement_choice(state, choice_player);
                return Ok(state.waiting_for.clone());
            }
        }
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 601.2h + CR 616.1: After a replacement choice during discard-for-cost payment, finish
/// discarding any remaining cards and continue the cast/activation pipeline.
pub(crate) fn resume_interrupted_cost_payment(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let Some(resume) = state.pending_discard_for_cost.take() {
        let player = resume.player;
        let pending = resume.pending;
        for &card_id in resume.chosen.iter().skip(resume.paused_at_index + 1) {
            match super::effects::discard::discard_as_cost(state, card_id, player, events) {
                super::effects::discard::DiscardOutcome::Complete => {}
                super::effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) => {
                    let paused_at_index = resume
                        .chosen
                        .iter()
                        .position(|&id| id == card_id)
                        .unwrap_or(resume.paused_at_index + 1);
                    state.pending_discard_for_cost = Some(PendingDiscardForCostResume {
                        player,
                        pending: pending.clone(),
                        chosen: resume.chosen.clone(),
                        paused_at_index,
                    });
                    super::casting::pause_cost_payment_for_replacement_choice(state, choice_player);
                    return Ok(state.waiting_for.clone());
                }
            }
        }
        return finish_pending_cost_or_cast(state, player, pending, events);
    }

    let Some(pending) = state.pending_cast.take() else {
        return Ok(WaitingFor::Priority {
            player: state.active_player,
        });
    };
    let pending = *pending;
    let player = state
        .objects
        .get(&pending.object_id)
        .map(|o| o.controller)
        .unwrap_or(state.active_player);
    if let Some(activation_ability_index) = pending.activation_ability_index {
        return push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            activation_ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            pending.x_residual_activation,
            events,
        );
    }
    finish_pending_cost_or_cast(state, player, pending, events)
}

fn replace_first_one_of_cost(cost: &mut AbilityCost, chosen: AbilityCost) -> bool {
    match cost {
        AbilityCost::OneOf { .. } => {
            *cost = chosen;
            true
        }
        AbilityCost::Composite { costs } => {
            for cost in costs {
                if replace_first_one_of_cost(cost, chosen.clone()) {
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// CR 118.12a + CR 602.2b: Complete disjunctive activation-cost branch selection.
pub(crate) fn handle_activation_cost_one_of_choice(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    costs: &[AbilityCost],
    index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if index >= costs.len() {
        return Err(EngineError::InvalidAction(format!(
            "Invalid OneOf cost branch index: {}",
            index
        )));
    }

    let chosen_cost = &costs[index];
    if !chosen_cost.is_payable(state, player, pending.object_id) {
        return Err(EngineError::ActionNotAllowed(
            "Chosen cost branch is not payable".to_string(),
        ));
    }

    let replaced = pending
        .activation_cost
        .as_mut()
        .is_some_and(|cost| replace_first_one_of_cost(cost, chosen_cost.clone()));
    if !replaced {
        return Err(EngineError::InvalidAction(
            "Pending activation cost no longer has a OneOf branch".to_string(),
        ));
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

#[derive(Clone, Copy)]
pub(crate) struct SpellCostPayment<'a> {
    pub(crate) cost: &'a AbilityCost,
    pub(crate) source: SpellCostSource,
}

pub(crate) struct CostSelection<'a> {
    pub(crate) min_count: usize,
    pub(crate) count: usize,
    pub(crate) legal_permanents: &'a [ObjectId],
    pub(crate) chosen: &'a [ObjectId],
}

pub(crate) fn handle_sacrifice_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    paid_cost: Option<SpellCostPayment<'_>>,
    selection: CostSelection<'_>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let CostSelection {
        min_count,
        count,
        legal_permanents,
        chosen,
    } = selection;
    if chosen.len() < min_count || chosen.len() > count {
        let requirement = if min_count == count {
            format!("exactly {} permanent(s)", count)
        } else {
            format!("between {} and {} permanent(s)", min_count, count)
        };
        return Err(EngineError::InvalidAction(format!(
            "Must sacrifice {requirement}, got {}",
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible for sacrifice".to_string(),
            ));
        }
    }

    // CR 702.48b-c / CR 702.119a-c: If this sacrifice is paying an Offering or
    // Emerge additional cost, use the chosen permanent's ObjectId BEFORE it
    // leaves the battlefield so the mana-value reduction can read its mana cost.
    let reduction_source = paid_cost.and_then(|payment| {
        if payment.source == SpellCostSource::Offering
            && is_offering_sacrifice_cost(state, player, pending.object_id, payment.cost)
        {
            Some(SpellCostSource::Offering)
        } else if payment.source == SpellCostSource::Emerge
            && is_emerge_sacrifice_cost(payment.cost)
        {
            Some(SpellCostSource::Emerge)
        } else {
            None
        }
    });

    // CR 117.1 + CR 400.7j + CR 608.2k: Capture the sacrificed object's public
    // characteristics BEFORE it leaves the battlefield, stamping it onto the
    // resolving ability for later cost-paid-object references.
    if let Some(&first) = chosen.first() {
        if let Some(obj) = state.objects.get(&first) {
            pending
                .ability
                .set_cost_paid_object_recursive(CostPaidObjectSnapshot {
                    object_id: first,
                    lki: obj.snapshot_for_mana_spent(),
                });
        }
    }

    // CR 702.48c / CR 702.119a: Offering and Emerge use different reduction
    // rules, but both must read the sacrificed permanent before it leaves.
    if let Some(reduction_source) = reduction_source {
        if let Some(&first) = chosen.first() {
            match reduction_source {
                SpellCostSource::Offering => {
                    apply_offering_cost_reduction(state, first, &mut pending.cost);
                }
                SpellCostSource::Emerge => {
                    apply_emerge_cost_reduction(state, first, &mut pending.cost);
                }
                SpellCostSource::Other => {}
            }
        }
    }

    // CR 601.2f: "for each [object] sacrificed this way" reductions depend on
    // the actual cost-payment selection. Count the chosen objects while they are
    // still permanents on the battlefield (CR 403.3), before sacrifice moves them.
    if pending.activation_ability_index.is_none()
        && pending.ability.context.additional_cost_paid
        && !chosen.is_empty()
    {
        apply_sacrificed_this_way_cost_reduction(
            state,
            pending.object_id,
            chosen,
            &mut pending.cost,
        );
    }

    // Boundary of the cost-payment events THIS handler produces — captured
    // before the sacrifice so the death/leaves-the-battlefield `ZoneChanged`
    // records (and their producer co-departed stamp, below) can be scanned for
    // observers if the cast pauses before Priority (see the deferred-parking
    // block after `finish_pending_cost_or_cast`).
    let cost_event_start = events.len();

    // Sacrifice each chosen permanent
    for &id in chosen {
        super::sacrifice::sacrifice_permanent(state, id, player, events)
            .map_err(|e| EngineError::InvalidAction(format!("{e}")))?;
    }

    // CR 603.10a + CR 701.21a + CR 601.2h + CR 118.8: permanents sacrificed to pay
    // one cost component leave the battlefield together; a co-departing observer
    // among them observes the rest (look-back-in-time). Single authority — identical
    // wiring to `effects::sacrifice::resolve`. `departed_subset` drops any permanent
    // that did not actually leave (CantBeSacrificed, replacement).
    crate::game::zones::mark_simultaneous_departures(
        events,
        &crate::game::zones::departed_subset(state, chosen),
    );
    let cost_event_end = events.len();

    // CR 107.3a: The selected payment count defines X for this activation or
    // additional cost while its ability is on the stack.
    if min_count == 0 {
        pending
            .ability
            .set_chosen_x_recursive(chosen.len().try_into().unwrap_or(u32::MAX));
    }

    let waiting_for = finish_pending_cost_or_cast(state, player, pending, events)?;

    // CR 603.6c + CR 603.10a + CR 603.3b: When `finish_pending_cost_or_cast`
    // lands on `Priority` the cast completed in THIS action, so
    // `run_post_action_pipeline` will scan `events` (including the
    // cost-sacrifice `ZoneChanged` records stamped just above) and the
    // leaves-the-battlefield / dies observers fire normally.
    //
    // But when the cast PAUSES on a later target/kicker/modal choice
    // (a non-`Priority` `WaitingFor`), `apply_action` does NOT run the
    // post-action pipeline over this action's `events` (engine.rs gates the
    // pipeline on `WaitingFor::Priority`), and the cast lands in a LATER
    // action whose fresh `events` vector no longer carries these records — so
    // the producer co-departed stamp would be unreadable and a "whenever a
    // creature you control dies" / leaves-the-battlefield observer among the
    // co-sacrificed permanents would under-observe. Mirror the established
    // B2 parking pattern in `engine_resolution_choices::batch_or_drain_observer_triggers`:
    // collect the cost-payment observer triggers into `deferred_triggers` now,
    // where the stamped records are still in scope. They are NOT drained while
    // the announced spell remains on the stack (`should_drain_deferred_triggers_now`
    // refuses to drain with a `Spell` entry present), so they reach the stack
    // at the next true resolution boundary after the cast completes — CR 603.3
    // ("the next time a player would receive priority").
    if !matches!(waiting_for, WaitingFor::Priority { .. }) {
        let cost_events: Vec<GameEvent> = events[cost_event_start..cost_event_end]
            .iter()
            .filter(|ev| !matches!(ev, GameEvent::PhaseChanged { .. }))
            .cloned()
            .collect();
        crate::game::triggers::collect_triggers_into_deferred(state, &cost_events);
    }

    Ok(waiting_for)
}

/// CR 118.3 + CR 601.2b: Complete return-to-hand-as-cost after player selection.
pub(crate) fn handle_return_to_hand_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    count: usize,
    legal_permanents: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must return exactly {} permanent(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_permanents.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible to return".to_string(),
            ));
        }
    }

    if pending.activation_ability_index.is_some() {
        if let Some(cost) = pending.activation_cost.take() {
            // CR 118.3 + CR 601.2h + CR 602.2b: A player pays an activated ability's total
            // cost before that ability becomes activated. For self-bounce costs
            // such as Maze's End, pay automatic components like {T} while the
            // source is still on the battlefield, then perform the chosen return.
            super::casting::pay_ability_cost(state, player, pending.object_id, &cost, events)?;
        }
    }

    // CR 603.10a co-departed sibling (confirmed-excluded, mirrors the Ward
    // GAP comment): permanents returned to hand as a cost leave the battlefield
    // together, so a co-departing leaves-the-battlefield observer among them
    // would under-observe — the same gap `handle_sacrifice_for_cost` closes with
    // a `mark_simultaneous_departures` stamp. Not stamped here because
    // return-to-hand-as-cost is effectively always a single permanent (Daze,
    // Karoo lands, Cavern Harpy): `count` is almost always 1, so the stamp's
    // `len() < 2` guard would no-op. If a >=2-permanent return-to-hand cost ever
    // ships, mirror the A1 stamp from `handle_sacrifice_for_cost` here.
    for &id in chosen {
        super::zones::move_to_zone(state, id, Zone::Hand, events);
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 118.3 + CR 122.1 + CR 601.2b: Complete remove-counter-as-cost after
/// player selection.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_remove_counter_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    count: u32,
    counter_type: crate::types::counter::CounterMatch,
    selection: CounterCostSelection,
    legal_permanents: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if selection == CounterCostSelection::AmongObjects {
        return Err(EngineError::InvalidAction(
            "Counter distribution is required for from-among counter costs".to_string(),
        ));
    }
    let paid_object = match selection {
        CounterCostSelection::SingleObject => {
            if chosen.len() != 1 {
                return Err(EngineError::InvalidAction(format!(
                    "Must choose exactly one permanent, got {}",
                    chosen.len()
                )));
            }
            Some(chosen[0])
        }
        CounterCostSelection::AmongObjects => chosen.first().copied(),
    };
    if chosen.is_empty() || chosen.iter().any(|id| !legal_permanents.contains(id)) {
        return Err(EngineError::InvalidAction(
            "Selected permanent not eligible for counter removal".to_string(),
        ));
    }

    let selected_removable = chosen
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|obj| super::casting::removable_counter_count(obj, &counter_type))
        .fold(0, u32::saturating_add);
    if selected_removable < count {
        return Err(EngineError::InvalidAction(
            "Selected permanents do not have enough removable counters".to_string(),
        ));
    }

    if pending.activation_ability_index.is_some() {
        if let Some(cost) = pending.activation_cost.take() {
            // CR 601.2h + CR 602.2b: Pay automatic activation-cost components such as
            // {T} before removing the chosen counter and putting the ability
            // on the stack. The targeted RemoveCounter sub-cost no-ops in
            // `pay_ability_cost` because this handler pays that choice.
            super::casting::pay_ability_cost(state, player, pending.object_id, &cost, events)?;
        }
    }

    let mut remaining = count;
    for &object_id in chosen {
        if remaining == 0 {
            break;
        }
        let Some(concrete_counter) = super::effects::counters::resolve_counter_match_for_removal(
            state,
            object_id,
            &counter_type,
        ) else {
            continue;
        };
        let removable = state
            .objects
            .get(&object_id)
            .and_then(|obj| obj.counters.get(&concrete_counter))
            .copied()
            .unwrap_or(0);
        let to_remove = removable.min(remaining);
        if to_remove > 0 {
            super::effects::counters::remove_counter_with_replacement(
                state,
                object_id,
                concrete_counter,
                to_remove,
                events,
            );
            remaining -= to_remove;
        }
    }
    if remaining > 0 {
        return Err(EngineError::ActionNotAllowed(
            "No removable counter".to_string(),
        ));
    }

    if let Some(obj) = paid_object.and_then(|id| state.objects.get(&id).map(|obj| (id, obj))) {
        pending
            .ability
            .set_cost_paid_object_recursive(CostPaidObjectSnapshot {
                object_id: obj.0,
                lki: obj.1.snapshot_for_mana_spent(),
            });
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 118.3 + CR 122.1 + CR 601.2b: Complete "remove N counters from among"
/// cost payment after the player assigns exact counter counts per object.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_remove_counter_distribution_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    count: u32,
    counter_type: crate::types::counter::CounterMatch,
    selection: CounterCostSelection,
    legal_permanents: &[ObjectId],
    distribution: &[CounterCostChoice],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if selection != CounterCostSelection::AmongObjects {
        return Err(EngineError::InvalidAction(
            "Counter distribution is only valid for from-among counter costs".to_string(),
        ));
    }

    let mut seen = HashSet::new();
    let mut total = 0u32;
    for choice in distribution {
        if choice.count == 0 {
            return Err(EngineError::InvalidAction(
                "Counter distribution amounts must be positive".to_string(),
            ));
        }
        if !seen.insert((choice.object_id, choice.counter_type.clone())) {
            return Err(EngineError::InvalidAction(
                "Counter distribution contains duplicate counter choices".to_string(),
            ));
        }
        if !legal_permanents.contains(&choice.object_id) {
            return Err(EngineError::InvalidAction(
                "Selected permanent not eligible for counter removal".to_string(),
            ));
        }
        if matches!(
            &counter_type,
            crate::types::counter::CounterMatch::OfType(required) if required != &choice.counter_type
        ) {
            return Err(EngineError::InvalidAction(
                "Counter distribution uses the wrong counter type".to_string(),
            ));
        }
        let removable = state
            .objects
            .get(&choice.object_id)
            .and_then(|obj| obj.counters.get(&choice.counter_type))
            .copied()
            .unwrap_or(0);
        if removable < choice.count {
            return Err(EngineError::InvalidAction(
                "Counter distribution exceeds removable counters".to_string(),
            ));
        }
        total = total.saturating_add(choice.count);
    }
    if total != count {
        return Err(EngineError::InvalidAction(format!(
            "Counter distribution must total {count}, got {total}",
        )));
    }

    if pending.activation_ability_index.is_some() {
        if let Some(cost) = pending.activation_cost.take() {
            // CR 601.2h + CR 602.2b: Pay automatic activation-cost components
            // such as {T} before removing the assigned counters and putting
            // the ability on the stack.
            super::casting::pay_ability_cost(state, player, pending.object_id, &cost, events)?;
        }
    }

    for choice in distribution {
        let removable = state
            .objects
            .get(&choice.object_id)
            .and_then(|obj| obj.counters.get(&choice.counter_type))
            .copied()
            .unwrap_or(0);
        if removable < choice.count {
            return Err(EngineError::InvalidAction(
                "Counter distribution exceeds removable counters".to_string(),
            ));
        }
        super::effects::counters::remove_counter_with_replacement(
            state,
            choice.object_id,
            choice.counter_type.clone(),
            choice.count,
            events,
        );
    }

    if let Some(choice) = distribution.first() {
        if let Some(obj) = state.objects.get(&choice.object_id) {
            pending
                .ability
                .set_cost_paid_object_recursive(CostPaidObjectSnapshot {
                    object_id: choice.object_id,
                    lki: obj.snapshot_for_mana_spent(),
                });
        }
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

/// Blight cost — CR 701.68a: put N -1/-1 counters on the one chosen creature.
pub(crate) fn handle_blight_choice(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    counters: u32,
    legal_creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 701.68a: to blight is to put N -1/-1 counters on a creature (one) you control.
    if chosen.len() != 1 {
        return Err(EngineError::InvalidAction(format!(
            "Must blight exactly one creature, got {}",
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for blight".to_string(),
            ));
        }
    }

    // CR 701.68a + CR 614.1: place N -1/-1 counters on the one chosen
    // creature, routed through the CR 122.6 replacement pipeline. Guarded
    // on N > 0 for exact parity with the #497 effect-form handler
    // (engine_resolution_choices.rs `EffectKind::BlightEffect`); the parser
    // does not structurally exclude a degenerate `Blight 0`.
    // CR 117.1 + CR 608.2k: snapshot the blighted creature as this ability's
    // cost-paid object so later `CostPaidObject` target filters / quantity
    // refs ("the creature you blighted") resolve to it. This writes the
    // `cost_paid_object` field — the cost-paid-object category — exactly as
    // the sacrifice-for-cost handler does. It is DELIBERATELY a different
    // field from the #497 EFFECT-form handler, which writes
    // `effect_context_object` (CR 608.2c). `TargetFilter::CostPaidObject`
    // (filter.rs) reads only `cost_paid_object`; cost != effect.
    if let Some(obj) = state.objects.get(&chosen[0]) {
        pending
            .ability
            .set_cost_paid_object_recursive(CostPaidObjectSnapshot {
                object_id: chosen[0],
                lki: obj.snapshot_for_mana_spent(),
            });
    }

    if counters > 0
        && !add_counter_with_replacement(
            state,
            player,
            chosen[0],
            crate::types::counter::CounterType::Minus1Minus1,
            counters,
            events,
        )
    {
        state.pending_cast = Some(Box::new(pending));
        return Ok(state.waiting_for.clone());
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 702.34a: Tap creatures cost — complete the tap-creatures cost after player selection.
pub(crate) fn handle_tap_creatures_for_spell_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    count: usize,
    legal_creatures: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must tap exactly {} creature(s), got {}",
            count,
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_creatures.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected creature not eligible for tapping".to_string(),
            ));
        }
    }

    // Tap each chosen creature
    for &id in chosen {
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.tapped = true;
        }
        events.push(GameEvent::PermanentTapped {
            object_id: id,
            caused_by: None,
        });
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_behold_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    count: usize,
    legal_choices: &[ObjectId],
    action: BeholdCostAction,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if chosen.len() != count {
        return Err(EngineError::InvalidAction(format!(
            "Must behold exactly {} object(s), got {}",
            count,
            chosen.len(),
        )));
    }
    for id in chosen {
        if !legal_choices.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected object not eligible to behold".to_string(),
            ));
        }
    }

    let mut revealed_ids = Vec::new();
    let mut revealed_names = Vec::new();
    let mut snapshot = None;
    for &chosen_id in chosen {
        let obj = state
            .objects
            .get(&chosen_id)
            .ok_or_else(|| EngineError::InvalidAction("Selected object no longer exists".into()))?;
        let from_hand = state
            .players
            .get(player.0 as usize)
            .is_some_and(|p| p.hand.contains(&chosen_id));
        let from_battlefield = obj.zone == Zone::Battlefield && obj.controller == player;
        if !from_hand && !from_battlefield {
            return Err(EngineError::InvalidAction(
                "Selected object is no longer eligible to behold".into(),
            ));
        }
        if snapshot.is_none() {
            snapshot = Some(CostPaidObjectSnapshot {
                object_id: chosen_id,
                lki: obj.snapshot_for_mana_spent(),
            });
        }
        if action == BeholdCostAction::ChooseOrReveal && from_hand {
            revealed_ids.push(chosen_id);
            revealed_names.push(obj.name.clone());
        }
    }

    if action == BeholdCostAction::ExileChosen {
        for &chosen_id in chosen {
            super::zones::move_to_zone(state, chosen_id, Zone::Exile, events);
        }
    } else if !revealed_ids.is_empty() {
        events.push(GameEvent::CardsRevealed {
            player,
            card_ids: revealed_ids,
            card_names: revealed_names,
        });
    }

    pending.ability.context.additional_cost_paid = true;
    if let Some(snapshot) = snapshot {
        pending.ability.set_cost_paid_object_recursive(snapshot);
    }
    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 118.9a + CR 601.2b + CR 601.2h: Complete the exile-for-cost cost after
/// player selection. Covers escape (CR 702.138a, `zone = Graveyard`) and
/// pitch spells (Force of Will and the rest of the pitch-spell family,
/// `zone = Hand`). CR 118.9a authorizes alternative costs; CR 601.2b covers
/// cost announcement; CR 601.2h covers payment. The only zone-specific branch
/// is the "still in zone" re-validation against the chosen cards.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_exile_for_cost(
    state: &mut GameState,
    player: PlayerId,
    zone: ExileCostSourceZone,
    pending: PendingCast,
    expected: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    finish_exile_selection_for_cost(
        state,
        player,
        pending,
        (expected, expected),
        legal_cards,
        chosen,
        events,
        "card(s)",
        "Selected card not eligible for exile",
        |state, player, id, _pending| {
            // Re-validate: chosen cards must still be in the cost's source zone.
            let still_in_zone = state
                .players
                .get(player.0 as usize)
                .is_some_and(|p| match zone {
                    ExileCostSourceZone::Hand => p.hand.contains(&id),
                    ExileCostSourceZone::Graveyard => p.graveyard.contains(&id),
                });
            if !still_in_zone {
                return Err(EngineError::InvalidAction(format!(
                    "Selected card is no longer in {:?}",
                    zone.as_zone()
                )));
            }
            Ok(())
        },
    )
}

/// CR 601.2h + CR 701.13: Resolve a battlefield exile-permanent additional cost
/// (Food Chain class; Lunar Hatchling's "Exile a land you control"). The player
/// has chosen permanents they control on the battlefield; validate count and
/// legality, re-validate eligibility against the live battlefield (still on the
/// battlefield, controlled by `player`, matching `filter`, not the source), then
/// EXILE each chosen object (CR 701.13 — not a sacrifice, so no sacrifice/death
/// triggers) and resume the pending cast. Mirrors `handle_exile_for_cost`
/// (single-zone, single-filter) but revalidates against the battlefield rather
/// than hand/graveyard. `count == min_count` for a mandatory fixed-count cost.
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_exile_permanent_for_cost(
    state: &mut GameState,
    player: PlayerId,
    filter: Option<TargetFilter>,
    pending: PendingCast,
    expected: usize,
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    finish_exile_selection_for_cost(
        state,
        player,
        pending,
        (expected, expected),
        legal_cards,
        chosen,
        events,
        "permanent(s)",
        "Selected permanent not eligible to exile as a cost",
        |state, player, id, pending| {
            // CR 601.2h: Re-validate against the live battlefield — the chosen
            // permanent must still be on the battlefield, controlled by the
            // payer, match the cost's filter, and not be the cast source.
            if id == pending.object_id {
                return Err(EngineError::InvalidAction(
                    "Cannot exile the spell being cast as its own escape cost".into(),
                ));
            }
            let ctx = super::filter::FilterContext::from_source(state, pending.object_id);
            let eligible = state.objects.get(&id).is_some_and(|obj| {
                obj.zone == Zone::Battlefield
                    && obj.controller == player
                    && filter
                        .as_ref()
                        .is_none_or(|f| super::filter::matches_target_filter(state, id, f, &ctx))
            });
            if !eligible {
                return Err(EngineError::InvalidAction(
                    "Selected permanent is no longer eligible to exile".into(),
                ));
            }
            Ok(())
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn finish_exile_selection_for_cost(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    bounds: (usize, usize),
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
    object_label: &str,
    illegal_message: &str,
    revalidate: impl Fn(&GameState, PlayerId, ObjectId, &PendingCast) -> Result<(), EngineError>,
) -> Result<WaitingFor, EngineError> {
    let (min_count, max_count) = bounds;
    if chosen.len() < min_count || chosen.len() > max_count {
        let expected = if min_count == max_count {
            format!("exactly {min_count}")
        } else {
            format!("{min_count} to {max_count}")
        };
        return Err(EngineError::InvalidAction(format!(
            "Must exile {expected} {object_label}, got {}",
            chosen.len()
        )));
    }
    for id in chosen {
        if !legal_cards.contains(id) {
            return Err(EngineError::InvalidAction(illegal_message.to_string()));
        }
    }

    for &id in chosen {
        revalidate(state, player, id, &pending)?;
    }

    // CR 608.2k: Capture the first exiled object's public characteristics BEFORE
    // it leaves the zone, stamping it recursively onto the resolving ability so
    // `TargetFilter::CostPaidObject` resolves during ability resolution.
    if let Some(&first) = chosen.first() {
        if let Some(obj) = state.objects.get(&first) {
            // CR 107.3a + CR 118.9: Shoal-style alternative costs ("exile a
            // [color] card with mana value X") define X from the pitched card's
            // mana value rather than a prior announcement.
            if pending.ability.chosen_x.is_none()
                && pending.cost == crate::types::mana::ManaCost::NoCost
                && pending.base_cost.as_ref().is_some_and(cost_has_x)
            {
                pending
                    .ability
                    .set_chosen_x_recursive(obj.mana_cost.mana_value());
            }
            pending
                .ability
                .set_cost_paid_object_recursive(CostPaidObjectSnapshot {
                    object_id: first,
                    lki: obj.snapshot_for_mana_spent(),
                });
        }
    }

    for &id in chosen {
        super::zones::move_to_zone(state, id, Zone::Exile, events);
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 702.167a/b + CR 601.2b: Resolve a craft materials cost. The player has
/// chosen objects from the battlefield/graveyard union; validate the
/// count and legality, re-validate eligibility against the live state via the
/// single-authority `eligible_craft_materials`, exile each chosen object, then
/// resume the pending activation (whose remaining Mana + self-exile sub-costs
/// are paid by `push_activated_ability_to_stack`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_exile_materials_for_cost(
    state: &mut GameState,
    player: PlayerId,
    materials: TargetFilter,
    pending: PendingCast,
    bounds: (usize, usize),
    legal_cards: &[ObjectId],
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let still_eligible = super::cost_payability::eligible_craft_materials(
        state,
        player,
        pending.object_id,
        &materials,
    );
    // Capture the crafting source id before `pending` is moved into the helper.
    // The craft source self-exiles and returns with the same ObjectId (CR
    // 702.167a), so this id is also the returned permanent's id.
    let source_id = pending.object_id;
    // CR 702.167a/b + CR 601.2h: chosen materials are revalidated against the
    // live battlefield/graveyard union immediately before payment.
    let result = finish_exile_selection_for_cost(
        state,
        player,
        pending,
        bounds,
        legal_cards,
        chosen,
        events,
        "material(s)",
        "Selected object not eligible as craft material",
        move |_state, _player, id, _pending| {
            if !still_eligible.contains(&id) {
                return Err(EngineError::InvalidAction(
                    "Selected craft material is no longer eligible".to_string(),
                ));
            }
            Ok(())
        },
    )?;
    // CR 702.167c: link each exiled material to the crafting source so a
    // "cares what was used to craft it" ability can read them after the
    // permanent returns transformed (same ObjectId across the exile round-trip;
    // the link survives the source's battlefield exit via the zones.rs preserve
    // arm). `push_with_kind` is idempotent on (exiled_id, source_id).
    for &material_id in chosen {
        crate::game::exile_links::push_with_kind(
            state,
            material_id,
            source_id,
            crate::types::game_state::ExileLinkKind::CraftMaterial,
        );
    }
    Ok(result)
}

/// Push an activated ability to the stack after costs are paid.
/// Shared by: direct path in `handle_activate_ability`, sacrifice detour, and
/// waterbend/ManaPayment finalization in the PassPriority handler.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_activated_ability_to_stack(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    mut resolved: ResolvedAbility,
    remaining_cost: Option<&crate::types::ability::AbilityCost>,
    // CR 601.2f + CR 601.2h: POSITIVE signal that the caller took the `{X}`-mana
    // detour, so `remaining_cost` is the unpaid residual non-mana tail. Threaded
    // from `PendingCast::x_residual_activation` — set ONLY by that detour.
    x_residual: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Pay any activation-cost tail still outstanding. Cost-selection flows may
    // pass the original full cost; choice-based sub-costs already paid by a
    // WaitingFor handler are no-ops here.
    if let Some(cost) = remaining_cost {
        // CR 601.2f + CR 601.2h + CR 701.9a: When the X-mana detour ran first
        // (e.g. the Momir Basic emblem's `{X}, Discard a card` cost), the non-self
        // "discard a card" sub-cost is still OUTSTANDING here AFTER mana payment,
        // signalled by `x_residual`. In contrast, the discard-FIRST pre-check
        // detour in `handle_activate_ability` already paid the discard and resumes
        // with `x_residual == false` and the ORIGINAL cost — its discard sub-cost
        // is correctly no-op'd by `pay_ability_cost_for_activation` below. Gating
        // on the positive `x_residual` signal (not the cost shape) avoids
        // double-charging the discard on that pre-check path even when the cost is
        // a BARE `Discard` (which would otherwise fail with an empty hand).
        if let Some((count, filter)) =
            super::casting::find_non_self_discard(cost).filter(|_| x_residual)
        {
            let count =
                super::quantity::resolve_quantity(state, count, player, source_id).max(0) as usize;
            let eligible =
                super::casting::find_eligible_discard_targets(state, player, source_id, filter);
            if eligible.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard".into(),
                ));
            }
            let mut pending_discard =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_discard.activation_ability_index = Some(ability_index);
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::Discard,
                choices: eligible,
                count,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending_discard),
                },
            });
        }

        // CR 118.3 + CR 601.2h: The `{X}`-mana detour (`extract_x_mana_cost` in
        // `handle_activate_ability`) hoists X-first and leaves the residual tail
        // here, but ONLY the non-self `Discard` arm above re-surfaces an
        // interactive cost. A residual non-self SACRIFICE or non-self EXILE would
        // otherwise fall through to `pay_ability_cost_for_activation`, where both
        // are silent `Paid` no-ops (they rely on the pre-payment
        // SacrificeForCost / ExileForCost WaitingFor interception that the X-first
        // hoist bypassed) — silently SKIPPING the cost. No current card has an
        // `{X} + non-self-sacrifice/exile` activated ability (corpus scan = zero),
        // and this was already broken pre-hoist (X→0), so this is not a
        // regression — but a silent cost-swallow must not ship. Fail LOUDLY here.
        // A future `{X} + non-self-sacrifice/exile` activated ability must extend
        // this residual detour with Sacrifice/Exile arms mirroring the Discard arm
        // above (route to `WaitingFor::PayCost { kind: Sacrifice | ExileFromZone }`
        // with the residual stored in the resumed pending).
        if x_residual
            && (super::casting::find_non_self_sacrifice_cost(cost).is_some()
                || super::casting::find_non_self_exile(cost).is_some())
        {
            debug_assert!(
                false,
                "X-residual activation left a non-self sacrifice/exile cost unhandled \
                 (cost: {cost:?}); extend the residual detour in \
                 push_activated_ability_to_stack"
            );
            return Err(EngineError::ActionNotAllowed(
                "Unsupported activated-ability cost: {X} combined with a non-self \
                 sacrifice/exile is not yet implemented"
                    .into(),
            ));
        }

        // CR 606.3 + CR 606.5: Capture the symbolic `[−X]` loyalty shape before
        // chosen-X concretization turns it into a fixed counter-removal count.
        let should_record_loyalty = crate::types::ability::is_loyalty_ability_cost(cost);
        let concretized_cost;
        let cost = if let Some(chosen_x) = resolved.chosen_x {
            // CR 602.2b + CR 601.2f + CR 122.1: Once X is announced for an
            // activation cost, the symbolic counter-removal cost becomes a
            // concrete count before payment removes counters.
            concretized_cost = concretize_chosen_x_cost(cost, chosen_x);
            &concretized_cost
        } else {
            cost
        };
        if super::casting::variable_speed_payment_range(
            cost,
            super::speed::effective_speed(state, player),
        )
        .is_some()
        {
            return Ok(super::casting::begin_variable_speed_payment(
                state,
                player,
                source_id,
                resolved,
                cost.clone(),
                ability_index,
            ));
        }
        // CR 606.3: A `[−X]` loyalty ability is modeled as a chosen-X removal of
        // loyalty counters, so it finalizes through this X-cost path rather than
        // `handle_activate_loyalty`. Capture whether it is a loyalty activation
        // (before payment mutates loyalty) so the once-per-turn activation can be
        // recorded after a successful payment — mirroring the post-target path in
        // `pay_activation_costs_after_target_selection`.
        super::casting::stamp_self_ref_discard_cost_paid_object(
            state,
            source_id,
            &mut resolved,
            cost,
        );
        if should_record_loyalty
            && !super::planeswalker::can_activate_loyalty_ability(
                state,
                source_id,
                player,
                ability_index,
            )
        {
            return Err(EngineError::ActionNotAllowed(
                "Cannot activate loyalty ability".to_string(),
            ));
        }
        if let super::casting::PaymentOutcome::Paused { remaining_cost } =
            super::casting::pay_ability_cost_for_activation(state, player, source_id, cost, events)?
        {
            let mut pending = PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending.activation_cost = remaining_cost;
            pending.activation_ability_index = Some(ability_index);
            state.pending_cast = Some(Box::new(pending));
            return Ok(state.waiting_for.clone());
        }
        if should_record_loyalty {
            super::planeswalker::record_loyalty_activation(state, source_id, player);
        }
    }

    // CR 602.2b: Check if the ability has targets that need selection.
    // This handles cases where cost payment (sacrifice, waterbend) detoured
    // before target selection in handle_activate_ability.
    let target_slots = build_target_slots(state, &resolved)?;
    let assigned_targets = flatten_targets_in_chain(&resolved);
    if !target_slots.is_empty() && assigned_targets.len() >= target_slots.len() {
        emit_targeting_events(state, &assigned_targets, source_id, player, events);
        return push_ability_entry(state, player, source_id, ability_index, resolved, events);
    }
    if !target_slots.is_empty() {
        // CR 115.1 + CR 701.9b: Random-target activated abilities — game picks
        // uniformly via `state.rng`, no controller prompt.
        if matches!(
            resolved.target_selection_mode,
            crate::types::ability::TargetSelectionMode::Random
        ) {
            let targets = random_select_targets_for_ability(state, &target_slots, &[])?;
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;

            let assigned_targets = flatten_targets_in_chain(&resolved);
            emit_targeting_events(state, &assigned_targets, source_id, player, events);

            return push_ability_entry(state, player, source_id, ability_index, resolved, events);
        }

        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &[])?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;

            let assigned_targets = flatten_targets_in_chain(&resolved);
            emit_targeting_events(state, &assigned_targets, source_id, player, events);

            return push_ability_entry(state, player, source_id, ability_index, resolved, events);
        }

        // Targets need interactive selection
        let selection = begin_target_selection_for_ability(state, &resolved, &target_slots, &[])?;
        let mut pending_act = PendingCast::new(
            source_id,
            CardId(0),
            resolved,
            crate::types::mana::ManaCost::NoCost,
        );
        // CR 602.2b: The remainder of the process for activating an ability is
        // identical to the process for casting a spell listed in rules 601.2b–i.
        // Note: The engine currently pays non-mana costs (Tap, Sacrifice, etc.)
        // before target selection, which is a shortcut that deviates from the
        // strict CR 601.2 order (targets at 601.2c, costs at 601.2h). To prevent
        // double-payment when target selection resumes, we clear the activation
        // cost here — it was already consumed above (issue #897 class).
        pending_act.activation_cost = None;
        pending_act.activation_ability_index = Some(ability_index);
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_act),
            target_slots,
            mode_labels: Vec::new(),
            selection,
        });
    }

    emit_targeting_events(state, &assigned_targets, source_id, player, events);

    push_ability_entry(state, player, source_id, ability_index, resolved, events)
}

fn concretize_chosen_x_cost(cost: &AbilityCost, chosen_x: u32) -> AbilityCost {
    match cost {
        AbilityCost::RemoveCounter {
            count,
            counter_type,
            target,
            selection,
        } if is_chosen_remove_counter_cost_count(*count) => AbilityCost::RemoveCounter {
            count: chosen_x,
            counter_type: counter_type.clone(),
            target: target.clone(),
            selection: *selection,
        },
        AbilityCost::Exile {
            count: EXILE_COST_X,
            zone: Some(Zone::Graveyard),
            filter,
        } => AbilityCost::Exile {
            count: chosen_x,
            zone: Some(Zone::Graveyard),
            filter: filter.clone(),
        },
        AbilityCost::Composite { costs } => AbilityCost::Composite {
            costs: costs
                .iter()
                .map(|cost| concretize_chosen_x_cost(cost, chosen_x))
                .collect(),
        },
        _ => cost.clone(),
    }
}

/// Final step: create stack entry and record activation.
fn push_ability_entry(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    mut resolved: ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking.
    resolved.ability_index = Some(ability_index);
    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id,
                ability: resolved,
            },
        },
        events,
    );

    restrictions::record_ability_activation(state, source_id, ability_index);
    // CR 117.1b: Priority permits unbounded activation. `pending_activations`
    // is a per-priority-window AI-guard — see `GameState::pending_activations`.
    state.pending_activations.push((source_id, ability_index));
    events.push(GameEvent::AbilityActivated {
        player_id: player,
        source_id,
    });
    // CR 702.142b: Emit additional event when a boast ability is activated.
    super::casting_targets::emit_keyword_ability_event_if_tagged(
        state,
        source_id,
        ability_index,
        player,
        events,
    );
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Check for an additional cost on the object being cast. If one exists,
/// return `WaitingFor::OptionalCostChoice` so the player can decide;
/// otherwise proceed directly to `pay_and_push`.
///
/// This function sits between targeting and payment in the casting pipeline:
/// `CastSpell → [ModeChoice] → [TargetSelection] → [AdditionalCostChoice] → pay_and_push → Stack`
#[allow(clippy::too_many_arguments)]
pub(super) fn check_additional_cost_or_pay(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    base_cost: Option<ManaCost>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    check_additional_cost_or_pay_with_distribute(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        base_cost,
        casting_variant,
        cast_timing_permission,
        None,
        origin_zone,
        payment_mode,
        events,
    )
}

pub(super) fn finish_pending_cast_cost_or_pay(
    state: &mut GameState,
    player: PlayerId,
    mut pending: PendingCast,
    ability: ResolvedAbility,
    cost: ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    pending.ability = ability;
    pending.cost = cost;
    // If an optional additional cost was already decided (paid or declined) in the
    // deferred-target-selection flow, skip re-detection — the player already made
    // their choice. Without this guard, check_additional_cost_or_pay_with_distribute
    // would re-find the cost on obj.additional_cost and prompt for a second sacrifice.
    if pending.additional_cost_flow.is_some()
        || !pending.additional_cost_queue.is_empty()
        || pending.additional_cost_decided
    {
        return finish_pending_cost_or_cast(state, player, pending, events);
    }
    let object_id = pending.object_id;
    let card_id = pending.card_id;
    let casting_variant = pending.casting_variant;
    let cast_timing_permission = pending.cast_timing_permission;
    let distribute = pending.distribute;
    let origin_zone = pending.origin_zone;
    let payment_mode = pending.payment_mode;
    let base_cost = pending.base_cost;
    let cost = pending.cost;
    let ability = pending.ability;
    check_additional_cost_or_pay_with_distribute(
        state,
        player,
        object_id,
        card_id,
        ability,
        &cost,
        base_cost,
        casting_variant,
        cast_timing_permission,
        distribute,
        origin_zone,
        payment_mode,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin_modal_additional_cost_declaration(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: ManaCost,
    base_cost: Option<ManaCost>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    modal: crate::types::ability::ModalChoice,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let additional = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone());
    let Some(AdditionalCost::Kicker {
        costs,
        repeatability,
    }) = additional
    else {
        let mut capped =
            modal_choice_for_player(state, player, object_id, &modal, &ability.context);
        capped.max_choices = capped.max_choices.min(capped.mode_count);
        let mut pending = PendingCast::new(object_id, card_id, ability, cost);
        pending.base_cost = base_cost;
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.target_constraints = target_constraints_from_modal(&capped);
        let mode_abilities = state
            .objects
            .get(&object_id)
            .map(super::ability_utils::modal_spell_mode_abilities)
            .unwrap_or_default();
        let unavailable_modes = super::ability_utils::spell_modal_unavailable_modes(
            state,
            object_id,
            player,
            &capped,
            &mode_abilities,
        );
        return Ok(WaitingFor::ModeChoice {
            player,
            modal: capped,
            pending_cast: Box::new(pending),
            unavailable_modes,
        });
    };

    let mut pending = PendingCast::new(object_id, card_id, ability, cost);
    pending.base_cost = base_cost;
    pending.casting_variant = casting_variant;
    pending.cast_timing_permission = cast_timing_permission;
    pending.distribute = distribute;
    pending.origin_zone = origin_zone;
    pending.payment_mode = payment_mode;
    pending.deferred_modal_choice = Some(modal);
    pending.additional_cost_flow = Some(AdditionalCost::Kicker {
        costs,
        repeatability,
    });
    finish_pending_cost_or_cast(state, player, pending, events)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin_target_dependent_additional_cost_declaration(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: ManaCost,
    base_cost: Option<ManaCost>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let additional = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone());
    let Some(AdditionalCost::Kicker {
        costs,
        repeatability,
    }) = additional
    else {
        return pay_and_push(
            state,
            player,
            object_id,
            card_id,
            ability,
            &cost,
            base_cost,
            casting_variant,
            cast_timing_permission,
            distribute,
            origin_zone,
            payment_mode,
            events,
        );
    };

    let mut pending = PendingCast::new(object_id, card_id, ability, cost);
    pending.base_cost = base_cost;
    pending.casting_variant = casting_variant;
    pending.cast_timing_permission = cast_timing_permission;
    pending.distribute = distribute;
    pending.origin_zone = origin_zone;
    pending.payment_mode = payment_mode;
    pending.deferred_target_selection = true;
    pending.additional_cost_flow = Some(AdditionalCost::Kicker {
        costs,
        repeatability,
    });
    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 601.2b: Present an optional additional cost (e.g. Casualty) to the player
/// BEFORE target selection. Creates a PendingCast with deferred_target_selection = true
/// so targets are chosen after the cost decision and any required sacrifice.
#[allow(clippy::too_many_arguments)]
pub(super) fn begin_optional_cost_before_targets(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: ManaCost,
    base_cost: Option<ManaCost>,
    optional_cost: AdditionalCost,
    cost_source: SpellCostSource,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut pending = PendingCast::new(object_id, card_id, ability, cost);
    pending.base_cost = base_cost;
    pending.casting_variant = casting_variant;
    pending.cast_timing_permission = cast_timing_permission;
    pending.distribute = distribute;
    pending.origin_zone = origin_zone;
    pending.payment_mode = payment_mode;
    pending.deferred_target_selection = true;
    pending.additional_cost_flow = Some(optional_cost);
    pending.additional_cost_source = cost_source;
    finish_pending_cost_or_cast(state, player, pending, events)
}

/// CR 601.2b: X in a variable additional cost is announced before later target choices.
pub(super) fn required_additional_cost_can_declare_x(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<AbilityCost> {
    let Some(AdditionalCost::Required(cost)) = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone())
    else {
        return None;
    };
    additional_cost_x_max(state, player, object_id, &cost)
        .is_some()
        .then_some(cost)
}

/// CR 601.2b: Some required additional costs announce X before targets are chosen.
/// CR 601.2c: Target choices are deferred until that required cost X is known.
/// CR 601.2f: The shared payment step then determines and pays the final total cost.
#[allow(clippy::too_many_arguments)]
pub(super) fn begin_required_cost_before_targets(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: ManaCost,
    base_cost: Option<ManaCost>,
    required_cost: AbilityCost,
    cost_source: SpellCostSource,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut pending = PendingCast::new(object_id, card_id, ability, cost);
    pending.base_cost = base_cost;
    pending.casting_variant = casting_variant;
    pending.cast_timing_permission = cast_timing_permission;
    pending.distribute = distribute;
    pending.origin_zone = origin_zone;
    pending.payment_mode = payment_mode;
    pending.deferred_target_selection = true;
    pending.additional_cost_flow = Some(AdditionalCost::Required(required_cost));
    pending.additional_cost_source = cost_source;
    finish_pending_cost_or_cast(state, player, pending, events)
}

fn combined_imposed_additional_cast_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    ability: &ResolvedAbility,
) -> Option<AbilityCost> {
    let imposed_costs =
        super::casting::collect_imposed_additional_cast_costs(state, player, object_id, ability);
    match imposed_costs.len() {
        0 => None,
        1 => imposed_costs.into_iter().next(),
        _ => Some(AbilityCost::Composite {
            costs: imposed_costs,
        }),
    }
}

fn merge_required_additional_cost(
    additional: Option<AdditionalCost>,
    imposed: Option<AbilityCost>,
) -> Option<AdditionalCost> {
    match (additional, imposed) {
        (Some(AdditionalCost::Required(required)), Some(imposed)) => Some(
            AdditionalCost::Required(merge_required_cost(required, Some(imposed))),
        ),
        (Some(additional), _) => Some(additional),
        (None, Some(imposed)) => Some(AdditionalCost::Required(imposed)),
        (None, None) => None,
    }
}

fn merge_required_cost(required: AbilityCost, imposed: Option<AbilityCost>) -> AbilityCost {
    let Some(imposed) = imposed else {
        return required;
    };
    match (required, imposed) {
        (AbilityCost::Composite { mut costs }, AbilityCost::Composite { costs: imposed }) => {
            costs.extend(imposed);
            AbilityCost::Composite { costs }
        }
        (AbilityCost::Composite { mut costs }, imposed) => {
            costs.push(imposed);
            AbilityCost::Composite { costs }
        }
        (required, AbilityCost::Composite { costs: imposed }) => {
            let mut costs = Vec::with_capacity(imposed.len() + 1);
            costs.push(required);
            costs.extend(imposed);
            AbilityCost::Composite { costs }
        }
        (required, imposed) => AbilityCost::Composite {
            costs: vec![required, imposed],
        },
    }
}

fn required_cost_from_additional(additional: Option<AdditionalCost>) -> Option<AbilityCost> {
    match additional {
        Some(AdditionalCost::Required(cost)) => Some(cost),
        _ => None,
    }
}

/// CR 601.2d: Extended version of `check_additional_cost_or_pay` that threads the
/// `distribute` flag through PendingCast creation so X-spell distribution
/// survives to the `(ManaPayment, PassPriority)` handler.
#[allow(clippy::too_many_arguments)]
pub(super) fn check_additional_cost_or_pay_with_distribute(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    base_cost: Option<ManaCost>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 601.3d + CR 702.8a: When the cast was authorized as-though-it-had-flash
    // via a target-dependent `SpellCastingOption.condition`, re-validate
    // against the just-committed targets BEFORE any additional cost (sacrifice,
    // discard, pay-life) is paid. Timely Ward — "you may cast this spell as
    // though it had flash if it targets a commander" — must fail the cast
    // before any cost is committed if the chosen targets do not satisfy the
    // gating condition; otherwise the player would forfeit additional-cost
    // resources for an illegal cast. We perform the same check again at
    // `finalize_cast_with_phyrexian_choices` so the canonical terminus is
    // closed even for flows that bypass this entry point.
    if cast_timing_permission == Some(CastTimingPermission::AsThoughHadFlash)
        && !super::restrictions::target_dependent_flash_permission_satisfied(
            state, player, object_id, &ability,
        )
    {
        let pending_for_cancel = PendingCast::new(object_id, card_id, ability, cost.clone());
        super::casting::handle_cancel_cast(state, &pending_for_cancel, events);
        return Err(EngineError::ActionNotAllowed(
            "Chosen targets do not satisfy the flash casting condition".to_string(),
        ));
    }

    // CR 601.2f: Strive cost increase + target-dependent self/battlefield cost
    // modifiers, applied once targets are chosen (CR 601.2c) and costs are
    // determined (CR 601.2f). Floors are excluded from the helper so they can
    // run LAST below.
    let mut target_adjusted_cost = cost.clone();
    super::casting::apply_target_dependent_cost_modifiers(
        state,
        player,
        object_id,
        &ability,
        &mut target_adjusted_cost,
    );
    // CR 601.2b + CR 601.2f: Cost-floor statics (Trinisphere) apply last, after
    // all additive/subtractive modifiers including target-dependent ones. For
    // `{X}` costs the floor is deferred until X is concretized (mana value 0
    // while symbolic would over-count) — see `apply_post_x_cost_modifiers`.
    if !cost_has_x(&target_adjusted_cost) {
        super::casting::apply_cost_floor_with_selected_targets(
            state,
            player,
            object_id,
            &ability,
            &mut target_adjusted_cost,
        );
    }
    let cost = &target_adjusted_cost;

    let flash_additional =
        flash_timing_non_mana_additional_cost(state, player, object_id, cast_timing_permission);
    let obj_additional = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone())
        .or(flash_additional);
    let imposed_required_cost =
        combined_imposed_additional_cast_cost(state, player, object_id, &ability);

    // CR 601.2b/f + CR 113.2c: non-kicker keyword additional costs with
    // independently functioning instances are announced through a queue. This
    // preserves one payment record per Casualty/Offspring/Squad/Replicate
    // instance while leaving Kicker on its existing `kickers_paid` path.
    let mut additional_cost_queue = Vec::new();
    additional_cost_queue.extend(effective_casualty_additional_cost_instances(
        state, player, object_id,
    ));
    additional_cost_queue.extend(effective_offspring_additional_cost_instances(
        state, player, object_id,
    ));
    additional_cost_queue.extend(effective_squad_additional_cost_instances(
        state, player, object_id,
    ));
    additional_cost_queue.extend(effective_replicate_additional_cost_instances(
        state, player, object_id,
    ));
    let obj_additional_matches_instance = obj_additional.as_ref().is_some_and(|cost| {
        additional_cost_queue
            .iter()
            .any(|instance| instance.cost == *cost)
    });
    let legacy_obj_additional = if obj_additional_matches_instance {
        None
    } else {
        obj_additional.clone()
    };
    let offering_additional = effective_offering_additional_cost(state, player, object_id);
    let conspire_additional = effective_conspire_additional_cost(state, player, object_id);

    let (additional, deferred_required, additional_cost_source) =
        if let Some(AdditionalCost::Required(ref req)) = legacy_obj_additional {
            if !additional_cost_queue.is_empty() {
                if !req.is_payable(state, player, object_id) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay required additional cost".to_string(),
                    ));
                }
                let deferred = merge_required_additional_cost(
                    legacy_obj_additional,
                    imposed_required_cost.clone(),
                );
                (None, deferred, SpellCostSource::Other)
            } else {
                let additional = merge_required_additional_cost(
                    legacy_obj_additional,
                    imposed_required_cost.clone(),
                );
                (additional, None, SpellCostSource::Other)
            }
        } else if legacy_obj_additional.is_some() {
            (
                legacy_obj_additional,
                imposed_required_cost.clone().map(AdditionalCost::Required),
                SpellCostSource::Other,
            )
        } else if !additional_cost_queue.is_empty() {
            (
                None,
                imposed_required_cost.clone().map(AdditionalCost::Required),
                SpellCostSource::Other,
            )
        } else if let Some(offering) = offering_additional {
            // CR 702.48a: Offering — optional sacrifice before target selection
            // (becomes Required when cast via Offering instant-speed timing; that
            // case is handled in the casting dispatch which routes to
            // `begin_required_cost_before_targets` before this function is reached).
            (
                Some(offering),
                imposed_required_cost.clone().map(AdditionalCost::Required),
                SpellCostSource::Offering,
            )
        } else if let Some(conspire) = conspire_additional {
            // CR 702.78a: statics-granted Conspire (Wort, the Raidmother /
            // Rassilon, the War President). Printed Conspire sets
            // `obj.additional_cost` and is caught by the `obj_additional.is_some()`
            // arm above, so this arm fires only for the granted path.
            (
                Some(conspire),
                imposed_required_cost.clone().map(AdditionalCost::Required),
                SpellCostSource::Other,
            )
        } else {
            (None, None, SpellCostSource::Other)
        };

    // CR 118.9 + CR 601.2b/f/h: Oracle text alternative costs are announced
    // before total cost determination and paid rather than the spell's mana
    // cost. Reuse the existing `AdditionalCost::Choice` prompt shape by making
    // the pending spell mana cost `NoCost`: accepting pays the alternative cost,
    // declining pays the printed mana cost as the fallback branch.
    if casting_variant == CastingVariant::Normal {
        let alt_cost = cast_timing_permission
            .and_then(|permission| {
                payable_spell_alternative_cost_for_timing(state, player, object_id, permission)
            })
            .or_else(|| payable_spell_alternative_cost_details(state, player, object_id));
        if let Some(alt_cost) = alt_cost {
            let mut pending = PendingCast::new(object_id, card_id, ability, ManaCost::NoCost);
            pending.base_cost = base_cost.clone();
            pending.casting_variant = casting_variant;
            pending.cast_timing_permission = cast_timing_permission;
            pending.distribute = distribute.clone();
            pending.origin_zone = origin_zone;
            pending.payment_mode = payment_mode;
            pending.additional_cost_flow =
                imposed_required_cost.clone().map(AdditionalCost::Required);
            let alt_cost_required_for_timing = cast_timing_permission.is_some()
                && alt_cost.timing_permission == cast_timing_permission;
            if alt_cost_required_for_timing {
                if matches!(alt_cost.cost, AbilityCost::Mana { .. }) {
                    pending.ability.context.alternative_mana_cost_paid = true;
                }
                return pay_additional_cost_with_source(
                    state,
                    player,
                    alt_cost.cost,
                    SpellCostSource::Other,
                    pending,
                    events,
                );
            }
            return Ok(WaitingFor::OptionalCostChoice {
                player,
                cost: AdditionalCost::Choice(
                    alt_cost.cost,
                    AbilityCost::Mana { cost: cost.clone() },
                ),
                times_kicked: 0,
                pending_cast: Box::new(pending),
            });
        }
    }

    if !additional_cost_queue.is_empty() {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute.clone();
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.additional_cost_queue = additional_cost_queue;
        pending.additional_cost_flow = additional.clone().or(deferred_required);
        pending.additional_cost_source = additional_cost_source;
        return finish_pending_cost_or_cast(state, player, pending, events);
    }

    if let Some(additional_cost) = additional {
        match &additional_cost {
            AdditionalCost::Required(req_cost) => {
                // CR 601.2b: Required additional cost whose choice-of-object is
                // unavailable makes the spell uncastable.
                if !req_cost.is_payable(state, player, object_id) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay required additional cost".to_string(),
                    ));
                }
                // Required additional costs bypass the choice prompt — pay directly.
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.base_cost = base_cost.clone();
                pending.casting_variant = casting_variant;
                pending.cast_timing_permission = cast_timing_permission;
                pending.origin_zone = origin_zone;
                pending.payment_mode = payment_mode;
                return pay_additional_cost_with_source(
                    state,
                    player,
                    req_cost.clone(),
                    additional_cost_source,
                    pending,
                    events,
                );
            }
            AdditionalCost::Kicker {
                costs,
                repeatability,
            } => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.base_cost = base_cost.clone();
                pending.casting_variant = casting_variant;
                pending.cast_timing_permission = cast_timing_permission;
                pending.distribute = distribute.clone();
                pending.origin_zone = origin_zone;
                pending.payment_mode = payment_mode;
                pending.deferred_required_additional_cost =
                    required_cost_from_additional(deferred_required.clone());
                pending.additional_cost_flow = Some(AdditionalCost::Kicker {
                    costs: costs.clone(),
                    repeatability: *repeatability,
                });
                if !pending.ability.context.kickers_paid.is_empty() {
                    pending.declared_kickers_to_pay = pending
                        .ability
                        .context
                        .kickers_paid
                        .iter()
                        .rev()
                        .copied()
                        .collect();
                }
                return finish_pending_cost_or_cast(state, player, pending, events);
            }
            AdditionalCost::Optional {
                cost: repeatable_cost,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
            } => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.base_cost = base_cost.clone();
                pending.casting_variant = casting_variant;
                pending.cast_timing_permission = cast_timing_permission;
                pending.distribute = distribute.clone();
                pending.origin_zone = origin_zone;
                pending.payment_mode = payment_mode;
                pending.deferred_required_additional_cost =
                    required_cost_from_additional(deferred_required.clone());
                pending.additional_cost_flow = Some(AdditionalCost::Optional {
                    cost: repeatable_cost.clone(),
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                });
                return finish_pending_cost_or_cast(state, player, pending, events);
            }
            AdditionalCost::Optional {
                cost: opt_cost,
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            } => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.base_cost = base_cost.clone();
                pending.casting_variant = casting_variant;
                pending.cast_timing_permission = cast_timing_permission;
                pending.distribute = distribute.clone();
                pending.origin_zone = origin_zone;
                pending.payment_mode = payment_mode;
                pending.additional_cost_source = additional_cost_source;
                // When a Required cost was deferred so Casualty could be offered first
                // (e.g., Village Rites + Casualty), stash it so finish_pending_cost_or_cast
                // can pay it after the Casualty decision.
                pending.additional_cost_flow = deferred_required;
                // CR 601.2b: If the optional additional cost requires a choice
                // of object and no legal object exists, skip the prompt and
                // proceed as if the player declined to pay.
                if !opt_cost.is_payable(state, player, object_id) {
                    return finish_pending_cost_or_cast(state, player, pending, events);
                }
                return Ok(WaitingFor::OptionalCostChoice {
                    player,
                    cost: additional_cost,
                    times_kicked: 0,
                    pending_cast: Box::new(pending),
                });
            }
            AdditionalCost::Choice(preferred, fallback) => {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.base_cost = base_cost.clone();
                pending.casting_variant = casting_variant;
                pending.cast_timing_permission = cast_timing_permission;
                pending.distribute = distribute;
                pending.origin_zone = origin_zone;
                pending.payment_mode = payment_mode;
                pending.additional_cost_flow =
                    imposed_required_cost.clone().map(AdditionalCost::Required);
                // CR 601.2b: If the preferred branch is unpayable, fall through
                // to the fallback without prompting. If both are unpayable, the
                // spell cannot be cast.
                if !preferred.is_payable(state, player, object_id) {
                    if !fallback.is_payable(state, player, object_id) {
                        return Err(EngineError::ActionNotAllowed(
                            "Cannot pay either alternative additional cost".to_string(),
                        ));
                    }
                    return pay_additional_cost(state, player, fallback.clone(), pending, events);
                }
                return Ok(WaitingFor::OptionalCostChoice {
                    player,
                    cost: additional_cost,
                    times_kicked: 0,
                    pending_cast: Box::new(pending),
                });
            }
        }
    }

    // CR 107.14: If this is an energy-from-exile cast, pay energy before pushing to stack.
    let energy_cost = state.objects.get(&object_id).and_then(|obj| {
        if obj.zone == Zone::Exile
            && obj.casting_permissions.iter().any(|p| {
                matches!(
                    p,
                    crate::types::ability::CastingPermission::ExileWithEnergyCost
                )
            })
        {
            Some(obj.mana_cost.mana_value())
        } else {
            None
        }
    });
    if let Some(energy_mv) = energy_cost {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.additional_cost_flow = imposed_required_cost.clone().map(AdditionalCost::Required);
        return pay_additional_cost(
            state,
            player,
            AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed {
                    value: energy_mv as i32,
                },
            },
            pending,
            events,
        );
    }

    // CR 118.9 + CR 119.4: ExileWithAltAbilityCost — non-mana alternative cost
    // (e.g. Nashi's "pay life equal to its mana value rather than paying its
    // mana cost"). The mana cost was already overridden to zero in
    // `casting::cast_spell` via `alt_cost_from_exile`; here we route the stored
    // `AbilityCost` through `pay_additional_cost` so dynamic-quantity refs
    // (`ObjectManaValue { CostPaidObject }`, etc.) resolve at cast time
    // against the spell's mana value. Single-authority — `AbilityCost::PayLife` and friends
    // are paid through the same pipeline as flashback's non-mana cost.
    let alt_ability_cost = state.objects.get(&object_id).and_then(|obj| {
        if obj.zone == Zone::Exile {
            // CR 611.2a: Match the grantee filter used by
            // `prepare_spell_cast_with_variant_override` so the alt-ability
            // cost is only consumed by the granted player.
            obj.casting_permissions.iter().find_map(|p| match p {
                crate::types::ability::CastingPermission::ExileWithAltAbilityCost {
                    cost,
                    granted_to,
                    ..
                } if granted_to.is_none() || *granted_to == Some(player) => Some(cost.clone()),
                _ => None,
            })
        } else if obj.zone == Zone::Library && obj.owner == player {
            // CR 401.5 + CR 118.9 + CR 601.2a: Top-of-library cast with an
            // alt-cost rider (Bolas's Citadel: "pay life equal to its mana
            // value rather than paying its mana cost").
            super::casting::top_of_library_alt_ability_cost_for_object(state, player, object_id)
        } else {
            None
        }
    });
    if let Some(alt_cost) = alt_ability_cost {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.additional_cost_flow = imposed_required_cost.clone().map(AdditionalCost::Required);
        return pay_additional_cost(state, player, alt_cost, pending, events);
    }

    // CR 702.138a: Escape's additional cost is the residual after extracting the
    // mana sub-cost. Usually "Exile N other cards from your graveyard"; may be a
    // Composite of multiple exile clauses (Lunar Hatchling: "Exile a land you
    // control, Exile five other cards from your graveyard"). Paid one sub-cost at
    // a time by `pay_additional_cost`'s Composite arm (CR 601.2h).
    if casting_variant == CastingVariant::Escape {
        if let Some((_, residual)) = super::keywords::effective_escape_data(state, object_id) {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.base_cost = base_cost.clone();
            pending.casting_variant = casting_variant;
            pending.cast_timing_permission = cast_timing_permission;
            pending.origin_zone = origin_zone;
            pending.payment_mode = payment_mode;
            pending.additional_cost_flow =
                imposed_required_cost.clone().map(AdditionalCost::Required);
            return pay_additional_cost(state, player, residual, pending, events);
        }
    }

    // CR 702.81a: Retrace requires discarding a land card as an additional
    // cost, then paying the card's normal mana cost.
    if casting_variant == CastingVariant::Retrace {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.additional_cost_flow = imposed_required_cost.clone().map(AdditionalCost::Required);
        return pay_additional_cost(state, player, retrace_discard_land_cost(), pending, events);
    }

    // CR 702.133a: Jump-start requires discarding a card (any card) as an
    // additional cost, then paying the card's normal mana cost.
    if casting_variant == CastingVariant::JumpStart {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.additional_cost_flow = imposed_required_cost.clone().map(AdditionalCost::Required);
        return pay_additional_cost(
            state,
            player,
            jumpstart_discard_card_cost(),
            pending,
            events,
        );
    }

    // CR 702.34a + CR 118.8: Flashback with a non-mana additional cost (Battle
    // Screech's "tap three white creatures") or a compound cost (Deep Analysis's
    // "{1}{U}, Pay 3 life") routes the residual non-mana sub-cost through
    // `pay_additional_cost`. The mana sub-cost (if any) was already extracted
    // into `cost` upstream by `split_flashback_cost_components` and is paid via
    // the normal mana-payment flow inside `pay_additional_cost`'s fall-through.
    if casting_variant == CastingVariant::Flashback {
        let flashback_cost = super::keywords::effective_flashback_cost(state, object_id);
        let (_mana, residual) =
            super::casting::split_flashback_cost_components(flashback_cost.as_ref());
        if let Some(non_mana_cost) = residual {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.base_cost = base_cost.clone();
            pending.casting_variant = casting_variant;
            pending.cast_timing_permission = cast_timing_permission;
            pending.distribute = distribute;
            pending.origin_zone = origin_zone;
            pending.payment_mode = payment_mode;
            pending.additional_cost_flow =
                imposed_required_cost.clone().map(AdditionalCost::Required);
            return pay_additional_cost(state, player, non_mana_cost, pending, events);
        }
    }

    // CR 702.74a + CR 118.9 + CR 601.2h: Evoke twin of the flashback branch
    // above. Non-mana evoke (Solitude — "Exile a white card from your hand.")
    // and any future compound mana+non-mana evoke route the residual non-mana
    // sub-cost through `pay_additional_cost` so it is paid alongside the
    // (potentially zero) mana sub-cost.
    if casting_variant == CastingVariant::Evoke {
        // CR 601.2h: non-mana evoke residual from effective keywords (granted
        // evoke).
        let evoke_split = super::casting::effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Evoke(ec) => {
                    Some(super::casting::split_evoke_cost_components(ec))
                }
                _ => None,
            });
        if let Some((_mana, Some(non_mana_cost))) = evoke_split {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.base_cost = base_cost.clone();
            pending.casting_variant = casting_variant;
            pending.cast_timing_permission = cast_timing_permission;
            pending.distribute = distribute;
            pending.origin_zone = origin_zone;
            pending.payment_mode = payment_mode;
            pending.additional_cost_flow =
                imposed_required_cost.clone().map(AdditionalCost::Required);
            return pay_additional_cost(state, player, non_mana_cost, pending, events);
        }
    }

    // CR 702.103a + CR 118.9 + CR 601.2h: Bestow twin of the Evoke branch above.
    // A compound bestow cost ("Bestow—{R}, Collect evidence 6." on Detective's
    // Phoenix) routes its residual non-mana sub-cost (Collect evidence) through
    // `pay_additional_cost`; the mana sub-cost ({R}) was already substituted as
    // the spell's mana cost in `prepare_spell_cast` and is paid through the
    // normal mana-payment flow inside `pay_additional_cost`'s fall-through.
    if casting_variant == CastingVariant::Bestow {
        let bestow_split = super::casting::effective_spell_keywords(state, player, object_id)
            .iter()
            .find_map(|k| match k {
                crate::types::keywords::Keyword::Bestow(bc) => {
                    Some(super::casting::split_bestow_cost_components(bc))
                }
                _ => None,
            });
        if let Some((_mana, Some(non_mana_cost))) = bestow_split {
            let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
            pending.base_cost = base_cost.clone();
            pending.casting_variant = casting_variant;
            pending.cast_timing_permission = cast_timing_permission;
            pending.distribute = distribute;
            pending.origin_zone = origin_zone;
            pending.payment_mode = payment_mode;
            pending.additional_cost_flow =
                imposed_required_cost.clone().map(AdditionalCost::Required);
            return pay_additional_cost(state, player, non_mana_cost, pending, events);
        }
    }

    // CR 601.2b: Check for Defiler cost reduction — optional life payment for colored mana
    // reduction on matching-color permanent spells.
    if let Some((life_cost, mana_reduction)) = find_defiler_reduction(state, player, object_id) {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.additional_cost_flow = imposed_required_cost.clone().map(AdditionalCost::Required);
        return Ok(WaitingFor::DefilerPayment {
            player,
            life_cost,
            mana_reduction,
            pending_cast: Box::new(pending),
        });
    }

    if let Some(imposed_cost) = imposed_required_cost {
        if !imposed_cost.is_payable(state, player, object_id) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay imposed additional cost".to_string(),
            ));
        }
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        return pay_additional_cost(state, player, imposed_cost, pending, events);
    }

    let waiting_for = pay_and_push(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        base_cost,
        casting_variant,
        cast_timing_permission,
        distribute,
        origin_zone,
        payment_mode,
        events,
    )?;
    Ok(drain_deferred_triggers_after_stack_object_announcement(
        state,
        events,
        waiting_for,
    ))
}

fn flash_timing_non_mana_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    cast_timing_permission: Option<CastTimingPermission>,
) -> Option<AdditionalCost> {
    if cast_timing_permission != Some(CastTimingPermission::AsThoughHadFlash) {
        return None;
    }
    state
        .objects
        .get(&object_id)?
        .casting_options
        .iter()
        .find_map(|option| {
            if option.kind != SpellCastingOptionKind::AsThoughHadFlash {
                return None;
            }
            if option.condition.as_ref().is_some_and(|condition| {
                !restrictions::evaluate_condition(state, player, object_id, condition)
            }) {
                return None;
            }
            let cost = option.cost.clone()?;
            if matches!(cost, AbilityCost::Mana { .. }) {
                return None;
            }
            cost.is_payable(state, player, object_id)
                .then_some(AdditionalCost::Required(cost))
        })
}

/// CR 601.2b: Find the first applicable Defiler cost reduction for a spell being cast.
/// Returns `Some((life_cost, mana_reduction))` if a controlled Defiler permanent has
/// `DefilerCostReduction` matching one of the spell's colors and the spell is a permanent spell.
fn find_defiler_reduction(
    state: &GameState,
    caster: PlayerId,
    spell_id: ObjectId,
) -> Option<(u32, crate::types::mana::ManaCost)> {
    use crate::types::statics::StaticMode;

    let spell = state.objects.get(&spell_id)?;

    // Defiler only applies to permanent spells (not instants/sorceries)
    let is_permanent = spell.card_types.core_types.iter().any(|ct| {
        matches!(
            ct,
            crate::types::card_type::CoreType::Creature
                | crate::types::card_type::CoreType::Artifact
                | crate::types::card_type::CoreType::Enchantment
                | crate::types::card_type::CoreType::Planeswalker
        )
    });
    if !is_permanent {
        return None;
    }

    let spell_colors = &spell.color;
    if spell_colors.is_empty() {
        return None;
    }

    // CR 702.26b + CR 604.1: `battlefield_active_statics` owns the gating.
    for (bf_obj, def) in super::functioning_abilities::battlefield_active_statics(state) {
        if bf_obj.controller != caster {
            continue;
        }
        {
            if let StaticMode::DefilerCostReduction {
                color,
                life_cost,
                mana_reduction,
            } = &def.mode
            {
                if spell_colors.contains(color) {
                    // CR 118.3 + CR 119.4b + CR 119.8: Don't offer the Defiler
                    // prompt when the caster can't actually pay the life — this
                    // keeps the UI from presenting an impossible choice.
                    if !super::life_costs::can_pay_life_cast_or_activation_cost(
                        state, caster, *life_cost,
                    ) {
                        return None;
                    }
                    return Some((*life_cost, mana_reduction.clone()));
                }
            }
        }
    }

    None
}

/// CR 601.2b: Handle the player's decision on Defiler life payment.
/// If accepted, pays life and reduces the spell's mana cost, then continues to mana payment.
/// If declined, continues with the original cost.
pub(crate) fn handle_defiler_payment(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    life_cost: u32,
    mana_reduction: &crate::types::mana::ManaCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut cost = pending.cost.clone();

    if pay {
        // CR 118.3b + CR 119.4 + CR 119.8: Defiler's optional life payment is a
        // cost — route through the single-authority helper so the replacement
        // pipeline and CantLoseLife lock are honored. If the cost can't be paid
        // (insufficient life or locked), fall through to casting without the
        // reduction — the Defiler prompt must not half-apply.
        let payment = super::life_costs::pay_life_as_cast_or_activation_cost(
            state, player, life_cost, events,
        );
        let reduction_applied = payment.is_paid();
        match payment {
            PayLifeCostResult::Paid { .. } => {}
            PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => {
                // Proceed with the original cost; no reduction.
            }
        }
        if !reduction_applied {
            let base_cost = pending.base_cost.clone();
            return pay_and_push(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &cost,
                base_cost,
                pending.casting_variant,
                pending.cast_timing_permission,
                pending.distribute,
                pending.origin_zone,
                pending.payment_mode,
                events,
            );
        }

        // Reduce mana cost — remove matching colored shards from the spell cost
        if let (
            crate::types::mana::ManaCost::Cost {
                shards: spell_shards,
                ..
            },
            crate::types::mana::ManaCost::Cost {
                shards: reduction_shards,
                generic: reduction_generic,
            },
        ) = (&mut cost, mana_reduction)
        {
            // Remove colored shards from spell cost that match the reduction
            for shard in reduction_shards {
                if let Some(pos) = spell_shards.iter().position(|s| s == shard) {
                    spell_shards.remove(pos);
                }
            }
            // Also reduce generic if the reduction specifies generic mana
            if let crate::types::mana::ManaCost::Cost {
                generic: spell_generic,
                ..
            } = &mut cost
            {
                *spell_generic = spell_generic.saturating_sub(*reduction_generic);
            }
        }
    }

    let base_cost = pending.base_cost.clone();
    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &cost,
        base_cost,
        pending.casting_variant,
        pending.cast_timing_permission,
        pending.distribute,
        pending.origin_zone,
        pending.payment_mode,
        events,
    )
}

/// CR 601.2b: Pay an additional cost, returning a WaitingFor if interactive input is needed
/// (e.g. choosing which card to discard), or continuing to pay_and_push if atomic.
fn pay_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    cost: AbilityCost,
    pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    pay_additional_cost_with_source(state, player, cost, SpellCostSource::Other, pending, events)
}

fn pay_additional_cost_with_source(
    state: &mut GameState,
    player: PlayerId,
    cost: AbilityCost,
    cost_source: SpellCostSource,
    pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if pending.ability.chosen_x.is_none() {
        if let Some(max) = additional_cost_x_max(state, player, pending.object_id, &cost) {
            let min = pending.ability.min_x_value;
            if min > max {
                super::casting::handle_cancel_cast(state, &pending, events);
                return Err(EngineError::ActionNotAllowed(format!(
                    "Minimum legal X value {min} exceeds maximum payable X value {max}"
                )));
            }
            let mut pending = pending;
            let cost = prepend_deferred_required_cost(cost, &mut pending);
            pending.additional_cost_flow = Some(AdditionalCost::Required(cost));
            state.pending_cast = Some(Box::new(pending.clone()));
            return Ok(WaitingFor::ChooseXValue {
                player,
                min,
                max,
                pending_cast: Box::new(pending),
                convoke_mode: None,
            });
        }
    }

    let cost = if let Some(chosen_x) = pending.ability.chosen_x {
        concretize_chosen_x_cost(&cost, chosen_x)
    } else {
        cost
    };

    match cost {
        AbilityCost::PayLife { amount } => {
            // CR 118.3 + CR 119.4 + CR 119.8: Pay life as an additional cost via
            // the single-authority helper. Unpayable = spell cannot be cast.
            // CR 119.4 + CR 903.4: `amount` is a QuantityExpr so dynamic refs
            // (e.g. commander color identity count) resolve at cast time.
            let resolved =
                super::quantity::resolve_quantity_with_targets(state, &amount, &pending.ability)
                    .max(0) as u32;
            match super::life_costs::pay_life_as_cast_or_activation_cost(
                state, player, resolved, events,
            ) {
                PayLifeCostResult::Paid { .. } => {}
                PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot pay life cost".to_string(),
                    ));
                }
            }
        }
        AbilityCost::Blight { count } => {
            // Blight N — player chooses creature(s) to put -1/-1 counters on.
            // Per reminder text: "(You may put a -1/-1 counter on a creature you control.)"
            let creatures: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && obj
                                .card_types
                                .core_types
                                .contains(&crate::types::card_type::CoreType::Creature)
                    })
                })
                .collect();
            // CR 701.68b + CR 601.2b: Blight is only choosable while the player
            // controls >=1 creature (N is irrelevant to eligibility). Defense-in-depth
            // — the is_payable gate must have already caught an empty eligibility set;
            // never construct a dead WaitingFor.
            if creatures.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No creature to blight".to_string(),
                ));
            }
            return Ok(WaitingFor::BlightChoice {
                player,
                counters: count,
                creatures,
                pending_cast: Box::new(pending),
            });
        }
        AbilityCost::Behold {
            count,
            ref filter,
            action,
        } => {
            let choices = eligible_behold_choices(state, player, pending.object_id, filter);
            if choices.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "No eligible object to behold".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::Behold { action },
                choices,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        AbilityCost::Discard { count, filter, .. } => {
            let count = super::quantity::resolve_quantity(state, &count, player, pending.object_id)
                .max(0) as usize;
            // CR 601.2b: Discard requires interactive card selection — return a WaitingFor.
            let eligible = super::casting::find_eligible_discard_targets(
                state,
                player,
                pending.object_id,
                filter.as_ref(),
            );
            // CR 601.2b: Defense-in-depth — empty hand means no legal choice.
            if eligible.len() < count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough cards in hand to discard".to_string(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::Discard,
                choices: eligible,
                count,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        AbilityCost::Mana { cost: mana_cost } => {
            // Add mana cost to the pending payment (handled by pay_and_push → pay_mana_cost)
            let combined = super::restrictions::add_mana_cost(&pending.cost, &mana_cost);
            return finish_pending_cost_or_cast(
                state,
                player,
                PendingCast {
                    cost: combined,
                    ..pending
                },
                events,
            );
        }
        AbilityCost::Sacrifice(cost) => {
            let target = &cost.target;
            let SacrificeRequirement::Count { count } = cost.requirement else {
                return Err(EngineError::ActionNotAllowed(
                    "Unsupported sacrifice cost requirement for spell payment".into(),
                ));
            };
            if matches!(target, crate::types::ability::TargetFilter::SelfRef) {
                if super::static_abilities::player_cant_sacrifice_as_cost(
                    state,
                    player,
                    pending.object_id,
                ) {
                    return Err(EngineError::ActionNotAllowed(
                        "Cannot sacrifice this permanent as a cost".into(),
                    ));
                }
                // CR 118.3: Self-sacrifice is atomic — no player choice needed
                super::sacrifice::sacrifice_permanent(state, pending.object_id, player, events)
                    .map_err(|e| EngineError::InvalidAction(format!("{e}")))?;
            } else {
                // CR 118.3: Non-self sacrifice needs interactive selection
                let eligible = super::casting::find_eligible_sacrifice_targets(
                    state,
                    player,
                    pending.object_id,
                    target,
                );
                let (min_count, max_count) = super::casting::sacrifice_cost_bounds_with_chosen_x(
                    count,
                    eligible.len(),
                    pending.ability.chosen_x,
                );
                if eligible.len() < min_count {
                    return Err(EngineError::ActionNotAllowed(
                        "Not enough eligible permanents to sacrifice".into(),
                    ));
                }
                return Ok(WaitingFor::PayCost {
                    player,
                    kind: PayCostKind::Sacrifice,
                    choices: eligible,
                    count: max_count,
                    min_count,
                    resume: CostResume::SpellCost {
                        spell: Box::new(pending),
                        cost: Box::new(AbilityCost::Sacrifice(SacrificeCost::count(
                            target.clone(),
                            count,
                        ))),
                        source: cost_source,
                    },
                });
            }
        }
        AbilityCost::ReturnToHand {
            count,
            ref filter,
            from_zone: _,
        } => {
            let eligible = super::casting::find_eligible_return_to_hand_targets(
                state,
                player,
                pending.object_id,
                filter.as_ref(),
            );
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to return".into(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ReturnToHand,
                choices: eligible,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        AbilityCost::RemoveCounter {
            count,
            ref counter_type,
            target: Some(ref target),
            selection,
        } => {
            if count == 0 {
                return finish_pending_cost_or_cast(state, player, pending, events);
            }
            let required_count = match selection {
                CounterCostSelection::SingleObject => count,
                CounterCostSelection::AmongObjects => 1,
            };
            let eligible = super::casting::find_eligible_remove_counter_for_cost_targets(
                state,
                player,
                pending.object_id,
                target,
                counter_type,
                required_count,
            );
            if eligible.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No eligible permanents with counters".into(),
                ));
            }
            if selection == CounterCostSelection::AmongObjects {
                let removable_count = eligible
                    .iter()
                    .filter_map(|object_id| state.objects.get(object_id))
                    .map(|obj| {
                        super::casting::removable_counter_count_for_cost_selection(
                            obj,
                            counter_type,
                            selection,
                        )
                    })
                    .fold(0, u32::saturating_add);
                if removable_count < count {
                    return Err(EngineError::ActionNotAllowed(
                        "Not enough eligible counters to remove".into(),
                    ));
                }
            }
            let max_count = match selection {
                CounterCostSelection::SingleObject => 1,
                CounterCostSelection::AmongObjects => eligible.len(),
            };
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::RemoveCounter {
                    counter_type: counter_type.clone(),
                    count,
                    selection,
                },
                choices: eligible,
                count: max_count,
                min_count: match selection {
                    CounterCostSelection::SingleObject => 0,
                    CounterCostSelection::AmongObjects => 1,
                },
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        AbilityCost::PayEnergy { amount } => {
            // CR 107.14: A player can pay {E} only if they have enough energy.
            // CR 107.3c: Resolve the `QuantityExpr` so dynamic amounts read game
            // state at cast time.
            let amount = u32::try_from(
                super::quantity::resolve_quantity(state, &amount, player, pending.object_id).max(0),
            )
            .unwrap_or(0);
            let player_state = &mut state.players[player.0 as usize];
            if player_state.energy < amount {
                return Err(EngineError::ActionNotAllowed("Not enough energy".into()));
            }
            player_state.energy -= amount;
            events.push(GameEvent::EnergyChanged {
                player,
                delta: -(amount as i32),
            });
        }
        AbilityCost::Waterbend { cost: wb_cost } => {
            // Waterbend: combine waterbend mana with spell mana, enter ManaPayment with Waterbend mode.
            let combined = restrictions::add_mana_cost(&pending.cost, &wb_cost);
            state.pending_cast = Some(Box::new(PendingCast {
                cost: combined,
                ..pending
            }));
            return enter_payment_step(state, player, Some(ConvokeMode::Waterbend), events);
        }
        AbilityCost::Composite { costs } => {
            let mut costs = costs.into_iter();
            let Some(first) = costs.next() else {
                return finish_pending_cost_or_cast(state, player, pending, events);
            };
            let remaining: Vec<_> = costs.collect();
            let mut pending = pending;
            if !remaining.is_empty() {
                pending.additional_cost_flow =
                    Some(AdditionalCost::Required(AbilityCost::Composite {
                        costs: remaining,
                    }));
            }
            return pay_additional_cost(state, player, first, pending, events);
        }
        // CR 118.9 + CR 601.2h + CR 701.13: Exile a permanent you control on the
        // battlefield as an additional/alternative cost (Food Chain class; Lunar
        // Hatchling's "Exile a land you control"). The parser emits zone: None +
        // a permanent-implying filter; `exile_cost_effective_zone` resolves it to
        // the battlefield. The permanent is EXILED, not sacrificed (CR 701.13).
        // `eligible_exile_cost_objects` is single-zone (only controller-owned
        // battlefield objects matching the filter) — graveyard cards are NEVER
        // offered (unlike the dual-zone craft union `ExileMaterials`). Ordered
        // before the hand/graveyard exile arm; the two are disjoint by effective
        // zone, but the battlefield case is checked first.
        AbilityCost::Exile {
            count,
            zone,
            ref filter,
        } if super::cost_payability::exile_cost_effective_zone(zone, filter.as_ref())
            == Zone::Battlefield =>
        {
            let effective_filter =
                super::cost_payability::exile_cost_effective_filter(filter.as_ref());
            let eligible = super::cost_payability::eligible_exile_cost_objects(
                state,
                player,
                pending.object_id,
                Zone::Battlefield,
                effective_filter.as_ref(),
                count,
            );
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to exile".into(),
                ));
            }
            // CR 601.2h: "Exile a land you control" is a mandatory fixed-count
            // cost (min == count), unlike the optional graveyard exile below.
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExilePermanent {
                    filter: filter.clone(),
                },
                choices: eligible,
                count: count as usize,
                min_count: count as usize,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        AbilityCost::Exile {
            count,
            zone: Some(zone),
            ref filter,
        } if matches!(zone, Zone::Hand | Zone::Graveyard) => {
            // CR 118.9a + CR 601.2b + CR 601.2h: Exile N cards from `zone` as
            // part of an alternative or additional casting cost. Covers escape
            // (CR 702.138a, graveyard) and pitch spells (Force of Will, Force
            // of Negation, Misdirection, Unmask, etc., hand). Eligibility is
            // filtered by the cost's `TargetFilter`; the cast source itself is
            // always excluded. The narrow `ExileCostSourceZone` makes invalid
            // zones unrepresentable downstream — `try_from_zone` is the single
            // construction site.
            let narrow_zone = ExileCostSourceZone::try_from_zone(zone)
                .expect("match guard restricts zone to Hand or Graveyard");
            let eligible = super::casting::find_eligible_exile_for_cost_targets(
                state,
                player,
                pending.object_id,
                narrow_zone,
                filter.as_ref(),
            );
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(format!(
                    "Not enough eligible cards in {zone:?} to exile"
                )));
            }
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromZone { zone: narrow_zone },
                choices: eligible,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        AbilityCost::CollectEvidence { amount } => {
            return super::effects::collect_evidence::begin_cost_payment(
                state, player, amount, pending,
            );
        }
        AbilityCost::TapCreatures { count, ref filter } => {
            // CR 702.34a: Tap untapped creatures matching filter as a cost.
            // The source is eligible unless a {T} cost is also present in the
            // activation cost (in which case the source was already tapped, so
            // !obj.tapped naturally excludes it).
            let eligible: Vec<ObjectId> = state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    state.objects.get(id).is_some_and(|obj| {
                        obj.controller == player
                            && !obj.tapped
                            && super::filter::matches_target_filter(
                                state,
                                obj.id,
                                filter,
                                &super::filter::FilterContext::from_source(
                                    state,
                                    pending.object_id,
                                ),
                            )
                    })
                })
                .collect();
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible creatures to tap".into(),
                ));
            }
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::TapCreatures,
                choices: eligible,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            });
        }
        _ => {
            // Other cost types (Exile, etc.) — not yet interactive
        }
    }

    finish_pending_cost_or_cast(state, player, pending, events)
}

fn prepend_deferred_required_cost(cost: AbilityCost, pending: &mut PendingCast) -> AbilityCost {
    match pending.additional_cost_flow.take() {
        Some(AdditionalCost::Required(AbilityCost::Composite { costs })) => {
            let mut combined = Vec::with_capacity(costs.len() + 1);
            combined.push(cost);
            combined.extend(costs);
            AbilityCost::Composite { costs: combined }
        }
        Some(AdditionalCost::Required(next)) => AbilityCost::Composite {
            costs: vec![cost, next],
        },
        Some(other) => {
            pending.additional_cost_flow = Some(other);
            cost
        }
        None => cost,
    }
}

fn is_offering_sacrifice_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    let Some(quality) = effective_offering_quality(state, player, object_id) else {
        return false;
    };
    matches!(
        cost,
        AbilityCost::Sacrifice(cost)
            if cost.requirement == SacrificeRequirement::count(1)
                && cost.target == offering_quality_filter(&quality)
    )
}

fn emerge_sacrifice_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature())
}

fn is_emerge_sacrifice_cost(cost: &AbilityCost) -> bool {
    matches!(
        cost,
        AbilityCost::Sacrifice(cost)
            if cost.requirement == SacrificeRequirement::count(1)
                && cost.target == emerge_sacrifice_filter()
    )
}

/// CR 702.119a-c: Build the required sacrifice component of Emerge's
/// alternative cost. The sacrificed creature's mana value is applied as a cost
/// reduction by `handle_sacrifice_for_cost` while the creature is still on the
/// battlefield.
pub(super) fn emerge_sacrifice_cost() -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(emerge_sacrifice_filter(), 1))
}

/// CR 702.119a-c: Emerge can be paid only if a legal creature can be
/// sacrificed and the resulting reduced emerge mana cost can be paid.
pub(super) fn can_pay_emerge_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    emerge_cost: &ManaCost,
) -> bool {
    super::casting::find_eligible_sacrifice_targets(
        state,
        player,
        object_id,
        &emerge_sacrifice_filter(),
    )
    .into_iter()
    .any(|creature| {
        let mut reduced = emerge_cost.clone();
        apply_emerge_cost_reduction(state, creature, &mut reduced);
        // CR 601.2f + CR 702.119a: Affordability probes must include the
        // final Trinisphere-class floor after Emerge's sacrifice reduction.
        if !cost_has_x(&reduced) {
            super::casting::apply_cost_floor(state, player, object_id, &mut reduced);
        }
        super::casting::can_pay_cost_after_auto_tap(state, player, object_id, &reduced)
    })
}

fn additional_cost_x_max(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
) -> Option<u32> {
    match cost {
        AbilityCost::PayLife { amount } if amount.contains_x() => {
            Some(max_pay_life_x(state, player))
        }
        AbilityCost::Sacrifice(cost)
            if cost.requirement == SacrificeRequirement::Count { count: u32::MAX } =>
        {
            // CR 601.2b: X in an additional sacrifice cost is announced before later target choices.
            Some(
                super::casting::find_eligible_sacrifice_targets(
                    state,
                    player,
                    source_id,
                    &cost.target,
                )
                .len()
                .try_into()
                .unwrap_or(u32::MAX),
            )
        }
        AbilityCost::Exile {
            count: EXILE_COST_X,
            zone: Some(Zone::Graveyard),
            filter,
            ..
        } => {
            // CR 601.2b: X in an additional graveyard-exile cost is announced
            // before the exile payment (Harvest Pyre).
            Some(
                super::casting::find_eligible_exile_for_cost_targets(
                    state,
                    player,
                    source_id,
                    ExileCostSourceZone::Graveyard,
                    filter.as_ref(),
                )
                .len()
                .try_into()
                .unwrap_or(u32::MAX),
            )
        }
        AbilityCost::RemoveCounter {
            target,
            count,
            counter_type,
            selection,
        } if is_chosen_remove_counter_cost_count(*count) => {
            // CR 601.2b: X in a variable counter removal cost is announced before later target choices.
            let target_filter = target.as_ref().unwrap_or(&TargetFilter::SelfRef);
            let eligible = super::casting::find_eligible_remove_counter_for_cost_targets(
                state,
                player,
                source_id,
                target_filter,
                counter_type,
                *count,
            );
            let removable_counts = eligible
                .into_iter()
                .filter_map(|object_id| state.objects.get(&object_id))
                .map(|obj| {
                    super::casting::removable_counter_count_for_cost_selection(
                        obj,
                        counter_type,
                        *selection,
                    )
                });
            Some(
                if target.is_some() && *selection == CounterCostSelection::SingleObject {
                    removable_counts.max().unwrap_or(0)
                } else {
                    removable_counts.fold(0, u32::saturating_add)
                },
            )
        }
        AbilityCost::Composite { costs } => costs
            .iter()
            .filter_map(|cost| additional_cost_x_max(state, player, source_id, cost))
            .min(),
        AbilityCost::PerCounter { base, .. } => {
            additional_cost_x_max(state, player, source_id, base)
        }
        _ => None,
    }
}

fn activation_counter_cost_x_max(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability: &ResolvedAbility,
    cost: &AbilityCost,
) -> Option<u32> {
    if !activation_cost_needs_x_choice(ability, cost) {
        return None;
    }
    additional_cost_x_max(state, player, source_id, cost)
}

pub(super) fn activation_cost_needs_x_choice(
    ability: &ResolvedAbility,
    cost: &AbilityCost,
) -> bool {
    ability.chosen_x.is_none() && cost_has_symbolic_counter_removal(cost)
}

fn cost_has_symbolic_counter_removal(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::RemoveCounter { count, .. } => is_chosen_remove_counter_cost_count(*count),
        AbilityCost::Composite { costs } => costs.iter().any(cost_has_symbolic_counter_removal),
        _ => false,
    }
}

fn cost_has_targeted_symbolic_counter_removal(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::RemoveCounter { count, target, .. } => {
            is_chosen_remove_counter_cost_count(*count) && target.is_some()
        }
        AbilityCost::Composite { costs } => {
            costs.iter().any(cost_has_targeted_symbolic_counter_removal)
        }
        _ => false,
    }
}

fn targeted_remove_counter_choice_cost(cost: &AbilityCost) -> Option<AbilityCost> {
    match cost {
        AbilityCost::RemoveCounter { target, .. } if target.is_some() => Some(cost.clone()),
        AbilityCost::Composite { costs } => {
            costs.iter().find_map(targeted_remove_counter_choice_cost)
        }
        _ => None,
    }
}

fn max_pay_life_x(state: &GameState, player: PlayerId) -> u32 {
    if !super::life_costs::can_pay_life_cast_or_activation_cost(state, player, 1) {
        return 0;
    }
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| u32::try_from(p.life.max(0)).unwrap_or(0))
        .unwrap_or(0)
}

pub(super) fn effective_casualty_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<AdditionalCost> {
    effective_casualty_additional_cost_instances(state, player, object_id)
        .into_iter()
        .next()
        .map(|instance| instance.cost)
}

pub(super) fn effective_casualty_additional_cost_instances(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Vec<AdditionalCostInstance> {
    super::casting::effective_spell_keyword_instances(state, player, object_id)
        .into_iter()
        .filter_map(|keyword| match keyword {
            Keyword::Casualty(threshold) => Some(threshold),
            _ => None,
        })
        .enumerate()
        .map(|(ordinal, threshold)| {
            AdditionalCostInstance::new_with_ordinal(
                AdditionalCostOrigin::Casualty,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
                AdditionalCost::Optional {
                    cost: AbilityCost::Sacrifice(SacrificeCost::count(
                        TargetFilter::Typed(TypedFilter::creature().properties(vec![
                            crate::types::ability::FilterProp::PtComparison {
                                stat: crate::types::ability::PtStat::Power,
                                scope: crate::types::ability::PtValueScope::Current,
                                comparator: crate::types::ability::Comparator::GE,
                                value: QuantityExpr::Fixed {
                                    value: threshold as i32,
                                },
                            },
                        ])),
                        1,
                    )),
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                },
            )
        })
        .collect()
}

/// CR 702.78a: Optional "tap two color-sharing creatures" additional cost from a
/// spell's effective Conspire keyword, including statics-granted Conspire (Wort,
/// the Raidmother / Rassilon, the War President). Mirrors
/// `effective_casualty_additional_cost`.
pub(super) fn effective_conspire_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<AdditionalCost> {
    super::casting::effective_spell_keywords(state, player, object_id)
        .into_iter()
        .any(|keyword| matches!(keyword, Keyword::Conspire))
        .then(|| AdditionalCost::Optional {
            cost: AbilityCost::TapCreatures {
                count: 2,
                filter: crate::database::synthesis::conspire_tap_filter(),
            },
            repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
        })
}

/// CR 702.56a: Return the repeatable optional additional cost from a spell's
/// effective Replicate keyword, including keywords granted by statics.
pub(super) fn effective_replicate_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<AdditionalCost> {
    effective_replicate_additional_cost_instances(state, player, object_id)
        .into_iter()
        .next()
        .map(|instance| instance.cost)
}

pub(super) fn effective_offspring_additional_cost_instances(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Vec<AdditionalCostInstance> {
    super::casting::effective_spell_keyword_instances(state, player, object_id)
        .into_iter()
        .filter_map(|keyword| match keyword {
            Keyword::Offspring(cost) => Some(cost),
            _ => None,
        })
        .enumerate()
        .map(|(ordinal, cost)| {
            AdditionalCostInstance::new_with_ordinal(
                AdditionalCostOrigin::Offspring,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
                AdditionalCost::Optional {
                    cost: AbilityCost::Mana { cost },
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                },
            )
        })
        .collect()
}

pub(super) fn effective_squad_additional_cost_instances(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Vec<AdditionalCostInstance> {
    super::casting::effective_spell_keyword_instances(state, player, object_id)
        .into_iter()
        .filter_map(|keyword| match keyword {
            Keyword::Squad(cost) => Some(cost),
            _ => None,
        })
        .enumerate()
        .map(|(ordinal, cost)| {
            AdditionalCostInstance::new_with_ordinal(
                AdditionalCostOrigin::Squad,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
                AdditionalCost::Optional {
                    cost: AbilityCost::Mana { cost },
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                },
            )
        })
        .collect()
}

pub(super) fn effective_replicate_additional_cost_instances(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Vec<AdditionalCostInstance> {
    super::casting::effective_spell_keyword_instances(state, player, object_id)
        .into_iter()
        .filter_map(|keyword| match keyword {
            Keyword::Replicate(cost) => Some(cost),
            _ => None,
        })
        .enumerate()
        .map(|(ordinal, cost)| {
            AdditionalCostInstance::new_with_ordinal(
                AdditionalCostOrigin::Replicate,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
                AdditionalCost::Optional {
                    cost: AbilityCost::Mana { cost },
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
                },
            )
        })
        .collect()
}

/// CR 702.48a: Return the quality (creature subtype) string from a spell's
/// Offering keyword, if it has one. Uses `effective_spell_keywords` so
/// layer-granted copies are included.
pub(super) fn effective_offering_quality(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<String> {
    super::casting::effective_spell_keywords(state, player, object_id)
        .into_iter()
        .find_map(|keyword| match keyword {
            Keyword::Offering(quality) => Some(quality),
            _ => None,
        })
}

/// CR 702.48a: Build a `TargetFilter` that matches any permanent on the
/// battlefield whose type line includes `quality` (e.g. "Spirit", "Artifact").
/// Creature subtypes use `Subtype`; card types like Artifact use `TypeFilter`.
fn offering_quality_filter(quality: &str) -> TargetFilter {
    let card_type = match quality {
        "Artifact" => Some(TypeFilter::Artifact),
        "Creature" => Some(TypeFilter::Creature),
        "Enchantment" => Some(TypeFilter::Enchantment),
        "Land" => Some(TypeFilter::Land),
        "Instant" => Some(TypeFilter::Instant),
        "Sorcery" => Some(TypeFilter::Sorcery),
        "Planeswalker" => Some(TypeFilter::Planeswalker),
        "Battle" => Some(TypeFilter::Battle),
        _ => None,
    };
    if let Some(tf) = card_type {
        TargetFilter::Typed(TypedFilter::new(tf))
    } else {
        TargetFilter::Typed(TypedFilter::permanent().subtype(quality.to_string()))
    }
}

pub(super) fn offering_sacrifice_cost(quality: &str) -> AbilityCost {
    AbilityCost::Sacrifice(SacrificeCost::count(offering_quality_filter(quality), 1))
}

/// CR 702.48a: Returns `true` when the controller has at least one permanent
/// on the battlefield that could be sacrificed for the Offering cost.
pub(super) fn can_pay_offering_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> bool {
    let Some(quality) = effective_offering_quality(state, player, object_id) else {
        return false;
    };
    !super::casting::find_eligible_sacrifice_targets(
        state,
        player,
        object_id,
        &offering_quality_filter(&quality),
    )
    .is_empty()
}

/// CR 702.48a: Build the `AdditionalCost::Optional` representing the Offering
/// sacrifice choice. The `repeatable` flag is `false` — Offering is paid at
/// most once per cast.
pub(super) fn effective_offering_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Option<AdditionalCost> {
    let quality = effective_offering_quality(state, player, object_id)?;
    Some(AdditionalCost::Optional {
        cost: offering_sacrifice_cost(&quality),
        repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
    })
}

/// CR 702.48c: Reduce `spell_cost` by the sacrificed permanent's mana cost.
///
/// Rules:
/// - Generic mana in the sacrificed cost reduces generic mana in the spell cost.
/// - Each colored/colorless shard in the sacrificed cost first tries to cancel
///   a matching shard in the spell cost; excess reduces generic instead.
///
/// If the permanent no longer exists the function is a no-op.
pub(super) fn apply_offering_cost_reduction(
    state: &GameState,
    sacrifice_id: ObjectId,
    spell_cost: &mut ManaCost,
) {
    let Some(sacrificed_obj) = state.objects.get(&sacrifice_id) else {
        return;
    };
    let sacrificed_mana_cost = sacrificed_obj.mana_cost.clone();

    let ManaCost::Cost {
        shards: ref sac_shards,
        generic: sac_generic,
    } = sacrificed_mana_cost
    else {
        return;
    };

    let ManaCost::Cost {
        shards: ref mut spell_shards,
        generic: ref mut spell_generic,
    } = spell_cost
    else {
        return;
    };

    // CR 702.48c: Each colored/colorless shard reduces a matching spell shard;
    // unmatched excess reduces generic instead.
    for &sac_shard in sac_shards {
        let pos = spell_shards
            .iter()
            .position(|&s| super::casting::cost_shard_matches_reduction(s, sac_shard));
        if let Some(idx) = pos {
            spell_shards.remove(idx);
        } else {
            // Excess colored/colorless reduces generic (floor 0).
            *spell_generic = spell_generic.saturating_sub(1);
        }
    }

    // CR 702.48c: Generic in sacrificed cost reduces generic in spell cost.
    *spell_generic = spell_generic.saturating_sub(sac_generic);
}

/// CR 702.119a: Reduce the Emerge cost by generic mana equal to the sacrificed
/// creature's mana value. Colored pips in the Emerge cost are never reduced.
pub(super) fn apply_emerge_cost_reduction(
    state: &GameState,
    sacrifice_id: ObjectId,
    spell_cost: &mut ManaCost,
) {
    let Some(sacrificed_obj) = state.objects.get(&sacrifice_id) else {
        return;
    };
    let reduction = sacrificed_obj.mana_cost.mana_value();

    let ManaCost::Cost { generic, .. } = spell_cost else {
        return;
    };

    *generic = generic.saturating_sub(reduction);
}

fn apply_sacrificed_this_way_cost_reduction(
    state: &GameState,
    spell_id: ObjectId,
    sacrificed: &[ObjectId],
    spell_cost: &mut ManaCost,
) {
    let Some(spell_obj) = state.objects.get(&spell_id) else {
        return;
    };
    let ManaCost::Cost {
        generic: ref mut spell_generic,
        ..
    } = spell_cost
    else {
        return;
    };

    for def in spell_obj.static_definitions.iter_all() {
        let StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount,
            dynamic_count: Some(dynamic_count),
            ..
        } = &def.mode
        else {
            continue;
        };
        if !matches!(def.affected, Some(TargetFilter::SelfRef)) {
            continue;
        }
        let Some(condition) = def.condition.as_ref() else {
            continue;
        };
        if !sacrificed_this_way_condition_matches(state, condition, spell_obj.controller, spell_id)
        {
            continue;
        }
        let ManaCost::Cost { generic: per, .. } = amount else {
            continue;
        };
        let Some(sacrifice_count) =
            sacrificed_this_way_count(state, spell_id, sacrificed, dynamic_count)
        else {
            continue;
        };
        *spell_generic = spell_generic.saturating_sub(per.saturating_mul(sacrifice_count));
    }
}

fn sacrificed_this_way_count(
    state: &GameState,
    spell_id: ObjectId,
    sacrificed: &[ObjectId],
    dynamic_count: &QuantityRef,
) -> Option<u32> {
    match dynamic_count {
        QuantityRef::TrackedSetSize => Some(sacrificed.len().try_into().unwrap_or(u32::MAX)),
        // The `sacrificed` slice already encodes the "sacrificed this way"
        // provenance (these are the objects sacrificed as a cost), so `caused_by`
        // is satisfied by construction and need not gate the count here.
        QuantityRef::FilteredTrackedSetSize { filter, .. } => {
            let ctx = super::filter::FilterContext::from_source(state, spell_id);
            Some(
                sacrificed
                    .iter()
                    .filter(|&&id| super::filter::matches_target_filter(state, id, filter, &ctx))
                    .count()
                    .try_into()
                    .unwrap_or(u32::MAX),
            )
        }
        _ => None,
    }
}

fn sacrificed_this_way_condition_matches(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    spell_id: ObjectId,
) -> bool {
    condition_requires_additional_cost_paid(condition)
        && condition_matches_with_additional_cost_paid(state, condition, controller, spell_id)
}

fn condition_requires_additional_cost_paid(condition: &StaticCondition) -> bool {
    match condition {
        StaticCondition::AdditionalCostPaid => true,
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => conditions
            .iter()
            .any(condition_requires_additional_cost_paid),
        _ => false,
    }
}

fn condition_matches_with_additional_cost_paid(
    state: &GameState,
    condition: &StaticCondition,
    controller: PlayerId,
    spell_id: ObjectId,
) -> bool {
    match condition {
        StaticCondition::AdditionalCostPaid => true,
        StaticCondition::And { conditions } => conditions.iter().all(|condition| {
            condition_matches_with_additional_cost_paid(state, condition, controller, spell_id)
        }),
        StaticCondition::Or { conditions } => conditions.iter().any(|condition| {
            condition_matches_with_additional_cost_paid(state, condition, controller, spell_id)
        }),
        _ => super::layers::evaluate_condition(state, condition, controller, spell_id),
    }
}

pub(super) fn retrace_discard_land_cost() -> AbilityCost {
    AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: Some(TargetFilter::Typed(TypedFilter::land())),
        selection: crate::types::ability::CardSelectionMode::Chosen,
        self_scope: crate::types::ability::DiscardSelfScope::FromHand,
    }
}

pub(super) fn can_pay_retrace_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> bool {
    let land_filter = TargetFilter::Typed(TypedFilter::land());
    !super::casting::find_eligible_discard_targets(state, player, object_id, Some(&land_filter))
        .is_empty()
}

/// CR 702.133a: Jump-start's additional cost is "discard a card" — any card,
/// unlike Retrace's land restriction.
pub(super) fn jumpstart_discard_card_cost() -> AbilityCost {
    AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: crate::types::ability::CardSelectionMode::Chosen,
        self_scope: crate::types::ability::DiscardSelfScope::FromHand,
    }
}

pub(super) fn can_pay_jumpstart_additional_cost(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> bool {
    // CR 702.133a: any card in hand can be discarded for the jump-start cost.
    !super::casting::find_eligible_discard_targets(state, player, object_id, None).is_empty()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_and_push(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    base_cost: Option<ManaCost>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.180a/b: Harmonize — offer optional creature tap to reduce generic mana cost.
    // CR 601.2b: Creature chosen and tapped as part of cost payment step.
    // CR 302.6: Summoning sickness does not restrict tapping for costs.
    if casting_variant == CastingVariant::Harmonize {
        let has_generic =
            matches!(cost, crate::types::mana::ManaCost::Cost { generic, .. } if *generic > 0);
        if has_generic {
            let eligible: Vec<ObjectId> = state
                .objects
                .values()
                .filter(|o| {
                    o.controller == player
                        && o.zone == Zone::Battlefield
                        && !o.tapped
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                        && o.power.is_some_and(|p| p > 0)
                })
                .map(|o| o.id)
                .collect();
            if !eligible.is_empty() {
                let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
                pending.base_cost = base_cost.clone();
                pending.casting_variant = casting_variant;
                pending.cast_timing_permission = cast_timing_permission;
                pending.origin_zone = origin_zone;
                pending.payment_mode = payment_mode;
                return Ok(WaitingFor::HarmonizeTapChoice {
                    player,
                    eligible_creatures: eligible,
                    pending_cast: Box::new(pending),
                });
            }
        }
    }

    pay_and_push_adventure(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        base_cost,
        casting_variant,
        cast_timing_permission,
        distribute,
        origin_zone,
        payment_mode,
        events,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn pay_and_push_adventure(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    base_cost: Option<ManaCost>,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    distribute: Option<DistributionUnit>,
    origin_zone: Zone,
    payment_mode: CastPaymentMode,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.51a: Convoke lets players tap creatures to reduce mana cost.
    // CR 702.126a: Improvise lets players tap artifacts to pay generic mana.
    // Check for Convoke, Waterbend, or Improvise keyword on the spell.
    let convoke_mode = super::casting::spell_tap_payment_mode(state, player, object_id);
    // Gate on eligible creatures/artifacts being present.
    let convoke_mode = convoke_mode.filter(|mode| {
        state.objects.values().any(|o| match mode {
            ConvokeMode::Convoke => o.is_convoke_eligible(player),
            ConvokeMode::Waterbend => o.is_waterbend_eligible(player),
            ConvokeMode::Improvise => o.is_improvise_eligible(player),
            // CR 702.66a: delve needs at least one card in the caster's graveyard.
            ConvokeMode::Delve => o.zone == Zone::Graveyard && o.owner == player,
        })
    });

    // Enter the payment step if cost needs player input (X) or convoke/waterbend is active.
    // `enter_payment_step` diverts to `ChooseXValue` when the cost has an unchosen X,
    // per CR 601.2f (X chosen before mana is paid).
    let has_x = cost_has_x(cost);
    let manual_payment = payment_mode == CastPaymentMode::Manual && cost.mana_value() > 0;
    if has_x || convoke_mode.is_some() || manual_payment {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        state.pending_cast = Some(Box::new(pending));
        return enter_payment_step(state, player, convoke_mode, events);
    }

    // CR 702.132a: Assist — the cost is now fully locked (no X / convoke / manual
    // step pending), so before finalizing, a spell with assist and a generic
    // component lets the caster choose another player to help pay it. Stash the
    // pending cast so the assist answer handlers can resume via `enter_payment_step`.
    if let Some((generic, candidates)) = assist_offer_params(state, player, object_id, cost) {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        pending.assist_state = AssistState::Offered;
        state.pending_cast = Some(Box::new(pending));
        return Ok(WaitingFor::AssistChoosePlayer {
            player,
            candidates,
            max_generic: generic,
            convoke_mode: None,
        });
    }

    // CR 107.4f + CR 601.2f: Pause before any Phyrexian shard would deduct life,
    // whether life is optional or the only legal route. The resume handler calls
    // `finalize_mana_payment_with_phyrexian_choices` which finishes the cast.
    if let Some(waiting) = maybe_pause_for_phyrexian_choice(
        state,
        player,
        object_id,
        cost,
        events,
        None,
        &HashSet::new(),
    ) {
        let mut pending = PendingCast::new(object_id, card_id, ability, cost.clone());
        pending.base_cost = base_cost.clone();
        pending.casting_variant = casting_variant;
        pending.cast_timing_permission = cast_timing_permission;
        pending.distribute = distribute;
        pending.origin_zone = origin_zone;
        pending.payment_mode = payment_mode;
        state.pending_cast = Some(Box::new(pending));
        return Ok(waiting);
    }

    finalize_cast(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        cast_timing_permission,
        origin_zone,
        events,
    )
}

/// CR 601.2i: Finalize a spell cast.
///
/// By the time this runs, `announce_spell_on_stack` has already pushed a
/// placeholder `StackEntry` with `ability: None, actual_mana_spent: 0`. The
/// object's `zone` field, however, is still at `origin_zone` — zone transition
/// is deferred here so continuous effects that granted castability (e.g.
/// "cards in your graveyard have escape") keep applying through cost payment.
/// This function:
///   1. Snapshots the mana pool, pays the declared cost, and records the actual
///      amount deducted (CR 700.14 — matters for cost reductions / convoke).
///   2. Moves the object from `origin_zone` to `Zone::Stack` now that the cast
///      is committed.
///   3. Updates the existing stack entry's `ability` (filling in the resolved
///      on-resolve effect) and `actual_mana_spent`.
///   4. Emits `SpellCast` (CR 603.6a — the trigger point for "whenever a player
///      casts a spell"), records commander cast taxes, and consumes any
///      graveyard-cast permissions / one-shot cost reductions.
///
/// Shared by `pay_and_push_adventure` (normal casting) and the
/// `(ManaPayment, PassPriority)` handler (after interactive mana payment).
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_cast(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    origin_zone: Zone,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    finalize_cast_with_phyrexian_choices(
        state,
        player,
        object_id,
        card_id,
        ability,
        cost,
        casting_variant,
        cast_timing_permission,
        origin_zone,
        None,
        events,
    )
}

/// CR 107.4f + CR 601.2f: Variant of `finalize_cast` that threads explicit per-shard
/// Phyrexian choices through `pay_mana_cost_with_choices`. `None` preserves
/// auto-decide behavior.
#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_cast_with_phyrexian_choices(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    casting_variant: CastingVariant,
    cast_timing_permission: Option<CastTimingPermission>,
    origin_zone: Zone,
    phyrexian_choices: Option<&[crate::types::game_state::ShardChoice]>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 702.150a: Record how many of this spell's Phyrexian mana symbols are
    // being paid with life. A compleated planeswalker entering from this spell
    // exposes this as an intrinsic AddCounter replacement so it can order with
    // Doubling Season-class modifiers (CR 616.1). Harmless for non-compleated
    // spells (the field is only read for `Keyword::Compleated` planeswalkers).
    {
        let phyrexian_life_paid = phyrexian_choices
            .map(|choices| {
                choices
                    .iter()
                    .filter(|c| matches!(**c, crate::types::game_state::ShardChoice::PayLife))
                    .count() as u32
            })
            .unwrap_or(0);
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.phyrexian_life_paid = phyrexian_life_paid;
        }
    }

    // CR 601.3d + CR 702.8a: When the cast was authorized as-though-it-had-flash
    // via a `SpellCastingOption` whose `condition` is target-dependent (e.g.,
    // Timely Ward — "you may cast this spell as though it had flash if it
    // targets a commander"), the condition could not be evaluated at the
    // announcement-time `flash_timing_cost` check because targets weren't yet
    // chosen. Now that the player has committed targets (and any cascade
    // resulting-MV constraint will be evaluated below before payment), we can
    // authoritatively re-validate: at least one `AsThoughHadFlash` option's
    // condition (or a real Flash keyword) must authorize the cast. If none do,
    // the cast is illegal under CR 601.3d — abort by popping the stack entry
    // and surface the error to the caller.
    if cast_timing_permission == Some(CastTimingPermission::AsThoughHadFlash)
        && !super::restrictions::target_dependent_flash_permission_satisfied(
            state, player, object_id, &ability,
        )
    {
        let pending_for_cancel = PendingCast::new(object_id, card_id, ability, cost.clone());
        super::casting::handle_cancel_cast(state, &pending_for_cancel, events);
        return Err(EngineError::ActionNotAllowed(
            "Chosen targets do not satisfy the flash casting condition".to_string(),
        ));
    }

    // CR 702.85a: Evaluate the cascade resulting-MV constraint BEFORE mana is
    // paid. By this point the player has chosen X (CR 601.2b runs at
    // `enter_payment_step`/`ChooseXValue`), so `ability.chosen_x` reflects the
    // final cost-X. Evaluating here means a rejection has nothing to rewind:
    // no mana has left the pool, no `cost_x_paid` has been stamped, and no
    // targets are committed beyond the announcement-time selections (which
    // `handle_cascade_rejection` clears alongside popping the stack entry).
    //
    // For the constraint we synthesize the resulting MV from the printed cost
    // + chosen_x rather than reading `obj.cost_x_paid`, since the latter is
    // not stamped until after payment further below.
    let cascade_resulting_mv = state
        .objects
        .get(&object_id)
        .map(|obj| obj.mana_cost.mana_value() + ability.chosen_x.unwrap_or(0));
    let mut cascade_cast_transformed = false;
    let mut resolution_success_waiting_for: Option<WaitingFor> = None;
    if let Some(resulting_mv) = cascade_resulting_mv {
        let cascade_check = match evaluate_cascade_constraint_with_resulting_mv(
            state,
            object_id,
            player,
            resulting_mv,
            events,
        ) {
            CascadeCheck::NotApplicable => None,
            CascadeCheck::Accepted {
                cast_transformed,
                waiting_for,
            } => {
                resolution_success_waiting_for = waiting_for.map(|wf| *wf);
                Some(cast_transformed)
            }
            CascadeCheck::Rejected {
                exiled_misses,
                reject_action,
            } => {
                return handle_resolution_cast_rejection(
                    state,
                    player,
                    object_id,
                    exiled_misses,
                    reject_action,
                    events,
                );
            }
        };
        if cascade_check.is_none()
            && !super::casting::selected_exile_alt_cost_permission_accepts_resulting_mv(
                state,
                object_id,
                player,
                resulting_mv,
            )
        {
            let pending_for_cancel = PendingCast::new(object_id, card_id, ability, cost.clone());
            super::casting::handle_cancel_cast(state, &pending_for_cancel, events);
            return Err(EngineError::ActionNotAllowed(
                "Spell mana value does not satisfy the cast permission".to_string(),
            ));
        }
        if cascade_check.is_none()
            && !super::casting::exile_alt_cost_permissions_accept_resulting_mv(
                state,
                object_id,
                player,
                resulting_mv,
            )
        {
            let pending_for_cancel = PendingCast::new(object_id, card_id, ability, cost.clone());
            super::casting::handle_cancel_cast(state, &pending_for_cancel, events);
            return Err(EngineError::ActionNotAllowed(
                "Spell mana value does not satisfy the cast permission".to_string(),
            ));
        }
        cascade_cast_transformed = cascade_check == Some(true);
    }

    // CR 700.14: Snapshot pool size before payment to compute actual mana spent.
    let pool_before = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.produced_mana_total())
        .unwrap_or(0);
    let cast_transformed = cascade_cast_transformed
        || super::casting::selected_exile_alt_cost_permission_casts_transformed(
            state, object_id, player,
        );

    super::casting::pay_mana_cost_with_choices(
        state,
        player,
        object_id,
        cost,
        phyrexian_choices,
        events,
    )?;

    // CR 702.190a / CR 702.188a: Sneak and Web-slinging additionally require
    // returning a creature to its owner's hand as part of paying the casting
    // cost. Sneak's returned creature was an attacker, so remove it from combat.
    let returned_creature = match casting_variant {
        CastingVariant::Sneak {
            returned_creature, ..
        }
        | CastingVariant::WebSlinging { returned_creature } => Some(returned_creature),
        _ => None,
    };
    if let Some(returned_creature) = returned_creature {
        super::zones::move_to_zone(state, returned_creature, Zone::Hand, events);
        if let Some(combat) = state.combat.as_mut() {
            combat
                .attackers
                .retain(|a| a.object_id != returned_creature);
            combat.blocker_assignments.remove(&returned_creature);
        }
    }

    // CR 700.14: Compute actual mana deducted from pool (not declared cost).
    let pool_after = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.mana_pool.produced_mana_total())
        .unwrap_or(0);
    let actual_mana_spent = pool_before.saturating_sub(pool_after) as u32;

    // CR 603.4 + CR 903.8: `origin_zone` preserves the pre-announcement zone so
    // that "cast from hand/graveyard/exile" conditions evaluate correctly and
    // commander-tax bookkeeping fires only when casting from the command zone.
    // The actual Hand→Stack zone transition is deferred to later in this
    // function (see the `move_to_zone` call below), after mana payment has
    // completed against the origin zone.
    let was_in_command_zone = origin_zone == Zone::Command
        && state
            .objects
            .get(&object_id)
            .map(|obj| obj.uses_command_zone_rules())
            .unwrap_or(false);
    let source_zone = origin_zone;

    // CR 603.4: Record the zone the spell was cast from so ETB triggers can
    // evaluate conditions like "if you cast it from your hand".
    let mut ability = ability;
    ability.context.cast_from_zone = Some(source_zone);
    ability.context.cast_controller = Some(player);
    ability.context.cast_phase = Some(state.phase);
    stamp_controller_controlled_as_cast(state, &mut ability, player, object_id);

    // Emit targeting events now that the cast is committed.
    emit_targeting_events(
        state,
        &flatten_targets_in_chain(&ability),
        object_id,
        player,
        events,
    );

    // CR 107.3m: Stash the paid X value directly on the permanent so replacement
    // effects ("enters with X counters") and ETB triggered abilities that
    // reference the cost X (via `QuantityRef::CostXPaid`) can resolve after the
    // spell leaves the stack. Set regardless of placeholder vs. real ability —
    // permanent spells with no on-resolve ability still need this for ETB
    // replacements on X-cost cards like Astral Cornucopia, Walking Ballista, etc.
    let cost_x_paid = ability.chosen_x;
    let kickers_paid = ability.context.kickers_paid.clone();
    let additional_cost_paid = ability.context.additional_cost_paid;
    let additional_cost_payment_count = ability.context.additional_cost_payment_count;
    let additional_cost_payments = ability.context.additional_cost_payments.clone();
    let convoked_creatures = state
        .pending_cast
        .as_ref()
        .filter(|pending| pending.object_id == object_id)
        .map(|pending| pending.convoked_creatures.clone())
        .unwrap_or_default();
    let convoked_creature_count = convoked_creatures.len();

    // Determine whether this spell has a meaningful on-resolve ability.
    // Permanent spells with no Spell-kind AbilityDefinition get a placeholder
    // Unimplemented effect through the cost pipeline (from continue_with_no_ability).
    // Only those remain `ability: None` on the stack — they simply enter the
    // battlefield on resolution. All other spells get their ResolvedAbility.
    let is_placeholder = matches!(
        ability.effect,
        crate::types::ability::Effect::Unimplemented { .. }
    ) && ability.targets.is_empty();
    let stack_ability = if !is_placeholder {
        Some(ability)
    } else {
        // CR 603.4: For permanent spells with no spell ability, store cast_from_zone
        // directly on the object since there's no ability context to carry it.
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cast_from_zone = Some(source_zone);
            obj.cast_controller = Some(player);
        }
        None
    };

    // CR 107.3m: Apply the paid-X snapshot to the object (after the placeholder
    // branch has already taken a mutable borrow). Done unconditionally so that
    // non-placeholder paths (permanents whose on-resolve ability also references
    // CostXPaid, e.g. future cards) share the same source-of-truth lookup.
    if let Some(x) = cost_x_paid {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cost_x_paid = Some(x);
        }
    }
    if !convoked_creatures.is_empty() {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.convoked_creatures = convoked_creatures;
        }
    }
    // CR 603.4 + CR 702.33d: Stamp kicker payments onto the spell-on-stack
    // object so cast-triggers ("When you cast this spell, if it was kicked,
    // ...") can evaluate their intervening-'if' AdditionalCostPaid condition.
    // Cast-triggers resolve BEFORE the spell does (CR 603.3), so the
    // permanent-entry stamp in stack.rs is too late for them. The stamped
    // Vec<KickerVariant> also carries multikicker counts (CR 702.33c). Mirrors
    // the cost_x_paid / convoked_creatures stamps directly above.
    if !kickers_paid.is_empty() {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.kickers_paid.clone_from(&kickers_paid);
        }
    }
    if additional_cost_payment_count > 0 {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.additional_cost_payment_count = additional_cost_payment_count;
            obj.additional_cost_payments
                .clone_from(&additional_cost_payments);
        }
    }
    if let Some(permission) = cast_timing_permission {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cast_timing_permission = Some((permission, state.turn_number));
        }
    }

    let exile_play_permission_source = if source_zone == Zone::Exile {
        state.objects.get(&object_id).and_then(|obj| {
            super::casting::play_from_exile_permission_source(state, obj, player, state.turn_number)
        })
    } else {
        None
    };
    // CR 601.2a + CR 603.7 + CR 611.2a: Capture the tracked-set group of a
    // single-use `PlayFromExile` grant authorizing this cast BEFORE the object
    // leaves exile for the stack.
    // Consumed after the move (see below) so the grant's one allowed cast is
    // spent and every sibling exiled card becomes uncastable (Chandra, Hope's
    // Beacon +1).
    let single_use_exile_play_group = if source_zone == Zone::Exile {
        state
            .objects
            .get(&object_id)
            .and_then(|obj| super::casting::single_use_play_from_exile_group(state, obj, player))
    } else {
        None
    };

    // CR 601.2a + CR 601.2i: The spell was announced onto the stack earlier,
    // but the object's `zone` field stayed at its origin through cost payment
    // so continuous effects that granted castability ("cards in your graveyard
    // have escape", "spells you cast from exile have convoke") continued to
    // apply. Now that the cast is committed, perform the Hand→Stack zone
    // transition so zone-change triggers, counterspell targeting
    // (`FilterProp::InZone { Stack }`), and on-resolution bookkeeping all see
    // the spell as living on the stack.
    //
    // CR 601.2a: "a player first moves that card ... to the stack" — part of the
    // casting process, not a discrete replaceable event. Route through the zone
    // pipeline under the `CastingToStack` exempt cause so this production caller
    // goes through the single entry while the consult is skipped (PLAN §3). The
    // spell moves itself, so the attribution source is the object.
    let stack_req =
        crate::game::zone_pipeline::ZoneMoveRequest::casting_to_stack(object_id, object_id);
    crate::game::zone_pipeline::move_object(state, stack_req, events);

    // CR 614.1a: `CastFromZone` grants with an "exile it instead" rider stamp the
    // synthetic self-scoped graveyard redirect when the granted cast finalizes.
    if state.objects.get(&object_id).is_some_and(|obj| {
        obj.casting_permissions.iter().any(|p| {
            matches!(
                p,
                crate::types::ability::CastingPermission::ExileWithAltCost {
                    exile_instead_of_graveyard_on_resolve: true,
                    ..
                }
            )
        })
    }) {
        apply_exile_instead_of_graveyard_rider(state, object_id);
    }

    if casting_variant == CastingVariant::Foretell {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cast_variant_paid = Some((
                crate::types::ability::CastVariantPaid::Foretell,
                state.turn_number,
            ));
        }
    }
    // CR 702.176a: Tag the stack object so stack resolution can read the impending
    // cost-paid marker and place time counters when the permanent enters.
    if casting_variant == CastingVariant::Impending {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            obj.cast_variant_paid = Some((
                crate::types::ability::CastVariantPaid::Impending,
                state.turn_number,
            ));
        }
    }
    // CR 702.102b + CR 709.4d: A fused split spell on the stack has the combined
    // characteristics of its two halves. The front face supplies the left half;
    // union in the right (Split back face) half's card types (CR 709.4c) and
    // colors (CR 105.2) so counterspell filters, type-matters effects, and
    // protection all see the merged characteristics while the spell resolves.
    if casting_variant == CastingVariant::Fuse {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            let right_half_characteristics = obj
                .back_face
                .as_ref()
                .filter(|bf| bf.layout_kind == Some(crate::types::card::LayoutKind::Split))
                .map(|back| (back.card_types.core_types.clone(), back.color.clone()));
            if let Some((core_types, colors)) = right_half_characteristics {
                for ct in core_types {
                    if !obj.card_types.core_types.contains(&ct) {
                        obj.card_types.core_types.push(ct);
                    }
                }
                for color in colors {
                    if !obj.color.contains(&color) {
                        obj.color.push(color);
                    }
                }
            }
        }
    }

    // CR 601.2i: Update the existing stack entry (pushed at announcement) with
    // the finalized ability and the actual mana spent. The entry must still be
    // present — no one else can have pushed/popped between announce and
    // finalize within a single cast.
    let entry = state
        .stack
        .iter_mut()
        .rfind(|entry| entry.id == object_id)
        .expect("spell stack entry from announcement still present at finalize");
    entry.kind = StackEntryKind::Spell {
        card_id,
        ability: stack_ability,
        casting_variant,
        actual_mana_spent,
    };
    let distinct_colors_spent = state
        .objects
        .get(&object_id)
        .map(|obj| obj.colors_spent_to_cast.distinct_colors() as u32)
        .unwrap_or_default();
    state.stack_paid_facts.insert(
        object_id,
        StackPaidSnapshot {
            actual_mana_spent,
            x_value: cost_x_paid,
            distinct_colors_spent,
            kickers_paid: kickers_paid.len(),
            additional_cost_payment_count,
            additional_cost_payments: additional_cost_payments.clone(),
            additional_cost_paid,
            casting_variant,
            cast_transformed,
            convoked_creatures: convoked_creature_count,
        },
    );

    // Track commander cast count for tax calculation
    if was_in_command_zone {
        super::commander::record_commander_cast(state, object_id);
    }

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::SpellCast {
        card_id,
        controller: player,
        object_id,
    });

    // CR 601.2a + CR 601.2b + CR 110.4: Record permission usage when the spell
    // is finalized onto the stack. This prevents casting a second spell via the
    // same source/slot before the first resolves. Only frequency-bounded
    // variants (`OncePerTurn`, `OncePerTurnPerPermanentType`) need tracking;
    // `Unlimited` permissions (Conduit of Worlds, Omniscience) skip.
    match casting_variant {
        CastingVariant::GraveyardPermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurn,
            ..
        } => {
            state.graveyard_cast_permissions_used.insert(source);
        }
        CastingVariant::GraveyardPermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurnPerPermanentType,
            slot_type: Some(slot),
            ..
        } => {
            // CR 110.4: Consume the chosen permanent-type slot for this source.
            state
                .graveyard_cast_permissions_used_per_type
                .insert((source, slot));
        }
        CastingVariant::GraveyardPermission {
            frequency: crate::types::statics::CastFrequency::OncePerTurnPerPermanentType,
            slot_type: None,
            ..
        } => {
            debug_assert!(
                false,
                "OncePerTurnPerPermanentType reached finalization with slot_type: None — \
                 the slot choice should have been resolved before reaching this point"
            );
        }
        CastingVariant::HandPermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurn,
        } => {
            state.hand_cast_free_permissions_used.insert(source);
        }
        // CR 601.2a + CR 113.6b: Maralen-class exile-cast permission. Stamp
        // the per-source slot when the static is `OncePerTurn`; `Unlimited`
        // (no shipping printing yet) skips tracking so the slot never blocks.
        CastingVariant::ExilePermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurn,
        }
        | CastingVariant::ExilePermission {
            source,
            frequency: crate::types::statics::CastFrequency::OncePerTurnPerPermanentType,
        } => {
            state.exile_cast_permissions_used.insert(source);
        }
        _ => {}
    }
    if let Some((source, crate::types::statics::CastFrequency::OncePerTurn)) =
        exile_play_permission_source
    {
        state.exile_play_permissions_used.insert(source);
    }
    // CR 601.2a + CR 603.7 + CR 611.2a: A single-use exile-cast grant is spent
    // on this cast. Record the group and strip the now-void `PlayFromExile` grant from
    // every other card still in the tracked set so the remaining exiled cards
    // can no longer be cast (Chandra, Hope's Beacon +1: "an instant or sorcery
    // spell" — one total).
    if let Some(group) = single_use_exile_play_group {
        super::casting::consume_single_use_play_from_exile(state, group);
    }

    // CR 611.2f: Snapshot the spell's effective keywords NOW, while the spell is
    // not yet in `spells_cast_this_turn_by_player`, so that
    // `SpellsCastThisTurn == 0`-gated `CastWithKeyword` grants (first-qualifying-
    // spell each turn) evaluate against the pre-record count and correctly attach
    // to this spell. The post-record SpellCast trigger seams (Cascade, Demonstrate)
    // read this snapshot instead of re-querying the now-incremented grant. Uses
    // `effective_spell_keyword_instances` so multi-instance keywords (Cascade x2,
    // Ripple) are preserved, matching what the seams' instance counting expects.
    let cast_spell_keywords =
        super::casting::effective_spell_keyword_instances(state, player, object_id);
    if let Some(obj_mut) = state.objects.get_mut(&object_id) {
        obj_mut.cast_spell_keywords = cast_spell_keywords;
    }

    let obj = state
        .objects
        .get(&object_id)
        .expect("spell object still exists after stack push")
        .clone();
    restrictions::record_spell_cast_from_zone(state, player, &obj, source_zone, casting_variant);

    // CR 601.2f: Consume any one-shot pending cost reductions now that the spell is finalized.
    super::casting::consume_pending_spell_cost_reduction(state, player);

    // CR 601.2f: Stamp and consume one-shot "the next spell …" modifiers.
    super::casting::apply_pending_next_spell_stack_grants(state, player, object_id);
    super::casting::consume_pending_next_spell_modifiers(state, player, object_id);

    // CR 700.14: Track cumulative mana spent on spells this turn for Expend triggers.
    // Uses actual mana deducted from pool (accounts for cost reduction, convoke, etc.).
    if actual_mana_spent > 0 {
        let cumulative = state
            .mana_spent_on_spells_this_turn
            .entry(player)
            .or_insert(0);
        *cumulative += actual_mana_spent;
        let new_cumulative = *cumulative;
        events.push(GameEvent::ManaExpended {
            player_id: player,
            amount_spent: actual_mana_spent,
            new_cumulative,
        });
    }

    Ok(resolution_success_waiting_for.unwrap_or(WaitingFor::Priority { player }))
}

/// CR 608.2g: Outcome of evaluating a cast-during-resolution constraint
/// (Cascade CR 702.85a / Discover CR 701.57a).
#[derive(Debug)]
enum CascadeCheck {
    /// No cast-during-resolution permission on this object — the cast proceeds
    /// normally (or via a plain standing `ManaValue` permission).
    NotApplicable,
    /// The constraint passed (Cascade: resulting MV < source MV; Discover:
    /// resulting MV <= N). The cast proceeds; the misses have already been
    /// bottom-shuffled as a side effect, unless a follow-up resolution choice
    /// remains for the same resolving ability.
    Accepted {
        cast_transformed: bool,
        waiting_for: Option<Box<WaitingFor>>,
    },
    /// The constraint failed. The cast must be aborted; the caller should
    /// unwind the announcement stack entry and route through
    /// `handle_resolution_cast_rejection`, which sends the hit to its
    /// `reject_action` destination.
    Rejected {
        exiled_misses: Vec<ObjectId>,
        reject_action: crate::types::ability::ResolutionMvRejectAction,
    },
}

/// CR 608.2g: Inspect the casting object's `ExileWithAltCost` permissions for a
/// cast-during-resolution permission (Cascade / Discover) and evaluate its
/// resulting-MV constraint. Identified by `resolution_cleanup.is_some()`, which
/// distinguishes it from plain standing `ManaValue`-constrained permissions
/// (Maralen, Beseech) that carry `constraint: Some(ManaValue)` but
/// `resolution_cleanup: None` and stay on the existing fallback path. Consumes
/// the matched permission only; all other permissions are untouched.
///
/// On acceptance, bottom-shuffles the exiled misses here so both accept paths
/// (plain free cast + X-cost cast) share a single cleanup point.
///
/// `resulting_mv` is the resulting spell's mana value — printed
/// `mana_cost.mana_value()` plus the chosen X. Caller synthesizes this because
/// X is known at announcement time but `obj.cost_x_paid` is not stamped until
/// after mana payment.
fn evaluate_cascade_constraint_with_resulting_mv(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    resulting_mv: u32,
    events: &mut Vec<GameEvent>,
) -> CascadeCheck {
    use crate::types::ability::CastingPermission;

    let index = match state.objects.get(&object_id) {
        Some(obj) => {
            let Some(index) = obj.casting_permissions.iter().position(|p| {
                super::casting::exile_alt_cost_permission_supports_cast(state, obj, player, p, None)
            }) else {
                return CascadeCheck::NotApplicable;
            };
            // CR 608.2g: only cast-during-resolution permissions carry
            // `resolution_cleanup`; standing ManaValue permissions do not.
            match obj.casting_permissions.get(index) {
                Some(CastingPermission::ExileWithAltCost {
                    resolution_cleanup: Some(_),
                    ..
                }) => Some(index),
                _ => None,
            }
        }
        None => return CascadeCheck::NotApplicable,
    };
    let index = match index {
        Some(i) => i,
        None => return CascadeCheck::NotApplicable,
    };

    let permission = state
        .objects
        .get_mut(&object_id)
        .expect("object present above")
        .casting_permissions
        .remove(index);
    let (constraint, cast_transformed, cleanup) = match permission {
        CastingPermission::ExileWithAltCost {
            constraint,
            cast_transformed,
            resolution_cleanup: Some(cleanup),
            ..
        } => (constraint, cast_transformed, cleanup),
        _ => unreachable!("position() already filtered to this variant"),
    };

    // CR 702.85a / CR 701.57a: evaluate the resulting-MV gate carried on the
    // permission (`< source_mv` for Cascade, `<= N` for Discover).
    let obj = state.objects.get(&object_id).expect("object present above");
    let accepted = super::casting::cast_permission_constraint_allows_cast(
        state,
        obj,
        &constraint,
        Some(resulting_mv),
    );

    if accepted {
        let waiting_for = handle_resolution_cast_success(
            state,
            player,
            object_id,
            resulting_mv,
            cleanup.exiled_misses,
            cleanup.success_action,
            events,
        );
        CascadeCheck::Accepted {
            cast_transformed,
            waiting_for,
        }
    } else {
        CascadeCheck::Rejected {
            exiled_misses: cleanup.exiled_misses,
            reject_action: cleanup.reject_action,
        }
    }
}

fn handle_resolution_cast_success(
    state: &mut GameState,
    player: PlayerId,
    cast_object: ObjectId,
    resulting_mv: u32,
    exiled_misses: Vec<ObjectId>,
    success_action: crate::types::ability::ResolutionCastSuccessAction,
    events: &mut Vec<GameEvent>,
) -> Option<Box<WaitingFor>> {
    use crate::types::ability::ResolutionCastSuccessAction;

    match success_action {
        // CR 702.85a / CR 701.57a: the hit is being cast, so only the misses
        // bottom-shuffle.
        ResolutionCastSuccessAction::BottomMisses => {
            crate::game::effects::cascade::shuffle_to_bottom(state, &exiled_misses, events);
            None
        }
        ResolutionCastSuccessAction::RippleOfferRemaining { mut remaining_hits } => {
            if remaining_hits.is_empty() {
                // CR 702.60a: after the last accepted hit, put the revealed
                // cards not cast this way on the library bottom.
                crate::game::effects::cascade::shuffle_to_bottom(state, &exiled_misses, events);
                None
            } else {
                let hit_card = remaining_hits.remove(0);
                Some(Box::new(WaitingFor::CastOffer {
                    player,
                    kind: crate::types::game_state::CastOfferKind::Ripple {
                        hit_card,
                        remaining_hits,
                        revealed_misses: exiled_misses,
                    },
                }))
            }
        }
        // CR 608.2g + CR 601.2 + CR 202.3: Invoke Calamity — the spell cast this
        // way has finished announcement and is on the stack. Apply the exile-
        // instead rider (CR 614.1a) to the cast spell, then reduce the running
        // MV budget by this spell's resulting mana value, decrement the cast
        // count, and re-open the window if any casts remain and candidates fit.
        ResolutionCastSuccessAction::FreeCastOfferRemaining {
            controller,
            remaining_casts,
            remaining_mv_budget,
            filter,
            zones,
            exile_instead_of_graveyard,
        } => {
            if exile_instead_of_graveyard {
                apply_exile_instead_of_graveyard_rider(state, cast_object);
            }
            let casts_left = remaining_casts.saturating_sub(1);
            // CR 202.3: shrink the shared budget by what was actually spent on
            // mana value (resulting MV after X, copies, etc.).
            let budget_left = remaining_mv_budget.map(|b| b.saturating_sub(resulting_mv));
            if casts_left == 0 {
                return None;
            }
            let candidates = crate::game::effects::free_cast_from_zones::eligible_candidates(
                state,
                controller,
                &filter,
                &zones,
                budget_left,
            );
            if candidates.is_empty() {
                return None;
            }
            Some(Box::new(WaitingFor::CastOffer {
                player: controller,
                kind: crate::types::game_state::CastOfferKind::FreeCastWindow {
                    candidates,
                    remaining_casts: casts_left,
                    remaining_mv_budget: budget_left,
                    filter,
                    zones,
                    exile_instead_of_graveyard,
                },
            }))
        }
    }
}

/// CR 614.1a + CR 608.2n + CR 614.6: Install the "if this spell would be put
/// into your graveyard, exile it instead" rider on a spell cast during
/// resolution via `Effect::FreeCastFromZones` (Invoke Calamity) as a synthetic
/// per-object `Moved` replacement on the cast spell rather than a bespoke
/// boolean marker. The rider is exactly a self-scoped graveyard→exile redirect
/// — the same class as Rest in Peace / Leyline of the Void, just scoped to this
/// one spell (`valid_card: SelfRef`) — so it routes through the standard
/// replacement pipeline when the spell leaves the stack (the stack-self-move
/// scan exception discovers it). `destination_zone: Graveyard` gates it to the
/// CR 608.2n default destination, so a flashback/aftermath/harmonize spell that
/// already resolves to Exile (a static destination rule, not a replacement)
/// never double-applies: its proposed move is stack→Exile, which the
/// Graveyard-scoped def does not match.
///
/// Applied here (not by mutating the casting variant) because the
/// during-resolution cast has not yet pushed its resolvable `StackEntry::Spell`
/// (that happens at finalize, after this cascade-check point), and the rider
/// must apply regardless of the spell's origin zone or casting variant.
///
/// Known scope gap (behavior-preserving vs the deleted boolean flag): the
/// printed rider is "this turn"-scoped, but the synthetic def carries no
/// duration — `ReplacementDefinition` has no duration field and
/// `revert_layered_characteristics_to_base` only runs for battlefield exits, so
/// the def lingers on the exiled card. Inert in practice (an exiled card's
/// graveyard moves are rare and re-casting mints a new object per CR 400.7),
/// but a `Duration` field on `ReplacementDefinition` is the eventual fix for
/// the rider's "this turn" scope.
pub(crate) fn apply_exile_instead_of_graveyard_rider(state: &mut GameState, cast_object: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&cast_object) {
        obj.replacement_definitions
            .push(exile_instead_of_graveyard_replacement());
    }
}

/// CR 614.1a + CR 608.2n: The synthetic self-scoped graveyard→exile redirect
/// installed by the Invoke Calamity free-cast rider. Mirrors the Rest in Peace
/// redirect shape (`ReplacementEvent::Moved`, `destination_zone: Graveyard`,
/// `execute: ChangeZone { destination: Exile, target: SelfRef }`) but scoped to
/// the cast spell via `valid_card: SelfRef`.
fn exile_instead_of_graveyard_replacement() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .valid_card(TargetFilter::SelfRef)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                destination: Zone::Exile,
                origin: None,
                target: TargetFilter::SelfRef,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        ))
        .description(
            "CR 614.1a: if this spell would be put into its owner's graveyard, exile it instead."
                .to_string(),
        )
}

/// CR 608.2g: Unwind a cast-during-resolution-rejected cast — remove the
/// announcement-time stack entry, dispose of the hit + misses per
/// `reject_action`, and return priority to the caster.
fn handle_resolution_cast_rejection(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    exiled_misses: Vec<ObjectId>,
    reject_action: crate::types::ability::ResolutionMvRejectAction,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    use crate::types::ability::ResolutionMvRejectAction;

    // CR 601.2a: Remove the announcement-time stack entry. The spell never
    // finishes entering the stack because we abort before the Hand→Stack
    // zone move in `finalize_cast_with_phyrexian_choices`.
    if let Some(pos) = state.stack.iter().rposition(|entry| entry.id == object_id) {
        state.stack.remove(pos);
    }

    match reject_action {
        // CR 702.85a: Cascade — misses + the hit (declined at cast time) all
        // bottom-shuffle together in a random order.
        ResolutionMvRejectAction::BottomWithMisses => {
            let mut all_to_bottom = exiled_misses;
            all_to_bottom.push(object_id);
            crate::game::effects::cascade::shuffle_to_bottom(state, &all_to_bottom, events);
        }
        // CR 701.57a: Discover — the misses go to the library bottom in a
        // random order; the hit goes to its owner's hand.
        ResolutionMvRejectAction::ToHand => {
            crate::game::effects::cascade::shuffle_to_bottom(state, &exiled_misses, events);
            super::zones::move_to_zone(state, object_id, Zone::Hand, events);
        }
        // CR 702.62a / CR 702.88a: Suspend / Rebound — no dig misses and no
        // resulting-MV gate, so this path is unreachable in practice. "If you
        // don't [cast it], it remains exiled": the card simply stays in exile
        // (the announcement-time stack entry was already removed above).
        ResolutionMvRejectAction::RemainExiled => {}
    }

    // CR 601.2a: Priority returns to the would-be caster.
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::Priority { player })
}

/// Count distinct source objects that can produce any of the `acceptable` colors.
fn count_available_sources(
    available: &[ManaSourceOption],
    used: &HashSet<ObjectId>,
    acceptable: &[ManaType],
    requires_two_or_more_color_source: bool,
    payment_context: Option<&PaymentContext<'_>>,
) -> usize {
    let mut seen = HashSet::new();
    for opt in available {
        // CR 605.3b: Filter-land combination rows contribute multi-mana
        // atomically. Any color in their combination satisfies the shard.
        if !used.contains(&opt.object_id)
            && option_satisfies(
                opt,
                acceptable,
                requires_two_or_more_color_source,
                payment_context,
            )
        {
            seen.insert(opt.object_id);
        }
    }
    seen.len()
}

/// True iff this source option can contribute any of the acceptable colors.
/// For single-color rows, checks `mana_type` directly; for combination rows,
/// checks whether any color in the combination is acceptable.
fn option_satisfies(
    opt: &ManaSourceOption,
    acceptable: &[ManaType],
    requires_two_or_more_color_source: bool,
    payment_context: Option<&PaymentContext<'_>>,
) -> bool {
    if !option_allowed_for_context(opt, payment_context) {
        return false;
    }
    if requires_two_or_more_color_source && !opt.source_could_produce_two_or_more_colors {
        return false;
    }
    if acceptable.is_empty() {
        return true;
    }
    match &opt.atomic_combination {
        Some(combo) => combo.iter().any(|t| acceptable.contains(t)),
        None => acceptable.contains(&opt.mana_type),
    }
}

fn option_allowed_for_context(
    opt: &ManaSourceOption,
    payment_context: Option<&PaymentContext<'_>>,
) -> bool {
    let Some(ctx) = payment_context else {
        return true;
    };
    opt.restrictions
        .iter()
        .all(|restriction| restriction.allows(ctx))
}

/// Pick the source with the fewest alternative color options (LCV heuristic).
/// Among ties, the tier-sort order of `available` acts as tiebreaker (pure lands
/// before dorks before land-creatures before sacrifice sources).
fn find_least_flexible_source(
    available: &[ManaSourceOption],
    used: &HashSet<ObjectId>,
    acceptable: &[ManaType],
    requires_two_or_more_color_source: bool,
    payment_context: Option<&PaymentContext<'_>>,
) -> Option<ManaSourceOption> {
    available
        .iter()
        .filter(|opt| {
            !used.contains(&opt.object_id)
                && option_satisfies(
                    opt,
                    acceptable,
                    requires_two_or_more_color_source,
                    payment_context,
                )
        })
        .min_by_key(|opt| {
            available
                .iter()
                .filter(|o| o.object_id == opt.object_id)
                .count()
        })
        .cloned()
}

/// Auto-tap mana sources controlled by `player` to produce enough mana for `cost`.
///
/// Considers all permanent types with mana abilities: lands, creatures (mana dorks),
/// artifacts, and sacrifice-for-mana sources (Treasure tokens).
///
/// Strategy: tap sources producing colors required by the cost first (colored shards),
/// then tap remaining sources for generic requirements.
///
/// `deprioritize_source` — if set, this permanent is tapped last (it's the permanent whose
/// activated ability we're paying for, so tapping other sources first is preferable UX).
///
/// Tier priority: pure land > non-land mana dork > land-creature > deprioritized > sacrifice source.
pub(super) fn auto_tap_mana_sources(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
) {
    auto_tap_mana_sources_excluding(
        state,
        player,
        cost,
        events,
        deprioritize_source,
        &HashSet::new(),
    );
}

pub(super) fn auto_tap_mana_sources_excluding(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
    excluded_sources: &HashSet<ObjectId>,
) {
    auto_tap_mana_sources_inner(
        state,
        player,
        cost,
        events,
        deprioritize_source,
        excluded_sources,
        None,
    );
}

pub(super) fn auto_tap_mana_sources_with_context(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
    payment_context: Option<&PaymentContext<'_>>,
) {
    auto_tap_mana_sources_with_context_excluding(
        state,
        player,
        cost,
        events,
        deprioritize_source,
        payment_context,
        &HashSet::new(),
    );
}

pub(super) fn auto_tap_mana_sources_with_context_excluding(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
    payment_context: Option<&PaymentContext<'_>>,
    excluded_sources: &HashSet<ObjectId>,
) {
    auto_tap_mana_sources_inner(
        state,
        player,
        cost,
        events,
        deprioritize_source,
        excluded_sources,
        payment_context,
    );
}

fn auto_tap_mana_sources_inner(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    deprioritize_source: Option<ObjectId>,
    excluded_sources: &HashSet<ObjectId>,
    payment_context: Option<&PaymentContext<'_>>,
) {
    use crate::types::card_type::CoreType;
    use crate::types::mana::ManaCost;

    // CR 601.2g: A player may spend mana from their mana pool to pay costs.
    // Plan against the *residual* cost (what the pool can't already cover) so
    // pre-floated mana isn't shadowed by redundant taps — e.g. Sol Ring + an
    // Island floated before casting a 3-mana spell must not tap three more
    // sources. Restriction-aware eligibility is delegated to
    // `reduce_cost_by_pool`, which mirrors the real payment path.
    let spell_meta =
        deprioritize_source.and_then(|sid| super::casting::build_spell_meta(state, player, sid));
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    let effective_ctx = payment_context.or(spell_ctx.as_ref());
    let any_color = if matches!(
        payment_context,
        Some(PaymentContext::Effect | PaymentContext::Activation { .. })
    ) {
        super::static_abilities::player_can_spend_as_any_color(state, player)
    } else {
        super::casting::player_can_spend_as_any_color_for_optional_spell(
            state,
            player,
            deprioritize_source,
        )
    };
    let residual = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| mana_payment::reduce_cost_by_pool(&p.mana_pool, cost, effective_ctx, any_color))
        .unwrap_or_else(|| cost.clone());

    let (shards, generic) = match &residual {
        ManaCost::NoCost | ManaCost::SelfManaCost => return,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0 => return,
        ManaCost::Cost { shards, generic } => (shards.as_slice(), *generic),
    };

    // Build list of activatable mana options for ALL permanents this player controls.
    // CR 605.1b: Non-land permanents can have mana abilities.
    let mut available: Vec<ManaSourceOption> = state
        .battlefield
        .iter()
        .filter(|oid| !excluded_sources.contains(oid))
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller != player || obj.tapped {
                return None;
            }
            // Use land-specific function for lands (includes basic-subtype
            // fallback), general function for everything else (includes
            // summoning sickness check). Auto-tap plans with potential mana
            // sources, not only sources whose own mana sub-cost is already
            // payable from the current pool; Phase 3 pays those sub-costs from
            // other selected sources before resolving the paid mana ability.
            if obj.card_types.core_types.contains(&CoreType::Land) {
                Some(mana_sources::auto_tap_land_mana_options(state, oid, player))
            } else {
                Some(mana_sources::auto_tap_mana_options(state, oid, player))
            }
        })
        .flatten()
        .collect();

    // CR 605.3b: Auto-tap sort key. Tier layout (the enum factors the two
    // scattered bool flags):
    //   outer (tier_byte): 0 = non-sacrifice mana source; 1 = sacrifice-for-mana
    //     (source will not come back — always last).
    //   middle (card_tier): 0 = free-colorless land row (ideal generic filler);
    //     1 = other land row; 2 = non-land non-creature rock (Signet);
    //     3 = non-land creature dork (preserve as blocker); 4 = land-creature
    //     manland (preserve as blocker); 5 = deprioritized source (spell's own
    //     source).
    //   inner (priority_amount): penalty sub-tier + fixed-amount tiebreak
    //     (e.g. painland-1 < painland-2 < painland-None). Replaces the
    //     collapsed `harms_controller` bool — amounts now rank.
    // The entire penalty axis is consulted only via `ManaSourcePenalty`
    // methods, so a future variant (e.g. `DiscardsOnActivation`) updates
    // the ordering at one place, not seven.
    available.sort_by_key(|option| {
        let obj = state.objects.get(&option.object_id);
        let is_land = obj.is_some_and(|o| o.card_types.core_types.contains(&CoreType::Land));
        let is_creature =
            obj.is_some_and(|o| o.card_types.core_types.contains(&CoreType::Creature));
        let row_is_free_colorless =
            option.atomic_combination.is_none() && option.mana_type == ManaType::Colorless;
        let card_tier: u32 = if deprioritize_source == Some(option.object_id) {
            5
        } else if is_land && is_creature {
            // CR 509.1a: a chosen blocker must be untapped. An animated manland
            // is a creature body — preserve it (and after a 1/1 dork: it is
            // usually the bigger blocker, so it sorts after the dork).
            4
        } else if is_creature {
            // CR 509.1a: preserve a non-land creature mana source (dork) as a
            // blocker.
            3
        } else if is_land && row_is_free_colorless {
            // Heuristic (no CR): a free colorless row is the ideal generic
            // filler — it commits no colored production a later shard in this
            // same payment needs.
            0
        } else if is_land {
            1
        } else {
            // non-land non-creature mana source (rock / Signet)
            2
        };
        (
            option.penalty.tier_byte() as u32,
            card_tier,
            option.penalty.priority_amount(),
        )
    });

    let mut to_tap: Vec<ManaSourceOption> = Vec::new();
    let mut used_sources: HashSet<ObjectId> = HashSet::new();

    // Build the typed shard-requirements list first — used by both the
    // combination pre-pass and the main MCV/LCV loop.
    //
    // CR 107.4f: Phyrexian shards can be paid with 2 life, so they should
    // not block using flexible mana sources (dual lands) for strict colored
    // requirements. We track Phyrexian shards separately and prioritize
    // them after strict colored shards (MCV will sort them as least constrained).
    let mut deferred_generic: usize = 0;
    let mut needs: Vec<(Vec<ManaType>, bool, bool, bool)> = Vec::new();
    for shard in shards {
        use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) => {
                let acceptable = if any_color { Vec::new() } else { vec![color] };
                needs.push((acceptable, false, false, false));
            }
            ShardRequirement::Phyrexian(color) => {
                // CR 107.4f: Mark as phyrexian (4th field = true) so MCV deprioritizes it
                // compared to strict Single color requirements. Phyrexian can be paid with
                // life, so we should consume other mana sources for strict requirements first.
                let acceptable = if any_color { Vec::new() } else { vec![color] };
                needs.push((acceptable, false, false, true));
            }
            ShardRequirement::Hybrid(a, b) => {
                let acceptable = if any_color { Vec::new() } else { vec![a, b] };
                needs.push((acceptable, false, false, false));
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                // CR 107.4f: Hybrid Phyrexian also allows life payment, so deprioritize.
                let acceptable = if any_color { Vec::new() } else { vec![a, b] };
                needs.push((acceptable, false, false, true));
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                let acceptable = if any_color { Vec::new() } else { vec![color] };
                needs.push((acceptable, true, false, false));
            }
            // CR 107.4f: K'rrik promotion never reaches the auto-tap
            // planner (`shard_to_mana_type` never emits this variant),
            // but the arm is required for exhaustiveness. Same
            // tap-planning shape as the unpromoted `TwoGenericHybrid` but
            // with potential life payment, so deprioritize.
            ShardRequirement::TwoGenericHybridPhyrexian(color) => {
                let acceptable = if any_color { Vec::new() } else { vec![color] };
                needs.push((acceptable, true, false, true));
            }
            ShardRequirement::ColorlessHybrid(color) => {
                let acceptable = if any_color {
                    Vec::new()
                } else {
                    vec![ManaType::Colorless, color]
                };
                needs.push((acceptable, false, false, false));
            }
            ShardRequirement::TwoOrMoreColorSource => {
                needs.push((Vec::new(), false, true, false));
            }
            ShardRequirement::Snow | ShardRequirement::X => {
                deferred_generic += 1;
            }
        }
    }

    let mut assigned = vec![false; needs.len()];

    // Phase 0 (combo pre-pass): CR 605.3b + CR 106.1a — filter-land rows
    // produce a full multi-mana combination atomically. A naive per-shard
    // loop can't see that tapping one filter land satisfies two colored
    // requirements. Pre-allocate combination sources against pairs of
    // still-unfilled shards before falling through to the single-color loop.
    assign_combination_sources(
        &available,
        &needs,
        &mut assigned,
        &mut used_sources,
        &mut to_tap,
        effective_ctx,
    );

    // Phase 1: Assign remaining single-color sources to shards using MCV/LCV.
    // The naive greedy approach (tap first matching source per shard) fails when
    // a flexible source (dual land, multi-color dork) gets consumed for a color
    // that a single-purpose source could have provided, leaving no source for
    // a color only the flexible source can produce.
    //
    // MCV: process the most constrained shard first (fewest available sources).
    // Phyrexian shards are deprioritized since they can be paid with life.
    // LCV: for each shard, prefer the least flexible source (fewest color options).
    for _ in 0..needs.len() {
        let mut best_idx = None;
        let mut min_sources = usize::MAX;
        let mut best_priority = 1u8; // 0 = strict color (prioritized), 1 = phyrexian (deprioritized)
        for (i, (acceptable, _, requires_two_or_more_color_source, is_phyrexian)) in
            needs.iter().enumerate()
        {
            if assigned[i] {
                continue;
            }
            let count = count_available_sources(
                &available,
                &used_sources,
                acceptable,
                *requires_two_or_more_color_source,
                effective_ctx,
            );
            let priority = if *is_phyrexian { 1u8 } else { 0u8 };
            // CR 107.4f: Prioritize strict colored shards (priority 0) over Phyrexian
            // shards (priority 1). Within the same priority tier, use MCV (fewest sources).
            if priority < best_priority || (priority == best_priority && count < min_sources) {
                min_sources = count;
                best_priority = priority;
                best_idx = Some(i);
            }
        }
        let Some(idx) = best_idx else { break };
        let (ref acceptable, two_generic_fallback, requires_two_or_more_color_source, _) =
            &needs[idx];
        if let Some(option) = find_least_flexible_source(
            &available,
            &used_sources,
            acceptable,
            *requires_two_or_more_color_source,
            effective_ctx,
        ) {
            used_sources.insert(option.object_id);
            to_tap.push(option);
        } else if *two_generic_fallback {
            deferred_generic += 2;
        }
        assigned[idx] = true;
    }

    // Phase 2: satisfy generic cost + deferred shards. CR 107.4b: generic mana
    // in costs can be paid with any type of mana — including colorless — so a
    // multi-mana source such as Sol Ring (`{T}: Add {C}{C}`) is valid generic
    // filler. Sources are spent in a fixed priority so the plan both stays
    // payable and matches player expectation:
    //   class 0 — color-locked sources (every unit colorless): usable ONLY for
    //             generic, so spend them first and keep flexible colored
    //             sources open. This is why a colorless rock (Sol Ring, Mind
    //             Stone) fills generic before a colored land is tapped.
    //   class 1 — flexible single-mana sources (one colored mana each).
    //   class 2 — flexible (colored) combination sources: last resort. Burning
    //             a 2-mana colored combo on generic wastes half its output when
    //             a cheaper line exists, so a filter land's `{T}: Add {C}`
    //             (class 0) is preferred over its colored combo for pure
    //             generic (see `auto_tap_does_not_use_combo_for_pure_generic`).
    // A combination source credits its full atomic width toward generic — one
    // activation yields every unit at once. Previously ALL combination sources
    // were skipped here, which stranded Sol Ring (a combo with no non-combo
    // sibling ability) and made spells payable only by colorless rocks read as
    // uncastable in the shared affordability preview.
    let mut remaining_generic = generic as usize + deferred_generic;
    let generic_priority = |option: &ManaSourceOption| -> u8 {
        let color_locked = match &option.atomic_combination {
            Some(combo) => combo.iter().all(|m| *m == ManaType::Colorless),
            None => option.mana_type == ManaType::Colorless,
        };
        if color_locked {
            0
        } else if option.atomic_combination.is_none() {
            1
        } else {
            2
        }
    };
    for class in 0u8..=2 {
        if remaining_generic == 0 {
            break;
        }
        for option in &available {
            if remaining_generic == 0 {
                break;
            }
            if generic_priority(option) != class {
                continue;
            }
            if !option_allowed_for_context(option, effective_ctx) {
                continue;
            }
            if used_sources.insert(option.object_id) {
                let width = option
                    .atomic_combination
                    .as_ref()
                    .map_or(1, |combo| combo.len());
                to_tap.push(option.clone());
                remaining_generic = remaining_generic.saturating_sub(width);
            }
        }
    }

    // Phase 3: activate each selected mana source.
    // Sources with an explicit ability delegate to resolve_mana_ability (the single
    // authority for cost payment — handles tap, sacrifice, and future cost types).
    // The basic-land-subtype fallback (ability_index: None) uses inline tap + produce.
    for option in to_tap {
        if let Some(idx) = option.ability_index {
            let ability_def = state
                .objects
                .get(&option.object_id)
                .and_then(|obj| obj.abilities.get(idx))
                .cloned();
            if let Some(ability_def) = ability_def {
                if let Some(sub_cost) = mana_sub_cost_of(&ability_def.cost) {
                    let mut excluded = excluded_sources.clone();
                    excluded.insert(option.object_id);
                    let (source_types, source_subtypes) =
                        super::casting::activation_source_types(state, option.object_id);
                    let activation_ctx = PaymentContext::Activation {
                        source_types: &source_types,
                        source_subtypes: &source_subtypes,
                    };
                    auto_tap_mana_sources_inner(
                        state,
                        player,
                        sub_cost,
                        events,
                        Some(option.object_id),
                        &excluded,
                        Some(&activation_ctx),
                    );
                }
                // color_override tells resolve_mana_ability how to resolve the
                // ability's choice dimension. `SingleColor` replays a per-color
                // pick (AnyOneColor/ChoiceAmongExiledColors); `Combination`
                // carries a pre-chosen multi-mana sequence (filter lands).
                // Errors are non-fatal here: auto-tap runs synchronously during payment,
                // so sources can't change state between collection and resolution. If a
                // source is somehow invalid (e.g., removed by a replacement effect), we
                // skip it silently — the player can still manually tap other sources.
                let override_value = production_override_for_option(&ability_def, &option);
                let _ = mana_abilities::resolve_mana_ability(
                    state,
                    option.object_id,
                    player,
                    &ability_def,
                    events,
                    override_value,
                );
            }
        } else {
            // Basic-land-subtype fallback — no explicit ability, just tap + produce.
            if let Some(obj) = state.objects.get_mut(&option.object_id) {
                if !obj.tapped {
                    obj.tapped = true;
                    events.push(GameEvent::PermanentTapped {
                        object_id: option.object_id,
                        caused_by: None,
                    });
                }
            }
            mana_payment::produce_mana(
                state,
                option.object_id,
                option.mana_type,
                player,
                true,
                events,
            );
            // CR 106.12 + CR 106.12a: a basic land's intrinsic mana ability
            // always includes `{T}` in its cost, so this auto-tap fallback
            // taps the land for mana. Emit one `TappedForMana` per resolution
            // so `TapsForMana` triggers fire exactly once.
            events.push(GameEvent::TappedForMana {
                player_id: player,
                source_id: option.object_id,
                produced: vec![option.mana_type],
                tap_state: ManaTapState::FromTap,
            });
        }
    }
}

fn production_override_for_option(
    ability_def: &crate::types::ability::AbilityDefinition,
    option: &ManaSourceOption,
) -> Option<crate::types::game_state::ProductionOverride> {
    if let Some(combo) = option.atomic_combination.clone() {
        return Some(crate::types::game_state::ProductionOverride::Combination(
            combo,
        ));
    }

    let Effect::Mana { produced, .. } = &*ability_def.effect else {
        return None;
    };
    match produced {
        crate::types::ability::ManaProduction::AnyOneColor { .. }
        | crate::types::ability::ManaProduction::AnyCombination { .. }
        | crate::types::ability::ManaProduction::AnyOneColorAmongPermanents { .. }
        | crate::types::ability::ManaProduction::ChoiceAmongExiledColors { .. }
        | crate::types::ability::ManaProduction::OpponentLandColors { .. }
        | crate::types::ability::ManaProduction::AnyTypeProduceableBy { .. }
        | crate::types::ability::ManaProduction::AnyInCommandersColorIdentity { .. } => Some(
            crate::types::game_state::ProductionOverride::SingleColor(option.mana_type),
        ),
        crate::types::ability::ManaProduction::Fixed { .. }
        | crate::types::ability::ManaProduction::Colorless { .. }
        | crate::types::ability::ManaProduction::Mixed { .. }
        | crate::types::ability::ManaProduction::ChosenColor { .. }
        | crate::types::ability::ManaProduction::ChoiceAmongCombinations { .. }
        | crate::types::ability::ManaProduction::DistinctColorsAmongPermanents { .. }
        | crate::types::ability::ManaProduction::TriggerEventManaType => None,
    }
}

fn mana_sub_cost_of(cost: &Option<AbilityCost>) -> Option<&ManaCost> {
    match cost {
        Some(AbilityCost::Mana { cost }) => Some(cost),
        Some(AbilityCost::Composite { costs }) => costs.iter().find_map(|sub| match sub {
            AbilityCost::Mana { cost } => Some(cost),
            _ => None,
        }),
        _ => None,
    }
}

/// CR 605.3b + CR 106.1a: Greedy pre-pass for `ManaProduction::ChoiceAmongCombinations`
/// (Shadowmoor/Eventide filter lands). Walks every source permanent that has
/// combination rows, picks the combination that covers the most still-unfilled
/// shards, and marks the source used + shards assigned. Runs before the
/// single-color shard assigner so a filter land's 2 mana is allocated
/// atomically instead of one shard at a time.
///
/// Uniqueness guarantee: every combination row for the same `object_id` shares
/// an `atomic_combination`-bearing identity, but only one such row can be
/// selected per object — when a combo is picked the object is inserted into
/// `used_sources`, blocking further rows of every combination variant.
fn assign_combination_sources(
    available: &[ManaSourceOption],
    needs: &[(Vec<ManaType>, bool, bool, bool)],
    assigned: &mut [bool],
    used_sources: &mut HashSet<ObjectId>,
    to_tap: &mut Vec<ManaSourceOption>,
    payment_context: Option<&PaymentContext<'_>>,
) {
    // Build per-object candidate list: for each object that has any
    // `atomic_combination`-bearing rows, collect all of its combination rows.
    let mut combo_objects: Vec<ObjectId> = Vec::new();
    for opt in available {
        if opt.atomic_combination.is_some()
            && !combo_objects.contains(&opt.object_id)
            && !used_sources.contains(&opt.object_id)
            && option_allowed_for_context(opt, payment_context)
        {
            combo_objects.push(opt.object_id);
        }
    }

    for oid in combo_objects {
        if used_sources.contains(&oid) {
            continue;
        }
        // Collect this object's combination rows in tier order.
        let candidates: Vec<&ManaSourceOption> = available
            .iter()
            .filter(|o| {
                o.object_id == oid
                    && o.atomic_combination.is_some()
                    && option_allowed_for_context(o, payment_context)
            })
            .collect();
        if candidates.is_empty() {
            continue;
        }

        // Score each candidate combo by the number of still-unfilled shards
        // it can satisfy. A combo's colors are consumed in sequence against
        // unmet needs: the same color unit can only satisfy one shard.
        let mut best_score = 0usize;
        let mut best_combo: Option<(&ManaSourceOption, Vec<usize>)> = None;
        for cand in &candidates {
            let combo = cand
                .atomic_combination
                .as_ref()
                .expect("combination row invariant");
            let (score, covered) = score_combination(combo, needs, assigned);
            if score > best_score {
                best_score = score;
                best_combo = Some((cand, covered));
            }
        }

        // Only commit the combo if it covers at least one colored shard. A
        // combo that covers no colored shards would waste its second mana on
        // generic — Phase 2 picks single-color sources for generic more
        // efficiently.
        if let Some((chosen, covered_indices)) = best_combo {
            used_sources.insert(chosen.object_id);
            to_tap.push((*chosen).clone());
            for idx in covered_indices {
                assigned[idx] = true;
            }
        }
    }
}

/// Simulate applying a combination's mana to still-unfilled shard needs.
/// Returns `(count_of_shards_covered, indices_of_covered_needs)` — each unit
/// of mana in the combination may cover at most one shard. Preference is
/// first-match in need order, mirroring Phase 1's MCV behaviour at a coarser
/// grain (Phase 1 already re-orders per-shard scarcity, so here a naive
/// first-fit is sufficient for the filter-land class).
fn score_combination(
    combo: &[ManaType],
    needs: &[(Vec<ManaType>, bool, bool, bool)],
    assigned: &[bool],
) -> (usize, Vec<usize>) {
    let mut locally_consumed: Vec<bool> = assigned.to_vec();
    let mut covered = Vec::new();
    for mana in combo {
        for (i, (acceptable, _, requires_two_or_more_color_source, _)) in needs.iter().enumerate() {
            if locally_consumed[i] {
                continue;
            }
            if *requires_two_or_more_color_source {
                continue;
            }
            if acceptable.contains(mana) {
                locally_consumed[i] = true;
                covered.push(i);
                break;
            }
        }
    }
    (covered.len(), covered)
}

/// Compute the maximum legal value of X the caster can choose for a pending cast.
///
/// Upper bound = (mana currently in pool) + (all activatable mana sources
/// under the caster's control) − (fixed portion of cost).
///
/// All activatable mana sources are counted regardless of penalty — Treasure
/// tokens (sacrifice), pain lands (life payment), and ordinary tap sources
/// all contribute. Since this is only an upper bound for UI/AI enumeration,
/// overcounting is safe; `ManaPayment` validates actual affordability later.
///
/// Each untapped producer counts once, regardless of how many color options it
/// offers (a shock land is still one tap → one mana).
///
/// This is an upper bound used for UI display and AI action enumeration only.
/// `ManaPayment` remains the authoritative check for whether the full colored
/// cost is actually payable after the player commits an X value.
///
/// When `object_id` is `Some`, the spell's tap-payment keywords (Convoke,
/// Waterbend, Improvise) are accounted for. CR 110.5 + CR 110.5c: a permanent
/// has exactly one tapped/untapped status and retains it until changed, so each
/// untapped permanent is a single tap unit. CR 118.3: a player can't pay a cost
/// without the resources. A permanent that is both a mana source and
/// tap-keyword-eligible can therefore serve only ONE channel — so each
/// permanent contributes `max(mana yield, tap-keyword yield)`, never the sum.
/// This is required for X-spells with these keywords (CR 601.2b: X is announced
/// before payment, so the cap must already reflect tap capacity per
/// CR 702.126a/702.51a).
///
/// CR 601.2b + CR 601.2f: X is announced as part of determining total cost,
/// before mana is paid.
pub fn max_x_value(
    state: &GameState,
    player: PlayerId,
    cost: &ManaCost,
    object_id: Option<ObjectId>,
) -> u32 {
    max_x_value_excluding(state, player, cost, object_id, &HashSet::new())
}

pub(super) fn max_x_value_excluding(
    state: &GameState,
    player: PlayerId,
    cost: &ManaCost,
    object_id: Option<ObjectId>,
    excluded_sources: &HashSet<ObjectId>,
) -> u32 {
    let ManaCost::Cost { shards, generic } = cost else {
        return 0;
    };
    let x_count = shards
        .iter()
        .filter(|s| matches!(s, ManaCostShard::X))
        .count() as u32;
    if x_count == 0 {
        return 0;
    }

    let fixed_portion: u32 = shards
        .iter()
        .filter(|s| !matches!(s, ManaCostShard::X))
        .map(|s| s.mana_value_contribution())
        .sum::<u32>()
        + *generic;

    let pool = state
        .players
        .iter()
        .find(|p| p.id == player)
        .map_or(0, |p| p.mana_pool.total() as u32);

    let tap_payment_mode =
        object_id.and_then(|oid| super::casting::spell_tap_payment_mode(state, player, oid));

    // CR 702.126a / 702.51a: tap-payment keywords (Improvise/Convoke/Waterbend)
    // let the caster pay generic mana by tapping permanents. The eligibility
    // predicate is spell-level (not per-object), so resolve it once here.
    let pred: Option<fn(&super::game_object::GameObject, PlayerId) -> bool> = match tap_payment_mode
    {
        Some(ConvokeMode::Convoke) => {
            Some(super::game_object::GameObject::is_convoke_eligible as _)
        }
        Some(ConvokeMode::Waterbend) => {
            Some(super::game_object::GameObject::is_waterbend_eligible as _)
        }
        Some(ConvokeMode::Improvise) => {
            Some(super::game_object::GameObject::is_improvise_eligible as _)
        }
        Some(ConvokeMode::Delve) | None => None,
    };

    // CR 110.5 + CR 110.5c + CR 118.3: each untapped permanent is a single tap
    // unit. CR 702.126a / 702.51a: a tap-payment keyword (Improvise/Convoke/
    // Waterbend) taps a permanent "rather than pay that mana" — so a permanent
    // that is both a mana source and tap-keyword-eligible can serve only ONE
    // channel, not both. Partition per object: each contributes
    // max(mana yield, tap-keyword yield), never the sum, or the X cap inflates
    // above what the caster can actually pay.
    //
    // CR 117.1d + CR 601.2g: Use `feasible_mana_capacity` (not the auto-tap-
    // only `max_mana_yield`) so sacrifice-/discard-/life-cost mana abilities
    // the controller could activate manually are counted. Without this, KCI
    // (and similar non-tap mana sources) understate the X cap for X-spells
    // — see #562. The per-permanent sum can over-count chain-sacrifice
    // configurations (tracked in #1235); colored-shard non-tap feasibility
    // is deferred separately (tracked in #1234).
    let permanent_capacity: u32 = state
        .battlefield
        .iter()
        .filter(|id| !excluded_sources.contains(id))
        .map(|&id| {
            let mana = mana_sources::feasible_mana_capacity(state, id, player, None);
            let tap = pred
                .filter(|p| state.objects.get(&id).is_some_and(|o| p(o, player)))
                .map_or(0, |_| 1);
            mana.max(tap)
        })
        .sum();
    // CR 702.66a-b: Delve applies after total cost is determined and can pay
    // only generic mana by exiling cards from the caster's graveyard. Unlike
    // tap-payment keywords, this is an additional graveyard-card channel rather
    // than an alternative use of battlefield permanents.
    let delve_capacity = if matches!(tap_payment_mode, Some(ConvokeMode::Delve)) {
        state
            .objects
            .iter()
            .filter(|(id, obj)| {
                obj.zone == Zone::Graveyard
                    && obj.owner == player
                    && Some(**id) != object_id
                    && !excluded_sources.contains(*id)
            })
            .count() as u32
    } else {
        0
    };

    // CR 107.1b: Each `ManaCostShard::X` in the cost contributes `value` generic,
    // so for `{X}{X}` each point of X costs 2 mana. Dividing by `x_count` yields
    // the largest X the caster can actually afford.
    let available = pool + permanent_capacity + delve_capacity;
    let formula_max = available.saturating_sub(fixed_portion) / x_count;

    // An object-less X cost (the `max_x_value` public path used by the
    // resolution-time probe in `effects/pay.rs`) is never a cast-time spell, so
    // no cast-time cost modifier or floor can apply: return the unfloored
    // arithmetic bound unchanged.
    let Some(spell_id) = object_id else {
        return formula_max;
    };

    // CR 601.2f: When this object is the pending spell being announced, the
    // arithmetic `formula_max` (which uses the symbolic, mana-value-0 cost,
    // CR 107.3g) understates the X cap whenever cost reductions exceed the fixed
    // non-X generic — reduction capacity is clamped at generic=0 while X is
    // symbolic and the surplus is lost. It can also overstate the cap when a
    // floor (Trinisphere) applies. Recompute the FULL concrete cost for each X
    // via the single orchestrator (`concrete_cost_for_x`) — reductions →
    // target-dependent modifiers + Strive → floors LAST — so the cap reflects
    // the real locked-in total (CR 601.2f).
    //
    // We only have the captured tax-inclusive base for the pending spell; for
    // any other object (e.g. a separate trial cost) fall back to the arithmetic
    // bound, preserving prior behavior.
    let Some(pending) = state
        .pending_cast
        .as_ref()
        .filter(|p| p.object_id == spell_id)
    else {
        return formula_max;
    };
    let Some(base) = pending.base_cost.clone() else {
        return formula_max;
    };
    let ability = pending.ability.clone();

    // CR 601.2b / CR 601.2f: The concrete total is monotonic non-decreasing in X.
    // `concretize_x` adds `x * x_count` generic; non-floor and target-dependent
    // reductions subtract an X-independent amount capped via `saturating_sub`
    // (never below {0}, CR 601.2f); floors are `max(., N)`. The composition of
    // these monotonic non-decreasing maps is monotonic non-decreasing, so the
    // predicate `P(x) := concrete_cost_for_x(x).mana_value() <= available` is a
    // monotone gate: once false it stays false. The answer is the largest X with
    // `P(x)` true. A linear ascent finds it in O(maxX) cost recomputations; an
    // exponential probe + bisection over the same monotone predicate finds the
    // identical value in O(log maxX). `concrete_cost_for_x` is pure read-only
    // (clones `base`, mutates only the local), so probing X out of ascending
    // order is safe. The explicit `!probe(0)` early return below reproduces the
    // old linear loop's `saturating_sub(1)` floor exactly: when even X=0
    // overshoots, the cap is 0 (not an underflow).
    largest_x_satisfying(formula_max, |x| {
        super::casting::concrete_cost_for_x(state, player, spell_id, &ability, &base, x)
            .mana_value()
            <= available
    })
}

/// Largest `x` for which `predicate(x)` holds, given `predicate` is a monotone
/// gate — true for an initial prefix `[0, cap]` and false above it. This is the
/// search underlying the X-cost cap (CR 601.2f): the per-X concrete cost is
/// monotonic non-decreasing, so "the largest affordable X" is the top of the
/// true-prefix.
///
/// `formula_max` is only a starting estimate for the exponential probe;
/// correctness does NOT depend on it (the true cap can be lower — Trinisphere
/// floor — or higher — reductions exceeding the fixed generic). Returns `0` when
/// even `predicate(0)` is false, reproducing the linear ascent's
/// `saturating_sub(1)` floor at the `X=0` boundary. O(log cap) evaluations of
/// `predicate` versus the linear scan's O(cap); identical result by monotonicity.
fn largest_x_satisfying(formula_max: u32, predicate: impl Fn(u32) -> bool) -> u32 {
    if !predicate(0) {
        return 0;
    }

    // Exponential probe: grow `hi` off `formula_max` until `predicate(hi)` is
    // false, yielding a proven upper bound above the true cap regardless of
    // whether `formula_max` under- or over-states it. `saturating_mul` guards
    // overflow; `max(saturating_add(1))` guards `hi == 0`.
    let mut hi = formula_max.max(1);
    while predicate(hi) {
        hi = hi.saturating_mul(2).max(hi.saturating_add(1));
    }

    // Bisect `[lo, hi]` with invariant `predicate(lo)` true, `predicate(hi)`
    // false. `lo` starts at 0 (proven true above). Returns the top of the prefix.
    let mut lo = 0u32;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if predicate(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Single authority for transitioning into the payment step of a cast.
///
/// Decides, in order:
/// 1. **`ChooseXValue`** — the cost still contains an unchosen X (CR 601.2f).
/// 2. **Auto-finalize** — the concretized cost contains no hybrid/Phyrexian shards
///    and convoke is not active, so `pay_mana_cost` can deterministically satisfy it.
///    The `ManaPayment` state is skipped entirely; we proceed directly to stack push.
///    This mirrors Arena's "cast and resolve" feel for unambiguous costs.
/// 3. **`ManaPayment`** — player input is required (hybrid choice, Phyrexian life
///    payment, or convoke tap selection).
///
/// All sites that would otherwise construct `WaitingFor::ManaPayment` during a
/// cast must go through this helper so X-selection and auto-pay are never bypassed.
/// CR 702.132a: If the spell `object_id` being cast by `player` has assist, its
/// locked `cost` includes a generic component, and at least one other player is
/// still in the game, return `(generic, candidates)` — the generic amount the
/// helper may pay and the eligible helper players. Returns `None` when assist
/// does not apply. Shared by the `enter_payment_step` (X / convoke / manual) and
/// `pay_and_push_adventure` (direct auto-finalize) offer sites.
fn assist_offer_params(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
    cost: &ManaCost,
) -> Option<(u32, Vec<PlayerId>)> {
    let generic = match cost {
        ManaCost::Cost { generic, .. } if *generic > 0 => *generic,
        _ => return None,
    };
    if !super::casting::effective_spell_keywords(state, player, object_id)
        .contains(&Keyword::Assist)
    {
        return None;
    }
    let candidates: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| p.id != player && !p.is_eliminated)
        .map(|p| p.id)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    Some((generic, candidates))
}

pub fn enter_payment_step(
    state: &mut GameState,
    player: PlayerId,
    convoke_mode: Option<ConvokeMode>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if let Some(pending) = state.pending_cast.as_ref() {
        let activation_counter_x_max = pending.activation_cost.as_ref().and_then(|cost| {
            activation_counter_cost_x_max(state, player, pending.object_id, &pending.ability, cost)
        });
        if pending.ability.chosen_x.is_none()
            && (cost_has_x(&pending.cost) || activation_counter_x_max.is_some())
        {
            // CR 601.2f: Every spell-cast path that reaches X announcement must
            // carry the captured tax-inclusive base so the X cap and the locked-in
            // cost can be recomputed from scratch (`concrete_cost_for_x`). Activated
            // / mana-ability casts (no spell announcement) legitimately have no
            // base; gate the miss-detector to spell casts.
            debug_assert!(
                pending.activation_ability_index.is_some() || pending.base_cost.is_some(),
                "spell-cast PendingCast reached X announcement without a captured base_cost",
            );
            let min = pending.ability.min_x_value;
            let excluded_sources = pending
                .activation_cost
                .as_ref()
                .map(|cost| {
                    super::casting::ability_mana_payment_excluded_sources(cost, pending.object_id)
                })
                .unwrap_or_default();
            let mana_max = if cost_has_x(&pending.cost) {
                max_x_value_excluding(
                    state,
                    player,
                    &pending.cost,
                    Some(pending.object_id),
                    &excluded_sources,
                )
            } else {
                u32::MAX
            };
            let max = pending
                .activation_cost
                .as_ref()
                .and_then(|cost| additional_cost_x_max(state, player, pending.object_id, cost))
                .or(activation_counter_x_max)
                .map_or(mana_max, |cost_max| mana_max.min(cost_max));
            if min > max {
                let pending_for_cancel = pending.clone();
                state.pending_cast = None;
                super::casting::handle_cancel_cast(state, &pending_for_cancel, events);
                return Err(EngineError::ActionNotAllowed(format!(
                    "Minimum legal X value {min} exceeds maximum payable X value {max}"
                )));
            }
            let pending_cast = pending.clone();
            return Ok(WaitingFor::ChooseXValue {
                player,
                min,
                max,
                pending_cast,
                convoke_mode,
            });
        }

        let targeted_counter_resume = pending.ability.chosen_x.and_then(|chosen_x| {
            pending
                .activation_cost
                .as_ref()
                .filter(|cost| cost_has_targeted_symbolic_counter_removal(cost))
                .cloned()
                .map(|cost| (pending.as_ref().clone(), cost, chosen_x))
        });
        if let Some((mut pending, cost, chosen_x)) = targeted_counter_resume {
            let concretized_cost = concretize_chosen_x_cost(&cost, chosen_x);
            let prompt_cost = targeted_remove_counter_choice_cost(&concretized_cost)
                .unwrap_or_else(|| concretized_cost.clone());
            pending.activation_cost = Some(concretized_cost);
            state.pending_cast = None;
            return pay_additional_cost_with_source(
                state,
                player,
                prompt_cost,
                SpellCostSource::Other,
                pending,
                events,
            );
        }
    }

    if state
        .pending_cast
        .as_ref()
        .is_some_and(|pending| pending.deferred_target_selection)
    {
        let pending = *state
            .pending_cast
            .take()
            .expect("checked pending cast presence");
        return begin_deferred_target_selection(state, player, pending, events);
    }

    if state.pending_cast.as_ref().is_some_and(|pending| {
        matches!(
            pending.additional_cost_flow,
            Some(AdditionalCost::Required(_))
        )
    }) {
        let pending = *state
            .pending_cast
            .take()
            .expect("checked pending cast presence");
        return finish_pending_cost_or_cast(state, player, pending, events);
    }

    // CR 702.132a: Assist — once the total cost is locked (X chosen, modifiers
    // applied) and before the caster pays, a spell with assist whose cost has a
    // generic component lets the caster choose another player to help pay that
    // generic mana. The offer is made once per cast (`assist_state`). This site
    // covers the X / convoke / manual paths that funnel through `enter_payment_step`;
    // `pay_and_push_adventure` covers the direct auto-finalize path.
    let assist_offer = state.pending_cast.as_ref().and_then(|pending| {
        if pending.assist_state != AssistState::NotOffered {
            return None;
        }
        assist_offer_params(state, player, pending.object_id, &pending.cost)
    });
    if let Some((generic, candidates)) = assist_offer {
        if let Some(pending) = state.pending_cast.as_mut() {
            pending.assist_state = AssistState::Offered;
        }
        return Ok(WaitingFor::AssistChoosePlayer {
            player,
            candidates,
            max_generic: generic,
            convoke_mode,
        });
    }

    // CR 601.2h: Auto-finalize when no player-level decision remains. Convoke requires
    // the caster to choose which creatures to tap, so it always surfaces the modal.
    if convoke_mode.is_none() {
        if let Some(pending) = state.pending_cast.as_ref() {
            if pending.payment_mode == CastPaymentMode::Auto
                && mana_payment::classify_payment(&pending.cost)
                    == mana_payment::PaymentClassification::Unambiguous
            {
                return finalize_mana_payment(state, player, events);
            }
        }
    }

    Ok(WaitingFor::ManaPayment {
        player,
        convoke_mode,
    })
}

/// Pay the pending cast's mana cost and transition to the next game state.
///
/// Dispatches on the shape of `state.pending_cast`:
/// - **Activated ability** — pay mana, then push the ability to the stack.
/// - **X-spell with distribution** (`Fireball`-like) — pay mana to determine X total,
///   then either auto-split (even-damage) or enter `DistributeAmong` (interactive).
/// - **Normal spell** — delegate to `finalize_cast` which pays mana and pushes.
///
/// Called both from the `(ManaPayment, PassPriority)` branch in the main engine
/// dispatcher and from `enter_payment_step` when classification skips the modal.
/// This is the single authority for completing a mana payment.
/// CR 702.132a: At the non-cancellable commit point (just before `finalize_cast`),
/// spend a committed Assist contribution by tapping the helper's mana sources for
/// the agreed generic amount. This is deferred to here — rather than performed at
/// `CommitAssistPayment` — so a `CancelCast` at any intervening (still-cancellable)
/// payment step never leaves the helper's lands tapped or their mana spent. The
/// caster's owed cost was already reduced by `generic` at commit time. A no-op for
/// non-assist casts and for declined/uncommitted assists.
pub(super) fn apply_committed_assist(
    state: &mut GameState,
    pending: &PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    let AssistState::Committed { helper, generic } = pending.assist_state else {
        return Ok(());
    };
    if generic == 0 {
        return Ok(());
    }
    let probe = ManaCost::Cost {
        shards: Vec::new(),
        generic,
    };
    auto_tap_mana_sources(state, helper, &probe, events, None);
    if let Some(p) = state.players.iter_mut().find(|p| p.id == helper) {
        mana_payment::pay_from_pool(&mut p.mana_pool, &probe).map_err(|e| {
            EngineError::ActionNotAllowed(format!(
                "Assisting player could not pay {generic} generic mana at finalization: {e:?}"
            ))
        })?;
        state.layers_dirty.mark_full();
    }
    Ok(())
}

pub fn finalize_mana_payment(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 107.4f + CR 601.2f: Pause for per-shard Phyrexian choice if the cost contains
    // Phyrexian mana AND at least one shard has both mana and life options available.
    // `PendingCast` stays in `state.pending_cast` across the pause — the resume handler
    // in `engine.rs` calls `finalize_mana_payment_with_phyrexian_choices`.
    if let Some(pending_ref) = state.pending_cast.as_ref() {
        let mana_cost = pending_ref.cost.clone();
        let source_id = pending_ref.object_id;
        if pending_ref.activation_ability_index.is_some() {
            let excluded_sources = pending_ref
                .activation_cost
                .as_ref()
                .map(|activation_cost| {
                    super::casting::ability_mana_payment_excluded_sources(
                        activation_cost,
                        source_id,
                    )
                })
                .unwrap_or_default();
            let (source_types, source_subtypes) =
                super::casting::activation_source_types(state, source_id);
            let activation_ctx = PaymentContext::Activation {
                source_types: &source_types,
                source_subtypes: &source_subtypes,
            };
            if let Some(waiting) = maybe_pause_for_phyrexian_choice(
                state,
                player,
                source_id,
                &mana_cost,
                events,
                Some(&activation_ctx),
                &excluded_sources,
            ) {
                return Ok(waiting);
            }
        } else if let Some(waiting) = maybe_pause_for_phyrexian_choice(
            state,
            player,
            source_id,
            &mana_cost,
            events,
            None,
            &HashSet::new(),
        ) {
            return Ok(waiting);
        }
    }

    let pending = state
        .pending_cast
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending cast to finalize".to_string()))?;

    // CR 702.132a: commit point reached — apply any committed Assist contribution
    // (tap the helper's sources) now that the cast can no longer be cancelled.
    apply_committed_assist(state, &pending, events)?;

    if let Some(ability_index) = pending.activation_ability_index {
        let excluded_sources = pending
            .activation_cost
            .as_ref()
            .map(|cost| {
                super::casting::ability_mana_payment_excluded_sources(cost, pending.object_id)
            })
            .unwrap_or_default();
        super::casting::pay_ability_mana_cost_excluding(
            state,
            player,
            pending.object_id,
            &pending.cost,
            events,
            &excluded_sources,
        )?;
        return push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            pending.x_residual_activation,
            events,
        );
    }

    if let Some(unit) = pending.distribute {
        // CR 601.2d: X-spell distribution — pay mana first to determine X, then
        // trigger DistributeAmong with total = X.
        let pool_before = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);

        super::casting::pay_mana_cost(state, player, pending.object_id, &pending.cost, events)?;

        let pool_after = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);
        // CR 107.1b + CR 601.2f: Prefer the explicit `chosen_x` set during
        // `WaitingFor::ChooseXValue`. Fallback to inference (total paid minus
        // non-X colored/generic costs) preserves behavior for any legacy paths
        // that bypass ChooseX. ManaCost::mana_value() excludes X (CR 202.3e).
        let non_x_cost = pending.cost.mana_value();
        let total_paid = pool_before.saturating_sub(pool_after) as u32;
        let x_value = pending
            .ability
            .chosen_x
            .unwrap_or_else(|| total_paid.saturating_sub(non_x_cost));

        let targets = super::ability_utils::flatten_targets_in_chain(&pending.ability);
        // Store pending cast for post-distribution resumption. Use `ManaCost::NoCost`
        // since mana was already paid above — `finalize_cast` must not re-deduct.
        let mut pending_resumed = PendingCast::new(
            pending.object_id,
            pending.card_id,
            pending.ability,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_resumed.casting_variant = pending.casting_variant;
        pending_resumed.origin_zone = pending.origin_zone;
        pending_resumed.convoked_creatures = pending.convoked_creatures.clone();

        // CR 601.2d: "divided evenly, rounded down" — EvenSplitDamage bypasses
        // interactive distribution. Remainder is intentionally lost per Oracle text.
        if unit == DistributionUnit::EvenSplitDamage && !targets.is_empty() {
            let num = targets.len() as u32;
            let per_target = x_value / num;
            let distribution: Vec<_> = targets.iter().map(|t| (t.clone(), per_target)).collect();
            pending_resumed.ability.distribution = Some(distribution);
            state.pending_cast = Some(Box::new(pending_resumed));

            let pending = state.pending_cast.take().unwrap();
            stamp_convoked_creatures(state, pending.object_id, &pending.convoked_creatures);
            let waiting_for = finalize_cast(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                pending.casting_variant,
                pending.cast_timing_permission,
                pending.origin_zone,
                events,
            )?;
            return Ok(drain_deferred_triggers_after_stack_object_announcement(
                state,
                events,
                waiting_for,
            ));
        }

        state.pending_cast = Some(Box::new(pending_resumed));
        return Ok(WaitingFor::DistributeAmong {
            player,
            total: x_value,
            targets,
            unit,
        });
    }

    stamp_convoked_creatures(state, pending.object_id, &pending.convoked_creatures);
    let waiting_for = finalize_cast(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.cast_timing_permission,
        pending.origin_zone,
        events,
    )?;
    Ok(drain_deferred_triggers_after_stack_object_announcement(
        state,
        events,
        waiting_for,
    ))
}

fn stamp_convoked_creatures(
    state: &mut GameState,
    object_id: ObjectId,
    convoked_creatures: &[ObjectId],
) {
    if convoked_creatures.is_empty() {
        return;
    }
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.convoked_creatures = convoked_creatures.to_vec();
    }
}

/// CR 107.4f + CR 601.2f: Resume cast completion after the caster submits their
/// per-shard Phyrexian choices. Mirrors `finalize_mana_payment` but threads the
/// explicit choices through `pay_mana_cost_with_choices`.
///
/// Caller (engine dispatcher) is responsible for validating choice count and current
/// affordability via `compute_phyrexian_shards` before invoking this helper. If the
/// revalidation fails, the caller returns `EngineError::ActionNotAllowed` instead.
pub fn finalize_mana_payment_with_phyrexian_choices(
    state: &mut GameState,
    player: PlayerId,
    phyrexian_choices: &[crate::types::game_state::ShardChoice],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let pending = state
        .pending_cast
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending cast to finalize".to_string()))?;

    // CR 702.132a: commit point reached — apply any committed Assist contribution
    // (tap the helper's sources) now that the cast can no longer be cancelled.
    apply_committed_assist(state, &pending, events)?;

    if let Some(ability_index) = pending.activation_ability_index {
        let excluded_sources = pending
            .activation_cost
            .as_ref()
            .map(|cost| {
                super::casting::ability_mana_payment_excluded_sources(cost, pending.object_id)
            })
            .unwrap_or_default();
        super::casting::pay_ability_mana_cost_with_choices_excluding(
            state,
            player,
            pending.object_id,
            &pending.cost,
            Some(phyrexian_choices),
            events,
            &excluded_sources,
        )?;
        return push_activated_ability_to_stack(
            state,
            player,
            pending.object_id,
            ability_index,
            pending.ability,
            pending.activation_cost.as_ref(),
            pending.x_residual_activation,
            events,
        );
    }

    if let Some(unit) = pending.distribute {
        // CR 601.2d: X + distribution + Phyrexian is extremely rare (no known current cards).
        // Fall through to the auto-decision distribution path for safety — the Phyrexian
        // choices were already consumed via pay_mana_cost_with_choices above (the X-spell
        // distribution path is orthogonal).
        let pool_before = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);

        super::casting::pay_mana_cost_with_choices(
            state,
            player,
            pending.object_id,
            &pending.cost,
            Some(phyrexian_choices),
            events,
        )?;

        let pool_after = state
            .players
            .iter()
            .find(|pl| pl.id == player)
            .map(|pl| pl.mana_pool.total())
            .unwrap_or(0);
        let non_x_cost = pending.cost.mana_value();
        let total_paid = pool_before.saturating_sub(pool_after) as u32;
        let x_value = pending
            .ability
            .chosen_x
            .unwrap_or_else(|| total_paid.saturating_sub(non_x_cost));

        let targets = super::ability_utils::flatten_targets_in_chain(&pending.ability);
        let mut pending_resumed = PendingCast::new(
            pending.object_id,
            pending.card_id,
            pending.ability,
            crate::types::mana::ManaCost::NoCost,
        );
        pending_resumed.casting_variant = pending.casting_variant;
        pending_resumed.origin_zone = pending.origin_zone;
        pending_resumed.convoked_creatures = pending.convoked_creatures.clone();

        if unit == DistributionUnit::EvenSplitDamage && !targets.is_empty() {
            let num = targets.len() as u32;
            let per_target = x_value / num;
            let distribution: Vec<_> = targets.iter().map(|t| (t.clone(), per_target)).collect();
            pending_resumed.ability.distribution = Some(distribution);
            state.pending_cast = Some(Box::new(pending_resumed));

            let pending = state.pending_cast.take().unwrap();
            stamp_convoked_creatures(state, pending.object_id, &pending.convoked_creatures);
            let waiting_for = finalize_cast(
                state,
                player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                pending.casting_variant,
                pending.cast_timing_permission,
                pending.origin_zone,
                events,
            )?;
            return Ok(drain_deferred_triggers_after_stack_object_announcement(
                state,
                events,
                waiting_for,
            ));
        }

        state.pending_cast = Some(Box::new(pending_resumed));
        return Ok(WaitingFor::DistributeAmong {
            player,
            total: x_value,
            targets,
            unit,
        });
    }

    stamp_convoked_creatures(state, pending.object_id, &pending.convoked_creatures);
    let waiting_for = finalize_cast_with_phyrexian_choices(
        state,
        player,
        pending.object_id,
        pending.card_id,
        pending.ability,
        &pending.cost,
        pending.casting_variant,
        pending.cast_timing_permission,
        pending.origin_zone,
        Some(phyrexian_choices),
        events,
    )?;
    Ok(drain_deferred_triggers_after_stack_object_announcement(
        state,
        events,
        waiting_for,
    ))
}

/// CR 107.4f + CR 601.2f: Determine whether this cast needs to pause for per-shard
/// Phyrexian payment choice, and construct the matching `WaitingFor::PhyrexianPayment`
/// if so.
///
/// Auto-taps mana sources first (idempotent: already-tapped lands are skipped) so the
/// shard-options computation reflects the pool the caster will actually spend from.
/// Returns `Some(WaitingFor::PhyrexianPayment {...})` when at least one Phyrexian shard
/// can deduct life; otherwise returns `None` so the caller proceeds with the existing
/// auto-decision path.
pub(super) fn maybe_pause_for_phyrexian_choice(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
    payment_context: Option<&PaymentContext<'_>>,
    excluded_sources: &HashSet<ObjectId>,
) -> Option<WaitingFor> {
    // CR 107.4f: Fast reject — pause only when cost has intrinsic Phyrexian
    // shards OR the player has a K'rrik-style grant whose color appears in the
    // cost. The grant scan is cheap (single battlefield scan).
    let life_colors = super::static_abilities::player_life_payment_colors(state, player);
    match cost {
        crate::types::mana::ManaCost::Cost { shards, .. } => {
            let any_intrinsic_phyrexian = shards.iter().any(|s| {
                matches!(
                    mana_payment::shard_to_mana_type(*s),
                    mana_payment::ShardRequirement::Phyrexian(..)
                        | mana_payment::ShardRequirement::HybridPhyrexian(..)
                )
            });
            let any_promoted = !life_colors.is_empty()
                && shards.iter().any(|s| {
                    // After promotion, a Phyrexian-shape shard appears iff
                    // the grant covers one of the shard's colors.
                    !matches!(
                        mana_payment::effective_shard_requirement(
                            mana_payment::shard_to_mana_type(*s),
                            life_colors,
                        ),
                        mana_payment::ShardRequirement::Single(..)
                            | mana_payment::ShardRequirement::Hybrid(..)
                            | mana_payment::ShardRequirement::TwoGenericHybrid(..)
                            | mana_payment::ShardRequirement::ColorlessHybrid(..)
                            | mana_payment::ShardRequirement::Snow
                            | mana_payment::ShardRequirement::X
                            | mana_payment::ShardRequirement::TwoOrMoreColorSource
                    )
                });
            if !any_intrinsic_phyrexian && !any_promoted {
                return None;
            }
        }
        _ => return None,
    }

    // CR 601.2h + CR 605: Auto-tap mana sources before shard-options computation so
    // the simulation reflects the actual post-tap pool.
    let events_before = events.len();
    if payment_context.is_none() && excluded_sources.is_empty() {
        auto_tap_mana_sources(state, player, cost, events, Some(source_id));
    } else {
        auto_tap_mana_sources_with_context_excluding(
            state,
            player,
            cost,
            events,
            Some(source_id),
            payment_context,
            excluded_sources,
        );
    }
    // CR 605.4a: Resolve coupled `TapsForMana` triggered mana abilities inline so
    // the bonus mana is in the pool before Phyrexian shard options are computed.
    super::triggers::resolve_tap_mana_triggers_inline(state, events, events_before);

    let spell_meta = payment_context
        .is_none()
        .then(|| super::casting::build_spell_meta(state, player, source_id))
        .flatten();
    let spell_ctx = spell_meta.as_ref().map(PaymentContext::Spell);
    let effective_payment_context = payment_context.or(spell_ctx.as_ref());
    let any_color = super::casting::player_can_spend_as_any_color_for_payment(
        state,
        player,
        source_id,
        effective_payment_context,
    );
    // CR 107.4f + CR 118.1: Single-authority permission bundle — passes
    // `life_colors` through to `compute_phyrexian_shards` so K'rrik-promoted
    // shards surface in the pause UI.
    let permissions =
        super::static_abilities::build_cost_permission_context(state, player, any_color);

    let (shards, payable) = {
        let player_data = state.players.iter().find(|p| p.id == player)?;
        let shards = mana_payment::compute_phyrexian_shards(
            &player_data.mana_pool,
            cost,
            effective_payment_context,
            permissions,
        );
        // CR 601.2h: Only pause when the cost is actually payable in aggregate.
        // Phyrexian shards may surface as `LifeOnly` even when the non-Phyrexian
        // portion (e.g., a {1} generic shard) is unpayable; in that case the
        // downstream finalizer must reject with "Cannot pay mana cost" rather
        // than pausing on an unpayable cast.
        let payable = mana_payment::can_pay_for_spell(
            &player_data.mana_pool,
            cost,
            effective_payment_context,
            permissions,
        );
        (shards, payable)
    };
    if !payable {
        return None;
    }

    // CR 107.4f + CR 601.2h: Pause whenever any shard would deduct life — either
    // because the player explicitly chooses (`ManaOrLife`) or because life is the
    // only remaining payment route (`LifeOnly`). The player retains the CR 601.2h
    // option to refuse the cast via `CancelCast` rather than have life silently
    // deducted (issue #704). `ManaOnly` shards have no life consequence and
    // continue to auto-resolve.
    let has_life_consequence = shards.iter().any(|s| {
        matches!(
            s.options,
            crate::types::game_state::ShardOptions::ManaOrLife
                | crate::types::game_state::ShardOptions::LifeOnly,
        )
    });
    if !has_life_consequence {
        return None;
    }

    Some(WaitingFor::PhyrexianPayment {
        player,
        spell_object: source_id,
        shards,
    })
}

/// Return true if the given cost contains a `ManaCostShard::X` shard.
pub fn cost_has_x(cost: &crate::types::mana::ManaCost) -> bool {
    match cost {
        crate::types::mana::ManaCost::Cost { shards, .. } => {
            shards.iter().any(|s| matches!(s, ManaCostShard::X))
        }
        _ => false,
    }
}

/// Return true when an activated ability's cost contains `{X}` anywhere in its
/// cost tree (including composite costs like `{T}, Pay {X}`).
pub fn ability_cost_has_x(cost: &crate::types::ability::AbilityCost) -> bool {
    use crate::types::ability::AbilityCost;
    match cost {
        AbilityCost::Mana { cost } => cost_has_x(cost),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            costs.iter().any(ability_cost_has_x)
        }
        _ => false,
    }
}

/// Return true when `source_id`'s most recent pending activation carries an
/// `{X}` activation cost. Used by `AbilityActivated` trigger matchers.
pub fn pending_activation_cost_has_x(state: &GameState, source_id: ObjectId) -> bool {
    let Some((_, ability_index)) = state
        .pending_activations
        .iter()
        .rev()
        .find(|(id, _)| *id == source_id)
    else {
        return false;
    };
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };
    let Some(ability) = obj.abilities.get(*ability_index) else {
        return false;
    };
    ability.cost.as_ref().is_some_and(ability_cost_has_x)
}

/// Extract a mana sub-cost containing X from an activated ability cost.
///
/// CR 107.1b + CR 601.2f: X must be chosen before mana is paid. For composite
/// activation costs (e.g., `Tap + Pay {X}`), the mana sub-cost with X is
/// routed through `ChooseXValue`/`ManaPayment` while the remaining sub-costs
/// (Tap, Sacrifice, etc.) are deferred to after payment via the pending cast's
/// `activation_cost`.
///
/// Returns `Some((mana_cost, remaining))` where `mana_cost` is the extracted
/// Mana cost and `remaining` is the rest of the cost (None if the whole cost
/// was the Mana sub-cost). Returns `None` if no X mana cost is present.
pub fn extract_x_mana_cost(
    cost: &crate::types::ability::AbilityCost,
) -> Option<(
    crate::types::mana::ManaCost,
    Option<crate::types::ability::AbilityCost>,
)> {
    use crate::types::ability::AbilityCost;
    match cost {
        AbilityCost::Mana { cost: mana } if cost_has_x(mana) => Some((mana.clone(), None)),
        AbilityCost::Composite { costs } => {
            let idx = costs
                .iter()
                .position(|sub| matches!(sub, AbilityCost::Mana { cost: m } if cost_has_x(m)))?;
            let mut remaining = costs.clone();
            let AbilityCost::Mana { cost: extracted } = remaining.remove(idx) else {
                unreachable!("position guarantees Mana variant")
            };
            let remaining_cost = match remaining.len() {
                0 => None,
                1 => Some(remaining.into_iter().next().unwrap()),
                _ => Some(AbilityCost::Composite { costs: remaining }),
            };
            Some((extracted, remaining_cost))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Comparator, ControllerRef, Effect, FilterProp,
        PtStat, PtValueScope, QuantityExpr, ReplacementDefinition, ReplacementMode,
        StaticDefinition, TargetFilter, TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;

    /// CR 614.1a + CR 608.2n (PLAN §8 Risk #2): the Invoke Calamity free-cast
    /// "if this spell would be put into your graveyard, exile it instead" rider
    /// is installed by `apply_exile_instead_of_graveyard_rider` as a synthetic
    /// self-scoped `Moved` replacement (the boolean flag is deleted). Driving a
    /// real resolution of a spell carrying the rider must redirect its
    /// stack→graveyard default move to exile through the replacement pipeline.
    #[test]
    fn invoke_calamity_rider_exiles_free_cast_spell_on_resolution() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(state.next_object_id);
        let spell = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Free-Cast Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        // Install the rider exactly as the FreeCastFromZones resolution path does.
        super::apply_exile_instead_of_graveyard_rider(&mut state, spell);
        assert!(
            state.objects[&spell]
                .replacement_definitions
                .iter_all()
                .any(|d| d.event == ReplacementEvent::Moved
                    && d.destination_zone == Some(Zone::Graveyard)),
            "rider installs a self-scoped graveyard→exile Moved replacement"
        );

        let resolved = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            spell,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        super::stack::resolve_top(&mut state, &mut events);

        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "the rider's synthetic Moved redirect must send the resolved spell to exile"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell),
            "the redirected spell must not also reach the graveyard"
        );
    }

    /// CR 608.2b + CR 616.1 (review fix): a free-cast spell carrying the Invoke
    /// Calamity rider FIZZLES under a single Rest in Peace — the rider and RIP
    /// are two simultaneous graveyard→exile redirect candidates, so the fizzle
    /// arm parks a CR 616.1 ordering prompt. The paused fizzle must still run
    /// the resolution epilogue (StackResolved emission + trigger-context /
    /// die-result clears) before bailing — the pre-fix bare `return` skipped it,
    /// leaking stale cross-resolution context and never emitting StackResolved.
    /// Answering the prompt via the real `GameAction::ChooseReplacement` then
    /// delivers the parked move to exile.
    #[test]
    fn invoke_calamity_rider_fizzle_under_rip_parks_choice_with_clean_epilogue() {
        let mut state = GameState::new_two_player(42);

        // Board-wide RIP-class redirect: any card's graveyard move → exile.
        let rip = create_object(
            &mut state,
            CardId(900),
            PlayerId(1),
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rip)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            destination: Zone::Exile,
                            origin: None,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            face_down_profile: None,
                        },
                    ))
                    .description("Rest in Peace".to_string()),
            );

        // Target creature that will be removed to force the fizzle arm.
        let target = create_object(
            &mut state,
            CardId(901),
            PlayerId(1),
            "Target Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&target).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Free-cast spell carrying the rider, targeting the bear.
        let card_id = CardId(state.next_object_id);
        let spell = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Free-Cast Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        super::apply_exile_instead_of_graveyard_rider(&mut state, spell);
        let resolved = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(target)],
            spell,
            PlayerId(0),
        );
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id,
                ability: Some(resolved),
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // CR 608.2b: remove the target so every target is illegal at resolution.
        crate::game::zones::move_to_zone(&mut state, target, Zone::Graveyard, &mut Vec::new());

        // Seed cross-resolution context the fizzle epilogue must clear even on
        // the paused path (resolve_top does not touch these for a Spell entry
        // before the fizzle arm, so a leaked value is attributable to the bail).
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 0,
        });
        state.current_trigger_events = vec![GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 0,
        }];
        state.current_trigger_match_count = Some(2);
        state.die_result_this_resolution = Some(4);

        let mut events = Vec::new();
        super::stack::resolve_top(&mut state, &mut events);

        // CR 616.1: rider + RIP are two applicable redirects → ordering prompt.
        assert!(
            matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
            "two simultaneous graveyard→exile redirects must park a CR 616.1 ordering choice, got {:?}",
            state.waiting_for
        );
        // Review fix: the fizzle epilogue runs before the pause-bail.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::StackResolved { object_id } if *object_id == spell
            )),
            "paused fizzle must still emit StackResolved"
        );
        assert!(
            state.current_trigger_event.is_none(),
            "paused fizzle must clear current_trigger_event"
        );
        assert!(
            state.current_trigger_events.is_empty(),
            "paused fizzle must clear current_trigger_events"
        );
        assert!(
            state.current_trigger_match_count.is_none(),
            "paused fizzle must clear current_trigger_match_count"
        );
        assert!(
            state.die_result_this_resolution.is_none(),
            "paused fizzle must clear die_result_this_resolution"
        );

        // Answer the CR 616.1 prompt through the real action pipeline; the
        // resume path delivers the parked move with both redirects applied in
        // the chosen order (both route to exile).
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("replacement-ordering choice must be acceptable");

        assert_eq!(
            state.objects[&spell].zone,
            Zone::Exile,
            "the fizzled free-cast spell must be exiled by the redirect after the choice resolves"
        );
        assert!(
            !state.players[0].graveyard.contains(&spell),
            "the redirected spell must not also reach the graveyard"
        );
    }

    /// Reference implementation of the X-cap search: the pre-refactor linear
    /// ascent. Returns the largest `x` with `predicate(x)` true, clamped at 0.
    fn linear_x_reference(predicate: impl Fn(u32) -> bool) -> u32 {
        let mut x = 0u32;
        loop {
            if !predicate(x) {
                return x.saturating_sub(1);
            }
            x += 1;
        }
    }

    /// `largest_x_satisfying` (exponential probe + bisection) must return the
    /// byte-identical X cap of the old linear ascent for every monotone cost
    /// shape. Each shape models `concrete_cost_for_x(x).mana_value()` as a
    /// monotone-non-decreasing function of X, then asserts the two searches agree.
    #[test]
    fn largest_x_satisfying_matches_linear_reference() {
        // cost(x) = max(fixed + x * x_count - reduction, floor); predicate is
        // cost(x) <= available. `reduction` and `floor` exercise the understate
        // (reduction > fixed) and overstate (Trinisphere floor) cases the cap
        // computation warns about.
        let cost = |fixed: u32, x_count: u32, reduction: u32, floor: u32, x: u32| -> u32 {
            (fixed + x * x_count).saturating_sub(reduction).max(floor)
        };

        for available in [0u32, 1, 2, 3, 5, 8, 13, 50, 100] {
            for fixed in [0u32, 1, 3, 6] {
                for x_count in [1u32, 2] {
                    for reduction in [0u32, 2, 9] {
                        for floor in [0u32, 3, 9] {
                            let predicate =
                                |x: u32| cost(fixed, x_count, reduction, floor, x) <= available;
                            // The arithmetic estimate the real function passes in.
                            let formula_max = available.saturating_sub(fixed) / x_count;
                            assert_eq!(
                                largest_x_satisfying(formula_max, predicate),
                                linear_x_reference(predicate),
                                "mismatch at available={available} fixed={fixed} \
                                 x_count={x_count} reduction={reduction} floor={floor}",
                            );
                        }
                    }
                }
            }
        }
    }

    fn make_pending(source_id: ObjectId) -> PendingCast {
        PendingCast {
            object_id: source_id,
            card_id: CardId(0),
            ability: ResolvedAbility::new(
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
                Vec::new(),
                source_id,
                PlayerId(0),
            ),
            cost: ManaCost::NoCost,
            base_cost: None,
            activation_cost: None,
            activation_ability_index: Some(0),
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            x_residual_activation: false,
        }
    }

    fn install_optional_discard_replacement(state: &mut GameState) -> ObjectId {
        let replacement_source = create_object(
            state,
            CardId(9_002),
            PlayerId(0),
            "Discard Replacement".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&replacement_source)
            .unwrap()
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Discard)
                    .mode(ReplacementMode::Optional { decline: None })
                    .description("Apply discard replacement".to_string()),
            );
        replacement_source
    }

    #[test]
    fn graveyard_exile_additional_cost_x_max_is_eligible_graveyard_size() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(9_100),
            PlayerId(0),
            "Harvest Pyre".to_string(),
            Zone::Hand,
        );
        for idx in 0..4 {
            create_object(
                &mut state,
                CardId(9_110 + idx),
                PlayerId(0),
                format!("Graveyard filler {idx}"),
                Zone::Graveyard,
            );
        }
        let cost = AbilityCost::Exile {
            count: EXILE_COST_X,
            zone: Some(Zone::Graveyard),
            filter: None,
        };
        assert_eq!(
            additional_cost_x_max(&state, PlayerId(0), source, &cost),
            Some(4)
        );
    }

    #[test]
    fn graveyard_exile_additional_cost_concretizes_after_x_is_chosen() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let source = create_object(
            &mut state,
            CardId(9_200),
            caster,
            "Harvest Pyre".to_string(),
            Zone::Hand,
        );
        let gy_cards: Vec<ObjectId> = (0..5)
            .map(|idx| {
                create_object(
                    &mut state,
                    CardId(9_210 + idx),
                    caster,
                    format!("Graveyard filler {idx}"),
                    Zone::Graveyard,
                )
            })
            .collect();
        let mut ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
            },
            Vec::new(),
            source,
            caster,
        );
        ability.chosen_x = Some(3);
        let pending = PendingCast::new(
            source,
            CardId(9_200),
            ability,
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Red],
            },
        );
        let mut events = Vec::new();
        let waiting = pay_additional_cost(
            &mut state,
            caster,
            AbilityCost::Exile {
                count: EXILE_COST_X,
                zone: Some(Zone::Graveyard),
                filter: None,
            },
            pending,
            &mut events,
        )
        .expect("chosen X should route to graveyard exile payment");
        match waiting {
            WaitingFor::PayCost {
                kind:
                    PayCostKind::ExileFromZone {
                        zone: ExileCostSourceZone::Graveyard,
                    },
                choices,
                count,
                ..
            } => {
                assert_eq!(count, 3);
                for card in gy_cards {
                    assert!(choices.contains(&card));
                }
            }
            other => panic!("expected PayCost ExileFromZone, got {other:?}"),
        }
    }

    #[test]
    fn remove_counter_additional_cost_x_max_counts_counters_not_targets() {
        use crate::types::ability::REMOVE_COUNTER_COST_X;
        use crate::types::counter::{CounterMatch, CounterType};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(9_003),
            PlayerId(0),
            "Marath Stand-In".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 3);

        let cost = AbilityCost::RemoveCounter {
            target: None,
            count: REMOVE_COUNTER_COST_X,
            counter_type: CounterMatch::OfType(CounterType::Plus1Plus1),
            selection: CounterCostSelection::SingleObject,
        };

        assert_eq!(
            additional_cost_x_max(&state, PlayerId(0), source, &cost),
            Some(3),
            "X must be capped by removable +1/+1 counters, not by eligible target count"
        );
    }

    /// CR 603.10a + CR 701.21a + CR 601.2h: when a spell's additional cost
    /// sacrifices ≥2 permanents simultaneously, a co-departing
    /// leaves-the-battlefield / "whenever you sacrifice" observer among the
    /// sacrificed group observes every co-sacrificed permanent (itself + the
    /// rest) via last-known information. This drives the FULL `apply_action`
    /// cast pipeline (not a `process_triggers` shape test): the `SelectCards`
    /// action runs `handle_sacrifice_for_cost` → `finish_pending_cost_or_cast`
    /// → `pay_and_push` → `WaitingFor::Priority` → `run_post_action_pipeline` →
    /// `process_triggers` over the same `events` vector that still carries the
    /// cost-sacrifice `ZoneChanged` records. The spell has NO kicker and NO
    /// deferred targets, so the cast lands in the SAME action — the only path
    /// where the producer stamp is readable (the kicker/target-paused sub-case
    /// is the deferred cross-action seam; see
    /// `cost_paid_multi_sacrifice_kicker_paused_under_observes`). Without the
    /// stamp at `handle_sacrifice_for_cost` the observer fires once (its own
    /// departure only); with it, twice.
    #[test]
    fn cost_paid_multi_sacrifice_blood_artist_co_departed() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{TargetFilter, TriggerDefinition};
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // The spell being cast: a no-target, no-kicker effect (Scry) so the cast
        // lands directly via `pay_and_push` to `Priority` in the same action.
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sacrificial Scry".to_string(),
            Zone::Hand,
        );

        // Blood-Artist-class observer: ChangesZone origin Battlefield, valid_card
        // = any creature, executes GainLife 1 on its controller — detectable as a
        // +1 life delta per co-departed creature once the triggers resolve.
        let observer = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Blood Artist Stand-In".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::default().with_type(TypeFilter::Creature),
                ))
                .execute(crate::types::ability::AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ));
            obj.trigger_definitions.push(trig.clone());
            Arc::make_mut(&mut obj.base_trigger_definitions).push(trig);
        }

        // A plain creature co-sacrificed alongside the observer.
        let plain = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Plain Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&plain).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }

        // Build the pending spell cast (NOT an activated ability: index None so
        // `finish_pending_cost_or_cast` routes to `pay_and_push`).
        let mut pending = make_pending(spell);
        pending.activation_ability_index = None;
        pending.card_id = CardId(1);
        pending.origin_zone = Zone::Hand;

        // CR 601.2a/601.2i: the spell was announced onto the stack before cost
        // payment; `pay_and_push` finalizes that existing entry rather than
        // pushing a new one. Mirror the announcement entry the real cast flow
        // leaves on the stack while costs are paid.
        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Park at the cost-sacrifice prompt for two creatures, then drive the
        // real `apply_action` resolution by selecting both.
        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::Sacrifice,
            choices: vec![observer, plain],
            count: 2,
            // Fixed (non-variable) sacrifice cost of exactly 2 — min == count.
            min_count: 2,
            resume: CostResume::Spell {
                spell: Box::new(pending),
            },
        };

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![observer, plain],
            },
        )
        .expect("select both creatures to sacrifice as the spell's additional cost");

        // The two co-departed observer triggers (same controller) require an
        // explicit ordering; drain the prompt with identity order, then resolve
        // the stack (observer triggers + the spell itself).
        crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        for _ in 0..30 {
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.stack.is_empty() {
                break;
            }
            apply_as_current(&mut state, GameAction::PassPriority).expect("pass priority");
        }

        // The observer's ChangesZone trigger fired once per co-sacrificed creature
        // (itself + the plain bear), so life is 20 + 2 = 22. Without the producer
        // stamp at `handle_sacrifice_for_cost`, the `co_departed` group on each
        // ZoneChanged record is empty and the observer fires once (life 21).
        assert_eq!(
            state.players[0].life, 22,
            "co-departing LTB observer must fire once per permanent sacrificed to \
             pay one additional cost (20 + 2 = 22)"
        );
    }

    /// CR 603.6c + CR 603.10a + CR 603.3b (DEFERRED kicker/target-paused
    /// sub-case): when an additional sacrifice cost is followed by a deferred
    /// target/kicker/modal pause, `finish_pending_cost_or_cast` returns a
    /// non-`Priority` `WaitingFor` (`TargetSelection` here), so `apply_action`
    /// does NOT run `run_post_action_pipeline` over the cost-sacrifice
    /// `ZoneChanged` events in this action, and the cast lands in a LATER
    /// `apply_action` whose fresh `events` vector no longer carries the records
    /// stamped by `handle_sacrifice_for_cost`. To bridge that cross-action seam,
    /// `handle_sacrifice_for_cost` parks the cost-payment observer triggers into
    /// `deferred_triggers` at the pause boundary (the established B2 pattern from
    /// `engine_resolution_choices::batch_or_drain_observer_triggers`); they are
    /// held while the announced spell remains on the stack and drained at the
    /// next resolution boundary after the cast completes. The co-departing
    /// observer therefore fires once per co-sacrificed creature (itself + the
    /// plain bear): life 20 + 2 = 22.
    #[test]
    fn cost_paid_multi_sacrifice_kicker_paused_under_observes() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::{TargetFilter, TriggerDefinition};
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.players[0].life = 20;
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // A spell whose effect TARGETS (DealDamage to a creature) and whose
        // target selection is DEFERRED to after costs are paid — so after the
        // additional sacrifice cost is paid the cast pauses on TargetSelection
        // (not Priority), and run_post_action_pipeline never scans the
        // cost-sacrifice events in this action.
        let spell = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Paused Sacrifice Bolt".to_string(),
            Zone::Hand,
        );

        let observer = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Blood Artist Stand-In".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let trig = TriggerDefinition::new(TriggerMode::ChangesZone)
                .origin(Zone::Battlefield)
                .valid_card(TargetFilter::Typed(
                    TypedFilter::default().with_type(TypeFilter::Creature),
                ))
                .execute(crate::types::ability::AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                ));
            obj.trigger_definitions.push(trig.clone());
            Arc::make_mut(&mut obj.base_trigger_definitions).push(trig);
        }

        let plain = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Plain Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&plain).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }

        // TWO legal damage targets so deferred target selection is AMBIGUOUS and
        // genuinely pauses on `WaitingFor::TargetSelection` (a single legal target
        // auto-resolves inline and would land the cast in the same action,
        // defeating the pause this sentinel models).
        for (cid, name) in [
            (CardId(4), "Opposing Bear A"),
            (CardId(5), "Opposing Bear B"),
        ] {
            let victim = create_object(
                &mut state,
                cid,
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&victim).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        let mut pending = make_pending(spell);
        pending.activation_ability_index = None;
        pending.card_id = CardId(1);
        pending.origin_zone = Zone::Hand;
        // Targeted effect with deferred target selection: the cast pauses after
        // costs are paid (CR 601.2c).
        pending.ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::Creature)),
                damage_source: None,
            },
            Vec::new(),
            spell,
            PlayerId(0),
        );
        pending.deferred_target_selection = true;

        state.stack.push_back(StackEntry {
            id: spell,
            source_id: spell,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        state.waiting_for = WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::Sacrifice,
            choices: vec![observer, plain],
            count: 2,
            // Fixed (non-variable) sacrifice cost of exactly 2 — min == count.
            min_count: 2,
            resume: CostResume::Spell {
                spell: Box::new(pending),
            },
        };

        apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![observer, plain],
            },
        )
        .expect("select both creatures to sacrifice as the spell's additional cost");

        // Precondition for the gap: after paying the cost the cast PAUSED on
        // deferred target selection (two ambiguous legal targets), so this action
        // returned a non-`Priority` `WaitingFor` and `apply_action` never ran
        // `run_post_action_pipeline` over the cost-sacrifice `ZoneChanged` events.
        assert!(
            matches!(state.waiting_for, WaitingFor::TargetSelection { .. }),
            "kicker/target-paused sub-case must pause on TargetSelection after the \
             additional sacrifice cost (got {:?})",
            state.waiting_for
        );

        // CR 603.6c + CR 603.10a + CR 603.3b: the cost-sacrifice `ZoneChanged`
        // records (carrying the producer co-departed stamp from
        // `handle_sacrifice_for_cost`) were emitted in THIS pausing action.
        // `handle_sacrifice_for_cost` now parks their observer triggers into
        // `deferred_triggers` because the cast paused on a non-`Priority`
        // `WaitingFor` (so `run_post_action_pipeline` will not scan this
        // action's `events`). The parked triggers drain when the cast finishes
        // and the player would receive priority, while the announced spell still
        // remains on the stack. Drive the rest of the cast (choose a damage
        // target, then resolve the stack) and confirm the co-departing observer
        // fired once per co-sacrificed creature (itself + the plain bear) — life
        // 20 + 2 = 22.
        if let WaitingFor::TargetSelection { target_slots, .. } = state.waiting_for.clone() {
            // Pick the first legal damage target to land the cast on the stack.
            let target = target_slots
                .first()
                .and_then(|slot| slot.legal_targets.first())
                .cloned()
                .expect("at least one legal damage target for the paused cast");
            apply_as_current(
                &mut state,
                GameAction::ChooseTarget {
                    target: Some(target),
                },
            )
            .expect("submit the deferred damage target");
        } else {
            panic!(
                "expected TargetSelection after the additional sacrifice cost (got {:?})",
                state.waiting_for
            );
        }

        if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
            crate::game::triggers::drain_order_triggers_with_identity(&mut state);
        }
        assert_eq!(
            state.deferred_triggers.len(),
            0,
            "cost-sacrifice triggers must be drained at cast completion, not left \
             parked behind the spell"
        );
        assert_eq!(
            state.stack.len(),
            3,
            "the two cost-sacrifice triggers must be on the stack above the spell \
             before priority is offered"
        );
        assert!(
            matches!(state.stack[0].kind, StackEntryKind::Spell { .. }),
            "the announced spell must remain below the cost-triggered abilities"
        );
        assert!(
            state
                .stack
                .iter()
                .skip(1)
                .all(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })),
            "cost-sacrifice triggers must sit above the announced spell before it resolves"
        );

        // Resolve the stack (observer triggers + the spell itself).
        for _ in 0..30 {
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) || state.stack.is_empty() {
                break;
            }
            apply_as_current(&mut state, GameAction::PassPriority).expect("pass priority");
        }

        assert_eq!(
            state.players[0].life, 22,
            "co-departing LTB observer must fire once per permanent sacrificed to \
             pay one additional cost even when the cast PAUSES on target selection \
             before Priority (20 + 2 = 22)"
        );
    }

    #[test]
    fn stamp_controller_controlled_as_cast_uses_quantity_resolver_snapshot() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Conditional Spell".to_string(),
            Zone::Hand,
        );
        let faerie_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Faerie".to_string(),
            Zone::Battlefield,
        );
        let faerie = state.objects.get_mut(&faerie_id).unwrap();
        faerie.card_types.core_types.push(CoreType::Creature);
        faerie.card_types.subtypes.push("Faerie".to_string());

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Faerie".to_string())
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::InZone {
                    zone: Zone::Battlefield,
                }]),
        );
        let mut ability = ResolvedAbility::new(
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source_id,
            PlayerId(0),
        )
        .sub_ability(
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                source_id,
                PlayerId(0),
            )
            .condition(AbilityCondition::ControllerControlledMatchingAsCast {
                filter: filter.clone(),
            }),
        );

        stamp_controller_controlled_as_cast(&state, &mut ability, PlayerId(0), source_id);

        assert_eq!(ability.context.controller_controlled_as_cast, vec![filter]);
    }

    #[test]
    fn activation_one_of_choice_replaces_nested_first_branch() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let source = create_object(
            &mut state,
            CardId(100),
            player,
            "Nested Choice Relic".to_string(),
            Zone::Battlefield,
        );
        let mut pending = make_pending(source);
        pending.activation_cost = Some(AbilityCost::Composite {
            costs: vec![AbilityCost::Composite {
                costs: vec![AbilityCost::OneOf {
                    costs: vec![
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                        },
                        AbilityCost::Mana {
                            cost: ManaCost::NoCost,
                        },
                    ],
                }],
            }],
        });
        let choices = match pending.activation_cost.as_ref().unwrap() {
            AbilityCost::Composite { costs } => match &costs[0] {
                AbilityCost::Composite { costs } => match &costs[0] {
                    AbilityCost::OneOf { costs } => costs.clone(),
                    other => panic!("expected nested OneOf, got {other:?}"),
                },
                other => panic!("expected nested Composite, got {other:?}"),
            },
            other => panic!("expected Composite, got {other:?}"),
        };
        let mut events = Vec::new();

        let waiting = handle_activation_cost_one_of_choice(
            &mut state,
            player,
            pending,
            &choices,
            1,
            &mut events,
        )
        .unwrap();

        assert!(matches!(
            waiting,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(
            state.stack.iter().any(|entry| entry.source_id == source),
            "activation should be pushed after the nested OneOf is replaced and paid"
        );
    }

    #[test]
    fn manual_payment_mode_pauses_unambiguous_spell_cost() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let spell = create_object(
            &mut state,
            CardId(100),
            caster,
            "Manual Payment Spell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&spell).unwrap().card_types.core_types = vec![CoreType::Instant];
        state.players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(900),
            false,
            Vec::new(),
        ));
        crate::game::stack::push_to_stack(
            &mut state,
            StackEntry {
                id: spell,
                source_id: spell,
                controller: caster,
                kind: StackEntryKind::Spell {
                    card_id: CardId(100),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut Vec::new(),
        );

        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            spell,
            caster,
        );
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        };
        let mut events = Vec::new();

        let waiting = pay_and_push_adventure(
            &mut state,
            caster,
            spell,
            CardId(100),
            ability,
            &cost,
            None,
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Manual,
            &mut events,
        )
        .expect("manual payment should pause before paying mana");

        assert!(matches!(
            waiting,
            WaitingFor::ManaPayment {
                player,
                convoke_mode: None,
            } if player == caster
        ));
        let pending = state
            .pending_cast
            .as_ref()
            .expect("manual payment should preserve pending cast");
        assert_eq!(pending.payment_mode, CastPaymentMode::Manual);
        assert_eq!(pending.cost, cost);
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert!(state.stack.iter().any(|entry| {
            entry.id == spell
                && matches!(
                    entry.kind,
                    StackEntryKind::Spell {
                        ability: None,
                        actual_mana_spent: 0,
                        ..
                    }
                )
        }));
    }

    #[test]
    fn next_kicker_option_walks_independent_kicker_costs_by_position() {
        let state = GameState::new_two_player(42);
        let source_id = ObjectId(7);
        let mut pending = make_pending(source_id);
        pending.activation_ability_index = None;
        pending.additional_cost_flow = Some(AdditionalCost::Kicker {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::Blue],
                        generic: 2,
                    },
                },
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::Black],
                        generic: 2,
                    },
                },
            ],
            repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
        });

        let (variant, _, repeatability) =
            next_kicker_option(&state, PlayerId(0), &pending).expect("first kicker option");
        assert_eq!(variant, KickerVariant::First);
        assert!(repeatability.is_once());

        pending
            .ability
            .context
            .kickers_paid
            .push(KickerVariant::First);
        let (variant, _, repeatability) =
            next_kicker_option(&state, PlayerId(0), &pending).expect("second kicker option");
        assert_eq!(variant, KickerVariant::Second);
        assert!(repeatability.is_once());
    }

    #[test]
    fn next_kicker_option_repeats_multikicker_first_variant() {
        let state = GameState::new_two_player(42);
        let source_id = ObjectId(7);
        let mut pending = make_pending(source_id);
        pending.activation_ability_index = None;
        pending.additional_cost_flow = Some(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Red],
                    generic: 1,
                },
            }],
            repeatability: crate::types::ability::AdditionalCostRepeatability::Repeatable,
        });

        pending
            .ability
            .context
            .kickers_paid
            .push(KickerVariant::First);
        pending
            .ability
            .context
            .kickers_paid
            .push(KickerVariant::First);

        let (variant, _, repeatability) =
            next_kicker_option(&state, PlayerId(0), &pending).expect("repeatable kicker option");
        assert_eq!(variant, KickerVariant::First);
        assert!(repeatability.is_repeatable());
    }

    #[test]
    fn granted_casualty_additional_cost_prompts_for_matching_spell() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            caster,
            "Silverquill Source".to_string(),
            Zone::Battlefield,
        );
        let grant = crate::types::ability::StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Casualty(1),
        })
        .affected(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Instant).controller(ControllerRef::You),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(grant);

        let spell = create_object(
            &mut state,
            CardId(2),
            caster,
            "Test Instant".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let sacrifice = create_object(
            &mut state,
            CardId(3),
            caster,
            "Power One Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&sacrifice).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
        }

        let mut events = Vec::new();
        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(2),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &ManaCost::NoCost,
            None,
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Auto,
            &mut events,
        )
        .expect("granted casualty should be castable");

        match waiting {
            WaitingFor::OptionalCostChoice { cost, .. } => match cost {
                AdditionalCost::Optional {
                    cost: AbilityCost::Sacrifice(cost),
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                } => {
                    assert_eq!(cost.requirement, SacrificeRequirement::count(1));
                    match cost.target {
                        TargetFilter::Typed(tf) => {
                            assert!(tf.type_filters.contains(&TypeFilter::Creature));
                            assert!(tf.properties.contains(&FilterProp::PtComparison {
                                stat: PtStat::Power,
                                scope: PtValueScope::Current,
                                comparator: Comparator::GE,
                                value: QuantityExpr::Fixed { value: 1 },
                            }));
                        }
                        other => panic!("expected typed casualty sacrifice filter, got {other:?}"),
                    }
                }
                other => panic!("expected optional casualty sacrifice cost, got {other:?}"),
            },
            other => panic!("expected OptionalCostChoice, got {other:?}"),
        }
    }

    /// CR 702.78a: Conspire granted by a `CastWithKeyword` static (Wort, the
    /// Raidmother / Rassilon) must surface the optional "tap two color-sharing
    /// creatures" additional cost (`TapCreatures { count: 2 }`) on a matching
    /// spell — exactly the printed-Conspire offer, but driven by
    /// `effective_conspire_additional_cost`. Discriminates CHANGE 2: without the
    /// conspire ladder arm, no `OptionalCostChoice` is offered.
    #[test]
    fn granted_conspire_additional_cost_prompts_for_matching_spell() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let source = create_object(
            &mut state,
            CardId(1),
            caster,
            "Conspire Grantor".to_string(),
            Zone::Battlefield,
        );
        let grant = crate::types::ability::StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Conspire,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Instant).controller(ControllerRef::You),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(grant);

        let spell = create_object(
            &mut state,
            CardId(2),
            caster,
            "Test Instant".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            // CR 702.78a: the spell must be colored so candidate creatures can
            // "share a color with it"; red here.
            obj.color = vec![ManaColor::Red];
        }

        // Two untapped red creatures the caster controls — eligible conspire tap
        // targets. The optional offer is gated on payability
        // (`AbilityCost::is_payable`), so the cost only surfaces when at least
        // two color-sharing creatures exist.
        for card in [CardId(3), CardId(4)] {
            let creature = create_object(
                &mut state,
                card,
                caster,
                "Red Creature".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.color = vec![ManaColor::Red];
        }

        let mut events = Vec::new();
        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(2),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &ManaCost::NoCost,
            None,
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Auto,
            &mut events,
        )
        .expect("granted conspire should be castable");

        match waiting {
            WaitingFor::OptionalCostChoice { cost, .. } => match cost {
                AdditionalCost::Optional {
                    cost: AbilityCost::TapCreatures { count, filter },
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                } => {
                    assert_eq!(count, 2, "conspire taps exactly two creatures");
                    match filter {
                        TargetFilter::Typed(tf) => {
                            assert!(tf.type_filters.contains(&TypeFilter::Creature));
                            assert!(tf.properties.iter().any(|p| matches!(
                                p,
                                FilterProp::SharesQuality {
                                    quality: crate::types::ability::SharedQuality::Color,
                                    ..
                                }
                            )));
                        }
                        other => panic!("expected typed conspire tap filter, got {other:?}"),
                    }
                }
                other => panic!("expected optional conspire TapCreatures cost, got {other:?}"),
            },
            other => panic!("expected OptionalCostChoice, got {other:?}"),
        }
    }

    /// CR 118.9 + CR 604.1: A `CastWithAlternativeCost { {0} }` static on a
    /// battlefield permanent (Rooftop Storm) grants its controller {0} as an
    /// alternative cost for matching spells in hand — but only for the
    /// controller's matching spells, never an opponent's or a non-matching one.
    #[test]
    fn granted_alternative_mana_cost_matches_controller_filter() {
        use crate::types::ability::StaticDefinition;

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        // Rooftop Storm: {0} for Zombie creature spells you cast.
        let source = create_object(
            &mut state,
            CardId(1),
            caster,
            "Rooftop Storm".to_string(),
            Zone::Battlefield,
        );
        let grant = StaticDefinition::new(StaticMode::CastWithAlternativeCost {
            cost: AbilityCost::Mana {
                cost: ManaCost::zero(),
            },
            timing_permission: None,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::creature()
                .subtype("Zombie".to_string())
                .controller(ControllerRef::You),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(grant);

        // Zombie creature in caster's hand → grant applies, {0} payable.
        let zombie = create_object(
            &mut state,
            CardId(2),
            caster,
            "Test Zombie".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&zombie).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Zombie".to_string());
        }
        assert_eq!(
            payable_spell_alternative_cost(&state, caster, zombie),
            Some(AbilityCost::Mana {
                cost: ManaCost::zero()
            }),
            "Zombie creature you cast must receive the {{0}} alternative cost"
        );

        // Non-Zombie creature in caster's hand → grant does not apply.
        let nonzombie = create_object(
            &mut state,
            CardId(3),
            caster,
            "Test Elf".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&nonzombie).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Elf".to_string());
        }
        assert_eq!(
            payable_spell_alternative_cost(&state, caster, nonzombie),
            None,
            "non-Zombie spell must not receive the grant"
        );

        // Zombie creature in the OPPONENT's hand → controller gate blocks it.
        let opp_zombie = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opponent Zombie".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&opp_zombie).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Zombie".to_string());
        }
        assert_eq!(
            payable_spell_alternative_cost(&state, PlayerId(1), opp_zombie),
            None,
            "opponent's Zombie must not receive the controller-You grant"
        );
    }

    /// CR 118.9 + CR 107.14: Primal Prayers grants {E} as an alternative cost
    /// for creature spells with MV ≤ 3 that the controller casts.
    #[test]
    fn granted_alternative_energy_cost_matches_creature_mv_filter() {
        use crate::types::ability::{Comparator, QuantityExpr, StaticDefinition};

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        state.players[caster.0 as usize].energy = 2;

        let source = create_object(
            &mut state,
            CardId(10),
            caster,
            "Primal Prayers".to_string(),
            Zone::Battlefield,
        );
        let grant = StaticDefinition::new(StaticMode::CastWithAlternativeCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            timing_permission: None,
        })
        .affected(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 },
                }]),
        ));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(grant);

        let rampager = create_object(
            &mut state,
            CardId(11),
            caster,
            "Greenbelt Rampager".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&rampager).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(1);
        }
        assert_eq!(
            payable_spell_alternative_cost(&state, caster, rampager),
            Some(AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 1 }
            }),
            "MV 1 creature must receive the {{E}} alternative cost"
        );

        let expensive = create_object(
            &mut state,
            CardId(12),
            caster,
            "Big Creature".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&expensive).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.mana_cost = ManaCost::generic(4);
        }
        assert_eq!(
            payable_spell_alternative_cost(&state, caster, expensive),
            None,
            "MV 4 creature must not receive the MV≤3 grant"
        );
    }

    fn create_starting_town(state: &mut GameState, card_id: CardId) -> ObjectId {
        let town = create_object(
            state,
            card_id,
            PlayerId(0),
            "Starting Town".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&town).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 1 },
                    },
                ],
            }),
        );
        town
    }

    /// CR 605.3b + CR 106.1a: Build a Sunken-Ruins-style filter land with both
    /// the secondary `{T}: Add {C}` ability and the primary
    /// `{U/B}, {T}: Add {U}{U}, {U}{B}, or {B}{B}` ability.
    fn create_filter_land(
        state: &mut GameState,
        name: &str,
        a: ManaColor,
        b: ManaColor,
    ) -> ObjectId {
        let land = create_object(
            state,
            CardId(900),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        // Only the combinations ability is what we exercise in auto-tap tests.
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::ChoiceAmongCombinations {
                        options: vec![vec![a, a], vec![a, b], vec![b, b]],
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        land
    }

    #[test]
    fn auto_tap_filter_land_covers_mixed_shards() {
        // Cost `{U}{B}` with a single Sunken Ruins available: the combo
        // pre-pass must pick the `{U}{B}` combination and tap the land once,
        // producing both colors atomically.
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
                generic: 0,
            },
            &mut events,
            None,
        );

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 1);
    }

    #[test]
    fn auto_tap_filter_land_picks_double_color_combination() {
        // Cost `{U}{U}`: combo pre-pass must pick `{U}{U}` (covers both
        // shards), not `{U}{B}` (wastes black).
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            },
            &mut events,
            None,
        );

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Blue),
            2,
            "auto-tap should pick {{U}}{{U}} combination"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    #[test]
    fn auto_tap_filter_land_covers_colored_plus_generic() {
        // CR 605.3b: Cost `{U}{1}`. Combo pre-pass picks `{U}{U}` — the first
        // {U} covers the shard, the second lands in the pool and can pay the
        // {1} generic (via the regular payment path). Auto-tap's job is to
        // ensure sufficient mana enters the pool; actual shard/generic
        // consumption happens in the downstream payment step.
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            },
            &mut events,
            None,
        );

        assert_eq!(
            state.players[0].mana_pool.total(),
            2,
            "filter land produces 2 blue mana — covers shard + generic"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 2);
    }

    #[test]
    fn auto_tap_does_not_use_combo_for_pure_generic() {
        // CR 605.3b: Pure generic cost `{1}` with a filter land available.
        // The combo pre-pass must NOT commit the combo (no shards to cover)
        // because spending a 2-mana combination on 1 generic wastes half
        // the production. Phase 2 prefers the land's secondary
        // `{T}: Add {C}` (non-combo) ability for the generic instead.
        let mut state = GameState::new_two_player(42);
        create_filter_land(
            &mut state,
            "Sunken Ruins",
            ManaColor::Blue,
            ManaColor::Black,
        );

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![],
                generic: 1,
            },
            &mut events,
            None,
        );

        // The secondary `{T}: Add {C}` should satisfy the generic with a
        // single colorless mana — NOT the combo (which would produce 2 mana
        // of a random colored combination for only 1 generic).
        assert_eq!(state.players[0].mana_pool.total(), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1,
            "pure generic should draw from `{{T}}: Add {{C}}`, not the combination"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 0);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Black), 0);
    }

    /// CR 605.1b: A non-land permanent's `{T}: Add {C}{C}` mana ability
    /// (Sol Ring's shape). One activation produces two colorless mana, so the
    /// source surfaces as a single atomic combination row.
    fn create_colorless_rock(state: &mut GameState, name: &str, count: i32) -> ObjectId {
        let rock = create_object(
            state,
            CardId(950),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&rock).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: count },
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        rock
    }

    #[test]
    fn auto_tap_uses_colorless_combo_for_pure_generic() {
        // CR 107.4b: generic mana can be paid with any type of mana, including
        // colorless. Sol Ring's `{T}: Add {C}{C}` is a combination with no
        // non-combo sibling ability, so it must still be tapped for a pure
        // generic `{2}` — the regression was that Phase 2 skipped every
        // combination source, leaving the cost unpayable (and the spell
        // reported uncastable by the shared affordability preview).
        let mut state = GameState::new_two_player(42);
        let sol_ring = create_colorless_rock(&mut state, "Sol Ring", 2);

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
            &mut events,
            None,
        );

        assert!(
            state.objects.get(&sol_ring).unwrap().tapped,
            "Sol Ring must be tapped to pay the generic cost"
        );
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            2,
            "`{{T}}: Add {{C}}{{C}}` must contribute both colorless mana to generic"
        );
    }

    #[test]
    fn auto_tap_prefers_colorless_rock_over_colored_lands_for_generic() {
        // "Use Sol Ring first": for a generic cost, color-locked colorless
        // mana is spent before flexible colored lands, so the colored sources
        // stay open. A single Sol Ring tap covers `{2}` and both Forests are
        // left untapped.
        let mut state = GameState::new_two_player(42);
        let sol_ring = create_colorless_rock(&mut state, "Sol Ring", 2);
        let mut forests = Vec::new();
        for i in 0..2 {
            let forest = create_object(
                &mut state,
                CardId(960 + i),
                PlayerId(0),
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            forests.push(forest);
        }

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
            &mut events,
            None,
        );

        assert!(
            state.objects.get(&sol_ring).unwrap().tapped,
            "the colorless rock should fill generic before any colored land"
        );
        for forest in &forests {
            assert!(
                !state.objects.get(forest).unwrap().tapped,
                "colored lands must stay open when colorless mana covers the generic"
            );
        }
    }

    #[test]
    fn auto_tap_filter_land_does_not_prompt_user() {
        // Regression: the filter-land activation must short-circuit the
        // `WaitingFor::ChooseManaColor` prompt during auto-tap — the caller
        // picks the combination via `ProductionOverride::Combination`.
        // If the prompt surfaced, `resolve_mana_ability` would return Ok but
        // with no mana added to the pool. Verify mana actually landed.
        let mut state = GameState::new_two_player(42);
        create_filter_land(&mut state, "Mystic Gate", ManaColor::White, ManaColor::Blue);

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::Blue],
                generic: 0,
            },
            &mut events,
            None,
        );

        // CR 605.3b: combination mana lands in the pool atomically, no prompt.
        assert_eq!(state.players[0].mana_pool.total(), 2);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::PermanentTapped { .. })));
    }

    #[test]
    fn auto_tap_pays_mana_source_sub_cost_from_other_source() {
        // Nykthos `{T}: Add {C}` can pay Sunscorched Divide's `{1}, {T}`
        // activation, which then produces `{R}{W}` for a spell cost. The
        // planner must not discard Sunscorched just because its mana sub-cost
        // is not payable from the initial empty pool.
        let mut state = GameState::new_two_player(42);
        let nykthos = create_object(
            &mut state,
            CardId(901),
            PlayerId(0),
            "Nykthos, Shrine to Nyx".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&nykthos).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let divide = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Sunscorched Divide".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&divide).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![ManaColor::Red, ManaColor::White],
                            contribution: crate::types::ability::ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Mana {
                            cost: ManaCost::generic(1),
                        },
                        AbilityCost::Tap,
                    ],
                }),
            );
        }

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Red, ManaCostShard::White],
                generic: 0,
            },
            &mut events,
            None,
        );

        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Red), 1);
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::White), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            0
        );
        assert!(state.objects.get(&nykthos).unwrap().tapped);
        assert!(state.objects.get(&divide).unwrap().tapped);
    }

    #[test]
    fn auto_tap_prefers_non_life_mana_sources_when_equivalent() {
        let mut state = GameState::new_two_player(42);
        create_starting_town(&mut state, CardId(10));
        let island = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&island).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
        }

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 1,
            },
            &mut events,
            None,
        );

        assert_eq!(
            state.players[0].life, 20,
            "auto-pay should avoid paying life"
        );
        assert_eq!(state.players[0].mana_pool.count_color(ManaType::Blue), 1);
        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { amount: -1, .. })),
            "auto-pay should not emit a life payment when an equivalent non-life line exists"
        );
    }

    #[test]
    fn auto_tap_skips_sources_when_pool_already_covers_cost() {
        // CR 601.2g regression: if the player has already tapped Sol Ring ({C}{C})
        // and an Island ({U}) before casting a {2}{U} spell, auto-tap must NOT
        // tap three more untapped lands — the floating pool already covers the
        // entire cost.
        use crate::types::mana::ManaUnit;
        let mut state = GameState::new_two_player(42);

        // Three untapped basic lands as potential victims if auto-tap misbehaves.
        let mut lands = Vec::new();
        for i in 0..3 {
            let land = create_object(
                &mut state,
                CardId(200 + i),
                PlayerId(0),
                "Island".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
            lands.push(land);
        }

        // Pre-float {C}{C}{U} into the pool (as if the player tapped sources
        // before initiating the cast).
        let floated_source = ObjectId(99);
        for color in [ManaType::Colorless, ManaType::Colorless, ManaType::Blue] {
            state.players[0].mana_pool.add(ManaUnit {
                color,
                source_id: floated_source,
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
            &mut events,
            None,
        );

        // Pool unchanged — reduce_cost_by_pool consumed the residual to NoCost.
        assert_eq!(
            state.players[0].mana_pool.total(),
            3,
            "pool must not grow when it already covers the cost"
        );
        // No permanents tapped, no mana produced.
        for land in &lands {
            assert!(
                !state.objects.get(land).unwrap().tapped,
                "no land should be tapped when floating mana covers the cost"
            );
        }
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::PermanentTapped { .. })),
            "auto-tap must emit no PermanentTapped events when pool covers cost"
        );
    }

    #[test]
    fn auto_tap_taps_only_the_shortfall_when_pool_partially_covers() {
        // CR 601.2g: If the pool covers part of the cost, auto-tap must only
        // produce the residual — not the full cost. Pool has {U}; cost is
        // {2}{U}; expect exactly 2 additional sources tapped (for the {2}).
        use crate::types::mana::ManaUnit;
        let mut state = GameState::new_two_player(42);

        let mut lands = Vec::new();
        for i in 0..4 {
            let land = create_object(
                &mut state,
                CardId(300 + i),
                PlayerId(0),
                "Island".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Island".to_string());
            lands.push(land);
        }

        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(99),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            },
            &mut events,
            None,
        );

        // Pool grew by exactly 2 (the residual {2} → two {U} from Islands).
        // Original {U} stays floating; two new units produced.
        assert_eq!(
            state.players[0].mana_pool.total(),
            3,
            "pool should grow by exactly the residual — 2 mana for the generic {{2}}"
        );
        let tapped_count = lands
            .iter()
            .filter(|l| state.objects.get(l).unwrap().tapped)
            .count();
        assert_eq!(
            tapped_count, 2,
            "exactly 2 lands should tap for the residual; the pre-floated {{U}} covers the shard"
        );
    }

    #[test]
    fn sacrifice_for_cost_valid_selection() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seer".to_string(),
            Zone::Battlefield,
        );
        let creature_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Goblin B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Give source an ability so push_activated_ability_to_stack can record activation
        state.objects.get_mut(&source).unwrap().abilities =
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )]);

        let pending = make_pending(source);
        let legal = vec![creature_a, creature_b];
        let chosen = vec![creature_a];
        let mut events = Vec::new();

        let result = handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            None,
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: &legal,
                chosen: &chosen,
            },
            &mut events,
        );

        assert!(result.is_ok());
        // creature_a should be in graveyard
        assert!(state.players[0].graveyard.contains(&creature_a));
        // creature_b should still be on battlefield
        assert!(state.battlefield.contains(&creature_b));
    }

    #[test]
    fn sacrifice_for_cost_wrong_count() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seer".to_string(),
            Zone::Battlefield,
        );
        let creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin".to_string(),
            Zone::Battlefield,
        );

        let pending = make_pending(source);
        let legal = vec![creature];
        let mut events = Vec::new();

        // Select 0 when count=1
        let result = handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            None,
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: &legal,
                chosen: &[],
            },
            &mut events,
        );
        assert!(result.is_err());
    }

    #[test]
    fn sacrifice_for_cost_illegal_permanent() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Seer".to_string(),
            Zone::Battlefield,
        );
        let legal_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        let illegal_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );

        let pending = make_pending(source);
        let legal = vec![legal_creature]; // Only legal_creature is eligible
        let chosen = vec![illegal_creature]; // Trying to sacrifice non-eligible
        let mut events = Vec::new();

        let result = handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            None,
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: &legal,
                chosen: &chosen,
            },
            &mut events,
        );
        assert!(result.is_err());
    }

    #[test]
    fn variable_sacrifice_for_cost_sets_chosen_x_from_selection_size() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chatterfang Test".to_string(),
            Zone::Battlefield,
        );
        let squirrel_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Squirrel A".to_string(),
            Zone::Battlefield,
        );
        let squirrel_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Squirrel B".to_string(),
            Zone::Battlefield,
        );
        for squirrel in [squirrel_a, squirrel_b] {
            let obj = state.objects.get_mut(&squirrel).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Squirrel".to_string());
        }

        state.objects.get_mut(&source).unwrap().abilities =
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: crate::types::ability::QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: crate::types::ability::TargetFilter::Controller,
                },
            )]);

        let mut pending = make_pending(source);
        pending.ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: crate::types::ability::TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        let legal = vec![squirrel_a, squirrel_b];
        let chosen = vec![squirrel_a, squirrel_b];
        let mut events = Vec::new();

        handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            None,
            CostSelection {
                min_count: 0,
                count: legal.len(),
                legal_permanents: &legal,
                chosen: &chosen,
            },
            &mut events,
        )
        .expect("variable sacrifice selection should succeed");

        let Some(stack_entry) = state.stack.back() else {
            panic!("activated ability should be pushed to the stack");
        };
        let chosen_x = match &stack_entry.kind {
            crate::types::game_state::StackEntryKind::ActivatedAbility { ability, .. } => {
                ability.chosen_x
            }
            other => panic!("expected activated ability on stack, got {other:?}"),
        };
        assert_eq!(chosen_x, Some(2));
        assert_eq!(state.objects[&squirrel_a].zone, Zone::Graveyard);
        assert_eq!(state.objects[&squirrel_b].zone, Zone::Graveyard);
    }

    #[test]
    fn discard_for_cost_resume_can_pause_on_each_remaining_discard() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        install_optional_discard_replacement(&mut state);
        let source = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Discard Outlet".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )]);
        let first = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "First Card".to_string(),
            Zone::Hand,
        );
        let second = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Second Card".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let waiting = handle_discard_for_cost(
            &mut state,
            PlayerId(0),
            make_pending(source),
            2,
            &[first, second],
            &[first, second],
            &mut events,
        )
        .expect("first discard should pause for replacement choice");

        assert!(matches!(waiting, WaitingFor::ReplacementChoice { .. }));
        assert_eq!(state.objects[&first].zone, Zone::Hand);
        assert!(state.stack.is_empty());

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("first replacement choice should resume to the second discard");
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert_eq!(state.objects[&first].zone, Zone::Graveyard);
        assert_eq!(state.objects[&second].zone, Zone::Hand);
        assert!(state.stack.is_empty());

        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("second replacement choice should finish cost payment");
        assert_eq!(state.objects[&second].zone, Zone::Graveyard);
        assert_eq!(state.stack.len(), 1, "activation should reach the stack");
    }

    /// CR 603.6c + CR 118.3: Sacrificing a permanent as part of a cost is a
    /// game event that triggers other abilities (e.g., Crime Novelist's
    /// "whenever you sacrifice an artifact"). Regression: cost-payment
    /// sacrifices must emit `PermanentSacrificed` so observer triggers fire,
    /// just like spell-effect sacrifices do.
    ///
    /// Covers the broader "sacrifice-cost-trigger" class — Crime Novelist,
    /// Syr Ginger, Mayhem Devil, Cruel Celebrant, Korvold etc.
    #[test]
    fn sacrifice_as_cost_emits_event_for_observer_trigger() {
        use crate::game::triggers::process_triggers;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        // Source: an artifact with an activated ability whose cost sacrifices
        // a Treasure (an artifact). Effect doesn't matter — we just need the
        // sacrifice cost to fire.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().abilities =
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);

        // Treasure-like artifact controlled by player 0 — sacrificed as cost.
        let treasure = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
        }

        // Observer: Crime-Novelist-style trigger.
        // "Whenever you sacrifice an artifact, ..." => valid_card = Typed{Artifact, controller: You}.
        let observer = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Crime Novelist".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::Sacrificed);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            // Trigger executes a draw so we can detect it on the stack.
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        // Pay the cost via the cost-payment helper directly — same path
        // taken when an activated ability's sacrifice subcost resumes after
        // `WaitingFor::SacrificeForCost`.
        let pending = make_pending(source);
        let mut events = Vec::new();
        handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            None,
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: &[treasure],
                chosen: &[treasure],
            },
            &mut events,
        )
        .expect("cost-payment sacrifice succeeds");

        // The cost-payment path must emit `PermanentSacrificed` — same event
        // the spell-effect sacrifice path emits — so observer triggers can fire.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == treasure
            )),
            "cost-payment sacrifice must emit PermanentSacrificed (got: {:?})",
            events
                .iter()
                .filter(|e| !matches!(e, GameEvent::ZoneChanged { .. }))
                .collect::<Vec<_>>()
        );

        // Run the trigger pass over the cost-payment events. Observer's
        // Sacrificed-mode trigger must register on the stack.
        let stack_before = state.stack.len();
        process_triggers(&mut state, &events);
        assert!(
            state.stack.len() > stack_before,
            "observer's `whenever you sacrifice an artifact` trigger must fire \
             when an artifact is sacrificed as part of an activated-ability cost"
        );
    }

    /// CR 603.6c + CR 603.10a: Sacrificing an artifact TOKEN as a cost must
    /// fire `whenever <artifact> is put into a graveyard from the battlefield`
    /// triggers (Syr Ginger). The token does cease to exist after SBAs (CR
    /// 704.5d), but the leaves-battlefield event still fires per CR 603.10a
    /// (last-known information). Cost-payment must emit the same `ZoneChanged`
    /// event that effect-sacrifice emits.
    #[test]
    fn sacrifice_token_as_cost_fires_dies_zone_trigger() {
        use crate::game::triggers::process_triggers;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{ControllerRef, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);

        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&source).unwrap().abilities =
            Arc::new(vec![crate::types::ability::AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Scry {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )]);

        // Artifact TOKEN (e.g., Treasure / Food) controlled by player 0.
        let token = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Treasure Token".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&token).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.is_token = true;
        }

        // Syr-Ginger-style observer: ChangesZone Battlefield → Graveyard,
        // valid_card = Artifact controller=You. Note: `Another` is not
        // exercised here — the sacrificed token is a different object.
        let observer = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Syr Ginger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::ChangesZone);
            trig.origin = Some(Zone::Battlefield);
            trig.destination = Some(Zone::Graveyard);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        let pending = make_pending(source);
        let mut events = Vec::new();
        handle_sacrifice_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            None,
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: &[token],
                chosen: &[token],
            },
            &mut events,
        )
        .expect("cost-payment sacrifice succeeds");

        // Cost-payment must emit ZoneChanged (battlefield → graveyard) for the
        // sacrificed token — Dies / leaves-battlefield triggers depend on it.
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == token
            )),
            "cost-payment sacrifice must emit ZoneChanged battlefield→graveyard"
        );

        let stack_before = state.stack.len();
        process_triggers(&mut state, &events);
        assert!(
            state.stack.len() > stack_before,
            "observer's `whenever an artifact is put into a graveyard from the battlefield` \
             trigger must fire when an artifact token is sacrificed as activation cost"
        );
    }

    /// End-to-end repro for L9-9: activate a Treasure-style mana ability
    /// (`{T}, Sacrifice this artifact: Add one mana of any color`). After
    /// `GameAction::ActivateAbility` resolves, Crime Novelist's sacrifice
    /// trigger must land on the stack via `run_post_action_pipeline`.
    #[test]
    fn mana_ability_sacrifice_cost_fires_observer_trigger_end_to_end() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{
            AbilityCost, ControllerRef, ManaContribution, ManaProduction, TargetFilter, TypeFilter,
            TypedFilter,
        };
        use crate::types::mana::ManaColor;
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Treasure token: `{T}, Sacrifice: Add one mana of any color`.
        let treasure = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
            obj.entered_battlefield_turn = Some(1); // CR 302.1: avoid summoning sickness for {T}
            Arc::make_mut(&mut obj.abilities).push(
                crate::types::ability::AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                ManaColor::White,
                                ManaColor::Blue,
                                ManaColor::Black,
                                ManaColor::Red,
                                ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                    ],
                }),
            );
        }

        // Crime-Novelist-style observer: Sacrificed-mode trigger on Artifact
        // controlled by `You`. Trigger executes a draw so it's detectable on
        // the stack (mana abilities don't use the stack — but the *triggered*
        // ability fired by the sacrifice does).
        let observer = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Crime Novelist".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::Sacrificed);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        // Activate the Treasure's mana ability — this is a "any color" choice,
        // so we expect a ChooseManaColor prompt before resolution.
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
            },
        )
        .expect("activation succeeds");

        // If the engine prompts for a mana color, pick one.
        if matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }) {
            apply_as_current(
                &mut state,
                GameAction::ChooseManaColor {
                    choice: crate::types::game_state::ManaChoice::SingleColor(
                        crate::types::mana::ManaType::Red,
                    ),
                    count: 1,
                },
            )
            .expect("color choice succeeds");
        }

        // Crime Novelist's Sacrificed trigger must have fired and landed
        // on the stack — even though the source mana ability did not.
        assert!(
            state.stack.iter().any(|entry| entry.source_id == observer),
            "Crime Novelist's sacrifice trigger must land on the stack \
             when a Treasure is sacrificed as part of an activated mana \
             ability cost (got stack: {:?}, treasure zone: {:?})",
            state.stack.iter().map(|e| e.source_id).collect::<Vec<_>>(),
            state.objects.get(&treasure).map(|o| o.zone),
        );
    }

    /// End-to-end repro for L9-9 (Syr Ginger class): activate a Treasure
    /// mana ability whose cost sacrifices the Treasure. Syr Ginger's
    /// ChangesZone (Battlefield → Graveyard) trigger must fire — same fix
    /// path as Crime Novelist, since `process_triggers` scans both
    /// `PermanentSacrificed` and `ZoneChanged` events emitted by the
    /// sacrifice cost step.
    #[test]
    fn mana_ability_sacrifice_cost_fires_dies_zone_trigger_end_to_end() {
        use crate::game::engine::apply_as_current;
        use crate::types::ability::TriggerDefinition;
        use crate::types::ability::{
            AbilityCost, ControllerRef, ManaContribution, ManaProduction, TargetFilter, TypeFilter,
            TypedFilter,
        };
        use crate::types::mana::ManaColor;
        use crate::types::phase::Phase;
        use crate::types::triggers::TriggerMode;
        use crate::types::GameAction;

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let treasure = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
            obj.entered_battlefield_turn = Some(1);
            Arc::make_mut(&mut obj.abilities).push(
                crate::types::ability::AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::AnyOneColor {
                            count: QuantityExpr::Fixed { value: 1 },
                            color_options: vec![
                                ManaColor::White,
                                ManaColor::Blue,
                                ManaColor::Black,
                                ManaColor::Red,
                                ManaColor::Green,
                            ],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Composite {
                    costs: vec![
                        AbilityCost::Tap,
                        AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                    ],
                }),
            );
        }

        // Syr Ginger-style observer: ChangesZone Battlefield → Graveyard,
        // valid_card = Artifact controller=You.
        let observer = create_object(
            &mut state,
            CardId(201),
            PlayerId(0),
            "Syr Ginger".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&observer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.entered_battlefield_turn = Some(1);
            let mut trig = TriggerDefinition::new(TriggerMode::ChangesZone);
            trig.origin = Some(Zone::Battlefield);
            trig.destination = Some(Zone::Graveyard);
            trig.valid_card = Some(TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Artifact],
                controller: Some(ControllerRef::You),
                properties: vec![],
            }));
            trig.execute = Some(Box::new(crate::types::ability::AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )));
            obj.trigger_definitions.push(trig);
        }

        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: treasure,
                ability_index: 0,
            },
        )
        .expect("activation succeeds");

        if matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }) {
            apply_as_current(
                &mut state,
                GameAction::ChooseManaColor {
                    choice: crate::types::game_state::ManaChoice::SingleColor(
                        crate::types::mana::ManaType::Red,
                    ),
                    count: 1,
                },
            )
            .expect("color choice succeeds");
        }

        assert!(
            state.stack.iter().any(|entry| entry.source_id == observer),
            "Syr Ginger's `whenever an artifact is put into a graveyard from \
             the battlefield` trigger must land on the stack when a Treasure \
             token is sacrificed as part of an activated mana ability cost"
        );
    }

    // -- Strive cost calculation tests ------------------------------------------

    #[test]
    fn strive_surcharge_with_three_targets() {
        // CR 601.2f: Strive cost increase — adds per-target surcharge.
        // Base cost {2}{R}, strive cost {1}{R}, 3 targets -> {2}{R} + 2*{1}{R} = {4}{R}{R}{R}
        use crate::types::mana::ManaCostShard;
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        };
        let strive_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        };
        let target_count = 3usize;
        let adjusted = (1..target_count).fold(base.clone(), |acc, _| {
            super::restrictions::add_mana_cost(&acc, &strive_cost)
        });
        // Total mana value: 2+1 (base) + 2*(1+1) = 3 + 4 = 7
        assert_eq!(adjusted.mana_value(), 7);
        match adjusted {
            ManaCost::Cost { generic, shards } => {
                assert_eq!(generic, 4); // 2 + 1 + 1
                assert_eq!(
                    shards
                        .iter()
                        .filter(|s| matches!(s, ManaCostShard::Red))
                        .count(),
                    3
                ); // R + R + R
            }
            _ => panic!("expected ManaCost::Cost"),
        }
    }

    #[test]
    fn strive_no_surcharge_with_one_target() {
        // CR 601.2f: Strive only adds cost for targets beyond the first.
        // With 1 target, no surcharge is added.
        use crate::types::mana::ManaCostShard;
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        let target_count = 1usize;
        // No fold iterations when target_count == 1
        let adjusted = if target_count > 1 {
            let strive_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
            (1..target_count).fold(base.clone(), |acc, _| {
                super::restrictions::add_mana_cost(&acc, &strive_cost)
            })
        } else {
            base.clone()
        };
        assert_eq!(adjusted.mana_value(), base.mana_value());
    }

    #[test]
    fn strive_surcharge_with_two_targets() {
        // CR 601.2f: Strive cost increase — with 2 targets, add strive cost once.
        use crate::types::mana::ManaCostShard;
        let base = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        };
        let strive_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        };
        let target_count = 2usize;
        let adjusted = (1..target_count).fold(base.clone(), |acc, _| {
            super::restrictions::add_mana_cost(&acc, &strive_cost)
        });
        // {1}{U} + {2}{U} = {3}{U}{U}
        assert_eq!(adjusted.mana_value(), 5);
    }

    // --- CR 601.2b: Defiler cost reduction tests ---

    #[test]
    fn find_defiler_reduction_matches_color() {
        use crate::types::ability::StaticDefinition;
        use crate::types::mana::{ManaColor, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // Create a green creature spell being cast
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&spell_id).unwrap().color = vec![ManaColor::Green];

        // Create Defiler of Vigor (green Defiler) on battlefield
        let defiler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        let reduction = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        };
        state
            .objects
            .get_mut(&defiler_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: reduction.clone(),
            }));

        let result = find_defiler_reduction(&state, PlayerId(0), spell_id);
        assert!(
            result.is_some(),
            "Should find Defiler reduction for green spell"
        );
        let (life, mana_red) = result.unwrap();
        assert_eq!(life, 2);
        assert_eq!(mana_red, reduction);
    }

    #[test]
    fn find_defiler_reduction_ignores_wrong_color() {
        use crate::types::ability::StaticDefinition;
        use crate::types::mana::{ManaColor, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // Create a red creature spell
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Goblin Guide".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&spell_id).unwrap().color = vec![ManaColor::Red];

        // Create Defiler of Vigor (green) — should not match red spell
        let defiler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&defiler_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }));

        let result = find_defiler_reduction(&state, PlayerId(0), spell_id);
        assert!(
            result.is_none(),
            "Green Defiler should not reduce red spell"
        );
    }

    #[test]
    fn find_defiler_reduction_ignores_non_permanent() {
        use crate::types::ability::StaticDefinition;
        use crate::types::mana::{ManaColor, ManaCostShard};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);

        // Create a green instant spell (not a permanent)
        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Instant);
        state.objects.get_mut(&spell_id).unwrap().color = vec![ManaColor::Green];

        // Create Defiler
        let defiler_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Defiler of Vigor".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&defiler_id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::DefilerCostReduction {
                color: ManaColor::Green,
                life_cost: 2,
                mana_reduction: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            }));

        let result = find_defiler_reduction(&state, PlayerId(0), spell_id);
        assert!(
            result.is_none(),
            "Defiler should not reduce non-permanent spells"
        );
    }

    #[test]
    fn handle_defiler_payment_accepted_reduces_cost() {
        use crate::types::mana::ManaCostShard;

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;

        let spell_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Hand,
        );

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Permanent".to_string(),
                description: None,
            },
            Vec::new(),
            spell_id,
            PlayerId(0),
        );

        let pending = PendingCast::new(
            spell_id,
            CardId(1),
            ability,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green, ManaCostShard::Green],
                generic: 2,
            },
        );

        let mana_reduction = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 0,
        };

        let mut events = Vec::new();
        let _result = handle_defiler_payment(
            &mut state,
            PlayerId(0),
            pending,
            2,
            &mana_reduction,
            true,
            &mut events,
        );

        // Life should be reduced by 2
        assert_eq!(state.players[0].life, 18, "Life should decrease by 2");

        // Check that a LifeChanged event was emitted
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::LifeChanged {
                    player_id,
                    amount: -2
                } if *player_id == PlayerId(0)
            )),
            "Should emit LifeChanged event"
        );
    }

    fn subtype_filter(subtype: &str) -> TargetFilter {
        TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(subtype.to_string())))
    }

    fn add_subtype(state: &mut GameState, object_id: ObjectId, subtype: &str) {
        state
            .objects
            .get_mut(&object_id)
            .unwrap()
            .card_types
            .subtypes
            .push(subtype.to_string());
    }

    #[test]
    fn behold_choices_include_controlled_permanents_and_hand_cards() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Piercing Exhale".to_string(),
            Zone::Hand,
        );
        let battlefield_dragon = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Dragon Permanent".to_string(),
            Zone::Battlefield,
        );
        add_subtype(&mut state, battlefield_dragon, "Dragon");
        let hand_dragon = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Dragon Card".to_string(),
            Zone::Hand,
        );
        add_subtype(&mut state, hand_dragon, "Dragon");
        let opposing_dragon = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Opposing Dragon".to_string(),
            Zone::Battlefield,
        );
        add_subtype(&mut state, opposing_dragon, "Dragon");

        let choices =
            eligible_behold_choices(&state, PlayerId(0), source, &subtype_filter("Dragon"));

        assert!(choices.contains(&battlefield_dragon));
        assert!(choices.contains(&hand_dragon));
        assert!(!choices.contains(&opposing_dragon));
        assert!(!choices.contains(&source));
    }

    #[test]
    fn handle_behold_for_cost_reveals_hand_card_without_moving_it() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Piercing Exhale".to_string(),
            Zone::Hand,
        );
        let hand_dragon = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Dragon Card".to_string(),
            Zone::Hand,
        );
        add_subtype(&mut state, hand_dragon, "Dragon");
        let pending = make_pending(source);
        let mut events = Vec::new();

        let result = handle_behold_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &[hand_dragon],
            BeholdCostAction::ChooseOrReveal,
            &[hand_dragon],
            &mut events,
        );

        assert!(result.is_ok());
        assert_eq!(
            state.objects.get(&hand_dragon).map(|obj| obj.zone),
            Some(Zone::Hand)
        );
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::CardsRevealed { card_ids, .. } if card_ids == &vec![hand_dragon]
            )
        }));
    }

    #[test]
    fn handle_behold_for_cost_exiles_selected_permanent_when_required() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Champion of the Path".to_string(),
            Zone::Hand,
        );
        let elemental = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Elemental Permanent".to_string(),
            Zone::Battlefield,
        );
        add_subtype(&mut state, elemental, "Elemental");
        let pending = make_pending(source);
        let mut events = Vec::new();

        let result = handle_behold_for_cost(
            &mut state,
            PlayerId(0),
            pending,
            1,
            &[elemental],
            BeholdCostAction::ExileChosen,
            &[elemental],
            &mut events,
        );

        assert!(result.is_ok());
        assert_eq!(
            state.objects.get(&elemental).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
    }

    #[test]
    fn auto_tap_assigns_flexible_sources_optimally() {
        // Reproduces the Spider Manifestation + Brightglass Gearhulk scenario:
        // Cost {G}{G}{W}{W}, sources: Forest({G}), Spider({R}/{G}),
        // Hushwood({G}/{W}), Air Temple({W}).
        // Greedy approach taps Hushwood for {G} first, leaving no second {W}.
        // MCV/LCV assigns: Forest→{G}, Spider→{G}, Air Temple→{W}, Hushwood→{W}.
        let mut state = GameState::new_two_player(42);

        let forest = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        state
            .objects
            .get_mut(&forest)
            .unwrap()
            .card_types
            .subtypes
            .push("Forest".to_string());

        let spider = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Spider Manifestation".to_string(),
            Zone::Battlefield,
        );
        let spider_obj = state.objects.get_mut(&spider).unwrap();
        spider_obj.card_types.core_types.push(CoreType::Creature);
        spider_obj.entered_battlefield_turn = Some(1);
        Arc::make_mut(&mut spider_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::Red, ManaColor::Green],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let hushwood = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Hushwood Verge".to_string(),
            Zone::Battlefield,
        );
        let hushwood_obj = state.objects.get_mut(&hushwood).unwrap();
        hushwood_obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut hushwood_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Green],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );
        Arc::make_mut(&mut hushwood_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::White],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        let air_temple = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Abandoned Air Temple".to_string(),
            Zone::Battlefield,
        );
        let air_obj = state.objects.get_mut(&air_temple).unwrap();
        air_obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut air_obj.abilities).push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::White],
                        contribution: crate::types::ability::ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Tap),
        );

        state.turn_number = 3;
        let mut events = Vec::new();
        auto_tap_mana_sources(
            &mut state,
            PlayerId(0),
            &ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Green,
                    ManaCostShard::Green,
                    ManaCostShard::White,
                    ManaCostShard::White,
                ],
                generic: 0,
            },
            &mut events,
            None,
        );

        let pool = &state.players[0].mana_pool;
        assert_eq!(
            pool.count_color(ManaType::Green),
            2,
            "should produce 2 green"
        );
        assert_eq!(
            pool.count_color(ManaType::White),
            2,
            "should produce 2 white"
        );
    }

    mod cascade_constraint {
        use super::*;
        use crate::types::ability::{
            CastPermissionConstraint, CastingPermission, Comparator, QuantityExpr,
            ResolutionCastCleanup, ResolutionCastSuccessAction, ResolutionMvRejectAction,
        };
        use crate::types::mana::{ManaCostShard, ManaType, ManaUnit};

        fn exile_card(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
            let card_id = CardId(state.next_object_id);
            create_object(state, card_id, owner, name.to_string(), Zone::Exile)
        }

        fn setup_fixed_mv_cascade_hit(
            source_mv: u32,
            printed_mv: u32,
        ) -> (GameState, ObjectId, Vec<ObjectId>) {
            let mut state = GameState::new_two_player(42);
            let miss_a = exile_card(&mut state, PlayerId(0), "Miss A");
            let miss_b = exile_card(&mut state, PlayerId(0), "Miss B");

            let hit = exile_card(&mut state, PlayerId(0), "Fixed MV Hit");
            let hit_obj = state.objects.get_mut(&hit).unwrap();
            hit_obj.mana_cost = ManaCost::generic(printed_mv);
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LT,
                        value: QuantityExpr::Fixed {
                            value: source_mv as i32,
                        },
                    }),
                    granted_to: None,
                    resolution_cleanup: Some(ResolutionCastCleanup {
                        exiled_misses: vec![miss_a, miss_b],
                        reject_action: ResolutionMvRejectAction::BottomWithMisses,
                        success_action: ResolutionCastSuccessAction::BottomMisses,
                    }),
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });

            (state, hit, vec![miss_a, miss_b])
        }

        fn placeholder_ability(source_id: ObjectId) -> ResolvedAbility {
            ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "test spell".to_string(),
                    description: None,
                },
                Vec::new(),
                source_id,
                PlayerId(0),
            )
        }

        fn push_announcement_stack_entry(state: &mut GameState, object_id: ObjectId) {
            state.stack.push_back(StackEntry {
                id: object_id,
                source_id: object_id,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(0),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
        }

        /// CR 702.85a: A fixed-MV 3 hit with source MV 4 has resulting spell
        /// MV 3, which is strictly less than 4, so the cast is
        /// accepted. Misses bottom-shuffle; the cascade permission is consumed.
        #[test]
        fn accepts_when_resulting_mv_below_source() {
            let (mut state, hit, misses) = setup_fixed_mv_cascade_hit(4, 3);
            let mut events = Vec::new();
            let resulting_mv = state.objects.get(&hit).unwrap().mana_cost.mana_value()
                + state.objects.get(&hit).unwrap().cost_x_paid.unwrap_or(0);
            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                PlayerId(0),
                resulting_mv,
                &mut events,
            );
            assert!(matches!(
                outcome,
                CascadeCheck::Accepted {
                    cast_transformed: false,
                    waiting_for: None
                }
            ));

            let hit_obj = state.objects.get(&hit).unwrap();
            assert!(
                hit_obj.casting_permissions.is_empty(),
                "cascade permission must be consumed on accept"
            );

            for miss in &misses {
                assert_eq!(
                    state.objects.get(miss).map(|o| o.zone),
                    Some(Zone::Library),
                    "misses must be bottom-shuffled on accept"
                );
            }
            assert_eq!(
                state.objects.get(&hit).map(|o| o.zone),
                Some(Zone::Exile),
                "hit card continues through normal cast flow — not bottom-shuffled"
            );
        }

        #[test]
        fn ripple_success_offers_remaining_hit_before_bottoming_misses() {
            let mut state = GameState::new_two_player(42);
            let miss = exile_card(&mut state, PlayerId(0), "Mountain");
            let next_hit = exile_card(&mut state, PlayerId(0), "Surging Flame");
            let hit = exile_card(&mut state, PlayerId(0), "Surging Flame");
            state
                .objects
                .get_mut(&hit)
                .unwrap()
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: Some(ResolutionCastCleanup {
                        exiled_misses: vec![miss],
                        reject_action: ResolutionMvRejectAction::BottomWithMisses,
                        success_action: ResolutionCastSuccessAction::RippleOfferRemaining {
                            remaining_hits: vec![next_hit],
                        },
                    }),
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });

            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                PlayerId(0),
                2,
                &mut Vec::new(),
            );

            match outcome {
                CascadeCheck::Accepted {
                    waiting_for: Some(waiting_for),
                    ..
                } => match *waiting_for {
                    WaitingFor::CastOffer {
                        player,
                        kind:
                            crate::types::game_state::CastOfferKind::Ripple {
                                hit_card,
                                remaining_hits,
                                revealed_misses,
                            },
                    } => {
                        assert_eq!(player, PlayerId(0));
                        assert_eq!(hit_card, next_hit);
                        assert!(remaining_hits.is_empty());
                        assert_eq!(revealed_misses, vec![miss]);
                    }
                    other => panic!("expected follow-up Ripple offer, got {other:?}"),
                },
                other => panic!("expected accepted Ripple cleanup, got {other:?}"),
            }
            assert_eq!(state.objects.get(&miss).map(|o| o.zone), Some(Zone::Exile));
            assert_eq!(
                state.objects.get(&next_hit).map(|o| o.zone),
                Some(Zone::Exile)
            );
        }

        #[test]
        fn ripple_success_bottoms_misses_after_last_hit() {
            let mut state = GameState::new_two_player(42);
            let miss = exile_card(&mut state, PlayerId(0), "Mountain");
            let hit = exile_card(&mut state, PlayerId(0), "Surging Flame");
            state
                .objects
                .get_mut(&hit)
                .unwrap()
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: Some(ResolutionCastCleanup {
                        exiled_misses: vec![miss],
                        reject_action: ResolutionMvRejectAction::BottomWithMisses,
                        success_action: ResolutionCastSuccessAction::RippleOfferRemaining {
                            remaining_hits: vec![],
                        },
                    }),
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });

            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                PlayerId(0),
                2,
                &mut Vec::new(),
            );

            assert!(matches!(
                outcome,
                CascadeCheck::Accepted {
                    waiting_for: None,
                    ..
                }
            ));
            assert_eq!(
                state.objects.get(&miss).map(|o| o.zone),
                Some(Zone::Library)
            );
        }

        #[test]
        fn accepted_cascade_is_not_vetoed_by_stale_mana_value_permission() {
            let (mut state, hit, _misses) = setup_fixed_mv_cascade_hit(4, 3);
            state
                .objects
                .get_mut(&hit)
                .unwrap()
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 2 },
                    }),
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            push_announcement_stack_entry(&mut state, hit);

            let waiting = finalize_cast_with_phyrexian_choices(
                &mut state,
                PlayerId(0),
                hit,
                CardId(0),
                placeholder_ability(hit),
                &ManaCost::zero(),
                CastingVariant::Normal,
                None,
                Zone::Exile,
                None,
                &mut Vec::new(),
            )
            .expect("accepted cascade permission must authorize the finalized cast");

            assert_eq!(
                waiting,
                WaitingFor::Priority {
                    player: PlayerId(0)
                }
            );
            assert!(state.stack.iter().any(|entry| entry.id == hit));
        }

        #[test]
        fn final_validation_rejects_permission_with_different_selected_cost() {
            let mut state = GameState::new_two_player(42);
            let hit = exile_card(&mut state, PlayerId(0), "Mixed Permission Hit");
            let hit_obj = state.objects.get_mut(&hit).unwrap();
            hit_obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
            let selected_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: selected_cost.clone(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }),
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::generic(5),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            push_announcement_stack_entry(&mut state, hit);

            let mut ability = placeholder_ability(hit);
            ability.chosen_x = Some(5);
            let result = finalize_cast_with_phyrexian_choices(
                &mut state,
                PlayerId(0),
                hit,
                CardId(0),
                ability,
                &selected_cost,
                CastingVariant::Normal,
                None,
                Zone::Exile,
                None,
                &mut Vec::new(),
            );

            assert!(
                result.is_err(),
                "a different permission must not authorize the already-selected X-cost path"
            );
            assert!(
                !state.stack.iter().any(|entry| entry.id == hit),
                "failed final validation must unwind the announcement stack entry"
            );
        }

        #[test]
        fn final_validation_accepts_free_alt_cost_after_cost_increase() {
            let mut state = GameState::new_two_player(42);
            let hit = exile_card(&mut state, PlayerId(0), "Taxed Free Permission Hit");
            let hit_obj = state.objects.get_mut(&hit).unwrap();
            hit_obj.mana_cost = ManaCost::generic(5);
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(99),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
            push_announcement_stack_entry(&mut state, hit);

            let waiting = finalize_cast_with_phyrexian_choices(
                &mut state,
                PlayerId(0),
                hit,
                CardId(0),
                placeholder_ability(hit),
                &ManaCost::generic(1),
                CastingVariant::Normal,
                None,
                Zone::Exile,
                None,
                &mut Vec::new(),
            )
            .expect("selected free permission must survive later cost increases");

            assert_eq!(
                waiting,
                WaitingFor::Priority {
                    player: PlayerId(0)
                }
            );
            assert_eq!(state.players[0].mana_pool.total(), 0);
            assert!(state.stack.iter().any(|entry| entry.id == hit));
        }

        #[test]
        fn later_cascade_permission_cannot_authorize_selected_failing_permission() {
            let mut state = GameState::new_two_player(42);
            let miss = exile_card(&mut state, PlayerId(0), "Miss");
            let hit = exile_card(&mut state, PlayerId(0), "Selected Permission Hit");
            let hit_obj = state.objects.get_mut(&hit).unwrap();
            hit_obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
            let selected_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            };
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: selected_cost.clone(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LE,
                        value: QuantityExpr::Fixed { value: 4 },
                    }),
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LT,
                        value: QuantityExpr::Fixed { value: 10 },
                    }),
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: Some(ResolutionCastCleanup {
                        exiled_misses: vec![miss],
                        reject_action: ResolutionMvRejectAction::BottomWithMisses,
                        success_action: ResolutionCastSuccessAction::BottomMisses,
                    }),
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            push_announcement_stack_entry(&mut state, hit);

            let mut ability = placeholder_ability(hit);
            ability.chosen_x = Some(5);
            let result = finalize_cast_with_phyrexian_choices(
                &mut state,
                PlayerId(0),
                hit,
                CardId(0),
                ability,
                &selected_cost,
                CastingVariant::Normal,
                None,
                Zone::Exile,
                None,
                &mut Vec::new(),
            );

            assert!(
                result.is_err(),
                "later cascade permission must not bypass the selected permission's MV check"
            );
            assert!(
                !state.stack.iter().any(|entry| entry.id == hit),
                "failed final validation must unwind the announcement stack entry"
            );
        }

        #[test]
        fn wrong_player_cascade_permission_does_not_reject_selected_permission() {
            let mut state = GameState::new_two_player(42);
            let miss = exile_card(&mut state, PlayerId(1), "Opponent Miss");
            let hit = exile_card(&mut state, PlayerId(0), "Authorized Hit");
            let hit_obj = state.objects.get_mut(&hit).unwrap();
            hit_obj.mana_cost = ManaCost::generic(5);
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: Some(CastPermissionConstraint::ManaValue {
                        comparator: Comparator::LT,
                        value: QuantityExpr::Fixed { value: 1 },
                    }),
                    granted_to: Some(PlayerId(1)),
                    resolution_cleanup: Some(ResolutionCastCleanup {
                        exiled_misses: vec![miss],
                        reject_action: ResolutionMvRejectAction::BottomWithMisses,
                        success_action: ResolutionCastSuccessAction::BottomMisses,
                    }),
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            hit_obj
                .casting_permissions
                .push(CastingPermission::ExileWithAltCost {
                    cost: ManaCost::zero(),
                    cast_transformed: false,
                    constraint: None,
                    granted_to: Some(PlayerId(0)),
                    resolution_cleanup: None,
                    duration: None,

                    exile_instead_of_graveyard_on_resolve: false,
                });
            push_announcement_stack_entry(&mut state, hit);

            let waiting = finalize_cast_with_phyrexian_choices(
                &mut state,
                PlayerId(0),
                hit,
                CardId(0),
                placeholder_ability(hit),
                &ManaCost::zero(),
                CastingVariant::Normal,
                None,
                Zone::Exile,
                None,
                &mut Vec::new(),
            )
            .expect("wrong-player cascade permission must be ignored");

            assert_eq!(
                waiting,
                WaitingFor::Priority {
                    player: PlayerId(0)
                }
            );
            assert!(state.stack.iter().any(|entry| entry.id == hit));
        }

        /// CR 702.85a: A cascade hit whose PRINTED MV (2) is below source MV (4)
        /// — a legal offer — but whose RESULTING MV reaches 4 (e.g. X chosen so
        /// printed 2 + X 2 = 4) is NOT strictly less than 4, so the cast is
        /// rejected. The permission is still consumed, and the returned misses
        /// match the original set for the caller to bottom-shuffle with the hit.
        #[test]
        fn rejects_when_resulting_mv_equals_source() {
            // Printed MV 2 (< source 4) so the permission is a valid offer at
            // offer time; the resulting MV of 4 is the post-X value the gate
            // rejects (4 is not < 4).
            let (mut state, hit, misses) = setup_fixed_mv_cascade_hit(4, 2);
            let mut events = Vec::new();
            let resulting_mv = 4;
            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                PlayerId(0),
                resulting_mv,
                &mut events,
            );
            match outcome {
                CascadeCheck::Rejected { exiled_misses, .. } => {
                    assert_eq!(exiled_misses, misses);
                }
                other => panic!("Expected Rejected, got {:?}", matches_name(&other)),
            }

            let hit_obj = state.objects.get(&hit).unwrap();
            assert!(
                hit_obj.casting_permissions.is_empty(),
                "cascade permission must be consumed on reject too"
            );

            for miss in &misses {
                assert_eq!(
                    state.objects.get(miss).map(|o| o.zone),
                    Some(Zone::Exile),
                    "misses stay put until handle_cascade_rejection runs"
                );
            }
        }

        /// CR 702.85a: A cascade hit whose PRINTED MV (3) is below source MV (4)
        /// — a legal offer — but whose RESULTING MV (5, after X) exceeds source,
        /// is rejected. Confirms strict inequality is enforced above the
        /// equality boundary as well.
        #[test]
        fn rejects_when_resulting_mv_above_source() {
            let (mut state, hit, _misses) = setup_fixed_mv_cascade_hit(4, 3);
            let mut events = Vec::new();
            let resulting_mv = 5;
            let outcome = evaluate_cascade_constraint_with_resulting_mv(
                &mut state,
                hit,
                PlayerId(0),
                resulting_mv,
                &mut events,
            );
            assert!(matches!(outcome, CascadeCheck::Rejected { .. }));
        }

        /// CR 702.85a + CR 601.2a: The rejection handler pops the
        /// announcement-time stack entry, bottom-shuffles misses + the hit in
        /// random order, and returns priority to the caster.
        #[test]
        fn rejection_handler_pops_stack_and_bottom_shuffles_all() {
            let (mut state, hit, misses) = setup_fixed_mv_cascade_hit(4, 4);

            state.stack.push_back(StackEntry {
                id: hit,
                source_id: hit,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(0),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            });
            let stack_depth_before = state.stack.len();

            let mut events = Vec::new();
            let waiting_for = handle_resolution_cast_rejection(
                &mut state,
                PlayerId(0),
                hit,
                misses.clone(),
                ResolutionMvRejectAction::BottomWithMisses,
                &mut events,
            )
            .expect("rejection handler must succeed");

            assert_eq!(
                state.stack.len(),
                stack_depth_before - 1,
                "announcement stack entry must be popped"
            );
            assert!(
                !state.stack.iter().any(|e| e.id == hit),
                "no stack entry for the rejected cast may remain"
            );

            for id in misses.iter().chain(std::iter::once(&hit)) {
                assert_eq!(
                    state.objects.get(id).map(|o| o.zone),
                    Some(Zone::Library),
                    "misses and hit must bottom-shuffle together on rejection"
                );
            }

            match waiting_for {
                WaitingFor::Priority { player } => assert_eq!(player, PlayerId(0)),
                other => panic!("Expected Priority for caster, got {:?}", other),
            }
        }

        fn matches_name(check: &CascadeCheck) -> &'static str {
            match check {
                CascadeCheck::NotApplicable => "NotApplicable",
                CascadeCheck::Accepted { .. } => "Accepted",
                CascadeCheck::Rejected { .. } => "Rejected",
            }
        }
    }

    /// CR 601.2b + CR 601.2h: `AbilityCost::Exile { zone: Some(Hand), filter }`
    /// must surface as a `WaitingFor::ExileForCost { zone: Hand, .. }` carrying
    /// only filter-matching cards from the caster's hand, with the cast source
    /// itself excluded. Building-block-level test — covers every pitch spell
    /// (Force of Will, Force of Negation, Force of Vigor, Misdirection,
    /// Unmask, Mindbreak Trap, …), not just one card.
    #[test]
    fn exile_from_hand_for_cost_filters_eligible_hand_cards() {
        use crate::game::zones::create_object;
        use crate::types::ability::{FilterProp, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        // Cast source — the spell being cast (must be excluded from eligibility).
        let source_id = create_object(
            &mut state,
            CardId(900),
            caster,
            "Pitch Source".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Blue);
        }

        // Eligible: blue card in hand.
        let blue_card = create_object(
            &mut state,
            CardId(901),
            caster,
            "Blue Filler".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&blue_card).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Blue);
        }

        // Ineligible: non-blue card in hand.
        let red_card = create_object(
            &mut state,
            CardId(902),
            caster,
            "Red Filler".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&red_card).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Red);
        }

        let mut events = Vec::new();
        let pending = PendingCast {
            object_id: source_id,
            card_id: CardId(900),
            ability: ResolvedAbility::new(
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                },
                Vec::new(),
                source_id,
                caster,
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            x_residual_activation: false,
        };

        let result = pay_additional_cost(
            &mut state,
            caster,
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: Some(crate::types::ability::ControllerRef::You),
                    properties: vec![FilterProp::HasColor {
                        color: ManaColor::Blue,
                    }],
                })),
            },
            pending,
            &mut events,
        )
        .expect("pitch cost should produce ExileForCost");

        match result {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromZone { zone },
                choices: cards,
                count,
                ..
            } => {
                assert_eq!(player, caster);
                assert_eq!(zone, ExileCostSourceZone::Hand);
                assert_eq!(count, 1);
                assert!(
                    cards.contains(&blue_card),
                    "blue hand card must be eligible: {cards:?}"
                );
                assert!(
                    !cards.contains(&red_card),
                    "non-blue hand card must be filtered out: {cards:?}"
                );
                assert!(
                    !cards.contains(&source_id),
                    "cast source itself must never be eligible: {cards:?}"
                );
            }
            other => panic!("expected PayCost ExileFromZone, got {other:?}"),
        }
    }

    /// CR 601.2b: When the hand has fewer eligible cards than the cost
    /// requires, the cost is unpayable and casting must fail rather than
    /// surfacing a dead `WaitingFor`.
    #[test]
    fn exile_from_hand_for_cost_rejects_when_insufficient_eligible_cards() {
        use crate::game::zones::create_object;
        use crate::types::ability::{FilterProp, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let source_id = create_object(
            &mut state,
            CardId(900),
            caster,
            "Pitch Source".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Blue);
        }

        // Only ineligible (non-blue) cards available.
        let red_card = create_object(
            &mut state,
            CardId(902),
            caster,
            "Red Filler".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&red_card).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Red);
        }

        let pending = PendingCast {
            object_id: source_id,
            card_id: CardId(900),
            ability: ResolvedAbility::new(
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                },
                Vec::new(),
                source_id,
                caster,
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            x_residual_activation: false,
        };

        let mut events = Vec::new();
        let result = pay_additional_cost(
            &mut state,
            caster,
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Hand),
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: Some(crate::types::ability::ControllerRef::You),
                    properties: vec![FilterProp::HasColor {
                        color: ManaColor::Blue,
                    }],
                })),
            },
            pending,
            &mut events,
        );

        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "unpayable pitch cost must fail: {result:?}"
        );
    }

    /// CR 601.2b + CR 601.2h: `handle_exile_for_cost` must reject a selection
    /// whose length differs from the required count and an attempt to exile a
    /// card that is not in the legal-cards list. These guards keep the pitch
    /// flow from accepting illegal payments.
    #[test]
    fn handle_exile_for_cost_rejects_wrong_count() {
        use crate::game::zones::create_object;

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let source_id = create_object(
            &mut state,
            CardId(900),
            caster,
            "Pitch Source".to_string(),
            Zone::Hand,
        );
        let blue_a = create_object(
            &mut state,
            CardId(901),
            caster,
            "Blue A".to_string(),
            Zone::Hand,
        );
        let blue_b = create_object(
            &mut state,
            CardId(902),
            caster,
            "Blue B".to_string(),
            Zone::Hand,
        );
        let pending = PendingCast {
            object_id: source_id,
            card_id: CardId(900),
            ability: ResolvedAbility::new(
                Effect::Counter {
                    target: crate::types::ability::TargetFilter::Any,
                    source_rider: None,
                },
                Vec::new(),
                source_id,
                caster,
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            x_residual_activation: false,
        };

        // Exactly one card is required. Selecting two must fail.
        let mut events = Vec::new();
        let result = handle_exile_for_cost(
            &mut state,
            caster,
            ExileCostSourceZone::Hand,
            pending.clone(),
            1,
            &[blue_a, blue_b],
            &[blue_a, blue_b],
            &mut events,
        );
        assert!(
            matches!(result, Err(EngineError::InvalidAction(_))),
            "wrong count must be rejected: {result:?}"
        );
    }

    #[test]
    fn handle_exile_for_cost_rejects_illegal_selection() {
        use crate::game::zones::create_object;

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let source_id = create_object(
            &mut state,
            CardId(900),
            caster,
            "Pitch Source".to_string(),
            Zone::Hand,
        );
        let blue = create_object(
            &mut state,
            CardId(901),
            caster,
            "Blue Legal".to_string(),
            Zone::Hand,
        );
        let red = create_object(
            &mut state,
            CardId(902),
            caster,
            "Red Illegal".to_string(),
            Zone::Hand,
        );
        let pending = PendingCast {
            object_id: source_id,
            card_id: CardId(900),
            ability: ResolvedAbility::new(
                Effect::Counter {
                    target: crate::types::ability::TargetFilter::Any,
                    source_rider: None,
                },
                Vec::new(),
                source_id,
                caster,
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            x_residual_activation: false,
        };

        // `red` is not in the legal-cards list, so the cost handler must reject
        // it even though it is in hand and the count matches.
        let mut events = Vec::new();
        let result = handle_exile_for_cost(
            &mut state,
            caster,
            ExileCostSourceZone::Hand,
            pending,
            1,
            &[blue],
            &[red],
            &mut events,
        );
        assert!(
            matches!(result, Err(EngineError::InvalidAction(_))),
            "card not in legal list must be rejected: {result:?}"
        );
    }

    /// CR 601.2b + CR 601.2h + CR 702.138a: The eligibility helper for an
    /// `AbilityCost::Exile` payment must apply the cost's `TargetFilter` in
    /// the graveyard branch — not just the hand branch. Escape today carries
    /// no filter, but any future graveyard-source exile cost with a filter
    /// would otherwise silently no-op. Building-block-level test exercising
    /// the filter against a heterogeneous graveyard.
    #[test]
    fn exile_for_cost_graveyard_applies_filter() {
        use crate::game::zones::create_object;
        use crate::types::ability::{FilterProp, TargetFilter, TypeFilter, TypedFilter};
        use crate::types::card_type::CoreType;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        // Cast source — not in graveyard, but its ID must still be excluded.
        let source_id = create_object(
            &mut state,
            CardId(900),
            caster,
            "Escape Source".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Blue);
        }

        // Eligible: blue card in graveyard.
        let blue_card = create_object(
            &mut state,
            CardId(901),
            caster,
            "Blue Filler".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&blue_card).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Blue);
        }

        // Ineligible: non-blue card in graveyard.
        let red_card = create_object(
            &mut state,
            CardId(902),
            caster,
            "Red Filler".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&red_card).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.color.push(ManaColor::Red);
        }

        let mut events = Vec::new();
        let pending = PendingCast {
            object_id: source_id,
            card_id: CardId(900),
            ability: ResolvedAbility::new(
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                },
                Vec::new(),
                source_id,
                caster,
            ),
            cost: crate::types::mana::ManaCost::NoCost,
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Graveyard,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            x_residual_activation: false,
        };

        let result = pay_additional_cost(
            &mut state,
            caster,
            AbilityCost::Exile {
                count: 1,
                zone: Some(Zone::Graveyard),
                filter: Some(TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Card],
                    controller: Some(crate::types::ability::ControllerRef::You),
                    properties: vec![FilterProp::HasColor {
                        color: ManaColor::Blue,
                    }],
                })),
            },
            pending,
            &mut events,
        )
        .expect("graveyard exile cost should produce ExileForCost");

        match result {
            WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromZone { zone },
                choices: cards,
                count,
                ..
            } => {
                assert_eq!(player, caster);
                assert_eq!(zone, ExileCostSourceZone::Graveyard);
                assert_eq!(count, 1);
                assert!(
                    cards.contains(&blue_card),
                    "blue graveyard card must be eligible: {cards:?}"
                );
                assert!(
                    !cards.contains(&red_card),
                    "non-blue graveyard card must be filtered out: {cards:?}"
                );
                assert!(
                    !cards.contains(&source_id),
                    "cast source itself must never be eligible: {cards:?}"
                );
            }
            other => panic!("expected PayCost ExileFromZone, got {other:?}"),
        }
    }

    // ── max_x_value tests ──────────────────────────────────────────────

    #[test]
    fn max_x_value_counts_treasure_tokens() {
        // CR 107.1b + CR 601.2f: X is chosen before mana payment.
        // Treasure tokens (sacrifice-for-mana) must be counted so the player
        // can choose an X that includes them as potential mana sources.
        use crate::types::ability::{ManaContribution, ManaProduction, TargetFilter};

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Create 3 basic lands (free mana sources) with tap-for-green abilities.
        for i in 0..3 {
            let land = create_object(
                &mut state,
                CardId(100 + i),
                player,
                "Forest".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Green],
                            contribution: ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        // Create 2 Treasure tokens (sacrifice-for-mana sources).
        for i in 0..2 {
            let treasure = create_object(
                &mut state,
                CardId(200 + i),
                player,
                "Treasure".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&treasure).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Treasure".to_string());

            let ability = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![
                            ManaColor::White,
                            ManaColor::Blue,
                            ManaColor::Black,
                            ManaColor::Red,
                            ManaColor::Green,
                        ],
                        contribution: ManaContribution::Base,
                    },
                    restrictions: vec![],
                    grants: vec![],
                    expiry: None,
                    target: None,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            });
            let obj = state.objects.get_mut(&treasure).unwrap();
            Arc::make_mut(&mut obj.abilities).push(ability);
        }

        // Cost: {X}{R} — 1 fixed colored shard, rest is X.
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        };

        // 3 lands + 2 Treasures = 5 sources, minus 1 for the {R} = max X of 4.
        let max = max_x_value(&state, player, &cost, None);
        assert_eq!(max, 4, "max X should count Treasure tokens as mana sources");
    }

    /// Issue #562: Krark-Clan Ironworks (`Sacrifice an artifact: Add {C}{C}`)
    /// is a non-tap mana ability — the cost is bare `Sacrifice`, not the
    /// `Composite { Tap, Sacrifice }` shape Treasure tokens use. Before this
    /// fix, `max_x_value` called `max_mana_yield`, which gates on
    /// `has_tap_component` and therefore reported 0 for KCI. The X chooser
    /// understated affordable X for X-spells that KCI could manually fund.
    ///
    /// With the routing change to `feasible_mana_capacity`, KCI's 2-mana yield
    /// per activation is counted up to the sacrifice supply.
    ///
    // CR 107.1b + CR 117.1d + CR 605.3a: Mana abilities (including non-tap-
    // cost ones) may be activated during cost payment, so the affordable X
    // cap must include their feasible yield.
    #[test]
    fn max_x_value_counts_kci_non_tap_sacrifice_mana_ability() {
        use crate::types::ability::{ManaProduction, TargetFilter, TypeFilter, TypedFilter};

        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // 1 Mountain — the only `{T}`-cost producer, supplies the fixed {R}.
        let mountain = create_object(
            &mut state,
            CardId(900),
            player,
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&mountain).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Fixed {
                            colors: vec![ManaColor::Red],
                            contribution: crate::types::ability::ManaContribution::Base,
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        // KCI — non-tap, bare `Sacrifice an artifact: Add {C}{C}`.
        let kci = create_object(
            &mut state,
            CardId(901),
            player,
            "Krark-Clan Ironworks".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&kci).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            Arc::make_mut(&mut obj.abilities).push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: ManaProduction::Colorless {
                            count: QuantityExpr::Fixed { value: 2 },
                        },
                        restrictions: vec![],
                        grants: vec![],
                        expiry: None,
                        target: None,
                    },
                )
                .cost(AbilityCost::Sacrifice(SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                    1,
                ))),
            );
        }

        // Three sacrificable artifact creatures so KCI's sacrifice supply
        // is non-empty.
        for i in 0..3 {
            let sac = create_object(
                &mut state,
                CardId(902 + i),
                player,
                format!("Sacrificial Artifact {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&sac).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.core_types.push(CoreType::Creature);
        }

        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red],
            generic: 0,
        };

        // Without the fix, `max_mana_yield` would return 0 for KCI (no `{T}`
        // cost component) and the cap would be 0 (1 Mountain − 1 fixed {R}
        // shard). With the fix, KCI's `feasible_mana_capacity` returns 2.
        //
        // Arithmetic (deterministic):
        //   - Mountain: feasible_mana_capacity = 1 ({R} via `{T}`)
        //   - KCI:      feasible_mana_capacity = 2 ({C}{C} via one Sacrifice)
        //   - 3 fodder: feasible_mana_capacity = 0 each (no mana abilities)
        //   - pool = 0, fixed_portion = 1 (the {R})
        //   - capacity = 1 + 2 = 3, remaining = 3 − 1 = 2
        //   - x_count = 1, so max X = 2 / 1 = 2.
        //
        // The tight `assert_eq!(max, 2)` is a falsifiable expectation in
        // both directions: an *under-count* regression (the original #562
        // bug) would report max == 0, and an *over-count* regression
        // (e.g. counting fodder or chain-sacrificing the same mana source
        // twice) would report max >= 3.
        let max = max_x_value(&state, player, &cost, None);
        assert_eq!(
            max, 2,
            "Issue #562: KCI's non-tap mana ability must be counted by max_x_value. \
             Expected exactly 2 (1 Mountain + 2 KCI − 1 fixed {{R}}), got {max}",
        );
    }

    /// CR 702.51a + CR 601.2b: `max_x_value` must count Convoke-eligible
    /// creatures as potential tap-payments so an X-spell with convoke gets a
    /// raised cap. Untapped creatures the caster controls can pay generic mana.
    #[test]
    fn max_x_value_counts_convoke_creatures() {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        // 2 Islands (real mana producers) + 3 untapped creatures.
        scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        for _ in 0..3 {
            scenario.add_vanilla(PlayerId(0), 1, 1);
        }
        // Convoke X-spell `{X}{U}` in hand.
        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Convoke X-Spell", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        });
        let spell_id = builder.id();
        builder.with_keyword(Keyword::Convoke);

        let runner = scenario.build();
        let state = runner.state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        };

        // Without the spell context (no tap capacity): 2 Islands − {U} = 1.
        assert_eq!(max_x_value(state, PlayerId(0), &cost, None), 1);
        // With the spell context: +3 convoke creatures raises the cap to 4.
        assert_eq!(
            max_x_value(state, PlayerId(0), &cost, Some(spell_id)),
            4,
            "convoke creatures must raise the X cap"
        );
    }

    /// Issue #490 discriminator: Whir of Invention `{X}{U}{U}{U}` (Improvise)
    /// with 3 Islands + 3 artifacts. Pre-fix, `max_x_value` ignored improvise
    /// tap capacity, so the X chooser was capped at 0 (producible 3 − fixed 3).
    /// CR 702.126a: artifacts can pay the generic portion (the {X}), so X=3
    /// must be choosable. With Step 4 reverted this test FAILS (`max == 0`).
    #[test]
    fn whir_of_invention_improvise_allows_full_x() {
        use crate::game::scenario::GameScenario;
        use crate::types::GameAction;

        const WHIR_ORACLE: &str = "Improvise (Your artifacts can help cast this spell. \
Each artifact you tap after you're done activating mana abilities pays for {1}.)\n\
Search your library for an artifact card with mana value X or less, put it onto the \
battlefield, then shuffle.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        // 3 untapped Islands — the only real mana producers.
        let islands: Vec<ObjectId> = (0..3)
            .map(|_| scenario.add_basic_land(PlayerId(0), ManaColor::Blue))
            .collect();
        // 3 untapped artifacts — improvise-eligible tap-payers.
        let artifacts: Vec<ObjectId> = (0..3)
            .map(|i| {
                let mut b = scenario.add_creature(PlayerId(0), &format!("Artifact {i}"), 0, 0);
                b.as_artifact();
                b.id()
            })
            .collect();

        // Whir of Invention `{X}{U}{U}{U}` with Improvise, parsed from Oracle.
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            PlayerId(0),
            "Whir of Invention",
            true,
            WHIR_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![
                ManaCostShard::X,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
                ManaCostShard::Blue,
            ],
            generic: 0,
        });
        // Re-run synthesis with an explicit keyword hint so the
        // "Improvise (reminder text)" line is recognized as a keyword line.
        builder.from_oracle_text_with_keywords(&["Improvise"], WHIR_ORACLE);
        let spell_id = builder.id();

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell_id].card_id;
        assert!(
            runner.state().objects[&spell_id]
                .keywords
                .contains(&Keyword::Improvise),
            "Whir must parse with the Improvise keyword"
        );

        // Cast Whir — cost has X, so the engine enters ChooseXValue.
        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting Whir of Invention must be accepted");

        // THE DISCRIMINATOR: with 3 Islands (producible 3) and a fixed portion
        // of {U}{U}{U} (3), pre-fix `max` was 0. Improvise's 3 artifacts must
        // raise it to 3.
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue {
                max, convoke_mode, ..
            } => {
                assert_eq!(
                    convoke_mode,
                    Some(ConvokeMode::Improvise),
                    "Whir's keyword must be detected as Improvise"
                );
                assert_eq!(
                    max, 3,
                    "improvise artifacts must raise max X to 3 (pre-fix: 0)"
                );
            }
            other => panic!("expected ChooseXValue, got {other:?}"),
        }

        // Choose X = 3.
        runner
            .act(GameAction::ChooseX { value: 3 })
            .expect("choosing X=3 must be accepted");

        // Pay the {U}{U}{U} with the 3 Islands.
        for &island in &islands {
            runner
                .act(GameAction::ActivateAbility {
                    source_id: island,
                    ability_index: 0,
                })
                .expect("tapping an Island for {U} must be accepted");
        }
        // Pay the {3} generic by tapping the 3 artifacts via improvise.
        for &artifact in &artifacts {
            runner
                .act(GameAction::TapForConvoke {
                    object_id: artifact,
                    mana_type: ManaType::Colorless,
                })
                .expect("tapping an artifact for improvise must be accepted");
        }

        // Finalize payment.
        runner
            .act(GameAction::PassPriority)
            .expect("finalizing payment must be accepted");

        // Whir is on the stack; the 3 artifacts are tapped.
        assert_eq!(runner.state().stack.len(), 1, "Whir must be on the stack");
        for &artifact in &artifacts {
            assert!(
                runner.state().objects[&artifact].tapped,
                "improvise-tapped artifact must be tapped"
            );
        }
    }

    /// Build a `{T}: Add <count> colorless` activated mana ability — the shape
    /// of a mana-dork (`{T}: Add {G}`) or a mana-rock (`{T}: Add {C}{C}`).
    fn tap_mana_ability(count: i32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: crate::types::ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: count },
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    /// Issue #490 follow-up — reverted-fix discriminator (Convoke + mana-dorks).
    /// CR 110.5 + CR 110.5c + CR 702.51a: a creature tapped for Convoke cannot
    /// also be tapped for its mana ability. With 2 Islands + 3 mana-dorks
    /// (`{T}: Add {C}`), each dork is a single tap unit — `max(mana 1, tap 1)`.
    /// True max X for a Convoke `{X}{U}` = 2 Islands + 3 dorks − {U} = 4.
    /// Pre-fix the producible term (5) and the tap_capacity term (3) were summed
    /// → `(0 + 5 + 3) - 1 = 7`, an unpayable X. With the partition fix it is 4.
    #[test]
    fn max_x_value_convoke_does_not_double_count_mana_dorks() {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        // 2 Islands — pure mana sources, not Convoke-eligible.
        scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        // 3 mana-dorks — creatures (Convoke-eligible) that also produce mana.
        for i in 0..3 {
            let mut b = scenario.add_creature(PlayerId(0), &format!("Mana Dork {i}"), 1, 1);
            b.with_ability_definition(tap_mana_ability(1));
        }

        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Convoke X-Spell", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        });
        let spell_id = builder.id();
        builder.with_keyword(Keyword::Convoke);

        let runner = scenario.build();
        let state = runner.state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        };

        assert_eq!(
            max_x_value(state, PlayerId(0), &cost, Some(spell_id)),
            4,
            "Convoke must not double-count mana-dorks (pre-fix: 7)"
        );
    }

    /// Issue #490 follow-up — reverted-fix discriminator (Improvise + mana-rock).
    /// CR 702.126a: an artifact tapped for Improvise cannot also be tapped for
    /// its mana ability. Board: 1 Island + 1 Sol-Ring-like artifact
    /// (`{T}: Add {C}{C}`). For an Improvise `{X}`, the artifact is a single tap
    /// unit → `max(mana 2, improvise 1) = 2`; Island contributes 1.
    /// True max X = 3. Pre-fix: producible (1 + 2 = 3) + tap_capacity (1) = 4.
    #[test]
    fn max_x_value_improvise_does_not_double_count_mana_rocks() {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        // Sol-Ring-like artifact: untapped, Improvise-eligible, `{T}: Add {C}{C}`.
        let mut rock = scenario.add_creature(PlayerId(0), "Sol Ring", 0, 0);
        rock.as_artifact();
        rock.with_ability_definition(tap_mana_ability(2));

        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Improvise X-Spell", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        });
        let spell_id = builder.id();
        builder.with_keyword(Keyword::Improvise);

        let runner = scenario.build();
        let state = runner.state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        };

        assert_eq!(
            max_x_value(state, PlayerId(0), &cost, Some(spell_id)),
            3,
            "Improvise must not double-count a mana-rock (pre-fix: 4)"
        );
    }

    /// Issue #490 follow-up — Waterbend overlap. Waterbend is eligible on
    /// artifacts OR creatures, so a mana-rock satisfies both the mana and the
    /// tap-keyword channels. Board: 1 Island + 1 artifact (`{T}: Add {C}{C}`).
    /// Waterbend `{X}` → artifact is one tap unit (`max(2, 1) = 2`),
    /// Island 1 → max X = 3. Proves the partition is keyword-general.
    #[test]
    fn max_x_value_waterbend_does_not_double_count() {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        let mut rock = scenario.add_creature(PlayerId(0), "Waterbend Rock", 0, 0);
        rock.as_artifact();
        rock.with_ability_definition(tap_mana_ability(2));

        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Waterbend X-Spell", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        });
        let spell_id = builder.id();
        builder.with_keyword(Keyword::Waterbend);

        let runner = scenario.build();
        let state = runner.state();
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        };

        assert_eq!(
            max_x_value(state, PlayerId(0), &cost, Some(spell_id)),
            3,
            "Waterbend must not double-count an overlapping mana-rock"
        );
    }

    /// Issue #490 follow-up — runtime end-to-end. The X chooser's offered `max`
    /// for a Convoke X-spell cast alongside mana-dorks must be fully payable
    /// through the pipeline. Mirrors `whir_of_invention_improvise_allows_full_x`
    /// but with a board where the mana/tap overlap exists. CR 601.2f: X is
    /// announced before payment, so the offered cap must be honest.
    #[test]
    fn convoke_x_spell_offers_payable_x_with_mana_dork_overlap() {
        use crate::game::scenario::GameScenario;
        use crate::types::GameAction;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        // 1 Island + 2 mana-dorks (creatures with `{T}: Add {C}`).
        let island = scenario.add_basic_land(PlayerId(0), ManaColor::Blue);
        let dorks: Vec<ObjectId> = (0..2)
            .map(|i| {
                let mut b = scenario.add_creature(PlayerId(0), &format!("Mana Dork {i}"), 1, 1);
                b.with_ability_definition(tap_mana_ability(1));
                b.id()
            })
            .collect();

        // Convoke X-spell `{X}{U}` — no overlap means max X would be 2;
        // the partition keeps it at 2 (Island + 2 dorks − {U}).
        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Convoke X-Spell", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Blue],
            generic: 0,
        });
        let spell_id = builder.id();
        builder.with_keyword(Keyword::Convoke);

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell_id].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the Convoke X-spell must be accepted");

        let offered_max = match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { max, .. } => max,
            other => panic!("expected ChooseXValue, got {other:?}"),
        };
        assert_eq!(
            offered_max, 2,
            "partitioned cap: Island + 2 dorks − {{U}} = 2 (pre-fix: 3)"
        );

        runner
            .act(GameAction::ChooseX { value: offered_max })
            .expect("choosing the offered max X must be accepted");

        // Pay the {U} with the Island.
        runner
            .act(GameAction::ActivateAbility {
                source_id: island,
                ability_index: 0,
            })
            .expect("tapping the Island for {U} must be accepted");
        // Pay the {2} generic by Convoke-tapping the 2 dorks.
        for &dork in &dorks {
            runner
                .act(GameAction::TapForConvoke {
                    object_id: dork,
                    mana_type: ManaType::Colorless,
                })
                .expect("Convoke-tapping a mana-dork must be accepted");
        }
        runner
            .act(GameAction::PassPriority)
            .expect("finalizing payment must be accepted");

        assert_eq!(
            runner.state().stack.len(),
            1,
            "the Convoke X-spell must be on the stack — offered max X was payable"
        );
    }

    // -----------------------------------------------------------------------
    // Issue #454: multikicker (Everflowing Chalice) — the repeatable kicker
    // prompt must carry the live `AdditionalCost::Kicker` discriminant (not a
    // laundered `Optional`) and the running kick count, so the frontend can
    // render a kick-count-aware modal. CR 702.33c/d.
    // -----------------------------------------------------------------------

    const EVERFLOWING_CHALICE_ORACLE: &str = "Multikicker {2} (You may pay an additional {2} \
any number of times as you cast this spell.)\nThis artifact enters with a charge counter on \
it for each time it was kicked.\n{T}: Add {C} for each charge counter on this artifact.";

    /// Build an Everflowing Chalice in P0's hand at PreCombatMain, parsed from
    /// its real Oracle text (so the Multikicker additional cost and the
    /// `KickerCount`-driven PutCounter replacement are exactly as shipped).
    fn everflowing_chalice_scenario() -> (crate::game::scenario::GameRunner, ObjectId, CardId) {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        // {0} mana cost — the base cost is free; only the kicker costs mana.
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            PlayerId(0),
            "Everflowing Chalice",
            false,
            EVERFLOWING_CHALICE_ORACLE,
        );
        builder.as_artifact();
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        });
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;

        let runner = scenario.build();
        (runner, spell_id, card_id)
    }

    /// Give P0 `count` colorless mana so the {2}-per-kick total can be paid
    /// without modelling lands (the ManaPayment step auto-completes from pool).
    fn fund_colorless(runner: &mut crate::game::scenario::GameRunner, count: usize) {
        use crate::types::mana::ManaUnit;
        let p0 = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..count {
            p0.mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    fn fund_white(runner: &mut crate::game::scenario::GameRunner, count: usize) {
        use crate::types::mana::ManaUnit;
        let p0 = runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..count {
            p0.mana_pool.add(ManaUnit {
                color: ManaType::White,
                source_id: ObjectId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }
    }

    fn charge_counters(state: &GameState, object_id: ObjectId) -> u32 {
        state
            .objects
            .get(&object_id)
            .and_then(|o| {
                o.counters.get(&crate::types::counter::CounterType::Generic(
                    "charge".to_string(),
                ))
            })
            .copied()
            .unwrap_or(0)
    }

    /// Engine test 1 — multikicker paid twice. The re-prompt must remain a
    /// real `Kicker` (regression guard for the `Optional` laundering bug),
    /// `times_kicked` must round-trip, and the artifact must enter with 2
    /// charge counters (exercises `KickerCount` → PutCounter).
    #[test]
    fn multikicker_paid_twice_enters_with_two_charge_counters() {
        use crate::types::GameAction;
        let (mut runner, spell_id, card_id) = everflowing_chalice_scenario();
        fund_colorless(&mut runner, 4); // {2} + {2} for two kicks

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting Everflowing Chalice must be accepted");

        // First prompt: real Kicker, repeatable, times_kicked == 0.
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost, times_kicked, ..
            } => {
                assert!(
                    matches!(
                        cost,
                        AdditionalCost::Kicker {
                            repeatability:
                                crate::types::ability::AdditionalCostRepeatability::Repeatable,
                            ..
                        }
                    ),
                    "first prompt must be a repeatable Kicker, not laundered Optional: {cost:?}"
                );
                assert_eq!(times_kicked, 0, "first prompt times_kicked must be 0");
            }
            other => panic!("expected OptionalCostChoice, got {other:?}"),
        }

        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("first kick must be accepted");

        // Re-prompt after one kick.
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost, times_kicked, ..
            } => {
                assert!(
                    matches!(
                        cost,
                        AdditionalCost::Kicker {
                            repeatability:
                                crate::types::ability::AdditionalCostRepeatability::Repeatable,
                            ..
                        }
                    ),
                    "re-prompt must still be a Kicker: {cost:?}"
                );
                assert_eq!(times_kicked, 1, "times_kicked must be 1 after one kick");
            }
            other => panic!("expected OptionalCostChoice re-prompt, got {other:?}"),
        }

        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("second kick must be accepted");

        // Re-prompt after two kicks.
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost, times_kicked, ..
            } => {
                assert!(matches!(cost, AdditionalCost::Kicker { .. }));
                assert_eq!(times_kicked, 2, "times_kicked must be 2 after two kicks");
            }
            other => panic!("expected OptionalCostChoice re-prompt, got {other:?}"),
        }

        // Decline ("Done") — finish casting; spell resolves.
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining further kicks must finish the cast");
        runner.advance_until_stack_empty();

        assert_eq!(
            charge_counters(runner.state(), spell_id),
            2,
            "Everflowing Chalice kicked twice must enter with 2 charge counters"
        );
        assert!(
            !runner.state().cancelled_casts.contains(&spell_id),
            "a completed multikicker cast must not be in cancelled_casts"
        );
    }

    /// Engine test 2 — declining the kicker at the first prompt COMPLETES the
    /// cast (decline != abort). The artifact enters with 0 charge counters.
    #[test]
    fn declined_kicker_completes_cast_with_zero_counters() {
        use crate::types::GameAction;
        let (mut runner, spell_id, card_id) = everflowing_chalice_scenario();
        // {0} base cost — no extra mana needed.

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting Everflowing Chalice must be accepted");

        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice {
                    times_kicked: 0,
                    ..
                }
            ),
            "expected the first kicker prompt"
        );

        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining the kicker must finish the cast");
        runner.advance_until_stack_empty();

        assert_eq!(
            charge_counters(runner.state(), spell_id),
            0,
            "an unkicked Everflowing Chalice enters with 0 charge counters"
        );
        assert!(
            !runner.state().cancelled_casts.contains(&spell_id),
            "declining the kicker must NOT cancel the cast"
        );
        assert_eq!(
            runner.state().objects[&spell_id].zone,
            Zone::Battlefield,
            "the unkicked artifact must have resolved onto the battlefield"
        );
    }

    /// CR 702.157a: Squad uses a repeatable non-kicker additional-cost flow,
    /// then creates one copy token for each squad payment as the permanent
    /// enters.
    #[test]
    fn squad_paid_twice_creates_two_copy_tokens() {
        use crate::game::scenario::GameScenario;
        use crate::types::GameAction;

        const ENDLESS_FOOT_ASSAULT_ORACLE: &str = "Squad {1}{W} (As an additional cost to cast \
this spell, you may pay {1}{W} any number of times. When this enchantment enters, create that \
many tokens that are copies of it.)";

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);
        let mut builder = scenario.add_creature_to_hand(PlayerId(0), "Endless Foot Assault", 0, 0);
        builder.as_enchantment();
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        });
        builder.from_oracle_text_with_keywords(&["squad:{1}{W}"], ENDLESS_FOOT_ASSAULT_ORACLE);
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;
        let mut runner = scenario.build();
        fund_white(&mut runner, 4);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting squad spell must be accepted");

        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost, times_kicked, ..
            } => {
                assert!(matches!(
                    cost,
                    AdditionalCost::Optional {
                        cost: AbilityCost::Mana { .. },
                        repeatability:
                            crate::types::ability::AdditionalCostRepeatability::Repeatable,
                    }
                ));
                assert_eq!(times_kicked, 0);
            }
            other => panic!("expected first squad prompt, got {other:?}"),
        }

        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("first squad payment must be accepted");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("second squad payment must be accepted");
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining further squad payments must finish the cast");
        runner.advance_until_stack_empty();

        let assault_permanents = runner
            .state()
            .battlefield
            .iter()
            .filter(|id| {
                runner
                    .state()
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.name == "Endless Foot Assault")
            })
            .count();
        assert_eq!(
            assault_permanents, 3,
            "original permanent plus two squad copy tokens should be on the battlefield"
        );
    }

    /// CR 702.175a-b: Offspring granted only while a spell is being cast still
    /// installs the linked ETB copy trigger on the resolving permanent.
    #[test]
    fn granted_offspring_paid_creates_copy_token_on_etb() {
        use crate::game::scenario::GameScenario;
        use crate::types::keywords::Keyword;
        use crate::types::GameAction;

        let offspring_cost = ManaCost::generic(1);
        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);
        scenario
            .add_creature(PlayerId(0), "Offspring Grantor", 1, 1)
            .with_static_definition(
                StaticDefinition::new(StaticMode::CastWithKeyword {
                    keyword: Keyword::Offspring(offspring_cost.clone()),
                })
                .affected(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
            );

        let mut builder =
            scenario.add_creature_to_hand(PlayerId(0), "Granted Offspring Bear", 2, 2);
        builder.with_mana_cost(ManaCost::generic(0));
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;
        let mut runner = scenario.build();
        fund_colorless(&mut runner, 1);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting granted-Offspring creature must be accepted");

        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { cost, .. } => assert!(matches!(
                cost,
                AdditionalCost::Optional {
                    cost: AbilityCost::Mana { .. },
                    repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                }
            )),
            other => panic!("expected granted Offspring prompt, got {other:?}"),
        }

        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("granted Offspring payment must be accepted");
        runner.advance_until_stack_empty();

        let offspring_bears: Vec<_> = runner
            .state()
            .battlefield
            .iter()
            .filter_map(|id| runner.state().objects.get(id))
            .filter(|obj| obj.name == "Granted Offspring Bear")
            .collect();
        assert_eq!(
            offspring_bears.len(),
            2,
            "original granted-Offspring permanent plus one copy token should be on the battlefield"
        );
        assert!(
            offspring_bears
                .iter()
                .any(|obj| obj.power == Some(1) && obj.toughness == Some(1)),
            "the granted-Offspring copy must be 1/1"
        );
    }

    // -----------------------------------------------------------------------
    // CR 702.56a: Replicate — repeatable optional additional cost paid any
    // number of times at cast (CR 601.2b/f-h), then a "when you cast this
    // spell" trigger copies the spell once per replicate payment (CR 707.10).
    // Reuses the same repeatable-`Optional` cost flow as Squad/multikicker and
    // the same `CopySpell` machinery as Casualty — the copy count comes from
    // `repeat_for = AdditionalCostPaymentCount`.
    // -----------------------------------------------------------------------

    /// Build a targetless "draw a card" instant in P0's hand carrying Replicate
    /// {1}. A targetless spell avoids the per-copy `CopyRetarget` prompt
    /// (CR 707.10c), so the copies resolve straight through and the copy count
    /// is observable via `SpellCopied` events alone.
    fn replicate_draw_scenario() -> (crate::game::scenario::GameRunner, ObjectId, CardId) {
        use crate::game::scenario::GameScenario;

        const REPLICATE_DRAW_ORACLE: &str = "Replicate {1} (As an additional cost to cast this \
spell, you may pay {1} any number of times. When you cast this spell, copy it for each time \
its replicate cost was paid.)\nDraw a card.";

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(PlayerId(0), "Test Replicate Draw", true, "");
        // {0} base cost — only the replicate payments cost mana.
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        });
        builder.from_oracle_text_with_keywords(&["replicate:{1}"], REPLICATE_DRAW_ORACLE);
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;
        let runner = scenario.build();
        (runner, spell_id, card_id)
    }

    fn granted_replicate_static() -> StaticDefinition {
        let replicate_cost = ManaCost::Cost {
            shards: vec![],
            generic: 1,
        };
        StaticDefinition::new(StaticMode::CastWithKeyword {
            keyword: Keyword::Replicate(replicate_cost),
        })
        .affected(TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Instant).controller(ControllerRef::You),
        ))
    }

    fn granted_replicate_draw_scenario() -> (crate::game::scenario::GameRunner, ObjectId, CardId) {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        scenario
            .add_creature(PlayerId(0), "Replicate Grantor", 1, 1)
            .with_static_definition(granted_replicate_static());

        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Granted Replicate Draw", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        });
        builder.from_oracle_text("Draw a card.");
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;
        let runner = scenario.build();
        (runner, spell_id, card_id)
    }

    /// Count `SpellCopied` events emitted while resolving the stack to empty.
    /// Each `Effect::CopySpell` iteration emits exactly one (CR 707.10), so the
    /// total equals the number of replicate copies created.
    fn drain_counting_spell_copies(runner: &mut crate::game::scenario::GameRunner) -> usize {
        use crate::types::actions::GameAction;
        let mut copies = 0usize;
        for _ in 0..40 {
            if runner.state().stack.is_empty() {
                break;
            }
            match runner.act(GameAction::PassPriority) {
                Ok(result) => {
                    copies += result
                        .events
                        .iter()
                        .filter(|e| {
                            matches!(e, crate::types::events::GameEvent::SpellCopied { .. })
                        })
                        .count();
                }
                Err(_) => break,
            }
        }
        copies
    }

    /// CR 702.56a: Replicate paid twice copies the spell twice — two extra
    /// copies on the stack (plus the original spell), per CR 707.10.
    #[test]
    fn replicate_paid_twice_creates_two_copies() {
        use crate::types::GameAction;
        let (mut runner, spell_id, card_id) = replicate_draw_scenario();
        fund_colorless(&mut runner, 2); // {1} + {1} for two replicate payments

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the replicate spell must be accepted");

        // CR 601.2b/f-h: the repeatable additional cost surfaces as the same
        // `OptionalCostChoice` prompt Squad/multikicker use.
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost, times_kicked, ..
            } => {
                assert!(
                    matches!(
                        cost,
                        AdditionalCost::Optional {
                            cost: AbilityCost::Mana { .. },
                            repeatability:
                                crate::types::ability::AdditionalCostRepeatability::Repeatable,
                        }
                    ),
                    "replicate must surface a repeatable Optional mana cost: {cost:?}"
                );
                assert_eq!(times_kicked, 0, "first replicate prompt count must be 0");
            }
            other => panic!("expected the first replicate prompt, got {other:?}"),
        }

        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("first replicate payment must be accepted");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("second replicate payment must be accepted");
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining further replicate payments must finish the cast");

        // CR 601.2i + CR 603.3: after the cast commits, the stack holds the
        // original spell plus its "when you cast this spell" replicate trigger.
        assert!(
            runner.state().stack.iter().any(|e| e.id == spell_id),
            "the original replicate spell must be on the stack after the cast commits"
        );

        // CR 702.56a + CR 707.10: resolving the cast trigger copies the spell
        // once per replicate payment — exactly two copies.
        let copies = drain_counting_spell_copies(&mut runner);
        assert_eq!(
            copies, 2,
            "replicate paid twice must create exactly two copies (original + 2 copies)"
        );
    }

    /// CR 702.56a: Replicate granted by `CastWithKeyword` must use the same
    /// optional payment and copy-on-cast machinery as printed Replicate.
    #[test]
    fn granted_replicate_paid_twice_creates_two_copies() {
        use crate::types::GameAction;
        let (mut runner, spell_id, card_id) = granted_replicate_draw_scenario();
        fund_colorless(&mut runner, 2);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the granted-replicate spell must be accepted");

        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice {
                cost, times_kicked, ..
            } => {
                assert!(
                    matches!(
                        cost,
                        AdditionalCost::Optional {
                            cost: AbilityCost::Mana { .. },
                            repeatability:
                                crate::types::ability::AdditionalCostRepeatability::Repeatable,
                        }
                    ),
                    "granted Replicate must surface a repeatable Optional mana cost: {cost:?}"
                );
                assert_eq!(times_kicked, 0, "first granted Replicate prompt count");
            }
            other => panic!("expected granted Replicate prompt, got {other:?}"),
        }

        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("first granted Replicate payment must be accepted");
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("second granted Replicate payment must be accepted");
        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining further granted Replicate payments must finish the cast");

        let copies = drain_counting_spell_copies(&mut runner);
        assert_eq!(
            copies, 2,
            "granted Replicate paid twice must create exactly two copies"
        );
    }

    /// CR 601.2b + CR 702.56a: Replicate's optional cost is declared before
    /// target selection for targeted spells, including when granted by a static.
    #[test]
    fn granted_replicate_targeted_spell_prompts_before_target_selection() {
        use crate::game::scenario::GameScenario;
        use crate::types::GameAction;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);
        scenario
            .add_creature(PlayerId(0), "Replicate Grantor", 1, 1)
            .with_static_definition(granted_replicate_static());
        scenario.add_creature(PlayerId(1), "Target Bear", 2, 2);

        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Granted Replicate Bolt", true);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        });
        builder.with_ability(Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
            damage_source: None,
        });
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;
        let mut runner = scenario.build();
        fund_colorless(&mut runner, 1);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting targeted granted-Replicate spell must start");

        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { .. }
            ),
            "granted Replicate must prompt before target selection, got {:?}",
            runner.state().waiting_for
        );
    }

    /// CR 702.56a: Paying replicate zero times makes no copies — the "if a
    /// replicate cost was paid" intervening clause is false, and the
    /// `AdditionalCostPaymentCount`-driven copy count is zero.
    #[test]
    fn replicate_paid_zero_times_creates_no_copies() {
        use crate::types::GameAction;
        let (mut runner, spell_id, card_id) = replicate_draw_scenario();
        // {0} base cost — no mana needed when replicate is declined.

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the replicate spell must be accepted");

        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice {
                    times_kicked: 0,
                    ..
                }
            ),
            "expected the first replicate prompt"
        );

        runner
            .act(GameAction::DecideOptionalCost { pay: false })
            .expect("declining replicate must finish the cast");

        let copies = drain_counting_spell_copies(&mut runner);
        assert_eq!(
            copies, 0,
            "declining replicate must create zero copies (just the original spell)"
        );
        assert!(
            !runner.state().cancelled_casts.contains(&spell_id),
            "declining replicate must NOT cancel the cast"
        );
    }

    /// Engine test 2b — `CancelCast` at the first kicker prompt aborts the
    /// cast: the spell returns to its origin zone and lands in `cancelled_casts`.
    /// Proves abort and decline are genuinely distinct engine outcomes.
    #[test]
    fn cancel_cast_at_first_kicker_prompt_aborts_the_cast() {
        use crate::types::GameAction;
        let (mut runner, spell_id, card_id) = everflowing_chalice_scenario();

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting Everflowing Chalice must be accepted");

        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ));

        runner
            .act(GameAction::CancelCast)
            .expect("CancelCast at the kicker prompt must be accepted");

        assert!(
            runner.state().cancelled_casts.contains(&spell_id),
            "aborting the cast must record the spell in cancelled_casts"
        );
        assert_eq!(
            runner.state().objects[&spell_id].zone,
            Zone::Hand,
            "an aborted cast must return the card to its origin (hand) zone"
        );
        assert!(
            runner.state().stack.is_empty(),
            "an aborted cast must not leave the spell on the stack"
        );
    }

    // ---------------------------------------------------------------------
    // Issue #510 — blight COST form: N -1/-1 counters on ONE chosen creature.
    // CR 701.68a-c. Tests drive the real `apply` casting pipeline.
    // ---------------------------------------------------------------------

    /// Build a sorcery in P0's hand carrying a `Required(Blight N)` additional
    /// cost. The spell has a parsed Scry ability so the resolved ability (and
    /// its `cost_paid_object` snapshot) is observable on the stack entry.
    fn blight_cost_scenario(
        blight_n: u32,
        controlled_creatures: usize,
    ) -> (
        crate::game::scenario::GameRunner,
        ObjectId,
        CardId,
        Vec<ObjectId>,
    ) {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        scenario.at_phase(crate::types::Phase::PreCombatMain);

        let creatures: Vec<ObjectId> = (0..controlled_creatures)
            .map(|i| {
                scenario
                    .add_creature(PlayerId(0), &format!("Bear {i}"), 3, 3)
                    .id()
            })
            .collect();

        let mut builder = scenario.add_spell_to_hand(PlayerId(0), "Blight Sorcery", false);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        });
        builder.from_oracle_text("Scry 1.");
        builder.with_additional_cost(AdditionalCost::Required(AbilityCost::Blight {
            count: blight_n,
        }));
        let spell_id = builder.id();
        let card_id = scenario.state.objects[&spell_id].card_id;

        let runner = scenario.build();
        (runner, spell_id, card_id, creatures)
    }

    /// Read the `Minus1Minus1` counter total on a battlefield object.
    fn minus_counters(state: &GameState, id: ObjectId) -> u32 {
        state
            .objects
            .get(&id)
            .and_then(|o| {
                o.counters
                    .get(&crate::types::counter::CounterType::Minus1Minus1)
            })
            .copied()
            .unwrap_or(0)
    }

    /// The resolved ability's `cost_paid_object` snapshot, read off the spell's
    /// stack entry after the blight cost has been paid.
    fn stack_cost_paid_object(
        state: &GameState,
        spell_id: ObjectId,
    ) -> Option<crate::types::ability::CostPaidObjectSnapshot> {
        state
            .stack
            .iter()
            .filter(|entry| entry.source_id == spell_id)
            .find_map(|entry| entry.ability().and_then(|a| a.cost_paid_object.clone()))
    }

    /// Test A — CR 701.68a: blighting N places N -1/-1 counters on the ONE
    /// chosen creature, not one counter per creature. Reverted fix lands 1.
    #[test]
    fn blight_cost_places_n_counters_on_one_creature() {
        use crate::types::GameAction;

        let (mut runner, spell_id, card_id, creatures) = blight_cost_scenario(2, 1);
        let target = creatures[0];

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the Blight 2 sorcery must be accepted");

        match runner.state().waiting_for.clone() {
            WaitingFor::BlightChoice {
                counters,
                creatures,
                ..
            } => {
                assert_eq!(counters, 2, "BlightChoice must carry N=2 counters");
                assert_eq!(creatures, vec![target], "eligibility pool is the one Bear");
            }
            other => panic!("expected BlightChoice, got {other:?}"),
        }

        runner
            .act(GameAction::SelectCards {
                cards: vec![target],
            })
            .expect("selecting the one creature to blight must be accepted");

        assert_eq!(
            minus_counters(runner.state(), target),
            2,
            "CR 701.68a: Blight 2 must place 2 -1/-1 counters on the chosen creature"
        );
    }

    /// Test B — CR 701.68b: blight is payable while the player controls >=1
    /// creature, even when N exceeds the controlled-creature count. Reverted
    /// fix demands N creatures and returns false.
    #[test]
    fn blight_payable_with_n_greater_than_creature_count() {
        use crate::game::scenario::GameScenario;

        let mut scenario = GameScenario::new();
        let bear = scenario.add_creature(PlayerId(0), "Lone Bear", 2, 2).id();

        assert!(
            AbilityCost::Blight { count: 3 }.is_payable(&scenario.state, PlayerId(0), bear),
            "CR 701.68b: Blight 3 is payable with a single controlled creature"
        );
    }

    /// Test C — CR 701.68b eligibility gate: with zero controlled creatures the
    /// cast is rejected and no `BlightChoice` is ever constructed.
    #[test]
    fn blight_cost_rejected_with_no_creatures() {
        use crate::types::GameAction;

        let (mut runner, spell_id, card_id, _) = blight_cost_scenario(2, 0);

        let err = runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect_err("casting Blight 2 with no creatures must be rejected");

        assert!(
            !matches!(runner.state().waiting_for, WaitingFor::BlightChoice { .. }),
            "no BlightChoice WaitingFor may be constructed when ineligible"
        );
        let _ = err; // the cast is rejected before any blight prompt
    }

    /// Test D — CR 614.1: the counter placement routes through
    /// `add_counter_with_replacement`. With a counter-doubling replacement
    /// active, Blight 1 lands 2 counters. Reverted fix mutates counters
    /// directly and lands only 1.
    #[test]
    fn blight_cost_is_replacement_aware() {
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;
        use crate::types::GameAction;

        let (mut runner, spell_id, card_id, creatures) = blight_cost_scenario(1, 1);
        let target = creatures[0];

        // CR 614.1a: counter-doubling replacement effect (Doubling Season-class).
        let repl = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Double);
        runner
            .state_mut()
            .objects
            .get_mut(&target)
            .unwrap()
            .replacement_definitions = vec![repl].into();

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the Blight 1 sorcery must be accepted");
        runner
            .act(GameAction::SelectCards {
                cards: vec![target],
            })
            .expect("selecting the creature to blight must be accepted");

        assert_eq!(
            minus_counters(runner.state(), target),
            2,
            "CR 614.1: Blight 1 under a doubling replacement must land 2 counters"
        );
    }

    /// Test E — CR 701.68a: exactly one creature must be chosen. Selecting two
    /// creatures against the `BlightChoice` is an `InvalidAction`.
    #[test]
    fn blight_cost_rejects_multiple_creatures() {
        use crate::types::GameAction;

        let (mut runner, spell_id, card_id, creatures) = blight_cost_scenario(2, 2);

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the Blight 2 sorcery must be accepted");

        let err = runner
            .act(GameAction::SelectCards {
                cards: vec![creatures[0], creatures[1]],
            })
            .expect_err("selecting two creatures to blight must be rejected");

        match err {
            EngineError::InvalidAction(msg) => assert!(
                msg.contains("Must blight exactly one creature, got 2"),
                "unexpected error message: {msg}"
            ),
            other => panic!("expected InvalidAction, got {other:?}"),
        }
    }

    /// Test F — CR 117.1 / CR 608.2k: the blighted creature is snapshotted as
    /// the resolving ability's `cost_paid_object`. Reverted fix leaves the
    /// field `None`.
    #[test]
    fn blight_cost_snapshots_cost_paid_object() {
        use crate::types::GameAction;

        let (mut runner, spell_id, card_id, creatures) = blight_cost_scenario(2, 1);
        let target = creatures[0];

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the Blight 2 sorcery must be accepted");
        runner
            .act(GameAction::SelectCards {
                cards: vec![target],
            })
            .expect("selecting the creature to blight must be accepted");

        let snapshot = stack_cost_paid_object(runner.state(), spell_id)
            .expect("CR 608.2k: the resolving ability must carry a cost_paid_object snapshot");
        assert_eq!(
            snapshot.object_id, target,
            "the cost-paid object must be the blighted creature"
        );
    }

    /// Test G — degenerate `Blight 0` guard (#510 SHOULD-FIX 2): no counter is
    /// placed (the `if counters > 0` guard suppresses the call) but the
    /// `cost_paid_object` snapshot is still taken (it is unconditional).
    #[test]
    fn blight_zero_places_no_counter_but_still_snapshots() {
        use crate::types::GameAction;

        let (mut runner, spell_id, card_id, creatures) = blight_cost_scenario(0, 1);
        let target = creatures[0];

        runner
            .act(GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: vec![],

                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting the Blight 0 sorcery must be accepted");
        runner
            .act(GameAction::SelectCards {
                cards: vec![target],
            })
            .expect("selecting the creature to blight must be accepted");

        assert_eq!(
            minus_counters(runner.state(), target),
            0,
            "Blight 0 must place no -1/-1 counter (if counters > 0 guard)"
        );
        let snapshot = stack_cost_paid_object(runner.state(), spell_id)
            .expect("the cost_paid_object snapshot is unconditional, even for Blight 0");
        assert_eq!(snapshot.object_id, target);
    }

    // ────────────────────────────────────────────────────────────────────────
    // CR 702.48: Offering
    // ────────────────────────────────────────────────────────────────────────

    /// CR 702.48a: A Spirit-offering spell at sorcery speed presents an
    /// optional sacrifice prompt for a Spirit permanent the controller controls.
    #[test]
    fn spirit_offering_presents_optional_sacrifice_for_spirit() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let spirit = create_object(
            &mut state,
            CardId(10),
            caster,
            "Thief of Hope Spirit Sac".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spirit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Spirit".to_string());
        }

        let spell = create_object(
            &mut state,
            CardId(11),
            caster,
            "Kitsune Blademaster".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Offering("Spirit".to_string()));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            };
        }

        let mut events = Vec::new();
        // Use NoCost so the test focuses on Offering detection, not mana payment.
        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(11),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &ManaCost::NoCost,
            None,
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Auto,
            &mut events,
        )
        .expect("Spirit offering spell must be castable");

        match waiting {
            WaitingFor::OptionalCostChoice { ref cost, .. } => {
                assert!(
                    matches!(
                        cost,
                        AdditionalCost::Optional {
                            cost: AbilityCost::Sacrifice(c),
                            repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
                        } if c.requirement == SacrificeRequirement::count(1)
                    ),
                    "expected optional Spirit sacrifice, got {cost:?}"
                );
            }
            other => panic!("expected OptionalCostChoice for Offering, got {other:?}"),
        }
    }

    /// CR 702.48c: `apply_offering_cost_reduction` reduces by the sacrificed
    /// permanent's mana cost. {1}{G} sacrifice reduces {3}{W} spell to {1}{W}.
    ///   shard {G} → no match in {W} → excess reduces generic: 3→2
    ///   sac generic 1 → generic: 2→1. Result: {W}{1}.
    #[test]
    fn offering_cost_reduction_applies_per_cr_702_48c() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        // Spirit with {1}{G} mana cost.
        let spirit = create_object(
            &mut state,
            CardId(20),
            caster,
            "Floating Spirit Sac".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&spirit).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };

        // Spell with Spirit offering.
        let spell = create_object(
            &mut state,
            CardId(21),
            caster,
            "Spirit Offering Spell".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .keywords
            .push(Keyword::Offering("Spirit".to_string()));

        let mut spell_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 3,
        };
        apply_offering_cost_reduction(&state, spirit, &mut spell_cost);

        assert_eq!(
            spell_cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            },
            "{{3}}{{W}} reduced by {{1}}{{G}} must equal {{W}}{{1}}"
        );
    }

    /// CR 601.2f: A "for each [filter] sacrificed this way" reduction counts
    /// only selected cost-payment objects that match the parsed dynamic filter.
    #[test]
    fn sacrificed_this_way_reduction_filters_selected_objects() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);
        let spell = create_object(
            &mut state,
            CardId(30),
            caster,
            "Filtered Sacrifice Spell".to_string(),
            Zone::Hand,
        );
        let static_def = StaticDefinition::new(StaticMode::ModifyCost {
            mode: CostModifyMode::Reduce,
            amount: ManaCost::Cost {
                shards: vec![],
                generic: 1,
            },
            spell_filter: None,
            dynamic_count: Some(QuantityRef::FilteredTrackedSetSize {
                filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
                caused_by: None,
            }),
        })
        .affected(TargetFilter::SelfRef)
        .condition(StaticCondition::And {
            conditions: vec![StaticCondition::None, StaticCondition::AdditionalCostPaid],
        });
        state
            .objects
            .get_mut(&spell)
            .unwrap()
            .static_definitions
            .push(static_def);

        let creature = create_object(
            &mut state,
            CardId(31),
            caster,
            "Creature Fodder".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let artifact = create_object(
            &mut state,
            CardId(32),
            caster,
            "Artifact Fodder".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let mut spell_cost = ManaCost::Cost {
            shards: vec![],
            generic: 5,
        };
        apply_sacrificed_this_way_cost_reduction(
            &state,
            spell,
            &[creature, artifact],
            &mut spell_cost,
        );

        assert_eq!(
            spell_cost,
            ManaCost::Cost {
                shards: vec![],
                generic: 4,
            },
            "only the sacrificed creature should count for the filtered reduction"
        );
        assert!(
            state.tracked_object_sets.is_empty(),
            "cost-time sacrificed-this-way reduction must not publish a stale tracked set"
        );
    }

    /// CR 702.48b: Accepting the Offering prompts to sacrifice a qualifying
    /// permanent before target selection.
    #[test]
    fn accepting_spirit_offering_prompts_sacrifice_selection() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let spirit = create_object(
            &mut state,
            CardId(22),
            caster,
            "Selectable Spirit".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spirit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Spirit".to_string());
        }

        let spell = create_object(
            &mut state,
            CardId(23),
            caster,
            "Spirit Offering Spell 2".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Offering("Spirit".to_string()));
            obj.mana_cost = ManaCost::NoCost;
        }

        let mut events = Vec::new();
        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(23),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &ManaCost::NoCost,
            None,
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Auto,
            &mut events,
        )
        .expect("Spirit offering spell must be castable");

        let WaitingFor::OptionalCostChoice {
            cost: ref offering_cost,
            pending_cast: ref pending_box,
            ..
        } = waiting
        else {
            panic!("expected OptionalCostChoice for Offering, got {waiting:?}");
        };
        let pending_cast = *pending_box.clone();

        // Accept the Offering.
        let waiting = handle_decide_additional_cost(
            &mut state,
            caster,
            pending_cast,
            offering_cost,
            true,
            &mut events,
        )
        .expect("accepting offering must succeed");

        // Engine should now prompt for which Spirit to sacrifice.
        let WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ref choices,
            ..
        } = waiting
        else {
            panic!("expected PayCost(Sacrifice) for Offering, got {waiting:?}");
        };
        assert!(
            choices.contains(&spirit),
            "spirit must be in eligible sacrifice list"
        );
    }

    /// CR 702.48a: Artifact offering matches card type Artifact, not subtype.
    #[test]
    fn accepting_artifact_offering_prompts_artifact_sacrifice() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let artifact = create_object(
            &mut state,
            CardId(220),
            caster,
            "Jeweled Bird".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&artifact).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.base_card_types = obj.card_types.clone();
        }

        let spell = create_object(
            &mut state,
            CardId(221),
            caster,
            "Blast-Furnace Hellkite".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Offering("Artifact".to_string()));
            obj.mana_cost = ManaCost::NoCost;
        }

        let mut events = Vec::new();
        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(221),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &ManaCost::NoCost,
            None,
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Auto,
            &mut events,
        )
        .expect("Artifact offering spell must be castable");

        let WaitingFor::OptionalCostChoice {
            cost: ref offering_cost,
            pending_cast: ref pending_box,
            ..
        } = waiting
        else {
            panic!("expected OptionalCostChoice for Artifact Offering, got {waiting:?}");
        };

        let waiting = handle_decide_additional_cost(
            &mut state,
            caster,
            *pending_box.clone(),
            offering_cost,
            true,
            &mut events,
        )
        .expect("accepting Artifact offering must succeed");

        let WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ref choices,
            ..
        } = waiting
        else {
            panic!("expected PayCost(Sacrifice) for Artifact Offering, got {waiting:?}");
        };
        assert!(
            choices.contains(&artifact),
            "artifact must be in eligible sacrifice list, got {choices:?}"
        );
    }

    /// CR 702.48b: Selecting a Spirit for sacrifice removes it from the battlefield.
    /// CR 702.48c: The selected Spirit's mana cost reduces the spell's pending
    /// mana payment.
    #[test]
    fn accepting_spirit_offering_sacrifices_permanent_and_reduces_cost() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let spirit = create_object(
            &mut state,
            CardId(24),
            caster,
            "Sacrificed Spirit".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spirit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Spirit".to_string());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        let spell = create_object(
            &mut state,
            CardId(25),
            caster,
            "Spirit Offering Spell 3".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Offering("Spirit".to_string()));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            };
        }
        for _ in 0..4 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::White,
                ObjectId(940),
                false,
                Vec::new(),
            ));
        }

        let mut events = Vec::new();
        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(25),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            },
            Some(ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            }),
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Manual,
            &mut events,
        )
        .expect("Spirit offering spell must be castable");

        let WaitingFor::OptionalCostChoice {
            cost: ref offering_cost,
            pending_cast: ref pending_box,
            ..
        } = waiting
        else {
            panic!("expected OptionalCostChoice for Offering, got {waiting:?}");
        };
        let pending_cast = *pending_box.clone();

        let waiting = handle_decide_additional_cost(
            &mut state,
            caster,
            pending_cast,
            offering_cost,
            true,
            &mut events,
        )
        .expect("accepting offering must succeed");

        // Confirm the sacrifice selection prompt includes the spirit.
        let WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ref choices,
            ref resume,
            ..
        } = waiting
        else {
            panic!("expected PayCost(Sacrifice) for Offering, got {waiting:?}");
        };
        assert!(choices.contains(&spirit), "spirit must be in eligible list");

        // Execute sacrifice selection and verify the spirit leaves the battlefield.
        let CostResume::SpellCost {
            spell: ref pending_box2,
            cost: ref offering_pay_cost,
            source,
            ..
        } = resume
        else {
            panic!("expected CostResume::SpellCost");
        };
        assert_eq!(
            *source,
            SpellCostSource::Offering,
            "Offering sacrifice prompt must carry Offering source identity"
        );
        let pending2 = *pending_box2.clone();

        // Move spell to stack (normally done by announce_spell_on_stack in the
        // real casting pipeline — needed by finalize_cast_to_stack).
        crate::game::stack::push_to_stack(
            &mut state,
            crate::types::game_state::StackEntry {
                id: spell,
                source_id: spell,
                controller: caster,
                kind: crate::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(25),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut events,
        );

        let waiting = handle_sacrifice_for_cost(
            &mut state,
            caster,
            pending2,
            Some(SpellCostPayment {
                cost: offering_pay_cost.as_ref(),
                source: *source,
            }),
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: choices,
                chosen: &[spirit],
            },
            &mut events,
        )
        .expect("sacrifice selection must succeed");

        assert!(
            !state.battlefield.contains(&spirit),
            "sacrificed spirit must leave battlefield"
        );
        let WaitingFor::ManaPayment { .. } = waiting else {
            panic!("expected ManaPayment after offering sacrifice, got {waiting:?}");
        };
        let pending = state
            .pending_cast
            .as_ref()
            .expect("pending cast must exist");
        assert_eq!(
            pending.cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 1,
            },
            "{{3}}{{W}} reduced by {{1}}{{G}} must equal {{1}}{{W}}"
        );
    }

    /// CR 702.48c: Only the Offering additional cost reduces the spell. A
    /// different sacrifice cost on an Offering spell must not reduce the cost
    /// just because the sacrificed permanent also matches the Offering quality.
    #[test]
    fn non_offering_sacrifice_on_offering_spell_does_not_reduce_cost() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let spirit = create_object(
            &mut state,
            CardId(26),
            caster,
            "Sacrificed Spirit For Other Cost".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spirit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Spirit".to_string());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        let spell = create_object(
            &mut state,
            CardId(27),
            caster,
            "Spirit Offering Spell With Other Cost".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Offering("Spirit".to_string()));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            };
        }

        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            spell,
            caster,
        );
        ability.context.additional_cost_paid = true;
        let mut pending = PendingCast::new(
            spell,
            CardId(27),
            ability,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            },
        );
        pending.base_cost = Some(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 3,
        });
        pending.payment_mode = CastPaymentMode::Manual;

        let mut events = Vec::new();
        crate::game::stack::push_to_stack(
            &mut state,
            crate::types::game_state::StackEntry {
                id: spell,
                source_id: spell,
                controller: caster,
                kind: crate::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(27),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut events,
        );

        let non_offering_cost =
            AbilityCost::Sacrifice(SacrificeCost::count(offering_quality_filter("Spirit"), 1));
        let waiting = handle_sacrifice_for_cost(
            &mut state,
            caster,
            pending,
            Some(SpellCostPayment {
                cost: &non_offering_cost,
                source: SpellCostSource::Other,
            }),
            CostSelection {
                min_count: 1,
                count: 1,
                legal_permanents: &[spirit],
                chosen: &[spirit],
            },
            &mut events,
        )
        .expect("non-offering sacrifice selection must succeed");

        let WaitingFor::ManaPayment { .. } = waiting else {
            panic!("expected ManaPayment after non-offering sacrifice, got {waiting:?}");
        };
        let pending = state
            .pending_cast
            .as_ref()
            .expect("pending cast must exist");
        assert_eq!(
            pending.cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            },
            "non-offering sacrifice must not reduce an Offering spell's cost"
        );
    }

    /// CR 702.48a: Declining the Offering leaves the spell's cost unchanged.
    #[test]
    fn declining_spirit_offering_preserves_full_cost() {
        let mut state = GameState::new_two_player(42);
        let caster = PlayerId(0);

        let spirit = create_object(
            &mut state,
            CardId(30),
            caster,
            "Declining Spirit".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spirit).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.card_types.subtypes.push("Spirit".to_string());
            obj.mana_cost = ManaCost::Cost {
                shards: vec![],
                generic: 2,
            };
        }

        let spell = create_object(
            &mut state,
            CardId(31),
            caster,
            "Kitsune Blademaster 3".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Offering("Spirit".to_string()));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            };
        }

        let printed_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 3,
        };
        // Fund enough mana to pass the affordability pre-check.
        for _ in 0..4 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::White,
                ObjectId(930),
                false,
                Vec::new(),
            ));
        }
        let mut events = Vec::new();

        let waiting = check_additional_cost_or_pay_with_distribute(
            &mut state,
            caster,
            spell,
            CardId(31),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                spell,
                caster,
            ),
            &printed_cost,
            Some(printed_cost.clone()),
            CastingVariant::Normal,
            None,
            None,
            Zone::Hand,
            CastPaymentMode::Manual, // manual so mana payment pauses, not auto-completes
            &mut events,
        )
        .expect("Spirit offering spell must be castable");

        let WaitingFor::OptionalCostChoice {
            cost: ref offering_cost,
            pending_cast: ref pending_box,
            ..
        } = waiting
        else {
            panic!("expected OptionalCostChoice, got {waiting:?}");
        };
        let pending_cast = *pending_box.clone();

        // Pre-announce spell to the stack (normally done by announce_spell_on_stack).
        crate::game::stack::push_to_stack(
            &mut state,
            crate::types::game_state::StackEntry {
                id: spell,
                source_id: spell,
                controller: caster,
                kind: crate::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(31),
                    ability: None,
                    casting_variant: CastingVariant::Normal,
                    actual_mana_spent: 0,
                },
            },
            &mut events,
        );

        // Decline the Offering.
        let waiting = handle_decide_additional_cost(
            &mut state,
            caster,
            pending_cast,
            offering_cost,
            false,
            &mut events,
        )
        .expect("declining offering must succeed");

        // Spirit survives.
        assert!(
            state.battlefield.contains(&spirit),
            "spirit must survive when offering is declined"
        );

        // After declining, engine proceeds to mana payment with unchanged cost.
        let WaitingFor::ManaPayment { .. } = waiting else {
            panic!("expected ManaPayment after declining offering, got {waiting:?}");
        };
        let pending = state
            .pending_cast
            .as_ref()
            .expect("pending cast must exist");
        assert_eq!(
            pending.cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 3,
            },
            "declined offering must leave cost at full {{3}}{{W}}"
        );
    }

    /// CR 118.3 + CR 601.2h: A `{X}` + non-self SACRIFICE activated-ability cost
    /// takes the X-mana detour, leaving the non-self sacrifice as the unpaid
    /// residual in `push_activated_ability_to_stack`. That function only
    /// re-surfaces the non-self DISCARD arm; a non-self sacrifice/exile residual
    /// would otherwise fall through to `pay_ability_cost_for_activation` and be a
    /// SILENT `Paid` no-op (the cost would be skipped). The guard must instead
    /// fail LOUDLY. No real card has this cost shape; this guards the shared
    /// pipeline against a future one. In debug builds the `debug_assert!` panics
    /// (asserted here); in release builds it is compiled out and the function
    /// returns `EngineError::ActionNotAllowed` instead, so the cost is never
    /// silently skipped on either profile.
    #[test]
    #[should_panic(expected = "non-self sacrifice/exile cost unhandled")]
    fn x_residual_non_self_sacrifice_fails_loudly() {
        let mut state = GameState::new_two_player(42);
        // A permanent the player could sacrifice — present so the failure is the
        // unhandled-cost guard, not an empty eligible set.
        let source = create_object(
            &mut state,
            CardId(7_700),
            PlayerId(0),
            "X-Sacrifice Source".to_string(),
            Zone::Battlefield,
        );
        let _victim = create_object(
            &mut state,
            CardId(7_701),
            PlayerId(0),
            "Sac Fodder".to_string(),
            Zone::Battlefield,
        );
        let resolved = ResolvedAbility::new(
            Effect::Scry {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        // The X-mana sub-cost has already been extracted/paid by the detour; the
        // residual handed to `push_activated_ability_to_stack` is the non-self
        // sacrifice, flagged as the X-residual path (`x_residual = true`).
        let residual = AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
            1,
        ));
        let mut events = Vec::new();
        let _ = push_activated_ability_to_stack(
            &mut state,
            PlayerId(0),
            source,
            0,
            resolved,
            Some(&residual),
            true,
            &mut events,
        );
    }
}
