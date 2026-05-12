use crate::types::ability::{
    AbilityCost, AbilityTag, AdditionalCost, Effect, ModalChoice, QuantityExpr, TargetRef,
    TargetSelectionMode,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, auto_select_targets_for_ability,
    begin_target_selection_for_ability, build_chained_resolved, build_target_slots,
    choose_target_for_ability, flatten_targets_in_chain, random_select_targets_for_ability,
    validate_modal_indices, validate_selected_targets_for_ability, TargetSelectionAdvance,
};
use super::casting::{emit_targeting_events, pay_ability_cost};
use super::casting_costs::finish_pending_cast_cost_or_pay;
use super::engine::EngineError;
use super::restrictions;
use super::stack;

/// Handle mode selection for a modal spell.
///
/// Combines chosen mode abilities into a single ResolvedAbility chain (sub_abilities),
/// then proceeds to targeting or directly to payment.
pub(crate) fn handle_select_modes(
    state: &mut GameState,
    player: PlayerId,
    indices: Vec<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (modal, pending) = match &state.waiting_for {
        WaitingFor::ModeChoice {
            modal,
            pending_cast,
            ..
        } => (modal.clone(), *pending_cast.clone()),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for mode selection".to_string(),
            ));
        }
    };

    // Spells resolve once — no cross-resolution mode constraints apply.
    validate_modal_indices(&modal, &indices, &[])?;

    // CR 702.172a + CR 601.2f: Spree mode costs (and entwine, CR 702.42a) are additional
    // costs layered on top of the base cost. `restrictions::add_mana_cost` treats `NoCost`/
    // zero as identity, so a cast-without-paying path (`pending.cost == zero`) yields exactly
    // the additional costs — alternative-cost permissions never waive them.
    let total_cost = compute_modal_total_cost(&pending.cost, &modal, &indices);
    let mut pending = pending;
    if let Some(cost) = escalate_cost_for_selected_modes(state, player, &pending, indices.len()) {
        pending.additional_cost_flow = Some(AdditionalCost::Required(cost));
    }

    // Get the card's abilities to build combined resolved ability from chosen modes
    let obj = state
        .objects
        .get(&pending.object_id)
        .ok_or_else(|| EngineError::InvalidAction("Modal spell object not found".to_string()))?;
    let abilities = obj.abilities.clone();

    // Build a chain of ResolvedAbility from chosen modes (in order)
    let mut resolved = build_chained_resolved(&abilities, &indices, pending.object_id, player)?;
    resolved.set_context_recursive(pending.ability.context.clone());

    // Check for targeting on the combined ability
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let target_slots = build_target_slots(state, &resolved)?;
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
                state, player, pending, resolved, total_cost, events,
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
                state, player, pending, resolved, total_cost, events,
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
        pending_sel.target_constraints = pending.target_constraints;
        pending_sel.casting_variant = pending.casting_variant;
        pending_sel.origin_zone = pending.origin_zone;
        pending_sel.additional_cost_flow = pending.additional_cost_flow;
        pending_sel.deferred_target_selection = pending.deferred_target_selection;
        pending_sel.declared_kickers_to_pay = pending.declared_kickers_to_pay;
        pending_sel.declined_kickers = pending.declined_kickers;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_sel),
            target_slots,
            selection,
        });
    }

    // No targets needed -- check additional cost, then pay
    finish_pending_cast_cost_or_pay(state, player, pending, resolved, total_cost, events)
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

    // CR 601.2d: If this spell requires distribution among targets, trigger
    // WaitingFor::DistributeAmong. For non-X spells, extract the fixed total now.
    // For X-spells, distribution is deferred to after mana payment (engine.rs).
    if let Some(ref unit) = pending.distribute {
        if let Some(total) = extract_fixed_distribution_total(&ability.effect) {
            let assigned_targets = flatten_targets_in_chain(&ability);
            // Store ability + targets on pending_cast for post-distribution resumption.
            let mut pending_dist = PendingCast::new(
                pending.object_id,
                pending.card_id,
                ability,
                pending.cost.clone(),
            );
            pending_dist.casting_variant = pending.casting_variant;
            pending_dist.distribute = Some(unit.clone());
            pending_dist.origin_zone = pending.origin_zone;
            pending_dist.additional_cost_flow = pending.additional_cost_flow.clone();
            pending_dist.deferred_target_selection = pending.deferred_target_selection;
            pending_dist.declared_kickers_to_pay = pending.declared_kickers_to_pay.clone();
            pending_dist.declined_kickers = pending.declined_kickers.clone();
            state.pending_cast = Some(Box::new(pending_dist));
            return Ok(WaitingFor::DistributeAmong {
                player,
                total,
                targets: assigned_targets,
                unit: unit.clone(),
            });
        }
        // X-spell: distribution deferred to after mana payment.
        // Propagate distribute flag through to pending_cast for the
        // (ManaPayment, PassPriority) handler.
    }

    if let Some(ability_index) = pending.activation_ability_index {
        if let Some(ref activation_cost) = pending.activation_cost {
            pay_ability_cost(state, player, pending.object_id, activation_cost, events)?;
        }

        let assigned_targets = flatten_targets_in_chain(&ability);
        emit_targeting_events(state, &assigned_targets, pending.object_id, player, events);

        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking.
        let mut ability = ability;
        ability.ability_index = Some(ability_index);
        stack::push_to_stack(
            state,
            StackEntry {
                id: entry_id,
                source_id: pending.object_id,
                controller: player,
                kind: StackEntryKind::ActivatedAbility {
                    source_id: pending.object_id,
                    ability,
                },
            },
            events,
        );

        restrictions::record_ability_activation(state, pending.object_id, ability_index);
        // CR 117.1b: Priority permits unbounded activation. `pending_activations`
        // is a per-priority-window AI-guard — see `GameState::pending_activations`.
        state
            .pending_activations
            .push((pending.object_id, ability_index));
        events.push(GameEvent::AbilityActivated {
            source_id: pending.object_id,
        });
        // CR 702.142b: Emit additional event when a boast ability is activated.
        emit_boast_event_if_tagged(state, pending.object_id, ability_index, player, events);
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        return Ok(WaitingFor::Priority { player });
    }

    let cost = pending.cost.clone();
    finish_pending_cast_cost_or_pay(state, player, pending, ability, cost, events)
}

