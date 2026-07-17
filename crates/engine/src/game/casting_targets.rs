use crate::types::ability::{
    AbilityCost, AbilityTag, AdditionalCost, Effect, ModalChoice, QuantityExpr, ResolvedAbility,
    TargetRef, TargetSelectionMode,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActivationTargetSelection, CostResume, GameState, PayCostKind, PendingCast, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;
use crate::types::zones::ExileCostSourceZone;

use super::ability_utils::{
    ability_target_legality_needs_chosen_x, assign_selected_slots_in_chain,
    assign_targets_in_chain, auto_select_targets_for_ability, begin_target_selection_for_ability,
    build_chained_resolved, build_target_slots_labelled, choose_target_for_ability,
    distribution_targets, ordered_selected_mode_indices, random_select_targets_for_ability,
    selected_mode_labels, validate_modal_indices, validate_selected_targets_for_ability,
    TargetSelectionAdvance,
};
use super::casting_costs::{
    cost_has_x, drain_deferred_triggers_after_stack_object_announcement, enter_payment_step,
    finish_pending_cast_cost_or_pay,
};
use super::engine::EngineError;
use super::restrictions;

/// Handle mode selection for a modal spell.
///
/// Combines chosen mode abilities into a single ResolvedAbility chain (sub_abilities),
/// then proceeds to targeting or directly to payment.
pub(crate) fn handle_select_modes(
    state: &mut GameState,
    // CR 700.2e: the mode *chooser* (controller for standard modals, the
    // opponent for "an opponent chooses —"). Used only by the dispatch-layer
    // authorization check in `engine.rs`; all spell control/cost/targeting
    // here uses `controller` derived from the pending cast.
    _mode_chooser: PlayerId,
    indices: Vec<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (modal, pending, unavailable_modes) = match &state.waiting_for {
        WaitingFor::ModeChoice {
            modal,
            pending_cast,
            unavailable_modes,
            ..
        } => (
            modal.clone(),
            *pending_cast.clone(),
            unavailable_modes.clone(),
        ),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for mode selection".to_string(),
            ));
        }
    };

    // CR 700.2a-b: Reject unavailable modes (repeat constraints or no legal targets).
    validate_modal_indices(&modal, &indices, &unavailable_modes)?;

    // CR 700.2 + CR 601.2c: Sorted ascending to match the slot order produced by
    // `build_chained_resolved` and `build_target_slots_labelled`. Persisted on
    // every `PendingCast` produced below so a later deferred target-selection
    // step (e.g. after `ChooseX`) can re-derive per-slot mode labels for the
    // targeting UI without re-running the mode-choice flow.
    let sorted_indices = ordered_selected_mode_indices(&indices);

    // CR 700.2e + CR 115.1: The `player` parameter is the mode *chooser* (the
    // controller for standard modals; the opponent for "an opponent chooses
    // —"). Mode selection (CR 601.2b) routes to that player, but the spell is
    // still controlled, targeted, and paid for by its controller (CR 115.1) —
    // captured on the pending cast's ability. All downstream cost/target/
    // resolution logic uses `controller`, never the mode-chooser.
    let controller = pending.ability.controller;

    // CR 702.172a + CR 601.2f: Spree mode costs (and entwine, CR 702.42a) are additional
    // costs layered on top of the base cost. `restrictions::add_mana_cost` treats `NoCost`/
    // zero as identity, so a cast-without-paying path (`pending.cost == zero`) yields exactly
    // the additional costs — alternative-cost permissions never waive them.
    let mut total_cost = compute_modal_total_cost(&pending.cost, &modal, &indices);
    let mut pending = pending;
    // CR 601.2b + CR 601.2f: Fold the chosen modal mode costs (Spree / Entwine
    // cost increases, computed against a zero base) into the declared mana
    // additions so any later pending recompute includes them without rewriting
    // the tax-inclusive base.
    if pending.base_cost.is_some() {
        let modal_only = compute_modal_total_cost(&ManaCost::zero(), &modal, &indices);
        if !modal_only.is_without_paying_mana() {
            pending.declared_mana_additions.push(modal_only);
            total_cost = super::casting::recompute_pending_mana_total(
                state,
                controller,
                &pending,
                pending.ability.chosen_x,
            );
        }
    }
    if let Some(cost) = escalate_cost_for_selected_modes(state, controller, &pending, indices.len())
    {
        pending.additional_cost_flow = Some(AdditionalCost::Required(cost));
    }

    // Get the card's abilities to build combined resolved ability from chosen modes
    let obj = state
        .objects
        .get(&pending.object_id)
        .ok_or_else(|| EngineError::InvalidAction("Modal spell object not found".to_string()))?;
    let abilities = obj.abilities.clone();

    // Build a chain of ResolvedAbility from chosen modes (in order)
    let mut resolved = build_chained_resolved(&abilities, &indices, pending.object_id, controller)?;
    resolved.set_context_recursive(pending.ability.context.clone());
    resolved.selected_mode_labels = selected_mode_labels(&modal.mode_descriptions, &indices);
    // CR 700.2a + CR 700.2d + CR 601.2b: latch the chosen modal-mode indices onto
    // the spell's context so finalize can stamp them on the stack object (read by
    // QuantityRef::EventContextSourceModesChosen). `sorted_indices` is the same
    // ground-truth vector stored on every derived PendingCast.chosen_modes below;
    // `resolved` becomes the finalize `ability` on all three sub-paths (direct via
    // finish_pending_cast_cost_or_pay, deferred-X and deferred-target-selection via
    // PendingCast::new), so the top-level context carries the count to finalize.
    resolved.context.chosen_modes = sorted_indices.clone();

    if pending.activation_ability_index.is_none()
        && pending.additional_cost_flow.is_none()
        && cost_has_x(&total_cost)
        && ability_target_legality_needs_chosen_x(&resolved, pending.distribute.as_ref())
    {
        let mut pending_x =
            PendingCast::new(pending.object_id, pending.card_id, resolved, total_cost);
        pending_x.base_cost = pending.base_cost.clone();
        pending_x.declared_mana_additions = pending.declared_mana_additions.clone();
        pending_x.target_constraints = pending.target_constraints;
        pending_x.casting_variant = pending.casting_variant;
        pending_x.casting_permission_index = pending.casting_permission_index;
        pending_x.cast_timing_permission = pending.cast_timing_permission;
        pending_x.distribute = pending.distribute;
        pending_x.origin_zone = pending.origin_zone;
        pending_x.payment_mode = pending.payment_mode;
        pending_x.deferred_target_selection = true;
        pending_x.chosen_modes = sorted_indices.clone();
        pending_x.additional_cost_decided = pending.additional_cost_decided;
        pending_x.declared_kickers_to_pay = pending.declared_kickers_to_pay;
        pending_x.declined_kickers = pending.declined_kickers;
        state.pending_cast = Some(Box::new(pending_x));
        return enter_payment_step(state, controller, None, events);
    }

    // Check for targeting on the combined ability
    super::layers::flush_layers(state);

    // CR 700.2 / CR 601.2b: Build slots and their per-mode display labels
    // together against the SAME post-flush state, so `mode_labels.len()` can
    // never diverge from `target_slots.len()` (slot count is state-dependent).
    let (target_slots, mode_labels) = build_target_slots_labelled(
        state,
        &abilities,
        &indices,
        &modal.mode_descriptions,
        pending.object_id,
        controller,
        &pending.ability.context,
        // CR 107.1b: X is announced during the cost-payment step (after target
        // selection on this non-deferred path), so it is not yet known here.
        None,
    )?;
    if !target_slots.is_empty() {
        // CR 115.1 + CR 701.9b: For abilities marked `Random`, the game (not the
        // controller) selects targets uniformly from each slot's legal-target set.
        // No `WaitingFor::TargetSelection` is emitted — the choice is made now
        // using the seeded engine RNG. Checked before the auto-select degenerate
        // path so multi-target-legal random spells (where there's a choice to
        // make but the *controller* doesn't make it) take this branch.
        if matches!(resolved.target_selection_mode, TargetSelectionMode::Random) {
            let targets = random_select_targets_for_ability(
                state,
                &target_slots,
                &pending.target_constraints,
            )?;
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;
            return finish_pending_cast_cost_or_pay(
                state, controller, pending, resolved, total_cost, events,
            );
        }

        if let Some(targets) = auto_select_targets_for_ability(
            state,
            &resolved,
            &target_slots,
            &pending.target_constraints,
        )? {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;
            return finish_pending_cast_cost_or_pay(
                state, controller, pending, resolved, total_cost, events,
            );
        }

        let selection = begin_target_selection_for_ability(
            state,
            &resolved,
            &target_slots,
            &pending.target_constraints,
        )?;
        let mut pending_sel =
            PendingCast::new(pending.object_id, pending.card_id, resolved, total_cost);
        pending_sel.base_cost = pending.base_cost.clone();
        pending_sel.declared_mana_additions = pending.declared_mana_additions.clone();
        pending_sel.target_constraints = pending.target_constraints;
        pending_sel.casting_variant = pending.casting_variant;
        pending_sel.casting_permission_index = pending.casting_permission_index;
        pending_sel.origin_zone = pending.origin_zone;
        pending_sel.additional_cost_flow = pending.additional_cost_flow;
        pending_sel.deferred_target_selection = pending.deferred_target_selection;
        pending_sel.chosen_modes = sorted_indices.clone();
        pending_sel.additional_cost_decided = pending.additional_cost_decided;
        pending_sel.declared_kickers_to_pay = pending.declared_kickers_to_pay;
        pending_sel.declined_kickers = pending.declined_kickers;
        // CR 601.2c + CR 115.1: target declaration belongs to the controller by
        // default, but the FIRST slot may route its announcement to another player
        // ("of an opponent's choice"). For this card class slot 0 is the
        // controller; the general `chooser.unwrap_or(controller)` keeps the path
        // correct for any future card whose first slot is opponent-chosen.
        let initial_player = target_slots
            .first()
            .and_then(|slot| slot.chooser)
            .unwrap_or(controller);
        pending_sel.activation_cost = pending.activation_cost;
        pending_sel.activation_ability_index = pending.activation_ability_index;
        pending_sel.pending_loyalty_activation_player = pending.pending_loyalty_activation_player;
        pending_sel.activation_residual = pending.activation_residual;
        pending_sel.activation_target_selection = pending.activation_target_selection;
        return Ok(WaitingFor::TargetSelection {
            player: initial_player,
            pending_cast: Box::new(pending_sel),
            target_slots,
            mode_labels,
            selection,
        });
    }

    // No targets needed -- check additional cost, then pay
    finish_pending_cast_cost_or_pay(state, controller, pending, resolved, total_cost, events)
}

