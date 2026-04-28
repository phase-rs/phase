use crate::types::ability::{
    AbilityCondition, AbilityDefinition, ControllerRef, Effect, ModalChoice,
    ModalSelectionConstraint, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use crate::types::game_state::{
    GameState, TargetSelectionConstraint, TargetSelectionProgress, TargetSelectionSlot,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

use super::engine::EngineError;
use super::targeting;
use super::triggers;

/// CR 113.1a: Build a resolved ability from its definition, preserving sub-ability chains,
/// conditions, durations, and targeting configuration.
pub fn build_resolved_from_def(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    let mut resolved =
        ResolvedAbility::new(*def.effect.clone(), Vec::new(), source_id, controller).kind(def.kind);
    if let Some(sub) = &def.sub_ability {
        resolved = resolved.sub_ability(build_resolved_from_def(sub, source_id, controller));
    }
    if let Some(else_ab) = &def.else_ability {
        resolved.else_ability = Some(Box::new(build_resolved_from_def(
            else_ab, source_id, controller,
        )));
    }
    if let Some(duration) = def.duration.clone() {
        resolved = resolved.duration(duration);
    }
    if let Some(condition) = def.condition.clone() {
        resolved = resolved.condition(condition);
    }
    resolved.optional_targeting = def.optional_targeting;
    resolved.optional = def.optional;
    resolved.optional_for = def.optional_for;
    resolved.multi_target = def.multi_target.clone();
    resolved.repeat_for = def.repeat_for.clone();
    resolved.description = def.description.clone();
    resolved.forward_result = def.forward_result;
    resolved.player_scope = def.player_scope;
    resolved
}

/// CR 700.2: For modal spells/abilities, build a chained resolved ability from the
/// selected mode indices, linking them via the sub_ability chain.
///
/// CR 608.2c: "The controller of the spell or ability follows its instructions
/// in the order written." For modes chosen from a "Choose one or more —" /
/// "Choose up to N —" list, the printed (source) order is the ascending
/// ordering of the mode indices — independent of the order the player
/// announced them in. We sort the input indices here so the resulting
/// sub_ability chain always resolves in printed order. Duplicate indices are
/// preserved (CR 700.2d: "You may choose the same mode more than once"
/// repeats the mode in sequence).
pub fn build_chained_resolved(
    abilities: &[AbilityDefinition],
    indices: &[usize],
    source_id: ObjectId,
    controller: PlayerId,
) -> Result<ResolvedAbility, EngineError> {
    if indices.is_empty() {
        return Err(EngineError::InvalidAction("No modes selected".to_string()));
    }

    let mut ordered: Vec<usize> = indices.to_vec();
    ordered.sort();

    let mut result: Option<ResolvedAbility> = None;
    for &idx in ordered.iter().rev() {
        let def = abilities
            .get(idx)
            .ok_or_else(|| EngineError::InvalidAction(format!("Mode index {idx} out of range")))?;
        let mut resolved = build_resolved_from_def(def, source_id, controller);
        // CR 700.2d: When chaining multiple modes, append subsequent modes after
        // the current mode's own sub_ability chain (e.g., Cathartic Pyre mode 2's
        // "discard, then draw that many" must preserve the draw sub_ability).
        if let Some(next_mode) = result {
            append_to_sub_chain(&mut resolved, next_mode);
        }
        result = Some(resolved);
    }

    result.ok_or_else(|| EngineError::InvalidAction("No modes selected".to_string()))
}

/// Append `next` to the tail of `ability`'s sub_ability chain.
pub(crate) fn append_to_sub_chain(ability: &mut ResolvedAbility, next: ResolvedAbility) {
    let mut node = ability;
    while node.sub_ability.is_some() {
        node = node.sub_ability.as_mut().unwrap().as_mut();
    }
    node.sub_ability = Some(Box::new(next));
}

pub fn find_first_target_filter_in_chain(ability: &ResolvedAbility) -> Option<&TargetFilter> {
    if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
        return Some(filter);
    }
    ability
        .sub_ability
        .as_deref()
        .and_then(find_first_target_filter_in_chain)
}

/// CR 601.2c / CR 602.2b: Collect all target slots for an ability chain. Each targeting
/// effect in the chain produces a slot whose legal targets are computed from the game state.
pub fn build_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Result<Vec<TargetSelectionSlot>, EngineError> {
    let mut slots = Vec::new();
    collect_target_slots(state, ability, &mut slots)?;
    Ok(slots)
}

pub fn target_constraints_from_modal(modal: &ModalChoice) -> Vec<TargetSelectionConstraint> {
    modal
        .constraints
        .iter()
        .filter_map(|constraint| match constraint {
            ModalSelectionConstraint::DifferentTargetPlayers => {
                Some(TargetSelectionConstraint::DifferentTargetPlayers)
            }
            // NoRepeatThisTurn/NoRepeatThisGame are mode-selection constraints, not target constraints.
            _ => None,
        })
        .collect()
}

/// Returns mode indices unavailable due to NoRepeatThisTurn/NoRepeatThisGame constraints.
/// CR 700.2: Checks per-turn and per-game tracking maps for previously chosen modes.
pub fn compute_unavailable_modes(
    state: &GameState,
    source_id: ObjectId,
    modal: &ModalChoice,
) -> Vec<usize> {
    let mut unavailable = Vec::new();
    for constraint in &modal.constraints {
        match constraint {
            ModalSelectionConstraint::NoRepeatThisTurn => {
                for mode_idx in 0..modal.mode_count {
                    if state
                        .modal_modes_chosen_this_turn
                        .contains(&(source_id, mode_idx))
                    {
                        unavailable.push(mode_idx);
                    }
                }
            }
            ModalSelectionConstraint::NoRepeatThisGame => {
                for mode_idx in 0..modal.mode_count {
                    if state
                        .modal_modes_chosen_this_game
                        .contains(&(source_id, mode_idx))
                    {
                        unavailable.push(mode_idx);
                    }
                }
            }
            _ => {} // Other constraints (e.g. DifferentTargetPlayers) are handled elsewhere
        }
    }
    unavailable.sort_unstable();
    unavailable.dedup();
    unavailable
}

/// Records chosen mode indices for NoRepeat constraint enforcement.
/// CR 700.2: Inserts into per-turn and/or per-game tracking maps.
pub fn record_modal_mode_choices(
    state: &mut GameState,
    source_id: ObjectId,
    modal: &ModalChoice,
    indices: &[usize],
) {
    for constraint in &modal.constraints {
        match constraint {
            ModalSelectionConstraint::NoRepeatThisTurn => {
                for &idx in indices {
                    state.modal_modes_chosen_this_turn.insert((source_id, idx));
                }
            }
            ModalSelectionConstraint::NoRepeatThisGame => {
                for &idx in indices {
                    state.modal_modes_chosen_this_game.insert((source_id, idx));
                }
            }
            _ => {}
        }
    }
}

pub enum TargetSelectionAdvance {
    InProgress(TargetSelectionProgress),
    Complete(Vec<Option<TargetRef>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetSlotSpec {
    filter: TargetFilter,
    optional: bool,
}

struct AbilityTargetingView<'a> {
    state: &'a GameState,
    ability: &'a ResolvedAbility,
    specs: &'a [TargetSlotSpec],
    target_slots: &'a [TargetSelectionSlot],
    constraints: &'a [TargetSelectionConstraint],
}

/// CR 601.2c: Begin target selection by computing legal targets for the first slot.
pub fn begin_target_selection(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<TargetSelectionProgress, EngineError> {
    build_target_selection_progress(target_slots, constraints, 0, Vec::new())
}

pub fn begin_target_selection_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<TargetSelectionProgress, EngineError> {
    build_target_selection_progress_for_ability(
        state,
        ability,
        target_slots,
        constraints,
        0,
        Vec::new(),
    )
}

/// CR 115.1: Targets are declared as part of putting a spell or ability on the stack.
/// CR 115.3: The same target can't be chosen multiple times for one instance of "target".
pub fn choose_target(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    progress: &TargetSelectionProgress,
    target: Option<TargetRef>,
) -> Result<TargetSelectionAdvance, EngineError> {
    if progress.current_slot >= target_slots.len() {
        return Err(EngineError::InvalidAction(
            "No target slot is currently active".to_string(),
        ));
    }
    if progress.selected_slots.len() != progress.current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }

    let slot = &target_slots[progress.current_slot];
    let mut selected_slots = progress.selected_slots.clone();
    match target {
        Some(target) => {
            if !progress.current_legal_targets.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Illegal target selected".to_string(),
                ));
            }
            selected_slots.push(Some(target));
        }
        None => {
            if !slot.optional {
                return Err(EngineError::InvalidAction(
                    "Cannot skip a required target".to_string(),
                ));
            }
            selected_slots.push(None);
        }
    }

    let next_slot = progress.current_slot + 1;
    if next_slot == target_slots.len() {
        validate_selected_slot_prefix(target_slots, &selected_slots, constraints)?;
        return Ok(TargetSelectionAdvance::Complete(selected_slots));
    }

    Ok(TargetSelectionAdvance::InProgress(
        build_target_selection_progress(target_slots, constraints, next_slot, selected_slots)?,
    ))
}

