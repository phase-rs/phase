use crate::types::ability::{
    AbilityCondition, AbilityDefinition, Effect, ModalChoice, ModalSelectionConstraint,
    ResolvedAbility, TargetFilter, TargetRef,
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
pub fn build_chained_resolved(
    abilities: &[AbilityDefinition],
    indices: &[usize],
    source_id: ObjectId,
    controller: PlayerId,
) -> Result<ResolvedAbility, EngineError> {
    if indices.is_empty() {
        return Err(EngineError::InvalidAction("No modes selected".to_string()));
    }

    let mut result: Option<ResolvedAbility> = None;
    for &idx in indices.iter().rev() {
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

/// CR 601.2c: Begin target selection by computing legal targets for the first slot.
pub fn begin_target_selection(
    target_slots: &[TargetSelectionSlot],
    constraints: &[TargetSelectionConstraint],
) -> Result<TargetSelectionProgress, EngineError> {
    build_target_selection_progress(target_slots, constraints, 0, Vec::new())
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
    if let Some(filter) = triggers::extract_target_filter_from_effect(&ability.effect) {
        let legal_targets =
            targeting::find_legal_targets(state, filter, ability.controller, ability.source_id);
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

fn assign_targets_recursive(
    ability: &mut ResolvedAbility,
    targets: &[TargetRef],
    next_target: &mut usize,
) -> Result<(), EngineError> {
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
                    ability.targets = targets[*next_target..*next_target + current_count].to_vec();
                    *next_target += current_count;
                }
                // CR 115.1d: unbounded multi-target — fall through to single-target path.
                None => {
                    if let Some(target) = targets.get(*next_target) {
                        ability.targets = vec![target.clone()];
                        *next_target += 1;
                    } else if ability.optional_targeting {
                        ability.targets.clear();
                    } else {
                        return Err(EngineError::InvalidAction(
                            "Missing required target".to_string(),
                        ));
                    }
                }
            }
        } else if let Some(target) = targets.get(*next_target) {
            ability.targets = vec![target.clone()];
            *next_target += 1;
        } else if ability.optional_targeting {
            ability.targets.clear();
        } else {
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
                    ability.targets = window.iter().flatten().cloned().collect();
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
                        Some(target) => ability.targets = vec![target.clone()],
                        None if ability.optional_targeting => ability.targets.clear(),
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
                Some(target) => ability.targets = vec![target.clone()],
                None if ability.optional_targeting => ability.targets.clear(),
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
    use crate::types::ability::{
        AbilityKind, Effect, ModalChoice, ModalSelectionConstraint, QuantityExpr, QuantityRef,
        TargetFilter, TypedFilter,
    };
    use crate::types::game_state::{GameState, TargetSelectionConstraint, TargetSelectionSlot};
    use crate::types::identifiers::ObjectId;
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
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Any,
                random: false,
                up_to: true,
                unless_filter: None,
            },
        );
        mode2.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
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