/// CR 601.2d: After targets are committed on a pending cast, pause for
/// `WaitingFor::DistributeAmong` when the spell divides a fixed pool among
/// those targets. Shared by bulk `SelectTargets` and slot-by-slot
/// `ChooseTarget` completion paths — the client drives the latter.
fn maybe_pause_for_cast_distribution(
    state: &mut GameState,
    player: PlayerId,
    pending: &PendingCast,
    ability: &ResolvedAbility,
) -> Result<Option<WaitingFor>, EngineError> {
    let Some(unit) = &pending.distribute else {
        return Ok(None);
    };
    let Some(total) = extract_distribution_total(state, ability, &ability.effect) else {
        // X-spell: distribution deferred to after mana payment.
        return Ok(None);
    };
    let assigned_targets = distribution_targets(ability);
    let mut pending_dist = pending.clone();
    pending_dist.ability = ability.clone();
    state.pending_cast = Some(Box::new(pending_dist));
    Ok(Some(WaitingFor::DistributeAmong {
        player,
        total,
        targets: assigned_targets,
        unit: unit.clone(),
    }))
}

/// Handle target selection for a pending cast.
pub(crate) fn handle_select_targets(
    state: &mut GameState,
    player: PlayerId,
    targets: Vec<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Extract PendingCast from WaitingFor::TargetSelection
    let pending = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            ..
        } => {
            // CR 601.2c + CR 115.1: when any slot is announced by a player other
            // than the controller ("of an opponent's choice"), the bulk SelectTargets
            // action (which submits every slot at once on the controller's behalf)
            // is not valid — each slot must be announced one at a time via
            // ChooseTarget so the correct player declares each target.
            if target_slots.iter().any(|slot| slot.chooser.is_some()) {
                return Err(EngineError::InvalidAction(
                    "Mixed-chooser targets must be announced one slot at a time".to_string(),
                ));
            }
            validate_selected_targets_for_ability(
                state,
                &pending_cast.ability,
                target_slots,
                &targets,
                &pending_cast.target_constraints,
            )?;
            *pending_cast.clone()
        }
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    let mut ability = pending.ability.clone();
    assign_targets_in_chain(state, &mut ability, &targets)?;

    if let Some(waiting_for) = maybe_pause_for_cast_distribution(state, player, &pending, &ability)?
    {
        return Ok(waiting_for);
    }

    if pending.activation_ability_index.is_some() {
        let mut pending = pending;
        pending.ability = ability;
        pending.activation_target_selection = ActivationTargetSelection::Settled;
        if let Some(waiting_for) = pay_activation_costs_after_target_selection(
            state,
            player,
            &pending,
            pending.ability.clone(),
        )? {
            return Ok(waiting_for);
        }

        return super::casting_costs::finish_target_selected_activated_ability_at_payment_boundary(
            state, player, pending, events,
        );
    }

    let cost = pending.cost.clone();
    finish_pending_cast_cost_or_pay(state, player, pending, ability, cost, events)
}