pub fn choose_target_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    progress: &TargetSelectionProgress,
    target: Option<TargetRef>,
) -> Result<TargetSelectionAdvance, EngineError> {
    if progress.current_slot >= target_slots.len() {
        return Err(EngineError::InvalidAction(
            "No target slot is currently active".to_string(),
        ));
    }
    if progress.selected_slots.len() != progress.current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }

    let slot = &target_slots[progress.current_slot];
    let mut selected_slots = progress.selected_slots.clone();
    match target {
        Some(target) => {
            if !progress.current_legal_targets.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Illegal target selected".to_string(),
                ));
            }
            selected_slots.push(Some(target));
        }
        None => {
            if !slot.optional {
                return Err(EngineError::InvalidAction(
                    "Cannot skip a required target".to_string(),
                ));
            }
            selected_slots.push(None);
        }
    }

    let next_slot = progress.current_slot + 1;
    if next_slot == target_slots.len() {
        validate_selected_slots_for_ability(
            state,
            ability,
            target_slots,
            &selected_slots,
            constraints,
        )?;
        return Ok(TargetSelectionAdvance::Complete(selected_slots));
    }

    Ok(TargetSelectionAdvance::InProgress(
        build_target_selection_progress_for_ability(
            state,
            ability,
            target_slots,
            constraints,
            next_slot,
            selected_slots,
        )?,
    ))
}

pub fn auto_select_targets(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<Option<Vec<TargetRef>>, EngineError> {
    let assignments = generate_target_assignments(target_slots, constraints);
    match assignments.as_slice() {
        [] => Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        )),
        [only] => Ok(Some(only.clone())),
        _ => Ok(None),
    }
}

pub fn auto_select_targets_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<Option<Vec<TargetRef>>, EngineError> {
    let assignments =
        build_target_assignments_for_ability(state, ability, target_slots, constraints);
    match assignments.as_slice() {
        [] => Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        )),
        [only] => Ok(Some(only.clone())),
        _ => Ok(None),
    }
}

/// CR 608.2b: When resolving, check that targets are still legal. If all targets are illegal,
/// the spell or ability doesn't resolve.
pub fn validate_selected_targets(
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let minimum_targets = target_slots.iter().filter(|slot| !slot.optional).count();
    if targets.len() < minimum_targets || targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(format!(
            "Expected between {minimum_targets} and {} targets, got {}",
            target_slots.len(),
            targets.len()
        )));
    }

    validate_target_prefix(target_slots, targets, constraints)
}

pub fn validate_selected_targets_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let minimum_targets = target_slots.iter().filter(|slot| !slot.optional).count();
    if targets.len() < minimum_targets || targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(format!(
            "Expected between {minimum_targets} and {} targets, got {}",
            target_slots.len(),
            targets.len()
        )));
    }

    validate_target_prefix_for_ability(state, ability, target_slots, targets, constraints)
}

fn validate_target_prefix(
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    for (index, target) in targets.iter().enumerate() {
        let Some(slot) = target_slots.get(index) else {
            return Err(EngineError::InvalidAction(
                "Too many targets selected".to_string(),
            ));
        };
        if !slot.legal_targets.contains(target) {
            return Err(EngineError::InvalidAction(
                "Illegal target selected".to_string(),
            ));
        }
    }

    validate_target_constraints(targets, constraints)
}

pub fn generate_target_assignments(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Vec<Vec<TargetRef>> {
    let mut current = Vec::with_capacity(target_slots.len());
    let mut out = Vec::new();
    build_target_assignments(target_slots, constraints, 0, &mut current, &mut out);
    out
}

/// CR 601.2c: Assign chosen targets to the correct effects in the ability chain.
pub fn assign_targets_in_chain(
    ability: &mut ResolvedAbility,
    targets: &[TargetRef],
) -> Result<(), EngineError> {
    if !chain_has_target_sink(ability) {
        ability.targets = targets.to_vec();
        return Ok(());
    }
    let mut next_target = 0usize;
    assign_targets_recursive(ability, targets, &mut next_target)?;
    if next_target != targets.len() {
        return Err(EngineError::InvalidAction(
            "Unused selected targets".to_string(),
        ));
    }
    Ok(())
}

pub fn assign_selected_slots_in_chain(
    ability: &mut ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
) -> Result<(), EngineError> {
    if !chain_has_target_sink(ability) {
        ability.targets = selected_slots.iter().flatten().cloned().collect();
        return Ok(());
    }
    let mut next_slot = 0usize;
    assign_selected_slots_recursive(ability, selected_slots, &mut next_slot)?;
    if next_slot != selected_slots.len() {
        return Err(EngineError::InvalidAction(
            "Unused selected target slots".to_string(),
        ));
    }
    Ok(())
}

pub fn flatten_targets_in_chain(ability: &ResolvedAbility) -> Vec<TargetRef> {
    let mut targets = ability.targets.clone();
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        targets.extend(flatten_targets_in_chain(sub_ability));
    }
    if let Some(else_ability) = ability.else_ability.as_deref() {
        targets.extend(flatten_targets_in_chain(else_ability));
    }
    targets
}

/// CR 608.2b: Re-validate targets on resolution — remove any that are no longer legal.
pub fn validate_targets_in_chain(state: &GameState, ability: &ResolvedAbility) -> ResolvedAbility {
    let mut validated = ability.clone();
    validated.targets = match triggers::extract_target_filter_from_effect(&validated.effect) {
        Some(filter) => targeting::validate_targets(
            state,
            &validated.targets,
            filter,
            validated.controller,
            validated.source_id,
        ),
        None => validated
            .targets
            .iter()
            .filter(|target| match target {
                TargetRef::Object(object_id) => state.battlefield.contains(object_id),
                TargetRef::Player(_) => true,
            })
            .cloned()
            .collect(),
    };
    if let Some(sub_ability) = validated.sub_ability.as_mut() {
        **sub_ability = validate_targets_in_chain(state, sub_ability);
    }
    if let Some(else_ability) = validated.else_ability.as_mut() {
        **else_ability = validate_targets_in_chain(state, else_ability);
    }
    validated
}

fn collect_target_slots(
    state: &GameState,
    ability: &ResolvedAbility,
    slots: &mut Vec<TargetSelectionSlot>,
) -> Result<(), EngineError> {
    // CR 701.12a: ExchangeControl carries two distinct per-slot filters. SelfRef
    // slots (e.g. "this artifact and target …") are filled by the resolver from
    // ability.source_id and don't require a player choice. Surface one slot per
    // non-SelfRef filter, in declaration order.
    if let Effect::ExchangeControl { target_a, target_b } = &ability.effect {
        for filter in [target_a, target_b] {
            if matches!(filter, TargetFilter::SelfRef) {
                continue;
            }
            let legal_targets = legal_targets_for_ability_filter(state, ability, filter, slots);
            if legal_targets.is_empty() && !ability.optional_targeting {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets available".to_string(),
                ));
            }
            slots.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
        return Ok(());
    }

    // CR 109.4 + CR 115.1: If the effect contains a filter referencing
    // `ControllerRef::TargetPlayer` (e.g. "each creature target player controls"
    // on `PutCounterAll`), surface a companion `TargetFilter::Player` slot
    // BEFORE the effect's primary filter slot. The chosen player is read back
    // at filter-evaluation time via `ability.targets`. Runs before the primary
    // filter so the player is chosen first (target declaration order matches
    // Oracle text order).
    if effect_references_target_player(&ability.effect) {
        let player_targets = targeting::find_legal_targets(
            state,
            &TargetFilter::Player,
            ability.controller,
            ability.source_id,
        );
        if player_targets.is_empty() && !ability.optional_targeting {
            return Err(EngineError::ActionNotAllowed(
                "No legal targets available".to_string(),
            ));
        }
        slots.push(TargetSelectionSlot {
            legal_targets: player_targets,
            optional: ability.optional_targeting,
        });
    }
    if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
        let legal_targets = legal_targets_for_ability_filter(state, ability, filter, slots);
        if legal_targets.is_empty() && !ability.optional_targeting {
            return Err(EngineError::ActionNotAllowed(
                "No legal targets available".to_string(),
            ));
        }
        if let Some(spec) = ability.multi_target.as_ref() {
            match spec.max {
                Some(max_targets) => {
                    for slot_index in 0..max_targets {
                        slots.push(TargetSelectionSlot {
                            legal_targets: legal_targets.clone(),
                            optional: slot_index >= spec.min,
                        });
                    }
                }
                // CR 115.1d: "any number" (unbounded max) requires a stop-on-None UI
                // flow not yet implemented; fall back to a single optional slot.
                None => slots.push(TargetSelectionSlot {
                    legal_targets,
                    optional: true,
                }),
            }
        } else {
            slots.push(TargetSelectionSlot {
                legal_targets,
                optional: ability.optional_targeting,
            });
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        return Ok(());
    }
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        // CR 603.12: Sub-abilities with reflexive trigger conditions (WhenYouDo,
        // QuantityCheck) represent deferred triggers whose targets are chosen at
        // resolution time, not when the parent ability goes on the stack.
        // Skip target pre-collection for these — they'll be handled during
        // resolve_ability_chain when the condition is evaluated.
        if !defers_conditional_target_selection(sub_ability) {
            collect_target_slots(state, sub_ability, slots)?;
        }
    }
    Ok(())
}