pub(crate) fn handle_choose_target(
    state: &mut GameState,
    player: PlayerId,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (pending, target_slots, selection) = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            selection,
            ..
        } => (
            *pending_cast.clone(),
            target_slots.clone(),
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
        TargetSelectionAdvance::InProgress(selection) => Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending),
            target_slots,
            selection,
        }),
        TargetSelectionAdvance::Complete(selected_slots) => {
            let mut ability = pending.ability.clone();
            assign_selected_slots_in_chain(&mut ability, &selected_slots)?;

            if let Some(ability_index) = pending.activation_ability_index {
                if let Some(ref activation_cost) = pending.activation_cost {
                    pay_ability_cost(state, player, pending.object_id, activation_cost, events)?;
                }

                let assigned_targets = flatten_targets_in_chain(&ability);
                emit_targeting_events(state, &assigned_targets, pending.object_id, player, events);

                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking.
                let mut ability = ability;
                ability.ability_index = Some(ability_index);
                stack::push_to_stack(
                    state,
                    StackEntry {
                        id: entry_id,
                        source_id: pending.object_id,
                        controller: player,
                        kind: StackEntryKind::ActivatedAbility {
                            source_id: pending.object_id,
                            ability,
                        },
                    },
                    events,
                );

                restrictions::record_ability_activation(state, pending.object_id, ability_index);
                // CR 117.1b: Priority permits unbounded activation.
                // `pending_activations` is a per-priority-window AI-guard — see
                // `GameState::pending_activations`.
                state
                    .pending_activations
                    .push((pending.object_id, ability_index));
                events.push(GameEvent::AbilityActivated {
                    source_id: pending.object_id,
                });
                // CR 702.142b: Emit additional event when a boast ability is activated.
                emit_boast_event_if_tagged(state, pending.object_id, ability_index, player, events);
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                return Ok(WaitingFor::Priority { player });
            }

            let cost = pending.cost.clone();
            finish_pending_cast_cost_or_pay(state, player, pending, ability, cost, events)
        }
    }
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
fn extract_fixed_distribution_total(effect: &Effect) -> Option<u32> {
    match effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            ..
        } => Some(*value as u32),
        Effect::PutCounter {
            count: QuantityExpr::Fixed { value },
            ..
        }
        | Effect::AddCounter {
            count: QuantityExpr::Fixed { value },
            ..
        } => Some(*value as u32),
        _ => None,
    }
}

/// CR 702.142b: If the activated ability at `ability_index` on the source object
/// has `ability_tag == Some(AbilityTag::Boast)`, emit a `BoastAbilityActivated` event.
pub(crate) fn emit_boast_event_if_tagged(
    state: &GameState,
    source_id: ObjectId,
    ability_index: usize,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let is_boast = state
        .objects
        .get(&source_id)
        .and_then(|obj| obj.abilities.get(ability_index))
        .is_some_and(|def| def.ability_tag == Some(AbilityTag::Boast));
    if is_boast {
        events.push(GameEvent::BoastAbilityActivated {
            player_id: player,
            source_id,
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