pub(crate) fn handle_choose_target(
    state: &mut GameState,
    // CR 601.2c + CR 115.1: the announcer of the slot just submitted (the
    // controller, or an opponent for an "of an opponent's choice" slot). The
    // dispatch layer already authorized this player against `WaitingFor.player`;
    // routing here re-derives the next slot's announcer and the completion
    // controller from the slots and the pending cast, so the inbound announcer is
    // not read again.
    _player: PlayerId,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (pending, target_slots, mode_labels, selection) = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            mode_labels,
            selection,
            ..
        } => (
            *pending_cast.clone(),
            target_slots.clone(),
            mode_labels.clone(),
            selection.clone(),
        ),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    match choose_target_for_ability(
        state,
        &pending.ability,
        &target_slots,
        &pending.target_constraints,
        &selection,
        target,
    )? {
        // CR 700.2: preserve the inbound mode labels unchanged — walking the
        // slots one at a time does not change the slot→mode mapping.
        TargetSelectionAdvance::InProgress(selection) => {
            // CR 601.2c + CR 115.1: the announcer for the NEXT slot may differ from
            // this slot's announcer ("of an opponent's choice"). Route the prompt to
            // that slot's `chooser`, defaulting to the spell/ability's controller.
            // This is the ONLY place the announcing player flips mid-walk.
            let next_player = target_slots
                .get(selection.current_slot)
                .and_then(|slot| slot.chooser)
                .unwrap_or(pending.ability.controller);
            Ok(WaitingFor::TargetSelection {
                player: next_player,
                pending_cast: Box::new(pending),
                target_slots,
                mode_labels,
                selection,
            })
        }
        TargetSelectionAdvance::Complete(selected_slots) => {
            let mut ability = pending.ability.clone();
            assign_selected_slots_in_chain(state, &mut ability, &selected_slots)?;
            // CR 115.1: regardless of who announced each slot, the spell/ability is
            // controlled, paid for, and put on the stack by its controller. Volcanic
            // Offering's final slot is opponent-chosen; without re-anchoring here the
            // inbound per-slot `player` (the opponent) would pay and stack the spell.
            let controller = pending.ability.controller;

            if let Some(waiting_for) =
                maybe_pause_for_cast_distribution(state, controller, &pending, &ability)?
            {
                return Ok(waiting_for);
            }

            if pending.activation_ability_index.is_some() {
                let mut pending = pending;
                pending.ability = ability;
                pending.activation_target_selection = ActivationTargetSelection::Settled;
                if let Some(waiting_for) = pay_activation_costs_after_target_selection(
                    state,
                    controller,
                    &pending,
                    pending.ability.clone(),
                )? {
                    return Ok(waiting_for);
                }

                let waiting_for =
                    super::casting_costs::finish_target_selected_activated_ability_at_payment_boundary(
                        state, controller, pending, events,
                    )?;
                return Ok(drain_deferred_triggers_after_stack_object_announcement(
                    state,
                    events,
                    waiting_for,
                ));
            }

            let cost = pending.cost.clone();
            finish_pending_cast_cost_or_pay(state, controller, pending, ability, cost, events)
        }
    }
}