/// CR 109.4 + CR 115.1: Returns true if any filter inside `effect` references
/// `ControllerRef::TargetPlayer`. Used to surface a companion
/// `TargetFilter::Player` target slot for mass-placement effects like
/// `PutCounterAll { target: Typed { controller: TargetPlayer, .. } }`.
fn effect_references_target_player(effect: &Effect) -> bool {
    match effect.target_filter() {
        Some(f) if filter_references_target_player(f) => return true,
        _ => {}
    }
    // Also inspect mass-placement `target` fields that are NOT surfaced as
    // target slots (PutCounterAll, DestroyAll, PumpAll, DamageAll, etc. —
    // their `target_filter()` returns None because the field is a mass
    // filter, not a targeting filter).
    //
    // CR 115.1 + CR 404 + CR 406: A mass filter set to `TargetFilter::Player`
    // (e.g. `ChangeZoneAll { origin: Graveyard, target: Player }` for
    // "exile target player's graveyard" — Nihil Spellbomb, Bojuka Bog,
    // Tormod's Crypt class) parameterizes the scan by a player target. Surface
    // the companion player slot so the resolver's `player_scope` branch
    // reads the chosen target out of `ability.targets` instead of falling
    // back to the activator's own graveyard.
    match effect {
        Effect::PutCounterAll { target, .. }
        | Effect::DestroyAll { target, .. }
        | Effect::PumpAll { target, .. }
        | Effect::DamageAll { target, .. }
        | Effect::TapAll { target, .. }
        | Effect::UntapAll { target, .. }
        | Effect::BounceAll { target, .. }
        | Effect::CounterAll { target, .. }
        | Effect::ChangeZoneAll { target, .. }
        | Effect::DoublePTAll { target, .. } => {
            matches!(target, TargetFilter::Player) || filter_references_target_player(target)
        }
        _ => false,
    }
}

/// Tree-walks a `TargetFilter` and returns true if any `TypedFilter` inside
/// it has `controller == Some(ControllerRef::TargetPlayer)`.
fn filter_references_target_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(TypedFilter { controller, .. }) => {
            matches!(controller, Some(ControllerRef::TargetPlayer))
        }
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_references_target_player)
        }
        TargetFilter::Not { filter } => filter_references_target_player(filter),
        _ => false,
    }
}

fn collect_target_slot_specs(ability: &ResolvedAbility, specs: &mut Vec<TargetSlotSpec>) {
    // CR 701.12a: Mirror the ExchangeControl branch in `collect_target_slots`
    // so per-slot specs match the surfaced TargetSelectionSlots one-for-one
    // (SelfRef slots are auto-resolved and not surfaced).
    if let Effect::ExchangeControl { target_a, target_b } = &ability.effect {
        for filter in [target_a, target_b] {
            if matches!(filter, TargetFilter::SelfRef) {
                continue;
            }
            specs.push(TargetSlotSpec {
                filter: filter.clone(),
                optional: ability.optional_targeting,
            });
        }
        return;
    }

    // CR 109.4 + CR 115.1: Companion TargetFilter::Player slot surfaced by
    // `collect_target_slots` must have a matching spec here so subsequent
    // slot recomputation treats it correctly.
    if effect_references_target_player(&ability.effect) {
        specs.push(TargetSlotSpec {
            filter: TargetFilter::Player,
            optional: ability.optional_targeting,
        });
    }
    if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
        if let Some(spec) = ability.multi_target.as_ref() {
            match spec.max {
                Some(max_targets) => {
                    for slot_index in 0..max_targets {
                        specs.push(TargetSlotSpec {
                            filter: filter.clone(),
                            optional: slot_index >= spec.min,
                        });
                    }
                }
                None => specs.push(TargetSlotSpec {
                    filter: filter.clone(),
                    optional: true,
                }),
            }
        } else {
            specs.push(TargetSlotSpec {
                filter: filter.clone(),
                optional: ability.optional_targeting,
            });
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        return;
    }
    if let Some(sub_ability) = ability.sub_ability.as_deref() {
        if !defers_conditional_target_selection(sub_ability) {
            collect_target_slot_specs(sub_ability, specs);
        }
    }
}

fn legal_targets_for_ability_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    existing_slots: &[TargetSelectionSlot],
) -> Vec<TargetRef> {
    if let Some(targets) = damage_any_target_legal_targets(state, ability, filter) {
        return targets;
    }

    let relative_kind = relative_controller_kind(filter);
    if relative_kind.is_none() {
        return targeting::find_legal_targets(state, filter, ability.controller, ability.source_id);
    }

    let Some(player_slot) = existing_slots.iter().rev().find(|slot| {
        !slot.legal_targets.is_empty()
            && slot
                .legal_targets
                .iter()
                .all(|target| matches!(target, TargetRef::Player(_)))
    }) else {
        return targeting::find_legal_targets(state, filter, ability.controller, ability.source_id);
    };

    // CR 109.4 + CR 115.1: For each candidate from the companion player slot,
    // re-enumerate with the relative controller bound to that player. The
    // filter is rewritten to `ControllerRef::You` so `find_legal_targets`'s
    // existing source-controller plumbing handles per-player substitution
    // uniformly for both the `You` (per-player iteration) and `TargetPlayer`
    // (Karazikar-style attacked-player) cases.
    let enumeration_filter = match relative_kind {
        Some(crate::types::ability::ControllerRef::TargetPlayer) => rewrite_relative_controller(
            filter,
            crate::types::ability::ControllerRef::TargetPlayer,
            crate::types::ability::ControllerRef::You,
        ),
        _ => filter.clone(),
    };

    let mut legal_targets = Vec::new();
    for player_id in player_slot
        .legal_targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Player(player_id) => Some(*player_id),
            TargetRef::Object(_) => None,
        })
    {
        for target in
            targeting::find_legal_targets(state, &enumeration_filter, player_id, ability.source_id)
        {
            if !legal_targets.contains(&target) {
                legal_targets.push(target);
            }
        }
    }

    legal_targets
}

/// Returns the relative `ControllerRef` (`You` or `TargetPlayer`) embedded in
/// `filter`, if any. Used by `legal_targets_for_ability_filter` to detect
/// filters that need per-player re-enumeration against a companion player slot.
fn relative_controller_kind(filter: &TargetFilter) -> Option<crate::types::ability::ControllerRef> {
    use crate::types::ability::ControllerRef;
    match filter {
        TargetFilter::Typed(tf) => match tf.controller {
            Some(ControllerRef::You) => Some(ControllerRef::You),
            Some(ControllerRef::TargetPlayer) => Some(ControllerRef::TargetPlayer),
            _ => None,
        },
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().find_map(relative_controller_kind)
        }
        TargetFilter::Not { filter } => relative_controller_kind(filter),
        _ => None,
    }
}

/// True iff `filter` carries a `ControllerRef::You` binding requiring per-
/// player rebinding at target-resolution time. Thin wrapper over
/// `relative_controller_kind` for the `You`-specific call sites.
fn uses_relative_controller_you(filter: &TargetFilter) -> bool {
    matches!(
        relative_controller_kind(filter),
        Some(crate::types::ability::ControllerRef::You)
    )
}

/// Substitute every `from`-controller binding in `filter` with `to`. Used to
/// rewrite `TargetPlayer` → `You` so per-player enumeration through
/// `find_legal_targets`'s `source_controller` parameter works uniformly.
fn rewrite_relative_controller(
    filter: &TargetFilter,
    from: crate::types::ability::ControllerRef,
    to: crate::types::ability::ControllerRef,
) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) if tf.controller == Some(from.clone()) => {
            let mut new_tf = tf.clone();
            new_tf.controller = Some(to);
            TargetFilter::Typed(new_tf)
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .iter()
                .map(|f| rewrite_relative_controller(f, from.clone(), to.clone()))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .iter()
                .map(|f| rewrite_relative_controller(f, from.clone(), to.clone()))
                .collect(),
        },
        TargetFilter::Not { filter: inner } => TargetFilter::Not {
            filter: Box::new(rewrite_relative_controller(inner, from, to)),
        },
        other => other.clone(),
    }
}

fn target_slot_specs(ability: &ResolvedAbility) -> Vec<TargetSlotSpec> {
    let mut specs = Vec::new();
    collect_target_slot_specs(ability, &mut specs);
    specs
}

fn relative_filter_controller(
    ability: &ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
) -> PlayerId {
    selected_slots
        .iter()
        .rev()
        .find_map(|slot| match slot {
            Some(TargetRef::Player(player_id)) => Some(*player_id),
            Some(TargetRef::Object(_)) | None => None,
        })
        .unwrap_or(ability.controller)
}

fn legal_targets_for_selected_slot(
    state: &GameState,
    ability: &ResolvedAbility,
    spec: &TargetSlotSpec,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    if let Some(targets) = damage_any_target_legal_targets(state, ability, &spec.filter) {
        return targets;
    }

    let controller = if uses_relative_controller_you(&spec.filter) {
        relative_filter_controller(ability, selected_slots)
    } else {
        ability.controller
    };

    targeting::find_legal_targets(state, &spec.filter, controller, ability.source_id)
}

fn damage_any_target_legal_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<Vec<TargetRef>> {
    if !matches!(
        (&ability.effect, filter),
        (
            Effect::DealDamage {
                target: TargetFilter::Any,
                ..
            },
            TargetFilter::Any
        )
    ) {
        return None;
    }

    let player_targets = targeting::find_legal_targets(
        state,
        &TargetFilter::Player,
        ability.controller,
        ability.source_id,
    );
    let permanent_targets = targeting::find_legal_targets(
        state,
        &TargetFilter::Typed(TypedFilter::default().with_type(TypeFilter::AnyOf(vec![
            TypeFilter::Creature,
            TypeFilter::Planeswalker,
            TypeFilter::Battle,
        ]))),
        ability.controller,
        ability.source_id,
    );

    Some(
        player_targets
            .into_iter()
            .chain(permanent_targets)
            .collect(),
    )
}

/// CR 603.12: Check if a sub-ability represents a reflexive trigger whose targeting
/// should be deferred to resolution time. Reflexive trigger conditions (WhenYouDo,
/// QuantityCheck on CountersOnSelf) indicate the sub-ability fires as a separate
/// triggered ability during resolution — targets are chosen then, not at stack time.
fn defers_conditional_target_selection(sub: &ResolvedAbility) -> bool {
    matches!(
        &sub.condition,
        Some(AbilityCondition::WhenYouDo) | Some(AbilityCondition::QuantityCheck { .. })
    )
}

fn defers_sub_ability_target_selection(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Scry { .. }
            | Effect::Dig { .. }
            | Effect::Surveil { .. }
            | Effect::ChooseCard { .. }
            | Effect::SearchLibrary { .. }
            | Effect::RevealHand { .. }
            | Effect::Choose { .. }
    )
}

fn build_target_assignments(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    index: usize,
    current: &mut Vec<TargetRef>,
    out: &mut Vec<Vec<TargetRef>>,
) {
    if index == target_slots.len() {
        if validate_selected_targets(target_slots, current, constraints).is_ok() {
            out.push(current.clone());
        }
        return;
    }

    let slot = &target_slots[index];
    if slot.optional {
        build_target_assignments(target_slots, constraints, index + 1, current, out);
    }
    for target in &slot.legal_targets {
        current.push(target.clone());
        if validate_target_prefix(target_slots, current, constraints).is_ok() {
            build_target_assignments(target_slots, constraints, index + 1, current, out);
        }
        current.pop();
    }
}

fn build_target_assignments_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Vec<Vec<TargetRef>> {
    let specs = target_slot_specs(ability);
    let view = AbilityTargetingView {
        state,
        ability,
        specs: &specs,
        target_slots,
        constraints,
    };
    let mut current = Vec::with_capacity(target_slots.len());
    let mut out = Vec::new();
    build_target_assignments_with_specs(&view, 0, &mut current, &mut out);
    out
}

fn build_target_assignments_with_specs(
    view: &AbilityTargetingView<'_>,
    index: usize,
    current: &mut Vec<TargetRef>,
    out: &mut Vec<Vec<TargetRef>>,
) {
    if index == view.target_slots.len() {
        if validate_target_prefix_with_specs(
            view.state,
            view.ability,
            view.specs,
            view.target_slots,
            current,
            view.constraints,
        )
        .is_ok()
        {
            out.push(current.clone());
        }
        return;
    }

    let slot = &view.target_slots[index];
    if slot.optional {
        build_target_assignments_with_specs(view, index + 1, current, out);
    }

    let selected_slots: Vec<Option<TargetRef>> = current.iter().cloned().map(Some).collect();
    let legal_targets = legal_targets_for_spec_slot(
        view.state,
        view.ability,
        view.specs,
        view.target_slots,
        index,
        &selected_slots,
    );
    for target in legal_targets {
        current.push(target);
        if validate_target_prefix_with_specs(
            view.state,
            view.ability,
            view.specs,
            view.target_slots,
            current,
            view.constraints,
        )
        .is_ok()
        {
            build_target_assignments_with_specs(view, index + 1, current, out);
        }
        current.pop();
    }
}

fn build_target_selection_progress(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: Vec<Option<TargetRef>>,
) -> Result<TargetSelectionProgress, EngineError> {
    if current_slot > target_slots.len() || selected_slots.len() != current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }
    validate_selected_slot_prefix(target_slots, &selected_slots, constraints)?;

    if current_slot == target_slots.len() {
        return Ok(TargetSelectionProgress {
            current_slot,
            selected_slots,
            current_legal_targets: Vec::new(),
        });
    }

    let current_legal_targets =
        legal_targets_for_slot(target_slots, constraints, current_slot, &selected_slots);
    let slot = &target_slots[current_slot];
    let mut skipped_slots = selected_slots.clone();
    skipped_slots.push(None);
    let can_skip = slot.optional
        && has_legal_completion(target_slots, constraints, current_slot + 1, &skipped_slots);

    if current_legal_targets.is_empty() && !can_skip {
        return Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        ));
    }

    Ok(TargetSelectionProgress {
        current_slot,
        selected_slots,
        current_legal_targets,
    })
}

fn build_target_selection_progress_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: Vec<Option<TargetRef>>,
) -> Result<TargetSelectionProgress, EngineError> {
    if current_slot > target_slots.len() || selected_slots.len() != current_slot {
        return Err(EngineError::InvalidAction(
            "Target selection progress is out of sync".to_string(),
        ));
    }
    validate_selected_slots_for_ability(
        state,
        ability,
        target_slots,
        &selected_slots,
        constraints,
    )?;

    if current_slot == target_slots.len() {
        return Ok(TargetSelectionProgress {
            current_slot,
            selected_slots,
            current_legal_targets: Vec::new(),
        });
    }

    let specs = target_slot_specs(ability);
    let current_legal_targets = legal_targets_for_slot_with_specs(
        state,
        ability,
        &specs,
        target_slots,
        constraints,
        current_slot,
        &selected_slots,
    );
    let slot = &target_slots[current_slot];
    let mut skipped_slots = selected_slots.clone();
    skipped_slots.push(None);
    let can_skip = slot.optional
        && has_legal_completion_with_specs(
            state,
            ability,
            &specs,
            target_slots,
            constraints,
            current_slot + 1,
            &skipped_slots,
        );

    if current_legal_targets.is_empty() && !can_skip {
        return Err(EngineError::ActionNotAllowed(
            "No legal target combinations available".to_string(),
        ));
    }

    Ok(TargetSelectionProgress {
        current_slot,
        selected_slots,
        current_legal_targets,
    })
}

fn legal_targets_for_slot(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    let Some(slot) = target_slots.get(current_slot) else {
        return Vec::new();
    };

    slot.legal_targets
        .iter()
        .filter(|target| {
            let mut next_slots = selected_slots.to_vec();
            next_slots.push(Some((*target).clone()));
            validate_selected_slot_prefix(target_slots, &next_slots, constraints).is_ok()
                && has_legal_completion(target_slots, constraints, current_slot + 1, &next_slots)
        })
        .cloned()
        .collect()
}

fn has_legal_completion(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    index: usize,
    selected_slots: &[Option<TargetRef>],
) -> bool {
    if index == target_slots.len() {
        return validate_selected_slot_prefix(target_slots, selected_slots, constraints).is_ok();
    }

    let slot = &target_slots[index];
    if slot.optional {
        let mut skipped_slots = selected_slots.to_vec();
        skipped_slots.push(None);
        if has_legal_completion(target_slots, constraints, index + 1, &skipped_slots) {
            return true;
        }
    }

    slot.legal_targets.iter().any(|target| {
        let mut next_slots = selected_slots.to_vec();
        next_slots.push(Some(target.clone()));
        validate_selected_slot_prefix(target_slots, &next_slots, constraints).is_ok()
            && has_legal_completion(target_slots, constraints, index + 1, &next_slots)
    })
}

fn legal_targets_for_spec_slot(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    current_slot: usize,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    let Some(spec) = specs.get(current_slot) else {
        return target_slots
            .get(current_slot)
            .map(|slot| slot.legal_targets.clone())
            .unwrap_or_default();
    };
    legal_targets_for_selected_slot(state, ability, spec, selected_slots)
}

fn legal_targets_for_slot_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    current_slot: usize,
    selected_slots: &[Option<TargetRef>],
) -> Vec<TargetRef> {
    legal_targets_for_spec_slot(
        state,
        ability,
        specs,
        target_slots,
        current_slot,
        selected_slots,
    )
    .into_iter()
    .filter(|target| {
        let mut next_slots = selected_slots.to_vec();
        next_slots.push(Some(target.clone()));
        validate_selected_slots_with_specs(
            state,
            ability,
            specs,
            target_slots,
            &next_slots,
            constraints,
        )
        .is_ok()
            && has_legal_completion_with_specs(
                state,
                ability,
                specs,
                target_slots,
                constraints,
                current_slot + 1,
                &next_slots,
            )
    })
    .collect()
}

fn has_legal_completion_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
    index: usize,
    selected_slots: &[Option<TargetRef>],
) -> bool {
    if index == target_slots.len() {
        return validate_selected_slots_with_specs(
            state,
            ability,
            specs,
            target_slots,
            selected_slots,
            constraints,
        )
        .is_ok();
    }

    let slot = &target_slots[index];
    if slot.optional {
        let mut skipped_slots = selected_slots.to_vec();
        skipped_slots.push(None);
        if has_legal_completion_with_specs(
            state,
            ability,
            specs,
            target_slots,
            constraints,
            index + 1,
            &skipped_slots,
        ) {
            return true;
        }
    }

    legal_targets_for_spec_slot(state, ability, specs, target_slots, index, selected_slots)
        .into_iter()
        .any(|target| {
            let mut next_slots = selected_slots.to_vec();
            next_slots.push(Some(target));
            validate_selected_slots_with_specs(
                state,
                ability,
                specs,
                target_slots,
                &next_slots,
                constraints,
            )
            .is_ok()
                && has_legal_completion_with_specs(
                    state,
                    ability,
                    specs,
                    target_slots,
                    constraints,
                    index + 1,
                    &next_slots,
                )
        })
}