fn pay_activation_costs_after_target_selection(
    state: &mut GameState,
    player: PlayerId,
    pending: &PendingCast,
    assigned_ability: ResolvedAbility,
) -> Result<Option<WaitingFor>, EngineError> {
    if let Some(ref activation_cost) = pending.activation_cost {
        if let Some((count, zone, filter)) = super::casting::find_non_self_exile(activation_cost) {
            let narrow_zone = ExileCostSourceZone::try_from_zone(zone)
                .expect("find_non_self_exile restricts zone to Hand or Graveyard");
            let eligible = super::casting::find_eligible_exile_for_cost_targets(
                state,
                player,
                pending.object_id,
                narrow_zone,
                filter,
            );
            if eligible.len() < count as usize {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible cards to exile".into(),
                ));
            }
            let mut pending = pending.clone();
            pending.ability = assigned_ability;
            return Ok(Some(WaitingFor::PayCost {
                player,
                kind: PayCostKind::ExileFromZone { zone: narrow_zone },
                choices: eligible,
                count: count as usize,
                min_count: 0,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            }));
        }

        // CR 701.3d + CR 601.2c/601.2h: a targeted "unattach a matching
        // attachment from ~" ability chooses its targets first, then surfaces the
        // Unattach cost interactively (Captain America's Throw). The chosen
        // Equipment's mana value drives the divided damage (CR 202.3 + CR 608.2k),
        // so eligibility narrows to attachments whose mana value is at least the
        // announced target count (CR 601.2c). Modeled on the non-self exile detour.
        if let Some((count, filter)) = super::casting::find_unattach_from_cost(activation_cost) {
            let n = distribution_targets(&assigned_ability).len() as u32;
            let eligible = super::casting::find_eligible_unattach_for_cost_targets(
                state,
                player,
                pending.object_id,
                filter,
                n,
            );
            if eligible.is_empty() {
                // CR 733 / CR 601.2h: unpayable cost — nothing has been detached
                // (pre-commit revert), so the activation is simply illegal.
                return Err(EngineError::ActionNotAllowed(
                    "No eligible Equipment to unattach with mana value >= target count".into(),
                ));
            }
            let mut pending = pending.clone();
            pending.ability = assigned_ability;
            // CR 601.2h: unattach-from is a required cost — no decline, exactly
            // `count` selections.
            return Ok(Some(WaitingFor::PayCost {
                player,
                kind: PayCostKind::UnattachFrom {
                    filter: filter.clone(),
                },
                choices: eligible,
                count: count as usize,
                min_count: count as usize,
                resume: CostResume::Spell {
                    spell: Box::new(pending),
                },
            }));
        }
    }

    Ok(None)
}