fn validate_selected_slot_prefix(
    target_slots: &[TargetSelectionSlot],
    selected_slots: &[Option<TargetRef>],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if selected_slots.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    let mut compact_targets = Vec::new();
    for (index, selected_slot) in selected_slots.iter().enumerate() {
        let Some(slot) = target_slots.get(index) else {
            return Err(EngineError::InvalidAction(
                "Too many targets selected".to_string(),
            ));
        };

        match selected_slot {
            Some(target) => {
                if !slot.legal_targets.contains(target) {
                    return Err(EngineError::InvalidAction(
                        "Illegal target selected".to_string(),
                    ));
                }
                compact_targets.push(target.clone());
            }
            None if slot.optional => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
    }

    validate_target_constraints(&compact_targets, constraints)
}

fn validate_target_prefix_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let specs = target_slot_specs(ability);
    validate_target_prefix_with_specs(state, ability, &specs, target_slots, targets, constraints)
}

fn validate_target_prefix_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if targets.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    let selected_slots: Vec<Option<TargetRef>> = targets.iter().cloned().map(Some).collect();
    validate_selected_slots_with_specs(
        state,
        ability,
        specs,
        target_slots,
        &selected_slots,
        constraints,
    )
}

fn validate_selected_slots_for_ability(
    state: &GameState,
    ability: &ResolvedAbility,
    target_slots: &[TargetSelectionSlot],
    selected_slots: &[Option<TargetRef>],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    let specs = target_slot_specs(ability);
    validate_selected_slots_with_specs(
        state,
        ability,
        &specs,
        target_slots,
        selected_slots,
        constraints,
    )
}

fn validate_selected_slots_with_specs(
    state: &GameState,
    ability: &ResolvedAbility,
    specs: &[TargetSlotSpec],
    target_slots: &[TargetSelectionSlot],
    selected_slots: &[Option<TargetRef>],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    if selected_slots.len() > target_slots.len() {
        return Err(EngineError::InvalidAction(
            "Too many targets selected".to_string(),
        ));
    }

    let mut compact_targets = Vec::new();
    for (index, selected_slot) in selected_slots.iter().enumerate() {
        let Some(slot) = target_slots.get(index) else {
            return Err(EngineError::InvalidAction(
                "Too many targets selected".to_string(),
            ));
        };

        let legal_targets = specs
            .get(index)
            .map(|spec| {
                legal_targets_for_selected_slot(state, ability, spec, &selected_slots[..index])
            })
            .unwrap_or_else(|| slot.legal_targets.clone());

        match selected_slot {
            Some(target) => {
                if !legal_targets.contains(target) {
                    return Err(EngineError::InvalidAction(
                        "Illegal target selected".to_string(),
                    ));
                }
                compact_targets.push(target.clone());
            }
            None if slot.optional => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
    }

    validate_target_constraints(&compact_targets, constraints)
}

fn assign_targets_recursive(
    ability: &mut ResolvedAbility,
    targets: &[TargetRef],
    next_target: &mut usize,
) -> Result<(), EngineError> {
    // CR 109.4 + CR 115.1: Mirror the companion-player slot pushed by
    // `collect_target_slots` for effects whose filters reference
    // `ControllerRef::TargetPlayer` (DamageAll, PutCounterAll, etc.). The
    // selected player must be written onto THIS node's `targets` so the
    // filter's `TargetPlayer` resolution at runtime (filter.rs) finds it.
    // Slot order matches `collect_target_slots`: player slot before primary.
    if effect_references_target_player(&ability.effect) {
        if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }
    if triggers::extract_target_filter_from_effect(&ability.effect).is_some() {
        if let Some(spec) = ability.multi_target.as_ref() {
            match spec.max {
                Some(max_targets) => {
                    let remaining_minimum = ability
                        .sub_ability
                        .as_deref()
                        .map(minimum_targets_in_chain)
                        .unwrap_or(0);
                    let remaining_after_current = targets.len().saturating_sub(*next_target);
                    let current_count = remaining_after_current.saturating_sub(remaining_minimum);
                    if current_count < spec.min || current_count > max_targets {
                        return Err(EngineError::InvalidAction(
                            "Incorrect number of multi-target selections".to_string(),
                        ));
                    }
                    // CR 109.4: Use `extend_from_slice` so a companion player target
                    // pushed by the `effect_references_target_player` branch above
                    // survives — both slots live on this node's `targets`.
                    ability
                        .targets
                        .extend_from_slice(&targets[*next_target..*next_target + current_count]);
                    *next_target += current_count;
                }
                // CR 115.1d: unbounded multi-target — fall through to single-target path.
                None => {
                    if let Some(target) = targets.get(*next_target) {
                        ability.targets.push(target.clone());
                        *next_target += 1;
                    } else if !ability.optional_targeting {
                        return Err(EngineError::InvalidAction(
                            "Missing required target".to_string(),
                        ));
                    }
                }
            }
        } else if let Some(target) = targets.get(*next_target) {
            ability.targets.push(target.clone());
            *next_target += 1;
        } else if !ability.optional_targeting {
            return Err(EngineError::InvalidAction(
                "Missing required target".to_string(),
            ));
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        return Ok(());
    }
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        assign_targets_recursive(sub_ability, targets, next_target)?;
    }
    Ok(())
}

fn assign_selected_slots_recursive(
    ability: &mut ResolvedAbility,
    selected_slots: &[Option<TargetRef>],
    next_slot: &mut usize,
) -> Result<(), EngineError> {
    // CR 109.4 + CR 115.1: Mirror the companion-player slot pushed by
    // `collect_target_slots` for `ControllerRef::TargetPlayer` filters
    // (DamageAll, PutCounterAll, etc.). See `assign_targets_recursive`.
    if effect_references_target_player(&ability.effect) {
        let Some(selected_slot) = selected_slots.get(*next_slot) else {
            return Err(EngineError::InvalidAction(
                "Missing target selection".to_string(),
            ));
        };
        match selected_slot {
            Some(target) => ability.targets.push(target.clone()),
            None if ability.optional_targeting => {}
            None => {
                return Err(EngineError::InvalidAction(
                    "Missing required target".to_string(),
                ));
            }
        }
        *next_slot += 1;
    }
    if triggers::extract_target_filter_from_effect(&ability.effect).is_some() {
        if let Some(spec) = ability.multi_target.as_ref() {
            match spec.max {
                Some(max_targets) => {
                    let end_slot = *next_slot + max_targets;
                    let Some(window) = selected_slots.get(*next_slot..end_slot) else {
                        return Err(EngineError::InvalidAction(
                            "Missing required target".to_string(),
                        ));
                    };
                    if window[..spec.min].iter().any(Option::is_none) {
                        return Err(EngineError::InvalidAction(
                            "Missing required target".to_string(),
                        ));
                    }
                    ability.targets.extend(window.iter().flatten().cloned());
                    *next_slot = end_slot;
                }
                // CR 115.1d: unbounded multi-target — fall through to single-target path.
                None => {
                    let Some(selected_slot) = selected_slots.get(*next_slot) else {
                        return Err(EngineError::InvalidAction(
                            "Missing target selection".to_string(),
                        ));
                    };
                    match selected_slot {
                        Some(target) => ability.targets.push(target.clone()),
                        None if ability.optional_targeting => {}
                        None => {
                            return Err(EngineError::InvalidAction(
                                "Missing required target".to_string(),
                            ));
                        }
                    }
                    *next_slot += 1;
                }
            }
        } else {
            let Some(selected_slot) = selected_slots.get(*next_slot) else {
                return Err(EngineError::InvalidAction(
                    "Missing target selection".to_string(),
                ));
            };

            match selected_slot {
                Some(target) => ability.targets.push(target.clone()),
                None if ability.optional_targeting => {}
                None => {
                    return Err(EngineError::InvalidAction(
                        "Missing required target".to_string(),
                    ));
                }
            }
            *next_slot += 1;
        }
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        return Ok(());
    }
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        assign_selected_slots_recursive(sub_ability, selected_slots, next_slot)?;
    }
    Ok(())
}

/// CR 115.3: Validate targeting constraints — e.g., different target players must be distinct.
fn validate_target_constraints(
    targets: &[TargetRef],
    constraints: &[TargetSelectionConstraint],
) -> Result<(), EngineError> {
    for constraint in constraints {
        match constraint {
            TargetSelectionConstraint::DifferentTargetPlayers => {
                let players = targets
                    .iter()
                    .filter_map(|target| match target {
                        TargetRef::Player(player) => Some(*player),
                        TargetRef::Object(_) => None,
                    })
                    .collect::<std::collections::HashSet<_>>();
                let player_target_count = targets
                    .iter()
                    .filter(|target| matches!(target, TargetRef::Player(_)))
                    .count();
                if players.len() != player_target_count {
                    return Err(EngineError::InvalidAction(
                        "Selected player targets must be different".to_string(),
                    ));
                }
            }
        }
    }

    Ok(())
}