/// CR 602.2b + CR 605.3b + CR 616.1: Resume an automatic activation mana leg
/// through the same target-first cost suffix that owns chosen targets,
/// distribution, and interactive non-mana costs. The mana payment is already
/// settled, so its root is marked `NoCost` before the suffix is continued.
pub(crate) fn finish_activation_after_automatic_mana_payment(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if pending.activation_ability_index.is_none() {
        return Err(EngineError::InvalidAction(
            "automatic activation mana finalization missing an ability index".to_string(),
        ));
    }
    if matches!(
        pending.activation_target_selection,
        ActivationTargetSelection::Settled
    ) {
        let ability = pending.ability.clone();
        if let Some(waiting) =
            pay_activation_costs_after_target_selection(state, player, &pending, ability)?
        {
            return Ok(waiting);
        }
    }

    super::casting_costs::finish_activated_ability_at_payment_boundary(
        state, player, pending, events,
    )
}

/// CR 702.172a + CR 601.2f + CR 702.42a: Compose a modal spell's total cost.
///
/// Sums the base cost with any Spree mode costs and, when all modes are chosen, the entwine
/// cost. Because `restrictions::add_mana_cost` treats zero/`NoCost` as identity, a base of
/// `ManaCost::zero()` (from a cast-without-paying permission) yields exactly the additional
/// costs — never waiving them.
pub(crate) fn compute_modal_total_cost(
    base: &ManaCost,
    modal: &ModalChoice,
    indices: &[usize],
) -> ManaCost {
    let mut total = if modal.mode_costs.is_empty() {
        base.clone()
    } else {
        let spree_total = indices.iter().fold(ManaCost::zero(), |acc, &idx| {
            restrictions::add_mana_cost(&acc, &modal.mode_costs[idx])
        });
        restrictions::add_mana_cost(base, &spree_total)
    };

    // CR 702.42a: Entwine — add entwine cost when all modes are chosen.
    if indices.len() == modal.mode_count {
        if let Some(ref entwine_cost) = modal.entwine_cost {
            total = restrictions::add_mana_cost(&total, entwine_cost);
        }
    }

    total
}

fn escalate_cost_for_selected_modes(
    state: &GameState,
    player: PlayerId,
    pending: &PendingCast,
    selected_mode_count: usize,
) -> Option<AbilityCost> {
    let additional_modes = selected_mode_count.checked_sub(1)?;
    if additional_modes == 0 {
        return None;
    }

    // CR 702.120a + CR 702.102b: Reads the spell's own Escalate keyword. Left on the
    // marker-default (non-fuse-aware) `effective_spell_keywords` deliberately: no
    // real split card carries Escalate, and the only fuse-sensitive input is a
    // `CastWithKeyword` `affected` filter keyed on the combined mana value / colors
    // — a class that does not arise for Escalate. If a fused split spell were ever
    // granted Escalate by a value-keyed static, this would need the `_for` variant.
    let cost = super::casting::effective_spell_keywords(state, player, pending.object_id)
        .into_iter()
        .find_map(|keyword| match keyword {
            Keyword::Escalate(cost) => Some(cost),
            _ => None,
        })?;

    Some(repeat_escalate_cost(cost, additional_modes))
}

fn repeat_escalate_cost(cost: AbilityCost, count: usize) -> AbilityCost {
    if count == 1 {
        cost
    } else {
        AbilityCost::Composite {
            costs: vec![cost; count],
        }
    }
}

/// CR 601.2d: Extract a fixed distribution total from an effect's amount field.
/// Returns `None` if the amount depends on X or other runtime values (deferred to post-payment).
pub(super) fn extract_fixed_distribution_total(effect: &Effect) -> Option<u32> {
    match effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            ..
        } => Some(*value as u32),
        Effect::PutCounter {
            count: QuantityExpr::Fixed { value },
            ..
        } => Some(*value as u32),
        _ => None,
    }
}

/// CR 601.2d + CR 603.3d: Resolve the distribution pool for damage/counter division.
pub(super) fn extract_distribution_total(
    state: &GameState,
    ability: &ResolvedAbility,
    effect: &Effect,
) -> Option<u32> {
    if let Some(fixed) = extract_fixed_distribution_total(effect) {
        return Some(fixed);
    }
    let count_expr = match effect {
        Effect::DealDamage { amount, .. } => amount,
        Effect::PutCounter { count, .. } => count,
        _ => return None,
    };
    let (inner, _) = count_expr.peel_up_to();
    let total = super::quantity::resolve_quantity_with_targets(state, inner, ability).max(0) as u32;
    (total > 0).then_some(total)
}