fn chain_has_target_sink(ability: &ResolvedAbility) -> bool {
    // CR 109.4 + CR 115.1: A node also acts as a target sink when its filter
    // references `ControllerRef::TargetPlayer` (DamageAll, PutCounterAll,
    // etc.) — `collect_target_slots` pushes a companion player slot for it,
    // and `assign_targets_recursive` consumes one target into this node.
    if effect_references_target_player(&ability.effect) {
        return true;
    }
    if triggers::extract_target_filter_from_effect(&ability.effect).is_some() {
        return true;
    }
    if defers_sub_ability_target_selection(&ability.effect) {
        return false;
    }
    ability
        .sub_ability
        .as_deref()
        .is_some_and(chain_has_target_sink)
}

fn minimum_targets_in_chain(ability: &ResolvedAbility) -> usize {
    // CR 109.4: Companion player slot for `ControllerRef::TargetPlayer` filters
    // contributes one required slot (or zero when targeting is optional).
    let player_companion =
        if effect_references_target_player(&ability.effect) && !ability.optional_targeting {
            1
        } else {
            0
        };
    let current = if triggers::extract_target_filter_from_effect(&ability.effect).is_some() {
        if let Some(spec) = ability
            .multi_target
            .as_ref()
            .filter(|spec| spec.max.is_some())
        {
            spec.min
        } else if ability.optional_targeting {
            0
        } else {
            1
        }
    } else {
        0
    };
    let current = player_companion + current;

    let rest = if defers_sub_ability_target_selection(&ability.effect) {
        0
    } else {
        ability
            .sub_ability
            .as_deref()
            .map(minimum_targets_in_chain)
            .unwrap_or(0)
    };

    current + rest
}

/// CR 700.2a: The controller of a modal spell or activated ability chooses the mode(s)
/// as part of casting. If a mode would be illegal, it can't be chosen.
/// CR 700.2d: A player normally can't choose the same mode more than once.
pub fn validate_modal_indices(
    modal: &ModalChoice,
    indices: &[usize],
    unavailable_modes: &[usize],
) -> Result<(), EngineError> {
    if indices.len() < modal.min_choices || indices.len() > modal.max_choices {
        return Err(EngineError::InvalidAction(format!(
            "Must choose between {} and {} modes, got {}",
            modal.min_choices,
            modal.max_choices,
            indices.len()
        )));
    }

    let mut seen = std::collections::HashSet::new();
    for &idx in indices {
        if idx >= modal.mode_count {
            return Err(EngineError::InvalidAction(format!(
                "Mode index {idx} out of range ({})",
                modal.mode_count
            )));
        }
        if !modal.allow_repeat_modes && !seen.insert(idx) {
            return Err(EngineError::InvalidAction(format!(
                "Duplicate mode index {idx}"
            )));
        }
        // CR 700.2: Reject modes already chosen per NoRepeatThisTurn/NoRepeatThisGame.
        if unavailable_modes.contains(&idx) {
            return Err(EngineError::InvalidAction(format!(
                "Mode index {idx} is unavailable (already chosen)"
            )));
        }
    }

    Ok(())
}

/// CR 700.2d: Generate all valid mode selection sequences for a modal spell/ability.
pub fn generate_modal_index_sequences(modal: &ModalChoice) -> Vec<Vec<usize>> {
    let mut actions = Vec::new();
    for count in modal.min_choices..=modal.max_choices {
        let mut current = Vec::with_capacity(count);
        let start = if modal.allow_repeat_modes {
            0
        } else {
            usize::MAX
        };
        build_mode_sequences(
            modal.mode_count,
            count,
            start,
            modal.allow_repeat_modes,
            &mut current,
            &mut actions,
        );
    }
    actions
}

fn build_mode_sequences(
    mode_count: usize,
    remaining: usize,
    min_index: usize,
    allow_repeat: bool,
    current: &mut Vec<usize>,
    out: &mut Vec<Vec<usize>>,
) {
    if remaining == 0 {
        out.push(current.clone());
        return;
    }

    let start_index = if min_index == usize::MAX {
        0
    } else {
        min_index
    };
    for idx in start_index..mode_count {
        current.push(idx);
        build_mode_sequences(
            mode_count,
            remaining - 1,
            if allow_repeat { idx } else { idx + 1 },
            allow_repeat,
            current,
            out,
        );
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, Effect, ModalChoice, ModalSelectionConstraint, QuantityExpr, QuantityRef,
        TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{GameState, TargetSelectionConstraint, TargetSelectionSlot};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn build_chained_resolved_preserves_mode_sub_abilities() {
        // CR 700.2d: Cathartic Pyre mode 2 has "Discard up to two, then draw that many"
        // — the draw sub_ability must not be clobbered when chaining modes.
        let mode1 = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let mut mode2 = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Discard {
                count: QuantityExpr::up_to(QuantityExpr::Fixed { value: 2 }),
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
        );
        mode2.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
        )));

        let abilities = vec![mode1, mode2];

        // Single mode: mode 2 only
        let resolved = build_chained_resolved(&abilities, &[1], ObjectId(1), PlayerId(0)).unwrap();
        assert!(
            matches!(resolved.effect, Effect::Discard { .. }),
            "Root should be Discard"
        );
        let sub = resolved
            .sub_ability
            .as_ref()
            .expect("Draw sub_ability must be preserved");
        assert!(
            matches!(sub.effect, Effect::Draw { .. }),
            "Sub_ability should be Draw, got {:?}",
            sub.effect
        );

        // Both modes: mode 1 then mode 2 — mode 2's internal chain must survive
        let resolved =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();
        assert!(matches!(resolved.effect, Effect::Destroy { .. }));
        let mode2_node = resolved
            .sub_ability
            .as_ref()
            .expect("mode 2 should follow mode 1");
        assert!(matches!(mode2_node.effect, Effect::Discard { .. }));
        let draw_node = mode2_node
            .sub_ability
            .as_ref()
            .expect("Draw sub must survive multi-mode chaining");
        assert!(matches!(draw_node.effect, Effect::Draw { .. }));
    }

    #[test]
    fn build_chained_resolved_sorts_indices_to_printed_order() {
        // CR 608.2c: Modes resolve in printed order regardless of the order
        // the player announced them in. Feeding [2, 0, 1] must still produce
        // a chain in order [0 → 1 → 2] (Destroy → Draw → Discard).
        let mode_destroy = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        let mode_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mode_discard = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                random: false,
                unless_filter: None,
                filter: None,
            },
        );
        let abilities = vec![mode_destroy, mode_draw, mode_discard];

        let resolved =
            build_chained_resolved(&abilities, &[2, 0, 1], ObjectId(1), PlayerId(0)).unwrap();
        assert!(
            matches!(resolved.effect, Effect::Destroy { .. }),
            "Root should be mode 0 (Destroy) — printed first"
        );
        let draw_node = resolved
            .sub_ability
            .as_ref()
            .expect("mode 1 should follow mode 0");
        assert!(
            matches!(draw_node.effect, Effect::Draw { .. }),
            "Second link should be mode 1 (Draw)"
        );
        let discard_node = draw_node
            .sub_ability
            .as_ref()
            .expect("mode 2 should follow mode 1");
        assert!(
            matches!(discard_node.effect, Effect::Discard { .. }),
            "Third link should be mode 2 (Discard) — printed last"
        );
    }

    #[test]
    fn chained_draw_player_plus_damageall_targetplayer_assigns_both_targets() {
        use crate::types::ability::{ControllerRef, TargetRef};
        // Reproduce Ashling's Command modes 2 + 3 chained:
        //   Mode 2: Draw 2, target: Player
        //   Mode 3: DamageAll { target: Typed{ controller: TargetPlayer } }
        // collect_target_slots emits 2 slots (one per mode). assign_targets_in_chain
        // must distribute both selected players — one to Draw.targets, one to
        // DamageAll.targets — so each effect's resolver sees the right player.
        let mode_draw = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
            },
        );
        let mode_damageall = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
                player_filter: None,
            },
        );

        let abilities = vec![mode_draw, mode_damageall];
        let mut chain =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();

        let p_a = TargetRef::Player(PlayerId(0));
        let p_b = TargetRef::Player(PlayerId(1));
        let result = assign_targets_in_chain(&mut chain, &[p_a.clone(), p_b.clone()]);
        assert!(
            result.is_ok(),
            "assigning two player targets to [Draw{{Player}}, DamageAll{{TargetPlayer}}] \
             chain must succeed, got {result:?}"
        );

        // Draw root should have first selected player.
        assert_eq!(chain.targets, vec![p_a.clone()], "Draw should get target 0");
        // DamageAll sub should have second selected player so its
        // `ControllerRef::TargetPlayer` filter resolves to the right player.
        let sub = chain
            .sub_ability
            .as_deref()
            .expect("sub_ability must exist");
        assert_eq!(
            sub.targets,
            vec![p_b],
            "DamageAll should get target 1 (the second player slot)"
        );
    }

    #[test]
    fn chained_token_player_plus_damageall_targetplayer_assigns_both_targets() {
        // CR 111.2 + CR 601.2c: Mirror of the Draw chain test for the Token
        // owner-target pathway. With Token{owner: Player} as mode 4 of a modal
        // spell paired with DamageAll{controller: TargetPlayer} as mode 3,
        // collect_target_slots must surface 2 slots (one per mode) and
        // assign_targets_in_chain must distribute both selected players —
        // one to Token.targets, one to DamageAll.targets.
        let mode_token = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Token {
                name: "Treasure".to_string(),
                power: crate::types::ability::PtValue::Fixed(0),
                toughness: crate::types::ability::PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::Player,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
        );
        let mode_damageall = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DamageAll {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
                player_filter: None,
            },
        );

        let abilities = vec![mode_token, mode_damageall];
        let mut chain =
            build_chained_resolved(&abilities, &[0, 1], ObjectId(1), PlayerId(0)).unwrap();

        let p_a = TargetRef::Player(PlayerId(0));
        let p_b = TargetRef::Player(PlayerId(1));
        let result = assign_targets_in_chain(&mut chain, &[p_a.clone(), p_b.clone()]);
        assert!(
            result.is_ok(),
            "assigning two player targets to [Token{{Player}}, DamageAll{{TargetPlayer}}] \
             chain must succeed, got {result:?}"
        );

        // Token root should have first selected player.
        assert_eq!(
            chain.targets,
            vec![p_a.clone()],
            "Token should get target 0"
        );
        let sub = chain
            .sub_ability
            .as_deref()
            .expect("sub_ability must exist");
        assert_eq!(
            sub.targets,
            vec![p_b],
            "DamageAll should get target 1 (the second player slot)"
        );
    }

    #[test]
    fn build_resolved_copies_optional_targeting() {
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        )
        .optional_targeting();

        let resolved = build_resolved_from_def(&def, ObjectId(10), PlayerId(0));

        assert!(resolved.optional_targeting);
    }

    #[test]
    fn validate_modal_indices_allows_repeat_when_enabled() {
        let modal = ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 3,
            allow_repeat_modes: true,
            constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
            ..Default::default()
        };

        assert!(validate_modal_indices(&modal, &[1, 1], &[]).is_ok());
    }

    #[test]
    fn validate_modal_indices_rejects_unavailable_modes() {
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 3,
            ..Default::default()
        };

        // Mode 1 is unavailable — should be rejected.
        let result = validate_modal_indices(&modal, &[1], &[1]);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unavailable (already chosen)"));

        // Mode 0 is available — should succeed.
        assert!(validate_modal_indices(&modal, &[0], &[1]).is_ok());
    }

    #[test]
    fn compute_unavailable_modes_returns_previously_chosen() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);

        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 3,
            constraints: vec![ModalSelectionConstraint::NoRepeatThisTurn],
            ..Default::default()
        };

        // No modes chosen yet.
        assert!(compute_unavailable_modes(&state, source_id, &modal).is_empty());

        // Record mode 1 chosen.
        record_modal_mode_choices(&mut state, source_id, &modal, &[1]);
        assert_eq!(
            compute_unavailable_modes(&state, source_id, &modal),
            vec![1]
        );

        // Different source_id is unaffected.
        assert!(compute_unavailable_modes(&state, ObjectId(200), &modal).is_empty());
    }

    #[test]
    fn record_modal_mode_choices_tracks_game_scoped() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);

        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 4,
            constraints: vec![ModalSelectionConstraint::NoRepeatThisGame],
            ..Default::default()
        };

        record_modal_mode_choices(&mut state, source_id, &modal, &[2]);
        assert!(state.modal_modes_chosen_this_game.contains(&(source_id, 2)));
        // Turn-scoped map should NOT be populated for game-scoped constraint.
        assert!(!state.modal_modes_chosen_this_turn.contains(&(source_id, 2)));
    }

    #[test]
    fn generate_modal_index_sequences_supports_repeated_modes() {
        let modal = ModalChoice {
            min_choices: 2,
            max_choices: 2,
            mode_count: 2,
            allow_repeat_modes: true,
            ..Default::default()
        };

        let sequences = generate_modal_index_sequences(&modal);

        assert_eq!(sequences, vec![vec![0, 0], vec![0, 1], vec![1, 1]]);
    }

    #[test]
    fn generate_target_assignments_enforces_different_target_players() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ];

        let assignments = generate_target_assignments(
            &slots,
            &[TargetSelectionConstraint::DifferentTargetPlayers],
        );

        assert_eq!(
            assignments,
            vec![
                vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1))
                ],
                vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(0))
                ],
            ]
        );
    }

    #[test]
    fn auto_select_targets_preserves_optional_single_target_choice() {
        let slots = vec![TargetSelectionSlot {
            legal_targets: vec![TargetRef::Player(PlayerId(1))],
            optional: true,
        }];

        let selected = auto_select_targets(&slots, &[]).expect("optional targeting stays legal");

        assert_eq!(selected, None);
    }

    #[test]
    fn auto_select_targets_skips_optional_first_slot_when_only_one_completion_exists() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(0))],
                optional: true,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(0))],
                optional: false,
            },
        ];

        let selected =
            auto_select_targets(&slots, &[TargetSelectionConstraint::DifferentTargetPlayers])
                .expect("unique assignment should be auto-selected");

        assert_eq!(selected, Some(vec![TargetRef::Player(PlayerId(0))]));
    }

    #[test]
    fn auto_select_targets_rejects_unsatisfied_target_constraints() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: false,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: false,
            },
        ];

        let result =
            auto_select_targets(&slots, &[TargetSelectionConstraint::DifferentTargetPlayers]);

        assert!(result.is_err());
    }

    #[test]
    fn begin_target_selection_filters_next_slot_choices_in_engine() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ];

        let progress =
            begin_target_selection(&slots, &[TargetSelectionConstraint::DifferentTargetPlayers])
                .expect("initial target selection should be legal");

        let TargetSelectionAdvance::InProgress(progress) = choose_target(
            &slots,
            &[TargetSelectionConstraint::DifferentTargetPlayers],
            &progress,
            Some(TargetRef::Player(PlayerId(0))),
        )
        .expect("first target should be accepted") else {
            panic!("expected target selection to continue");
        };

        assert_eq!(progress.current_slot, 1);
        assert_eq!(
            progress.selected_slots,
            vec![Some(TargetRef::Player(PlayerId(0)))]
        );
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Player(PlayerId(1))]
        );
    }

    #[test]
    fn choose_target_supports_skipping_optional_slot_before_required_target() {
        let slots = vec![
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: true,
            },
            TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(ObjectId(42))],
                optional: false,
            },
        ];

        let progress = begin_target_selection(&slots, &[]).expect("selection should start");
        let TargetSelectionAdvance::InProgress(progress) =
            choose_target(&slots, &[], &progress, None).expect("optional slot can be skipped")
        else {
            panic!("expected target selection to continue");
        };

        assert_eq!(progress.current_slot, 1);
        assert_eq!(progress.selected_slots, vec![None]);
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(ObjectId(42))]
        );
    }

    #[test]
    fn build_target_slots_uses_prior_player_targets_for_relative_controller_filters() {
        use crate::types::ability::ControllerRef;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let your_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for creature in [your_creature, opponent_creature] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Player(PlayerId(1))]);
        assert_eq!(
            slots[1].legal_targets,
            vec![TargetRef::Object(opponent_creature)]
        );
    }

    #[test]
    fn build_target_slots_restricts_deal_damage_any_to_any_target_classes() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let land = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Land".to_string(),
            Zone::Battlefield,
        );
        let planeswalker = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Planeswalker".to_string(),
            Zone::Battlefield,
        );
        let battle = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Battle".to_string(),
            Zone::Battlefield,
        );

        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];
        state.objects.get_mut(&land).unwrap().card_types.core_types = vec![CoreType::Land];
        state
            .objects
            .get_mut(&planeswalker)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Planeswalker];
        state
            .objects
            .get_mut(&battle)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Battle];

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("damage spell should have targets");
        assert_eq!(slots.len(), 1);
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(creature)),
            "creatures are legal any-target damage recipients"
        );
        assert!(
            !slots[0].legal_targets.contains(&TargetRef::Object(land)),
            "lands must not be legal any-target damage recipients"
        );
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Object(planeswalker)),
            "planeswalkers are legal any-target damage recipients"
        );
        assert!(
            slots[0].legal_targets.contains(&TargetRef::Object(battle)),
            "battles are legal any-target damage recipients"
        );
        assert!(
            slots[0]
                .legal_targets
                .contains(&TargetRef::Player(PlayerId(0)))
                && slots[0]
                    .legal_targets
                    .contains(&TargetRef::Player(PlayerId(1))),
            "players remain legal any-target damage recipients"
        );
    }

    #[test]
    fn choose_target_for_ability_rebinds_relative_controller_to_selected_player() {
        use crate::game::zones::create_object;
        use crate::types::ability::ControllerRef;
        use crate::types::card_type::CoreType;
        use crate::types::format::FormatConfig;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let opponent_one_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent One Creature".to_string(),
            Zone::Battlefield,
        );
        let opponent_two_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(2),
            "Opponent Two Creature".to_string(),
            Zone::Battlefield,
        );
        for creature in [opponent_one_creature, opponent_two_creature] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::TargetOnly {
                target: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("target slots should build");
        let progress =
            begin_target_selection_for_ability(&state, &ability, &slots, &[]).expect("selection");

        let TargetSelectionAdvance::InProgress(progress) = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Player(PlayerId(1))),
        )
        .expect("first opponent target should be accepted") else {
            panic!("expected second slot to remain");
        };

        assert_eq!(progress.current_slot, 1);
        assert_eq!(
            progress.current_legal_targets,
            vec![TargetRef::Object(opponent_one_creature)]
        );

        let result = choose_target_for_ability(
            &state,
            &ability,
            &slots,
            &[],
            &progress,
            Some(TargetRef::Object(opponent_two_creature)),
        );
        assert!(result.is_err());
    }

    #[test]
    fn assign_selected_slots_handles_skipped_optional_slot_in_chain() {
        let mut ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.optional_targeting = true;
        let mut ability = ability.sub_ability(ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Player,
                damage_source: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        ));

        assign_selected_slots_in_chain(&mut ability, &[None, Some(TargetRef::Player(PlayerId(1)))])
            .expect("slot-based assignment should support skipped optional targets");

        assert!(ability.targets.is_empty());
        assert_eq!(
            flatten_targets_in_chain(&ability),
            vec![TargetRef::Player(PlayerId(1))]
        );
    }

    #[test]
    fn build_target_slots_stops_at_interactive_continuation_boundary() {
        let state = crate::types::game_state::GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::RevealHand {
                target: TargetFilter::Player,
                card_filter: TargetFilter::Any,
                count: None,
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        ));

        let slots = build_target_slots(&state, &ability).expect("reveal target should be legal");

        assert_eq!(slots.len(), 1);
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));
    }

    /// CR 109.4 + CR 115.1: `PutCounterAll` with a filter referencing
    /// `ControllerRef::TargetPlayer` surfaces a companion `TargetFilter::Player`
    /// target slot so the player is chosen at target-declaration time. This
    /// covers the Splinter & Leo mode-2 gap ("put a +1/+1 counter on each other
    /// creature target player controls") and is the class-level fix for every
    /// mass-placement effect (DestroyAll, PumpAll, DamageAll, etc.).
    #[test]
    fn build_target_slots_surfaces_player_slot_for_target_player_filter() {
        use crate::game::filter::{matches_target_filter, FilterContext};
        use crate::game::zones::create_object;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;

        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let your_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        for c in [your_creature, opp_creature] {
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::PutCounterAll {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::TargetPlayer),
                ),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        // Target-slot surfacing: a companion Player slot must appear, offering
        // both players as legal choices.
        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            1,
            "expected a single TargetFilter::Player slot for TargetPlayer filter"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))));
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));

        // Runtime filter evaluation: with player=0 chosen, only P0's creatures
        // match the TypedFilter. With player=1 chosen, only P1's match.
        for (chosen, expected_match) in [(PlayerId(0), your_creature), (PlayerId(1), opp_creature)]
        {
            let mut resolved = ability.clone();
            resolved.targets = vec![TargetRef::Player(chosen)];
            let ctx = FilterContext::from_ability(&resolved);
            let filter = TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::TargetPlayer),
            );
            assert!(
                matches_target_filter(&state, expected_match, &filter, &ctx),
                "chosen player P{} — creature they control should match",
                chosen.0
            );
            let other = if expected_match == your_creature {
                opp_creature
            } else {
                your_creature
            };
            assert!(
                !matches_target_filter(&state, other, &filter, &ctx),
                "chosen player P{} — other player's creature should NOT match",
                chosen.0
            );
        }
    }

    /// CR 115.1 + CR 404 + CR 406: Nihil Spellbomb / Bojuka Bog / Tormod's
    /// Crypt regression guard. "Exile target player's graveyard" lowers to
    /// `ChangeZoneAll { origin: Graveyard, destination: Exile, target: Player }`.
    /// The mass `target: Player` filter parameterizes the scan by a player —
    /// the resolver enumerates that player's graveyard at resolution time —
    /// so a companion `TargetFilter::Player` slot must be surfaced; otherwise
    /// `ability.targets` stays empty and `player_scope` falls back to the
    /// activator, exiling the wrong (usually empty) graveyard.
    #[test]
    fn build_target_slots_surfaces_player_slot_for_change_zone_all_player_filter() {
        let state = crate::types::game_state::GameState::new_two_player(42);

        let ability = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Exile,
                target: TargetFilter::Player,
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("should build");
        assert_eq!(
            slots.len(),
            1,
            "expected a single TargetFilter::Player slot for graveyard-mass exile"
        );
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(0))));
        assert!(slots[0]
            .legal_targets
            .contains(&TargetRef::Player(PlayerId(1))));
    }

    /// CR 109.4 + CR 115.1 + CR 506.2: Karazikar regression guard.
    ///
    /// "Whenever you attack a player, tap target creature that player controls
    /// and goad it." The Tap effect's target filter has
    /// `controller = ControllerRef::TargetPlayer`. Auto-surfacing must produce
    /// a Player target slot, and runtime filter evaluation with a chosen player
    /// must restrict legal creature targets to only that player's creatures —
    /// never the trigger controller's own creatures.
    #[test]
    fn karazikar_tap_target_player_restricts_to_chosen_players_creatures() {
        use crate::game::filter::{matches_target_filter, FilterContext};
        use crate::types::ability::ControllerRef;

        let mut state = GameState::new_two_player(42);
        let your_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Your Soldier".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Goblin".to_string(),
            Zone::Battlefield,
        );
        for c in [your_creature, opp_creature] {
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let creature_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetPlayer));

        let ability = ResolvedAbility::new(
            Effect::Tap {
                target: creature_filter.clone(),
            },
            vec![],
            ObjectId(900),
            PlayerId(0),
        );

        // Auto-surface produces the companion Player slot first.
        let slots = build_target_slots(&state, &ability).expect("should build");
        assert!(
            slots
                .iter()
                .any(|s| s.legal_targets.contains(&TargetRef::Player(PlayerId(1)))),
            "expected a Player slot offering opponent as a target"
        );

        // Runtime filter: with the opponent chosen, only the opponent's creature
        // matches; your own creature must be excluded.
        let mut resolved = ability.clone();
        resolved.targets = vec![TargetRef::Player(PlayerId(1))];
        let ctx = FilterContext::from_ability(&resolved);
        assert!(
            matches_target_filter(&state, opp_creature, &creature_filter, &ctx),
            "opponent's creature should be a legal tap target",
        );
        assert!(
            !matches_target_filter(&state, your_creature, &creature_filter, &ctx),
            "your own creature must NOT be a legal tap target — this is the Karazikar bug",
        );
    }

    /// CR 701.12a: ExchangeControl must surface two independent target slots,
    /// each honouring its per-slot filter. This is the regression guard for Bug A:
    /// the parser previously dropped both target clauses and the resolver's
    /// early `targets.len() < 2` branch made the effect a no-op.
    #[test]
    fn build_target_slots_exchange_control_surfaces_two_slots() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let p0_land = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "P0 Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p0_land)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);
        let p1_land = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(1),
            "P1 Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p1_land)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Land);

        let target_a = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: Some(ControllerRef::You),
            ..Default::default()
        });
        let target_b = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Land],
            controller: Some(ControllerRef::Opponent),
            ..Default::default()
        });
        let ability = ResolvedAbility::new(
            Effect::ExchangeControl { target_a, target_b },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("two slots should build");
        assert_eq!(slots.len(), 2, "exchange-control must surface two slots");
        // Slot 0: "land you control" → only p0_land legal (caster is PlayerId(0)).
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(p0_land)]);
        // Slot 1: "land an opponent controls" → only p1_land legal.
        assert_eq!(slots[1].legal_targets, vec![TargetRef::Object(p1_land)]);
    }

    /// CR 701.12a: SelfRef slots ("this artifact and target X") are filled by
    /// the resolver from `ability.source_id` and must NOT be surfaced as a
    /// user-selectable slot. Only the non-SelfRef slot appears.
    #[test]
    fn build_target_slots_exchange_control_self_ref_suppressed() {
        use crate::types::ability::{ControllerRef, TypeFilter};
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let p1_artifact = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(1),
            "Opponent Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&p1_artifact)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);

        let target_b = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            controller: Some(ControllerRef::Opponent),
            ..Default::default()
        });
        let ability = ResolvedAbility::new(
            Effect::ExchangeControl {
                target_a: TargetFilter::SelfRef,
                target_b,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let slots = build_target_slots(&state, &ability).expect("one slot should build");
        assert_eq!(slots.len(), 1, "SelfRef slot must not be surfaced");
        assert_eq!(slots[0].legal_targets, vec![TargetRef::Object(p1_artifact)]);
    }

    #[test]
    fn build_target_slots_expands_finite_multi_target() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);
        let creature_a = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);
        let creature_b = crate::game::zones::create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Creature);

        let mut ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec {
            min: 0,
            max: Some(2),
        });

        let slots = build_target_slots(&state, &ability).expect("multi-target slots should build");

        assert_eq!(slots.len(), 2);
        assert!(slots.iter().all(|slot| slot.optional));
    }

    #[test]
    fn assign_selected_slots_collects_multi_target_choices() {
        let mut ability = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: "P1P1".to_string(),
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![],
            ObjectId(10),
            PlayerId(0),
        );
        ability.multi_target = Some(crate::types::ability::MultiTargetSpec {
            min: 0,
            max: Some(2),
        });

        assign_selected_slots_in_chain(
            &mut ability,
            &[
                Some(TargetRef::Object(ObjectId(1))),
                Some(TargetRef::Object(ObjectId(2))),
            ],
        )
        .expect("slot-based assignment should preserve both chosen targets");

        assert_eq!(
            ability.targets,
            vec![
                TargetRef::Object(ObjectId(1)),
                TargetRef::Object(ObjectId(2))
            ]
        );
    }
}