/// CR 702.142b + CR 702.177a: If the activated ability at `ability_index` on
/// the source object has a keyword ability tag, emit the matching activation
/// event so "whenever you activate a [keyword] ability" triggers can see it.
pub(crate) fn emit_keyword_ability_event_if_tagged(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let Some(def) = state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.abilities.get(ability_index))
    else {
        return;
    };
    if let Some(ability_tag) = def.ability_tag {
        // CR 702.29c: Cycling does not use the generic `KeywordAbilityActivated`
        // path — activating it emits a dedicated `GameEvent::Cycled` so "When you
        // cycle this card" triggers fire. The card has already been discarded to
        // the graveyard as the cycling cost (the zone the trigger fires from).
        // The cost also emitted a `Discarded` event, so "whenever you discard"
        // and "cycle or discard" (CR 702.29d, matched on `Discarded`) still fire
        // exactly once.
        if ability_tag == AbilityTag::Cycling {
            events.push(GameEvent::Cycled {
                player_id: player,
                object_id: source_id,
            });
            return;
        }
        let is_mana_ability =
            ability_tag == AbilityTag::Exhaust && super::mana_abilities::is_mana_ability(def);
        events.push(GameEvent::KeywordAbilityActivated {
            ability_tag,
            player_id: player,
            source_id,
            is_mana_ability,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::mana::ManaCost;

    fn spree_modal(mode_costs: Vec<ManaCost>) -> ModalChoice {
        ModalChoice {
            min_choices: 1,
            max_choices: mode_costs.len(),
            mode_count: mode_costs.len(),
            mode_costs,
            ..ModalChoice::default()
        }
    }

    /// CR 702.172a + CR 601.2f: Spree mode costs are additional costs that survive a
    /// cast-without-paying permission (zero base cost).
    #[test]
    fn spree_mode_cost_survives_cast_without_paying() {
        let modal = spree_modal(vec![ManaCost::generic(1), ManaCost::generic(2)]);
        let base = ManaCost::zero();

        // One mode selected (cost {1}) → total = {1}.
        assert_eq!(
            compute_modal_total_cost(&base, &modal, &[0]),
            ManaCost::generic(1),
        );

        // Both modes selected ({1} + {2}) → total = {3}.
        assert_eq!(
            compute_modal_total_cost(&base, &modal, &[0, 1]),
            ManaCost::generic(3),
        );
    }

    /// Sanity: with a normal (non-zero) base, mode costs add to the base.
    #[test]
    fn spree_mode_cost_pays_full_amount_with_normal_base_cost() {
        let modal = spree_modal(vec![ManaCost::generic(1), ManaCost::generic(2)]);
        let base = ManaCost::generic(2);

        // Base {2} + mode {1} → total = {3}.
        assert_eq!(
            compute_modal_total_cost(&base, &modal, &[0]),
            ManaCost::generic(3),
        );

        // Base {2} + both modes ({1} + {2}) → total = {5}.
        assert_eq!(
            compute_modal_total_cost(&base, &modal, &[0, 1]),
            ManaCost::generic(5),
        );
    }

    /// CR 702.42a: Entwine cost applies when all modes are chosen and is preserved
    /// through a zero-base cast-without-paying path.
    #[test]
    fn entwine_cost_survives_cast_without_paying_when_all_modes_chosen() {
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 2,
            mode_count: 2,
            entwine_cost: Some(ManaCost::generic(2)),
            ..ModalChoice::default()
        };
        let base = ManaCost::zero();

        // One of two modes: entwine does NOT apply → total = {0}.
        assert_eq!(
            compute_modal_total_cost(&base, &modal, &[0]),
            ManaCost::zero(),
        );

        // Both modes: entwine applies → total = {2}.
        assert_eq!(
            compute_modal_total_cost(&base, &modal, &[0, 1]),
            ManaCost::generic(2),
        );
    }

    /// CR 702.120a: Escalate cost is paid once per mode chosen beyond the first.
    /// Single repetition returns the cost unwrapped; multi repetition wraps in
    /// `Composite` so each repeat is paid sequentially.
    #[test]
    fn repeat_escalate_cost_wraps_in_composite_for_multiple_extra_modes() {
        let cost = AbilityCost::Mana {
            cost: ManaCost::generic(1),
        };

        // One extra mode (2 modes selected): no Composite wrapper.
        assert!(matches!(
            repeat_escalate_cost(cost.clone(), 1),
            AbilityCost::Mana { .. }
        ));

        // Two extra modes (3 modes selected): Composite with two clones.
        match repeat_escalate_cost(cost.clone(), 2) {
            AbilityCost::Composite { costs } => assert_eq!(costs.len(), 2),
            other => panic!("expected Composite, got {other:?}"),
        }

        // Three extra modes (4 modes selected): Composite with three clones.
        match repeat_escalate_cost(cost, 3) {
            AbilityCost::Composite { costs } => assert_eq!(costs.len(), 3),
            other => panic!("expected Composite, got {other:?}"),
        }
    }
}
